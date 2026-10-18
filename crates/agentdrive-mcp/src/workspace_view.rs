//! Dashboard presentation derived from the workspace and revision API responses.
use agentdrive_model::*;
use agentdrive_ports::WorkspaceStatus;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Serialize)]
pub(crate) struct WorkspaceView {
    pub status: WorkspaceStatus,
    pub revisions: Vec<Revision>,
    pub summary: WorkspaceSummary,
    pub origins: Vec<BranchOrigin>,
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
        let summary = WorkspaceSummary::new(&status.branches, &status.mounts);
        let by_id = revisions
            .iter()
            .map(|revision| (revision.id, revision))
            .collect();
        let origins = status
            .branches
            .iter()
            .map(|branch| BranchOrigin::new(branch, status.workspace.initial_revision, &by_id))
            .collect();
        Self {
            status,
            revisions,
            summary,
            origins,
        }
    }
}

impl WorkspaceSummary {
    fn new(branches: &[Branch], mounts: &[MountBinding]) -> Self {
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
        branch: &Branch,
        initial: RevisionId,
        revisions: &BTreeMap<RevisionId, &Revision>,
    ) -> Self {
        let revision = revisions.get(&branch.base_revision);
        Self {
            branch: branch.id,
            source_branch: branch.source_branch,
            revision: branch.base_revision,
            revision_available: revision.is_some(),
            workspace_root: branch.source_branch.is_none()
                && branch.base_revision == initial
                && revision.is_some_and(|revision| revision.branch == Some(branch.id)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            WorkspaceSummary::new(&[first, second, deleted], &mounts),
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
        assert!(BranchOrigin::new(&source, initial, &revisions).workspace_root);
        let fork = Branch {
            id: BranchId::new(),
            source_branch: Some(source.id),
            ..source.clone()
        };
        let origin = BranchOrigin::new(&fork, initial, &revisions);
        assert_eq!(origin.revision, initial);
        assert_eq!(origin.source_branch, Some(source.id));
        assert!(origin.revision_available);
        assert!(!origin.workspace_root);
        let unavailable = BranchOrigin::new(&fork, initial, &BTreeMap::new());
        assert_eq!(unavailable.revision, initial);
        assert_eq!(unavailable.source_branch, Some(source.id));
        assert!(!unavailable.revision_available);
        assert!(!unavailable.workspace_root);
    }
}
