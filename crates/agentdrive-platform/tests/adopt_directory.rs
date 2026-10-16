use agentdrive_config::{LiveConfig, RuntimeConfig};
use agentdrive_engine::{Adapters, Engine, EngineConfig};
use agentdrive_local::LocalBackend;
use agentdrive_model::*;
use agentdrive_platform::HostDirectories;
use agentdrive_ports::*;
use agentdrive_test_support::{ManualClock, MemoryMountDriver};
use std::{
    path::Path,
    sync::{Arc, atomic::Ordering},
};

fn context(id: &str) -> RequestContext {
    RequestContext {
        principal: "owner".to_owned().try_into().unwrap(),
        request_id: id.into(),
    }
}
async fn open(root: &Path, driver: Arc<MemoryMountDriver>) -> (Arc<LocalBackend>, Arc<Engine>) {
    let local = Arc::new(LocalBackend::open(root.join("state")).unwrap());
    let engine = Engine::open(
        Adapters {
            state: local.clone(),
            objects: local.clone(),
            working: local.clone(),
            remote_objects: None,
            remote_refs: None,
            clock: Arc::new(ManualClock::default()),
            mounts: driver,
            directories: HostDirectories::new(vec![root.to_path_buf()]).unwrap(),
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
    (local, engine)
}
async fn workspace(engine: &Engine) -> WorkspaceId {
    let result = engine
        .services
        .workspace
        .create(
            context("create"),
            CreateWorkspace {
                name: "test".into(),
                grants: vec![],
                max_file_bytes: None,
                max_working_bytes: None,
                max_inodes: None,
            },
        )
        .await
        .unwrap();
    assert!(result.error.is_none());
    result.workspace.unwrap()
}

#[tokio::test]
async fn adoption_requires_restart_and_resumes_after_mount_failure_and_restart() {
    check_adoption(true).await;
    check_adoption(false).await;
}

async fn check_adoption(existing: bool) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let source = root.join("project");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("hello.txt"), b"original content").unwrap();
    let driver = Arc::new(MemoryMountDriver::default());
    let (local, engine) = open(&root, driver.clone()).await;
    let target = if existing {
        Some(workspace(&engine).await)
    } else {
        None
    };
    let mut request = AdoptDirectory {
        workspace: target,
        path: source.clone(),
        restart_agent: false,
    };
    let error = engine
        .services
        .workspace
        .adopt_directory(context("adopt"), request.clone())
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert_eq!(
        local.workspaces().await.unwrap().len(),
        usize::from(existing)
    );
    request.restart_agent = true;
    driver.fail_mount.store(true, Ordering::SeqCst);
    let failed = engine
        .services
        .workspace
        .adopt_directory(context("adopt"), request.clone())
        .await
        .unwrap();
    let workspace = failed.workspace.unwrap();
    assert!(failed.error.is_some());
    assert_eq!(failed.phase, OperationPhase::RecoveryRequired);
    let Some(CommandResult::DirectoryAdoption {
        branch,
        backup_path,
        restart_required,
        ..
    }) = failed.result
    else {
        panic!("missing recovery information")
    };
    assert!(restart_required);
    assert_eq!(
        std::fs::read(backup_path.join("hello.txt")).unwrap(),
        b"original content"
    );
    assert_eq!(std::fs::read_dir(&source).unwrap().count(), 0);
    let imported = local.branch(branch).await.unwrap().unwrap();
    let revision = imported.formal_head;
    if !existing {
        let created = local.workspaces().await.unwrap().pop().unwrap();
        assert_eq!(created.name, "project");
        assert_eq!(created.initial_revision, revision);
        assert_eq!(imported.name, "main");
        assert_eq!(imported.base_revision, revision);
        assert_eq!(imported.source_branch, None);
        assert!(imported.next_inode > 2);
        assert_eq!(
            local.selected_branch(workspace).await.unwrap(),
            Some(branch)
        );
    }
    assert_eq!(
        engine
            .services
            .revisions
            .read_file(
                &context("read").principal,
                workspace,
                revision,
                "/hello.txt".to_owned().try_into().unwrap(),
                0,
                1024
            )
            .await
            .unwrap(),
        b"original content"[..]
    );
    drop((engine, local, driver));
    let (local, engine) = open(&root, Arc::new(MemoryMountDriver::default())).await;
    let done = engine
        .services
        .workspace
        .adopt_directory(context("adopt"), request.clone())
        .await
        .unwrap();
    assert!(done.error.is_none(), "{done:?}");
    assert_eq!(done.phase, OperationPhase::Complete);
    assert_eq!(done.mount_ready, Some(true));
    assert_eq!(done.branch, Some(branch));
    let replay = engine
        .services
        .workspace
        .adopt_directory(context("adopt"), request)
        .await
        .unwrap();
    assert_eq!(replay, done);
    assert_eq!(
        local.branches(workspace).await.unwrap().len(),
        if existing { 2 } else { 1 }
    );
    assert_eq!(local.workspaces().await.unwrap().len(), 1);
    assert_eq!(
        std::fs::read(backup_path.join("hello.txt")).unwrap(),
        b"original content"
    );
}

#[tokio::test]
async fn staging_retries_preserve_original_and_reject_new_files() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let directories = HostDirectories::new(vec![root.clone()]).unwrap();
    let source = root.join("source");
    let backup = root.join("backup");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("file"), b"original").unwrap();
    directories.stage_mount(&source, &backup).await.unwrap();
    directories.stage_mount(&source, &backup).await.unwrap();
    std::fs::write(source.join("new"), b"new write").unwrap();
    assert_eq!(
        directories
            .stage_mount(&source, &backup)
            .await
            .unwrap_err()
            .code,
        ErrorCode::MountBusy
    );
    assert_eq!(
        std::fs::read(backup.join("original/file")).unwrap(),
        b"original"
    );
    assert_eq!(std::fs::read(source.join("new")).unwrap(), b"new write");
}

#[cfg(unix)]
#[tokio::test]
async fn invalid_import_leaves_original_directory_untouched() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let source = root.join("project");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("file"), b"original").unwrap();
    std::os::unix::fs::symlink("file", source.join("link")).unwrap();
    let (local, engine) = open(&root, Arc::new(MemoryMountDriver::default())).await;
    let record = engine
        .services
        .workspace
        .adopt_directory(
            context("adopt"),
            AdoptDirectory {
                workspace: None,
                path: source.clone(),
                restart_agent: true,
            },
        )
        .await
        .unwrap();
    assert!(record.error.is_some());
    assert!(local.workspaces().await.unwrap().is_empty());
    assert_eq!(std::fs::read(source.join("file")).unwrap(), b"original");
    assert!(
        std::fs::symlink_metadata(source.join("link"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        !root
            .join(format!(".agentdrive-backup-{}", record.id))
            .exists()
    );
}
