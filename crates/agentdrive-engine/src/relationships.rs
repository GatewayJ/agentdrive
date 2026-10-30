use agentdrive_model::*;
use agentdrive_ports::*;
use std::collections::{BTreeMap, BTreeSet};

/// All authorization and pagination use the same local observation.
pub(crate) fn query(
    snapshot: RelationshipSnapshot,
    actor: &Principal,
    filter: RelationshipFilter,
) -> Result<WorkspaceRelationships> {
    let limit = filter.limit.unwrap_or(50);
    if !(1..=200).contains(&limit) {
        return Err(Error::invalid(
            "relationship limit must be between 1 and 200",
        ));
    }
    let search = filter.search.as_deref().unwrap_or("").trim().to_lowercase();
    if search.len() > 1024 {
        return Err(Error::invalid("relationship search exceeds 1024 bytes"));
    }
    let can_read = |branch| {
        snapshot
            .workspace
            .authorize(actor, branch, Permission::Read)
            .is_ok()
    };
    if let Some(branch) = filter.branch {
        snapshot
            .workspace
            .authorize(actor, Some(branch), Permission::Read)?;
    }
    let local: BTreeMap<_, _> = snapshot
        .branches
        .iter()
        .map(|branch| (branch.id, branch))
        .collect();
    let remote: BTreeMap<_, _> = snapshot
        .remote_refs
        .iter()
        .map(|value| (value.reference.branch, value))
        .collect();
    let ids: BTreeSet<_> = local.keys().chain(remote.keys()).copied().collect();
    if !can_read(None) && !ids.iter().any(|id| can_read(Some(*id))) {
        return Err(Error::new(
            ErrorCode::PermissionDenied,
            "workspace has no readable relationships",
        ));
    }
    let revisions: BTreeMap<_, _> = snapshot
        .revisions
        .iter()
        .map(|revision| (revision.id, revision))
        .collect();
    let mut creations = snapshot.creations;
    creations.sort_by_key(|record| (record.created_ns, record.id));
    let mut created = BTreeMap::new();
    for record in &creations {
        if record.local_saved
            && let Some(CommandResult::Branch { branch }) = &record.result
            && branch.workspace == snapshot.workspace.id
        {
            created.entry(branch.id).or_insert(branch);
        }
    }
    let mut metadata: BTreeMap<_, _> = remote
        .iter()
        .filter_map(|(id, value)| {
            value
                .reference
                .metadata
                .clone()
                .map(|metadata| (*id, metadata))
        })
        .collect();
    if let Some(initial) = revisions.get(&snapshot.workspace.initial_revision)
        && let Some(id) = initial.branch
    {
        metadata.entry(id).or_insert_with(|| BranchMetadata {
            name: "main".into(),
            base_revision: initial.id,
            source_branch: None,
        });
    }
    metadata.extend(
        created
            .iter()
            .chain(local.iter())
            .map(|(id, value)| (*id, BranchMetadata::from(*value))),
    );
    let mut mounts: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for mount in &snapshot.mounts {
        if mount.workspace == snapshot.workspace.id {
            mounts.entry(mount.branch).or_default().push(mount);
        }
    }
    let mut entries = Vec::new();
    for id in ids {
        if !can_read(Some(id)) || filter.branch.is_some_and(|branch| branch != id) {
            continue;
        }
        let (ownership, state) = ownership(
            local.get(&id).copied(),
            remote.get(&id).copied(),
            snapshot.location,
        )?;
        if filter
            .owner_location
            .is_some_and(|location| location != ownership.location)
            || filter
                .mount_location
                .is_some_and(|location| location != snapshot.location)
        {
            continue;
        }
        let branch = RelationshipBranch {
            id,
            name: metadata.get(&id).map(|branch| branch.name.clone()),
            ownership,
            state,
            origin: origin(
                id,
                created.get(&id).copied(),
                &metadata,
                &revisions,
                snapshot.workspace.initial_revision,
                &can_read,
            ),
        };
        let mut matching: Vec<_> = mounts
            .get(&id)
            .into_iter()
            .flatten()
            .copied()
            .filter(|mount| {
                filter.mount.is_none_or(|id| id == mount.id)
                    && filter.mount_state.map_or(
                        filter.include_released
                            || filter.mount.is_some()
                            || mount.state != MountState::Released,
                        |state| state == mount.state,
                    )
            })
            .collect();
        matching.sort_by_key(|mount| mount.id);
        if matching.is_empty() {
            if filter.mount.is_none()
                && filter.mount_state.is_none()
                && filter.mount_location.is_none()
            {
                entries.push(WorkspaceRelationship {
                    branch,
                    mount: None,
                    mount_location: None,
                });
            }
            continue;
        }
        for mount in matching {
            let mut mount = mount.clone();
            if mount.revision.is_some_and(|id| {
                !can_read(revisions.get(&id).and_then(|revision| revision.branch))
            }) {
                mount.revision = None;
            }
            entries.push(WorkspaceRelationship {
                branch: branch.clone(),
                mount: Some(mount),
                mount_location: Some(snapshot.location),
            });
        }
    }
    entries.retain(|entry| {
        let mounted = entry
            .mount
            .as_ref()
            .is_some_and(|mount| mount.state == MountState::Ready);
        if filter.mounted.is_some_and(|expected| expected != mounted) {
            return false;
        }
        if search.is_empty() {
            return true;
        }
        let mut values = vec![
            entry.branch.name.clone().unwrap_or_default(),
            entry.branch.id.to_string(),
            entry.branch.ownership.location.to_string(),
        ];
        if let Some(mount) = &entry.mount {
            values.extend([
                mount.id.to_string(),
                mount.path.to_string_lossy().into_owned(),
                mount.principal.as_str().to_owned(),
            ]);
        }
        values
            .iter()
            .any(|value| value.to_lowercase().contains(&search))
    });
    paginate(
        WorkspaceRelationships {
            workspace: snapshot.workspace.id,
            observing_location: snapshot.location,
            selected_branch: snapshot
                .selected_branch
                .filter(|branch| can_read(Some(*branch))),
            remote_mounts_known: false,
            remote_selections_known: false,
            total: entries.len(),
            entries,
            next: None,
        },
        actor,
        filter,
        limit,
    )
}

fn ownership(
    branch: Option<&Branch>,
    observation: Option<&ObservedBranchRef>,
    location: LocationId,
) -> Result<(RelationshipOwnership, BranchState)> {
    let use_remote = observation.is_some_and(|observation| {
        branch.is_none_or(|branch| {
            branch.owner != location
                || observation.reference.source_generation > branch.source_generation
        })
    });
    let result = if use_remote {
        let observation = observation.unwrap();
        let reference = &observation.reference;
        (
            RelationshipOwnership {
                location: reference.owner,
                authority_epoch: reference.authority_epoch,
                generation: reference.generation,
                source: OwnershipSource::RemoteReference,
                storage_version: observation.storage_version.clone(),
                observed_ns: observation.observed_ns,
            },
            if reference.deleted {
                BranchState::Deleted
            } else if reference.write_stopped {
                BranchState::Stopped
            } else {
                BranchState::Writable
            },
        )
    } else {
        let branch = branch.ok_or_else(|| Error::integrity("relationship has no branch record"))?;
        (
            RelationshipOwnership {
                location: branch.owner,
                authority_epoch: branch.authority_epoch,
                generation: branch.generation,
                source: if branch.owner == location {
                    OwnershipSource::LocalAuthority
                } else {
                    OwnershipSource::LocalRecord
                },
                storage_version: None,
                observed_ns: None,
            },
            branch.state,
        )
    };
    Ok(result)
}

fn paginate(
    mut result: WorkspaceRelationships,
    actor: &Principal,
    mut filter: RelationshipFilter,
    limit: usize,
) -> Result<WorkspaceRelationships> {
    let cursor = filter.cursor.take();
    // Bind continuation to the authorized view, filters and observing principal.
    let version = object_ref(
        ObjectKind::RecordIndex,
        &encode(&(actor, &filter, &result))?,
    )
    .id;
    let offset = match cursor {
        None => 0,
        Some(cursor) => {
            let (expected, offset) = cursor
                .split_once(':')
                .filter(|_| cursor.len() <= 96)
                .ok_or_else(|| Error::invalid("invalid relationship cursor"))?;
            let expected: ObjectId = expected.parse()?;
            let offset: usize = offset
                .parse()
                .map_err(|_| Error::invalid("invalid relationship cursor"))?;
            if expected != version {
                return Err(Error::new(
                    ErrorCode::TargetMoved,
                    "relationships changed; restart from the first page",
                )
                .retryable());
            }
            if offset == 0 || offset >= result.total {
                return Err(Error::invalid("relationship cursor is outside the result"));
            }
            offset
        }
    };
    let end = (offset + limit).min(result.total);
    result.next = (end < result.total).then(|| format!("{version}:{end}"));
    result.entries = result
        .entries
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect();
    Ok(result)
}

fn origin(
    id: BranchId,
    creation: Option<&Branch>,
    metadata: &BTreeMap<BranchId, BranchMetadata>,
    revisions: &BTreeMap<RevisionId, &Revision>,
    initial: RevisionId,
    can_read: &impl Fn(Option<BranchId>) -> bool,
) -> RelationshipOrigin {
    let creation_metadata = creation.map(BranchMetadata::from);
    let branch = creation_metadata.as_ref().or_else(|| metadata.get(&id));
    let initial = revisions
        .get(&initial)
        .filter(|revision| revision.branch == Some(id));
    let base_revision = branch
        .map(|branch| branch.base_revision)
        .or_else(|| initial.map(|revision| revision.id));
    let revision = base_revision.and_then(|id| revisions.get(&id).copied());
    let source_branch = branch.and_then(|branch| branch.source_branch).or_else(|| {
        revision
            .and_then(|revision| revision.branch)
            .filter(|source| *source != id)
    });
    if source_branch.is_some_and(|source| !can_read(Some(source)))
        || (base_revision.is_some()
            && !can_read(revision.map_or(source_branch, |revision| revision.branch)))
    {
        return RelationshipOrigin {
            workspace_root: false,
            source_branch: None,
            source_name: None,
            base_revision: None,
            captured_working_root: None,
            status: OriginStatus::Restricted,
        };
    }
    let source_name = source_branch
        .and_then(|source| metadata.get(&source))
        .map(|branch| branch.name.clone());
    let available = revision.is_some() && (source_branch.is_none() || source_name.is_some());
    RelationshipOrigin {
        workspace_root: initial.is_some() && initial.map(|revision| revision.id) == base_revision,
        source_branch,
        source_name,
        base_revision,
        captured_working_root: creation
            .map(|branch| branch.working_root.clone())
            .or_else(|| initial.map(|revision| revision.root.clone())),
        status: if available {
            OriginStatus::Available
        } else {
            OriginStatus::Unavailable
        },
    }
}
