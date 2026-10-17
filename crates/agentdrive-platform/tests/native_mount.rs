#![cfg(any(
    target_os = "linux",
    windows,
    all(target_os = "macos", feature = "macos-mount")
))]
use agentdrive_config::{LiveConfig, RuntimeConfig};
use agentdrive_engine::{Adapters, Engine, EngineConfig};
use agentdrive_local::LocalBackend;
use agentdrive_model::*;
use agentdrive_platform::{HostDirectories, NativeMountDriver, SystemClock, local_identity};
use agentdrive_ports::*;
use std::{
    io::{Read, Seek, SeekFrom, Write},
    sync::Arc,
};

fn user_entries(path: &std::path::Path) -> usize {
    std::fs::read_dir(path)
        .unwrap()
        .filter(|entry| {
            let name = entry.as_ref().unwrap().file_name();
            // macOS 会为不支持扩展属性的文件系统创建 AppleDouble 文件。
            !cfg!(target_os = "macos") || !name.to_string_lossy().starts_with("._")
        })
        .count()
}

fn context() -> RequestContext {
    RequestContext {
        principal: "native-test".to_owned().try_into().unwrap(),
        request_id: OperationId::new().to_string(),
    }
}
fn success(record: OperationRecord) -> OperationRecord {
    assert!(record.error.is_none(), "{record:?}");
    record
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an installed native filesystem driver and mount permission"]
async fn native_mount_supports_file_io_durable_snapshots_and_read_only_history() {
    let root = tempfile::tempdir().unwrap();
    let local = Arc::new(LocalBackend::open(root.path().join("state")).unwrap());
    let mounts = root.path().join("mounts");
    let driver = NativeMountDriver::new(vec![mounts.clone()]).unwrap();
    let engine = Engine::open(
        Adapters {
            state: local.clone(),
            objects: local.clone(),
            working: local.clone(),
            remote_objects: None,
            remote_refs: None,
            clock: Arc::new(SystemClock::default()),
            mounts: driver.clone(),
            directories: HostDirectories::new(vec![]).unwrap(),
        },
        EngineConfig {
            live: LiveConfig::new(RuntimeConfig::default()).unwrap(),
            identity: local_identity(),
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
                    name: "native".into(),
                    grants: vec![],
                    max_file_bytes: None,
                    max_working_bytes: None,
                    max_inodes: None,
                },
            )
            .await
            .unwrap(),
    );
    let workspace = match created.result.unwrap() {
        CommandResult::Workspace { workspace } => workspace,
        _ => panic!("wrong result"),
    };
    let path = mounts.join("working");
    let normalized = driver.normalize_path(&path).await.unwrap();
    let opened = success(
        engine
            .services
            .mounts
            .prepare(
                context(),
                PrepareMount {
                    workspace: workspace.id,
                    branch: None,
                    revision: None,
                    path: path.clone(),
                    access: AccessMode::ReadWrite,
                    durability: Durability::Local,
                },
            )
            .await
            .unwrap(),
    );
    assert_eq!(opened.mount_ready, Some(true));
    assert_eq!(driver.normalize_path(&path).await.unwrap(), normalized);
    let binding = match opened.result.unwrap() {
        CommandResult::Mount { binding } => binding,
        _ => panic!("wrong result"),
    };
    let operation_path = path.clone();
    let expected = tokio::task::spawn_blocking(move || {
        let path = operation_path;
        std::fs::create_dir(path.join("directory")).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(path.join("directory/Data"))
            .unwrap();
        let payload: Vec<u8> = (0..3 * 1024 * 1024 + 17)
            .map(|index| (index % 251) as u8)
            .collect();
        file.write_all(&payload).unwrap();
        file.seek(SeekFrom::Start(23)).unwrap();
        file.write_all(b"updated").unwrap();
        file.set_len(1_000_013).unwrap();
        file.sync_all().unwrap();
        std::fs::rename(path.join("directory/Data"), path.join("directory/Renamed")).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut expected = Vec::new();
        file.read_to_end(&mut expected).unwrap();
        assert_eq!(&expected[23..30], b"updated");
        assert_eq!(expected.len(), 1_000_013);
        drop(file);
        let mut append = std::fs::OpenOptions::new()
            .append(true)
            .open(path.join("directory/renamed"))
            .unwrap();
        append.write_all(b"appended").unwrap();
        append.sync_all().unwrap();
        drop(append);
        expected.extend_from_slice(b"appended");
        assert_eq!(
            std::fs::read(path.join("directory/Renamed")).unwrap(),
            expected
        );
        assert_eq!(user_entries(&path.join("directory")), 1);
        std::fs::write(path.join("temporary"), b"remove").unwrap();
        std::fs::remove_file(path.join("temporary")).unwrap();
        for index in 0..130 {
            std::fs::write(path.join(format!("entry-{index:03}")), [index as u8]).unwrap();
        }
        expected
    })
    .await
    .unwrap();
    let branch = local.branch(binding.branch).await.unwrap().unwrap();
    let committed = success(
        engine
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
    let revision = match committed.result.unwrap() {
        CommandResult::Revision { revision, .. } => revision,
        _ => panic!("wrong result"),
    };
    let history_path = mounts.join("history");
    let history = success(
        engine
            .services
            .mounts
            .prepare(
                context(),
                PrepareMount {
                    workspace: workspace.id,
                    branch: Some(binding.branch),
                    revision: Some(revision),
                    path: history_path.clone(),
                    access: AccessMode::ReadOnly,
                    durability: Durability::Local,
                },
            )
            .await
            .unwrap(),
    );
    assert_eq!(history.mount_ready, Some(true));
    tokio::task::spawn_blocking(move || {
        assert_eq!(
            std::fs::read(history_path.join("directory/Renamed")).unwrap(),
            expected
        );
        assert_eq!(user_entries(&history_path), 131);
        assert!(std::fs::write(history_path.join("forbidden"), b"readonly").is_err());
    })
    .await
    .unwrap();
    for mount in local.mounts().await.unwrap() {
        let record = success(
            engine
                .services
                .mounts
                .unmount(context(), mount.id, mount.generation)
                .await
                .unwrap(),
        );
        assert_eq!(record.phase, OperationPhase::Complete, "{record:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an installed native filesystem driver and mount permission"]
async fn native_directory_adoption_preserves_files_at_original_path() {
    let root = tempfile::tempdir().unwrap();
    let local = Arc::new(LocalBackend::open(root.path().join("state")).unwrap());
    let mounts = root.path().join("mounts");
    std::fs::create_dir_all(&mounts).unwrap();
    let source = mounts.join("project");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("original.txt"), b"existing data").unwrap();
    let driver = NativeMountDriver::new(vec![mounts.clone()]).unwrap();
    let engine = Engine::open(
        Adapters {
            state: local.clone(),
            objects: local.clone(),
            working: local.clone(),
            remote_objects: None,
            remote_refs: None,
            clock: Arc::new(SystemClock::default()),
            mounts: driver.clone(),
            directories: HostDirectories::new(vec![mounts]).unwrap(),
        },
        EngineConfig {
            live: LiveConfig::new(RuntimeConfig::default()).unwrap(),
            identity: local_identity(),
        },
    )
    .await
    .unwrap();
    let request_context = context();
    let request = AdoptDirectory {
        workspace: None,
        path: source.clone(),
        restart_agent: true,
    };
    let record = success(
        engine
            .services
            .workspace
            .adopt_directory(request_context.clone(), request.clone())
            .await
            .unwrap(),
    );
    let Some(CommandResult::DirectoryAdoption {
        binding: Some(binding),
        backup_path,
        restart_required,
        ..
    }) = record.result
    else {
        panic!("missing adopted mount")
    };
    assert!(restart_required);
    let path = source.clone();
    tokio::task::spawn_blocking(move || {
        assert_eq!(
            std::fs::read(path.join("original.txt")).unwrap(),
            b"existing data"
        );
        let mut file = std::fs::File::create(path.join("new.txt")).unwrap();
        file.write_all(b"mounted data").unwrap();
        file.sync_all().unwrap();
        assert_eq!(
            std::fs::read(backup_path.join("original.txt")).unwrap(),
            b"existing data"
        );
        assert!(!backup_path.join("new.txt").exists());
    })
    .await
    .unwrap();
    let replay = success(
        engine
            .services
            .workspace
            .adopt_directory(request_context, request)
            .await
            .unwrap(),
    );
    assert_eq!(replay.id, record.id);
    success(
        engine
            .services
            .mounts
            .unmount(context(), binding.id, binding.generation)
            .await
            .unwrap(),
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an installed native filesystem driver and mount permission"]
async fn native_branch_switch_completes_and_reports_file_and_cwd_occupancy() {
    let root = tempfile::tempdir().unwrap();
    let local = Arc::new(LocalBackend::open(root.path().join("state")).unwrap());
    let mounts = root.path().join("mounts");
    std::fs::create_dir_all(&mounts).unwrap();
    let source = mounts.join("project");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("original.txt"), b"existing data").unwrap();
    let driver = NativeMountDriver::new(vec![mounts.clone()]).unwrap();
    let engine = Engine::open(
        Adapters {
            state: local.clone(),
            objects: local.clone(),
            working: local.clone(),
            remote_objects: None,
            remote_refs: None,
            clock: Arc::new(SystemClock::default()),
            mounts: driver.clone(),
            directories: HostDirectories::new(vec![mounts]).unwrap(),
        },
        EngineConfig {
            live: LiveConfig::new(RuntimeConfig::default()).unwrap(),
            identity: local_identity(),
        },
    )
    .await
    .unwrap();

    let adopted = success(
        engine
            .services
            .workspace
            .adopt_directory(
                context(),
                AdoptDirectory {
                    workspace: None,
                    path: source.clone(),
                    restart_agent: true,
                },
            )
            .await
            .unwrap(),
    );
    let workspace = adopted.workspace.unwrap();
    let main = adopted.branch.unwrap();
    let forked = success(
        engine
            .services
            .revisions
            .fork(
                context(),
                ForkBranch {
                    workspace,
                    source: None,
                    source_branch: None,
                    name: "feature".into(),
                },
            )
            .await
            .unwrap(),
    );
    let feature = forked.branch.unwrap();
    let p = source.clone();
    let file = tokio::task::spawn_blocking(move || {
        let mut file = std::fs::File::open(p.join("original.txt")).unwrap();
        let mut byte = [0];
        file.read_exact(&mut byte).unwrap();
        file
    })
    .await
    .unwrap();
    let ctx = context();
    let request = SwitchBranch {
        workspace,
        branch: feature,
    };
    let busy = engine
        .services
        .mounts
        .switch_branch(ctx.clone(), request.clone())
        .await
        .unwrap();
    drop(file);
    assert_eq!(busy.error.as_ref().unwrap().code, ErrorCode::MountBusy);
    assert!(busy.error.unwrap().message.contains("restart the Agent"));
    assert_eq!(local.selected_branch(workspace).await.unwrap(), Some(main));
    let switched = success(
        engine
            .services
            .mounts
            .switch_branch(ctx, request)
            .await
            .unwrap(),
    );
    assert_eq!(switched.id, busy.id);
    assert_eq!(switched.mount_ready, Some(true));
    let p = source.clone();
    tokio::task::spawn_blocking(move || {
        std::fs::write(p.join("feature.txt"), b"feature only").unwrap();
    })
    .await
    .unwrap();
    let mut child = std::process::Command::new("/bin/sleep")
        .arg("60")
        .current_dir(&source)
        .spawn()
        .unwrap();
    let ctx = context();
    let request = SwitchBranch {
        workspace,
        branch: main,
    };
    let busy = engine
        .services
        .mounts
        .switch_branch(ctx.clone(), request.clone())
        .await;
    child.kill().unwrap();
    child.wait().unwrap();
    let busy = busy.unwrap();
    assert_eq!(busy.error.as_ref().unwrap().code, ErrorCode::MountBusy);
    assert!(busy.error.unwrap().message.contains("restart the Agent"));
    assert_eq!(
        local.selected_branch(workspace).await.unwrap(),
        Some(feature)
    );
    let returned = success(
        engine
            .services
            .mounts
            .switch_branch(ctx, request)
            .await
            .unwrap(),
    );
    assert_eq!(returned.mount_ready, Some(true));
    let p = source.clone();
    tokio::task::spawn_blocking(move || {
        assert_eq!(
            std::fs::read(p.join("original.txt")).unwrap(),
            b"existing data"
        );
        assert!(!p.join("feature.txt").exists());
    })
    .await
    .unwrap();
    let switched = success(
        engine
            .services
            .mounts
            .switch_branch(
                context(),
                SwitchBranch {
                    workspace,
                    branch: feature,
                },
            )
            .await
            .unwrap(),
    );
    assert_eq!(switched.mount_ready, Some(true));
    tokio::task::spawn_blocking(move || {
        assert_eq!(
            std::fs::read(source.join("feature.txt")).unwrap(),
            b"feature only"
        );
    })
    .await
    .unwrap();
    for mount in local.mounts().await.unwrap() {
        if mount.state == MountState::Ready {
            success(
                engine
                    .services
                    .mounts
                    .unmount(context(), mount.id, mount.generation)
                    .await
                    .unwrap(),
            );
        }
    }
}
