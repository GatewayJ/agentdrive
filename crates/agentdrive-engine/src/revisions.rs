use crate::{
    OperationService, Runtime, TreeService,
    files::read_content,
    runtime::{check_guard, increment},
};
use agentdrive_model::*;
use agentdrive_ports::*;
use async_trait::async_trait;
use std::sync::Arc;

pub(crate) struct ForkSource {
    pub revision: RevisionId,
    pub working_root: Option<ObjectRef>,
}

pub struct RevisionService {
    pub(crate) runtime: Arc<Runtime>,
    pub(crate) trees: Arc<TreeService>,
    pub(crate) operations: Arc<OperationService>,
    pub(crate) publisher: Arc<dyn ReplicaPublisher>,
    snapshots: Arc<dyn SnapshotCapture>,
}

impl std::fmt::Debug for RevisionService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RevisionService").finish_non_exhaustive()
    }
}

impl RevisionService {
    pub(crate) async fn finish_bounded(
        self: &Arc<Self>,
        operation: OperationRecord,
        durability: Durability,
        wait_ms: u64,
    ) -> Result<OperationRecord> {
        if durability == Durability::Local {
            return self.operations.progress(operation).await;
        }
        let id = operation.id;
        let actor = operation.principal.clone();
        let service = self.clone();
        // The publication owns its cleanup even after the caller stops waiting.
        let task =
            tokio::spawn(async move { service.finish_durability(operation, durability).await });
        match tokio::time::timeout(std::time::Duration::from_millis(wait_ms), task).await {
            Ok(result) => {
                result.map_err(|error| Error::new(ErrorCode::Unavailable, error.to_string()))??;
                self.operations.get(&actor, id).await
            }
            Err(_) => self.operations.get(&actor, id).await,
        }
    }
    pub fn new(
        runtime: Arc<Runtime>,
        trees: Arc<TreeService>,
        operations: Arc<OperationService>,
        publisher: Arc<dyn ReplicaPublisher>,
        snapshots: Arc<dyn SnapshotCapture>,
    ) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            trees,
            operations,
            publisher,
            snapshots,
        })
    }

    pub(crate) async fn autosave(&self, id: BranchId, actor: Principal) -> Result<()> {
        let _gate = self.runtime.lock_branch(id).await;
        let _protection = self.runtime.objects.read().await;
        let branch = self.runtime.branch(id).await?;
        let guard = BranchGuard::from(&branch);
        let start = self
            .operations
            .start(
                RequestContext {
                    principal: actor,
                    request_id: format!("autosave-{}", OperationId::new()),
                },
                "autosave",
                &guard,
                Some(branch.workspace),
                Some(id),
            )
            .await?;
        let OperationStart::New(context) = start else {
            return Err(Error::integrity(
                "generated autosave operation already exists",
            ));
        };
        if let Err(error) = self
            .save_locked(
                context.clone(),
                SaveRevision {
                    message: None,
                    guard,
                    kind: RevisionKind::Autosave,
                    parents: None,
                    replace_root: None,
                    durability: Durability::Local,
                },
            )
            .await
        {
            self.operations.fail(&context, error.clone()).await?;
            return Err(error);
        }
        Ok(())
    }

    pub(crate) async fn finish_durability(
        &self,
        operation: OperationRecord,
        durability: Durability,
    ) -> Result<OperationRecord> {
        if durability == Durability::Remote {
            let workspace = operation
                .workspace
                .ok_or_else(|| Error::integrity("saved operation has no workspace"))?;
            if let Err(error) = self.publisher.drain(workspace, operation.branch).await {
                let mut records = self
                    .operations
                    .update(&[operation.id], LocalCommit::default(), |current| {
                        if !current.remote_confirmed {
                            current.error = Some(error.clone());
                        }
                    })
                    .await?;
                return Ok(records.remove(0));
            }
        }
        self.runtime
            .state
            .operation(operation.id)
            .await?
            .ok_or_else(|| Error::integrity("saved operation is missing"))
    }

    /// The branch lock and object retention lock are owned by the caller.
    pub(crate) async fn save_locked(
        &self,
        context: OperationContext,
        request: SaveRevision,
    ) -> Result<OperationRecord> {
        let mut branch = self.runtime.branch(request.guard.branch).await?;
        check_guard(&branch, &request.guard)?;
        let permission = if request
            .parents
            .as_ref()
            .is_some_and(|parents| parents.len() > 1)
        {
            Permission::Merge
        } else {
            Permission::Write
        };
        self.runtime
            .authorize(
                &context.operation.principal,
                branch.workspace,
                Some(branch.id),
                permission,
            )
            .await?;
        if branch.owner != self.runtime.location || branch.state != BranchState::Writable {
            return Err(Error::new(
                ErrorCode::StaleAuthority,
                "branch is not writable at this location",
            ));
        }
        if branch.protected && permission != Permission::Merge {
            return Err(Error::new(
                ErrorCode::ReadOnly,
                "protected branch requires a merge",
            ));
        }
        let replacing = request.replace_root.is_some();
        let captured = if let Some(root) = request.replace_root {
            let tree = self.trees.tree(branch.workspace, &root).await?;
            CapturedTree {
                branch: branch.clone(),
                root,
                captured_seq: increment(branch.mutation_seq)?,
                nodes: tree
                    .into_iter()
                    .map(|(path, inode)| Node {
                        path,
                        inode,
                        dirty: false,
                    })
                    .collect(),
            }
        } else {
            self.snapshots.capture(&branch).await?
        };
        if request.message.as_ref().is_some_and(|message| {
            message.len() > 4096 || message.chars().any(|character| character == '\0')
        }) {
            return Err(Error::invalid(
                "revision message exceeds 4096 bytes or contains NUL",
            ));
        }
        let formal = self
            .trees
            .revision(branch.workspace, branch.formal_head)
            .await?;
        let reuse = request.parents.is_none() && captured.root == formal.root;
        let autosave_root = if let Some(id) = branch.autosave_head {
            Some(self.trees.revision(branch.workspace, id).await?.root)
        } else {
            None
        };
        let reuse_autosave = request.parents.is_none()
            && request.kind != RevisionKind::Commit
            && autosave_root.as_ref() == Some(&captured.root);
        let revision_id = if reuse {
            branch.formal_head
        } else if reuse_autosave {
            branch
                .autosave_head
                .ok_or_else(|| Error::integrity("autosave reference is missing"))?
        } else {
            RevisionId::new()
        };
        let revision = if reuse || reuse_autosave {
            None
        } else {
            let parents = request
                .parents
                .unwrap_or_else(|| vec![branch.autosave_head.unwrap_or(branch.formal_head)]);
            if parents.is_empty() || parents.len() > 2 {
                return Err(Error::invalid("revision must have one or two parents"));
            }
            for parent in &parents {
                self.trees.revision(branch.workspace, *parent).await?;
            }
            Some(Revision {
                message: request.message.clone(),
                id: revision_id,
                workspace: branch.workspace,
                branch: Some(branch.id),
                parents,
                root: captured.root.clone(),
                kind: request.kind,
                actor: context.operation.principal.clone(),
                created_ns: self.runtime.clock.now_ns(),
                operation: context.operation.id,
            })
        };
        if replacing {
            branch.generation = increment(branch.generation)?;
        }
        branch.working_root = captured.root;
        branch.saved_seq = captured.captured_seq;
        branch.mutation_seq = captured.captured_seq;
        branch.source_generation = increment(branch.source_generation)?;
        branch.next_inode = branch.next_inode.max(increment(
            captured
                .nodes
                .iter()
                .map(|node| node.inode.id.0)
                .max()
                .unwrap_or(1),
        )?);
        if request.kind == RevisionKind::Commit {
            branch.formal_head = revision_id;
            branch.autosave_head = None;
            branch.dirty = false;
        } else {
            branch.autosave_head = if reuse { None } else { Some(revision_id) };
            branch.dirty = branch.working_root != formal.root;
        }
        let mut operation = context.operation;
        operation.workspace = Some(branch.workspace);
        operation.branch = Some(branch.id);
        operation.local_saved = true;
        operation.phase = if operation.pending.is_some() {
            OperationPhase::CommittedMountPending
        } else {
            OperationPhase::LocalSaved
        };
        operation.result = Some(CommandResult::Revision {
            revision: revision_id,
            branch: branch.id,
        });
        let mut merges = Vec::new();
        if let Some(PendingAction::ApplyRevision {
            merge: Some(id), ..
        }) = &operation.pending
        {
            let mut candidate = self
                .runtime
                .state
                .merge(*id)
                .await?
                .ok_or_else(|| Error::integrity("merge candidate is missing"))?;
            candidate.applied_revision = Some(revision_id);
            merges.push(candidate);
        }
        let job = self
            .publisher
            .build_job(&branch, revision.as_ref(), &operation)
            .await?;
        self.runtime
            .state
            .commit(LocalCommit {
                guards: vec![request.guard],
                branches: vec![branch.clone()],
                revisions: revision.into_iter().collect(),
                node_changes: if replacing {
                    vec![NodeChange::Replace {
                        branch: branch.id,
                        nodes: captured.nodes,
                    }]
                } else {
                    captured
                        .nodes
                        .into_iter()
                        .map(|node| NodeChange::Put {
                            branch: branch.id,
                            node,
                        })
                        .collect()
                },
                checkpointed_branches: vec![branch.id],
                operations: vec![operation.clone()],
                merges,
                sync_jobs: vec![job],
                ..Default::default()
            })
            .await?;
        self.runtime.clear_dirty(branch.id, branch.saved_seq);
        Ok(operation)
    }

    pub(crate) async fn preserve_working_locked(
        &self,
        branch: &Branch,
        actor: &Principal,
    ) -> Result<Branch> {
        if branch.mutation_seq == branch.saved_seq {
            let version = self
                .trees
                .revision(
                    branch.workspace,
                    branch.autosave_head.unwrap_or(branch.formal_head),
                )
                .await?;
            if branch.working_root == version.root {
                return Ok(branch.clone());
            }
        }
        let request = SaveRevision {
            message: None,
            guard: BranchGuard::from(branch),
            kind: RevisionKind::Autosave,
            parents: None,
            replace_root: None,
            durability: Durability::Local,
        };
        let start = self
            .operations
            .start(
                RequestContext {
                    principal: actor.clone(),
                    request_id: format!("checkpoint-{}", OperationId::new()),
                },
                "working_checkpoint",
                &request.guard,
                Some(branch.workspace),
                Some(branch.id),
            )
            .await?;
        let OperationStart::New(context) = start else {
            return Err(Error::integrity(
                "generated checkpoint operation already exists",
            ));
        };
        match self.save_locked(context.clone(), request).await {
            Ok(_) => self.runtime.branch(branch.id).await,
            Err(error) => {
                self.operations.fail(&context, error.clone()).await?;
                Err(error)
            }
        }
    }

    pub(crate) async fn fork_locked(
        &self,
        context: OperationContext,
        workspace: WorkspaceId,
        source: ForkSource,
        name: String,
        source_branch: Option<BranchId>,
        select: bool,
    ) -> Result<OperationRecord> {
        validate_label(&name, "branch name")?;
        let revision = self.trees.revision(workspace, source.revision).await?;
        let root = source.working_root.unwrap_or_else(|| revision.root.clone());
        let tree = self.trees.tree(workspace, &root).await?;
        let id = BranchId::new();
        let next_inode = increment(tree.values().map(|inode| inode.id.0).max().unwrap_or(1))?;
        let branch = Branch {
            id,
            workspace,
            name,
            owner: self.runtime.location,
            authority_epoch: 1,
            generation: 1,
            mutation_seq: 0,
            saved_seq: 0,
            source_generation: 1,
            formal_head: source.revision,
            autosave_head: None,
            working_root: root.clone(),
            base_revision: source.revision,
            source_branch,
            next_inode,
            dirty: root != revision.root,
            state: BranchState::Writable,
            protected: false,
        };
        let mut operation = context.operation;
        operation.workspace = Some(workspace);
        operation.branch = Some(id);
        operation.local_saved = true;
        operation.phase = if operation.pending.is_some() {
            OperationPhase::CommittedMountPending
        } else {
            OperationPhase::LocalSaved
        };
        operation.result = Some(CommandResult::Branch {
            branch: branch.clone(),
        });
        let job = self.publisher.build_job(&branch, None, &operation).await?;
        self.runtime
            .state
            .commit(LocalCommit {
                selections: if select {
                    vec![WorkspaceSelection {
                        workspace,
                        branch: id,
                    }]
                } else {
                    Vec::new()
                },
                new_branches: vec![branch.clone()],
                node_changes: vec![NodeChange::Replace {
                    branch: id,
                    nodes: tree
                        .into_iter()
                        .map(|(path, inode)| Node {
                            path,
                            inode,
                            dirty: false,
                        })
                        .collect(),
                }],
                operations: vec![operation.clone()],
                sync_jobs: vec![job],
                ..Default::default()
            })
            .await?;
        if branch.dirty {
            self.runtime.mark_dirty(&branch);
        }
        Ok(operation)
    }
}

pub(crate) fn validate_label(value: &str, field: &str) -> Result<()> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(Error::invalid(format!("invalid {field}")));
    }
    Ok(())
}

#[async_trait]
impl RevisionWriter for RevisionService {
    async fn save(
        &self,
        context: OperationContext,
        request: SaveRevision,
    ) -> Result<OperationRecord> {
        let durability = request.durability;
        let operation = {
            let _gate = self.runtime.lock_branch(request.guard.branch).await;
            let _protection = self.runtime.objects.read().await;
            self.save_locked(context, request).await?
        };
        self.finish_durability(operation, durability).await
    }
}

#[async_trait]
impl DurabilityService for RevisionService {
    async fn flush(
        &self,
        branch: BranchId,
        actor: Principal,
        durability: Durability,
    ) -> Result<DurabilityReceipt> {
        let (record, sequence) = {
            let _gate = self.runtime.lock_branch(branch).await;
            let _protection = self.runtime.objects.read().await;
            let mut branch = self.runtime.branch(branch).await?;
            branch.check_write(self.runtime.location, branch.authority_epoch)?;
            self.runtime
                .authorize(&actor, branch.workspace, Some(branch.id), Permission::Write)
                .await?;
            let guard = BranchGuard::from(&branch);
            let mut commit = LocalCommit::default();
            if branch.saved_seq != branch.mutation_seq {
                let captured = self.snapshots.capture(&branch).await?;
                branch.working_root = captured.root;
                branch.saved_seq = captured.captured_seq;
                branch.dirty = branch.working_root
                    != self
                        .trees
                        .revision(branch.workspace, branch.formal_head)
                        .await?
                        .root;
                commit.node_changes = captured
                    .nodes
                    .into_iter()
                    .map(|node| NodeChange::Put {
                        branch: branch.id,
                        node,
                    })
                    .collect();
                commit.checkpointed_branches.push(branch.id);
                commit.branches.push(branch.clone());
                commit.guards.push(guard);
            }
            let record = if durability == Durability::Remote {
                let start = self
                    .operations
                    .start(
                        RequestContext {
                            principal: actor,
                            request_id: format!("fsync-{}", OperationId::new()),
                        },
                        "fsync_checkpoint",
                        &guard,
                        Some(branch.workspace),
                        Some(branch.id),
                    )
                    .await?;
                let OperationStart::New(context) = start else {
                    return Err(Error::integrity(
                        "generated checkpoint operation already exists",
                    ));
                };
                let mut operation = context.operation;
                operation.local_saved = true;
                operation.phase = OperationPhase::LocalSaved;
                operation.result = Some(CommandResult::Updated);
                branch.source_generation = increment(branch.source_generation)?;
                commit.branches = vec![branch.clone()];
                commit.operations.push(operation.clone());
                commit
                    .sync_jobs
                    .push(self.publisher.build_job(&branch, None, &operation).await?);
                Some(operation)
            } else {
                None
            };
            if !commit.branches.is_empty() {
                self.runtime.state.commit(commit).await?;
            }
            (record, branch.saved_seq)
        };
        let operation = if let Some(record) = record {
            let operation = self.finish_durability(record, durability).await?;
            if !operation.remote_confirmed {
                return Err(operation.error.unwrap_or_else(|| {
                    Error::new(
                        ErrorCode::RemoteUnknown,
                        "remote durability is not confirmed",
                    )
                    .retryable()
                }));
            }
            Some(operation.id)
        } else {
            None
        };
        Ok(DurabilityReceipt {
            branch,
            mutation_seq: sequence,
            local_saved: true,
            remote_confirmed: durability == Durability::Remote,
            operation,
        })
    }
}

pub struct VersionService {
    revisions: Arc<RevisionService>,
    transitions: Arc<dyn RevisionTransition>,
    queries: Arc<dyn TreeQuery>,
}

impl std::fmt::Debug for VersionService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VersionService").finish_non_exhaustive()
    }
}

impl VersionService {
    pub fn new(
        revisions: Arc<RevisionService>,
        transitions: Arc<dyn RevisionTransition>,
        queries: Arc<dyn TreeQuery>,
    ) -> Arc<Self> {
        Arc::new(Self {
            revisions,
            transitions,
            queries,
        })
    }
}

#[async_trait]
impl RevisionApi for VersionService {
    async fn commit(
        &self,
        context: RequestContext,
        request: CommitRevision,
    ) -> Result<OperationRecord> {
        let branch = self.revisions.runtime.branch(request.guard.branch).await?;
        self.revisions
            .runtime
            .authorize(
                &context.principal,
                branch.workspace,
                Some(branch.id),
                Permission::Write,
            )
            .await?;
        let operation = match self
            .revisions
            .operations
            .start(
                context,
                "revision_commit",
                &request,
                Some(branch.workspace),
                Some(branch.id),
            )
            .await?
        {
            OperationStart::Existing(record) => return Ok(record),
            OperationStart::New(context) => context,
        };
        let save = SaveRevision {
            message: request.message,
            guard: request.guard,
            kind: request.kind.into(),
            parents: None,
            replace_root: None,
            durability: request.durability,
        };
        match self.revisions.save(operation.clone(), save).await {
            Ok(record) => Ok(record),
            Err(error) => self.revisions.operations.fail(&operation, error).await,
        }
    }

    async fn fork(&self, context: RequestContext, request: ForkBranch) -> Result<OperationRecord> {
        let runtime = &self.revisions.runtime;
        let _lifecycle = runtime.lifecycle.lock().await;
        runtime
            .authorize(
                &context.principal,
                request.workspace,
                None,
                Permission::Write,
            )
            .await?;
        let operation = match self
            .revisions
            .operations
            .start(
                context,
                "branch_create",
                &request,
                Some(request.workspace),
                None,
            )
            .await?
        {
            OperationStart::Existing(record) => return Ok(record),
            OperationStart::New(context) => context,
        };
        let result = async {
            if request.source.is_some() && request.source_branch.is_some() {
                return Err(Error::invalid("choose one branch source"));
            }
            if runtime
                .state
                .branches(request.workspace)
                .await?
                .iter()
                .any(|branch| branch.name == request.name && branch.state != BranchState::Deleted)
            {
                return Err(Error::new(
                    ErrorCode::AlreadyExists,
                    "branch name already exists",
                ));
            }
            let (source, source_branch) = if let Some(source) = request.source {
                let revision = self
                    .revisions
                    .trees
                    .revision(request.workspace, source)
                    .await?;
                (source, revision.branch)
            } else {
                let id = match request.source_branch {
                    Some(id) => id,
                    None => runtime
                        .state
                        .selected_branch(request.workspace)
                        .await?
                        .ok_or_else(|| {
                            Error::invalid("select a branch or provide an explicit source")
                        })?,
                };
                runtime
                    .authorize(
                        &operation.operation.principal,
                        request.workspace,
                        Some(id),
                        Permission::Read,
                    )
                    .await?;
                let _gate = runtime.lock_branch(id).await;
                let _protection = runtime.objects.read().await;
                let branch = runtime.branch(id).await?;
                let branch = self
                    .revisions
                    .preserve_working_locked(&branch, &operation.operation.principal)
                    .await?;
                (
                    branch.autosave_head.unwrap_or(branch.formal_head),
                    Some(branch.id),
                )
            };
            let _protection = runtime.objects.read().await;
            self.revisions
                .fork_locked(
                    operation.clone(),
                    request.workspace,
                    ForkSource {
                        revision: source,
                        working_root: None,
                    },
                    request.name,
                    source_branch,
                    false,
                )
                .await
        }
        .await;
        match result {
            Ok(record) => Ok(record),
            Err(error) => self.revisions.operations.fail(&operation, error).await,
        }
    }

    async fn restore(
        &self,
        context: RequestContext,
        request: RestoreBranch,
    ) -> Result<OperationRecord> {
        let branch = self.revisions.runtime.branch(request.guard.branch).await?;
        self.revisions
            .runtime
            .authorize(
                &context.principal,
                branch.workspace,
                Some(branch.id),
                Permission::Write,
            )
            .await?;
        let operation = match self
            .revisions
            .operations
            .start(
                context,
                "revision_restore",
                &request,
                Some(branch.workspace),
                Some(branch.id),
            )
            .await?
        {
            OperationStart::Existing(record) => return Ok(record),
            OperationStart::New(context) => context,
        };
        let result = self
            .transitions
            .replace_revision(
                operation.clone(),
                request.guard,
                request.revision,
                None,
                request.durability,
            )
            .await;
        match result {
            Ok(record) => Ok(record),
            Err(error) => self.revisions.operations.fail(&operation, error).await,
        }
    }

    async fn list(
        &self,
        actor: &Principal,
        workspace: WorkspaceId,
        branch: Option<BranchId>,
    ) -> Result<Vec<Revision>> {
        self.revisions
            .runtime
            .authorize(actor, workspace, branch, Permission::Read)
            .await?;
        let mut revisions: Vec<_> = self
            .revisions
            .runtime
            .state
            .revisions(workspace)
            .await?
            .into_iter()
            .filter(|revision| branch.is_none() || revision.branch == branch)
            .collect();
        revisions.sort_by_key(|revision| (revision.created_ns, revision.id));
        Ok(revisions)
    }

    async fn diff(
        &self,
        actor: &Principal,
        request: CompareRevisions,
    ) -> Result<RevisionDifferences> {
        let workspace = request.workspace;
        self.revisions
            .runtime
            .authorize(actor, workspace, None, Permission::Read)
            .await?;
        let limit = request.limit.unwrap_or(100);
        if !(1..=200).contains(&limit) {
            return Err(Error::invalid("diff page size must be between 1 and 200"));
        }
        let _protection = self.revisions.runtime.objects.read().await;
        let before = match request.before {
            Some(id) => Some(self.revisions.trees.revision(workspace, id).await?),
            None => None,
        };
        let after = self
            .revisions
            .trees
            .revision(workspace, request.after)
            .await?;
        let changes = self
            .queries
            .differences(workspace, before.as_ref().map(|r| &r.root), &after.root)
            .await?;
        let total = changes.len();
        let mut changes = changes.into_iter().filter(|change| {
            request
                .cursor
                .as_ref()
                .is_none_or(|cursor| &change.path > cursor)
        });
        let entries: Vec<_> = changes.by_ref().take(limit).collect();
        let next = if changes.next().is_some() {
            entries.last().map(|entry| entry.path.clone())
        } else {
            None
        };
        Ok(RevisionDifferences {
            entries,
            next,
            total,
        })
    }

    async fn text_diff(
        &self,
        actor: &Principal,
        request: CompareRevisionFile,
    ) -> Result<RevisionTextDiff> {
        self.revisions
            .runtime
            .authorize(actor, request.workspace, None, Permission::Read)
            .await?;
        let protection = self.revisions.runtime.objects.read().await;
        let before = match request.before {
            Some(id) => {
                let revision = self.revisions.trees.revision(request.workspace, id).await?;
                self.queries
                    .lookup(
                        request.workspace,
                        &revision.root,
                        request.previous_path.as_ref().unwrap_or(&request.path),
                    )
                    .await?
            }
            None => None,
        };
        let revision = self
            .revisions
            .trees
            .revision(request.workspace, request.after)
            .await?;
        let after = self
            .queries
            .lookup(request.workspace, &revision.root, &request.path)
            .await?;
        if before.is_none() && after.is_none() {
            return Err(Error::new(
                ErrorCode::NotFound,
                "file is absent from both revisions",
            ));
        }
        let mut response = RevisionTextDiff {
            status: TextDiffStatus::Text,
            before,
            after,
            hunks: Vec::new(),
        };
        if let (Some(before), Some(after)) = (&response.before, &response.after)
            && before.kind == FileKind::File
            && after.kind == FileKind::File
            && before.content == after.content
        {
            response.status = TextDiffStatus::Unchanged;
            return Ok(response);
        }
        if response
            .before
            .iter()
            .chain(&response.after)
            .all(|i| i.kind == FileKind::Directory)
        {
            response.status = TextDiffStatus::Directory;
            return Ok(response);
        }
        if response
            .before
            .iter()
            .chain(&response.after)
            .any(|i| i.size > crate::text_diff::MAX_BYTES as u64)
        {
            response.status = TextDiffStatus::TooLarge;
            return Ok(response);
        }
        let mut contents = Vec::new();
        for inode in [&response.before, &response.after] {
            let bytes = match inode.as_ref().filter(|i| i.kind == FileKind::File) {
                Some(inode) => {
                    let object = inode
                        .content
                        .as_ref()
                        .ok_or_else(|| Error::integrity("file content is missing"))?;
                    read_content(
                        self.revisions.trees.content.as_ref(),
                        request.workspace,
                        object,
                        0,
                        crate::text_diff::MAX_BYTES as u32,
                    )
                    .await?
                    .to_vec()
                }
                None => Vec::new(),
            };
            contents.push(bytes);
        }
        drop(protection);
        let after = contents.pop().expect("after content");
        let before = contents.pop().expect("before content");
        tokio::task::spawn_blocking(move || crate::text_diff::compare(response, &before, &after))
            .await
            .map_err(|error| Error::new(ErrorCode::Io, format!("text diff failed: {error}")))
    }

    async fn files(&self, actor: &Principal, request: ListRevisionFiles) -> Result<RevisionFiles> {
        self.revisions
            .runtime
            .authorize(actor, request.workspace, None, Permission::Read)
            .await?;
        let _protection = self.revisions.runtime.objects.read().await;
        let revision = self
            .revisions
            .trees
            .revision(request.workspace, request.revision)
            .await?;
        let tree = self
            .revisions
            .trees
            .tree(request.workspace, &revision.root)
            .await?;
        let selected = request.path.unwrap_or_else(WorkspacePath::root);
        let selected = tree
            .keys()
            .find(|path| path.lookup_key() == selected.lookup_key())
            .ok_or_else(|| Error::new(ErrorCode::NotFound, "revision path does not exist"))?
            .clone();
        let mut entries = tree.into_iter().filter(|(path, _)| {
            selected.contains(path) && request.after.as_ref().is_none_or(|after| path > after)
        });
        let page: FileTree = entries.by_ref().take(200).collect();
        let next = if entries.next().is_some() {
            page.last_key_value().map(|(path, _)| path.clone())
        } else {
            None
        };
        Ok(RevisionFiles {
            entries: page,
            next,
        })
    }

    async fn read_file(
        &self,
        actor: &Principal,
        workspace: WorkspaceId,
        revision: RevisionId,
        path: WorkspacePath,
        offset: u64,
        size: u32,
    ) -> Result<bytes::Bytes> {
        self.revisions
            .runtime
            .authorize(actor, workspace, None, Permission::Read)
            .await?;
        let _protection = self.revisions.runtime.objects.read().await;
        let revision = self.revisions.trees.revision(workspace, revision).await?;
        let inode = self
            .queries
            .lookup(workspace, &revision.root, &path)
            .await?
            .ok_or_else(|| Error::new(ErrorCode::NotFound, "file does not exist in revision"))?;
        if inode.kind != FileKind::File {
            return Err(Error::new(
                ErrorCode::IsDirectory,
                "revision path is a directory",
            ));
        }
        read_content(
            self.revisions.trees.content.as_ref(),
            workspace,
            inode
                .content
                .as_ref()
                .ok_or_else(|| Error::integrity("file content is missing"))?,
            offset,
            size,
        )
        .await
    }
}

pub(crate) fn same_entry(left: Option<&Inode>, right: Option<&Inode>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.kind == right.kind
                && left.mode == right.mode
                && left.content == right.content
                && left.size == right.size
        }
        _ => false,
    }
}
