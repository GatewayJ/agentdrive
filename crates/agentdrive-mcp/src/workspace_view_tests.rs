use super::WorkspaceView;
use agentdrive_config::{LiveConfig, RuntimeConfig};
use agentdrive_engine::{Adapters, Engine, EngineConfig};
use agentdrive_local::LocalBackend;
use agentdrive_model::*;
use agentdrive_ports::*;
use agentdrive_test_support::{ManualClock, MemoryMountDriver, MemoryRemote, NoHostDirectories};
use bytes::Bytes;
use std::sync::Arc;

struct Location {
    directory: tempfile::TempDir,
    local: Arc<LocalBackend>,
    engine: Arc<Engine>,
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

fn identity() -> LocalIdentity {
    LocalIdentity {
        uid: 1000,
        gid: 1000,
    }
}

fn success(record: OperationRecord) -> CommandResult {
    assert!(record.error.is_none(), "{record:?}");
    record.result.unwrap()
}

impl Location {
    async fn new(remote: Arc<MemoryRemote>) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let local = Arc::new(LocalBackend::open(directory.path()).unwrap());
        let engine = Engine::open(
            Adapters {
                state: local.clone(),
                objects: local.clone(),
                working: local.clone(),
                remote_objects: Some(remote.clone()),
                remote_refs: Some(remote),
                clock: Arc::new(ManualClock::default()),
                mounts: Arc::new(MemoryMountDriver::default()),
                directories: Arc::new(NoHostDirectories),
            },
            EngineConfig {
                live: LiveConfig::new(RuntimeConfig::default()).unwrap(),
                identity: identity(),
            },
        )
        .await
        .unwrap();
        Self {
            directory,
            local,
            engine,
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

    async fn mount(&self, branch: &Branch) -> FsContext {
        let result = success(
            self.engine
                .services
                .mounts
                .prepare(
                    context(),
                    PrepareMount {
                        workspace: branch.workspace,
                        branch: Some(branch.id),
                        revision: None,
                        path: self.directory.path().join("project"),
                        access: AccessMode::ReadWrite,
                        durability: Durability::Local,
                    },
                )
                .await
                .unwrap(),
        );
        let CommandResult::Mount { binding } = result else {
            panic!("expected mount result");
        };
        FsContext {
            mount: binding.id,
            generation: binding.generation,
            identity: identity(),
        }
    }

    async fn write(&self, fs: &FsContext, name: &str) {
        let files = &self.engine.services.files;
        let node = files
            .create(fs, InodeId::ROOT, name, FileKind::File, 0o644)
            .await
            .unwrap();
        let handle = files
            .open(
                fs,
                node.id,
                OpenMode {
                    read: true,
                    write: true,
                    append: false,
                },
            )
            .await
            .unwrap();
        files
            .write(fs, handle.id, 0, Bytes::copy_from_slice(name.as_bytes()))
            .await
            .unwrap();
        files.release(fs, handle.id).await.unwrap();
    }

    async fn commit(&self, id: BranchId) -> RevisionId {
        let branch = self.local.branch(id).await.unwrap().unwrap();
        let result = success(
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
        let CommandResult::Revision { revision, .. } = result else {
            panic!("expected revision result");
        };
        revision
    }
}

#[tokio::test]
async fn former_owner_displays_and_forks_the_new_owners_published_content() {
    let remote = Arc::new(MemoryRemote::default());
    let first = Location::new(remote.clone()).await;
    let second = Location::new(remote).await;
    let result = success(
        first
            .engine
            .services
            .workspace
            .create(
                context(),
                CreateWorkspace {
                    name: "handoff".into(),
                    grants: vec![],
                    max_file_bytes: None,
                    max_working_bytes: None,
                    max_inodes: None,
                },
            )
            .await
            .unwrap(),
    );
    let CommandResult::Workspace { workspace } = result else {
        panic!("expected workspace result");
    };
    let branch = first.local.branches(workspace.id).await.unwrap().remove(0);
    let fs = first.mount(&branch).await;
    first.write(&fs, "from-a.txt").await;
    let previous_head = first.commit(branch.id).await;
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
    let branch = first.local.branch(branch.id).await.unwrap().unwrap();
    success(
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
    second.sync(workspace.id, SyncDirection::Pull).await;
    let adopted = second.local.branch(branch.id).await.unwrap().unwrap();
    let fs = second.mount(&adopted).await;
    second.write(&fs, "from-b.txt").await;
    let latest_head = second.commit(branch.id).await;
    second.sync(workspace.id, SyncDirection::Push).await;
    first.sync(workspace.id, SyncDirection::Pull).await;

    let services = &first.engine.services;
    let status = services
        .workspace
        .status(&actor(), workspace.id)
        .await
        .unwrap();
    let revisions = services
        .revisions
        .list(&actor(), workspace.id, None)
        .await
        .unwrap();
    let view = WorkspaceView::new(status, revisions);
    let displayed = view
        .branches
        .iter()
        .find(|item| item.id == branch.id)
        .unwrap();
    assert_eq!(displayed.formal_head, latest_head);
    assert_eq!(displayed.state, BranchState::Writable);
    assert_eq!(displayed.owner, second.engine.runtime.location);
    assert!(!displayed.local);
    assert!(displayed.local_available);
    assert_eq!(displayed.name, branch.name);
    assert_eq!(displayed.base_revision, Some(branch.base_revision));
    assert_eq!(
        first
            .local
            .branch(branch.id)
            .await
            .unwrap()
            .unwrap()
            .formal_head,
        previous_head
    );

    let relationships = services
        .workspace
        .relationships(
            &actor(),
            WorkspaceRelationshipsQuery {
                workspace: workspace.id,
                filter: RelationshipFilter {
                    branch: Some(branch.id),
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(relationships.entries[0].branch.state, displayed.state);
    assert_eq!(
        relationships.entries[0].branch.ownership.location,
        displayed.owner
    );

    let fork = success(
        services
            .revisions
            .fork(
                context(),
                ForkBranch {
                    workspace: workspace.id,
                    source: Some(displayed.formal_head),
                    source_branch: None,
                    name: "continue-on-a".into(),
                },
            )
            .await
            .unwrap(),
    );
    let CommandResult::Branch { branch: fork } = fork else {
        panic!("expected branch result");
    };
    assert_eq!(fork.base_revision, latest_head);
    assert!(
        first
            .local
            .node_at(fork.id, &"/from-b.txt".to_owned().try_into().unwrap())
            .await
            .unwrap()
            .is_some()
    );

    let mounted = success(
        services
            .mounts
            .prepare(
                context(),
                PrepareMount {
                    workspace: workspace.id,
                    branch: Some(branch.id),
                    revision: Some(displayed.autosave_head.unwrap_or(displayed.formal_head)),
                    path: first.directory.path().join("remote-preview"),
                    access: AccessMode::ReadOnly,
                    durability: Durability::Local,
                },
            )
            .await
            .unwrap(),
    );
    let CommandResult::Mount { binding } = mounted else {
        panic!("expected read-only mount result");
    };
    assert_eq!(binding.revision, Some(latest_head));
    let preview = FsContext {
        mount: binding.id,
        generation: binding.generation,
        identity: identity(),
    };
    let file = services
        .files
        .lookup(&preview, InodeId::ROOT, "from-b.txt")
        .await
        .unwrap();
    let handle = services
        .files
        .open(
            &preview,
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
        services
            .files
            .read(&preview, handle.id, 0, 128)
            .await
            .unwrap(),
        "from-b.txt"
    );
    services.files.release(&preview, handle.id).await.unwrap();
    assert_eq!(
        services
            .files
            .create(
                &preview,
                InodeId::ROOT,
                "blocked.txt",
                FileKind::File,
                0o644
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::ReadOnly
    );
    assert!(
        first
            .local
            .node_at(branch.id, &"/from-b.txt".to_owned().try_into().unwrap())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn discovered_remote_branch_history_pages_without_creating_a_local_copy() {
    let remote = Arc::new(MemoryRemote::default());
    let first = Location::new(remote.clone()).await;
    let second = Location::new(remote).await;
    let created = success(
        first
            .engine
            .services
            .workspace
            .create(
                context(),
                CreateWorkspace {
                    name: "remote-history".into(),
                    grants: vec![],
                    max_file_bytes: None,
                    max_working_bytes: None,
                    max_inodes: None,
                },
            )
            .await
            .unwrap(),
    );
    let CommandResult::Workspace { workspace } = created else {
        panic!("expected workspace result");
    };
    first.sync(workspace.id, SyncDirection::Push).await;
    second.sync(workspace.id, SyncDirection::Pull).await;
    let created = success(
        first
            .engine
            .services
            .revisions
            .fork(
                context(),
                ForkBranch {
                    workspace: workspace.id,
                    source: Some(workspace.initial_revision),
                    source_branch: None,
                    name: "remote-only".into(),
                },
            )
            .await
            .unwrap(),
    );
    let CommandResult::Branch { branch } = created else {
        panic!("expected branch result");
    };
    let fs = first.mount(&branch).await;
    let mut expected = std::collections::BTreeSet::new();
    for name in ["one.txt", "two.txt", "three.txt"] {
        first.write(&fs, name).await;
        expected.insert(first.commit(branch.id).await);
    }
    first.sync(workspace.id, SyncDirection::Push).await;
    let services = &second.engine.services;
    services
        .workspace
        .refresh_history(&actor(), workspace.id)
        .await
        .unwrap();
    assert!(second.local.branch(branch.id).await.unwrap().is_none());

    let query = RevisionHistoryQuery {
        workspace: workspace.id,
        branch: branch.id,
        search: None,
        limit: Some(2),
        cursor: None,
    };
    let first_page = services
        .revisions
        .history(&actor(), query.clone())
        .await
        .unwrap();
    assert_eq!(first_page.total, 3);
    assert_eq!(first_page.entries.len(), 2);
    let next = first_page.next.clone().expect("remaining remote history");
    let last_page = services
        .revisions
        .history(
            &actor(),
            RevisionHistoryQuery {
                cursor: Some(next.clone()),
                ..query.clone()
            },
        )
        .await
        .unwrap();
    assert_eq!(last_page.total, 3);
    assert_eq!(last_page.entries.len(), 1);
    assert!(last_page.next.is_none());
    let entries: Vec<_> = first_page
        .entries
        .iter()
        .chain(&last_page.entries)
        .collect();
    assert!(
        entries
            .iter()
            .all(|revision| revision.branch == Some(branch.id))
    );
    assert_eq!(
        entries
            .iter()
            .map(|revision| revision.id)
            .collect::<std::collections::BTreeSet<_>>(),
        expected
    );
    assert!(
        entries
            .windows(2)
            .all(|pair| pair[0].created_ns > pair[1].created_ns
                || pair[0].created_ns == pair[1].created_ns && pair[0].id < pair[1].id)
    );

    let reader: Principal = "branch-reader".to_owned().try_into().unwrap();
    assert_eq!(
        services
            .revisions
            .history(&reader, query.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    let unrelated = success(
        services
            .workspace
            .create(
                context(),
                CreateWorkspace {
                    name: "unrelated".into(),
                    grants: vec![],
                    max_file_bytes: None,
                    max_working_bytes: None,
                    max_inodes: None,
                },
            )
            .await
            .unwrap(),
    );
    let CommandResult::Workspace {
        workspace: unrelated,
    } = unrelated
    else {
        panic!("expected unrelated workspace result");
    };
    let unrelated_branch = second.local.branches(unrelated.id).await.unwrap().remove(0);
    for invalid in [
        RevisionHistoryQuery {
            branch: unrelated_branch.id,
            ..query.clone()
        },
        RevisionHistoryQuery {
            workspace: unrelated.id,
            ..query.clone()
        },
    ] {
        assert_eq!(
            services
                .revisions
                .history(&actor(), invalid)
                .await
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
    }

    let mut scoped = workspace.clone();
    scoped.grants.push(Grant {
        principal: reader.clone(),
        branch: Some(branch.id),
        permissions: vec![Permission::Read],
    });
    second
        .local
        .commit(LocalCommit {
            workspaces: vec![scoped],
            ..Default::default()
        })
        .await
        .unwrap();
    let readable = services
        .revisions
        .history(&reader, query.clone())
        .await
        .unwrap();
    assert_eq!(readable.total, 3);
    assert_eq!(readable.entries, first_page.entries);
    assert_eq!(
        services
            .workspace
            .status(&reader, workspace.id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    assert!(services.workspace.list(&reader).await.unwrap().is_empty());
    assert_eq!(
        services
            .revisions
            .history(
                &reader,
                RevisionHistoryQuery {
                    cursor: Some(next),
                    ..query.clone()
                }
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    let initial = second
        .local
        .revision(workspace.initial_revision)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        services
            .revisions
            .history(
                &reader,
                RevisionHistoryQuery {
                    branch: initial.branch.unwrap(),
                    ..query
                }
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    assert!(second.local.branch(branch.id).await.unwrap().is_none());
}
