use agentdrive_model::*;

fn branch() -> Branch {
    let revision = RevisionId::new();
    Branch {
        id: BranchId::new(),
        workspace: WorkspaceId::new(),
        name: "main".into(),
        owner: LocationId::new(),
        authority_epoch: 2,
        generation: 3,
        mutation_seq: 8,
        saved_seq: 7,
        source_generation: 4,
        formal_head: revision,
        autosave_head: None,
        working_root: object_ref(ObjectKind::Tree, b"local"),
        base_revision: revision,
        source_branch: None,
        next_inode: 2,
        dirty: true,
        state: BranchState::Writable,
        protected: false,
    }
}

fn reference(branch: &Branch) -> BranchRef {
    BranchRef {
        workspace: branch.workspace,
        branch: branch.id,
        owner: branch.owner,
        authority_epoch: branch.authority_epoch,
        generation: branch.generation,
        source_generation: branch.source_generation,
        durable_seq: branch.saved_seq,
        formal_head: branch.formal_head,
        autosave_head: branch.autosave_head,
        working_root: branch.working_root.clone(),
        record_index: object_ref(ObjectKind::RecordIndex, b"history"),
        deleted: false,
        write_stopped: false,
        metadata: Some(branch.into()),
    }
}

#[test]
fn current_owner_keeps_unpublished_content_until_authority_changes() {
    let local = branch();
    let mut remote = reference(&local);
    assert!(!remote.supersedes_local(&local, local.owner));
    remote.source_generation += 1;
    assert!(!remote.supersedes_local(&local, local.owner));
    remote.authority_epoch += 1;
    remote.owner = LocationId::new();
    assert!(remote.supersedes_local(&local, local.owner));
}

#[test]
fn former_owner_uses_current_references_and_rejects_older_observations() {
    let mut local = branch();
    local.state = BranchState::Stopped;
    let location = LocationId::new();
    let current = reference(&local);
    assert!(current.supersedes_local(&local, location));
    let mut older = current.clone();
    older.authority_epoch -= 1;
    older.generation += 100;
    assert!(!older.supersedes_local(&local, location));
    older = current.clone();
    older.generation -= 1;
    older.source_generation += 100;
    assert!(!older.supersedes_local(&local, location));
    older = current.clone();
    older.source_generation -= 1;
    older.durable_seq += 100;
    assert!(!older.supersedes_local(&local, location));
    older = current.clone();
    older.durable_seq -= 1;
    assert!(!older.supersedes_local(&local, location));
    let mut newer = current;
    newer.durable_seq += 1;
    assert!(newer.supersedes_local(&local, location));
}

#[test]
fn confirmed_deletion_overrides_current_content_without_resurrecting_local_deletion() {
    let mut local = branch();
    let mut remote = reference(&local);
    remote.deleted = true;
    remote.source_generation += 1;
    assert!(remote.supersedes_local(&local, local.owner));
    remote.authority_epoch -= 1;
    assert!(!remote.supersedes_local(&local, local.owner));
    local.state = BranchState::Deleted;
    let remote = reference(&local);
    assert!(!remote.supersedes_local(&local, LocationId::new()));
}
