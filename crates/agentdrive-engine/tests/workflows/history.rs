use super::*;

#[tokio::test]
async fn revision_history_pages_are_ordered_bounded_and_query_scoped() {
    let h = Harness::new(None).await;
    let workspace = h.workspace().await;
    let initial = h
        .local
        .revision(workspace.initial_revision)
        .await
        .unwrap()
        .unwrap();
    let branch = initial.branch.unwrap();
    let mut records = vec![initial.clone()];
    records.extend((0..253).map(|index| Revision {
        id: RevisionId::new(),
        message: Some(format!("version {index}")),
        created_ns: initial.created_ns + 100 + index / 3,
        ..initial.clone()
    }));
    h.local
        .commit(LocalCommit {
            revisions: records[1..].to_vec(),
            ..Default::default()
        })
        .await
        .unwrap();
    let query = RevisionHistoryQuery {
        workspace: workspace.id,
        branch,
        search: None,
        limit: Some(37),
        cursor: None,
    };
    let mut ids = Vec::new();
    let mut request = query.clone();
    loop {
        let page = h
            .engine
            .services
            .revisions
            .history(&actor(), request.clone())
            .await
            .unwrap();
        assert_eq!(page.total, records.len());
        assert!(page.entries.len() <= 37);
        ids.extend(page.entries.iter().map(|revision| revision.id));
        let Some(cursor) = page.next else { break };
        request.cursor = Some(cursor);
    }
    records.sort_by(|a, b| b.created_ns.cmp(&a.created_ns).then(a.id.cmp(&b.id)));
    assert_eq!(
        ids,
        records
            .iter()
            .map(|revision| revision.id)
            .collect::<Vec<_>>()
    );
    let first = h
        .engine
        .services
        .revisions
        .history(&actor(), query.clone())
        .await
        .unwrap();
    let (other, _) = h.branch_mount(workspace.id, "other", None).await;
    for invalid in [
        RevisionHistoryQuery {
            search: Some("another search".into()),
            cursor: first.next.clone(),
            ..query.clone()
        },
        RevisionHistoryQuery {
            branch: other.branch,
            cursor: first.next.clone(),
            ..query.clone()
        },
        RevisionHistoryQuery {
            limit: Some(201),
            ..query.clone()
        },
        RevisionHistoryQuery {
            cursor: Some("invalid".into()),
            ..query.clone()
        },
    ] {
        assert_eq!(
            h.engine
                .services
                .revisions
                .history(&actor(), invalid)
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidArgument
        );
    }
    let search = h
        .engine
        .services
        .revisions
        .history(
            &actor(),
            RevisionHistoryQuery {
                search: Some("version 252".into()),
                ..query.clone()
            },
        )
        .await
        .unwrap();
    assert_eq!(search.total, 1);
    assert_eq!(search.entries[0].message.as_deref(), Some("version 252"));
    let visitor: Principal = "visitor".to_owned().try_into().unwrap();
    assert_eq!(
        h.engine
            .services
            .revisions
            .history(&visitor, query)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    let described = h
        .engine
        .services
        .revisions
        .metadata(&actor(), workspace.id, vec![initial.id, RevisionId::new()])
        .await
        .unwrap();
    assert_eq!(described, vec![initial]);
    let connection = rusqlite::Connection::open(h.directory.path().join("meta.db")).unwrap();
    let mut statement = connection.prepare("EXPLAIN QUERY PLAN SELECT data FROM revisions WHERE workspace=?1 AND json_extract(CAST(data AS TEXT),'$.branch')=?2 ORDER BY json_extract(CAST(data AS TEXT),'$.created_ns') DESC,id ASC LIMIT 50").unwrap();
    let plan = statement
        .query_map(
            rusqlite::params![workspace.id.to_string(), branch.to_string()],
            |row| row.get::<_, String>(3),
        )
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
        .join(" ");
    assert!(plan.contains("revisions_branch_history"), "{plan}");
    assert!(!plan.contains("TEMP B-TREE"), "{plan}");
}

#[tokio::test]
async fn synchronization_requires_saved_content_and_matching_remote_revision() {
    let h = Harness::new(Some(Arc::new(MemoryRemote::default()))).await;
    let workspace = h.workspace().await;
    let (mount, fs) = h.branch_mount(workspace.id, "editing", None).await;
    h.sync(workspace.id, SyncDirection::Push).await;
    let status = h
        .engine
        .services
        .workspace
        .status(&actor(), workspace.id)
        .await
        .unwrap();
    assert_eq!(
        status.synchronization.state,
        SynchronizationState::Confirmed
    );
    h.write_new(&fs, "new.txt", b"work in progress").await;
    let status = h
        .engine
        .services
        .workspace
        .status(&actor(), workspace.id)
        .await
        .unwrap();
    assert_eq!(status.pending_jobs, 0);
    assert_eq!(status.synchronization.state, SynchronizationState::Unsaved);
    success(
        h.engine
            .services
            .mounts
            .save(
                context(),
                SaveMount {
                    mount: mount.id,
                    expected_binding_generation: mount.generation,
                    durability: Durability::Local,
                    wait_for_remote_ms: None,
                },
            )
            .await
            .unwrap(),
    );
    let status = h
        .engine
        .services
        .workspace
        .status(&actor(), workspace.id)
        .await
        .unwrap();
    assert_eq!(status.synchronization.state, SynchronizationState::Pending);
    h.sync(workspace.id, SyncDirection::Push).await;
    let status = h
        .engine
        .services
        .workspace
        .status(&actor(), workspace.id)
        .await
        .unwrap();
    assert_eq!(
        status.synchronization.state,
        SynchronizationState::Confirmed
    );
    let reference = status
        .remote_branches
        .iter()
        .find(|reference| reference.branch == mount.branch)
        .unwrap();
    for changed in [
        BranchRef {
            owner: LocationId::new(),
            ..reference.clone()
        },
        BranchRef {
            authority_epoch: reference.authority_epoch + 1,
            ..reference.clone()
        },
        BranchRef {
            generation: reference.generation + 1,
            ..reference.clone()
        },
        BranchRef {
            durable_seq: reference.durable_seq + 1,
            ..reference.clone()
        },
        BranchRef {
            formal_head: RevisionId::new(),
            ..reference.clone()
        },
        BranchRef {
            working_root: object_ref(ObjectKind::Tree, b"different content"),
            ..reference.clone()
        },
    ] {
        let mut references = status.remote_branches.clone();
        *references
            .iter_mut()
            .find(|value| value.branch == mount.branch)
            .unwrap() = changed;
        assert_eq!(
            WorkspaceSynchronization::new(status.location, &status.branches, &references, true, 0)
                .state,
            SynchronizationState::Unconfirmed
        );
    }
}
