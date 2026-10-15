use crate::{
    MountService, OperationService, RevisionService, Runtime, revisions::validate_label,
    runtime::increment,
};
use agentfs_model::*;
use agentfs_ports::*;
use async_trait::async_trait;
use parking_lot::Mutex;
use std::{
    collections::BTreeMap,
    sync::{Arc, Weak},
};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

pub struct SessionService {
    runtime: Arc<Runtime>,
    revisions: Arc<RevisionService>,
    operations: Arc<OperationService>,
    mounts: Arc<MountService>,
    gates: Mutex<BTreeMap<SessionKey, Weak<AsyncMutex<()>>>>,
}

impl std::fmt::Debug for SessionService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionService").finish_non_exhaustive()
    }
}

impl SessionService {
    pub fn new(
        runtime: Arc<Runtime>,
        revisions: Arc<RevisionService>,
        operations: Arc<OperationService>,
        mounts: Arc<MountService>,
    ) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            revisions,
            operations,
            mounts,
            gates: Mutex::new(BTreeMap::new()),
        })
    }

    async fn lock(&self, key: &SessionKey) -> Result<OwnedMutexGuard<()>> {
        key.validate()?;
        if key.location != self.runtime.location {
            return Err(Error::new(
                ErrorCode::CoordinatorRequired,
                "session key belongs to another location",
            ));
        }
        let gate = {
            let mut gates = self.gates.lock();
            if let Some(gate) = gates.get(key).and_then(Weak::upgrade) {
                gate
            } else {
                gates.retain(|_, gate| gate.strong_count() != 0);
                let gate = Arc::new(AsyncMutex::new(()));
                gates.insert(key.clone(), Arc::downgrade(&gate));
                gate
            }
        };
        Ok(gate.lock_owned().await)
    }

    async fn binding(&self, actor: &Principal, key: &SessionKey) -> Result<SessionBinding> {
        let binding = self
            .runtime
            .state
            .session(key)
            .await?
            .ok_or_else(|| Error::new(ErrorCode::NotFound, "session does not exist"))?;
        if &binding.principal != actor {
            return Err(Error::new(
                ErrorCode::PermissionDenied,
                "session belongs to another principal",
            ));
        }
        self.runtime
            .authorize(
                actor,
                key.workspace,
                Some(binding.branch),
                Permission::Write,
            )
            .await?;
        Ok(binding)
    }

    async fn record(&self, key: &SessionKey, turn_id: &str) -> Result<Option<TurnRecord>> {
        Ok(self
            .runtime
            .state
            .turns(key.workspace)
            .await?
            .into_iter()
            .find(|turn| &turn.session == key && turn.turn_id == turn_id))
    }

    async fn save_binding(
        &self,
        context: OperationContext,
        session: SessionBinding,
        turn: Option<TurnRecord>,
        result: SaveResult,
    ) -> Result<OperationRecord> {
        let mut branch = self.runtime.branch(session.branch).await?;
        let guard = BranchGuard::from(&branch);
        branch.source_generation = increment(branch.source_generation)?;
        let mut record = context.operation;
        record.branch = Some(branch.id);
        record.local_saved = true;
        record.phase = if record.pending.is_some() {
            OperationPhase::CommittedMountPending
        } else {
            OperationPhase::LocalSaved
        };
        record.result = Some(match result {
            SaveResult::Turn => CommandResult::Turn {
                record: turn
                    .clone()
                    .ok_or_else(|| Error::integrity("turn result is missing"))?,
            },
            _ => CommandResult::Session {
                binding: session.clone(),
            },
        });
        let job = self
            .revisions
            .publisher
            .build_job(&branch, None, &record, turn.as_ref())
            .await?;
        self.runtime
            .state
            .commit(LocalCommit {
                guards: vec![guard],
                branches: vec![branch],
                sessions: vec![session],
                turns: turn.into_iter().collect(),
                operations: vec![record.clone()],
                sync_jobs: vec![job],
                ..Default::default()
            })
            .await?;
        Ok(record)
    }

    async fn open_session(
        &self,
        mut operation: OperationContext,
        request: OpenSession,
    ) -> Result<OperationRecord> {
        if self.runtime.state.session(&request.key).await?.is_some() {
            return Err(Error::new(
                ErrorCode::AlreadyExists,
                "session exists; use session_resume",
            ));
        }
        let branch_id = match request.branch {
            Some(id) => id,
            None => self
                .runtime
                .state
                .selected_branch(request.key.workspace)
                .await?
                .ok_or_else(|| {
                    Error::invalid("select a workspace branch before opening a session")
                })?,
        };
        self.runtime
            .authorize(
                &operation.operation.principal,
                request.key.workspace,
                Some(branch_id),
                Permission::Write,
            )
            .await?;
        let branch = self.runtime.branch(branch_id).await?;
        branch.check_write(self.runtime.location, branch.authority_epoch)?;
        if request.mount_path.is_some()
            && self
                .runtime
                .state
                .sessions(request.key.workspace)
                .await?
                .iter()
                .any(|session| session.branch == branch_id && session.active_turn.is_some())
        {
            return Err(Error::new(
                ErrorCode::BranchBusy,
                "finish the active branch turn before attaching another session",
            ));
        }
        let mut binding = SessionBinding {
            key: request.key.clone(),
            principal: operation.operation.principal.clone(),
            branch: branch_id,
            mount: None,
            state: SessionState::Active,
            active_turn: None,
        };
        let planned = if let Some(path) = request.mount_path {
            let mount_request = PrepareMount {
                workspace: request.key.workspace,
                branch: binding.branch,
                revision: None,
                path,
                access: AccessMode::ReadWrite,
                durability: request.durability,
            };
            let (old, new) = self
                .mounts
                .plan(&binding.principal, &mount_request, branch.generation)
                .await?;
            binding.mount = Some(new.id);
            operation.operation.pending = Some(PendingAction::MountChange {
                old: old.clone(),
                new: Some(new.clone()),
                session: Some(Box::new(binding.clone())),
            });
            operation.operation.mount_ready = Some(false);
            Some((old, new))
        } else {
            None
        };
        let mut result = {
            let _gate = self.runtime.lock_branch(branch_id).await;
            let _protection = self.runtime.objects.read().await;
            self.save_binding(operation, binding.clone(), None, SaveResult::Session)
                .await?
        };
        if let Some((old, new)) = planned {
            result = self
                .mounts
                .attach_session(result, old, Some(new), binding)
                .await?;
        }
        self.revisions
            .finish_durability(result, request.durability)
            .await
    }

    async fn pause_session(
        &self,
        operation: OperationContext,
        request: SessionAction,
        close: bool,
    ) -> Result<OperationRecord> {
        let mut session = self
            .binding(&operation.operation.principal, &request.key)
            .await?;
        if close && session.active_turn.is_some() {
            return Err(Error::new(
                ErrorCode::BranchBusy,
                "finish or interrupt the active turn before closing the session",
            ));
        }
        let mut old_mount = None;
        let record = {
            let _gate = self.runtime.lock_branch(session.branch).await;
            let _protection = self.runtime.objects.read().await;
            let branch = self.runtime.branch(session.branch).await?;
            let mut turn = None;
            if let Some(id) = &session.active_turn {
                let mut record = self
                    .record(&session.key, id)
                    .await?
                    .ok_or_else(|| Error::integrity("active turn record is missing"))?;
                record.status = TurnStatus::Paused;
                record.operation = operation.operation.id;
                turn = Some(record);
            }
            if let Some(id) = session.mount
                && let Some(mut mount) = self.runtime.state.mount(id).await?
            {
                old_mount = Some(mount.clone());
                if mount.state != MountState::Released {
                    mount.state = MountState::Paused;
                    self.runtime
                        .state
                        .commit(LocalCommit {
                            mounts: vec![mount],
                            ..Default::default()
                        })
                        .await?;
                }
            }
            session.state = if close {
                SessionState::Closed
            } else {
                SessionState::Paused
            };
            if close {
                session.mount = None;
            }
            self.revisions
                .save_locked(
                    operation.clone(),
                    SaveRevision {
                        message: None,
                        guard: BranchGuard::from(&branch),
                        kind: RevisionKind::Checkpoint,
                        parents: None,
                        replace_root: None,
                        turn,
                        session: Some(session.clone()),
                        result: SaveResult::Session,
                        durability: Durability::Local,
                    },
                )
                .await?
        };
        let record = if close && old_mount.is_some() {
            self.mounts
                .attach_session(record, old_mount, None, session)
                .await?
        } else {
            record
        };
        self.revisions
            .finish_durability(record, request.durability)
            .await
    }
}

#[async_trait]
impl SessionApi for SessionService {
    async fn get(&self, actor: &Principal, key: &SessionKey) -> Result<Option<SessionBinding>> {
        key.validate()?;
        self.runtime
            .authorize(actor, key.workspace, None, Permission::Write)
            .await?;
        match self.runtime.state.session(key).await? {
            Some(binding) if &binding.principal == actor => Ok(Some(binding)),
            Some(_) => Err(Error::new(
                ErrorCode::PermissionDenied,
                "session belongs to another principal",
            )),
            None => Ok(None),
        }
    }

    async fn open(&self, context: RequestContext, request: OpenSession) -> Result<OperationRecord> {
        let _lifecycle = self.runtime.lifecycle.lock().await;
        let _session_gate = self.lock(&request.key).await?;
        self.runtime
            .authorize(
                &context.principal,
                request.key.workspace,
                None,
                Permission::Write,
            )
            .await?;
        let operation = match self
            .operations
            .start(
                context,
                "session_open",
                &request,
                Some(request.key.workspace),
                None,
            )
            .await?
        {
            OperationStart::Existing(record) => return Ok(record),
            OperationStart::New(context) => context,
        };
        match self.open_session(operation.clone(), request).await {
            Ok(record) => Ok(record),
            Err(error) => self.operations.fail(&operation, error).await,
        }
    }

    async fn resume(
        &self,
        context: RequestContext,
        request: ResumeSession,
    ) -> Result<OperationRecord> {
        let _lifecycle = self.runtime.lifecycle.lock().await;
        let _session_gate = self.lock(&request.key).await?;
        let mut session = self.binding(&context.principal, &request.key).await?;
        let operation = match self
            .operations
            .start(
                context,
                "session_resume",
                &request,
                Some(request.key.workspace),
                Some(session.branch),
            )
            .await?
        {
            OperationStart::Existing(record) => return Ok(record),
            OperationStart::New(context) => context,
        };
        let result = async {
            if session.state == SessionState::Closed {
                return Err(Error::new(
                    ErrorCode::Conflict,
                    "closed session cannot be resumed",
                ));
            }
            if self
                .runtime
                .state
                .sessions(request.key.workspace)
                .await?
                .iter()
                .any(|other| {
                    other.branch == session.branch
                        && other.key != session.key
                        && other.active_turn.is_some()
                })
            {
                return Err(Error::new(
                    ErrorCode::BranchBusy,
                    "another session has an active turn on this branch",
                ));
            }
            let mut turn = if let Some(id) = &session.active_turn {
                Some(
                    self.record(&session.key, id)
                        .await?
                        .ok_or_else(|| Error::integrity("active turn is missing"))?,
                )
            } else {
                None
            };
            if session.state == SessionState::RecoveryRequired && turn.is_some() {
                let action = request.recovery_action.ok_or_else(|| {
                    Error::new(
                        ErrorCode::RecoveryRequired,
                        "choose continue or interrupt for the unfinished turn",
                    )
                })?;
                if request.turn_id.as_ref() != session.active_turn.as_ref() {
                    return Err(Error::new(
                        ErrorCode::TurnResultConflict,
                        "recovery must identify the unfinished turn",
                    ));
                }
                if let Some(turn) = &mut turn {
                    turn.status = match action {
                        RecoveryAction::Continue => TurnStatus::Running,
                        RecoveryAction::Interrupt => TurnStatus::Interrupted,
                    };
                    turn.operation = operation.operation.id;
                    if matches!(action, RecoveryAction::Interrupt) {
                        session.active_turn = None;
                    }
                }
            } else if let Some(turn) = &mut turn {
                turn.status = TurnStatus::Running;
                turn.operation = operation.operation.id;
            }
            session.state = SessionState::Active;
            let path = if let Some(path) = request.mount_path.clone() {
                Some(path)
            } else if let Some(id) = session.mount {
                self.runtime
                    .state
                    .mount(id)
                    .await?
                    .filter(|mount| mount.state != MountState::Released)
                    .map(|mount| mount.path)
            } else {
                None
            };
            let mut branch = self.runtime.branch(session.branch).await?;
            if branch.owner != self.runtime.location {
                return Err(Error::new(
                    ErrorCode::StaleAuthority,
                    "session branch belongs to another location",
                ));
            }
            if branch.state != BranchState::Writable {
                return Err(Error::new(
                    ErrorCode::RecoveryRequired,
                    "stopped branch requires ownership or storage recovery",
                ));
            }
            let planned = if let Some(path) = path {
                let (old, new) = self
                    .mounts
                    .plan(
                        &session.principal,
                        &PrepareMount {
                            workspace: request.key.workspace,
                            branch: session.branch,
                            revision: None,
                            path,
                            access: AccessMode::ReadWrite,
                            durability: request.durability,
                        },
                        branch.generation,
                    )
                    .await?;
                session.mount = Some(new.id);
                Some((old, new))
            } else {
                session.mount = None;
                None
            };
            let mut context = operation.clone();
            if let Some((old, new)) = &planned {
                context.operation.pending = Some(PendingAction::MountChange {
                    old: old.clone(),
                    new: Some(new.clone()),
                    session: Some(Box::new(session.clone())),
                });
                context.operation.mount_ready = Some(false);
            }
            let mut record = {
                let _gate = self.runtime.lock_branch(branch.id).await;
                let _protection = self.runtime.objects.read().await;
                branch = self.runtime.branch(branch.id).await?;
                if turn
                    .as_ref()
                    .is_some_and(|turn| turn.status == TurnStatus::Interrupted)
                {
                    self.revisions
                        .save_locked(
                            context,
                            SaveRevision {
                                message: None,
                                guard: BranchGuard::from(&branch),
                                kind: RevisionKind::Commit,
                                parents: None,
                                replace_root: None,
                                turn,
                                session: Some(session.clone()),
                                result: SaveResult::Session,
                                durability: Durability::Local,
                            },
                        )
                        .await?
                } else {
                    self.save_binding(context, session.clone(), turn, SaveResult::Session)
                        .await?
                }
            };
            if let Some((old, new)) = planned {
                record = self
                    .mounts
                    .attach_session(record, old, Some(new), session)
                    .await?;
            }
            self.revisions
                .finish_durability(record, request.durability)
                .await
        }
        .await;
        match result {
            Ok(record) => Ok(record),
            Err(error) => self.operations.fail(&operation, error).await,
        }
    }

    async fn pause(
        &self,
        context: RequestContext,
        request: SessionAction,
    ) -> Result<OperationRecord> {
        let _lifecycle = self.runtime.lifecycle.lock().await;
        let _session_gate = self.lock(&request.key).await?;
        let session = self.binding(&context.principal, &request.key).await?;
        let operation = match self
            .operations
            .start(
                context,
                "session_pause",
                &request,
                Some(request.key.workspace),
                Some(session.branch),
            )
            .await?
        {
            OperationStart::Existing(record) => return Ok(record),
            OperationStart::New(context) => context,
        };
        match self.pause_session(operation.clone(), request, false).await {
            Ok(record) => Ok(record),
            Err(error) => self.operations.fail(&operation, error).await,
        }
    }

    async fn close(
        &self,
        context: RequestContext,
        request: SessionAction,
    ) -> Result<OperationRecord> {
        let _lifecycle = self.runtime.lifecycle.lock().await;
        let _session_gate = self.lock(&request.key).await?;
        let session = self.binding(&context.principal, &request.key).await?;
        let operation = match self
            .operations
            .start(
                context,
                "session_close",
                &request,
                Some(request.key.workspace),
                Some(session.branch),
            )
            .await?
        {
            OperationStart::Existing(record) => return Ok(record),
            OperationStart::New(context) => context,
        };
        match self.pause_session(operation.clone(), request, true).await {
            Ok(record) => Ok(record),
            Err(error) => self.operations.fail(&operation, error).await,
        }
    }

    async fn turn_begin(
        &self,
        context: RequestContext,
        request: BeginTurn,
    ) -> Result<OperationRecord> {
        let _lifecycle = self.runtime.lifecycle.lock().await;
        let _session_gate = self.lock(&request.key).await?;
        let mut session = self.binding(&context.principal, &request.key).await?;
        let operation = match self
            .operations
            .start(
                context,
                "turn_begin",
                &request,
                Some(request.key.workspace),
                Some(session.branch),
            )
            .await?
        {
            OperationStart::Existing(record) => return Ok(record),
            OperationStart::New(context) => context,
        };
        let result = async {
            validate_label(&request.turn_id, "turn_id")?;
            if self
                .runtime
                .state
                .operations(request.key.workspace)
                .await?
                .iter()
                .any(|record| matches!(record.pending, Some(PendingAction::BranchSwitch { .. })))
            {
                return Err(Error::new(
                    ErrorCode::BranchBusy,
                    "complete or cancel the pending branch switch before starting a turn",
                ));
            }
            if let Some(existing) = self.record(&request.key, &request.turn_id).await? {
                return self
                    .operations
                    .finish(
                        operation.clone(),
                        CommandResult::Turn { record: existing },
                        LocalCommit::default(),
                    )
                    .await;
            }
            if session.state != SessionState::Active {
                return Err(Error::new(
                    ErrorCode::RecoveryRequired,
                    "session is not active",
                ));
            }
            if session.active_turn.is_some() {
                return Err(Error::new(
                    ErrorCode::BranchBusy,
                    "session already has an active turn",
                ));
            }
            if self
                .runtime
                .state
                .sessions(request.key.workspace)
                .await?
                .iter()
                .any(|other| other.branch == session.branch && other.active_turn.is_some())
            {
                return Err(Error::new(
                    ErrorCode::BranchBusy,
                    "another session has an active turn on this branch",
                ));
            }
            let _gate = self.runtime.lock_branch(session.branch).await;
            if let Some(id) = session.mount
                && self
                    .runtime
                    .state
                    .mount(id)
                    .await?
                    .is_none_or(|mount| mount.state != MountState::Ready)
            {
                return Err(Error::new(
                    ErrorCode::MountBusy,
                    "session mount is not ready",
                ));
            }
            let _protection = self.runtime.objects.read().await;
            let branch = self.runtime.branch(session.branch).await?;
            branch.check_write(self.runtime.location, branch.authority_epoch)?;
            let branch = self
                .revisions
                .preserve_working_locked(&branch, &session.principal)
                .await?;
            let turn = TurnRecord {
                session: session.key.clone(),
                turn_id: request.turn_id.clone(),
                begin_revision: branch.autosave_head.unwrap_or(branch.formal_head),
                result_revision: None,
                status: TurnStatus::Running,
                operation: operation.operation.id,
            };
            session.active_turn = Some(request.turn_id);
            self.save_binding(operation.clone(), session, Some(turn), SaveResult::Turn)
                .await
        }
        .await;
        match result {
            Ok(record) => Ok(record),
            Err(error) => self.operations.fail(&operation, error).await,
        }
    }

    async fn turn_end(&self, context: RequestContext, request: EndTurn) -> Result<OperationRecord> {
        let _lifecycle = self.runtime.lifecycle.lock().await;
        let _session_gate = self.lock(&request.key).await?;
        let mut session = self.binding(&context.principal, &request.key).await?;
        let operation = match self
            .operations
            .start(
                context,
                "turn_end",
                &request,
                Some(request.key.workspace),
                Some(session.branch),
            )
            .await?
        {
            OperationStart::Existing(record) => return Ok(record),
            OperationStart::New(context) => context,
        };
        let result = async {
            if !matches!(
                request.status,
                TurnStatus::Completed
                    | TurnStatus::Failed
                    | TurnStatus::Cancelled
                    | TurnStatus::Interrupted
            ) {
                return Err(Error::invalid("turn_end requires a terminal turn status"));
            }
            let mut turn = self
                .record(&request.key, &request.turn_id)
                .await?
                .ok_or_else(|| Error::new(ErrorCode::NotFound, "turn does not exist"))?;
            if turn.status.is_end() {
                if turn.status != request.status {
                    return Err(Error::new(
                        ErrorCode::TurnResultConflict,
                        "turn already has a different final result",
                    ));
                }
                if request.durability == Durability::Remote {
                    let record = {
                        let _gate = self.runtime.lock_branch(session.branch).await;
                        let _protection = self.runtime.objects.read().await;
                        let branch = self.runtime.branch(session.branch).await?;
                        branch.check_write(self.runtime.location, branch.authority_epoch)?;
                        self.save_binding(operation.clone(), session, Some(turn), SaveResult::Turn)
                            .await?
                    };
                    return self
                        .revisions
                        .finish_durability(record, Durability::Remote)
                        .await;
                }
                return self
                    .operations
                    .finish(
                        operation.clone(),
                        CommandResult::Turn { record: turn },
                        LocalCommit::default(),
                    )
                    .await;
            }
            if session.active_turn.as_ref() != Some(&request.turn_id) {
                return Err(Error::new(
                    ErrorCode::TurnResultConflict,
                    "turn is not the active turn",
                ));
            }
            if session.state == SessionState::RecoveryRequired {
                return Err(Error::new(
                    ErrorCode::RecoveryRequired,
                    "resume the session before ending its recovered turn",
                ));
            }
            session.active_turn = None;
            turn.status = request.status;
            turn.operation = operation.operation.id;
            let record = {
                let _gate = self.runtime.lock_branch(session.branch).await;
                let _protection = self.runtime.objects.read().await;
                let branch = self.runtime.branch(session.branch).await?;
                self.revisions
                    .save_locked(
                        operation.clone(),
                        SaveRevision {
                            message: None,
                            guard: BranchGuard::from(&branch),
                            kind: RevisionKind::Commit,
                            parents: None,
                            replace_root: None,
                            turn: Some(turn),
                            session: Some(session),
                            result: SaveResult::Turn,
                            durability: Durability::Local,
                        },
                    )
                    .await?
            };
            self.revisions
                .finish_durability(record, request.durability)
                .await
        }
        .await;
        match result {
            Ok(record) => Ok(record),
            Err(error) => self.operations.fail(&operation, error).await,
        }
    }
}
