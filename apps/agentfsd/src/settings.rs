use agentfs_config::{ConfigApplier, RuntimeConfig};
use agentfs_engine::{Engine, RemoteStorage};
use agentfs_model::Result;
use agentfs_s3::S3Backend;
use std::sync::Arc;

#[derive(Debug)]
pub struct StorageSettings {
    pub engine: Arc<Engine>,
    pub runtime: tokio::runtime::Handle,
}

impl ConfigApplier for StorageSettings {
    fn apply(
        &self,
        previous: &RuntimeConfig,
        next: &RuntimeConfig,
        persist: &mut dyn FnMut() -> Result<()>,
    ) -> Result<()> {
        let active = previous.remote.as_ref().filter(|remote| remote.enabled);
        let requested = next.remote.as_ref().filter(|remote| remote.enabled);
        if active == requested {
            return persist();
        }
        self.runtime.block_on(async {
            let remote = if let Some(config) = requested {
                let backend = S3Backend::new(config.clone())?;
                backend.check_connection().await?;
                Some(RemoteStorage {
                    objects: backend.clone(),
                    refs: backend,
                })
            } else {
                None
            };
            self.engine.switch_storage(remote, persist).await
        })
    }
}
