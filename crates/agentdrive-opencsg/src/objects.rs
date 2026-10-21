use crate::OpenCsgBackend;
use agentdrive_model::*;
use agentdrive_ports::*;
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio_util::io::StreamReader;

// Both the stored text and the API request use base64. Four MiB stays below the
// server's default non-LFS size limit and bounds memory independently of file size.
const CHUNK_BYTES: usize = 4 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: u32,
    object: ObjectRef,
    chunks: Vec<String>,
}

impl OpenCsgBackend {
    fn object_path(&self, workspace: WorkspaceId, object: &ObjectRef) -> String {
        format!("{}/{workspace}/objects/{}.json", self.prefix, object.id)
    }

    async fn manifest(
        &self,
        workspace: WorkspaceId,
        object: &ObjectRef,
        head: &str,
    ) -> Result<Option<Manifest>> {
        let Some(bytes) = self
            .repository
            .read(&self.object_path(workspace, object), head)
            .await?
        else {
            return Ok(None);
        };
        if bytes.len() > MAX_METADATA_BYTES {
            return Err(Error::integrity(
                "OpenCSG object manifest exceeds the size limit",
            ));
        }
        let manifest: Manifest = decode(&bytes)?;
        if manifest.format != 1
            || manifest.object != *object
            || manifest.chunks.len() as u64 != object.size.div_ceil(CHUNK_BYTES as u64)
            || manifest
                .chunks
                .iter()
                .any(|id| id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(Error::integrity(
                "OpenCSG object manifest differs from its reference",
            ));
        }
        Ok(Some(manifest))
    }
}

#[async_trait]
impl RemoteObjectStore for OpenCsgBackend {
    async fn upload(&self, workspace: WorkspaceId, mut source: ObjectStream) -> Result<()> {
        self.check_connection().await?;
        if source.reference.size.div_ceil(CHUNK_BYTES as u64) > (MAX_METADATA_BYTES / 70) as u64 {
            return Err(Error::new(
                ErrorCode::CapacityExceeded,
                "object exceeds the OpenCSG manifest capacity",
            ));
        }
        let mut digest = object_hasher(source.reference.kind);
        let mut remaining = source.reference.size;
        let mut chunks = Vec::new();
        let mut buffer = vec![0; CHUNK_BYTES];
        while remaining > 0 {
            let size = remaining.min(CHUNK_BYTES as u64) as usize;
            source.reader.read_exact(&mut buffer[..size]).await?;
            let bytes = &buffer[..size];
            digest.update(bytes);
            let id = format!("{:x}", Sha256::digest(bytes));
            self.create(
                &format!("{}/{workspace}/chunks/{id}", self.prefix),
                STANDARD.encode(bytes).as_bytes(),
            )
            .await?;
            chunks.push(id);
            remaining -= size as u64;
        }
        if source.reader.read(&mut buffer[..1]).await? != 0
            || finish_digest(digest) != source.reference.id
        {
            return Err(Error::integrity(
                "uploaded content differs from its object reference",
            ));
        }
        let path = self.object_path(workspace, &source.reference);
        let manifest = Manifest {
            format: 1,
            object: source.reference,
            chunks,
        };
        self.create(&path, &encode(&manifest)?).await
    }

    async fn download(&self, workspace: WorkspaceId, object: &ObjectRef) -> Result<ObjectStream> {
        let head = self.repository.head().await?;
        let manifest = Arc::new(
            self.manifest(workspace, object, &head)
                .await?
                .ok_or_else(|| Error::new(ErrorCode::NotFound, "OpenCSG object does not exist"))?,
        );
        if object.size == 0 {
            verify_object(object, &[])?;
        }
        let repository = self.repository.clone();
        let prefix = format!("{}/{workspace}/chunks", self.prefix);
        let stream = futures::stream::try_unfold(
            (0usize, object_hasher(object.kind)),
            move |(index, mut digest)| {
                let repository = repository.clone();
                let head = head.clone();
                let prefix = prefix.clone();
                let manifest = manifest.clone();
                async move {
                    if index == manifest.chunks.len() {
                        return Ok(None);
                    }
                    let result = async {
                        let id = &manifest.chunks[index];
                        let encoded = repository
                            .read(&format!("{prefix}/{id}"), &head)
                            .await?
                            .ok_or_else(|| Error::integrity("OpenCSG object chunk is missing"))?;
                        let bytes = STANDARD.decode(encoded).map_err(|_| {
                            Error::integrity("invalid OpenCSG object chunk encoding")
                        })?;
                        let expected = (manifest.object.size - index as u64 * CHUNK_BYTES as u64)
                            .min(CHUNK_BYTES as u64)
                            as usize;
                        if bytes.len() != expected || format!("{:x}", Sha256::digest(&bytes)) != *id
                        {
                            return Err(Error::integrity(
                                "OpenCSG object chunk failed verification",
                            ));
                        }
                        digest.update(&bytes);
                        if index + 1 == manifest.chunks.len()
                            && finish_digest(digest.clone()) != manifest.object.id
                        {
                            return Err(Error::integrity("OpenCSG object failed verification"));
                        }
                        Ok(Bytes::from(bytes))
                    }
                    .await;
                    result
                        .map(|bytes| Some((bytes, (index + 1, digest))))
                        .map_err(std::io::Error::other)
                }
            },
        );
        Ok(ObjectStream {
            reference: object.clone(),
            reader: Box::pin(StreamReader::new(Box::pin(stream))),
        })
    }

    async fn contains(&self, workspace: WorkspaceId, object: &ObjectRef) -> Result<bool> {
        let head = self.repository.head().await?;
        Ok(self.manifest(workspace, object, &head).await?.is_some())
    }
}
