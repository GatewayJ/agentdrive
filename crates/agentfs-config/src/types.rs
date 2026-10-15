use agentfs_model::{Error, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct CacheConfig {
    pub max_bytes: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            max_bytes: 2 * 1024 * 1024 * 1024,
        }
    }
}

impl CacheConfig {
    pub fn validate(&self) -> Result<()> {
        if self.max_bytes == 0 {
            return Err(Error::invalid("cache capacity must be greater than zero"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct S3Config {
    #[serde(default = "enabled")]
    pub enabled: bool,
    pub bucket: String,
    #[serde(default = "default_prefix")]
    pub prefix: String,
    #[serde(default = "default_region")]
    pub region: String,
    pub endpoint: Option<String>,
    #[serde(default)]
    pub allow_http: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials: Option<S3Credentials>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct S3Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_token: Option<String>,
}

impl std::fmt::Debug for S3Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Credentials").finish_non_exhaustive()
    }
}

impl S3Config {
    pub fn validate(&self) -> Result<()> {
        if self.bucket.trim().is_empty()
            || self.bucket.contains(['/', '\\'])
            || self.bucket.chars().any(char::is_whitespace)
            || self.region.trim().is_empty()
            || self.prefix.trim_matches('/').is_empty()
            || self
                .prefix
                .split('/')
                .any(|part| matches!(part, "." | ".."))
            || self.prefix.chars().any(char::is_control)
        {
            return Err(Error::invalid("invalid S3 bucket, region or prefix"));
        }
        if let Some(endpoint) = &self.endpoint {
            let url =
                url::Url::parse(endpoint).map_err(|_| Error::invalid("invalid S3 endpoint"))?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
                || (url.scheme() == "http" && !self.allow_http)
            {
                return Err(Error::invalid(
                    "S3 endpoint requires HTTP(S), no credentials, query or fragment, and allow_http for HTTP",
                ));
            }
        }
        if let Some(credentials) = &self.credentials
            && (credentials.access_key_id.trim().is_empty()
                || credentials.secret_access_key.is_empty())
        {
            return Err(Error::invalid(
                "S3 access key and secret key must both be provided",
            ));
        }
        Ok(())
    }

    pub(crate) fn redact(&mut self) {
        if let Some(credentials) = &mut self.credentials {
            credentials.secret_access_key.clear();
            credentials.session_token = None;
        }
    }
}
fn default_prefix() -> String {
    "agentfs/v1".into()
}
fn default_region() -> String {
    "us-east-1".into()
}

fn enabled() -> bool {
    true
}
