use agentfs_model::{Error, ErrorCode, ObjectKind, OperationId, Result, WorkspaceId, object_ref};
use clap::{Args, ValueEnum};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    ffi::OsString,
    path::PathBuf,
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    process::Command,
};

const MAX_MESSAGE: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum Protocol {
    Acp,
    CodexAppServer,
}

#[derive(Debug, Args)]
pub struct Options {
    #[arg(long, value_enum)]
    protocol: Protocol,
    #[arg(long)]
    workspace: WorkspaceId,
    #[arg(long)]
    session_id: String,
    #[arg(long)]
    mount_path: PathBuf,
    #[arg(required = true, last = true)]
    command: Vec<OsString>,
}

struct Lifecycle {
    http: reqwest::Client,
    endpoint: String,
    token: String,
    key: Value,
}

impl Lifecycle {
    async fn event(&self, request_id: &str, event: &str, fields: Value) -> Result<Value> {
        let mut request = fields;
        request["event"] = json!(event);
        request["key"] = self.key.clone();
        request["request_id"] = json!(request_id);
        for attempt in 0..3 {
            let response = async {
                let response = self
                    .http
                    .post(&self.endpoint)
                    .bearer_auth(&self.token)
                    .json(&request)
                    .send()
                    .await?;
                let status = response.status();
                let value = response.json::<Value>().await?;
                Ok::<_, reqwest::Error>((status, value))
            }
            .await;
            match response {
                Ok((status, value)) => {
                    if status.is_success() {
                        return Ok(value);
                    }
                    if let Ok(error) = serde_json::from_value::<Error>(value["error"].clone()) {
                        return Err(error);
                    }
                    return Err(Error::new(
                        ErrorCode::Unavailable,
                        format!("lifecycle request failed: {status}"),
                    ));
                }
                Err(error) if attempt == 2 => {
                    return Err(Error::new(ErrorCode::Unavailable, error.to_string()));
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(100 * (attempt + 1))).await,
            }
        }
        unreachable!()
    }

    async fn begin(&self, turn: &str) -> Result<()> {
        let receipt = self
            .event(&format!("{turn}-begin"), "begin", json!({"turn_id": turn}))
            .await?;
        if receipt["result"]["record"]["status"] != "running" {
            return Err(Error::new(
                ErrorCode::TurnResultConflict,
                "turn already completed",
            ));
        }
        Ok(())
    }

    async fn end(&self, turn: &str, status: &str) -> Result<()> {
        self.event(
            &format!("{turn}-end-{status}"),
            "end",
            json!({"turn_id": turn, "status": status}),
        )
        .await?;
        Ok(())
    }
}

struct Pending {
    fingerprint: String,
    method: String,
}

struct ActiveTurn {
    local: String,
    request: Option<String>,
    thread: String,
    external: Option<String>,
    review: bool,
}

struct Bridge {
    protocol: Protocol,
    mount_path: PathBuf,
    lifecycle: Lifecycle,
    pending: BTreeMap<String, Pending>,
    replies: VecDeque<(String, String, Value, usize)>,
    reply_bytes: usize,
    active: Option<ActiveTurn>,
    completed: VecDeque<(String, String)>,
}

fn id(message: &Value) -> Option<String> {
    message.get("id").map(Value::to_string)
}
fn text(message: &Value, name: &str) -> Result<String> {
    message
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| Error::invalid(format!("missing {name}")))
}
fn rpc_error(message: &Value, error: &Error) -> Value {
    json!({"jsonrpc":"2.0", "id":message["id"], "error":{"code":-32000,"message":error.to_string()}})
}

impl Bridge {
    async fn finish(&mut self, status: &str) -> Result<()> {
        if let Some(active) = &self.active {
            self.lifecycle.end(&active.local, status).await?;
            if let Some(external) = &active.external {
                self.completed
                    .push_back((active.thread.clone(), external.clone()));
                if self.completed.len() > 256 {
                    self.completed.pop_front();
                }
            }
            self.active = None;
        }
        Ok(())
    }

    async fn client(
        &mut self,
        mut message: Value,
        child: &mut (impl AsyncWrite + Unpin),
        output: &mut (impl AsyncWrite + Unpin),
    ) -> Result<()> {
        let Some(method) = message
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return send(child, &message).await;
        };
        let request = id(&message);
        let fingerprint = object_ref(ObjectKind::RecordIndex, &serde_json::to_vec(&message)?)
            .id
            .to_string();
        if let Some(request) = &request {
            if let Some(pending) = self.pending.get(request) {
                if pending.fingerprint != fingerprint {
                    send(
                        output,
                        &rpc_error(&message, &Error::invalid("request id is already in use")),
                    )
                    .await?;
                }
                return Ok(());
            }
            if let Some((_, previous, reply, _)) =
                self.replies.iter().find(|(key, _, _, _)| key == request)
            {
                let reply = if previous == &fingerprint {
                    reply.clone()
                } else {
                    rpc_error(
                        &message,
                        &Error::invalid("request id was reused with different parameters"),
                    )
                };
                return send(output, &reply).await;
            }
            if self.pending.len() >= 1024 {
                return send(
                    output,
                    &rpc_error(
                        &message,
                        &Error::new(ErrorCode::Busy, "too many pending requests"),
                    ),
                )
                .await;
            }
        }
        let setup = match self.protocol {
            Protocol::Acp => matches!(
                method.as_str(),
                "session/new" | "session/load" | "session/resume"
            ),
            Protocol::CodexAppServer => matches!(
                method.as_str(),
                "thread/start" | "thread/resume" | "thread/fork"
            ),
        };
        let begin = match self.protocol {
            Protocol::Acp => method == "session/prompt",
            Protocol::CodexAppServer => matches!(method.as_str(), "turn/start" | "review/start"),
        };
        if setup || (self.protocol == Protocol::CodexAppServer && method == "turn/start") {
            if message.get("params").is_none_or(Value::is_null) {
                message["params"] = json!({});
            }
            if !message["params"].is_object() {
                return send(
                    output,
                    &rpc_error(&message, &Error::invalid("params must be an object")),
                )
                .await;
            }
            message["params"]["cwd"] = json!(self.mount_path);
        }
        if begin {
            let result = async {
                if self.active.is_some() {
                    return Err(Error::new(
                        ErrorCode::BranchBusy,
                        "workspace already has an active turn",
                    ));
                }
                let thread = text(
                    &message["params"],
                    if self.protocol == Protocol::Acp {
                        "sessionId"
                    } else {
                        "threadId"
                    },
                )?;
                let local = OperationId::new().to_string();
                self.lifecycle.begin(&local).await?;
                self.active = Some(ActiveTurn {
                    local,
                    request: request.clone(),
                    thread,
                    external: None,
                    review: self.protocol == Protocol::CodexAppServer && method == "review/start",
                });
                Ok(())
            }
            .await;
            if let Err(error) = result {
                return send(output, &rpc_error(&message, &error)).await;
            }
        }
        if let Some(request) = request {
            self.pending.insert(
                request,
                Pending {
                    fingerprint,
                    method,
                },
            );
        }
        send(child, &message).await
    }

    async fn agent(
        &mut self,
        message: Value,
        output: &mut (impl AsyncWrite + Unpin),
    ) -> Result<()> {
        if self.protocol == Protocol::CodexAppServer {
            let method = message.get("method").and_then(Value::as_str);
            if matches!(method, Some("turn/started" | "turn/completed")) {
                let thread = text(&message["params"], "threadId")?;
                let external = text(&message["params"]["turn"], "id")?;
                let completed = self
                    .completed
                    .iter()
                    .any(|entry| entry == &(thread.clone(), external.clone()));
                if let Some(active) = &mut self.active
                    && active.review
                    && active.external.is_none()
                {
                    active.thread = thread.clone();
                }
                if method == Some("turn/started") && !completed {
                    if let Some(active) = &mut self.active {
                        if active.thread != thread
                            || active.external.as_ref().is_some_and(|id| id != &external)
                        {
                            return Err(Error::new(
                                ErrorCode::BranchBusy,
                                "received overlapping agent turns",
                            ));
                        }
                        active.external = Some(external);
                    } else {
                        let local = object_ref(
                            ObjectKind::RecordIndex,
                            format!("{thread}:{external}").as_bytes(),
                        )
                        .id
                        .to_string();
                        self.lifecycle.begin(&local).await?;
                        self.active = Some(ActiveTurn {
                            local,
                            request: None,
                            thread,
                            external: Some(external),
                            review: false,
                        });
                    }
                } else if method == Some("turn/completed") && !completed {
                    let active = self.active.as_mut().ok_or_else(|| {
                        Error::invalid("completion arrived without a managed turn")
                    })?;
                    if active.thread != thread
                        || active.external.as_ref().is_some_and(|id| id != &external)
                    {
                        return Err(Error::invalid("completion does not match the managed turn"));
                    }
                    active.external = Some(external);
                    let status = match message["params"]["turn"]["status"].as_str() {
                        Some("completed") => "completed",
                        Some("failed") => "failed",
                        Some("interrupted") => "cancelled",
                        _ => return Err(Error::invalid("unknown app-server turn status")),
                    };
                    self.finish(status).await?;
                }
            }
        }
        if message.get("method").is_none()
            && let Some(request) = id(&message)
            && let Some(pending) = self.pending.remove(&request)
        {
            if self
                .active
                .as_ref()
                .is_some_and(|active| active.request.as_ref() == Some(&request))
            {
                if message.get("error").is_some() {
                    self.finish("failed").await?;
                } else if self.protocol == Protocol::Acp && pending.method == "session/prompt" {
                    let status = match message["result"]["stopReason"].as_str() {
                        Some("cancelled") => "cancelled",
                        Some("end_turn" | "max_tokens" | "max_turn_requests" | "refusal") => {
                            "completed"
                        }
                        _ => return Err(Error::invalid("unknown ACP stop reason")),
                    };
                    self.finish(status).await?;
                } else if self.protocol == Protocol::CodexAppServer
                    && matches!(pending.method.as_str(), "turn/start" | "review/start")
                {
                    let external = text(&message["result"]["turn"], "id")?;
                    if let Some(active) = &mut self.active {
                        active.external = Some(external);
                        if active.review {
                            active.thread = text(&message["result"], "reviewThreadId")?;
                        }
                    }
                }
            }
            let size = serde_json::to_vec(&message)?.len();
            self.reply_bytes += size;
            self.replies
                .push_back((request, pending.fingerprint, message.clone(), size));
            while self.reply_bytes > 16 * 1024 * 1024 || self.replies.len() > 256 {
                if let Some((_, _, _, size)) = self.replies.pop_front() {
                    self.reply_bytes -= size;
                }
            }
        }
        send(output, &message).await
    }
}

struct MessageReader<R> {
    reader: R,
    bytes: Vec<u8>,
}
impl<R: AsyncBufRead + Unpin> MessageReader<R> {
    fn new(reader: R) -> Self {
        Self {
            reader,
            bytes: Vec::new(),
        }
    }
    async fn receive(&mut self) -> Result<Option<Value>> {
        loop {
            let buffer = self.reader.fill_buf().await?;
            if buffer.is_empty() {
                return if self.bytes.is_empty() {
                    Ok(None)
                } else {
                    Err(Error::invalid("incomplete protocol message"))
                };
            }
            let end = buffer.iter().position(|byte| *byte == b'\n');
            let take = end.map_or(buffer.len(), |index| index + 1);
            if self.bytes.len() + take > MAX_MESSAGE {
                return Err(Error::invalid("protocol message exceeds 8 MiB"));
            }
            self.bytes.extend_from_slice(&buffer[..take]);
            self.reader.consume(take);
            if end.is_some() {
                let value: Value = serde_json::from_slice(&self.bytes)
                    .map_err(|error| Error::invalid(error.to_string()))?;
                self.bytes.clear();
                if !value.is_object() {
                    return Err(Error::invalid("protocol message must be an object"));
                }
                return Ok(Some(value));
            }
        }
    }
}

async fn send(writer: &mut (impl AsyncWrite + Unpin), value: &Value) -> Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

pub fn base_url(endpoint: &str) -> Result<reqwest::Url> {
    let mut url =
        reqwest::Url::parse(endpoint).map_err(|error| Error::invalid(error.to_string()))?;
    url.set_path("/");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

pub async fn run(endpoint: &str, token: String, options: Options) -> Result<bool> {
    if !options.mount_path.is_absolute() {
        return Err(Error::invalid("mount-path must be absolute"));
    }
    let base = base_url(endpoint)?;
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| Error::invalid(error.to_string()))?;
    let status: Value = http
        .get(format!("{base}api/workspaces/{}", options.workspace))
        .bearer_auth(&token)
        .send()
        .await
        .map_err(|error| Error::new(ErrorCode::Unavailable, error.to_string()))?
        .error_for_status()
        .map_err(|error| Error::new(ErrorCode::Unavailable, error.to_string()))?
        .json()
        .await
        .map_err(|error| Error::new(ErrorCode::Unavailable, error.to_string()))?;
    let namespace = if options.protocol == Protocol::Acp {
        "acp"
    } else {
        "codex-app-server"
    };
    let key = json!({"workspace":options.workspace,"app_namespace":namespace,"session_id":options.session_id,"location":status["status"]["location"]});
    let lock_name = object_ref(ObjectKind::RecordIndex, &serde_json::to_vec(&key)?)
        .id
        .to_string();
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|path| PathBuf::from(path).join(".cache")))
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
        .ok_or_else(|| Error::invalid("a user cache directory is required"))?
        .join("agentfs/integrations");
    std::fs::create_dir_all(&cache)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(cache.join(format!("{lock_name}.lock")))?;
    lock.try_lock().map_err(|_| {
        Error::new(
            ErrorCode::Busy,
            "this integration session is already connected",
        )
    })?;
    let lifecycle = Lifecycle {
        http,
        endpoint: format!("{base}api/lifecycle"),
        token,
        key,
    };
    let connection = OperationId::new().to_string();
    let attached = lifecycle
        .event(
            &format!("{connection}-attach"),
            "attach",
            json!({"mount_path":options.mount_path}),
        )
        .await?;
    let mount_path = PathBuf::from(text(&attached, "mount_path")?);
    let mut bridge = Bridge {
        protocol: options.protocol,
        mount_path,
        lifecycle,
        pending: BTreeMap::new(),
        replies: VecDeque::new(),
        reply_bytes: 0,
        active: None,
        completed: VecDeque::new(),
    };
    let result = async {
        let mut child = Command::new(&options.command[0]).args(&options.command[1..]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).kill_on_drop(true).spawn()?;
        let mut child_input = child.stdin.take().ok_or_else(|| Error::integrity("agent stdin is missing"))?;
        let mut child_output = MessageReader::new(BufReader::new(child.stdout.take().ok_or_else(|| Error::integrity("agent stdout is missing"))?));
        let mut input = MessageReader::new(BufReader::new(tokio::io::stdin()));
        let mut output = tokio::io::stdout();
        let result: Result<()> = async {
            loop {
                tokio::select! {
                    message = input.receive() => match message? {
                        Some(message) => bridge.client(message, &mut child_input, &mut output).await?,
                        None => break,
                    },
                    message = child_output.receive() => match message? {
                        Some(message) => bridge.agent(message, &mut output).await?,
                        None => break,
                    },
                    _ = shutdown() => break,
                }
            }
            Ok(())
        }.await;
        drop(child_input);
        // stdout 关闭和进程退出之间可能存在时间差，等待实际退出状态。
        let status = match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
            Ok(status) => Some(status?),
            Err(_) => { child.kill().await?; child.wait().await?; None },
        };
        result?;
        Ok(status.is_none_or(|status| status.success()))
    }.await;
    let saved = bridge.finish("interrupted").await;
    let detached = if saved.is_ok() {
        bridge
            .lifecycle
            .event(&format!("{connection}-detach"), "detach", json!({}))
            .await
            .map(|_| ())
    } else {
        saved
    };
    detached?;
    result
}

async fn shutdown() {
    #[cfg(unix)]
    if let Ok(mut terminate) =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    {
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
        return;
    }
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn cancelled_reads_preserve_partial_messages() {
        let (mut writer, reader) = tokio::io::duplex(1024);
        let mut messages = MessageReader::new(BufReader::new(reader));
        writer.write_all(b"{\"id\":").await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(10), messages.receive())
                .await
                .is_err()
        );
        writer.write_all(b"1}\n{\"id\":2}\n").await.unwrap();
        assert_eq!(messages.receive().await.unwrap(), Some(json!({"id":1})));
        assert_eq!(messages.receive().await.unwrap(), Some(json!({"id":2})));
    }

    #[tokio::test]
    async fn lost_response_body_retries_the_same_lifecycle_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut bodies = Vec::new();
            for attempt in 0..2 {
                let (stream, _) = listener.accept().await.unwrap();
                let mut reader = BufReader::new(stream);
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).await.unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_lowercase().strip_prefix("content-length: ") {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).await.unwrap();
                bodies.push(body);
                let response = if attempt == 0 {
                    "HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{"
                } else {
                    "HTTP/1.1 200 OK\r\nContent-Length: 14\r\nConnection: close\r\n\r\n{\"saved\":true}"
                };
                reader
                    .get_mut()
                    .write_all(response.as_bytes())
                    .await
                    .unwrap();
                reader.get_mut().shutdown().await.unwrap();
            }
            assert_eq!(bodies[0], bodies[1]);
        });
        let lifecycle = Lifecycle {
            http: reqwest::Client::new(),
            endpoint,
            token: "test".into(),
            key: json!({"session_id":"same"}),
        };
        assert_eq!(
            lifecycle
                .event(
                    "stable",
                    "end",
                    json!({"turn_id":"turn","status":"completed"})
                )
                .await
                .unwrap(),
            json!({"saved":true})
        );
        server.await.unwrap();
    }
}
