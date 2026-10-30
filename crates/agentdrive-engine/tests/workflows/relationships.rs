use super::*;

async fn query(
    harness: &Harness,
    workspace: WorkspaceId,
    filter: RelationshipFilter,
) -> WorkspaceRelationships {
    harness
        .engine
        .services
        .workspace
        .relationships(&actor(), WorkspaceRelationshipsQuery { workspace, filter })
        .await
        .unwrap()
}

#[tokio::test]
async fn relationships_keep_creation_roots_and_filter_mount_identities() {
    let harness = Harness::new(None).await;
    let workspace = harness.workspace().await;
    let (source, fs) = harness.branch_mount(workspace.id, "source", None).await;
    harness
        .write_new(&fs, "pending.txt", b"pending source")
        .await;
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
                    source_branch: Some(source.branch),
                    name: "child".into(),
                },
            )
            .await
            .unwrap(),
    );
    let child = match created.result.unwrap() {
        CommandResult::Branch { branch } => branch,
        _ => unreachable!(),
    };
    let mount = mounted(
        harness
            .engine
            .services
            .mounts
            .prepare(
                context(),
                PrepareMount {
                    branch: Some(child.id),
                    ..default_mount(workspace.id, harness.directory.path().join("child"))
                },
            )
            .await
            .unwrap(),
    );
    harness
        .write_new(
            &FsContext {
                mount: mount.id,
                generation: mount.generation,
                identity: identity(),
            },
            "later.txt",
            b"child only",
        )
        .await;
    harness.commit(child.id).await;
    let result = query(
        &harness,
        workspace.id,
        RelationshipFilter {
            branch: Some(child.id),
            mount: Some(mount.id),
            owner_location: Some(harness.engine.runtime.location),
            mount_location: Some(harness.engine.runtime.location),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(result.total, 1);
    let entry = &result.entries[0];
    assert_eq!(entry.mount.as_ref(), Some(&mount));
    assert_eq!(entry.mount_location, Some(harness.engine.runtime.location));
    assert_eq!(entry.branch.origin.source_branch, Some(source.branch));
    assert_eq!(entry.branch.origin.source_name.as_deref(), Some("source"));
    assert_eq!(entry.branch.origin.base_revision, Some(child.base_revision));
    assert_eq!(
        entry.branch.origin.captured_working_root,
        Some(child.working_root.clone())
    );
    assert_eq!(entry.branch.origin.status, OriginStatus::Available);
    assert_ne!(
        harness
            .local
            .branch(child.id)
            .await
            .unwrap()
            .unwrap()
            .working_root,
        child.working_root
    );
    assert_ne!(result.selected_branch, Some(child.id));
    assert!(!result.remote_mounts_known && !result.remote_selections_known);
    let remote_mounts = query(
        &harness,
        workspace.id,
        RelationshipFilter {
            mount_location: Some(LocationId::new()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(remote_mounts.total, 0);
    assert!(!remote_mounts.remote_mounts_known);
}

#[tokio::test]
async fn relationships_enforce_branch_and_ancestry_permissions_before_counts() {
    let harness = Harness::new(None).await;
    let mut workspace = harness.workspace().await;
    let (private, _) = harness.branch_mount(workspace.id, "private", None).await;
    let revision = harness
        .local
        .branch(private.branch)
        .await
        .unwrap()
        .unwrap()
        .formal_head;
    // Create from the private branch so its identity is retained in the creation receipt.
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
                    source_branch: Some(private.branch),
                    name: "visible".into(),
                },
            )
            .await
            .unwrap(),
    );
    let visible = mounted(
        harness
            .engine
            .services
            .mounts
            .prepare(
                context(),
                PrepareMount {
                    branch: created.branch,
                    ..default_mount(workspace.id, harness.directory.path().join("visible"))
                },
            )
            .await
            .unwrap(),
    );
    let reader: Principal = "reader".to_owned().try_into().unwrap();
    workspace.grants.push(Grant {
        principal: reader.clone(),
        branch: Some(visible.branch),
        permissions: vec![Permission::Read],
    });
    harness
        .local
        .commit(LocalCommit {
            workspaces: vec![workspace.clone()],
            ..Default::default()
        })
        .await
        .unwrap();
    let result = harness
        .engine
        .services
        .workspace
        .relationships(
            &reader,
            WorkspaceRelationshipsQuery {
                workspace: workspace.id,
                filter: RelationshipFilter::default(),
            },
        )
        .await
        .unwrap();
    assert_eq!(result.total, 1);
    assert_eq!(result.selected_branch, None);
    assert_eq!(result.entries[0].branch.id, visible.branch);
    assert_eq!(
        result.entries[0].branch.origin.status,
        OriginStatus::Restricted
    );
    let encoded = String::from_utf8(encode(&result).unwrap()).unwrap();
    assert!(!encoded.contains(&private.branch.to_string()));
    assert!(!encoded.contains(&private.id.to_string()));
    assert!(!encoded.contains(&revision.to_string()));
    assert!(!encoded.contains(private.path.to_str().unwrap()));
    let forbidden = harness
        .engine
        .services
        .workspace
        .relationships(
            &reader,
            WorkspaceRelationshipsQuery {
                workspace: workspace.id,
                filter: RelationshipFilter {
                    branch: Some(private.branch),
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap_err();
    assert_eq!(forbidden.code, ErrorCode::PermissionDenied);
    let hidden = harness
        .engine
        .services
        .workspace
        .relationships(
            &reader,
            WorkspaceRelationshipsQuery {
                workspace: workspace.id,
                filter: RelationshipFilter {
                    mount: Some(private.id),
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(hidden.total, 0);
    let stranger: Principal = "stranger".to_owned().try_into().unwrap();
    assert_eq!(
        harness
            .engine
            .services
            .workspace
            .relationships(
                &stranger,
                WorkspaceRelationshipsQuery {
                    workspace: workspace.id,
                    filter: RelationshipFilter::default(),
                }
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
}

#[tokio::test]
async fn relationships_protect_unscoped_origins_and_preserve_unavailable_metadata() {
    let harness = Harness::new(None).await;
    let mut workspace = harness.workspace().await;
    let candidate = Revision {
        id: RevisionId::new(),
        branch: None,
        kind: RevisionKind::Candidate,
        ..harness
            .local
            .revision(workspace.initial_revision)
            .await
            .unwrap()
            .unwrap()
    };
    harness
        .local
        .commit(LocalCommit {
            revisions: vec![candidate.clone()],
            ..Default::default()
        })
        .await
        .unwrap();
    let (branch, _) = harness
        .branch_mount(workspace.id, "from-candidate", Some(candidate.id))
        .await;
    let mount = mounted(
        harness
            .engine
            .services
            .mounts
            .prepare(
                context(),
                PrepareMount {
                    branch: Some(branch.branch),
                    revision: Some(candidate.id),
                    access: AccessMode::ReadOnly,
                    ..default_mount(workspace.id, harness.directory.path().join("candidate"))
                },
            )
            .await
            .unwrap(),
    );
    let reader: Principal = "reader".to_owned().try_into().unwrap();
    workspace.grants.push(Grant {
        principal: reader.clone(),
        branch: Some(branch.branch),
        permissions: vec![Permission::Read],
    });
    harness
        .local
        .commit(LocalCommit {
            workspaces: vec![workspace.clone()],
            ..Default::default()
        })
        .await
        .unwrap();
    let filter = RelationshipFilter {
        mount: Some(mount.id),
        ..Default::default()
    };
    for available in [true, false] {
        if !available {
            let database =
                rusqlite::Connection::open(harness.directory.path().join("meta.db")).unwrap();
            database
                .execute(
                    "DELETE FROM revisions WHERE id=?1",
                    [candidate.id.to_string()],
                )
                .unwrap();
        }
        let owner_view = query(&harness, workspace.id, filter.clone()).await;
        let origin = &owner_view.entries[0].branch.origin;
        assert_eq!(origin.base_revision, Some(candidate.id));
        assert_eq!(origin.captured_working_root.as_ref(), Some(&candidate.root));
        assert_eq!(
            origin.status,
            if available {
                OriginStatus::Available
            } else {
                OriginStatus::Unavailable
            }
        );
        let restricted = harness
            .engine
            .services
            .workspace
            .relationships(
                &reader,
                WorkspaceRelationshipsQuery {
                    workspace: workspace.id,
                    filter: filter.clone(),
                },
            )
            .await
            .unwrap();
        assert_eq!(restricted.total, 1);
        let entry = &restricted.entries[0];
        assert_eq!(entry.mount.as_ref().unwrap().revision, None);
        assert_eq!(entry.branch.origin.status, OriginStatus::Restricted);
        assert_eq!(entry.branch.origin.base_revision, None);
        assert_eq!(entry.branch.origin.captured_working_root, None);
    }
}

#[tokio::test]
async fn relationships_paginate_deterministically_and_reject_changed_views() {
    let harness = Harness::new(None).await;
    let workspace = harness.workspace().await;
    let (mount, _) = harness.branch_mount(workspace.id, "second", None).await;
    harness.branch_mount(workspace.id, "third", None).await;
    let all = query(&harness, workspace.id, RelationshipFilter::default()).await;
    let mut filter = RelationshipFilter {
        limit: Some(1),
        ..Default::default()
    };
    let first = query(&harness, workspace.id, filter.clone()).await;
    let mut collected = vec![first.entries[0].branch.id];
    filter.cursor = first.next.clone();
    loop {
        let page = query(&harness, workspace.id, filter.clone()).await;
        assert_eq!(page.total, all.total);
        collected.push(page.entries[0].branch.id);
        filter.cursor = page.next;
        if filter.cursor.is_none() {
            break;
        }
    }
    assert_eq!(
        collected,
        all.entries
            .iter()
            .map(|entry| entry.branch.id)
            .collect::<Vec<_>>()
    );
    let mut changed = mount;
    changed.state = MountState::Paused;
    harness
        .local
        .commit(LocalCommit {
            mounts: vec![changed],
            ..Default::default()
        })
        .await
        .unwrap();
    filter.cursor = first.next;
    let error = harness
        .engine
        .services
        .workspace
        .relationships(
            &actor(),
            WorkspaceRelationshipsQuery {
                workspace: workspace.id,
                filter,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::TargetMoved);
    assert!(error.retryable);
    for limit in [0, 201] {
        assert_eq!(
            harness
                .engine
                .services
                .workspace
                .relationships(
                    &actor(),
                    WorkspaceRelationshipsQuery {
                        workspace: workspace.id,
                        filter: RelationshipFilter {
                            limit: Some(limit),
                            ..Default::default()
                        },
                    }
                )
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidArgument
        );
    }
}

#[tokio::test]
async fn relationships_distinguish_mount_states_and_retained_released_bindings() {
    let harness = Harness::new(None).await;
    let workspace = harness.workspace().await;
    let (mount, _) = harness.branch_mount(workspace.id, "mounted", None).await;
    let mut bindings = Vec::new();
    for (index, state) in [
        MountState::Preparing,
        MountState::Paused,
        MountState::Released,
        MountState::RecoveryRequired,
    ]
    .into_iter()
    .enumerate()
    {
        bindings.push(MountBinding {
            id: MountId::new(),
            path: harness.directory.path().join(format!("state-{index}")),
            access: AccessMode::ReadOnly,
            state,
            ..mount.clone()
        });
    }
    harness
        .local
        .commit(LocalCommit {
            mounts: bindings.clone(),
            ..Default::default()
        })
        .await
        .unwrap();
    let current = query(
        &harness,
        workspace.id,
        RelationshipFilter {
            branch: Some(mount.branch),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(current.total, 4);
    assert!(
        current
            .entries
            .iter()
            .all(|entry| entry.mount.as_ref().unwrap().state != MountState::Released)
    );
    for binding in bindings.iter().chain([&mount]) {
        let page = query(
            &harness,
            workspace.id,
            RelationshipFilter {
                branch: Some(mount.branch),
                mount_state: Some(binding.state),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(page.total, 1);
        assert_eq!(page.entries[0].mount.as_ref(), Some(binding));
    }
    let released = bindings
        .iter()
        .find(|binding| binding.state == MountState::Released)
        .unwrap();
    let by_id = query(
        &harness,
        workspace.id,
        RelationshipFilter {
            mount: Some(released.id),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(by_id.entries[0].mount.as_ref(), Some(released));
    let released_id = released.id;
    let Harness {
        directory,
        local,
        engine,
        clock,
        driver,
    } = harness;
    drop((local, engine, clock, driver));
    let (_, reopened, _, _) = open(directory.path(), None).await;
    let after = reopened
        .services
        .workspace
        .relationships(
            &actor(),
            WorkspaceRelationshipsQuery {
                workspace: workspace.id,
                filter: RelationshipFilter {
                    mount: Some(released_id),
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(
        after.entries[0].mount.as_ref().unwrap().state,
        MountState::Released
    );
    assert_eq!(after.selected_branch, current.selected_branch);
}

#[tokio::test]
async fn relationships_track_remote_observations_and_ownership_transfer() {
    let remote = Arc::new(MemoryRemote::default());
    let first = Harness::new(Some(remote.clone())).await;
    let second = Harness::new(Some(remote.clone())).await;
    let workspace = first.workspace().await;
    let (mount, _) = first.branch_mount(workspace.id, "shared", None).await;
    let original = first.local.branch(mount.branch).await.unwrap().unwrap();
    first.sync(workspace.id, SyncDirection::Push).await;
    second.clock.advance(1234);
    second.sync(workspace.id, SyncDirection::Pull).await;
    let filter = RelationshipFilter {
        branch: Some(mount.branch),
        ..Default::default()
    };
    let observed = query(&second, workspace.id, filter.clone()).await;
    let entry = &observed.entries[0];
    assert_eq!(
        entry.branch.ownership.location,
        first.engine.runtime.location
    );
    assert_eq!(
        entry.branch.ownership.source,
        OwnershipSource::RemoteReference
    );
    assert!(entry.branch.ownership.storage_version.is_some());
    assert_eq!(
        entry.branch.ownership.observed_ns,
        Some(second.clock.now_ns())
    );
    assert_eq!(entry.branch.name.as_deref(), Some("shared"));
    assert_eq!(
        entry.branch.origin.captured_working_root,
        Some(original.working_root)
    );
    assert!(entry.mount.is_none());
    assert!(!observed.remote_mounts_known && !observed.remote_selections_known);
    let reference = second
        .local
        .remote_refs(workspace.id)
        .await
        .unwrap()
        .into_iter()
        .find(|reference| reference.branch == mount.branch)
        .unwrap();
    let database = rusqlite::Connection::open(second.directory.path().join("meta.db")).unwrap();
    database
        .execute(
            "UPDATE remote_refs SET data=?2 WHERE id=?1",
            rusqlite::params![mount.branch.to_string(), encode(&reference).unwrap(),],
        )
        .unwrap();
    drop(database);
    let legacy = query(&second, workspace.id, filter.clone()).await;
    assert_eq!(legacy.entries[0].branch.ownership.storage_version, None);
    assert_eq!(legacy.entries[0].branch.ownership.observed_ns, None);
    success(
        first
            .engine
            .services
            .mounts
            .unmount(context(), mount.id, mount.generation)
            .await
            .unwrap(),
    );
    let branch = first.local.branch(mount.branch).await.unwrap().unwrap();
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
    let transferred = query(&first, workspace.id, filter.clone()).await;
    assert_eq!(
        transferred.entries[0].branch.ownership.location,
        second.engine.runtime.location
    );
    assert_eq!(
        transferred.entries[0].branch.ownership.source,
        OwnershipSource::RemoteReference
    );
    second.sync(workspace.id, SyncDirection::Pull).await;
    let adopted = query(&second, workspace.id, filter).await;
    assert_eq!(
        adopted.entries[0].branch.ownership.source,
        OwnershipSource::LocalAuthority
    );
    assert_eq!(
        adopted.entries[0].branch.ownership.authority_epoch,
        branch.authority_epoch + 1
    );
    let Harness {
        directory,
        local,
        engine,
        clock,
        driver,
    } = first;
    drop((local, engine, clock, driver));
    let (_, reopened, _, _) = open(directory.path(), Some(remote)).await;
    let after = reopened
        .services
        .workspace
        .relationships(
            &actor(),
            WorkspaceRelationshipsQuery {
                workspace: workspace.id,
                filter: RelationshipFilter {
                    branch: Some(mount.branch),
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(
        after.entries[0].branch.ownership.storage_version,
        transferred.entries[0].branch.ownership.storage_version
    );
    assert_eq!(
        after.entries[0].branch.ownership.observed_ns,
        transferred.entries[0].branch.ownership.observed_ns
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relationships_read_one_transaction_during_binding_changes() {
    let harness = Harness::new(None).await;
    let workspace = harness.workspace().await;
    let (mount, _) = harness.branch_mount(workspace.id, "changing", None).await;
    let branch = harness.local.branch(mount.branch).await.unwrap().unwrap();
    let writer = harness.local.clone();
    let branch_id = branch.id;
    let task = tokio::spawn(async move {
        for generation in 2..60 {
            writer
                .commit(LocalCommit {
                    branches: vec![Branch {
                        generation,
                        ..branch.clone()
                    }],
                    mounts: vec![MountBinding {
                        generation,
                        ..mount.clone()
                    }],
                    selections: vec![WorkspaceSelection {
                        workspace: workspace.id,
                        branch: branch.id,
                    }],
                    ..Default::default()
                })
                .await
                .unwrap();
            tokio::task::yield_now().await;
        }
    });
    for _ in 0..60 {
        let result = query(
            &harness,
            workspace.id,
            RelationshipFilter {
                branch: Some(branch_id),
                ..Default::default()
            },
        )
        .await;
        let entry = &result.entries[0];
        assert_eq!(
            entry.branch.ownership.generation,
            entry.mount.as_ref().unwrap().generation
        );
        if entry.branch.ownership.generation > 1 {
            assert_eq!(result.selected_branch, Some(branch_id));
        }
    }
    task.await.unwrap();
}
