use agentfs_engine::{Adapters, Engine, EngineConfig};
use agentfs_local::LocalBackend;
use agentfs_mcp::{Client, Credential, GatewayConfig, router};
use agentfs_model::*;
use agentfs_test_support::*;
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
use tokio_util::sync::CancellationToken;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_mcp_clients_share_operations_and_enforce_principals() {
    let temporary = tempfile::tempdir().unwrap();
    let local = Arc::new(LocalBackend::open(temporary.path()).unwrap());
    let engine = Engine::open(
        Adapters {
            state: local.clone(),
            objects: local.clone(),
            working: local,
            remote_objects: None,
            remote_refs: None,
            clock: Arc::new(ManualClock::default()),
            mounts: Arc::new(MemoryMountDriver::default()),
            directories: Arc::new(NoHostDirectories),
            validation: Arc::new(TestValidation),
        },
        EngineConfig {
            cache: CacheConfig::default(),
            identity: LocalIdentity {
                uid: 1000,
                gid: 1000,
            },
            validation: BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    let owner_token = "owner-test-token-with-at-least-32-bytes";
    let visitor_token = "visitor-test-token-with-at-least-32-bytes";
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let endpoint = format!("http://{address}/mcp");
    let cancellation = CancellationToken::new();
    let app = router(
        engine.services.clone(),
        vec![
            Credential {
                principal: "owner".to_owned().try_into().unwrap(),
                token: owner_token.into(),
            },
            Credential {
                principal: "visitor".to_owned().try_into().unwrap(),
                token: visitor_token.into(),
            },
        ],
        GatewayConfig {
            allowed_hosts: vec![address.to_string()],
            allowed_origins: vec![],
            cancellation: cancellation.clone(),
        },
    )
    .unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let http = reqwest::Client::new();
    assert_eq!(http.post(&endpoint).send().await.unwrap().status(), 401);
    assert_eq!(
        http.post(&endpoint)
            .bearer_auth(owner_token)
            .header("Origin", "https://untrusted.example")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        http.post(&endpoint)
            .bearer_auth(owner_token)
            .header("Host", "untrusted.example")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let owner = Client::connect(&endpoint, owner_token.into())
        .await
        .unwrap();
    let visitor = Client::connect(&endpoint, visitor_token.into())
        .await
        .unwrap();
    let tools = owner.tools().await.unwrap().tools;
    assert_eq!(tools.len(), 32);
    assert!(!tools.iter().any(|tool| matches!(
        tool.name.as_ref(),
        "turn_begin" | "turn_end" | "branch_fork"
    )));
    assert!(tools.iter().any(|tool| tool.name == "branch_switch"));
    assert!(
        tools
            .iter()
            .all(|tool| tool.input_schema.get("type") == Some(&json!("object")))
    );
    let request = json!({"request_id":"same-request", "request":{"name":"shared","grants":[]}});
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..12 {
        let endpoint = endpoint.clone();
        let request = request.clone();
        tasks.spawn(async move {
            let client = Client::connect(&endpoint, owner_token.into())
                .await
                .unwrap();
            client
                .call("workspace_create".into(), request)
                .await
                .unwrap()
                .structured_content
                .unwrap()["id"]
                .clone()
        });
    }
    let mut ids = Vec::new();
    while let Some(result) = tasks.join_next().await {
        ids.push(result.unwrap());
    }
    assert!(ids.iter().all(|id| id == &ids[0]));
    let result = owner
        .call(
            "operation_by_request".into(),
            json!({"request_id":"same-request"}),
        )
        .await
        .unwrap()
        .structured_content
        .unwrap();
    assert_eq!(result["local_saved"], true);
    let workspace = result["result"]["workspace"]["id"].clone();
    assert!(workspace.is_string(), "{result}");
    let denied = visitor
        .call("workspace_status".into(), json!({"workspace": workspace}))
        .await
        .unwrap();
    assert_eq!(denied.is_error, Some(true));
    let hidden = visitor
        .call("workspace_list".into(), json!({}))
        .await
        .unwrap()
        .structured_content
        .unwrap();
    assert_eq!(hidden, Value::Array(vec![]));
    let conflict = owner
        .call(
            "workspace_create".into(),
            json!({"request_id":"same-request","request":{"name":"different"}}),
        )
        .await
        .unwrap();
    assert_eq!(
        conflict.structured_content.unwrap()["error"]["code"],
        "REQUEST_ID_CONFLICT"
    );
    let forged = owner
        .call(
            "workspace_create".into(),
            json!({"request_id":"forged", "principal":"visitor", "request":{"name":"forged"}}),
        )
        .await
        .unwrap();
    assert_eq!(forged.is_error, Some(true));
    let base = format!("http://{address}");
    let view = format!("{base}/api/workspaces/{}", workspace.as_str().unwrap());
    assert_eq!(http.get(&view).send().await.unwrap().status(), 401);
    assert_eq!(
        http.get(&view)
            .bearer_auth(visitor_token)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        http.get(&view)
            .bearer_auth(owner_token)
            .header("Origin", "https://untrusted.example")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        http.get(&view)
            .bearer_auth(owner_token)
            .header("Host", "untrusted.example")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let response = http
        .get(&view)
        .bearer_auth(owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.headers()["cache-control"], "no-store");
    let view: Value = response.json().await.unwrap();
    assert_eq!(view["status"]["branches"].as_array().unwrap().len(), 1);
    assert!(view["status"]["selected_branch"].is_string());
    assert_eq!(view["revisions"].as_array().unwrap().len(), 1);
    let key = json!({"workspace":workspace,"location":view["status"]["location"],"app_namespace":"test","session_id":"bridge"});
    let lifecycle = format!("{base}/api/lifecycle");
    let attachment = json!({"request_id":"attach", "event":"attach", "key":key, "mount_path":temporary.path().join("mount")});
    let first: Value = http
        .post(&lifecycle)
        .bearer_auth(owner_token)
        .json(&attachment)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let repeated: Value = http
        .post(&lifecycle)
        .bearer_auth(owner_token)
        .json(&attachment)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(first, repeated);
    let mut changed = attachment.clone();
    changed["mount_path"] = json!(temporary.path().join("different"));
    assert_eq!(
        http.post(&lifecycle)
            .bearer_auth(owner_token)
            .json(&changed)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let begin = json!({"request_id":"begin", "event":"begin", "key":key, "turn_id":"turn"});
    assert!(
        http.post(&lifecycle)
            .bearer_auth(owner_token)
            .json(&begin)
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let end = json!({"request_id":"end", "event":"end", "key":key, "turn_id":"turn", "status":"completed"});
    let saved: Value = http
        .post(&lifecycle)
        .bearer_auth(owner_token)
        .json(&end)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let repeated: Value = http
        .post(&lifecycle)
        .bearer_auth(owner_token)
        .json(&end)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(saved, repeated);
    assert_eq!(saved["local_saved"], true);
    let detach = json!({"request_id":"detach", "event":"detach", "key":key});
    assert!(
        http.post(&lifecycle)
            .bearer_auth(owner_token)
            .json(&detach)
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let mut reconnect = attachment;
    reconnect["request_id"] = json!("reconnect");
    assert!(
        http.post(&lifecycle)
            .bearer_auth(owner_token)
            .json(&reconnect)
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let begin = json!({"request_id":"begin-next", "event":"begin", "key":key, "turn_id":"next"});
    assert!(
        http.post(&lifecycle)
            .bearer_auth(owner_token)
            .json(&begin)
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    assert!(
        http.post(&lifecycle)
            .bearer_auth(owner_token)
            .json(&detach)
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let session = engine
        .services
        .sessions
        .get(
            &"owner".to_owned().try_into().unwrap(),
            &serde_json::from_value(key).unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.active_turn.as_deref(), Some("next"));
    assert_eq!(session.state, SessionState::Active);
    let page = http.get(&base).send().await.unwrap();
    assert_eq!(page.status(), 200);
    assert!(
        page.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("script-src 'self'")
    );
    owner.close().await.unwrap();
    visitor.close().await.unwrap();
    cancellation.cancel();
    server.abort();
}
