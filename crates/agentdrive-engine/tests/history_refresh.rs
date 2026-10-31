use agentdrive_config::{LiveConfig, RuntimeConfig};
use agentdrive_engine::{Adapters, Engine, EngineConfig};
use agentdrive_local::LocalBackend;
use agentdrive_model::*;
use agentdrive_ports::*;
use agentdrive_test_support::{ManualClock, MemoryMountDriver, MemoryRemote, NoHostDirectories};
use async_trait::async_trait;
use bytes::Bytes;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::sync::oneshot;

type Pause = (oneshot::Sender<()>, oneshot::Receiver<()>);

#[derive(Default)]
struct ObservedRemote {
    inner: MemoryRemote,
    pause: Mutex<Option<Pause>>,
    branch_gets: AtomicUsize,
}

impl ObservedRemote {
    fn pause_next_branch_get(&self) -> (oneshot::Receiver<()>, oneshot::Sender<()>) {
        let (started, waiting) = oneshot::channel();
        let (resume, paused) = oneshot::channel();
        *self.pause.lock().unwrap() = Some((started, paused));
        (waiting, resume)
    }
}

#[async_trait]
impl RemoteRefStore for ObservedRemote {
    async fn get(&self, key: &RefKey) -> Result<Option<VersionedRef>> {
        let value = self.inner.get(key).await?;
        if matches!(key, RefKey::Branch { .. }) {
            self.branch_gets.fetch_add(1, Ordering::SeqCst);
            let pause = self.pause.lock().unwrap().take();
            if let Some((started, paused)) = pause {
                let _ = started.send(());
                let _ = paused.await;
            }
        }
        Ok(value)
    }
    async fn compare_exchange(&self, update: RefUpdate) -> Result<CasOutcome> {
        self.inner.compare_exchange(update).await
    }
    async fn list_branches(&self, workspace: WorkspaceId) -> Result<Vec<BranchId>> {
        self.inner.list_branches(workspace).await
    }
}

#[async_trait]
impl RemoteObjectStore for ObservedRemote {
    async fn upload(&self, workspace: WorkspaceId, source: ObjectStream) -> Result<()> {
        self.inner.upload(workspace, source).await
    }
    async fn download(&self, workspace: WorkspaceId, object: &ObjectRef) -> Result<ObjectStream> {
        self.inner.download(workspace, object).await
    }
    async fn contains(&self, workspace: WorkspaceId, object: &ObjectRef) -> Result<bool> {
        self.inner.contains(workspace, object).await
    }
}

fn actor() -> Principal {
    "owner".to_owned().try_into().unwrap()
}
fn context() -> RequestContext {
    RequestContext {
        principal: actor(),
        request_id: OperationId::new().to_string(),
    }
}
fn success(record: OperationRecord) -> OperationRecord {
    assert!(record.error.is_none(), "{record:?}");
    record
}

struct Harness {
    directory: tempfile::TempDir,
    local: Arc<LocalBackend>,
    engine: Arc<Engine>,
    remote: Arc<ObservedRemote>,
    clock: Arc<ManualClock>,
    workspace: WorkspaceId,
    branch: BranchId,
}

impl Harness {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let local = Arc::new(LocalBackend::open(directory.path()).unwrap());
        let remote = Arc::new(ObservedRemote::default());
        let clock = Arc::new(ManualClock::default());
        let engine = Engine::open(
            Adapters {
                state: local.clone(),
                objects: local.clone(),
                working: local.clone(),
                remote_objects: Some(remote.clone()),
                remote_refs: Some(remote.clone()),
                clock: clock.clone(),
                mounts: Arc::new(MemoryMountDriver::default()),
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
        let created = success(
            engine
                .services
                .workspace
                .create(
                    context(),
                    CreateWorkspace {
                        name: "history refresh".into(),
                        grants: vec![],
                        max_file_bytes: None,
                        max_working_bytes: None,
                        max_inodes: None,
                    },
                )
                .await
                .unwrap(),
        );
        let workspace = created.workspace.unwrap();
        let branch = created.branch.unwrap();
        success(
            engine
                .services
                .replica
                .sync(
                    context(),
                    SyncRequest {
                        workspace,
                        branches: None,
                        direction: SyncDirection::Push,
                    },
                )
                .await
                .unwrap(),
        );
        Self {
            directory,
            local,
            engine,
            remote,
            clock,
            workspace,
            branch,
        }
    }

    async fn request(&self) -> HistoryRefreshStatus {
        self.engine
            .services
            .workspace
            .request_history_refresh(&actor(), self.workspace)
            .await
            .unwrap()
    }

    async fn writable_file(&self) -> (FsContext, FileHandle) {
        let mounted = success(
            self.engine
                .services
                .mounts
                .prepare(
                    context(),
                    PrepareMount {
                        workspace: self.workspace,
                        branch: Some(self.branch),
                        revision: None,
                        path: self.directory.path().join("mount"),
                        access: AccessMode::ReadWrite,
                        durability: Durability::Local,
                    },
                )
                .await
                .unwrap(),
        );
        let CommandResult::Mount { binding } = mounted.result.unwrap() else {
            panic!("mount result missing")
        };
        let fs = FsContext {
            mount: binding.id,
            generation: binding.generation,
            identity: LocalIdentity {
                uid: 1000,
                gid: 1000,
            },
        };
        let inode = self
            .engine
            .services
            .files
            .create(&fs, InodeId::ROOT, "file", FileKind::File, 0o644)
            .await
            .unwrap();
        let handle = self
            .engine
            .services
            .files
            .open(
                &fs,
                inode.id,
                OpenMode {
                    read: true,
                    write: true,
                    append: false,
                },
            )
            .await
            .unwrap();
        (fs, handle)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn history_refresh_coalesces_requests_without_blocking_local_file_writes() {
    let harness = Harness::new().await;
    let (fs, handle) = harness.writable_file().await;
    let baseline_gets = harness.remote.branch_gets.load(Ordering::SeqCst);
    let (started, resume) = harness.remote.pause_next_branch_get();
    for _ in 0..8 {
        assert!(harness.request().await.refreshing);
    }
    let engine = harness.engine.clone();
    let task = tokio::spawn(async move { engine.refresh_requested_history().await });
    tokio::time::timeout(Duration::from_secs(5), started)
        .await
        .unwrap()
        .unwrap();
    let written = tokio::time::timeout(
        Duration::from_secs(1),
        harness
            .engine
            .services
            .files
            .write(&fs, handle.id, 0, Bytes::from_static(b"local edit")),
    )
    .await
    .expect("remote reads must not hold the local branch lock")
    .unwrap();
    assert_eq!(written, 10);
    for _ in 0..8 {
        assert!(harness.request().await.refreshing);
    }
    resume.send(()).unwrap();
    task.await.unwrap();
    let status = harness.request().await;
    assert!(!status.refreshing);
    assert!(status.last_attempt_ns.is_some() && status.last_success_ns.is_some());
    assert!(status.error.is_none());
    assert_eq!(
        harness.remote.branch_gets.load(Ordering::SeqCst) - baseline_gets,
        1
    );
    assert_eq!(
        harness
            .engine
            .services
            .files
            .read(&fs, handle.id, 0, 10)
            .await
            .unwrap(),
        b"local edit"[..]
    );
    harness.engine.refresh_requested_history().await;
    assert_eq!(
        harness.remote.branch_gets.load(Ordering::SeqCst) - baseline_gets,
        1
    );
    harness.clock.advance(15_000);
    assert!(harness.request().await.refreshing);
    harness.engine.refresh_requested_history().await;
    assert_eq!(
        harness.remote.branch_gets.load(Ordering::SeqCst) - baseline_gets,
        2
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delayed_history_observation_preserves_newer_remote_and_local_branch_state() {
    let harness = Harness::new().await;
    let before = harness.local.branch(harness.branch).await.unwrap().unwrap();
    let (started, resume) = harness.remote.pause_next_branch_get();
    let service = harness.engine.services.workspace.clone();
    let workspace = harness.workspace;
    let delayed = tokio::spawn(async move { service.refresh_history(&actor(), workspace).await });
    tokio::time::timeout(Duration::from_secs(5), started)
        .await
        .unwrap()
        .unwrap();
    let key = RefKey::Branch {
        workspace,
        branch: harness.branch,
    };
    let old = harness.remote.inner.get(&key).await.unwrap().unwrap();
    let mut newer: BranchRef = decode(&old.bytes).unwrap();
    newer.source_generation += 1;
    assert!(matches!(
        harness
            .remote
            .inner
            .compare_exchange(RefUpdate {
                key,
                expected_version: Some(old.version),
                bytes: encode(&newer).unwrap().into()
            })
            .await
            .unwrap(),
        CasOutcome::Applied { .. }
    ));
    harness
        .engine
        .services
        .workspace
        .refresh_history(&actor(), workspace)
        .await
        .unwrap();
    resume.send(()).unwrap();
    delayed.await.unwrap().unwrap();
    assert_eq!(
        harness.local.remote_refs(workspace).await.unwrap(),
        vec![newer]
    );
    assert_eq!(
        harness.local.branch(harness.branch).await.unwrap().unwrap(),
        before
    );
}

#[tokio::test]
async fn refresh_status_reports_failures_retries_and_disabled_storage() {
    let harness = Harness::new().await;
    harness.remote.inner.set_available(false);
    assert!(harness.request().await.refreshing);
    harness.engine.refresh_requested_history().await;
    let status = harness.request().await;
    assert!(!status.refreshing);
    assert_eq!(status.error.unwrap().code, ErrorCode::Unavailable);
    assert!(status.last_success_ns.is_none());
    harness.remote.inner.set_available(true);
    harness.clock.advance(15_000);
    assert!(harness.request().await.refreshing);
    harness.engine.refresh_requested_history().await;
    let status = harness.request().await;
    assert!(status.error.is_none() && status.last_success_ns.is_some());
    harness.clock.advance(15_000);
    assert!(harness.request().await.refreshing);
    harness
        .engine
        .switch_storage(None, &mut || Ok(()))
        .await
        .unwrap();
    harness.engine.refresh_requested_history().await;
    let status = harness.request().await;
    assert!(!status.refreshing);
    assert!(
        status.error.is_none()
            && status.last_attempt_ns.is_none()
            && status.last_success_ns.is_none()
    );
}

#[tokio::test]
async fn cancelling_history_worker_clears_running_status() {
    let harness = Harness::new().await;
    let (started, _resume) = harness.remote.pause_next_branch_get();
    assert!(harness.request().await.refreshing);
    let engine = harness.engine.clone();
    let task = tokio::spawn(async move { engine.refresh_requested_history().await });
    tokio::time::timeout(Duration::from_secs(5), started)
        .await
        .unwrap()
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let status = harness.request().await;
    assert!(!status.refreshing);
    assert!(status.error.unwrap().retryable);
}
