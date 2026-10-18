use crate::sqlite::{db_integer, sql_error};
use crate::{LocalBackend, create_private_dir, now_ns, publish_object, sync_directory};
use agentdrive_model::*;
use agentdrive_ports::*;
use async_trait::async_trait;
use bytes::Bytes;
use parking_lot::Mutex;
use rusqlite::params;
use sha2::Digest;
use std::collections::{BTreeMap, BTreeSet};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug)]
pub(crate) struct StagedObject {
    pub reference: ObjectRef,
    temporary: Option<tempfile::TempPath>,
}

#[derive(Debug)]
pub(crate) struct Batch {
    backend: LocalBackend,
    workspace: WorkspaceId,
    staged: Mutex<BTreeMap<ObjectId, StagedObject>>,
}

impl Batch {
    pub(crate) fn new(backend: LocalBackend, workspace: WorkspaceId) -> Self {
        Self {
            backend,
            workspace,
            staged: Mutex::new(BTreeMap::new()),
        }
    }

    async fn stage(
        &self,
        workspace: WorkspaceId,
        kind: ObjectKind,
        reader: ByteStream,
    ) -> Result<ObjectRef> {
        if workspace != self.workspace {
            return Err(Error::invalid("object batch belongs to another workspace"));
        }
        let object = self
            .backend
            .stage_object(workspace, kind, reader, None)
            .await?;
        let reference = object.reference.clone();
        self.staged
            .lock()
            .entry(reference.id.clone())
            .or_insert(object);
        Ok(reference)
    }
}

#[async_trait]
impl ObjectWriter for Batch {
    async fn put(
        &self,
        workspace: WorkspaceId,
        kind: ObjectKind,
        bytes: Bytes,
    ) -> Result<ObjectRef> {
        self.stage(workspace, kind, Box::pin(std::io::Cursor::new(bytes)))
            .await
    }

    async fn seal(&self, file: WorkingFile) -> Result<ObjectRef> {
        let reader = tokio::fs::File::open(self.backend.working_path(file)).await?;
        self.stage(file.workspace, ObjectKind::File, Box::pin(reader))
            .await
    }
}

#[async_trait]
impl ObjectBatch for Batch {
    async fn finish(self: Box<Self>) -> Result<()> {
        self.backend
            .publish_batch(
                self.workspace,
                self.staged.into_inner().into_values().collect(),
            )
            .await
    }
}

impl LocalBackend {
    pub(crate) async fn stage_object(
        &self,
        workspace: WorkspaceId,
        kind: ObjectKind,
        mut reader: ByteStream,
        expected: Option<ObjectRef>,
    ) -> Result<StagedObject> {
        let staging = self.inner.root.join("staging");
        let (file, temporary) = self
            .blocking(move |_| Ok(tempfile::NamedTempFile::new_in(staging)?.into_parts()))
            .await?;
        let mut output = tokio::fs::File::from_std(file);
        let mut digest = object_hasher(kind);
        let mut size = 0u64;
        let mut buffer = vec![0; IO_BUFFER_BYTES];
        loop {
            let count = reader.read(&mut buffer).await?;
            if count == 0 {
                break;
            }
            size = size
                .checked_add(count as u64)
                .ok_or_else(|| Error::invalid("object is too large"))?;
            if expected.as_ref().is_some_and(|object| size > object.size) {
                return Err(Error::integrity(
                    "object stream exceeded its declared length",
                ));
            }
            if kind != ObjectKind::File && size > MAX_METADATA_BYTES as u64 {
                return Err(Error::invalid("metadata object exceeds its size limit"));
            }
            digest.update(&buffer[..count]);
            output.write_all(&buffer[..count]).await?;
        }
        let reference = ObjectRef {
            id: finish_digest(digest),
            kind,
            size,
        };
        if expected.as_ref().is_some_and(|object| object != &reference) {
            return Err(Error::integrity(
                "downloaded object failed digest verification",
            ));
        }
        let existing = reference.clone();
        let reusable = self
            .blocking(move |backend| {
                let _guard = backend.inner.object_io.lock();
                let indexed: bool = backend
                    .inner
                    .connection
                    .lock()
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM objects WHERE workspace=?1 AND hash=?2)",
                        params![workspace.to_string(), existing.id.as_str()],
                        |row| row.get(0),
                    )
                    .map_err(sql_error)?;
                if !indexed {
                    return Ok(false);
                }
                match backend.verify_file(workspace, &existing) {
                    Ok(_) => Ok(true),
                    Err(error) if error.code == ErrorCode::NotFound => Ok(false),
                    Err(error) => Err(error),
                }
            })
            .await?;
        if reusable {
            return Ok(StagedObject {
                reference,
                temporary: None,
            });
        }
        output.sync_all().await?;
        drop(output);
        Ok(StagedObject {
            reference,
            temporary: Some(temporary),
        })
    }

    pub(crate) async fn publish_batch(
        &self,
        workspace: WorkspaceId,
        objects: Vec<StagedObject>,
    ) -> Result<()> {
        let pending: Vec<_> = objects
            .into_iter()
            .filter(|object| object.temporary.is_some())
            .collect();
        if pending.is_empty() {
            return Ok(());
        }
        self.blocking(move |backend| {
            let _guard = backend.inner.object_io.lock();
            let parent = backend.inner.root.join("objects").join(workspace.to_string());
            let existed = parent.is_dir();
            create_private_dir(&parent)?;
            let mut references = Vec::with_capacity(pending.len());
            for object in pending {
                let reference = object.reference;
                let destination = backend.object_path(workspace, &reference.id);
                let temporary = object
                    .temporary
                    .ok_or_else(|| Error::integrity("staged object has no file"))?;
                match publish_object(temporary, &destination) {
                    Ok(()) => (),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        backend.verify_file(workspace, &reference)?;
                    }
                    Err(error) => return Err(error.into()),
                }
                let metadata = std::fs::metadata(&destination)?;
                backend.inner.verified.lock().insert(
                    (workspace, reference.id.clone()),
                    (metadata.len(), metadata.modified()?),
                );
                references.push(reference);
            }
            // Every staged file is synced before publication. Confirm the entire
            // namespace before exposing any of its references to SQLite commits.
            sync_directory(&parent)?;
            if !existed || !backend.inner.durable_directories.lock().contains(&workspace) {
                sync_directory(&backend.inner.root.join("objects"))?;
                backend.inner.durable_directories.lock().insert(workspace);
            }
            let mut connection = backend.inner.connection.lock();
            let transaction = connection.transaction().map_err(sql_error)?;
            for reference in references {
                transaction.execute(
                    "INSERT INTO objects(workspace,hash,kind,size,remote_confirmed,last_access_ns) \
                     VALUES(?1,?2,?3,?4,0,?5) ON CONFLICT(workspace,hash) \
                     DO UPDATE SET last_access_ns=excluded.last_access_ns",
                    params![
                        workspace.to_string(), reference.id.as_str(),
                        serde_json::to_string(&reference.kind)?, db_integer(reference.size)?, now_ns()
                    ],
                ).map_err(sql_error)?;
            }
            transaction.commit().map_err(sql_error)?;
            Ok(())
        })
        .await
    }

    pub(crate) async fn remove_objects(&self, objects: Vec<CachedObject>) -> Result<(u64, usize)> {
        self.blocking(move |backend| {
            let _guard = backend.inner.object_io.lock();
            let mut connection = backend.inner.connection.lock();
            let transaction = connection.transaction().map_err(sql_error)?;
            let mut directories = BTreeSet::new();
            let mut removed = 0u64;
            let mut count = 0;
            for entry in objects {
                let key = (entry.workspace, entry.reference.id.clone());
                if backend
                    .inner
                    .leases
                    .lock()
                    .get(&key)
                    .is_some_and(|count| *count > 0)
                {
                    continue;
                }
                let eligible: bool = transaction
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM objects \
                     WHERE workspace=?1 AND hash=?2 AND remote_confirmed=?3)",
                        params![
                            entry.workspace.to_string(),
                            entry.reference.id.as_str(),
                            entry.remote_confirmed
                        ],
                        |row| row.get(0),
                    )
                    .map_err(sql_error)?;
                if !eligible {
                    continue;
                }
                let path = backend.object_path(entry.workspace, &entry.reference.id);
                match std::fs::remove_file(&path) {
                    Ok(()) => (),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                    Err(error) => return Err(error.into()),
                }
                directories.insert(
                    path.parent()
                        .ok_or_else(|| Error::integrity("object path has no parent"))?
                        .to_owned(),
                );
                transaction
                    .execute(
                        "DELETE FROM objects WHERE workspace=?1 AND hash=?2",
                        params![entry.workspace.to_string(), entry.reference.id.as_str()],
                    )
                    .map_err(sql_error)?;
                backend.inner.verified.lock().remove(&key);
                count += 1;
                removed = removed
                    .checked_add(entry.reference.size)
                    .ok_or_else(|| Error::integrity("reclaimed byte count overflow"))?;
            }
            for directory in directories {
                sync_directory(&directory)?;
            }
            transaction.commit().map_err(sql_error)?;
            Ok((removed, count))
        })
        .await
    }
}
