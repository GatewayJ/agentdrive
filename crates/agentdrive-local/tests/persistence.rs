use agentdrive_local::LocalBackend;
use agentdrive_model::*;
use agentdrive_ports::*;
use bytes::Bytes;
use tokio::io::AsyncReadExt;

#[tokio::test]
async fn objects_are_verified_and_live_readers_prevent_eviction() {
    let directory = tempfile::tempdir().unwrap();
    let backend = LocalBackend::open(directory.path()).unwrap();
    let workspace = WorkspaceId::new();
    let object = backend
        .put(
            workspace,
            ObjectKind::File,
            Bytes::from_static(b"persistent bytes"),
        )
        .await
        .unwrap();
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_TEMPORARY;
        let stored = backend
            .data_dir()
            .join("objects")
            .join(workspace.to_string())
            .join(object.id.as_str());
        assert_eq!(
            std::fs::metadata(stored).unwrap().file_attributes() & FILE_ATTRIBUTE_TEMPORARY,
            0
        );
    }
    assert!(!backend.evict(workspace, &object).await.unwrap());
    backend.confirm_remote(workspace, &object).await.unwrap();
    let mut stream = backend.open(workspace, &object).await.unwrap();
    assert!(!backend.evict(workspace, &object).await.unwrap());
    let mut content = Vec::new();
    stream.reader.read_to_end(&mut content).await.unwrap();
    assert_eq!(content, b"persistent bytes");
    drop(stream);
    assert!(backend.evict(workspace, &object).await.unwrap());
    assert!(!backend.contains(workspace, &object).await.unwrap());
}

#[tokio::test]
async fn corrupt_download_is_never_published() {
    let directory = tempfile::tempdir().unwrap();
    let backend = LocalBackend::open(directory.path()).unwrap();
    let workspace = WorkspaceId::new();
    let reference = object_ref(ObjectKind::File, b"expected");
    let error = backend
        .install(
            workspace,
            ObjectStream {
                reference: reference.clone(),
                reader: Box::pin(std::io::Cursor::new(b"altered!")),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Integrity);
    assert!(!backend.contains(workspace, &reference).await.unwrap());
    assert!(backend.objects(workspace).await.unwrap().is_empty());
}

fn operation(request: &str) -> OperationRecord {
    OperationRecord {
        id: OperationId::new(),
        request_id: request.into(),
        principal: "owner".to_owned().try_into().unwrap(),
        workspace: None,
        branch: None,
        name: "create".into(),
        fingerprint: object_ref(ObjectKind::RecordIndex, b"parameters").id,
        phase: OperationPhase::Reserved,
        local_saved: false,
        remote_confirmed: false,
        mount_ready: None,
        result: None,
        error: None,
        pending: None,
        created_ns: 1,
        sync_progress: None,
    }
}

#[tokio::test]
async fn idempotency_survives_reopen_and_conflicting_commit_rolls_back() {
    let directory = tempfile::tempdir().unwrap();
    let backend = LocalBackend::open(directory.path()).unwrap();
    let location = backend.location().await.unwrap();
    assert_eq!(
        LocalBackend::open(directory.path()).unwrap_err().code,
        ErrorCode::Busy
    );
    let first = operation("request");
    assert!(
        backend
            .reserve_operation(first.clone())
            .await
            .unwrap()
            .created
    );
    let again = backend
        .reserve_operation(operation("request"))
        .await
        .unwrap();
    assert!(!again.created);
    assert_eq!(again.record.id, first.id);
    let mut changed = operation("request");
    changed.fingerprint = object_ref(ObjectKind::RecordIndex, b"different").id;
    assert_eq!(
        backend.reserve_operation(changed).await.unwrap_err().code,
        ErrorCode::RequestIdConflict
    );
    let workspace = Workspace {
        id: WorkspaceId::new(),
        name: "test".into(),
        owner: first.principal.clone(),
        initial_revision: RevisionId::new(),
        format_version: FORMAT_VERSION,
        name_policy: "portable_v1".into(),
        grants: vec![],
        max_file_bytes: 1024,
        max_working_bytes: 4096,
        max_inodes: 100,
    };
    let mut conflict = first.clone();
    conflict.name = "other".into();
    let error = backend
        .commit(LocalCommit {
            new_workspaces: vec![workspace.clone()],
            operations: vec![conflict],
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::RequestIdConflict);
    assert!(backend.workspace(workspace.id).await.unwrap().is_none());
    drop(backend);
    let backend = LocalBackend::open(directory.path()).unwrap();
    assert_eq!(backend.location().await.unwrap(), location);
    assert_eq!(backend.operation(first.id).await.unwrap(), Some(first));
}

#[test]
fn unknown_database_format_is_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("meta.db");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection.execute_batch("CREATE TABLE metadata(key TEXT PRIMARY KEY,value); INSERT INTO metadata VALUES('format_version',999);").unwrap();
    drop(connection);
    let before = std::fs::read(&database).unwrap();
    assert_eq!(
        LocalBackend::open(directory.path()).unwrap_err().code,
        ErrorCode::Unsupported
    );
    assert_eq!(before, std::fs::read(database).unwrap());
}

#[tokio::test]
async fn object_usage_survives_duplicates_migration_and_removal() {
    let dir = tempfile::tempdir().unwrap();
    let backend = LocalBackend::open(dir.path()).unwrap();
    let workspace = WorkspaceId::new();
    let value = Bytes::from_static(b"accounted bytes");
    let object = backend
        .put(workspace, ObjectKind::File, value.clone())
        .await
        .unwrap();
    backend
        .put(workspace, ObjectKind::File, value)
        .await
        .unwrap();
    assert_eq!(backend.stats().await.unwrap().protected_bytes, object.size);
    backend.confirm_remote(workspace, &object).await.unwrap();
    backend.confirm_remote(workspace, &object).await.unwrap();
    let stats = backend.stats().await.unwrap();
    assert_eq!(
        (stats.cached_bytes, stats.protected_bytes),
        (object.size, 0)
    );
    drop(backend);
    let db = rusqlite::Connection::open(dir.path().join("meta.db")).unwrap();
    db.execute_batch("DROP TRIGGER objects_usage_insert; DROP TRIGGER objects_usage_update; DROP TRIGGER objects_usage_delete; DROP TABLE object_usage;").unwrap();
    drop(db);
    let backend = LocalBackend::open(dir.path()).unwrap();
    assert_eq!(backend.stats().await.unwrap().cached_bytes, object.size);
    assert!(
        !backend
            .discard_unconfirmed(workspace, &object)
            .await
            .unwrap()
    );
    assert!(backend.evict(workspace, &object).await.unwrap());
    assert_eq!(backend.stats().await.unwrap().cached_bytes, 0);
    assert!(backend.volume_stats().await.unwrap().total_bytes > 0);
}

#[tokio::test]
async fn receipt_guards_reject_stale_updates_atomically() {
    let directory = tempfile::tempdir().unwrap();
    let backend = LocalBackend::open(directory.path()).unwrap();
    let initial = operation("concurrent-receipt");
    backend.reserve_operation(initial.clone()).await.unwrap();
    let mut confirmed = initial.clone();
    confirmed.local_saved = true;
    confirmed.remote_confirmed = true;
    backend
        .commit(LocalCommit {
            operations: vec![confirmed.clone()],
            ..Default::default()
        })
        .await
        .unwrap();
    let unrelated = operation("batch-receipt");
    let error = backend
        .commit(LocalCommit {
            operation_guards: vec![initial.clone()],
            operations: vec![unrelated.clone(), initial],
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::TargetMoved);
    assert!(backend.operation(unrelated.id).await.unwrap().is_none());
    assert_eq!(
        backend.operation(confirmed.id).await.unwrap(),
        Some(confirmed.clone())
    );
    let mut complete = confirmed.clone();
    complete.phase = OperationPhase::Complete;
    backend
        .commit(LocalCommit {
            operation_guards: vec![confirmed],
            operations: vec![complete.clone()],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        backend.operation(complete.id).await.unwrap(),
        Some(complete)
    );
}
