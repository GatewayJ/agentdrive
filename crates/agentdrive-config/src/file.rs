use crate::{CacheConfig, RuntimeConfig, S3Config};
use agentdrive_model::*;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    net::SocketAddr,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<S3Config>,
    #[serde(flatten)]
    pub startup: StartupConfig,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_origins: Vec<String>,
    #[serde(default, skip_serializing_if = "is_default_cache")]
    pub cache: CacheConfig,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupConfig {
    pub data_dir: PathBuf,
    pub listen: SocketAddr,
    pub mount_roots: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub directory_roots: Vec<PathBuf>,
    pub credentials: Vec<TokenFile>,
    #[serde(
        default = "administrators",
        skip_serializing_if = "is_default_administrators"
    )]
    pub administrators: Vec<Principal>,
}
fn is_default_cache(cache: &CacheConfig) -> bool {
    cache == &CacheConfig::default()
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenFile {
    pub principal: Principal,
    pub token_file: PathBuf,
}
impl Config {
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self> {
        let mut value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|error| Error::invalid(error.to_string()))?;
        // 已保存的配置继续使用容量设置，清理阈值由程序管理。
        if let Some(cache) = value
            .get_mut("cache")
            .and_then(|cache| cache.as_object_mut())
        {
            cache.remove("target_bytes");
            cache.remove("min_free_bytes");
        }
        let config: Self =
            serde_json::from_value(value).map_err(|error| Error::invalid(error.to_string()))?;
        config.validate()?;
        Ok(config)
    }
    pub fn runtime(&self) -> RuntimeConfig {
        RuntimeConfig {
            remote: self.remote.clone(),
            cache: self.cache.clone(),
            allowed_origins: self.allowed_origins.clone(),
        }
    }
    pub(crate) fn with_runtime(startup: StartupConfig, runtime: RuntimeConfig) -> Self {
        Self {
            remote: runtime.remote,
            startup,
            cache: runtime.cache,
            allowed_origins: runtime.allowed_origins,
        }
    }
    fn validate(&self) -> Result<()> {
        if !self.startup.listen.ip().is_loopback() {
            return Err(Error::invalid(
                "MCP must listen on a loopback address; use an authenticated TLS proxy for remote access",
            ));
        }
        for path in std::iter::once(&self.startup.data_dir)
            .chain(self.startup.mount_roots.iter())
            .chain(self.startup.directory_roots.iter())
            .chain(
                self.startup
                    .credentials
                    .iter()
                    .map(|entry| &entry.token_file),
            )
        {
            if !path.is_absolute() {
                return Err(Error::invalid(
                    "configured filesystem paths must be absolute",
                ));
            }
        }
        if self.startup.credentials.is_empty() {
            return Err(Error::invalid("at least one credential is required"));
        }
        self.runtime().validate()
    }
}
impl StartupConfig {
    pub fn credentials(&self) -> Result<Vec<Credential>> {
        self.credentials
            .iter()
            .map(|entry| {
                let metadata = fs::symlink_metadata(&entry.token_file)?;
                if !metadata.is_file() {
                    return Err(Error::invalid("token must be a regular file"));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if metadata.permissions().mode() & 0o077 != 0 {
                        return Err(Error::new(
                            ErrorCode::PermissionDenied,
                            "token file must be accessible only to its owner",
                        ));
                    }
                }
                if metadata.len() > 4096 {
                    return Err(Error::invalid("token file is too large"));
                }
                let token = fs::read_to_string(&entry.token_file)?.trim().to_owned();
                Ok(Credential {
                    principal: entry.principal.clone(),
                    token,
                })
            })
            .collect()
    }
}
fn create_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
pub fn initialize(directory: &Path, listen: SocketAddr) -> Result<PathBuf> {
    fs::create_dir_all(directory)?;
    let directory = directory.canonicalize()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    }
    let config_path = directory.join("agentdrive.json");
    if config_path.exists() || directory.join("token").exists() {
        return Err(Error::new(
            ErrorCode::AlreadyExists,
            "configuration or token already exists",
        ));
    }
    let mounts = directory.join("mounts");
    fs::create_dir_all(&mounts)?;
    let exchange = directory.join("exchange");
    fs::create_dir_all(&exchange)?;
    let token_file = directory.join("token");
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    create_file(&token_file, token.as_bytes())?;
    let config = Config {
        remote: None,
        startup: StartupConfig {
            data_dir: directory.join("state"),
            listen,
            mount_roots: vec![mounts],
            directory_roots: vec![exchange],
            credentials: vec![TokenFile {
                principal: "owner".to_owned().try_into()?,
                token_file,
            }],
            administrators: administrators(),
        },
        allowed_origins: vec![],
        cache: CacheConfig::default(),
    };
    create_file(&config_path, &serde_json::to_vec_pretty(&config)?)?;
    Ok(config_path)
}

fn administrators() -> Vec<Principal> {
    vec![
        "owner"
            .to_owned()
            .try_into()
            .expect("valid built-in principal"),
    ]
}
fn is_default_administrators(value: &[Principal]) -> bool {
    value == administrators()
}

pub struct Credential {
    pub principal: Principal,
    pub token: String,
}
impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential")
            .field("principal", &self.principal)
            .finish_non_exhaustive()
    }
}
