use super::McpServer;
use agentdrive_config::{LiveConfig, RuntimeConfig};
use agentdrive_engine::{Adapters, Engine, EngineConfig};
use agentdrive_local::LocalBackend;
use agentdrive_model::*;
use agentdrive_ports::{LocalStateStore, RemoteObjectStore, RemoteRefStore};
use agentdrive_test_support::{ManualClock, MemoryMountDriver, MemoryRemote, NoHostDirectories};
use serde_json::{Value, json};
use std::sync::{Arc, atomic::Ordering};

struct Fixture {
    server: McpServer,
    local: Arc<LocalBackend>,
    driver: Arc<MemoryMountDriver>,
    workspace: Workspace,
    binding: MountBinding,
    directory: tempfile::TempDir,
}

impl Fixture {
    async fn new() -> Self {
        Self::with_remote(None).await
    }

    async fn with_remote(remote: Option<Arc<MemoryRemote>>) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let local = Arc::new(LocalBackend::open(directory.path()).unwrap());
        let driver = Arc::new(MemoryMountDriver::default());
        let engine = Engine::open(
            Adapters {
                state: local.clone(),
                objects: local.clone(),
                working: local.clone(),
                remote_objects: remote
                    .clone()
                    .map(|remote| remote as Arc<dyn RemoteObjectStore>),
                remote_refs: remote.map(|remote| remote as Arc<dyn RemoteRefStore>),
                clock: Arc::new(ManualClock::default()),
                mounts: driver.clone(),
                directories: Arc::new(NoHostDirectories),
            },
            EngineConfig {
                live: LiveConfig::new(RuntimeConfig::default()).unwrap(),
                identity: LocalIdentity {
                    uid: 1000,
                    gid: 1000,
                },
            },
        )
        .await
        .unwrap();
        let server = McpServer::new(
            engine.services.clone(),
            "owner".to_owned().try_into().unwrap(),
        );
        let created = server
            .dispatch("workspace_create", mutation(json!({"name":"recovery"})))
            .await
            .unwrap();
        let workspace: Workspace =
            serde_json::from_value(created["result"]["workspace"].clone()).unwrap();
        let mounted = server
            .dispatch(
                "mount_prepare",
                mutation(json!({
                    "workspace":workspace.id, "path":directory.path().join("selected"),
                    "access":"read_write", "durability":"local",
                })),
            )
            .await
            .unwrap();
        let binding = serde_json::from_value(mounted["result"]["binding"].clone()).unwrap();
        Self {
            server,
            local,
            driver,
            workspace,
            binding,
            directory,
        }
    }

    async fn call(&self, name: &str, input: Value) -> Value {
        self.server.dispatch(name, input).await.unwrap()
    }

    fn busy_once(&self) {
        *self.driver.unmount_error.lock() =
            Some(Error::new(ErrorCode::MountBusy, "directory is in use").retryable());
    }

    async fn pending_unmount(&self) -> Value {
        self.busy_once();
        self.call(
            "mount_unmount",
            mutation(json!({"mount":self.binding.id, "generation":self.binding.generation})),
        )
        .await
    }
}

fn mutation(request: Value) -> Value {
    json!({"request_id":OperationId::new().to_string(), "request":request})
}

fn assert_complete(record: &Value) {
    assert_eq!(record["phase"], "complete", "{record}");
    assert!(record["error"].is_null(), "{record}");
    assert!(record["pending"].is_null(), "{record}");
    assert!(record["recovery"].is_null(), "{record}");
}

#[tokio::test]
async fn branch_switch_advertises_recovery_after_a_nonretryable_mount_error() {
    let fixture = Fixture::new().await;
    let created = fixture
        .call(
            "branch_create",
            mutation(json!({"workspace":fixture.workspace.id, "name":"target"})),
        )
        .await;
    let request = mutation(json!({"workspace":fixture.workspace.id, "branch":created["branch"]}));
    fixture.driver.fail_mount.store(true, Ordering::SeqCst);
    let failed = fixture.call("branch_switch", request.clone()).await;
    assert_eq!(failed["phase"], "recovery_required");
    assert_eq!(failed["error"]["retryable"], false);
    assert_eq!(failed["recovery"], json!({"kind":"retry_request"}));
    let checked = fixture
        .call(
            "operation_by_request",
            json!({"request_id":request["request_id"]}),
        )
        .await;
    assert_eq!(checked["recovery"], failed["recovery"]);
    let recovered = fixture.call("branch_switch", request.clone()).await;
    assert_complete(&recovered);
    assert_eq!(recovered["id"], failed["id"]);
    assert_eq!(fixture.call("branch_switch", request).await, recovered);
    assert_complete(
        &fixture
            .call("operation_get", json!({"operation":failed["id"]}))
            .await,
    );
}

#[tokio::test]
async fn initial_mount_failure_advertises_retry_without_an_old_binding() {
    let fixture = Fixture::new().await;
    fixture.driver.fail_mount.store(true, Ordering::SeqCst);
    let request = mutation(
        json!({"workspace":fixture.workspace.id, "branch":fixture.binding.branch,
        "path":fixture.directory.path().join("preview"), "access":"read_only", "durability":"local"}),
    );
    let failed = fixture.call("mount_prepare", request.clone()).await;
    assert_eq!(failed["phase"], "waiting_for_quiesce");
    assert_eq!(failed["error"]["retryable"], false);
    assert!(failed["pending"]["old"].is_null());
    assert_eq!(failed["recovery"], json!({"kind":"retry_request"}));
    assert_complete(&fixture.call("mount_prepare", request).await);
}

#[tokio::test]
async fn release_retries_its_saved_target_after_a_failed_attempt_and_lost_response() {
    let fixture = Fixture::new().await;
    let pending = fixture.pending_unmount().await;
    assert_eq!(
        pending["recovery"],
        json!({"kind":"release_mount", "expected_binding_generation":fixture.binding.generation})
    );
    let request = mutation(
        json!({"operation":pending["id"], "expected_binding_generation":fixture.binding.generation}),
    );
    fixture.busy_once();
    let failed = fixture.call("mount_release", request.clone()).await;
    assert_eq!(failed["phase"], "failed");
    assert!(failed["recovery"].is_null());
    let recovered = fixture.call("mount_release", request.clone()).await;
    assert_complete(&recovered);
    assert_eq!(recovered["id"], failed["id"]);
    // Repeating a successful attempt recovers its receipt without repeating effects.
    assert_eq!(fixture.call("mount_release", request).await, recovered);
    assert_complete(
        &fixture
            .call("operation_get", json!({"operation":pending["id"]}))
            .await,
    );
    assert_eq!(
        fixture
            .local
            .mount(fixture.binding.id)
            .await
            .unwrap()
            .unwrap()
            .state,
        MountState::Released
    );
}

#[tokio::test]
async fn release_recovery_preserves_generation_and_request_fingerprint_checks() {
    let fixture = Fixture::new().await;
    let pending = fixture.pending_unmount().await;
    let stale = mutation(
        json!({"operation":pending["id"], "expected_binding_generation":fixture.binding.generation+1}),
    );
    for _ in 0..2 {
        let failed = fixture.call("mount_release", stale.clone()).await;
        assert_eq!(failed["error"]["code"], "STALE_BINDING");
    }
    let mut changed = stale;
    changed["request"]["expected_binding_generation"] = json!(fixture.binding.generation);
    let error = fixture
        .server
        .dispatch("mount_release", changed)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::RequestIdConflict);
    let recovered = fixture.call("mount_release", mutation(json!({"operation":pending["id"], "expected_binding_generation":fixture.binding.generation}))).await;
    assert_complete(&recovered);
}

#[tokio::test]
async fn committed_revision_mount_recovery_clears_the_previous_error() {
    let fixture = Fixture::new().await;
    let branch = fixture
        .local
        .branch(fixture.binding.branch)
        .await
        .unwrap()
        .unwrap();
    let pending = fixture.call("branch_restore", mutation(json!({
        "guard":BranchGuard::from(&branch), "revision":fixture.workspace.initial_revision, "durability":"local",
    }))).await;
    let request = mutation(
        json!({"operation":pending["id"], "expected_binding_generation":fixture.binding.generation}),
    );
    fixture.driver.fail_mount.store(true, Ordering::SeqCst);
    let failed = fixture.call("mount_release", request.clone()).await;
    assert_eq!(failed["phase"], "failed");
    let committed = fixture
        .call("operation_get", json!({"operation":pending["id"]}))
        .await;
    assert_eq!(committed["phase"], "committed_mount_pending");
    assert_eq!(committed["local_saved"], true);
    assert_eq!(
        committed["recovery"],
        json!({"kind":"release_mount", "expected_binding_generation":fixture.binding.generation})
    );
    let head = fixture
        .local
        .branch(fixture.binding.branch)
        .await
        .unwrap()
        .unwrap()
        .formal_head;
    assert_complete(&fixture.call("mount_release", request).await);
    let completed = fixture
        .call("operation_get", json!({"operation":pending["id"]}))
        .await;
    assert_eq!(completed["mount_ready"], true);
    assert!(completed["error"].is_null());
    assert!(completed["pending"].is_null());
    assert!(completed["recovery"].is_null());
    assert_eq!(
        fixture
            .local
            .branch(fixture.binding.branch)
            .await
            .unwrap()
            .unwrap()
            .formal_head,
        head
    );
}

#[tokio::test]
async fn mount_recovery_preserves_a_remote_publication_failure() {
    let remote = Arc::new(MemoryRemote::default());
    let fixture = Fixture::with_remote(Some(remote.clone())).await;
    let branch = fixture
        .local
        .branch(fixture.binding.branch)
        .await
        .unwrap()
        .unwrap();
    let pending = fixture.call("branch_restore", mutation(json!({
        "guard":BranchGuard::from(&branch), "revision":fixture.workspace.initial_revision, "durability":"remote",
    }))).await;
    let request = mutation(
        json!({"operation":pending["id"], "expected_binding_generation":fixture.binding.generation}),
    );
    fixture.driver.fail_mount.store(true, Ordering::SeqCst);
    assert_eq!(
        fixture.call("mount_release", request.clone()).await["phase"],
        "failed"
    );
    let remount = fixture
        .call("operation_get", json!({"operation":pending["id"]}))
        .await;
    assert_eq!(remount["phase"], "committed_mount_pending");
    remote.set_available(false);
    assert_eq!(
        fixture.call("mount_release", request).await["phase"],
        "failed"
    );
    let unpublished = fixture
        .call("operation_get", json!({"operation":pending["id"]}))
        .await;
    assert_eq!(unpublished["local_saved"], true);
    assert_eq!(unpublished["mount_ready"], true);
    assert_eq!(unpublished["remote_confirmed"], false);
    assert_eq!(unpublished["error"]["code"], "UNAVAILABLE");
    assert!(unpublished["pending"].is_null());
    assert!(unpublished["recovery"].is_null());
    remote.set_available(true);
    fixture
        .call(
            "sync",
            mutation(json!({"workspace":fixture.workspace.id,"direction":"push"})),
        )
        .await;
    let confirmed = fixture
        .call("operation_get", json!({"operation":pending["id"]}))
        .await;
    assert_complete(&confirmed);
    assert_eq!(confirmed["remote_confirmed"], true);
}

#[tokio::test]
async fn cancelled_operations_do_not_offer_recovery() {
    let fixture = Fixture::new().await;
    let pending = fixture.pending_unmount().await;
    let cancelled = fixture
        .call("operation_cancel", json!({"operation":pending["id"]}))
        .await;
    assert_eq!(cancelled["phase"], "cancelled");
    assert!(cancelled["recovery"].is_null());
    assert!(
        fixture
            .call("operation_get", json!({"operation":pending["id"]}))
            .await["recovery"]
            .is_null()
    );
}
