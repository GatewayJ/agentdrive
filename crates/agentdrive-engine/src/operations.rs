use crate::Runtime;
use agentdrive_model::*;
use agentdrive_ports::*;
use async_trait::async_trait;
use serde::Serialize;
use std::sync::Arc;

#[derive(Debug)]
pub struct OperationService {
    runtime: Arc<Runtime>,
}

impl OperationService {
    pub fn new(runtime: Arc<Runtime>) -> Arc<Self> {
        Arc::new(Self { runtime })
    }

    /// Reapply a receipt update if publication or mount completion changed its input.
    pub(crate) async fn update<F>(
        &self,
        ids: &[OperationId],
        commit: LocalCommit,
        update: F,
    ) -> Result<Vec<OperationRecord>>
    where
        F: Fn(&mut OperationRecord) + Send + Sync,
    {
        for _ in 0..8 {
            let mut commit = commit.clone();
            let mut records = Vec::with_capacity(ids.len());
            for id in ids {
                let mut record = self
                    .runtime
                    .state
                    .operation(*id)
                    .await?
                    .ok_or_else(|| Error::integrity("operation receipt is missing"))?;
                commit.operation_guards.push(record.clone());
                update(&mut record);
                records.push(record);
            }
            commit.operations.extend(records.iter().cloned());
            match self.runtime.state.commit(commit).await {
                Ok(()) => return Ok(records),
                Err(error) if error.code == ErrorCode::TargetMoved => continue,
                Err(error) => return Err(error),
            }
        }
        Err(Error::new(
            ErrorCode::Busy,
            "operation receipt kept changing during update",
        )
        .retryable())
    }

    pub(crate) async fn progress(&self, mut record: OperationRecord) -> Result<OperationRecord> {
        let active = self.runtime.sync_progress.lock().get(&record.id).cloned();
        if !record.remote_confirmed
            && let Some(progress) = active
        {
            record.sync_progress = Some(progress.lock().clone());
        } else if !record.remote_confirmed
            && let Some(workspace) = record.workspace
        {
            let jobs = self.runtime.state.sync_jobs(workspace).await?;
            if let Some(position) = jobs.iter().position(|job| job.operation == record.id) {
                record.sync_progress = Some(SyncProgress {
                    stage: SyncStage::Queued,
                    pending_jobs: jobs.len(),
                    jobs_before: position,
                    batch_jobs: 0,
                    total_objects: None,
                    completed_objects: 0,
                    total_bytes: None,
                    completed_bytes: 0,
                    updated_ns: self.runtime.clock.now_ns(),
                });
            }
        }
        Ok(record)
    }

    pub async fn start<T: Serialize + Sync>(
        &self,
        context: RequestContext,
        name: &str,
        request: &T,
        workspace: Option<WorkspaceId>,
        branch: Option<BranchId>,
    ) -> Result<OperationStart> {
        let fingerprint = object_ref(ObjectKind::RecordIndex, &encode(&(name, request))?).id;
        self.begin(context, name, fingerprint, workspace, branch)
            .await
    }

    pub async fn finish(
        &self,
        context: OperationContext,
        result: CommandResult,
        mut commit: LocalCommit,
    ) -> Result<OperationRecord> {
        let mut operation = context.operation;
        operation.phase = OperationPhase::Complete;
        operation.local_saved = true;
        operation.result = Some(result);
        operation.error = None;
        commit.operations.push(operation.clone());
        self.runtime.state.commit(commit).await?;
        self.progress(operation).await
    }
}

#[async_trait]
impl OperationCoordinator for OperationService {
    async fn begin(
        &self,
        context: RequestContext,
        name: &str,
        fingerprint: ObjectId,
        workspace: Option<WorkspaceId>,
        branch: Option<BranchId>,
    ) -> Result<OperationStart> {
        context.validate()?;
        let record = OperationRecord {
            id: OperationId::new(),
            request_id: context.request_id,
            principal: context.principal,
            workspace,
            branch,
            name: name.into(),
            fingerprint,
            phase: OperationPhase::Reserved,
            local_saved: false,
            remote_confirmed: false,
            mount_ready: None,
            result: None,
            error: None,
            pending: None,
            created_ns: self.runtime.clock.now_ns(),
            sync_progress: None,
        };
        let reserved = self.runtime.state.reserve_operation(record).await?;
        Ok(if reserved.created {
            OperationStart::New(OperationContext {
                operation: reserved.record,
            })
        } else {
            OperationStart::Existing(reserved.record)
        })
    }

    async fn fail(&self, context: &OperationContext, error: Error) -> Result<OperationRecord> {
        let mut operation = self
            .runtime
            .state
            .operation(context.operation.id)
            .await?
            .ok_or_else(|| Error::integrity("operation reservation is missing"))?;
        if !operation.local_saved {
            operation.phase = OperationPhase::Failed;
            operation.error = Some(error);
            self.runtime
                .state
                .commit(LocalCommit {
                    operations: vec![operation.clone()],
                    ..Default::default()
                })
                .await?;
        }
        Ok(operation)
    }
}

#[async_trait]
impl OperationApi for OperationService {
    async fn get(&self, actor: &Principal, id: OperationId) -> Result<OperationRecord> {
        let operation = self
            .runtime
            .state
            .operation(id)
            .await?
            .ok_or_else(|| Error::new(ErrorCode::NotFound, "operation does not exist"))?;
        if &operation.principal != actor {
            return Err(Error::new(
                ErrorCode::PermissionDenied,
                "operation belongs to another principal",
            ));
        }
        self.progress(operation).await
    }

    async fn by_request(&self, actor: &Principal, request: &str) -> Result<OperationRecord> {
        let record = self
            .runtime
            .state
            .operation_by_request(actor, request)
            .await?
            .ok_or_else(|| Error::new(ErrorCode::NotFound, "request does not exist"))?;
        self.progress(record).await
    }

    async fn cancel(&self, actor: &Principal, id: OperationId) -> Result<OperationRecord> {
        let _lifecycle = self.runtime.lifecycle.lock().await;
        let mut operation = self.get(actor, id).await?;
        if operation.phase.terminal() {
            return Ok(operation);
        }
        if operation.phase != OperationPhase::WaitingForQuiesce || operation.local_saved {
            return Err(Error::new(
                ErrorCode::Busy,
                "operation cannot be cancelled at this stage",
            ));
        }
        let old = match &operation.pending {
            Some(
                PendingAction::MountChange { old, .. } | PendingAction::BranchSwitch { old, .. },
            ) => old.as_ref(),
            Some(PendingAction::ApplyRevision { mount, .. }) => mount.as_ref(),
            _ => {
                return Err(Error::new(
                    ErrorCode::Busy,
                    "operation has no cancellable mount handoff",
                ));
            }
        };
        if let Some(old) = old {
            let current = self.runtime.state.mount(old.id).await?;
            if current.is_none_or(|mount| {
                mount.generation != old.generation || mount.state != MountState::Ready
            }) {
                return Err(Error::new(
                    ErrorCode::Busy,
                    "mount handoff has already changed the binding",
                ));
            }
        }
        operation.phase = OperationPhase::Cancelled;
        operation.pending = None;
        self.runtime
            .state
            .commit(LocalCommit {
                operations: vec![operation.clone()],
                ..Default::default()
            })
            .await?;
        Ok(operation)
    }
}
