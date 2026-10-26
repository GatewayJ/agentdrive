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
async fn replicated_receipts_preserve_local_request_identity_across_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let backend = LocalBackend::open(directory.path()).unwrap();
    let workspace = WorkspaceId::new();
    let mut first = operation("save");
    first.workspace = Some(workspace);
    let mut second = first.clone();
    second.id = OperationId::new();
    second.fingerprint = object_ref(ObjectKind::RecordIndex, b"second location").id;
    backend
        .commit(LocalCommit {
            replicated_operations: vec![first.clone(), second.clone()],
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        backend
            .operation_by_request(&first.principal, "save")
            .await
            .unwrap()
            .is_none()
    );
    let mut local = operation("save");
    local.workspace = Some(workspace);
    assert!(
        backend
            .reserve_operation(local.clone())
            .await
            .unwrap()
            .created
    );
    backend
        .commit(LocalCommit {
            replicated_operations: vec![first.clone(), second.clone(), local.clone()],
            ..Default::default()
        })
        .await
        .unwrap();
    drop(backend);
    let backend = LocalBackend::open(directory.path()).unwrap();
    for record in [&first, &second, &local] {
        assert_eq!(
            backend.operation(record.id).await.unwrap().as_ref(),
            Some(record)
        );
    }
    assert_eq!(backend.operations(workspace).await.unwrap().len(), 3);
    assert_eq!(
        backend
            .operation_by_request(&local.principal, "save")
            .await
            .unwrap(),
        Some(local.clone())
    );
    let replay = backend.reserve_operation(operation("save")).await.unwrap();
    assert!(!replay.created);
    assert_eq!(replay.record.id, local.id);
    let mut conflict = first.clone();
    conflict.fingerprint = object_ref(ObjectKind::RecordIndex, b"changed identity").id;
    assert_eq!(
        backend
            .commit(LocalCommit {
                replicated_operations: vec![conflict],
                ..Default::default()
            })
            .await
            .unwrap_err()
            .code,
        ErrorCode::RequestIdConflict
    );
}

#[tokio::test]
async fn existing_databases_add_replication_receipts_without_changing_local_requests() {
    let directory = tempfile::tempdir().unwrap();
    let backend = LocalBackend::open(directory.path()).unwrap();
    let local = operation("existing");
    backend.reserve_operation(local.clone()).await.unwrap();
    let location = backend.location().await.unwrap();
    drop(backend);
    let database = rusqlite::Connection::open(directory.path().join("meta.db")).unwrap();
    database
        .execute_batch("DROP TABLE replicated_operations")
        .unwrap();
    drop(database);
    let backend = LocalBackend::open(directory.path()).unwrap();
    assert_eq!(backend.location().await.unwrap(), location);
    assert_eq!(
        backend
            .operation_by_request(&local.principal, "existing")
            .await
            .unwrap(),
        Some(local)
    );
    let mut imported = operation("existing");
    imported.workspace = Some(WorkspaceId::new());
    backend
        .commit(LocalCommit {
            replicated_operations: vec![imported.clone()],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        backend.operation(imported.id).await.unwrap(),
        Some(imported)
    );
}

#[tokio::test]
async fn legacy_imports_release_request_ids_and_preserve_local_retries() {
    for has_replication_table in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let backend = LocalBackend::open(directory.path()).unwrap();
        let location = backend.location().await.unwrap();
        let workspace = WorkspaceId::new();
        let mut imported = operation("save-result");
        imported.workspace = Some(workspace);
        imported.branch = Some(BranchId::new());
        imported.name = "revision_commit".into();
        imported.local_saved = true;
        imported.remote_confirmed = has_replication_table;
        imported.phase = OperationPhase::LocalSaved;

        let root = backend
            .put(workspace, ObjectKind::Tree, Bytes::from_static(b"tree"))
            .await
            .unwrap();
        let revision = RevisionId::new();
        let branch = Branch {
            id: BranchId::new(),
            workspace,
            name: "transferred".into(),
            owner: LocationId::new(),
            authority_epoch: 2,
            generation: 2,
            mutation_seq: 0,
            saved_seq: 0,
            source_generation: 2,
            formal_head: revision,
            autosave_head: None,
            working_root: root,
            base_revision: revision,
            source_branch: None,
            next_inode: 2,
            dirty: false,
            state: BranchState::Stopped,
            protected: false,
        };
        let mut local = operation("local-save");
        local.workspace = Some(workspace);
        local.branch = Some(branch.id);
        local.local_saved = true;
        local.phase = OperationPhase::Complete;
        // A failed local request can refer to a branch that was never created here.
        let mut failed = operation("failed-switch");
        failed.workspace = Some(workspace);
        failed.branch = imported.branch;
        failed.phase = OperationPhase::Failed;
        let mut standalone = operation("workspace-sync");
        standalone.workspace = Some(workspace);
        standalone.local_saved = true;
        standalone.phase = OperationPhase::Complete;
        let retained = [local, failed, standalone, operation("reserved")];
        backend
            .commit(LocalCommit {
                new_branches: vec![branch],
                operations: retained.iter().cloned().chain([imported.clone()]).collect(),
                ..Default::default()
            })
            .await
            .unwrap();
        drop(backend);
        let database = rusqlite::Connection::open(directory.path().join("meta.db")).unwrap();
        database
            .execute(
                "DELETE FROM metadata WHERE key='replicated_operations_migrated'",
                [],
            )
            .unwrap();
        if !has_replication_table {
            database
                .execute_batch("DROP TABLE replicated_operations")
                .unwrap();
        }
        drop(database);

        let backend = LocalBackend::open(directory.path()).unwrap();
        assert_eq!(backend.location().await.unwrap(), location);
        assert_eq!(
            backend.operation(imported.id).await.unwrap(),
            Some(imported.clone())
        );
        assert!(
            backend
                .operation_by_request(&imported.principal, &imported.request_id)
                .await
                .unwrap()
                .is_none()
        );
        for record in &retained {
            let retry = backend.reserve_operation(record.clone()).await.unwrap();
            assert!(!retry.created);
            assert_eq!(&retry.record, record);
            let mut conflict = record.clone();
            conflict.fingerprint = object_ref(ObjectKind::RecordIndex, b"changed request").id;
            assert_eq!(
                backend.reserve_operation(conflict).await.unwrap_err().code,
                ErrorCode::RequestIdConflict
            );
        }
        let mut local = operation("save-result");
        local.workspace = Some(workspace);
        local.fingerprint = object_ref(ObjectKind::RecordIndex, b"new local request").id;
        assert!(
            backend
                .reserve_operation(local.clone())
                .await
                .unwrap()
                .created
        );
        backend
            .commit(LocalCommit {
                replicated_operations: vec![imported.clone()],
                ..Default::default()
            })
            .await
            .unwrap();
        drop(backend);

        let backend = LocalBackend::open(directory.path()).unwrap();
        assert_eq!(
            backend.operation(imported.id).await.unwrap(),
            Some(imported)
        );
        let retry = backend.reserve_operation(local.clone()).await.unwrap();
        assert!(!retry.created);
        assert_eq!(retry.record, local);
        assert_eq!(backend.operations(workspace).await.unwrap().len(), 5);
    }
}

#[tokio::test]
async fn failed_receipt_migration_rolls_back_and_can_retry() {
    let directory = tempfile::tempdir().unwrap();
    let backend = LocalBackend::open(directory.path()).unwrap();
    let mut imported = operation("imported");
    imported.workspace = Some(WorkspaceId::new());
    imported.branch = Some(BranchId::new());
    imported.local_saved = true;
    backend
        .commit(LocalCommit {
            operations: vec![imported.clone()],
            ..Default::default()
        })
        .await
        .unwrap();
    drop(backend);
    let database = rusqlite::Connection::open(directory.path().join("meta.db")).unwrap();
    database
        .execute_batch(
            "DELETE FROM metadata WHERE key='replicated_operations_migrated';
         DROP TABLE replicated_operations;
         CREATE TRIGGER fail_migration BEFORE DELETE ON operations
         BEGIN SELECT RAISE(ABORT, 'injected migration failure'); END;",
        )
        .unwrap();
    drop(database);
    assert_eq!(
        LocalBackend::open(directory.path()).unwrap_err().code,
        ErrorCode::Conflict
    );
    let database = rusqlite::Connection::open(directory.path().join("meta.db")).unwrap();
    let data: Vec<u8> = database
        .query_row(
            "SELECT data FROM operations WHERE id=?1",
            [imported.id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(decode::<OperationRecord>(&data).unwrap(), imported);
    for query in [
        "SELECT COUNT(*) FROM sqlite_master WHERE name='replicated_operations'",
        "SELECT COUNT(*) FROM metadata WHERE key='replicated_operations_migrated'",
    ] {
        assert_eq!(
            database
                .query_row(query, [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    database
        .execute_batch("DROP TRIGGER fail_migration")
        .unwrap();
    drop(database);
    let backend = LocalBackend::open(directory.path()).unwrap();
    assert_eq!(
        backend.operation(imported.id).await.unwrap(),
        Some(imported.clone())
    );
    assert!(
        backend
            .operation_by_request(&imported.principal, &imported.request_id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn object_usage_survives_duplicates_reopen_and_removal() {
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

#[tokio::test]
async fn object_batch_publishes_after_finish_and_survives_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let backend = LocalBackend::open(directory.path()).unwrap();
    let workspace = WorkspaceId::new();
    let batch = backend.begin_batch(workspace).await.unwrap();
    let mut references = Vec::new();
    for value in [b"first".as_slice(), b"second", b""] {
        let reference = batch
            .put(workspace, ObjectKind::File, Bytes::copy_from_slice(value))
            .await
            .unwrap();
        assert!(!backend.contains(workspace, &reference).await.unwrap());
        references.push((reference, value.to_vec()));
    }
    assert!(backend.objects(workspace).await.unwrap().is_empty());
    batch.finish().await.unwrap();
    assert_eq!(backend.objects(workspace).await.unwrap().len(), 3);
    drop(backend);
    let backend = LocalBackend::open(directory.path()).unwrap();
    for (reference, expected) in references {
        let mut stream = backend.open(workspace, &reference).await.unwrap();
        let mut actual = Vec::new();
        stream.reader.read_to_end(&mut actual).await.unwrap();
        assert_eq!(actual, expected);
    }
}

#[tokio::test]
async fn abandoned_batches_remove_staging_and_reject_other_workspaces() {
    let directory = tempfile::tempdir().unwrap();
    let backend = LocalBackend::open(directory.path()).unwrap();
    let workspace = WorkspaceId::new();
    let batch = backend.begin_batch(workspace).await.unwrap();
    let reference = batch
        .put(
            workspace,
            ObjectKind::File,
            Bytes::from_static(b"abandoned"),
        )
        .await
        .unwrap();
    assert_eq!(
        batch
            .put(WorkspaceId::new(), ObjectKind::File, Bytes::new())
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    drop(batch);
    assert_eq!(
        std::fs::read_dir(directory.path().join("staging"))
            .unwrap()
            .count(),
        0
    );
    assert!(!backend.contains(workspace, &reference).await.unwrap());
    assert!(backend.objects(workspace).await.unwrap().is_empty());
}

#[tokio::test]
async fn batch_reclamation_respects_readers_confirmation_and_empty_files() {
    let directory = tempfile::tempdir().unwrap();
    let backend = LocalBackend::open(directory.path()).unwrap();
    let workspace = WorkspaceId::new();
    let held = backend
        .put(workspace, ObjectKind::File, Bytes::from_static(b"held"))
        .await
        .unwrap();
    let changed = backend
        .put(
            workspace,
            ObjectKind::File,
            Bytes::from_static(b"confirmed later"),
        )
        .await
        .unwrap();
    let removed = backend
        .put(workspace, ObjectKind::File, Bytes::from_static(b"unused"))
        .await
        .unwrap();
    let empty = backend
        .put(workspace, ObjectKind::File, Bytes::new())
        .await
        .unwrap();
    let candidates = backend.objects(workspace).await.unwrap();
    let reader = backend.open(workspace, &held).await.unwrap();
    backend.confirm_remote(workspace, &changed).await.unwrap();
    assert_eq!(
        backend.remove_batch(candidates).await.unwrap(),
        removed.size
    );
    assert!(backend.contains(workspace, &held).await.unwrap());
    assert!(backend.contains(workspace, &changed).await.unwrap());
    assert!(!backend.contains(workspace, &removed).await.unwrap());
    assert!(!backend.contains(workspace, &empty).await.unwrap());
    drop(reader);
    assert!(backend.discard_unconfirmed(workspace, &held).await.unwrap());
    let stats = backend.stats().await.unwrap();
    assert_eq!(
        (stats.cached_bytes, stats.protected_bytes),
        (changed.size, 0)
    );
}
