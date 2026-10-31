//! Dashboard presentation derived from the workspace and revision API responses.
use agentdrive_model::*;
use agentdrive_ports::WorkspaceStatus;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Serialize)]
pub(crate) struct WorkspaceView {
    pub status: WorkspaceStatus,
    pub branches: Vec<WorkspaceBranch>,
    pub revisions: Vec<Revision>,
    pub summary: WorkspaceSummary,
    pub origins: Vec<BranchOrigin>,
    pub remote_error: Option<Error>,
}

#[derive(Serialize)]
pub(crate) struct WorkspaceBranch {
    pub id: BranchId,
    pub name: String,
    pub owner: LocationId,
    pub local: bool,
    pub state: BranchState,
    pub formal_head: RevisionId,
    pub autosave_head: Option<RevisionId>,
    pub working_root: ObjectRef,
    pub base_revision: Option<RevisionId>,
    pub source_branch: Option<BranchId>,
    pub authority_epoch: u64,
    pub generation: u64,
    pub mutation_seq: u64,
    pub saved_seq: u64,
    pub dirty: bool,
}

impl WorkspaceBranch {
    fn local(branch: &Branch) -> Self {
        Self {
            id: branch.id,
            name: branch.name.clone(),
            owner: branch.owner,
            local: true,
            state: branch.state,
            formal_head: branch.formal_head,
            autosave_head: branch.autosave_head,
            working_root: branch.working_root.clone(),
            base_revision: Some(branch.base_revision),
            source_branch: branch.source_branch,
            authority_epoch: branch.authority_epoch,
            generation: branch.generation,
            mutation_seq: branch.mutation_seq,
            saved_seq: branch.saved_seq,
            dirty: branch.dirty,
        }
    }

    fn remote(reference: &BranchRef, revisions: &BTreeMap<RevisionId, &Revision>) -> Self {
        Self {
            id: reference.branch,
            name: reference
                .metadata
                .as_ref()
                .map(|metadata| metadata.name.clone())
                .unwrap_or_else(|| format!("分支 {}", reference.branch)),
            owner: reference.owner,
            local: false,
            state: if reference.deleted {
                BranchState::Deleted
            } else if reference.write_stopped {
                BranchState::Stopped
            } else {
                BranchState::Writable
            },
            formal_head: reference.formal_head,
            autosave_head: reference.autosave_head,
            working_root: reference.working_root.clone(),
            base_revision: reference
                .metadata
                .as_ref()
                .map(|metadata| metadata.base_revision),
            source_branch: reference
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.source_branch),
            authority_epoch: reference.authority_epoch,
            generation: reference.generation,
            mutation_seq: reference.durable_seq,
            saved_seq: reference.durable_seq,
            dirty: revisions
                .get(&reference.formal_head)
                .is_some_and(|revision| revision.root != reference.working_root),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub(crate) struct WorkspaceSummary {
    pub branch_count: usize,
    pub mount_count: usize,
    pub ready_mount_count: usize,
    pub mounted_branch_count: usize,
}

#[derive(Serialize)]
pub(crate) struct BranchOrigin {
    pub branch: BranchId,
    pub source_branch: Option<BranchId>,
    pub revision: RevisionId,
    pub revision_available: bool,
    pub workspace_root: bool,
}

impl WorkspaceView {
    pub fn new(mut status: WorkspaceStatus, revisions: Vec<Revision>) -> Self {
        // Released bindings are history, not current mount inventory.
        status
            .mounts
            .retain(|mount| mount.state != MountState::Released);
        status
            .mounts
            .sort_by(|a, b| a.path.cmp(&b.path).then(a.id.cmp(&b.id)));
        let by_id = revisions
            .iter()
            .map(|revision| (revision.id, revision))
            .collect();
        let mut branches: BTreeMap<_, _> = status
            .remote_branches
            .iter()
            .map(|reference| (reference.branch, WorkspaceBranch::remote(reference, &by_id)))
            .collect();
        for branch in &status.branches {
            // 本机尚未发布的修改使用当前工作状态。
            if !branches
                .get(&branch.id)
                .is_some_and(|remote| remote.authority_epoch > branch.authority_epoch)
            {
                branches.insert(branch.id, WorkspaceBranch::local(branch));
            }
        }
        let branches: Vec<_> = branches.into_values().collect();
        let summary = WorkspaceSummary::new(&branches, &status.mounts);
        let origins = branches
            .iter()
            .filter_map(|branch| {
                BranchOrigin::new(branch, status.workspace.initial_revision, &by_id)
            })
            .collect();
        Self {
            status,
            branches,
            revisions,
            summary,
            origins,
            remote_error: None,
        }
    }
}

impl WorkspaceSummary {
    fn new(branches: &[WorkspaceBranch], mounts: &[MountBinding]) -> Self {
        let ready: Vec<_> = mounts
            .iter()
            .filter(|mount| mount.state == MountState::Ready)
            .collect();
        Self {
            branch_count: branches
                .iter()
                .filter(|branch| branch.state != BranchState::Deleted)
                .count(),
            mount_count: mounts
                .iter()
                .filter(|mount| mount.state != MountState::Released)
                .count(),
            ready_mount_count: ready.len(),
            mounted_branch_count: ready
                .iter()
                .map(|mount| mount.branch)
                .collect::<BTreeSet<_>>()
                .len(),
        }
    }
}

impl BranchOrigin {
    fn new(
        branch: &WorkspaceBranch,
        initial: RevisionId,
        revisions: &BTreeMap<RevisionId, &Revision>,
    ) -> Option<Self> {
        let base_revision = branch.base_revision?;
        let revision = revisions.get(&base_revision);
        Some(Self {
            branch: branch.id,
            source_branch: branch.source_branch,
            revision: base_revision,
            revision_available: revision.is_some(),
            workspace_root: branch.source_branch.is_none()
                && base_revision == initial
                && revision.is_some_and(|revision| revision.branch == Some(branch.id)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            deleted: branch.state == BranchState::Deleted,
            write_stopped: branch.state == BranchState::Stopped,
            metadata: Some(branch.into()),
        }
    }

    fn status(
        root: &Branch,
        branches: Vec<Branch>,
        remote_branches: Vec<BranchRef>,
    ) -> WorkspaceStatus {
        WorkspaceStatus {
            location: branches[0].owner,
            selected_branch: Some(branches[0].id),
            branches,
            remote_branches,
            workspace: Workspace {
                id: root.workspace,
                name: "project".into(),
                owner: "owner".to_owned().try_into().unwrap(),
                initial_revision: root.base_revision,
                format_version: FORMAT_VERSION,
                name_policy: "portable_v1".into(),
                grants: vec![],
                max_file_bytes: 1024,
                max_working_bytes: 4096,
                max_inodes: 100,
            },
            mounts: vec![],
            pending_jobs: 0,
            capabilities: agentdrive_ports::Capabilities {
                format_version: FORMAT_VERSION,
                platform_mount: "test".into(),
                file_operations: vec![],
                remote_configured: true,
                automatic_text_merge: false,
                physical_remote_gc: false,
            },
            storage: StorageStats {
                total_bytes: 4096,
                available_bytes: 4096,
                cached_bytes: 0,
                protected_bytes: 0,
            },
        }
    }

    #[test]
    fn workspace_view_combines_locations_and_keeps_local_unsaved_work() {
        let root = branch();
        let a = Branch {
            id: BranchId::new(),
            name: "feat/a".into(),
            source_branch: Some(root.id),
            formal_head: RevisionId::new(),
            ..root.clone()
        };
        let b = Branch {
            id: BranchId::new(),
            name: "feat/b".into(),
            owner: LocationId::new(),
            source_branch: Some(a.id),
            base_revision: a.formal_head,
            mutation_seq: 9,
            saved_seq: 3,
            dirty: true,
            ..a.clone()
        };
        let initial = Revision {
            id: root.base_revision,
            workspace: root.workspace,
            branch: Some(root.id),
            parents: vec![],
            root: root.working_root.clone(),
            kind: RevisionKind::Commit,
            message: None,
            actor: "owner".to_owned().try_into().unwrap(),
            created_ns: 1,
            operation: OperationId::new(),
        };
        let a_revision = Revision {
            id: a.formal_head,
            branch: Some(a.id),
            parents: vec![initial.id],
            ..initial.clone()
        };
        let view = WorkspaceView::new(
            status(
                &root,
                vec![b.clone()],
                vec![reference(&root), reference(&a), reference(&b)],
            ),
            vec![initial, a_revision],
        );
        assert_eq!(view.summary.branch_count, 3);
        assert_eq!(view.status.branches, vec![b.clone()]);
        let remote = view.branches.iter().find(|value| value.id == a.id).unwrap();
        assert!(!remote.local);
        assert_eq!(remote.name, "feat/a");
        let local = view.branches.iter().find(|value| value.id == b.id).unwrap();
        assert!(local.local && local.dirty);
        assert_eq!((local.mutation_seq, local.saved_seq), (9, 3));
        let origin = view
            .origins
            .iter()
            .find(|origin| origin.branch == b.id)
            .unwrap();
        assert_eq!(
            (origin.source_branch, origin.revision),
            (Some(a.id), a.formal_head)
        );
        assert!(origin.revision_available);
        assert!(
            view.origins
                .iter()
                .find(|origin| origin.branch == root.id)
                .unwrap()
                .workspace_root
        );
    }

    #[test]
    fn missing_remote_metadata_and_newer_ownership_are_displayed_without_invented_origins() {
        let root = branch();
        let mut remote = reference(&root);
        remote.metadata = None;
        remote.authority_epoch += 1;
        remote.owner = LocationId::new();
        let view = WorkspaceView::new(
            status(&root, vec![root.clone()], vec![remote.clone()]),
            vec![],
        );
        assert_eq!(view.branches.len(), 1);
        assert_eq!(view.branches[0].owner, remote.owner);
        assert!(!view.branches[0].local);
        assert!(view.origins.is_empty());
        assert_eq!(view.status.branches, vec![root]);
    }

    fn branch() -> Branch {
        let revision = RevisionId::new();
        Branch {
            id: BranchId::new(),
            workspace: WorkspaceId::new(),
            name: "main".into(),
            owner: LocationId::new(),
            authority_epoch: 1,
            generation: 1,
            mutation_seq: 0,
            saved_seq: 0,
            source_generation: 1,
            formal_head: revision,
            autosave_head: None,
            working_root: object_ref(ObjectKind::Tree, b"tree"),
            base_revision: revision,
            source_branch: None,
            next_inode: 2,
            dirty: false,
            state: BranchState::Writable,
            protected: false,
        }
    }

    fn mount(branch: &Branch, state: MountState, access: AccessMode) -> MountBinding {
        MountBinding {
            id: MountId::new(),
            workspace: branch.workspace,
            branch: branch.id,
            revision: None,
            generation: 1,
            path: "/workspace".into(),
            principal: "owner".to_owned().try_into().unwrap(),
            identity: LocalIdentity {
                uid: 1000,
                gid: 1000,
            },
            durability: Durability::Local,
            access,
            state,
        }
    }

    #[test]
    fn counts_distinguish_ready_branches_from_retained_mount_records() {
        let first = branch();
        let second = Branch {
            id: BranchId::new(),
            ..first.clone()
        };
        let deleted = Branch {
            id: BranchId::new(),
            state: BranchState::Deleted,
            ..first.clone()
        };
        let mounts = vec![
            mount(&first, MountState::Ready, AccessMode::ReadWrite),
            mount(&first, MountState::Ready, AccessMode::ReadOnly),
            mount(&second, MountState::RecoveryRequired, AccessMode::ReadWrite),
            mount(&second, MountState::Preparing, AccessMode::ReadOnly),
            mount(&second, MountState::Paused, AccessMode::ReadOnly),
            mount(&second, MountState::Released, AccessMode::ReadOnly),
        ];
        assert_eq!(
            WorkspaceSummary::new(
                &[first, second, deleted]
                    .iter()
                    .map(WorkspaceBranch::local)
                    .collect::<Vec<_>>(),
                &mounts
            ),
            WorkspaceSummary {
                branch_count: 2,
                mount_count: 5,
                ready_mount_count: 2,
                mounted_branch_count: 1,
            }
        );
        assert_eq!(
            WorkspaceSummary::new(&[], &[]),
            WorkspaceSummary {
                branch_count: 0,
                mount_count: 0,
                ready_mount_count: 0,
                mounted_branch_count: 0,
            }
        );
    }

    #[test]
    fn origin_keeps_the_exact_creation_revision_when_heads_advance_or_sources_disappear() {
        let mut source = branch();
        let initial = source.base_revision;
        let revision = Revision {
            id: initial,
            workspace: source.workspace,
            branch: Some(source.id),
            parents: vec![],
            root: source.working_root.clone(),
            kind: RevisionKind::Autosave,
            message: None,
            actor: "owner".to_owned().try_into().unwrap(),
            created_ns: 1,
            operation: OperationId::new(),
        };
        let revisions = BTreeMap::from([(initial, &revision)]);
        source.formal_head = RevisionId::new();
        assert!(
            BranchOrigin::new(&WorkspaceBranch::local(&source), initial, &revisions)
                .unwrap()
                .workspace_root
        );
        let fork = Branch {
            id: BranchId::new(),
            source_branch: Some(source.id),
            ..source.clone()
        };
        let origin =
            BranchOrigin::new(&WorkspaceBranch::local(&fork), initial, &revisions).unwrap();
        assert_eq!(origin.revision, initial);
        assert_eq!(origin.source_branch, Some(source.id));
        assert!(origin.revision_available);
        assert!(!origin.workspace_root);
        let unavailable =
            BranchOrigin::new(&WorkspaceBranch::local(&fork), initial, &BTreeMap::new()).unwrap();
        assert_eq!(unavailable.revision, initial);
        assert_eq!(unavailable.source_branch, Some(source.id));
        assert!(!unavailable.revision_available);
        assert!(!unavailable.workspace_root);
    }
}
