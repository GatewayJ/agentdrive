use agentfs_model::{Error, MAX_METADATA_BYTES, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct CacheConfig {
    pub max_bytes: u64,
    pub target_bytes: u64,
    pub min_free_bytes: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            max_bytes: 2 * 1024 * 1024 * 1024,
            target_bytes: 1536 * 1024 * 1024,
            min_free_bytes: 64 * 1024 * 1024,
        }
    }
}

impl CacheConfig {
    pub fn validate(&self) -> Result<()> {
        if self.target_bytes >= self.max_bytes {
            return Err(Error::invalid("cache target must be below its limit"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidationConfig {
    pub image: String,
    pub program: PathBuf,
    pub arguments: Vec<String>,
    pub timeout_seconds: u64,
    pub max_output_bytes: usize,
}

impl ValidationConfig {
    pub fn validate(&self) -> Result<()> {
        if self.image.is_empty()
            || self.image.starts_with('-')
            || self.image.chars().any(char::is_whitespace)
            || self.program.as_os_str().is_empty()
            || self.timeout_seconds == 0
            || self.timeout_seconds > 3600
            || self.max_output_bytes == 0
            || self.max_output_bytes > MAX_METADATA_BYTES
        {
            return Err(Error::invalid(
                "invalid validation program, limits or image",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct S3Config {
    pub bucket: String,
    #[serde(default = "default_prefix")]
    pub prefix: String,
    #[serde(default = "default_region")]
    pub region: String,
    pub endpoint: Option<String>,
    #[serde(default)]
    pub allow_http: bool,
}
fn default_prefix() -> String {
    "agentfs/v1".into()
}
fn default_region() -> String {
    "us-east-1".into()
}
