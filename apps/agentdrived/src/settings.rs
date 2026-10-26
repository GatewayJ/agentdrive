use agentdrive_config::{ConfigApplier, RemoteConfig, RuntimeConfig};
use agentdrive_engine::{Engine, RemoteStorage};
use agentdrive_model::Result;
use agentdrive_opencsg::OpenCsgBackend;
use agentdrive_s3::S3Backend;
use std::sync::Arc;

pub fn remote_storage(config: Option<&RemoteConfig>) -> Result<Option<RemoteStorage>> {
    let Some(config) = config.filter(|config| config.enabled()) else {
        return Ok(None);
    };
    Ok(Some(match config {
        RemoteConfig::S3(config) => {
            let backend = S3Backend::new(config.clone())?;
            RemoteStorage {
                objects: backend.clone(),
                refs: backend,
            }
        }
        RemoteConfig::OpenCsg(config) => {
            let backend = OpenCsgBackend::new(config.clone())?;
            RemoteStorage {
                objects: backend.clone(),
                refs: backend,
            }
        }
    }))
}

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
        let active = previous.remote.as_ref().filter(|remote| remote.enabled());
        let requested = next.remote.as_ref().filter(|remote| remote.enabled());
        if active == requested {
            return persist();
        }
        self.runtime.block_on(async {
            let remote = remote_storage(requested)?;
            if let Some(remote) = &remote {
                remote.refs.check_connection().await?;
            }
            self.engine.switch_storage(remote, persist).await
        })
    }
}
