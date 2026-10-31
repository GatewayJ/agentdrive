use crate::{FileService, OperationService, RevisionService, Runtime, runtime::check_guard};
use agentdrive_model::*;
use agentdrive_ports::*;
use async_trait::async_trait;
use std::sync::Arc;

pub struct MountService {
    runtime: Arc<Runtime>,
    revisions: Arc<RevisionService>,
    operations: Arc<OperationService>,
    files: Arc<FileService>,
    driver: Arc<dyn MountDriver>,
    identity: LocalIdentity,
}

impl std::fmt::Debug for MountService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MountService")
            .field("driver", &self.driver.capability())
            .finish_non_exhaustive()
    }
}

impl MountService {
    pub fn new(
        runtime: Arc<Runtime>,
        revisions: Arc<RevisionService>,
        operations: Arc<OperationService>,
        files: Arc<FileService>,
        driver: Arc<dyn MountDriver>,
        identity: LocalIdentity,
    ) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            revisions,
            operations,
            files,
            driver,
            identity,
        })
    }

    pub(crate) async fn plan(
        &self,
        actor: &Principal,
        request: &PrepareMount,
        generation: u64,
    ) -> Result<(Option<MountBinding>, MountBinding)> {
        let branch = request
            .branch
            .ok_or_else(|| Error::integrity("mount branch is unresolved"))?;
        let path = self.driver.normalize_path(&request.path).await?;
        let mounts = self.runtime.state.mounts().await?;
        let old = mounts
            .iter()
            .find(|mount| mount.path == path && mount.state != MountState::Released)
            .cloned();
        if let Some(old) = &old {
            self.runtime
                .authorize(actor, old.workspace, Some(old.branch), Permission::Manage)
                .await?;
        }
        if request.access == AccessMode::ReadWrite
            && mounts.iter().any(|mount| {
                mount.branch == branch
                    && mount.access == AccessMode::ReadWrite
                    && mount.state != MountState::Released
                    && old.as_ref().is_none_or(|old| old.id != mount.id)
            })
        {
            return Err(Error::new(
                ErrorCode::MountBusy,
                "branch already has a writable mount",
            ));
        }
        let new = MountBinding {
            id: MountId::new(),
            workspace: request.workspace,
            branch,
            revision: request.revision,
            generation,
            path,
            principal: actor.clone(),
            identity: self.identity,
            durability: request.durability,
            access: request.access,
            state: MountState::Preparing,
        };
        Ok((old, new))
    }

    // 调用方持有 lifecycle 锁，分支创建与本机选择在同一个事务中提交。
    async fn location_branch(&self, actor: &Principal, workspace: WorkspaceId) -> Result<Branch> {
        if let Some(id) = self.runtime.state.selected_branch(workspace).await? {
            let branch = self.runtime.branch(id).await?;
            branch.check_write(self.runtime.location, branch.authority_epoch)?;
            return Ok(branch);
        }
        let mut local: Vec<_> = self
            .runtime
            .state
            .branches(workspace)
            .await?
            .into_iter()
            .filter(|branch| {
                branch.owner == self.runtime.location
                    && branch.state == BranchState::Writable
                    && !branch.protected
            })
            .collect();
        let references = if local.is_empty() {
            let references = self.revisions.publisher.discover(workspace).await?;
            local = self
                .runtime
                .state
                .branches(workspace)
                .await?
                .into_iter()
                .filter(|branch| {
                    branch.owner == self.runtime.location
                        && branch.state == BranchState::Writable
                        && !branch.protected
                })
                .collect();
            references
        } else {
            Vec::new()
        };
        if local.len() == 1 {
            let branch = local.into_iter().next().unwrap();
            self.runtime
                .state
                .commit(LocalCommit {
                    selections: vec![WorkspaceSelection {
                        workspace,
                        branch: branch.id,
                    }],
                    ..Default::default()
                })
                .await?;
            return Ok(branch);
        }
        if !local.is_empty() {
            return Err(Error::invalid("select a local branch with branch_switch"));
        }
        let info = self
            .runtime
            .authorize(actor, workspace, None, Permission::Write)
            .await?;
        let initial = self
            .revisions
            .trees
            .revision(workspace, info.initial_revision)
            .await?;
        let source_branch = initial.branch.ok_or_else(|| {
            Error::new(
                ErrorCode::RecoveryRequired,
                "workspace has no root branch; select an explicit branch",
            )
        })?;
        info.authorize(actor, Some(source_branch), Permission::Read)?;
        let remote = references
            .into_iter()
            .find(|reference| reference.branch == source_branch && !reference.deleted)
            .ok_or_else(|| {
                Error::new(ErrorCode::NotFound, "workspace root branch is unavailable")
            })?;
        let source = remote.autosave_head.unwrap_or(remote.formal_head);
        let name = format!("location-{}", self.runtime.location);
        let context = match self
            .operations
            .start(
                RequestContext {
                    principal: actor.clone(),
                    request_id: OperationId::new().to_string(),
                },
                "location_branch_create",
                &(workspace, source, &name),
                Some(workspace),
                None,
            )
            .await?
        {
            OperationStart::New(context) => context,
            OperationStart::Existing(_) => {
                return Err(Error::integrity("generated request already exists"));
            }
        };
        let _protection = self.runtime.objects.read().await;
        let record = self
            .revisions
            .fork_locked(
                context.clone(),
                workspace,
                crate::revisions::ForkSource {
                    revision: source,
                    working_root: Some(remote.working_root),
                },
                name,
                Some(source_branch),
                true,
            )
            .await;
        match record {
            Ok(record) => match record.result {
                Some(CommandResult::Branch { branch }) => Ok(branch),
                _ => Err(Error::integrity("created branch result is missing")),
            },
            Err(error) => {
                self.operations.fail(&context, error.clone()).await?;
                Err(error)
            }
        }
    }

    async fn pause_old(&self, old: &MountBinding) -> Result<()> {
        let _gate = self.runtime.lock_branch(old.branch).await;
        let mut current = self
            .runtime
            .state
            .mount(old.id)
            .await?
            .ok_or_else(|| Error::new(ErrorCode::StaleBinding, "old mount is missing"))?;
        if current.generation != old.generation {
            return Err(Error::new(
                ErrorCode::StaleBinding,
                "old mount generation changed",
            ));
        }
        if self.files.open_handles(old.id) != 0 {
            return Err(Error::new(
                ErrorCode::MountBusy,
                "close open file handles before releasing the mount",
            ));
        }
        if current.state != MountState::Released {
            current.state = MountState::Paused;
            self.runtime
                .state
                .commit(LocalCommit {
                    mounts: vec![current],
                    ..Default::default()
                })
                .await?;
        }
        Ok(())
    }

    async fn remove_old(&self, old: &MountBinding) -> Result<()> {
        self.pause_old(old).await?;
        let result = async {
            if old.access == AccessMode::ReadWrite {
                let branch = self.runtime.branch(old.branch).await?;
                if branch.state == BranchState::Writable
                    && branch.owner == self.runtime.location
                    && branch.mutation_seq != branch.saved_seq
                {
                    self.revisions
                        .flush(old.branch, old.principal.clone(), old.durability)
                        .await?;
                }
            }
            if self.driver.status(old).await? != NativeMountState::Absent {
                self.driver.unmount(old).await?;
            }
            let mut released = old.clone();
            released.state = MountState::Released;
            self.runtime
                .state
                .commit(LocalCommit {
                    mounts: vec![released],
                    ..Default::default()
                })
                .await
        }
        .await;
        if result.is_err() {
            let mut restored = old.clone();
            restored.state = if self.driver.status(old).await? == NativeMountState::Mounted {
                MountState::Ready
            } else {
                MountState::RecoveryRequired
            };
            self.runtime
                .state
                .commit(LocalCommit {
                    mounts: vec![restored],
                    ..Default::default()
                })
                .await?;
        }
        result
    }

    async fn install_new(&self, binding: &MountBinding) -> Result<MountBinding> {
        let mut ready = binding.clone();
        ready.state = MountState::Ready;
        self.runtime
            .state
            .commit(LocalCommit {
                mounts: vec![ready.clone()],
                ..Default::default()
            })
            .await?;
        match self.driver.status(&ready).await? {
            NativeMountState::Mounted => (),
            NativeMountState::Absent => {
                if let Err(error) = self
                    .driver
                    .mount(
                        MountSpec {
                            binding: ready.clone(),
                        },
                        self.files.clone(),
                    )
                    .await
                {
                    let mut failed = ready.clone();
                    failed.state = MountState::RecoveryRequired;
                    self.runtime
                        .state
                        .commit(LocalCommit {
                            mounts: vec![failed],
                            ..Default::default()
                        })
                        .await?;
                    return Err(error);
                }
            }
            NativeMountState::Unknown => {
                return Err(Error::new(
                    ErrorCode::MountBusy,
                    "native mount identity cannot be confirmed",
                ));
            }
        }
        Ok(ready)
    }

    async fn execute(&self, mut operation: OperationRecord) -> Result<OperationRecord> {
        let pending = operation
            .pending
            .clone()
            .ok_or_else(|| Error::invalid("operation has no pending mount action"))?;
        let result = async {
            match pending {
                PendingAction::BranchSwitch {
                    selection,
                    previous,
                    old,
                    new,
                } => {
                    let selected = self
                        .runtime
                        .state
                        .selected_branch(selection.workspace)
                        .await?;
                    if selected.is_some_and(|id| id != previous) {
                        return Err(Error::new(
                            ErrorCode::TargetMoved,
                            "workspace selection changed",
                        ));
                    }
                    let branch = self.runtime.branch(selection.branch).await?;
                    branch.check_write(self.runtime.location, branch.authority_epoch)?;
                    if new
                        .as_ref()
                        .is_some_and(|new| new.generation != branch.generation)
                    {
                        return Err(Error::new(
                            ErrorCode::TargetMoved,
                            "target branch generation changed",
                        ));
                    }
                    if let Some(old) = &old {
                        self.remove_old(old).await?;
                    }
                    let binding = match &new {
                        Some(new) => Some(self.install_new(new).await?),
                        None => None,
                    };
                    operation.pending = None;
                    operation.phase = OperationPhase::Complete;
                    operation.local_saved = true;
                    operation.mount_ready = binding.as_ref().map(|_| true);
                    operation.error = None;
                    operation.result = Some(CommandResult::Branch { branch });
                    self.runtime
                        .state
                        .commit(LocalCommit {
                            selections: vec![selection],
                            operations: vec![operation.clone()],
                            ..Default::default()
                        })
                        .await?;
                    Ok(operation.clone())
                }
                PendingAction::MountChange { old, new } => {
                    if let Some(new) = &new {
                        let branch = self.runtime.branch(new.branch).await?;
                        if branch.generation != new.generation
                            || self
                                .runtime
                                .state
                                .mount(new.id)
                                .await?
                                .is_some_and(|mount| {
                                    mount.state == MountState::Released
                                        || mount.generation != new.generation
                                })
                        {
                            return Err(Error::new(
                                ErrorCode::StaleBinding,
                                "pending mount binding has changed",
                            ));
                        }
                        if new.access == AccessMode::ReadWrite {
                            branch.check_write(self.runtime.location, branch.authority_epoch)?;
                        }
                        if self.runtime.state.mounts().await?.iter().any(|mount| {
                            mount.state != MountState::Released
                                && mount.id != new.id
                                && old.as_ref().is_none_or(|old| old.id != mount.id)
                                && (mount.path == new.path
                                    || (new.access == AccessMode::ReadWrite
                                        && mount.access == AccessMode::ReadWrite
                                        && mount.branch == new.branch))
                        }) {
                            return Err(Error::new(
                                ErrorCode::MountBusy,
                                "pending mount conflicts with an active binding",
                            ));
                        }
                    }
                    if let Some(old) = &old {
                        self.remove_old(old).await?;
                    }
                    let binding = match new {
                        Some(new) => Some(self.install_new(&new).await?),
                        None => None,
                    };
                    let mut commit = LocalCommit::default();
                    if let Some(binding) = binding {
                        operation.result = Some(CommandResult::Mount { binding });
                    } else {
                        operation.result = Some(CommandResult::Updated);
                    }
                    operation.phase = OperationPhase::Complete;
                    operation.pending = None;
                    operation.local_saved = true;
                    operation.mount_ready = Some(true);
                    operation.error = None;
                    commit.operations.push(operation.clone());
                    self.runtime.state.commit(commit).await?;
                    Ok(operation.clone())
                }
                PendingAction::ApplyRevision {
                    target,
                    revision,
                    merge,
                    mount,
                    durability,
                } => {
                    if !operation.local_saved {
                        if let Some(old) = &mount {
                            self.remove_old(old).await?;
                        }
                        operation = self
                            .apply_replacement(operation.clone(), target, revision, merge)
                            .await?;
                    }
                    if let Some(old) = mount {
                        let branch = self.runtime.branch(target.branch).await?;
                        let mut new = old;
                        new.generation = branch.generation;
                        new.state = MountState::Preparing;
                        self.install_new(&new).await?;
                    }
                    let mut commit = LocalCommit::default();
                    if let Some(id) = merge {
                        let mut candidate = self
                            .runtime
                            .state
                            .merge(id)
                            .await?
                            .ok_or_else(|| Error::integrity("merge candidate is missing"))?;
                        if let Some(CommandResult::Revision { revision, .. }) = operation.result {
                            candidate.applied_revision = Some(revision);
                        }
                        commit.merges.push(candidate);
                    }
                    let mut records = self
                        .operations
                        .update(&[operation.id], commit, |current| {
                            current.pending = None;
                            current.error = None;
                            current.mount_ready = Some(true);
                            current.phase = if current.remote_confirmed {
                                OperationPhase::Complete
                            } else {
                                OperationPhase::LocalSaved
                            };
                        })
                        .await?;
                    self.revisions
                        .finish_durability(records.remove(0), durability)
                        .await
                }
                PendingAction::Transfer { .. } => {
                    Err(Error::invalid("ownership transfer is not a mount action"))
                }
            }
        }
        .await;
        match result {
            Ok(record) => Ok(record),
            Err(error) => {
                let current = self
                    .runtime
                    .state
                    .operation(operation.id)
                    .await?
                    .ok_or_else(|| Error::integrity("mount operation is missing"))?;
                if !current.local_saved
                    && let Some(PendingAction::ApplyRevision {
                        mount: Some(old), ..
                    }) = &current.pending
                {
                    let branch = self.runtime.branch(old.branch).await?;
                    if branch.generation == old.generation
                        && branch.state == BranchState::Writable
                        && let Err(restore_error) = self.install_new(old).await
                    {
                        tracing::warn!(error = %restore_error, "previous mount requires recovery");
                    }
                }
                let mut records = self.operations.update(
                    &[operation.id], LocalCommit::default(), |current| {
                        let branch_switch = matches!(current.pending, Some(PendingAction::BranchSwitch { .. }));
                        let mut error = error.clone();
                        if branch_switch && error.code == ErrorCode::MountBusy {
                            error.message = format!(
                                "{}; close files and processes using the mounted directory, or restart the Agent, then retry branch_switch with the same request_id",
                                error.message
                            );
                            error.retryable = true;
                        }
                        let needs_recovery = branch_switch && error.code != ErrorCode::MountBusy;
                        current.error = Some(error);
                        current.mount_ready = Some(false);
                        current.phase = if current.local_saved {
                            OperationPhase::CommittedMountPending
                        } else if needs_recovery {
                            OperationPhase::RecoveryRequired
                        } else {
                            OperationPhase::WaitingForQuiesce
                        };
                    }
                ).await?;
                Ok(records.remove(0))
            }
        }
    }

    async fn apply_replacement(
        &self,
        operation: OperationRecord,
        target: BranchGuard,
        revision: RevisionId,
        merge: Option<MergeId>,
    ) -> Result<OperationRecord> {
        let _gate = self.runtime.lock_branch(target.branch).await;
        let _protection = self.runtime.objects.read().await;
        let branch = self.runtime.branch(target.branch).await?;
        check_guard(&branch, &target)?;
        if merge.is_none() {
            self.revisions
                .preserve_working_locked(&branch, &operation.principal)
                .await?;
        }
        let revision = self
            .revisions
            .trees
            .revision(branch.workspace, revision)
            .await?;
        self.files.replace(branch.id, &revision.root).await?;
        let parents = if let Some(id) = merge {
            let candidate =
                self.runtime.state.merge(id).await?.ok_or_else(|| {
                    Error::new(ErrorCode::NotFound, "merge candidate does not exist")
                })?;
            if candidate.revision != revision.id || candidate.target != target {
                return Err(Error::new(
                    ErrorCode::CandidateMoved,
                    "merge candidate changed",
                ));
            }
            if branch.dirty {
                return Err(Error::new(
                    ErrorCode::DirtyWorktree,
                    "merge target has uncommitted changes",
                ));
            }
            if !candidate.conflicts.is_empty() {
                return Err(Error::new(
                    ErrorCode::UnresolvedConflict,
                    "merge has unresolved conflicts",
                ));
            }
            Some(vec![target.expected_head, candidate.source])
        } else {
            None
        };
        self.revisions
            .save_locked(
                OperationContext { operation },
                SaveRevision {
                    message: None,
                    guard: target,
                    kind: RevisionKind::Commit,
                    parents,
                    replace_root: Some(revision.root),
                    durability: Durability::Local,
                },
            )
            .await
    }
}

#[async_trait]
impl MountApi for MountService {
    async fn switch_branch(
        &self,
        context: RequestContext,
        request: SwitchBranch,
    ) -> Result<OperationRecord> {
        let _lifecycle = self.runtime.lifecycle.lock().await;
        self.runtime
            .authorize(
                &context.principal,
                request.workspace,
                Some(request.branch),
                Permission::Manage,
            )
            .await?;
        let operation = match self
            .operations
            .start(
                context,
                "branch_switch",
                &request,
                Some(request.workspace),
                Some(request.branch),
            )
            .await?
        {
            OperationStart::Existing(record) => {
                return if matches!(record.pending, Some(PendingAction::BranchSwitch { .. })) {
                    self.execute(record).await
                } else {
                    Ok(record)
                };
            }
            OperationStart::New(context) => context,
        };
        let result = async {
            let branch = self.runtime.branch(request.branch).await?;
            branch.check_write(self.runtime.location, branch.authority_epoch)?;
            let selected = self
                .runtime
                .state
                .selected_branch(request.workspace)
                .await?;
            let previous = selected.unwrap_or(request.branch);
            if self
                .runtime
                .state
                .operations(request.workspace)
                .await?
                .iter()
                .any(|record| matches!(record.pending, Some(PendingAction::BranchSwitch { .. })))
            {
                return Err(Error::new(
                    ErrorCode::Busy,
                    "workspace has a pending branch switch",
                ));
            }
            if selected == Some(request.branch) {
                return self
                    .operations
                    .finish(
                        operation.clone(),
                        CommandResult::Branch { branch },
                        LocalCommit::default(),
                    )
                    .await;
            }
            let old = self
                .runtime
                .state
                .mounts()
                .await?
                .into_iter()
                .find(|mount| {
                    mount.branch == previous
                        && mount.access == AccessMode::ReadWrite
                        && mount.state != MountState::Released
                });
            let (old, new) = if let Some(old) = old {
                let (old, new) = self
                    .plan(
                        &operation.operation.principal,
                        &PrepareMount {
                            workspace: request.workspace,
                            branch: Some(request.branch),
                            revision: None,
                            path: old.path,
                            access: AccessMode::ReadWrite,
                            durability: old.durability,
                        },
                        branch.generation,
                    )
                    .await?;
                (old, Some(new))
            } else {
                (None, None)
            };
            {
                let _gate = self.runtime.lock_branch(previous).await;
                let _protection = self.runtime.objects.read().await;
                let previous_branch = self.runtime.branch(previous).await?;
                self.revisions
                    .preserve_working_locked(&previous_branch, &operation.operation.principal)
                    .await?;
            }
            let mut record = operation.operation.clone();
            record.pending = Some(PendingAction::BranchSwitch {
                selection: WorkspaceSelection {
                    workspace: request.workspace,
                    branch: request.branch,
                },
                previous,
                old: old.clone(),
                new,
            });
            record.phase = if old.is_some() {
                OperationPhase::WaitingForQuiesce
            } else {
                OperationPhase::Saving
            };
            record.mount_ready = old.as_ref().map(|_| false);
            self.runtime
                .state
                .commit(LocalCommit {
                    operations: vec![record.clone()],
                    ..Default::default()
                })
                .await?;
            self.execute(record).await
        }
        .await;
        match result {
            Ok(record) => Ok(record),
            Err(error) => self.operations.fail(&operation, error).await,
        }
    }

    async fn prepare(
        &self,
        context: RequestContext,
        mut request: PrepareMount,
    ) -> Result<OperationRecord> {
        let _lifecycle = self.runtime.lifecycle.lock().await;
        request.path = self.driver.normalize_path(&request.path).await?;
        self.revisions
            .publisher
            .load_workspace(&context.principal, request.workspace)
            .await?;
        let permission = if request.access == AccessMode::ReadWrite {
            Permission::Write
        } else {
            Permission::Read
        };
        self.runtime
            .authorize(
                &context.principal,
                request.workspace,
                request.branch,
                permission,
            )
            .await?;
        if request.revision.is_some() && request.access == AccessMode::ReadWrite {
            return Err(Error::invalid("revision mounts must be read-only"));
        }
        let operation = match self
            .operations
            .start(
                context,
                "mount_prepare",
                &request,
                Some(request.workspace),
                request.branch,
            )
            .await?
        {
            OperationStart::Existing(record) => {
                return if matches!(
                    record.pending,
                    Some(PendingAction::MountChange { old: None, .. })
                ) {
                    self.execute(record).await
                } else {
                    Ok(record)
                };
            }
            OperationStart::New(context) => context,
        };
        let result = async {
            if self
                .runtime
                .state
                .operations(request.workspace)
                .await?
                .iter()
                .any(|record| matches!(record.pending, Some(PendingAction::BranchSwitch { .. })))
            {
                return Err(Error::new(
                    ErrorCode::Busy,
                    "workspace has a pending branch switch",
                ));
            }
            let old = self
                .runtime
                .state
                .mounts()
                .await?
                .into_iter()
                .find(|mount| mount.path == request.path && mount.state != MountState::Released);
            if old
                .as_ref()
                .is_some_and(|mount| mount.workspace != request.workspace)
            {
                return Err(Error::new(
                    ErrorCode::MountBusy,
                    "mount path belongs to another workspace",
                ));
            }
            let branch = match request.branch {
                Some(id) => self.runtime.branch(id).await?,
                None if request.access == AccessMode::ReadWrite => {
                    self.location_branch(&operation.operation.principal, request.workspace)
                        .await?
                }
                None => {
                    let id = self
                        .runtime
                        .state
                        .selected_branch(request.workspace)
                        .await?
                        .ok_or_else(|| {
                            Error::invalid("read-only mount requires an explicit branch")
                        })?;
                    self.runtime.branch(id).await?
                }
            };
            if branch.workspace != request.workspace {
                return Err(Error::invalid("branch belongs to another workspace"));
            }
            self.runtime
                .authorize(
                    &operation.operation.principal,
                    request.workspace,
                    Some(branch.id),
                    permission,
                )
                .await?;
            if request.access == AccessMode::ReadWrite {
                branch.check_write(self.runtime.location, branch.authority_epoch)?;
            }
            if let Some(revision) = request.revision {
                self.revisions
                    .trees
                    .revision(request.workspace, revision)
                    .await?;
            }
            request.branch = Some(branch.id);
            if let Some(old) = old {
                if old.branch != branch.id
                    || old.revision != request.revision
                    || old.access != request.access
                    || old.durability != request.durability
                {
                    return Err(Error::new(
                        ErrorCode::MountBusy,
                        "mount path has a different binding; switch or unmount it explicitly",
                    ));
                }
                if old.principal != operation.operation.principal {
                    return Err(Error::new(
                        ErrorCode::PermissionDenied,
                        "mount belongs to another principal",
                    ));
                }
                if old.generation != branch.generation {
                    return Err(Error::new(
                        ErrorCode::StaleBinding,
                        "mount generation changed",
                    ));
                }
                let ready = self.install_new(&old).await?;
                let mut record = operation.operation.clone();
                record.branch = Some(branch.id);
                record.local_saved = true;
                record.mount_ready = Some(true);
                record.phase = OperationPhase::Complete;
                record.result = Some(CommandResult::Mount { binding: ready });
                self.runtime
                    .state
                    .commit(LocalCommit {
                        operations: vec![record.clone()],
                        ..Default::default()
                    })
                    .await?;
                return Ok(record);
            }
            let (_, new) = self
                .plan(&operation.operation.principal, &request, branch.generation)
                .await?;
            let mut record = operation.operation.clone();
            record.branch = Some(branch.id);
            record.pending = Some(PendingAction::MountChange {
                old: None,
                new: Some(new),
            });
            record.mount_ready = Some(false);
            record.phase = OperationPhase::Saving;
            self.runtime
                .state
                .commit(LocalCommit {
                    operations: vec![record.clone()],
                    ..Default::default()
                })
                .await?;
            self.execute(record).await
        }
        .await;
        match result {
            Ok(record) => Ok(record),
            Err(error) => self.operations.fail(&operation, error).await,
        }
    }

    async fn save(&self, context: RequestContext, request: SaveMount) -> Result<OperationRecord> {
        let _lifecycle = self.runtime.lifecycle.lock().await;
        let wait_ms = request.wait_for_remote_ms.unwrap_or(30_000);
        if wait_ms > 120_000 {
            return Err(Error::invalid("remote wait exceeds 120000 ms"));
        }
        let mount = self
            .runtime
            .state
            .mount(request.mount)
            .await?
            .ok_or_else(|| Error::new(ErrorCode::NotFound, "mount does not exist"))?;
        self.runtime
            .authorize(
                &context.principal,
                mount.workspace,
                Some(mount.branch),
                Permission::Write,
            )
            .await?;
        let operation = match self
            .operations
            .start(
                context,
                "mount_save",
                &request,
                Some(mount.workspace),
                Some(mount.branch),
            )
            .await?
        {
            OperationStart::Existing(record) => return self.operations.progress(record).await,
            OperationStart::New(context) => context,
        };
        let result = async {
            if mount.state != MountState::Ready
                || mount.generation != request.expected_binding_generation
            {
                return Err(Error::new(
                    ErrorCode::StaleBinding,
                    "mount is no longer ready at the requested generation",
                ));
            }
            if mount.access != AccessMode::ReadWrite {
                return Err(Error::new(ErrorCode::ReadOnly, "mount is read-only"));
            }
            let record = {
                let _gate = self.runtime.lock_branch(mount.branch).await;
                let _protection = self.runtime.objects.read().await;
                let branch = self.runtime.branch(mount.branch).await?;
                if branch.generation != mount.generation {
                    return Err(Error::new(
                        ErrorCode::StaleBinding,
                        "branch generation changed",
                    ));
                }
                self.revisions
                    .save_locked(
                        operation.clone(),
                        SaveRevision {
                            message: None,
                            guard: BranchGuard::from(&branch),
                            kind: RevisionKind::Commit,
                            parents: None,
                            replace_root: None,
                            durability: request.durability,
                        },
                    )
                    .await?
            };
            self.revisions
                .finish_bounded(record, request.durability, wait_ms)
                .await
        }
        .await;
        match result {
            Ok(record) => Ok(record),
            Err(error) => self.operations.fail(&operation, error).await,
        }
    }

    async fn release(
        &self,
        context: RequestContext,
        request: ReleaseMount,
    ) -> Result<OperationRecord> {
        let _lifecycle = self.runtime.lifecycle.lock().await;
        let target = self
            .operations
            .get(&context.principal, request.operation)
            .await?;
        let operation = match self
            .operations
            .start(
                context,
                "mount_release",
                &request,
                target.workspace,
                target.branch,
            )
            .await?
        {
            OperationStart::Existing(record)
                if matches!(
                    record.phase,
                    OperationPhase::Complete | OperationPhase::Cancelled
                ) =>
            {
                return Ok(record);
            }
            OperationStart::Existing(record) => OperationContext { operation: record },
            OperationStart::New(context) => context,
        };
        let result = async {
            let old = match &target.pending {
                Some(
                    PendingAction::MountChange { old, .. }
                    | PendingAction::BranchSwitch { old, .. },
                ) => old.as_ref(),
                Some(PendingAction::ApplyRevision { mount, .. }) => mount.as_ref(),
                Some(PendingAction::Transfer { .. }) => {
                    return Err(Error::invalid("operation is an ownership transfer"));
                }
                None => {
                    return self
                        .operations
                        .finish(
                            operation.clone(),
                            CommandResult::Operation {
                                operation: target.id,
                            },
                            LocalCommit::default(),
                        )
                        .await;
                }
            };
            if old.is_some_and(|old| old.generation != request.expected_binding_generation) {
                return Err(Error::new(
                    ErrorCode::StaleBinding,
                    "release acknowledged a different binding generation",
                ));
            }
            let resumed = self.execute(target.clone()).await?;
            if let Some(error) = resumed.error {
                return Err(error);
            }
            self.operations
                .finish(
                    operation.clone(),
                    CommandResult::Operation {
                        operation: target.id,
                    },
                    LocalCommit::default(),
                )
                .await
        }
        .await;
        match result {
            Ok(record) => Ok(record),
            Err(error) => self.operations.fail(&operation, error).await,
        }
    }

    async fn unmount(
        &self,
        context: RequestContext,
        id: MountId,
        generation: u64,
    ) -> Result<OperationRecord> {
        let _lifecycle = self.runtime.lifecycle.lock().await;
        let mount = self
            .runtime
            .state
            .mount(id)
            .await?
            .ok_or_else(|| Error::new(ErrorCode::NotFound, "mount does not exist"))?;
        self.runtime
            .authorize(
                &context.principal,
                mount.workspace,
                Some(mount.branch),
                Permission::Manage,
            )
            .await?;
        if mount.generation != generation {
            return Err(Error::new(
                ErrorCode::StaleBinding,
                "mount generation changed",
            ));
        }
        let operation = match self
            .operations
            .start(
                context,
                "mount_unmount",
                &(id, generation),
                Some(mount.workspace),
                Some(mount.branch),
            )
            .await?
        {
            OperationStart::Existing(record) => return Ok(record),
            OperationStart::New(context) => context,
        };
        let mut record = operation.operation.clone();
        record.pending = Some(PendingAction::MountChange {
            old: Some(mount),
            new: None,
        });
        record.phase = OperationPhase::WaitingForQuiesce;
        record.mount_ready = Some(false);
        self.runtime
            .state
            .commit(LocalCommit {
                operations: vec![record.clone()],
                ..Default::default()
            })
            .await?;
        self.execute(record).await
    }
}

#[async_trait]
impl RevisionTransition for MountService {
    async fn replace_revision(
        &self,
        context: OperationContext,
        target: BranchGuard,
        revision: RevisionId,
        merge: Option<MergeId>,
        durability: Durability,
    ) -> Result<OperationRecord> {
        let _lifecycle = self.runtime.lifecycle.lock().await;
        let record = {
            let _gate = self.runtime.lock_branch(target.branch).await;
            let branch = self.runtime.branch(target.branch).await?;
            check_guard(&branch, &target)?;
            self.revisions
                .trees
                .revision(branch.workspace, revision)
                .await?;
            if merge.is_none() {
                branch.check_write(self.runtime.location, target.authority_epoch)?;
            }
            let mount = self
                .runtime
                .state
                .mounts()
                .await?
                .into_iter()
                .find(|mount| {
                    mount.branch == target.branch
                        && mount.access == AccessMode::ReadWrite
                        && mount.state != MountState::Released
                });
            let mut record = context.operation;
            record.pending = Some(PendingAction::ApplyRevision {
                target,
                revision,
                merge,
                mount: mount.clone(),
                durability,
            });
            record.phase = if mount.is_some() {
                OperationPhase::WaitingForQuiesce
            } else {
                OperationPhase::Saving
            };
            record.mount_ready = Some(false);
            self.runtime
                .state
                .commit(LocalCommit {
                    operations: vec![record.clone()],
                    ..Default::default()
                })
                .await?;
            record
        };
        if matches!(
            &record.pending,
            Some(PendingAction::ApplyRevision { mount: Some(_), .. })
        ) {
            Ok(record)
        } else {
            self.execute(record).await
        }
    }
}
