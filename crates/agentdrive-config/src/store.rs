use crate::{CacheConfig, Config, RemoteConfig, StartupConfig};
use agentdrive_model::{Error, ErrorCode, Principal, Result};
use arc_swap::ArcSwap;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

const MAX_CONFIG_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    #[serde(default)]
    pub remote: Option<RemoteConfig>,
    pub cache: CacheConfig,
    pub allowed_origins: Vec<String>,
}

impl RuntimeConfig {
    pub fn validate(&self) -> Result<()> {
        self.cache.validate()?;
        if let Some(remote) = &self.remote {
            remote.validate()?;
        }
        for origin in &self.allowed_origins {
            let url = url::Url::parse(origin)
                .map_err(|_| Error::invalid("allowed_origins must contain HTTP(S) origins"))?;
            if !matches!(url.scheme(), "http" | "https")
                || url.origin().ascii_serialization() != *origin
            {
                return Err(Error::invalid(
                    "allowed_origins must use scheme://host[:port] without a path, credentials, wildcards or a default port",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub revision: String,
    pub values: RuntimeConfig,
    pub reload_error: Option<String>,
}

/// 每次操作取得一份快照，并在操作结束前保持该版本。
#[derive(Debug)]
pub struct LiveConfig(ArcSwap<Snapshot>);

impl LiveConfig {
    pub fn new(values: RuntimeConfig) -> Result<Arc<Self>> {
        values.validate()?;
        Ok(Arc::new(Self(ArcSwap::from_pointee(Snapshot {
            revision: revision(&serde_json::to_vec(&values)?),
            values,
            reload_error: None,
        }))))
    }

    pub fn load(&self) -> Arc<Snapshot> {
        self.0.load_full()
    }
}

#[derive(Debug, Serialize)]
pub struct Settings {
    pub config_path: PathBuf,
    pub revision: String,
    pub runtime: RuntimeConfig,
    pub startup: StartupConfig,
    pub reload_error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsUpdate {
    pub revision: String,
    pub runtime: RuntimeConfig,
}

/// 配置文件替换与资源切换共用一次操作，准备失败时保留当前配置。
pub trait ConfigApplier: Send + Sync + std::fmt::Debug {
    fn apply(
        &self,
        previous: &RuntimeConfig,
        next: &RuntimeConfig,
        persist: &mut dyn FnMut() -> Result<()>,
    ) -> Result<()>;
}

/// 写入与文件加载使用同一个锁；业务操作通过 ArcSwap 读取配置。
#[derive(Debug)]
pub struct ConfigStore {
    path: PathBuf,
    startup: StartupConfig,
    live: Arc<LiveConfig>,
    writer: Mutex<()>,
    applier: OnceLock<Arc<dyn ConfigApplier>>,
}

impl ConfigStore {
    pub fn open(path: &Path) -> Result<Arc<Self>> {
        let path = path.canonicalize()?;
        let bytes = read(&path)?;
        let config = Config::decode(&bytes)?;
        let live = Arc::new(LiveConfig(ArcSwap::from_pointee(Snapshot {
            revision: revision(&bytes),
            values: config.runtime(),
            reload_error: None,
        })));
        Ok(Arc::new(Self {
            path,
            startup: config.startup,
            live,
            writer: Mutex::new(()),
            applier: OnceLock::new(),
        }))
    }

    pub fn set_applier(&self, applier: Arc<dyn ConfigApplier>) -> Result<()> {
        self.applier.set(applier).map_err(|_| {
            Error::new(
                ErrorCode::Conflict,
                "configuration applier is already installed",
            )
        })
    }

    fn apply(
        &self,
        previous: &RuntimeConfig,
        next: &RuntimeConfig,
        persist: &mut dyn FnMut() -> Result<()>,
    ) -> Result<()> {
        match self.applier.get() {
            Some(applier) => applier.apply(previous, next, persist),
            None => persist(),
        }
    }

    pub fn startup(&self) -> &StartupConfig {
        &self.startup
    }

    pub fn live(&self) -> Arc<LiveConfig> {
        self.live.clone()
    }

    fn authorize(&self, actor: &Principal) -> Result<()> {
        if !self.startup.administrators.contains(actor) {
            return Err(Error::new(
                ErrorCode::PermissionDenied,
                "daemon administrator access is required",
            ));
        }
        Ok(())
    }

    pub fn settings(&self, actor: &Principal) -> Result<Settings> {
        self.authorize(actor)?;
        Ok(self.view())
    }

    fn view(&self) -> Settings {
        let snapshot = self.live.load();
        let mut runtime = snapshot.values.clone();
        if let Some(remote) = &mut runtime.remote {
            remote.redact();
        }
        Settings {
            config_path: self.path.clone(),
            revision: snapshot.revision.clone(),
            runtime,
            startup: self.startup.clone(),
            reload_error: snapshot.reload_error.clone(),
        }
    }

    pub fn reload(&self) -> Result<bool> {
        let _writer = self.writer.lock();
        let result = (|| {
            let bytes = read(&self.path)?;
            let revision = revision(&bytes);
            let current = self.live.load();
            if revision == current.revision && current.reload_error.is_none() {
                return Ok(false);
            }
            let config = Config::decode(&bytes)?;
            if config.startup != self.startup {
                let old = serde_json::to_value(&self.startup)?;
                let new = serde_json::to_value(&config.startup)?;
                let changed: Vec<_> = old
                    .as_object()
                    .expect("startup object")
                    .keys()
                    .chain(new.as_object().expect("startup object").keys())
                    .filter(|key| old[*key] != new[*key])
                    .cloned()
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect();
                return Err(Error::new(
                    ErrorCode::Conflict,
                    format!(
                        "restart required for {}; active configuration was retained",
                        changed.join(", ")
                    ),
                ));
            }
            self.apply(&current.values, &config.runtime(), &mut || {
                if self::revision(&read(&self.path)?) != revision {
                    return Err(Error::new(
                        ErrorCode::Conflict,
                        "configuration changed during reload",
                    ));
                }
                Ok(())
            })?;
            self.live.0.store(Arc::new(Snapshot {
                revision,
                values: config.runtime(),
                reload_error: None,
            }));
            Ok(true)
        })();
        if let Err(error) = &result {
            let current = self.live.load();
            if current.reload_error.as_deref() != Some(&error.to_string()) {
                let mut snapshot = (*current).clone();
                snapshot.reload_error = Some(error.to_string());
                self.live.0.store(Arc::new(snapshot));
            }
        }
        result
    }

    pub fn update(&self, actor: &Principal, mut update: SettingsUpdate) -> Result<Settings> {
        self.authorize(actor)?;
        let _writer = self.writer.lock();
        let current = self.live.load();
        if current.revision != update.revision || revision(&read(&self.path)?) != update.revision {
            return Err(Error::new(
                ErrorCode::Conflict,
                "configuration changed; reload settings before saving",
            ));
        }
        if let Some(remote) = &mut update.runtime.remote {
            remote.restore_credentials(current.values.remote.as_ref())?;
        }
        update.runtime.validate()?;
        let config = Config::with_runtime(self.startup.clone(), update.runtime);
        let bytes = serde_json::to_vec_pretty(&config)?;
        if bytes.len() > MAX_CONFIG_BYTES {
            return Err(Error::invalid("configuration exceeds 1 MiB"));
        }
        let parent = self.path.parent().expect("absolute configuration path");
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            temporary
                .as_file()
                .set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        #[cfg(not(unix))]
        temporary
            .as_file()
            .set_permissions(fs::metadata(&self.path)?.permissions())?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        let mut temporary = Some(temporary);
        self.apply(&current.values, &config.runtime(), &mut || {
            if revision(&read(&self.path)?) != update.revision {
                return Err(Error::new(
                    ErrorCode::Conflict,
                    "configuration changed; reload settings before saving",
                ));
            }
            temporary
                .take()
                .ok_or_else(|| Error::integrity("configuration was already persisted"))?
                .persist(&self.path)
                .map_err(|error| Error::from(error.error))?;
            Ok(())
        })?;
        let reload_error = sync_directory(parent)
            .err()
            .map(|error| format!("configuration applied; directory sync failed: {error}"));
        let snapshot = Snapshot {
            revision: revision(&bytes),
            values: config.runtime(),
            reload_error,
        };
        self.live.0.store(Arc::new(snapshot));
        Ok(self.view())
    }
}

fn read(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take((MAX_CONFIG_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(Error::invalid("configuration exceeds 1 MiB"));
    }
    Ok(bytes)
}

fn revision(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn sync_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        fs::File::open(path)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}
