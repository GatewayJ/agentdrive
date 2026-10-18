use crate::{
    LocalBackend, now_ns,
    sqlite::{sql_error, unsigned_integer},
};
use agentdrive_model::*;
use agentdrive_ports::*;
use async_trait::async_trait;
use bytes::Bytes;
use rusqlite::params;
use sha2::Digest;
use std::{
    fs::File,
    io::Read,
    path::PathBuf,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, ReadBuf};

impl LocalBackend {
    async fn remove_cached(
        &self,
        workspace: WorkspaceId,
        object: &ObjectRef,
        confirmed: bool,
    ) -> Result<bool> {
        let (_, count) = self
            .remove_objects(vec![CachedObject {
                workspace,
                reference: object.clone(),
                remote_confirmed: confirmed,
                last_access_ns: 0,
            }])
            .await?;
        Ok(count > 0)
    }

    pub(crate) fn object_path(&self, workspace: WorkspaceId, object: &ObjectId) -> PathBuf {
        self.inner
            .root
            .join("objects")
            .join(workspace.to_string())
            .join(object.as_str())
    }

    pub(crate) fn verify_file(&self, workspace: WorkspaceId, object: &ObjectRef) -> Result<File> {
        let path = self.object_path(workspace, &object.id);
        let mut file = File::open(path)?;
        let metadata = file.metadata()?;
        let signature = (metadata.len(), metadata.modified()?);
        if signature.0 != object.size {
            return Err(Error::integrity("local object has an invalid length"));
        }
        let key = (workspace, object.id.clone());
        let verified = self
            .inner
            .verified
            .lock()
            .get(&key)
            .is_some_and(|value| *value == signature);
        if !verified {
            let mut digest = object_hasher(object.kind);
            let mut buffer = vec![0; IO_BUFFER_BYTES];
            loop {
                let count = file.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                digest.update(&buffer[..count]);
            }
            if finish_digest(digest) != object.id {
                return Err(Error::integrity(
                    "local object digest does not match its identity",
                ));
            }
            self.inner.verified.lock().insert(key, signature);
            use std::io::{Seek, SeekFrom};
            file.seek(SeekFrom::Start(0))?;
        }
        Ok(file)
    }

    async fn install_stream(
        &self,
        workspace: WorkspaceId,
        kind: ObjectKind,
        reader: ByteStream,
        expected: Option<ObjectRef>,
    ) -> Result<ObjectRef> {
        let staged = self.stage_object(workspace, kind, reader, expected).await?;
        let reference = staged.reference.clone();
        self.publish_batch(workspace, vec![staged]).await?;
        Ok(reference)
    }
}

struct LeasedReader {
    file: tokio::fs::File,
    owner: Arc<crate::Inner>,
    key: (WorkspaceId, ObjectId),
}

impl AsyncRead for LeasedReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.file).poll_read(context, buffer)
    }
}

impl Drop for LeasedReader {
    fn drop(&mut self) {
        let mut leases = self.owner.leases.lock();
        if let Some(count) = leases.get_mut(&self.key) {
            *count -= 1;
            if *count == 0 {
                leases.remove(&self.key);
            }
        }
    }
}

#[async_trait]
impl LocalObjectStore for LocalBackend {
    async fn begin_batch(&self, workspace: WorkspaceId) -> Result<Box<dyn ObjectBatch>> {
        Ok(Box::new(crate::batch::Batch::new(self.clone(), workspace)))
    }

    async fn remove_batch(&self, objects: Vec<CachedObject>) -> Result<u64> {
        self.remove_objects(objects).await.map(|result| result.0)
    }

    async fn ingest(
        &self,
        workspace: WorkspaceId,
        kind: ObjectKind,
        reader: ByteStream,
    ) -> Result<ObjectRef> {
        self.install_stream(workspace, kind, reader, None).await
    }
    async fn install(&self, workspace: WorkspaceId, stream: ObjectStream) -> Result<()> {
        if stream.reference.kind != ObjectKind::File
            && stream.reference.size > MAX_METADATA_BYTES as u64
        {
            return Err(Error::integrity(
                "remote metadata object exceeds the format limit",
            ));
        }
        let reference = self
            .install_stream(
                workspace,
                stream.reference.kind,
                stream.reader,
                Some(stream.reference),
            )
            .await?;
        self.confirm_remote(workspace, &reference).await
    }

    async fn open(&self, workspace: WorkspaceId, object: &ObjectRef) -> Result<ObjectStream> {
        let reference = object.clone();
        let object = object.clone();
        let file = self
            .blocking(move |backend| {
                let _guard = backend.inner.object_io.lock();
                let file = backend.verify_file(workspace, &object)?;
                *backend
                    .inner
                    .leases
                    .lock()
                    .entry((workspace, object.id.clone()))
                    .or_default() += 1;
                let result = backend.inner.connection.lock().execute(
                    "UPDATE objects SET last_access_ns=?3 WHERE workspace=?1 AND hash=?2",
                    params![workspace.to_string(), object.id.as_str(), now_ns()],
                );
                if let Err(error) = result {
                    let mut leases = backend.inner.leases.lock();
                    if let Some(count) = leases.get_mut(&(workspace, object.id.clone())) {
                        *count -= 1;
                    }
                    return Err(sql_error(error));
                }
                Ok(file)
            })
            .await?;
        Ok(ObjectStream {
            reader: Box::pin(LeasedReader {
                file: tokio::fs::File::from_std(file),
                owner: self.inner.clone(),
                key: (workspace, reference.id.clone()),
            }),
            reference,
        })
    }

    async fn contains(&self, workspace: WorkspaceId, object: &ObjectRef) -> Result<bool> {
        let object = object.clone();
        self.blocking(move |backend| {
            let _guard = backend.inner.object_io.lock();
            match backend.verify_file(workspace, &object) {
                Ok(_) => Ok(true),
                Err(error) if error.code == ErrorCode::NotFound => Ok(false),
                Err(error) => Err(error),
            }
        })
        .await
    }

    async fn read_range(
        &self,
        workspace: WorkspaceId,
        object: &ObjectRef,
        offset: u64,
        size: u32,
    ) -> Result<Bytes> {
        if size as usize > IO_BUFFER_BYTES * 16 {
            return Err(Error::invalid("read request exceeds the buffer limit"));
        }
        let object = object.clone();
        self.blocking(move |backend| {
            use std::io::{Seek, SeekFrom};
            let _guard = backend.inner.object_io.lock();
            let mut file = backend.verify_file(workspace, &object)?;
            if offset >= object.size {
                return Ok(Bytes::new());
            }
            file.seek(SeekFrom::Start(offset))?;
            let mut data = vec![0; (object.size - offset).min(u64::from(size)) as usize];
            file.read_exact(&mut data)?;
            Ok(data.into())
        })
        .await
    }

    async fn confirm_remote(&self, workspace: WorkspaceId, object: &ObjectRef) -> Result<()> {
        let id = object.id.clone();
        self.db(move |connection| {
            connection
                .execute(
                    "UPDATE objects SET remote_confirmed=1 WHERE workspace=?1 AND hash=?2 AND remote_confirmed=0",
                    params![workspace.to_string(), id.as_str()],
                )
                .map_err(sql_error)?;
            Ok(())
        })
        .await
    }

    async fn objects(&self, workspace: WorkspaceId) -> Result<Vec<CachedObject>> {
        self.db(move |connection| {
            let mut statement = connection.prepare("SELECT hash,kind,size,remote_confirmed,last_access_ns FROM objects WHERE workspace=?1 ORDER BY last_access_ns,hash").map_err(sql_error)?;
            let rows = statement.query_map([workspace.to_string()], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?, row.get::<_, bool>(3)?, row.get::<_, i64>(4)?))).map_err(sql_error)?;
            rows.map(|row| {
                let (id, kind, size, remote_confirmed, last_access_ns) = row.map_err(sql_error)?;
                Ok(CachedObject { workspace, reference: ObjectRef { id: id.parse()?, kind: serde_json::from_str(&kind)?, size: unsigned_integer(size)? }, remote_confirmed, last_access_ns })
            }).collect()
        }).await
    }

    async fn evict(&self, workspace: WorkspaceId, object: &ObjectRef) -> Result<bool> {
        self.remove_cached(workspace, object, true).await
    }

    async fn discard_unconfirmed(
        &self,
        workspace: WorkspaceId,
        object: &ObjectRef,
    ) -> Result<bool> {
        self.remove_cached(workspace, object, false).await
    }

    async fn stats(&self) -> Result<StorageStats> {
        self.blocking(|backend| {
            let total_bytes = fs4::total_space(&backend.inner.root)?;
            let available_bytes = fs4::available_space(&backend.inner.root)?;
            let (cached_bytes, protected_bytes) = backend
                .inner
                .connection
                .lock()
                .query_row(
                    "SELECT cached_bytes,protected_bytes FROM object_usage WHERE id=1",
                    [],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .map_err(sql_error)?;
            Ok(StorageStats {
                total_bytes,
                available_bytes,
                cached_bytes: unsigned_integer(cached_bytes)?,
                protected_bytes: unsigned_integer(protected_bytes)?,
            })
        })
        .await
    }

    async fn volume_stats(&self) -> Result<VolumeStats> {
        self.blocking(|backend| {
            Ok(VolumeStats {
                total_bytes: fs4::total_space(&backend.inner.root)?,
                available_bytes: fs4::available_space(&backend.inner.root)?,
            })
        })
        .await
    }
}

#[async_trait]
impl ObjectWriter for LocalBackend {
    async fn put(
        &self,
        workspace: WorkspaceId,
        kind: ObjectKind,
        data: Bytes,
    ) -> Result<ObjectRef> {
        if kind != ObjectKind::File && data.len() > MAX_METADATA_BYTES {
            return Err(Error::invalid("metadata object exceeds the format limit"));
        }
        self.install_stream(workspace, kind, Box::pin(std::io::Cursor::new(data)), None)
            .await
    }

    async fn seal(&self, file: WorkingFile) -> Result<ObjectRef> {
        let reader = tokio::fs::File::open(self.working_path(file)).await?;
        self.install_stream(file.workspace, ObjectKind::File, Box::pin(reader), None)
            .await
    }
}
