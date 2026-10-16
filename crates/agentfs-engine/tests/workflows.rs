use agentfs_config::{
    CacheConfig, ConfigStore, LiveConfig, RuntimeConfig, SettingsUpdate, initialize,
};
use agentfs_engine::*;
use agentfs_local::LocalBackend;
use agentfs_model::*;
use agentfs_ports::*;
use agentfs_test_support::*;
use bytes::Bytes;
use std::sync::Arc;

fn actor() -> Principal {
    "owner".to_owned().try_into().unwrap()
}
fn context() -> RequestContext {
    RequestContext {
        principal: actor(),
        request_id: OperationId::new().to_string(),
    }
}
fn path(value: &str) -> WorkspacePath {
    value.to_owned().try_into().unwrap()
}
fn identity() -> LocalIdentity {
    LocalIdentity {
        uid: 1000,
        gid: 1000,
    }
}
fn success(operation: OperationRecord) -> OperationRecord {
    assert!(operation.error.is_none(), "{operation:?}");
    operation
}

struct Harness {
    directory: tempfile::TempDir,
    local: Arc<LocalBackend>,
    engine: Arc<Engine>,
    clock: Arc<ManualClock>,
    driver: Arc<MemoryMountDriver>,
}
impl Harness {
    async fn new(remote: Option<Arc<MemoryRemote>>) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let (local, engine, clock, driver) = open(directory.path(), remote).await;
        Self {
            directory,
            local,
            engine,
            clock,
            driver,
        }
    }
    async fn workspace(&self) -> Workspace {
        let operation = success(
            self.engine
                .services
                .workspace
                .create(
                    context(),
                    CreateWorkspace {
                        name: "test".into(),
                        grants: vec![],
                        max_file_bytes: None,
                        max_working_bytes: None,
                        max_inodes: None,
                    },
                )
                .await
                .unwrap(),
        );
        match operation.result.unwrap() {
            CommandResult::Workspace { workspace } => workspace,
            result => panic!("{result:?}"),
        }
    }
    async fn branch_mount(
        &self,
        workspace: WorkspaceId,
        name: &str,
        source: Option<RevisionId>,
    ) -> (MountBinding, FsContext) {
        let created = success(
            self.engine
                .services
                .revisions
                .fork(
                    context(),
                    ForkBranch {
                        workspace,
                        source: Some(
                            source.unwrap_or(
                                self.local
                                    .workspace(workspace)
                                    .await
                                    .unwrap()
                                    .unwrap()
                                    .initial_revision,
                            ),
                        ),
                        source_branch: None,
                        name: name.into(),
                    },
                )
                .await
                .unwrap(),
        );
        let branch = match created.result.unwrap() {
            CommandResult::Branch { branch } => branch.id,
            _ => panic!("branch result missing"),
        };
        let operation = success(
            self.engine
                .services
                .mounts
                .prepare(
                    context(),
                    PrepareMount {
                        workspace,
                        branch: Some(branch),
                        revision: None,
                        path: self.directory.path().join(name),
                        access: AccessMode::ReadWrite,
                        durability: Durability::Local,
                    },
                )
                .await
                .unwrap(),
        );
        let binding = match operation.result.unwrap() {
            CommandResult::Mount { binding } => binding,
            result => panic!("{result:?}"),
        };
        let mount = binding.clone();
        assert_eq!(mount.state, MountState::Ready);
        let fs = FsContext {
            mount: mount.id,
            generation: mount.generation,
            identity: identity(),
        };
        (binding, fs)
    }
    async fn write_new(&self, fs: &FsContext, name: &str, bytes: &[u8]) -> Inode {
        let file = self
            .engine
            .services
            .files
            .create(fs, InodeId::ROOT, name, FileKind::File, 0o644)
            .await
            .unwrap();
        let handle = self
            .engine
            .services
            .files
            .open(
                fs,
                file.id,
                OpenMode {
                    read: true,
                    write: true,
                    append: false,
                },
            )
            .await
            .unwrap();
        self.engine
            .services
            .files
            .write(fs, handle.id, 0, Bytes::copy_from_slice(bytes))
            .await
            .unwrap();
        self.engine
            .services
            .files
            .release(fs, handle.id)
            .await
            .unwrap();
        file
    }
    async fn commit(&self, branch: BranchId) -> RevisionId {
        let branch = self.local.branch(branch).await.unwrap().unwrap();
        let operation = success(
            self.engine
                .services
                .revisions
                .commit(
                    context(),
                    CommitRevision {
                        message: None,
                        guard: BranchGuard::from(&branch),
                        kind: CommitKind::Commit,
                        durability: Durability::Local,
                    },
                )
                .await
                .unwrap(),
        );
        match operation.result.unwrap() {
            CommandResult::Revision { revision, .. } => revision,
            result => panic!("{result:?}"),
        }
    }
    async fn sync(&self, workspace: WorkspaceId, direction: SyncDirection) {
        success(
            self.engine
                .services
                .replica
                .sync(
                    context(),
                    SyncRequest {
                        workspace,
                        branches: None,
                        direction,
                    },
                )
                .await
                .unwrap(),
        );
    }
}
async fn open(
    directory: &std::path::Path,
    remote: Option<Arc<MemoryRemote>>,
) -> (
    Arc<LocalBackend>,
    Arc<Engine>,
    Arc<ManualClock>,
    Arc<MemoryMountDriver>,
) {
    open_with_config(
        directory,
        remote,
        LiveConfig::new(RuntimeConfig {
            cache: CacheConfig { max_bytes: 1 },
            ..Default::default()
        })
        .unwrap(),
    )
    .await
}
async fn open_with_config(
    directory: &std::path::Path,
    remote: Option<Arc<MemoryRemote>>,
    live: Arc<LiveConfig>,
) -> (
    Arc<LocalBackend>,
    Arc<Engine>,
    Arc<ManualClock>,
    Arc<MemoryMountDriver>,
) {
    let local = Arc::new(LocalBackend::open(directory).unwrap());
    let clock = Arc::new(ManualClock::default());
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
            clock: clock.clone(),
            mounts: driver.clone(),
            directories: Arc::new(NoHostDirectories),
        },
        EngineConfig {
            live,
            identity: identity(),
        },
    )
    .await
    .unwrap();
    (local, engine, clock, driver)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_retries_create_exactly_one_workspace() {
    let harness = Harness::new(None).await;
    let request = context();
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..16 {
        let service = harness.engine.services.workspace.clone();
        let context = request.clone();
        tasks.spawn(async move {
            service
                .create(
                    context,
                    CreateWorkspace {
                        name: "once".into(),
                        grants: vec![],
                        max_file_bytes: None,
                        max_working_bytes: None,
                        max_inodes: None,
                    },
                )
                .await
                .unwrap()
                .id
        });
    }
    let mut ids = std::collections::BTreeSet::new();
    while let Some(result) = tasks.join_next().await {
        ids.insert(result.unwrap());
    }
    assert_eq!(ids.len(), 1);
    let record = harness
        .engine
        .services
        .operations
        .by_request(&actor(), &request.request_id)
        .await
        .unwrap();
    assert!(record.local_saved);
    assert_eq!(harness.local.workspaces().await.unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn append_is_atomic_and_rename_preserves_open_handles() {
    let harness = Harness::new(None).await;
    let workspace = harness.workspace().await;
    let (_, fs) = harness.branch_mount(workspace.id, "append", None).await;
    let file = harness.write_new(&fs, "data", b"").await;
    let handle = harness
        .engine
        .services
        .files
        .open(
            &fs,
            file.id,
            OpenMode {
                read: true,
                write: true,
                append: true,
            },
        )
        .await
        .unwrap();
    let mut tasks = tokio::task::JoinSet::new();
    for value in 0..64u8 {
        let service = harness.engine.services.files.clone();
        let fs = fs.clone();
        let id = handle.id;
        tasks.spawn(async move {
            service
                .write(&fs, id, 0, Bytes::from(vec![value]))
                .await
                .unwrap();
        });
    }
    while let Some(result) = tasks.join_next().await {
        result.unwrap();
    }
    harness
        .engine
        .services
        .files
        .rename(&fs, InodeId::ROOT, "data", InodeId::ROOT, "renamed", false)
        .await
        .unwrap();
    let mut bytes = harness
        .engine
        .services
        .files
        .read(&fs, handle.id, 0, 1024)
        .await
        .unwrap()
        .to_vec();
    bytes.sort();
    assert_eq!(bytes, (0..64u8).collect::<Vec<_>>());
    assert_eq!(
        harness
            .engine
            .services
            .files
            .unlink(&fs, InodeId::ROOT, "renamed", false)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    harness
        .engine
        .services
        .files
        .release(&fs, handle.id)
        .await
        .unwrap();
    harness.engine.services.files.fsync(&fs).await.unwrap();
}

#[tokio::test]
async fn lookup_preserves_normalized_names_and_revision_contents() {
    let harness = Harness::new(None).await;
    let workspace = harness.workspace().await;
    let (binding, fs) = harness.branch_mount(workspace.id, "lookup", None).await;
    let files = &harness.engine.services.files;
    let directory = files
        .create(&fs, InodeId::ROOT, "Parent", FileKind::Directory, 0o755)
        .await
        .unwrap();
    let file = files
        .create(&fs, directory.id, "CAFÉ", FileKind::File, 0o644)
        .await
        .unwrap();
    let revision = harness.commit(binding.branch).await;
    let mounted = success(
        harness
            .engine
            .services
            .mounts
            .prepare(
                context(),
                PrepareMount {
                    workspace: workspace.id,
                    branch: Some(binding.branch),
                    revision: Some(revision),
                    path: harness.directory.path().join("history"),
                    access: AccessMode::ReadOnly,
                    durability: Durability::Local,
                },
            )
            .await
            .unwrap(),
    );
    let Some(CommandResult::Mount { binding }) = mounted.result else {
        panic!("mount result missing");
    };
    let history = FsContext {
        mount: binding.id,
        generation: binding.generation,
        identity: identity(),
    };
    for context in [&fs, &history] {
        assert_eq!(
            files
                .lookup(context, InodeId::ROOT, "pArEnT")
                .await
                .unwrap()
                .id,
            directory.id
        );
        assert_eq!(
            files
                .lookup(context, directory.id, "cafe\u{301}")
                .await
                .unwrap()
                .id,
            file.id
        );
        assert_eq!(
            files.lookup(context, directory.id, ".").await.unwrap().id,
            directory.id
        );
        assert_eq!(
            files.lookup(context, directory.id, "..").await.unwrap().id,
            InodeId::ROOT
        );
        assert_eq!(
            files.lookup(context, InodeId::ROOT, "..").await.unwrap().id,
            InodeId::ROOT
        );
        assert_eq!(
            files
                .lookup(context, directory.id, "missing")
                .await
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
    }
    files
        .rename(&fs, directory.id, "CAFÉ", directory.id, "Renamed", false)
        .await
        .unwrap();
    assert_eq!(
        files.lookup(&fs, directory.id, "renamed").await.unwrap().id,
        file.id
    );
    assert_eq!(
        files
            .lookup(&fs, directory.id, "CAFÉ")
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    assert_eq!(
        files
            .lookup(&history, directory.id, "cafe\u{301}")
            .await
            .unwrap()
            .id,
        file.id
    );
    assert_eq!(
        files
            .lookup(&history, directory.id, "renamed")
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn restart_restores_saved_bytes_and_reuses_mount() {
    let harness = Harness::new(None).await;
    let workspace = harness.workspace().await;
    let (binding, fs) = harness.branch_mount(workspace.id, "resume", None).await;
    let file = harness.write_new(&fs, "file", b"saved bytes").await;
    let receipt = harness.engine.services.files.fsync(&fs).await.unwrap();
    assert!(receipt.local_saved);
    let handle = harness
        .engine
        .services
        .files
        .open(
            &fs,
            file.id,
            OpenMode {
                read: true,
                write: true,
                append: false,
            },
        )
        .await
        .unwrap();
    harness
        .engine
        .services
        .files
        .write(&fs, handle.id, 0, Bytes::from_static(b"lost writes"))
        .await
        .unwrap();
    let Harness {
        directory,
        local,
        engine,
        clock,
        driver,
    } = harness;
    drop((local, engine, clock, driver));
    let (local, engine, _, _) = open(directory.path(), None).await;
    let resumed = success(
        engine
            .services
            .mounts
            .prepare(
                context(),
                PrepareMount {
                    workspace: workspace.id,
                    branch: Some(binding.branch),
                    revision: None,
                    path: binding.path.clone(),
                    access: AccessMode::ReadWrite,
                    durability: Durability::Local,
                },
            )
            .await
            .unwrap(),
    );
    let mount = match resumed.result.unwrap() {
        CommandResult::Mount { binding } => binding,
        _ => panic!("mount missing"),
    };
    assert_eq!(mount.id, binding.id);
    assert_eq!(
        local.mount(mount.id).await.unwrap().unwrap().state,
        MountState::Ready
    );
    let fs = FsContext {
        mount: mount.id,
        generation: mount.generation,
        identity: identity(),
    };
    let handle = engine
        .services
        .files
        .open(
            &fs,
            file.id,
            OpenMode {
                read: true,
                write: false,
                append: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        engine
            .services
            .files
            .read(&fs, handle.id, 0, 1024)
            .await
            .unwrap(),
        "saved bytes"
    );
}

#[tokio::test]
async fn autosave_uses_quiet_and_continuous_write_deadlines() {
    let harness = Harness::new(None).await;
    let workspace = harness.workspace().await;
    let (_, fs) = harness.branch_mount(workspace.id, "autosave", None).await;
    let file = harness.write_new(&fs, "file", b"first").await;
    harness.clock.advance(4_999);
    assert_eq!(harness.engine.autosave_due().await.unwrap(), 0);
    harness.clock.advance(1);
    assert_eq!(harness.engine.autosave_due().await.unwrap(), 1);
    let handle = harness
        .engine
        .services
        .files
        .open(
            &fs,
            file.id,
            OpenMode {
                read: false,
                write: true,
                append: true,
            },
        )
        .await
        .unwrap();
    for _ in 0..8 {
        harness
            .engine
            .services
            .files
            .write(&fs, handle.id, 0, Bytes::from_static(b"x"))
            .await
            .unwrap();
        harness.clock.advance(4_000);
    }
    assert_eq!(harness.engine.autosave_due().await.unwrap(), 1);
}

#[tokio::test]
async fn remote_outage_preserves_local_save_and_sync_confirms_lost_responses() {
    let remote = Arc::new(MemoryRemote::default());
    let harness = Harness::new(Some(remote.clone())).await;
    let workspace = harness.workspace().await;
    let (binding, fs) = harness.branch_mount(workspace.id, "remote", None).await;
    harness.write_new(&fs, "file", b"durable").await;
    remote.set_available(false);
    let branch = harness.local.branch(binding.branch).await.unwrap().unwrap();
    let saved = harness
        .engine
        .services
        .revisions
        .commit(
            context(),
            CommitRevision {
                message: None,
                guard: BranchGuard::from(&branch),
                kind: CommitKind::Commit,
                durability: Durability::Remote,
            },
        )
        .await
        .unwrap();
    assert!(saved.local_saved);
    assert!(!saved.remote_confirmed);
    assert_eq!(saved.error.as_ref().unwrap().code, ErrorCode::Unavailable);
    remote.set_available(true);
    remote.lose_next_cas_response();
    harness.sync(workspace.id, SyncDirection::Push).await;
    assert!(
        harness
            .engine
            .services
            .operations
            .get(&actor(), saved.id)
            .await
            .unwrap()
            .remote_confirmed
    );
    let second = Harness::new(Some(remote)).await;
    second.sync(workspace.id, SyncDirection::Pull).await;
    let revision = harness
        .local
        .branch(binding.branch)
        .await
        .unwrap()
        .unwrap()
        .formal_head;
    let bytes = second
        .engine
        .services
        .revisions
        .read_file(&actor(), workspace.id, revision, path("/file"), 0, 1024)
        .await
        .unwrap();
    assert_eq!(bytes, "durable");
}

#[tokio::test]
async fn ownership_transfer_stops_old_writer_and_adopts_on_new_location() {
    let remote = Arc::new(MemoryRemote::default());
    let first = Harness::new(Some(remote.clone())).await;
    let second = Harness::new(Some(remote.clone())).await;
    let workspace = first.workspace().await;
    let (binding, fs) = first.branch_mount(workspace.id, "owner", None).await;
    first.write_new(&fs, "file", b"handoff").await;
    first.commit(binding.branch).await;
    first.sync(workspace.id, SyncDirection::Push).await;
    success(
        first
            .engine
            .services
            .mounts
            .unmount(context(), fs.mount, fs.generation)
            .await
            .unwrap(),
    );
    let branch = first.local.branch(binding.branch).await.unwrap().unwrap();
    remote.lose_next_cas_response();
    let operation = success(
        first
            .engine
            .services
            .ownership
            .transfer(
                context(),
                TransferOwnership {
                    guard: BranchGuard::from(&branch),
                    target: second.engine.runtime.location,
                },
            )
            .await
            .unwrap(),
    );
    assert!(operation.remote_confirmed);
    second.sync(workspace.id, SyncDirection::Pull).await;
    let adopted = second.local.branch(binding.branch).await.unwrap().unwrap();
    assert_eq!(adopted.owner, second.engine.runtime.location);
    assert_eq!(adopted.authority_epoch, branch.authority_epoch + 1);
    assert_eq!(adopted.state, BranchState::Writable);
    let old = first.local.branch(binding.branch).await.unwrap().unwrap();
    assert_eq!(old.state, BranchState::Stopped);
    assert_eq!(
        old.check_write(first.engine.runtime.location, old.authority_epoch)
            .unwrap_err()
            .code,
        ErrorCode::StaleAuthority
    );
}

#[tokio::test]
async fn merge_accepts_file_edits_and_preserves_both_parents() {
    let harness = Harness::new(None).await;
    let workspace = harness.workspace().await;
    let (base, base_fs) = harness.branch_mount(workspace.id, "base", None).await;
    harness.write_new(&base_fs, "file", b"base").await;
    let base_revision = harness.commit(base.branch).await;
    let (source, source_fs) = harness
        .branch_mount(workspace.id, "source", Some(base_revision))
        .await;
    let (target, target_fs) = harness
        .branch_mount(workspace.id, "target", Some(base_revision))
        .await;
    for (fs, data) in [
        (&source_fs, b"source".as_slice()),
        (&target_fs, b"target".as_slice()),
    ] {
        let file = harness
            .engine
            .services
            .files
            .lookup(fs, InodeId::ROOT, "file")
            .await
            .unwrap();
        let handle = harness
            .engine
            .services
            .files
            .open(
                fs,
                file.id,
                OpenMode {
                    read: false,
                    write: true,
                    append: false,
                },
            )
            .await
            .unwrap();
        harness
            .engine
            .services
            .files
            .write(fs, handle.id, 0, Bytes::copy_from_slice(data))
            .await
            .unwrap();
        harness
            .engine
            .services
            .files
            .release(fs, handle.id)
            .await
            .unwrap();
    }
    harness
        .write_new(&source_fs, "picture.png", b"\x89PNG\r\n\x1a\n\x00\xff")
        .await;
    let source_revision = harness.commit(source.branch).await;
    let target_revision = harness.commit(target.branch).await;
    let guard = BranchGuard::from(&harness.local.branch(target.branch).await.unwrap().unwrap());
    let prepared = success(
        harness
            .engine
            .services
            .merge
            .prepare(
                context(),
                PrepareMerge {
                    workspace: workspace.id,
                    source: source_revision,
                    target: guard,
                    base: None,
                },
            )
            .await
            .unwrap(),
    );
    let candidate = match prepared.result.unwrap() {
        CommandResult::Merge { candidate } => candidate,
        result => panic!("{result:?}"),
    };
    assert_eq!(candidate.conflicts.len(), 1);
    let rejected = harness
        .engine
        .services
        .merge
        .apply(
            context(),
            ApplyMerge {
                merge: candidate.id,
                candidate: candidate.revision,
                target: guard,
                durability: Durability::Local,
            },
        )
        .await
        .unwrap();
    assert_eq!(rejected.error.unwrap().code, ErrorCode::UnresolvedConflict);
    let previous_candidate = candidate.revision;
    let resolved = success(
        harness
            .engine
            .services
            .merge
            .resolve(
                context(),
                ResolveMerge {
                    merge: candidate.id,
                    candidate: candidate.revision,
                    resolutions: vec![
                        Resolution::Source {
                            path: path("/file"),
                        },
                        Resolution::Replace {
                            path: path("/note.md"),
                            content: b"AI merge notes".to_vec(),
                            mode: 0o644,
                        },
                        Resolution::Copy {
                            path: path("/merged.png"),
                            revision: source_revision,
                            source_path: path("/picture.png"),
                        },
                    ],
                },
            )
            .await
            .unwrap(),
    );
    let candidate = match resolved.result.unwrap() {
        CommandResult::Merge { candidate } => candidate,
        _ => unreachable!(),
    };
    let files = harness
        .engine
        .services
        .revisions
        .files(
            &actor(),
            ListRevisionFiles {
                workspace: workspace.id,
                revision: candidate.revision,
                path: None,
                after: None,
            },
        )
        .await
        .unwrap();
    assert!(files.entries.contains_key(&path("/note.md")));
    assert!(files.entries.contains_key(&path("/merged.png")));
    let stale = harness
        .engine
        .services
        .merge
        .apply(
            context(),
            ApplyMerge {
                merge: candidate.id,
                candidate: previous_candidate,
                target: guard,
                durability: Durability::Local,
            },
        )
        .await
        .unwrap();
    assert_eq!(stale.error.unwrap().code, ErrorCode::CandidateMoved);
    let pending = success(
        harness
            .engine
            .services
            .merge
            .apply(
                context(),
                ApplyMerge {
                    merge: candidate.id,
                    candidate: candidate.revision,
                    target: guard,
                    durability: Durability::Local,
                },
            )
            .await
            .unwrap(),
    );
    assert_eq!(pending.phase, OperationPhase::WaitingForQuiesce);
    success(
        harness
            .engine
            .services
            .mounts
            .release(
                context(),
                ReleaseMount {
                    operation: pending.id,
                    expected_binding_generation: target_fs.generation,
                },
            )
            .await
            .unwrap(),
    );
    let applied = harness
        .engine
        .services
        .operations
        .get(&actor(), pending.id)
        .await
        .unwrap();
    let revision = match applied.result.unwrap() {
        CommandResult::Revision { revision, .. } => revision,
        _ => unreachable!(),
    };
    assert_eq!(
        harness
            .local
            .revision(revision)
            .await
            .unwrap()
            .unwrap()
            .parents,
        vec![target_revision, source_revision]
    );
    assert_eq!(
        harness
            .engine
            .services
            .revisions
            .read_file(&actor(), workspace.id, revision, path("/file"), 0, 1024)
            .await
            .unwrap(),
        "source"
    );
    let image = harness
        .engine
        .services
        .revisions
        .read_file(
            &actor(),
            workspace.id,
            revision,
            path("/merged.png"),
            0,
            100,
        )
        .await
        .unwrap();
    assert_eq!(image.as_ref(), b"\x89PNG\r\n\x1a\n\x00\xff");
    let notes = harness
        .engine
        .services
        .revisions
        .read_file(&actor(), workspace.id, revision, path("/note.md"), 0, 100)
        .await
        .unwrap();
    assert_eq!(notes.as_ref(), b"AI merge notes");
    assert_eq!(
        harness
            .engine
            .services
            .files
            .getattr(&target_fs, InodeId::ROOT)
            .await
            .unwrap_err()
            .code,
        ErrorCode::StaleBinding
    );
}

#[tokio::test]
async fn pinned_historical_file_survives_eviction_and_unpin_releases_it() {
    let remote = Arc::new(MemoryRemote::default());
    let harness = Harness::new(Some(remote)).await;
    let workspace = harness.workspace().await;
    let (binding, fs) = harness.branch_mount(workspace.id, "cache", None).await;
    let file = harness.write_new(&fs, "file", b"old bytes").await;
    let old = harness.commit(binding.branch).await;
    let object = harness
        .local
        .node(binding.branch, file.id)
        .await
        .unwrap()
        .unwrap()
        .inode
        .content
        .unwrap();
    let handle = harness
        .engine
        .services
        .files
        .open(
            &fs,
            file.id,
            OpenMode {
                read: false,
                write: true,
                append: false,
            },
        )
        .await
        .unwrap();
    harness
        .engine
        .services
        .files
        .write(&fs, handle.id, 0, Bytes::from_static(b"new bytes"))
        .await
        .unwrap();
    harness
        .engine
        .services
        .files
        .release(&fs, handle.id)
        .await
        .unwrap();
    harness.commit(binding.branch).await;
    harness.sync(workspace.id, SyncDirection::Push).await;
    let range = RevisionRange {
        workspace: workspace.id,
        revision: old,
        paths: Some(vec![path("/file")]),
    };
    success(
        harness
            .engine
            .services
            .cache
            .pin(
                context(),
                PinRequest {
                    range: range.clone(),
                    enabled: true,
                },
            )
            .await
            .unwrap(),
    );
    harness.engine.services.cache.collect().await.unwrap();
    assert!(harness.local.contains(workspace.id, &object).await.unwrap());
    assert!(
        harness
            .engine
            .services
            .cache
            .status(&actor(), range.clone())
            .await
            .unwrap()
            .offline_ready
    );
    success(
        harness
            .engine
            .services
            .cache
            .pin(
                context(),
                PinRequest {
                    range,
                    enabled: false,
                },
            )
            .await
            .unwrap(),
    );
    harness.engine.services.cache.collect().await.unwrap();
    assert!(!harness.local.contains(workspace.id, &object).await.unwrap());
}

#[tokio::test]
async fn moved_restore_target_preserves_the_active_mount_and_can_be_cancelled() {
    let harness = Harness::new(None).await;
    let workspace = harness.workspace().await;
    let (binding, fs) = harness.branch_mount(workspace.id, "restore", None).await;
    harness.write_new(&fs, "original", b"keep").await;
    harness.commit(binding.branch).await;
    let branch = harness.local.branch(binding.branch).await.unwrap().unwrap();
    let pending = success(
        harness
            .engine
            .services
            .revisions
            .restore(
                context(),
                RestoreBranch {
                    guard: BranchGuard::from(&branch),
                    revision: workspace.initial_revision,
                    durability: Durability::Local,
                },
            )
            .await
            .unwrap(),
    );
    assert_eq!(pending.phase, OperationPhase::WaitingForQuiesce);
    harness.write_new(&fs, "newer", b"keep newer").await;
    let rejected = harness
        .engine
        .services
        .mounts
        .release(
            context(),
            ReleaseMount {
                operation: pending.id,
                expected_binding_generation: fs.generation,
            },
        )
        .await
        .unwrap();
    assert_eq!(rejected.error.unwrap().code, ErrorCode::TargetMoved);
    assert_eq!(
        harness.local.mount(fs.mount).await.unwrap().unwrap().state,
        MountState::Ready
    );
    harness
        .engine
        .services
        .files
        .lookup(&fs, InodeId::ROOT, "newer")
        .await
        .unwrap();
    let cancelled = harness
        .engine
        .services
        .operations
        .cancel(&actor(), pending.id)
        .await
        .unwrap();
    assert_eq!(cancelled.phase, OperationPhase::Cancelled);
}

#[tokio::test]
async fn complete_local_cache_can_be_prefetched_while_remote_is_offline() {
    let remote = Arc::new(MemoryRemote::default());
    let harness = Harness::new(Some(remote.clone())).await;
    let workspace = harness.workspace().await;
    let (binding, fs) = harness
        .branch_mount(workspace.id, "offline-prefetch", None)
        .await;
    harness.write_new(&fs, "file", b"cached bytes").await;
    let revision = harness.commit(binding.branch).await;
    remote.set_available(false);
    let record = harness
        .engine
        .services
        .cache
        .prefetch(
            context(),
            RevisionRange {
                workspace: workspace.id,
                revision,
                paths: None,
            },
        )
        .await
        .unwrap();
    assert!(record.error.is_none(), "{record:?}");
    let Some(CommandResult::Prefetch { status }) = record.result else {
        panic!("wrong result")
    };
    assert!(status.complete);
}

#[tokio::test]
async fn publication_failure_does_not_block_other_branches() {
    let remote = Arc::new(MemoryRemote::default());
    let harness = Harness::new(Some(remote.clone())).await;
    let workspace = harness.workspace().await;
    let (mut first, mut first_fs) = harness.branch_mount(workspace.id, "first", None).await;
    let (mut second, mut second_fs) = harness.branch_mount(workspace.id, "second", None).await;
    if first.branch > second.branch {
        std::mem::swap(&mut first, &mut second);
        std::mem::swap(&mut first_fs, &mut second_fs);
    }
    harness.sync(workspace.id, SyncDirection::Push).await;
    let key = RefKey::Branch {
        workspace: workspace.id,
        branch: first.branch,
    };
    let reference = remote.get(&key).await.unwrap().unwrap();
    let mut changed: BranchRef = decode(&reference.bytes).unwrap();
    changed.owner = LocationId::new();
    changed.authority_epoch += 1;
    assert!(matches!(
        remote
            .compare_exchange(RefUpdate {
                key,
                expected_version: Some(reference.version),
                bytes: encode(&changed).unwrap().into(),
            })
            .await
            .unwrap(),
        CasOutcome::Applied { .. }
    ));
    harness.write_new(&first_fs, "file", b"first").await;
    harness.commit(first.branch).await;
    harness.clock.advance(1000);
    harness.write_new(&second_fs, "file", b"second").await;
    harness.commit(second.branch).await;
    let error = harness.engine.sync_pending().await.unwrap_err();
    assert_eq!(error.code, ErrorCode::StaleAuthority);
    let remaining = harness.local.sync_jobs(workspace.id).await.unwrap();
    assert!(
        remaining
            .iter()
            .any(|job| job.reference.branch == first.branch)
    );
    assert!(
        remaining
            .iter()
            .all(|job| job.reference.branch != second.branch)
    );
}

#[tokio::test]
async fn explicit_branch_creation_captures_pending_content_once() {
    let harness = Harness::new(None).await;
    let workspace = harness.workspace().await;
    let (binding, fs) = harness.branch_mount(workspace.id, "source", None).await;
    harness
        .write_new(&fs, "pending.txt", b"pending content")
        .await;
    let request = ForkBranch {
        workspace: workspace.id,
        source: None,
        source_branch: Some(binding.branch),
        name: "feature".into(),
    };
    let identity = context();
    let created = success(
        harness
            .engine
            .services
            .revisions
            .fork(identity.clone(), request.clone())
            .await
            .unwrap(),
    );
    let repeated = success(
        harness
            .engine
            .services
            .revisions
            .fork(identity, request)
            .await
            .unwrap(),
    );
    assert_eq!(created.id, repeated.id);
    assert_eq!(created.branch, repeated.branch);
    let branch = harness
        .local
        .branch(created.branch.unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(branch.source_branch, Some(binding.branch));
    let content = harness
        .engine
        .services
        .revisions
        .read_file(
            &actor(),
            workspace.id,
            branch.base_revision,
            path("/pending.txt"),
            0,
            1024,
        )
        .await
        .unwrap();
    assert_eq!(content, b"pending content"[..]);
    harness.write_new(&fs, "later.txt", b"source only").await;
    assert!(
        harness
            .engine
            .services
            .revisions
            .read_file(
                &actor(),
                workspace.id,
                branch.formal_head,
                path("/later.txt"),
                0,
                1024
            )
            .await
            .is_err()
    );
    assert_eq!(harness.local.branches(workspace.id).await.unwrap().len(), 3);
}

#[tokio::test]
async fn branch_switch_preserves_edits_and_waits_for_handles() {
    let harness = Harness::new(None).await;
    let workspace = harness.workspace().await;
    let main = harness
        .local
        .selected_branch(workspace.id)
        .await
        .unwrap()
        .unwrap();
    let mounted = success(
        harness
            .engine
            .services
            .mounts
            .prepare(
                context(),
                PrepareMount {
                    workspace: workspace.id,
                    branch: None,
                    revision: None,
                    path: harness.directory.path().join("selected"),
                    access: AccessMode::ReadWrite,
                    durability: Durability::Local,
                },
            )
            .await
            .unwrap(),
    );
    let mount = match mounted.result.unwrap() {
        CommandResult::Mount { binding } => binding,
        _ => panic!("mount missing"),
    };
    let fs = FsContext {
        mount: mount.id,
        generation: mount.generation,
        identity: identity(),
    };
    let file = harness.write_new(&fs, "first.txt", b"main content").await;
    let created = success(
        harness
            .engine
            .services
            .revisions
            .fork(
                context(),
                ForkBranch {
                    workspace: workspace.id,
                    source: None,
                    source_branch: None,
                    name: "feature".into(),
                },
            )
            .await
            .unwrap(),
    );
    let feature = created.branch.unwrap();
    harness
        .write_new(&fs, "later.txt", b"saved before switching")
        .await;
    let handle = harness
        .engine
        .services
        .files
        .open(
            &fs,
            file.id,
            OpenMode {
                read: true,
                write: false,
                append: false,
            },
        )
        .await
        .unwrap();
    let switched = success(
        harness
            .engine
            .services
            .mounts
            .switch_branch(
                context(),
                SwitchBranch {
                    workspace: workspace.id,
                    branch: feature,
                },
            )
            .await
            .unwrap(),
    );
    assert_eq!(switched.phase, OperationPhase::WaitingForQuiesce);
    assert_eq!(
        harness.local.selected_branch(workspace.id).await.unwrap(),
        Some(main)
    );
    let rejected = harness
        .engine
        .services
        .mounts
        .release(
            context(),
            ReleaseMount {
                operation: switched.id,
                expected_binding_generation: mount.generation,
            },
        )
        .await
        .unwrap();
    assert_eq!(rejected.error.unwrap().code, ErrorCode::MountBusy);
    assert_eq!(
        harness.local.selected_branch(workspace.id).await.unwrap(),
        Some(main)
    );
    harness
        .engine
        .services
        .files
        .release(&fs, handle.id)
        .await
        .unwrap();
    success(
        harness
            .engine
            .services
            .mounts
            .release(
                context(),
                ReleaseMount {
                    operation: switched.id,
                    expected_binding_generation: mount.generation,
                },
            )
            .await
            .unwrap(),
    );
    assert_eq!(
        harness.local.selected_branch(workspace.id).await.unwrap(),
        Some(feature)
    );
    let binding = harness
        .local
        .mounts()
        .await
        .unwrap()
        .into_iter()
        .find(|binding| binding.state == MountState::Ready && binding.path == mount.path)
        .unwrap();
    assert_eq!(binding.branch, feature);
    assert_ne!(binding.id, mount.id);
    let stale = harness
        .engine
        .services
        .mounts
        .save(
            context(),
            SaveMount {
                mount: mount.id,
                expected_binding_generation: mount.generation,
                durability: Durability::Local,
            },
        )
        .await
        .unwrap();
    assert_eq!(stale.error.unwrap().code, ErrorCode::StaleBinding);
    let main = harness.local.branch(main).await.unwrap().unwrap();
    assert_eq!(
        harness
            .engine
            .services
            .revisions
            .read_file(
                &actor(),
                workspace.id,
                main.autosave_head.unwrap_or(main.formal_head),
                path("/later.txt"),
                0,
                1024
            )
            .await
            .unwrap(),
        b"saved before switching"[..]
    );
    let branch = harness.local.branch(feature).await.unwrap().unwrap();
    assert!(
        harness
            .engine
            .services
            .revisions
            .read_file(
                &actor(),
                workspace.id,
                branch.formal_head,
                path("/later.txt"),
                0,
                1024
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn cache_uses_reloaded_capacity_and_collects_to_three_quarters() {
    let directory = tempfile::tempdir().unwrap();
    let path = initialize(
        &directory.path().join("configuration"),
        "127.0.0.1:7421".parse().unwrap(),
    )
    .unwrap();
    let config = ConfigStore::open(&path).unwrap();
    let (local, engine, clock, driver) = open_with_config(
        directory.path(),
        Some(Arc::new(MemoryRemote::default())),
        config.live(),
    )
    .await;
    let harness = Harness {
        directory,
        local,
        engine,
        clock,
        driver,
    };
    let workspace = harness.workspace().await;
    let (source, source_fs) = harness.branch_mount(workspace.id, "source", None).await;
    let mib = 1024 * 1024;
    for index in 0..4 {
        let name = format!("old-{index}");
        harness
            .write_new(&source_fs, &name, &vec![index; mib])
            .await;
        harness.commit(source.branch).await;
        harness
            .engine
            .services
            .files
            .unlink(&source_fs, InodeId::ROOT, &name, false)
            .await
            .unwrap();
    }
    let current = harness
        .write_new(&source_fs, "current", &vec![4; mib])
        .await;
    harness.commit(source.branch).await;
    let current_object = harness
        .local
        .node(source.branch, current.id)
        .await
        .unwrap()
        .unwrap()
        .inode
        .content
        .unwrap();
    harness.sync(workspace.id, SyncDirection::Push).await;
    assert_eq!(harness.engine.services.cache.collect().await.unwrap(), 0);
    assert!(harness.local.stats().await.unwrap().cached_bytes > 5 * mib as u64);
    let initial = config.settings(&actor()).unwrap();
    let mut runtime = initial.runtime;
    runtime.cache = CacheConfig {
        max_bytes: 4 * mib as u64,
    };
    config
        .update(
            &actor(),
            SettingsUpdate {
                revision: initial.revision,
                runtime,
            },
        )
        .unwrap();
    assert!(harness.engine.services.cache.collect().await.unwrap() > 0);
    let remaining = harness.local.stats().await.unwrap().cached_bytes;
    assert!(remaining <= 3 * mib as u64, "remaining bytes: {remaining}");
    assert!(remaining > 2 * mib as u64, "remaining bytes: {remaining}");
    assert_eq!(harness.engine.services.cache.collect().await.unwrap(), 0);
    assert!(
        harness
            .local
            .contains(workspace.id, &current_object)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn storage_switch_preserves_history_mounts_and_pending_publications() {
    let remote = Arc::new(MemoryRemote::default());
    let harness = Harness::new(None).await;
    let workspace = harness.workspace().await;
    let (binding, fs) = harness.branch_mount(workspace.id, "switch", None).await;
    let file = harness.write_new(&fs, "file", b"old bytes").await;
    let old = harness.commit(binding.branch).await;
    let object = harness
        .local
        .node(binding.branch, file.id)
        .await
        .unwrap()
        .unwrap()
        .inode
        .content
        .unwrap();
    let target = || {
        Some(RemoteStorage {
            objects: remote.clone(),
            refs: remote.clone(),
        })
    };
    let mut persisted = false;
    remote.set_available(false);
    assert!(
        harness
            .engine
            .switch_storage(target(), &mut || {
                persisted = true;
                Ok(())
            })
            .await
            .is_err()
    );
    assert!(!persisted);
    assert!(
        !harness
            .engine
            .services
            .workspace
            .status(&actor(), workspace.id)
            .await
            .unwrap()
            .capabilities
            .remote_configured
    );
    remote.set_available(true);
    // 配置文件保存失败时，当前适配器继续有效；远程内容可以重复发布。
    assert!(
        harness
            .engine
            .switch_storage(target(), &mut || Err(Error::new(
                ErrorCode::Io,
                "injected configuration write failure"
            )))
            .await
            .is_err()
    );
    assert!(
        !harness
            .engine
            .services
            .workspace
            .status(&actor(), workspace.id)
            .await
            .unwrap()
            .capabilities
            .remote_configured
    );
    harness
        .engine
        .switch_storage(target(), &mut || Ok(()))
        .await
        .unwrap();
    harness.engine.sync_pending().await.unwrap();
    assert!(
        harness
            .local
            .sync_jobs(workspace.id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(remote.contains(workspace.id, &object).await.unwrap());
    let handle = harness
        .engine
        .services
        .files
        .open(
            &fs,
            file.id,
            OpenMode {
                read: true,
                write: true,
                append: false,
            },
        )
        .await
        .unwrap();
    harness
        .engine
        .services
        .files
        .write(&fs, handle.id, 0, Bytes::from_static(b"new bytes"))
        .await
        .unwrap();
    harness
        .engine
        .services
        .files
        .release(&fs, handle.id)
        .await
        .unwrap();
    harness.commit(binding.branch).await;
    harness.engine.sync_pending().await.unwrap();
    harness.engine.services.cache.collect().await.unwrap();
    assert!(!harness.local.contains(workspace.id, &object).await.unwrap());
    remote.set_available(false);
    assert!(
        harness
            .engine
            .switch_storage(None, &mut || Ok(()))
            .await
            .is_err()
    );
    assert!(
        harness
            .engine
            .services
            .workspace
            .status(&actor(), workspace.id)
            .await
            .unwrap()
            .capabilities
            .remote_configured
    );
    remote.set_available(true);
    harness
        .engine
        .switch_storage(None, &mut || Ok(()))
        .await
        .unwrap();
    remote.set_available(false);
    assert!(harness.local.contains(workspace.id, &object).await.unwrap());
    assert_eq!(harness.engine.services.cache.collect().await.unwrap(), 0);
    let read = harness
        .engine
        .services
        .revisions
        .read_file(&actor(), workspace.id, old, path("/file"), 0, 100)
        .await
        .unwrap();
    assert_eq!(read.as_ref(), b"old bytes");
    assert_eq!(
        harness.local.mount(fs.mount).await.unwrap().unwrap().state,
        MountState::Ready
    );
    harness.write_new(&fs, "after", b"local change").await;
    harness.commit(binding.branch).await;
    assert!(
        !harness
            .local
            .sync_jobs(workspace.id)
            .await
            .unwrap()
            .is_empty()
    );
    remote.set_available(true);
    harness
        .engine
        .switch_storage(target(), &mut || Ok(()))
        .await
        .unwrap();
    harness.engine.sync_pending().await.unwrap();
    assert!(
        harness
            .local
            .sync_jobs(workspace.id)
            .await
            .unwrap()
            .is_empty()
    );
    // 另一个存储目标也能接收已经完成同步的历史与分支引用。
    let other = Arc::new(MemoryRemote::default());
    harness
        .engine
        .switch_storage(
            Some(RemoteStorage {
                objects: other.clone(),
                refs: other.clone(),
            }),
            &mut || Ok(()),
        )
        .await
        .unwrap();
    assert!(other.contains(workspace.id, &object).await.unwrap());
    assert!(!other.list_branches(workspace.id).await.unwrap().is_empty());
}

fn default_mount(workspace: WorkspaceId, path: std::path::PathBuf) -> PrepareMount {
    PrepareMount {
        workspace,
        branch: None,
        revision: None,
        path,
        access: AccessMode::ReadWrite,
        durability: Durability::Local,
    }
}

fn mounted(record: OperationRecord) -> MountBinding {
    match success(record).result.unwrap() {
        CommandResult::Mount { binding } => binding,
        result => panic!("{result:?}"),
    }
}

#[tokio::test]
async fn concurrent_mounts_reuse_location_branch_and_mount() {
    let harness = Harness::new(None).await;
    let workspace = harness.workspace().await;
    let request = default_mount(workspace.id, harness.directory.path().join("shared"));
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..16 {
        let engine = harness.engine.clone();
        let request = request.clone();
        tasks.spawn(async move {
            mounted(
                engine
                    .services
                    .mounts
                    .prepare(context(), request)
                    .await
                    .unwrap(),
            )
        });
    }
    let mut bindings = Vec::new();
    while let Some(result) = tasks.join_next().await {
        bindings.push(result.unwrap());
    }
    assert!(bindings.iter().all(|binding| binding == &bindings[0]));
    assert_eq!(harness.local.branches(workspace.id).await.unwrap().len(), 1);
    assert_eq!(harness.local.mounts().await.unwrap().len(), 1);
    let binding = &bindings[0];
    let other_path = harness
        .engine
        .services
        .mounts
        .prepare(
            context(),
            default_mount(workspace.id, harness.directory.path().join("another")),
        )
        .await
        .unwrap();
    assert_eq!(other_path.error.unwrap().code, ErrorCode::MountBusy);
    success(
        harness
            .engine
            .services
            .mounts
            .unmount(context(), binding.id, binding.generation)
            .await
            .unwrap(),
    );
    let moved = mounted(
        harness
            .engine
            .services
            .mounts
            .prepare(
                context(),
                default_mount(workspace.id, harness.directory.path().join("another")),
            )
            .await
            .unwrap(),
    );
    assert_eq!(moved.branch, binding.branch);
    assert_eq!(harness.local.branches(workspace.id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn failed_mount_retry_cannot_replace_a_later_binding() {
    let harness = Harness::new(None).await;
    let workspace = harness.workspace().await;
    let request = default_mount(workspace.id, harness.directory.path().join("original"));
    let retry = context();
    harness
        .driver
        .fail_mount
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let failed = harness
        .engine
        .services
        .mounts
        .prepare(retry.clone(), request.clone())
        .await
        .unwrap();
    assert_eq!(failed.error.unwrap().code, ErrorCode::Unavailable);
    let recovered = mounted(
        harness
            .engine
            .services
            .mounts
            .prepare(context(), request.clone())
            .await
            .unwrap(),
    );
    success(
        harness
            .engine
            .services
            .mounts
            .unmount(context(), recovered.id, recovered.generation)
            .await
            .unwrap(),
    );
    let current = mounted(
        harness
            .engine
            .services
            .mounts
            .prepare(
                context(),
                default_mount(workspace.id, harness.directory.path().join("current")),
            )
            .await
            .unwrap(),
    );
    let stale = harness
        .engine
        .services
        .mounts
        .prepare(retry, request)
        .await
        .unwrap();
    assert_eq!(stale.error.unwrap().code, ErrorCode::StaleBinding);
    assert_eq!(
        harness.local.mount(current.id).await.unwrap(),
        Some(current)
    );
    assert_eq!(
        harness
            .local
            .mount(recovered.id)
            .await
            .unwrap()
            .unwrap()
            .state,
        MountState::Released
    );
    assert_eq!(harness.local.branches(workspace.id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn new_locations_fork_latest_published_root_and_persist_selection() {
    let remote = Arc::new(MemoryRemote::default());
    let first = Harness::new(Some(remote.clone())).await;
    let workspace = first.workspace().await;
    let main = mounted(
        first
            .engine
            .services
            .mounts
            .prepare(
                context(),
                default_mount(workspace.id, first.directory.path().join("root")),
            )
            .await
            .unwrap(),
    );
    let fs = FsContext {
        mount: main.id,
        generation: main.generation,
        identity: identity(),
    };
    first
        .write_new(&fs, "latest.txt", b"published working content")
        .await;
    success(
        first
            .engine
            .services
            .mounts
            .save(
                context(),
                SaveMount {
                    mount: main.id,
                    expected_binding_generation: main.generation,
                    durability: Durability::Remote,
                },
            )
            .await
            .unwrap(),
    );
    let saved = first.local.branch(main.branch).await.unwrap().unwrap();
    let second = Harness::new(Some(remote.clone())).await;
    let mount_path = second.directory.path().join("remote-workspace");
    let request = default_mount(workspace.id, mount_path);
    second
        .driver
        .fail_mount
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let failed = second
        .engine
        .services
        .mounts
        .prepare(context(), request.clone())
        .await
        .unwrap();
    assert_eq!(
        failed.error.as_ref().map(|error| error.code),
        Some(ErrorCode::Unavailable),
        "{failed:?}"
    );
    let selected = second
        .local
        .selected_branch(workspace.id)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(selected, main.branch);
    assert_eq!(second.local.branches(workspace.id).await.unwrap().len(), 1);
    let binding = mounted(
        second
            .engine
            .services
            .mounts
            .prepare(context(), request.clone())
            .await
            .unwrap(),
    );
    assert_eq!(binding.branch, selected);
    let branch = second.local.branch(selected).await.unwrap().unwrap();
    assert_eq!(branch.source_branch, Some(main.branch));
    assert_eq!(branch.base_revision, saved.autosave_head.unwrap());
    assert_eq!(branch.working_root, saved.working_root);
    assert_eq!(branch.owner, second.engine.runtime.location);
    assert_eq!(
        second
            .engine
            .services
            .revisions
            .read_file(
                &actor(),
                workspace.id,
                branch.formal_head,
                path("/latest.txt"),
                0,
                1024
            )
            .await
            .unwrap(),
        "published working content"
    );
    for _ in 0..3 {
        let repeated = mounted(
            second
                .engine
                .services
                .mounts
                .prepare(context(), request.clone())
                .await
                .unwrap(),
        );
        assert_eq!(repeated, binding);
    }
    let third = Harness::new(Some(remote.clone())).await;
    let third_mount = mounted(
        third
            .engine
            .services
            .mounts
            .prepare(
                context(),
                default_mount(workspace.id, third.directory.path().join("third")),
            )
            .await
            .unwrap(),
    );
    assert_ne!(third_mount.branch, selected);
    assert_ne!(third_mount.branch, main.branch);
    let location = second.engine.runtime.location;
    let Harness {
        directory,
        local,
        engine,
        clock,
        driver,
    } = second;
    drop((local, engine, clock, driver));
    let (local, engine, _, _) = open(directory.path(), Some(remote)).await;
    assert_eq!(engine.runtime.location, location);
    let restored = mounted(
        engine
            .services
            .mounts
            .prepare(context(), request)
            .await
            .unwrap(),
    );
    assert_eq!(restored.branch, selected);
    assert_eq!(local.branches(workspace.id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn mount_save_is_idempotent_and_can_confirm_remote_without_another_revision() {
    let harness = Harness::new(Some(Arc::new(MemoryRemote::default()))).await;
    let workspace = harness.workspace().await;
    let binding = mounted(
        harness
            .engine
            .services
            .mounts
            .prepare(
                context(),
                default_mount(workspace.id, harness.directory.path().join("save")),
            )
            .await
            .unwrap(),
    );
    let fs = FsContext {
        mount: binding.id,
        generation: binding.generation,
        identity: identity(),
    };
    harness.write_new(&fs, "saved.txt", b"content").await;
    let request = SaveMount {
        mount: binding.id,
        expected_binding_generation: binding.generation,
        durability: Durability::Local,
    };
    let identity = context();
    let first = success(
        harness
            .engine
            .services
            .mounts
            .save(identity.clone(), request.clone())
            .await
            .unwrap(),
    );
    let repeated = success(
        harness
            .engine
            .services
            .mounts
            .save(identity, request.clone())
            .await
            .unwrap(),
    );
    assert_eq!(first, repeated);
    let count = harness.local.revisions(workspace.id).await.unwrap().len();
    let confirmed = success(
        harness
            .engine
            .services
            .mounts
            .save(
                context(),
                SaveMount {
                    durability: Durability::Remote,
                    ..request
                },
            )
            .await
            .unwrap(),
    );
    assert!(confirmed.remote_confirmed);
    assert_eq!(confirmed.result, first.result);
    assert_eq!(
        harness.local.revisions(workspace.id).await.unwrap().len(),
        count
    );
}

#[tokio::test]
async fn explicit_branch_selection_survives_unmount_and_restart() {
    let harness = Harness::new(None).await;
    let workspace = harness.workspace().await;
    let created = success(
        harness
            .engine
            .services
            .revisions
            .fork(
                context(),
                ForkBranch {
                    workspace: workspace.id,
                    source: None,
                    source_branch: None,
                    name: "feature".into(),
                },
            )
            .await
            .unwrap(),
    );
    let feature = created.branch.unwrap();
    success(
        harness
            .engine
            .services
            .mounts
            .switch_branch(
                context(),
                SwitchBranch {
                    workspace: workspace.id,
                    branch: feature,
                },
            )
            .await
            .unwrap(),
    );
    let request = default_mount(workspace.id, harness.directory.path().join("selected"));
    let binding = mounted(
        harness
            .engine
            .services
            .mounts
            .prepare(context(), request.clone())
            .await
            .unwrap(),
    );
    assert_eq!(binding.branch, feature);
    success(
        harness
            .engine
            .services
            .mounts
            .unmount(context(), binding.id, binding.generation)
            .await
            .unwrap(),
    );
    let Harness {
        directory,
        local,
        engine,
        clock,
        driver,
    } = harness;
    drop((local, engine, clock, driver));
    let (local, engine, _, _) = open(directory.path(), None).await;
    let restored = mounted(
        engine
            .services
            .mounts
            .prepare(context(), request)
            .await
            .unwrap(),
    );
    assert_eq!(restored.branch, feature);
    assert_eq!(local.branches(workspace.id).await.unwrap().len(), 2);
}
