mod support;
use agentdrive_config::{LiveConfig, RuntimeConfig};
use agentdrive_engine::{Adapters, Engine, EngineConfig, RemoteStorage};
use agentdrive_local::LocalBackend;
use agentdrive_model::*;
use agentdrive_opencsg::OpenCsgBackend;
use agentdrive_ports::*;
use agentdrive_test_support::{ManualClock, MemoryMountDriver, NoHostDirectories};
use bytes::Bytes;
use std::{path::Path, sync::Arc};
use support::Server;

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
fn identity() -> LocalIdentity {
    LocalIdentity {
        uid: 1000,
        gid: 1000,
    }
}

async fn open(
    path: &Path,
    backend: Option<Arc<OpenCsgBackend>>,
) -> (Arc<LocalBackend>, Arc<Engine>, Arc<ManualClock>) {
    let local = Arc::new(LocalBackend::open(path).unwrap());
    let clock = Arc::new(ManualClock::default());
    let engine = Engine::open(
        Adapters {
            state: local.clone(),
            objects: local.clone(),
            working: local.clone(),
            remote_objects: backend.clone().map(|v| v as Arc<dyn RemoteObjectStore>),
            remote_refs: backend.map(|v| v as Arc<dyn RemoteRefStore>),
            clock: clock.clone(),
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
    (local, engine, clock)
}

#[tokio::test]
async fn storage_switch_preserves_autosaves_commits_and_history_on_a_fresh_location() {
    let server = Server::new().await;
    let first_dir = tempfile::tempdir().unwrap();
    let second_dir = tempfile::tempdir().unwrap();
    let (local, engine, clock) = open(first_dir.path(), None).await;
    let created = success(
        engine
            .services
            .workspace
            .create(
                context(),
                CreateWorkspace {
                    name: "HTTP storage".into(),
                    grants: vec![],
                    max_file_bytes: None,
                    max_working_bytes: None,
                    max_inodes: None,
                },
            )
            .await
            .unwrap(),
    );
    let CommandResult::Workspace { workspace } = created.result.unwrap() else {
        panic!("workspace");
    };
    let prepared = success(
        engine
            .services
            .mounts
            .prepare(
                context(),
                PrepareMount {
                    workspace: workspace.id,
                    branch: None,
                    revision: None,
                    path: first_dir.path().join("mounted"),
                    access: AccessMode::ReadWrite,
                    durability: Durability::Local,
                },
            )
            .await
            .unwrap(),
    );
    let CommandResult::Mount { binding } = prepared.result.unwrap() else {
        panic!("mount");
    };
    let fs = FsContext {
        mount: binding.id,
        generation: binding.generation,
        identity: identity(),
    };
    let file = engine
        .services
        .files
        .create(&fs, InodeId::ROOT, "binary.dat", FileKind::File, 0o644)
        .await
        .unwrap();
    let handle = engine
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
    let content = Bytes::from_static(&[0, 255, 1, 128, 10, 13]);
    engine
        .services
        .files
        .write(&fs, handle.id, 0, content.clone())
        .await
        .unwrap();
    engine.services.files.release(&fs, handle.id).await.unwrap();
    clock.advance(5_000);
    assert_eq!(engine.autosave_due().await.unwrap(), 1);
    let branch = local.branch(binding.branch).await.unwrap().unwrap();
    let autosave = branch.autosave_head.unwrap();
    let committed = success(
        engine
            .services
            .mounts
            .save(
                context(),
                SaveMount {
                    mount: binding.id,
                    expected_binding_generation: binding.generation,
                    durability: Durability::Local,
                    wait_for_remote_ms: None,
                },
            )
            .await
            .unwrap(),
    );
    let CommandResult::Revision { revision, .. } = committed.result.unwrap() else {
        panic!("revision");
    };
    let backend = server.backend();
    backend.check_connection().await.unwrap();
    let mut persisted = false;
    engine
        .switch_storage(
            Some(RemoteStorage {
                objects: backend.clone(),
                refs: backend,
            }),
            &mut || {
                persisted = true;
                Ok(())
            },
        )
        .await
        .unwrap();
    assert!(persisted);
    let (second_local, second, _) = open(second_dir.path(), Some(server.backend())).await;
    success(
        second
            .services
            .replica
            .sync(
                context(),
                SyncRequest {
                    workspace: workspace.id,
                    branches: None,
                    direction: SyncDirection::Pull,
                },
            )
            .await
            .unwrap(),
    );
    assert_eq!(
        second_local.revision(autosave).await.unwrap().unwrap().kind,
        RevisionKind::Autosave
    );
    let imported = second_local.revision(revision).await.unwrap().unwrap();
    assert_eq!(imported.kind, RevisionKind::Commit);
    assert_eq!(imported.parents, vec![autosave]);
    for version in [autosave, revision] {
        let bytes = second
            .services
            .revisions
            .read_file(
                &actor(),
                workspace.id,
                version,
                "/binary.dat".to_owned().try_into().unwrap(),
                0,
                1024,
            )
            .await
            .unwrap();
        assert_eq!(bytes, content);
    }
    for object in local.objects(workspace.id).await.unwrap() {
        if object.remote_confirmed && object.reference.kind == ObjectKind::File {
            local.evict(workspace.id, &object.reference).await.unwrap();
        }
    }
    engine.switch_storage(None, &mut || Ok(())).await.unwrap();
    server.data.lock().unwrap().unavailable = true;
    let bytes = engine
        .services
        .revisions
        .read_file(
            &actor(),
            workspace.id,
            revision,
            "/binary.dat".to_owned().try_into().unwrap(),
            0,
            1024,
        )
        .await
        .unwrap();
    assert_eq!(bytes, content);
}
