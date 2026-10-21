use agentdrive_config::OpenCsgConfig;
use agentdrive_model::{Error, ErrorCode, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::{Client, Method, RequestBuilder, StatusCode, header};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::json;
use std::time::Duration;
use url::Url;

const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct Repository {
    client: Client,
    base: Url,
    branch: String,
}

#[derive(Deserialize)]
struct Envelope<T> {
    data: T,
}

#[derive(Deserialize)]
struct Branch {
    name: String,
    commit: Commit,
}
#[derive(Deserialize)]
struct Commit {
    id: String,
}
#[derive(Deserialize)]
struct Page {
    #[serde(rename = "Files", default)]
    files: Vec<Entry>,
    #[serde(rename = "Cursor", default)]
    cursor: String,
}

#[derive(Deserialize)]
pub(crate) struct Entry {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
}

impl Repository {
    pub fn new(config: &OpenCsgConfig) -> Result<Self> {
        let mut authorization = header::HeaderValue::from_str(&format!("Bearer {}", config.token))
            .map_err(|_| Error::invalid("invalid OpenCSG token"))?;
        authorization.set_sensitive(true);
        let mut headers = header::HeaderMap::new();
        headers.insert(header::AUTHORIZATION, authorization);
        let client = Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(transport_error)?;
        let mut base =
            Url::parse(&config.endpoint).map_err(|_| Error::invalid("invalid OpenCSG domain"))?;
        base.path_segments_mut()
            .map_err(|_| Error::invalid("invalid OpenCSG domain"))?
            .clear()
            .extend(["api", "v1", "codes"])
            .extend(config.repository.split('/'));
        Ok(Self {
            client,
            base,
            branch: config.branch.clone(),
        })
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .expect("validated HTTP URL")
            .extend(path.split('/').filter(|s| !s.is_empty()));
        self.client.request(method, url)
    }

    async fn response<T: DeserializeOwned>(&self, request: RequestBuilder) -> Result<T> {
        let mut response = request.send().await.map_err(transport_error)?;
        let status = response.status();
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err(Error::integrity(
                "OpenCSG response exceeds the supported size",
            ));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(Error::integrity(
                    "OpenCSG response exceeds the supported size",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            // Some deployments return a missing Git file as an HTTP 500 with a structured code.
            let body = serde_json::from_slice::<serde_json::Value>(&bytes).unwrap_or_default();
            let git_code = body.get("code").and_then(|v| v.as_str());
            let message = body.get("msg").and_then(|v| v.as_str()).unwrap_or_default();
            let code =
                if status == StatusCode::INTERNAL_SERVER_ERROR && git_code == Some("GIT-ERR-15") {
                    ErrorCode::NotFound
                } else if git_code == Some("GIT-ERR-22")
                    && message.contains("A file with this name already exists")
                {
                    ErrorCode::AlreadyExists
                } else if git_code == Some("GIT-ERR-22") {
                    // Deployments can redact the underlying Git rejection. Callers
                    // verify the branch head and stored bytes before resolving it.
                    ErrorCode::Conflict
                } else {
                    match status.as_u16() {
                        401 | 403 => ErrorCode::PermissionDenied,
                        404 => ErrorCode::NotFound,
                        409 | 412 => ErrorCode::Conflict,
                        400 | 422 => ErrorCode::InvalidArgument,
                        413 => ErrorCode::CapacityExceeded,
                        _ => ErrorCode::Unavailable,
                    }
                };
            let error = Error::new(
                code,
                format!("OpenCSG request failed (HTTP {})", status.as_u16()),
            );
            return Err(
                if matches!(code, ErrorCode::Unavailable | ErrorCode::Conflict) {
                    error.retryable()
                } else {
                    error
                },
            );
        }
        if bytes.is_empty() {
            return serde_json::from_value(serde_json::Value::Null)
                .map_err(|_| Error::integrity("OpenCSG returned an empty response"));
        }
        serde_json::from_slice::<Envelope<T>>(&bytes)
            .map(|envelope| envelope.data)
            .map_err(|_| Error::integrity("OpenCSG returned an invalid response"))
    }

    pub async fn head(&self) -> Result<String> {
        let branches: Vec<Branch> = self.response(self.request(Method::GET, "branches")).await?;
        let id = branches
            .into_iter()
            .find(|branch| branch.name == self.branch)
            .ok_or_else(|| {
                Error::new(ErrorCode::NotFound, "OpenCSG storage branch does not exist")
            })?
            .commit
            .id;
        validate_commit(&id)?;
        Ok(id)
    }

    pub async fn read(&self, path: &str, commit: &str) -> Result<Option<Vec<u8>>> {
        match self
            .response::<String>(
                self.request(Method::GET, &format!("raw/{path}"))
                    .query(&[("ref", commit)]),
            )
            .await
        {
            Ok(content) => Ok(Some(content.into_bytes())),
            Err(error) if error.code == ErrorCode::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub async fn entries(&self, path: &str, commit: &str) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        let mut cursor = String::new();
        let mut seen = std::collections::BTreeSet::new();
        loop {
            let request = self
                .request(Method::GET, &format!("refs/{commit}/tree/{path}"))
                .query(&[("limit", "100"), ("cursor", &cursor)]);
            let page: Option<Page> = match self.response(request).await {
                Ok(page) => page,
                Err(error) if error.code == ErrorCode::NotFound => None,
                Err(error) => return Err(error),
            };
            let Some(page) = page else {
                return Ok(entries);
            };
            entries.extend(page.files);
            if entries.len() > 1_000_000 {
                return Err(Error::integrity(
                    "OpenCSG directory exceeds the supported entry count",
                ));
            }
            if page.cursor.is_empty() {
                return Ok(entries);
            }
            if !seen.insert(page.cursor.clone()) {
                return Err(Error::integrity(
                    "OpenCSG directory pagination did not advance",
                ));
            }
            cursor = page.cursor;
        }
    }

    /// start_sha supplies the parent and the expected old branch OID to Gitaly. A stale
    /// parent must fail; the connection probe verifies this behavior on the target service.
    pub async fn write(
        &self,
        method: Method,
        path: &str,
        content: &[u8],
        expected: &str,
    ) -> Result<()> {
        validate_commit(expected)?;
        let body = json!({
            "branch": self.branch, "start_branch": self.branch, "start_sha": expected,
            "content": STANDARD.encode(content), "message": "chore: persist AgentDrive storage data",
        });
        self.response::<serde_json::Value>(
            self.request(method, &format!("raw/{path}")).json(&body),
        )
        .await?;
        Ok(())
    }

    pub async fn remove_probe(&self, path: &str) {
        let request = self.request(Method::DELETE, &format!("raw/{path}")).json(
            &json!({"branch": self.branch, "message": "chore: remove AgentDrive connection probe"}),
        );
        let _ = self.response::<serde_json::Value>(request).await;
    }
}

pub(crate) fn validate_commit(value: &str) -> Result<()> {
    if !matches!(value.len(), 40 | 64) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::integrity("invalid OpenCSG commit identifier"));
    }
    Ok(())
}

fn transport_error(_: reqwest::Error) -> Error {
    // Responses and transport diagnostics may contain server-provided URLs or credentials.
    Error::new(
        ErrorCode::Unavailable,
        "OpenCSG HTTP request could not be completed",
    )
    .retryable()
}
