use crate::{Runtime, TreeService};
use agentdrive_model::*;
use agentdrive_ports::*;
use async_trait::async_trait;
use std::sync::Arc;

pub struct SnapshotService {
    runtime: Arc<Runtime>,
    trees: Arc<TreeService>,
    working: Arc<dyn WorkingTreeStore>,
}

impl std::fmt::Debug for SnapshotService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SnapshotService").finish_non_exhaustive()
    }
}

impl SnapshotService {
    pub fn new(
        runtime: Arc<Runtime>,
        trees: Arc<TreeService>,
        working: Arc<dyn WorkingTreeStore>,
    ) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            trees,
            working,
        })
    }

    /// Startup runs before native mounts or command transports are exposed.
    pub async fn recover(&self) -> Result<()> {
        for workspace in self.runtime.state.workspaces().await? {
            for branch in self.runtime.state.branches(workspace.id).await? {
                let _gate = self.runtime.lock_branch(branch.id).await;
                let tree = self.trees.tree(workspace.id, &branch.working_root).await?;
                let nodes = tree
                    .into_iter()
                    .map(|(path, inode)| Node {
                        path,
                        inode,
                        dirty: false,
                    })
                    .collect();
                let mut recovered = branch.clone();
                recovered.mutation_seq = recovered.saved_seq;
                let formal = self
                    .trees
                    .revision(workspace.id, branch.formal_head)
                    .await?;
                recovered.dirty = formal.root != recovered.working_root;
                let version = self
                    .trees
                    .revision(
                        workspace.id,
                        branch.autosave_head.unwrap_or(branch.formal_head),
                    )
                    .await?;
                if recovered.working_root != version.root {
                    self.runtime.mark_dirty(&recovered);
                }
                self.runtime
                    .state
                    .commit(LocalCommit {
                        guards: vec![BranchGuard::from(&branch)],
                        branches: vec![recovered],
                        node_changes: vec![NodeChange::Replace {
                            branch: branch.id,
                            nodes,
                        }],
                        ..Default::default()
                    })
                    .await?;
                self.working.discard_branch(workspace.id, branch.id).await?;
            }
            let mut commit = LocalCommit::default();
            for mut operation in self.runtime.state.operations(workspace.id).await? {
                if matches!(
                    operation.phase,
                    OperationPhase::Reserved | OperationPhase::Saving
                ) && !operation.local_saved
                {
                    operation.phase =
                        if matches!(operation.pending, Some(PendingAction::Transfer { .. })) {
                            OperationPhase::RecoveryRequired
                        } else {
                            OperationPhase::Failed
                        };
                    operation.error = Some(Error::new(
                        ErrorCode::RecoveryRequired,
                        "operation was interrupted before its durable local commit",
                    ));
                    commit.operations.push(operation);
                }
            }
            self.runtime.state.commit(commit).await?;
        }
        let mut commit = LocalCommit::default();
        for mut mount in self.runtime.state.mounts().await? {
            if mount.state != MountState::Released {
                mount.state = MountState::RecoveryRequired;
                commit.mounts.push(mount);
            }
        }
        self.runtime.state.commit(commit).await
    }
}

#[async_trait]
impl SnapshotCapture for SnapshotService {
    async fn capture(&self, branch: &Branch) -> Result<CapturedTree> {
        let deltas = self.runtime.state.node_deltas(branch.id).await?;
        let previous: std::collections::BTreeMap<_, _> = deltas
            .iter()
            .filter_map(|delta| delta.before.as_ref())
            .map(|node| (node.path.lookup_key(), node))
            .collect();
        let mut directories = std::collections::BTreeMap::new();
        let mut inodes = std::collections::BTreeMap::new();
        let mut nodes = Vec::new();
        // Remove old keys before inserting new keys, including case-only renames.
        for delta in &deltas {
            if let Some(node) = &delta.before {
                inodes.insert(crate::trees::inode_key(node.inode.id), None);
                if let Some(parent) = node.path.parent() {
                    let parent = if let Some(parent) = previous.get(&parent.lookup_key()) {
                        (*parent).clone()
                    } else {
                        self.runtime
                            .state
                            .node_at(branch.id, &parent)
                            .await?
                            .ok_or_else(|| Error::integrity("checkpoint parent is missing"))?
                    };
                    let entry = DirectoryEntry {
                        parent: parent.inode.id,
                        name: node.path.name().to_owned(),
                        inode: node.inode.id,
                    };
                    directories.insert(crate::trees::directory_key(&entry)?, None);
                }
            }
        }
        for delta in &deltas {
            if let Some(mut node) = delta.after.clone() {
                if node.dirty && node.inode.kind == FileKind::File {
                    let object = self
                        .trees
                        .content
                        .local
                        .seal(WorkingFile {
                            workspace: branch.workspace,
                            branch: branch.id,
                            inode: node.inode.id,
                        })
                        .await?;
                    if object.size != node.inode.size {
                        return Err(Error::integrity(
                            "working file size differs from committed metadata",
                        ));
                    }
                    node.inode.content = Some(object);
                }
                node.dirty = false;
                inodes.insert(
                    crate::trees::inode_key(node.inode.id),
                    Some(node.inode.clone()),
                );
                if let Some(parent) = node.path.parent() {
                    let parent = self
                        .runtime
                        .state
                        .node_at(branch.id, &parent)
                        .await?
                        .ok_or_else(|| Error::integrity("checkpoint parent is missing"))?;
                    let entry = DirectoryEntry {
                        parent: parent.inode.id,
                        name: node.path.name().to_owned(),
                        inode: node.inode.id,
                    };
                    directories.insert(crate::trees::directory_key(&entry)?, Some(entry));
                }
                nodes.push(node);
            }
        }
        let root = if deltas.is_empty() {
            branch.working_root.clone()
        } else {
            let old: TreeRoot = decode(
                &self
                    .trees
                    .content
                    .metadata(branch.workspace, &branch.working_root)
                    .await?,
            )?;
            let directories = self
                .trees
                .update_index(branch.workspace, &old.directories, directories)
                .await?;
            let inodes = self
                .trees
                .update_index(branch.workspace, &old.inodes, inodes)
                .await?;
            let data = encode(&TreeRoot {
                format_version: FORMAT_VERSION,
                directories,
                inodes,
            })?;
            if object_ref(ObjectKind::Tree, &data) == branch.working_root {
                branch.working_root.clone()
            } else {
                self.trees
                    .content
                    .local
                    .put(branch.workspace, ObjectKind::Tree, data.into())
                    .await?
            }
        };
        Ok(CapturedTree {
            branch: branch.clone(),
            root,
            nodes,
            captured_seq: branch.mutation_seq,
        })
    }
}
