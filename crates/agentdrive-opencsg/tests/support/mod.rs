use agentdrive_config::OpenCsgConfig;
use agentdrive_opencsg::OpenCsgBackend;
use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

#[derive(Default)]
pub struct Data {
    pub generation: u64,
    pub snapshots: BTreeMap<String, BTreeMap<String, String>>,
    pub ignore_start_sha: bool,
    pub ignore_create_start_sha: bool,
    pub overwrite_created: bool,
    pub lose_write_response: bool,
    pub lose_read_response: bool,
    pub lose_confirmation: bool,
    pub advance_after_write: bool,
    pub probe_conflicts: usize,
    pub fail_stale_probe: bool,
    pub redact_commit_errors: bool,
    pub unavailable: bool,
    pub redirect: Option<String>,
    pub requests: usize,
}
impl Data {
    pub fn head(&self) -> String {
        format!("{:040x}", self.generation)
    }
    pub fn files(&self) -> &BTreeMap<String, String> {
        self.snapshots.get(&self.head()).unwrap()
    }
    pub fn publish(&mut self, files: BTreeMap<String, String>) {
        self.generation += 1;
        self.snapshots.insert(self.head(), files);
    }
}

pub struct Server {
    pub data: Arc<Mutex<Data>>,
    pub config: OpenCsgConfig,
    task: tokio::task::JoinHandle<()>,
}
impl Server {
    pub async fn new() -> Self {
        let mut data = Data::default();
        data.publish(BTreeMap::from([(
            "README.md".into(),
            "Storage repository".into(),
        )]));
        let data = Arc::new(Mutex::new(data));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .fallback(handle)
            .with_state(data.clone())
            .layer(DefaultBodyLimit::max(16 * 1024 * 1024));
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let config = OpenCsgConfig {
            enabled: true,
            endpoint: format!("http://{address}"),
            repository: "test/storage".into(),
            branch: "main".into(),
            prefix: "agentdrive/v1".into(),
            token: "private-test-token".into(),
            allow_http: true,
        };
        Self { data, config, task }
    }
    pub fn backend(&self) -> Arc<OpenCsgBackend> {
        OpenCsgBackend::new(self.config.clone()).unwrap()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn ok(data: Value) -> Response {
    axum::Json(json!({"msg":"OK", "data":data})).into_response()
}
fn error(status: StatusCode) -> Response {
    (
        status,
        axum::Json(json!({"msg":"private-test-token must never appear in client diagnostics"})),
    )
        .into_response()
}

fn commit_error(data: &Data, detail: &str) -> Response {
    let message = if data.redact_commit_errors {
        "GIT-ERR-22: Commit git files failed"
    } else {
        detail
    };
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        axum::Json(json!({"code":"GIT-ERR-22", "msg":message})),
    )
        .into_response()
}

async fn handle(
    State(state): State<Arc<Mutex<Data>>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let mut data = state.lock().unwrap();
    data.requests += 1;
    if let Some(location) = &data.redirect {
        return (StatusCode::FOUND, [("location", location)]).into_response();
    }
    if headers.get("authorization").and_then(|v| v.to_str().ok())
        != Some("Bearer private-test-token")
    {
        return error(StatusCode::UNAUTHORIZED);
    }
    if data.unavailable {
        return error(StatusCode::SERVICE_UNAVAILABLE);
    }
    if method == Method::GET && data.lose_read_response {
        data.lose_read_response = false;
        return error(StatusCode::SERVICE_UNAVAILABLE);
    }
    let Some(path) = uri.path().strip_prefix("/api/v1/codes/test/storage/") else {
        return error(StatusCode::NOT_FOUND);
    };
    let url = url::Url::parse(&format!("http://localhost{uri}")).unwrap();
    let query: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
    if path == "branches" {
        return ok(json!([{"name":"main", "commit":{"id":data.head()}}]));
    }
    if let Some(rest) = path.strip_prefix("refs/") {
        let Some((commit, directory)) = rest.split_once("/tree/") else {
            return error(StatusCode::NOT_FOUND);
        };
        let Some(files) = data.snapshots.get(commit) else {
            return error(StatusCode::NOT_FOUND);
        };
        let prefix = format!("{directory}/");
        let mut entries = BTreeMap::new();
        for name in files.keys().filter_map(|path| path.strip_prefix(&prefix)) {
            let (name, kind) = match name.split_once('/') {
                Some((name, _)) => (name, "dir"),
                None => (name, "file"),
            };
            entries.insert(name.to_string(), json!({"name":name,"type":kind}));
        }
        let offset = query
            .get("cursor")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0);
        let limit = query
            .get("limit")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(100);
        let cursor = if offset + limit < entries.len() {
            (offset + limit).to_string()
        } else {
            String::new()
        };
        return ok(
            json!({"Files":entries.into_values().skip(offset).take(limit).collect::<Vec<_>>(),"Cursor":cursor}),
        );
    }
    let Some(path) = path.strip_prefix("raw/") else {
        return error(StatusCode::NOT_FOUND);
    };
    if method == Method::GET {
        let head = data.head();
        let commit = query.get("ref").unwrap_or(&head);
        return match data.snapshots.get(commit).and_then(|files| files.get(path)) {
            Some(content) => ok(content.clone().into()),
            None => error(StatusCode::NOT_FOUND),
        };
    }
    let body: Value = match serde_json::from_slice(&body) {
        Ok(body) => body,
        Err(_) => return error(StatusCode::BAD_REQUEST),
    };
    if method == Method::DELETE {
        let mut files = data.files().clone();
        files.remove(path);
        data.publish(files);
        return ok(Value::Null);
    }
    let content = STANDARD.decode(body["content"].as_str().unwrap()).unwrap();
    if content == b"updated" && data.probe_conflicts > 0 {
        data.probe_conflicts -= 1;
        let files = data.files().clone();
        data.publish(files);
    }
    if content == b"stale" && data.fail_stale_probe {
        return error(StatusCode::SERVICE_UNAVAILABLE);
    }
    let ignore_start_sha =
        data.ignore_start_sha || (method == Method::POST && data.ignore_create_start_sha);
    if !ignore_start_sha && body["start_sha"].as_str() != Some(data.head().as_str()) {
        return commit_error(&data, "reference does not point to expected object");
    }
    let mut files = data.files().clone();
    if method == Method::POST && files.contains_key(path) && !data.overwrite_created {
        return commit_error(&data, "rpc error: A file with this name already exists");
    }
    if method == Method::PUT && !files.contains_key(path) {
        return error(StatusCode::NOT_FOUND);
    }
    files.insert(path.into(), String::from_utf8(content).unwrap());
    data.publish(files);
    if data.advance_after_write {
        data.advance_after_write = false;
        let mut files = data.files().clone();
        files.insert(path.into(), "advanced".into());
        data.publish(files);
    }
    if data.lose_confirmation {
        data.lose_confirmation = false;
        data.lose_read_response = true;
    }
    if data.lose_write_response {
        data.lose_write_response = false;
        return error(StatusCode::SERVICE_UNAVAILABLE);
    }
    ok(Value::Null)
}
