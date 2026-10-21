use crate::{OpenCsgBackend, http::validate_commit};
use agentdrive_model::*;
use agentdrive_ports::*;
use async_trait::async_trait;
use reqwest::Method;

impl OpenCsgBackend {
    fn ref_path(&self, key: &RefKey) -> String {
        match key {
            RefKey::Workspace(workspace) => format!("{}/{workspace}/control.json", self.prefix),
            RefKey::Branch { workspace, branch } => {
                format!("{}/{workspace}/branches/{branch}.json", self.prefix)
            }
        }
    }
}

#[async_trait]
impl RemoteRefStore for OpenCsgBackend {
    async fn check_connection(&self) -> Result<()> {
        OpenCsgBackend::check_connection(self).await
    }

    async fn get(&self, key: &RefKey) -> Result<Option<VersionedRef>> {
        let version = self.repository.head().await?;
        let bytes = self.repository.read(&self.ref_path(key), &version).await?;
        bytes
            .map(|bytes| {
                if bytes.len() > MAX_METADATA_BYTES {
                    return Err(Error::integrity("OpenCSG reference exceeds the size limit"));
                }
                Ok(VersionedRef {
                    version,
                    bytes: bytes.into(),
                })
            })
            .transpose()
    }

    async fn compare_exchange(&self, update: RefUpdate) -> Result<CasOutcome> {
        if update.bytes.len() > MAX_METADATA_BYTES || std::str::from_utf8(&update.bytes).is_err() {
            return Err(Error::invalid(
                "OpenCSG references require UTF-8 metadata within the format size limit",
            ));
        }
        self.check_connection().await?;
        let _write = self.writes.lock().await;
        let head = self.repository.head().await?;
        let path = self.ref_path(&update.key);
        let method = if let Some(expected) = &update.expected_version {
            validate_commit(expected)?;
            if *expected != head {
                return Ok(CasOutcome::Conflict);
            }
            if self.repository.read(&path, &head).await?.is_none() {
                return Ok(CasOutcome::Conflict);
            }
            Method::PUT
        } else {
            if self.repository.read(&path, &head).await?.is_some() {
                return Ok(CasOutcome::Conflict);
            }
            Method::POST
        };
        let written = self
            .repository
            .write(method, &path, &update.bytes, &head)
            .await;
        let current = match self.repository.head().await {
            Ok(current) => current,
            Err(_) => return Ok(CasOutcome::Unknown),
        };
        match self.repository.read(&path, &current).await {
            Ok(Some(bytes)) if bytes == update.bytes => {
                Ok(CasOutcome::Applied { version: current })
            }
            Ok(_) => match written {
                Err(error)
                    if matches!(error.code, ErrorCode::Conflict | ErrorCode::AlreadyExists) =>
                {
                    Ok(CasOutcome::Conflict)
                }
                Err(error) if !error.retryable => Err(error),
                // Another writer may have advanced a successfully published reference.
                // The engine resolves that outcome from its retained operation history.
                _ => Ok(CasOutcome::Unknown),
            },
            Err(_) => Ok(CasOutcome::Unknown),
        }
    }

    async fn list_branches(&self, workspace: WorkspaceId) -> Result<Vec<BranchId>> {
        let head = self.repository.head().await?;
        let entries = self
            .repository
            .entries(&format!("{}/{workspace}/branches", self.prefix), &head)
            .await?;
        let mut branches = std::collections::BTreeSet::new();
        for entry in entries {
            if entry.kind != "file" {
                continue;
            }
            if let Some(id) = entry.name.strip_suffix(".json") {
                branches.insert(
                    id.parse::<BranchId>()
                        .map_err(|_| Error::integrity("invalid OpenCSG branch reference name"))?,
                );
            }
        }
        Ok(branches.into_iter().collect())
    }
}
