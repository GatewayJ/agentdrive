use crate::{ContentService, OperationService, Runtime, TreeService};
use agentdrive_model::*;
use agentdrive_ports::*;
use async_trait::async_trait;
use futures::{StreamExt, stream::FuturesUnordered};
use parking_lot::Mutex as ProgressMutex;
use std::{
    collections::BTreeMap,
    sync::{Arc, Weak},
};
use tokio::sync::{Mutex, RwLock, Semaphore};

#[derive(Debug)]
pub struct ReplicaService {
    runtime: Arc<Runtime>,
    trees: Arc<TreeService>,
    content: Arc<ContentService>,
    operations: Arc<OperationService>,
    pub(crate) publication: RwLock<()>,
    workspaces: ProgressMutex<BTreeMap<WorkspaceId, Weak<Mutex<()>>>>,
    uploads: Semaphore,
}

fn compatible(previous: &BranchRef, next: &BranchRef) -> bool {
    previous.workspace == next.workspace
        && previous.branch == next.branch
        && previous.owner == next.owner
        && previous.authority_epoch == next.authority_epoch
        && previous.generation == next.generation
        && previous.source_generation.checked_add(1) == Some(next.source_generation)
        && previous.durable_seq <= next.durable_seq
        && !previous.deleted
        && !next.deleted
        && !previous.write_stopped
        && !next.write_stopped
}

struct SyncActivity {
    runtime: Arc<Runtime>,
    operations: Vec<OperationId>,
    progress: Arc<ProgressMutex<SyncProgress>>,
}

impl SyncActivity {
    fn new(runtime: Arc<Runtime>, jobs: &[SyncJob], pending_jobs: usize) -> Self {
        let progress = Arc::new(ProgressMutex::new(SyncProgress {
            stage: SyncStage::Preparing,
            pending_jobs,
            jobs_before: 0,
            batch_jobs: jobs.len(),
            total_objects: None,
            completed_objects: 0,
            total_bytes: None,
            completed_bytes: 0,
            updated_ns: runtime.clock.now_ns(),
        }));
        let operations: Vec<_> = jobs.iter().map(|job| job.operation).collect();
        for id in &operations {
            runtime.sync_progress.lock().insert(*id, progress.clone());
        }
        Self {
            runtime,
            operations,
            progress,
        }
    }
}

impl Drop for SyncActivity {
    fn drop(&mut self) {
        let mut progress = self.runtime.sync_progress.lock();
        for id in &self.operations {
            progress.remove(id);
        }
    }
}

impl ReplicaService {
    pub fn new(
        runtime: Arc<Runtime>,
        trees: Arc<TreeService>,
        content: Arc<ContentService>,
        operations: Arc<OperationService>,
    ) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            trees,
            content,
            operations,
            publication: RwLock::new(()),
            workspaces: ProgressMutex::new(BTreeMap::new()),
            uploads: Semaphore::new(4),
        })
    }

    pub(crate) async fn upload_closure(
        &self,
        workspace: WorkspaceId,
        roots: Vec<ObjectRef>,
    ) -> Result<()> {
        self.upload_closure_progress(workspace, roots, Vec::new(), None)
            .await
    }

    async fn upload_closure_progress(
        &self,
        workspace: WorkspaceId,
        roots: Vec<ObjectRef>,
        retained: Vec<ObjectRef>,
        progress: Option<&Arc<ProgressMutex<SyncProgress>>>,
    ) -> Result<()> {
        let remote = self.content.remote().ok_or_else(|| {
            Error::new(
                ErrorCode::RemoteRequired,
                "remote storage is not configured",
            )
        })?;
        // 限制跨发布操作的传输数量；只有全部成功，调用方才能发布引用。
        let upload = |object: ObjectRef| {
            let remote = remote.clone();
            async move {
                let _permit =
                    self.uploads.acquire().await.map_err(|_| {
                        Error::new(ErrorCode::Unavailable, "upload scheduler closed")
                    })?;
                remote
                    .upload(workspace, self.content.open(workspace, &object).await?)
                    .await?;
                self.content
                    .local
                    .confirm_remote(workspace, &object)
                    .await?;
                if let Some(progress) = progress {
                    let mut progress = progress.lock();
                    progress.completed_objects += 1;
                    progress.completed_bytes += object.size;
                    progress.updated_ns = self.runtime.clock.now_ns();
                }
                Ok(())
            }
        };
        let objects = self.trees.closure_delta(workspace, roots, retained).await?;
        if let Some(progress) = progress {
            let mut progress = progress.lock();
            progress.stage = SyncStage::Transferring;
            progress.total_objects = Some(objects.len() as u64);
            progress.total_bytes = Some(objects.iter().map(|object| object.size).sum());
            progress.updated_ns = self.runtime.clock.now_ns();
        }
        let mut objects = objects.into_iter();
        let mut pending = FuturesUnordered::new();
        for object in objects.by_ref().take(4) {
            pending.push(upload(object));
        }
        let mut failure = None;
        while let Some(result) = pending.next().await {
            if let Err(error) = result {
                failure.get_or_insert(error);
            }
            if failure.is_none()
                && let Some(object) = objects.next()
            {
                pending.push(upload(object));
            }
        }
        // 失败后等待已开始的传输完成清理，保留 S3 multipart 的 abort 路径。
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(())
    }

    pub(crate) async fn contains_operation(
        &self,
        reference: &BranchRef,
        operation: &OperationRecord,
    ) -> Result<bool> {
        let Some(HistoryRecord::Operation(existing)) = self
            .trees
            .index_entry::<HistoryRecord>(
                reference.workspace,
                &reference.record_index,
                &format!("operation/{}", operation.id),
            )
            .await?
        else {
            return Ok(false);
        };
        if existing.fingerprint != operation.fingerprint
            || existing.principal != operation.principal
            || existing.result != operation.result
            || existing.branch != operation.branch
        {
            return Err(Error::integrity(
                "remote operation result differs from its local durable result",
            ));
        }
        Ok(true)
    }

    async fn verify_batch(&self, reference: &BranchRef, jobs: &[SyncJob]) -> Result<()> {
        if let [job] = jobs {
            let operation = self
                .runtime
                .state
                .operation(job.operation)
                .await?
                .ok_or_else(|| Error::integrity("sync job operation is missing"))?;
            return if self.contains_operation(reference, &operation).await? {
                Ok(())
            } else {
                Err(Error::integrity(
                    "publication does not retain a covered operation",
                ))
            };
        }
        let records: BTreeMap<_, _> = self
            .trees
            .load_index::<HistoryRecord>(reference.workspace, &reference.record_index, 1_000_000)
            .await?
            .into_iter()
            .filter_map(|entry| match entry.value {
                HistoryRecord::Operation(record) => Some((record.id, record)),
                _ => None,
            })
            .collect();
        for job in jobs {
            let local = self
                .runtime
                .state
                .operation(job.operation)
                .await?
                .ok_or_else(|| Error::integrity("sync job operation is missing"))?;
            let remote = records.get(&job.operation).ok_or_else(|| {
                Error::integrity("publication does not retain a covered operation")
            })?;
            if remote.fingerprint != local.fingerprint
                || remote.principal != local.principal
                || remote.result != local.result
                || remote.branch != local.branch
            {
                return Err(Error::integrity(
                    "publication changed a covered operation result",
                ));
            }
        }
        Ok(())
    }

    async fn acknowledge(
        &self,
        jobs: &[SyncJob],
        remote: BranchRef,
        storage_version: String,
        progress: &Arc<ProgressMutex<SyncProgress>>,
    ) -> Result<()> {
        let mut completed = progress.lock().clone();
        completed.stage = SyncStage::Complete;
        completed.pending_jobs = completed.pending_jobs.saturating_sub(jobs.len());
        completed.updated_ns = self.runtime.clock.now_ns();
        let ids: Vec<_> = jobs.iter().map(|job| job.operation).collect();
        self.operations
            .update(
                &ids,
                LocalCommit {
                    completed_jobs: ids.clone(),
                    remote_refs: vec![ObservedBranchRef::new(
                        remote.clone(),
                        storage_version,
                        self.runtime.clock.now_ns(),
                    )],
                    ..Default::default()
                },
                |operation| {
                    operation.remote_confirmed = true;
                    operation.error = None;
                    operation.sync_progress = Some(completed.clone());
                    if operation.pending.is_none() {
                        operation.phase = OperationPhase::Complete;
                    }
                },
            )
            .await?;
        *progress.lock() = completed;
        if let Err(error) = self
            .content
            .control(remote.workspace, |control| {
                let mut changed = false;
                for job in jobs {
                    changed |= control.active_operations.remove(&job.operation).is_some();
                    changed |= control.history_pins.remove(&job.operation).is_some();
                }
                Ok(changed)
            })
            .await
        {
            tracing::warn!(%error, "publication confirmed; retention registration remains for recovery");
        }
        Ok(())
    }

    async fn publish_batch(
        &self,
        jobs: &[SyncJob],
        progress: &Arc<ProgressMutex<SyncProgress>>,
    ) -> Result<()> {
        let refs = self.content.refs().ok_or_else(|| {
            Error::new(
                ErrorCode::RemoteRequired,
                "remote storage is not configured",
            )
        })?;
        let job = jobs
            .last()
            .ok_or_else(|| Error::integrity("empty publication batch"))?;
        let reference = &job.reference;
        self.verify_batch(reference, jobs).await?;
        let operation = self
            .runtime
            .state
            .operation(job.operation)
            .await?
            .ok_or_else(|| Error::integrity("sync job operation is missing"))?;
        let key = RefKey::Branch {
            workspace: reference.workspace,
            branch: reference.branch,
        };
        self.content
            .register_publication(
                reference.workspace,
                job.operation,
                vec![
                    reference.working_root.clone(),
                    reference.record_index.clone(),
                ],
            )
            .await?;
        let mut uploaded = false;
        for _ in 0..8 {
            let current = refs.get(&key).await?;
            let mut retained = Vec::new();
            if let Some(current) = &current {
                let remote =
                    decode_branch_ref(reference.workspace, reference.branch, &current.bytes)?;
                if remote == *reference || self.contains_operation(&remote, &operation).await? {
                    self.verify_batch(&remote, jobs).await?;
                    return self
                        .acknowledge(jobs, remote, current.version.clone(), progress)
                        .await;
                }
                if remote.owner != reference.owner
                    || remote.authority_epoch != reference.authority_epoch
                {
                    return Err(Error::new(
                        ErrorCode::StaleAuthority,
                        "remote branch authority has changed",
                    ));
                }
                if remote.source_generation >= reference.source_generation {
                    return Err(Error::integrity(
                        "remote publication is ahead without the expected operation record",
                    ));
                }
                retained.extend([remote.working_root, remote.record_index]);
            } else if reference.authority_epoch != 1 {
                return Err(Error::new(
                    ErrorCode::StaleAuthority,
                    "transferred branch is missing its remote authority record",
                ));
            }
            if !uploaded {
                self.upload_closure_progress(
                    reference.workspace,
                    vec![
                        reference.working_root.clone(),
                        reference.record_index.clone(),
                    ],
                    retained,
                    Some(progress),
                )
                .await?;
                uploaded = true;
            }
            {
                let mut progress = progress.lock();
                progress.stage = SyncStage::Publishing;
                progress.updated_ns = self.runtime.clock.now_ns();
            }
            match refs
                .compare_exchange(RefUpdate {
                    key: key.clone(),
                    expected_version: current.map(|value| value.version),
                    bytes: encode(reference)?.into(),
                })
                .await?
            {
                CasOutcome::Applied { version } => {
                    return self
                        .acknowledge(jobs, reference.clone(), version, progress)
                        .await;
                }
                CasOutcome::Conflict | CasOutcome::Unknown => continue,
            }
        }
        Err(Error::new(
            ErrorCode::RemoteUnknown,
            "remote publication outcome remains unconfirmed",
        )
        .retryable())
    }

    async fn import_history(&self, observation: ObservedBranchRef) -> Result<()> {
        let reference = &observation.reference;
        let mut commit = LocalCommit::default();
        for entry in self
            .trees
            .load_index::<HistoryRecord>(reference.workspace, &reference.record_index, 1_000_000)
            .await?
        {
            match entry.value {
                HistoryRecord::Revision(revision) => {
                    if revision.workspace != reference.workspace {
                        return Err(Error::integrity("revision belongs to another workspace"));
                    }
                    self.content
                        .metadata(reference.workspace, &revision.root)
                        .await?;
                    commit.revisions.push(revision);
                }
                HistoryRecord::Operation(operation) => {
                    if operation.workspace != Some(reference.workspace) {
                        return Err(Error::integrity("operation belongs to another workspace"));
                    }
                    if self.runtime.state.operation(operation.id).await?.is_none() {
                        commit.replicated_operations.push(*operation);
                    }
                }
            }
        }
        if !commit
            .revisions
            .iter()
            .any(|revision| revision.id == reference.formal_head)
            && self
                .runtime
                .state
                .revision(reference.formal_head)
                .await?
                .is_none()
        {
            return Err(Error::integrity(
                "remote formal head is absent from retained history",
            ));
        }
        commit.remote_refs.push(observation);
        self.runtime.state.commit(commit).await
    }
}

pub(crate) fn decode_branch_ref(
    workspace: WorkspaceId,
    branch: BranchId,
    bytes: &[u8],
) -> Result<BranchRef> {
    if bytes.len() > MAX_METADATA_BYTES {
        return Err(Error::integrity("branch reference exceeds format limit"));
    }
    let reference: BranchRef = decode(bytes)?;
    if reference.workspace != workspace
        || reference.branch != branch
        || reference.authority_epoch == 0
        || reference.record_index.kind != ObjectKind::RecordIndex
        || reference.working_root.kind != ObjectKind::Tree
    {
        return Err(Error::integrity("invalid branch reference identity"));
    }
    Ok(reference)
}

#[async_trait]
impl ReplicaPublisher for ReplicaService {
    async fn load_workspace(&self, actor: &Principal, workspace: WorkspaceId) -> Result<()> {
        if self.runtime.state.workspace(workspace).await?.is_none() {
            let _protection = self.runtime.objects.read().await;
            let refs = self.content.refs().ok_or_else(|| {
                Error::new(
                    ErrorCode::RemoteRequired,
                    "remote storage is not configured",
                )
            })?;
            let value = refs
                .get(&RefKey::Workspace(workspace))
                .await?
                .ok_or_else(|| {
                    Error::new(ErrorCode::NotFound, "remote workspace does not exist")
                })?;
            if value.bytes.len() > MAX_METADATA_BYTES {
                return Err(Error::integrity("workspace control exceeds format limit"));
            }
            let control: WorkspaceControl = decode(&value.bytes)?;
            if control.workspace.id != workspace
                || control.workspace.format_version != FORMAT_VERSION
                || control.initial_revision.id != control.workspace.initial_revision
                || control.initial_revision.workspace != workspace
            {
                return Err(Error::integrity("invalid remote workspace"));
            }
            control.workspace.authorize(actor, None, Permission::Read)?;
            self.content
                .metadata(workspace, &control.initial_revision.root)
                .await?;
            match self
                .runtime
                .state
                .commit(LocalCommit {
                    new_workspaces: vec![control.workspace],
                    revisions: vec![control.initial_revision],
                    ..Default::default()
                })
                .await
            {
                Ok(()) => (),
                Err(error)
                    if error.code == ErrorCode::Conflict
                        && self.runtime.state.workspace(workspace).await?.is_some() => {}
                Err(error) => return Err(error),
            }
        }
        self.runtime
            .authorize(actor, workspace, None, Permission::Read)
            .await?;
        Ok(())
    }

    async fn publish_workspace(&self, workspace: WorkspaceId) -> Result<()> {
        let refs = self.content.refs().ok_or_else(|| {
            Error::new(
                ErrorCode::RemoteRequired,
                "remote storage is not configured",
            )
        })?;
        let workspace = self
            .runtime
            .state
            .workspace(workspace)
            .await?
            .ok_or_else(|| Error::new(ErrorCode::NotFound, "workspace does not exist"))?;
        let revision = self
            .trees
            .revision(workspace.id, workspace.initial_revision)
            .await?;
        let key = RefKey::Workspace(workspace.id);
        for _ in 0..8 {
            if let Some(existing) = refs.get(&key).await? {
                let control: WorkspaceControl = decode(&existing.bytes)?;
                if control.workspace != workspace || control.initial_revision != revision {
                    return Err(Error::integrity(
                        "remote workspace identity or policy differs",
                    ));
                }
                return Ok(());
            }
            self.upload_closure(workspace.id, vec![revision.root.clone()])
                .await?;
            let control = WorkspaceControl {
                workspace: workspace.clone(),
                initial_revision: revision.clone(),
                gc_epoch: 0,
                phase: GcPhase::Open,
                active_operations: BTreeMap::new(),
                history_pins: BTreeMap::new(),
            };
            match refs
                .compare_exchange(RefUpdate {
                    key: key.clone(),
                    expected_version: None,
                    bytes: encode(&control)?.into(),
                })
                .await?
            {
                CasOutcome::Applied { .. } => return Ok(()),
                CasOutcome::Conflict | CasOutcome::Unknown => continue,
            }
        }
        Err(Error::new(
            ErrorCode::RemoteUnknown,
            "workspace publication remains unconfirmed",
        )
        .retryable())
    }

    async fn drain(&self, workspace: WorkspaceId, branch: Option<BranchId>) -> Result<()> {
        let _publication = self.publication.read().await;
        let gate = {
            let mut workspaces = self.workspaces.lock();
            if let Some(gate) = workspaces.get(&workspace).and_then(Weak::upgrade) {
                gate
            } else {
                workspaces.retain(|_, gate| gate.strong_count() != 0);
                let gate = Arc::new(Mutex::new(()));
                workspaces.insert(workspace, Arc::downgrade(&gate));
                gate
            }
        };
        let _workspace = gate.lock().await;
        self.publish_workspace(workspace).await?;
        let jobs = self.runtime.state.sync_jobs(workspace).await?;
        let mut batches: Vec<Vec<SyncJob>> = Vec::new();
        for job in jobs {
            if branch.is_some_and(|branch| branch != job.reference.branch) {
                continue;
            }
            if let Some(batch) = batches.last_mut()
                && compatible(&batch[batch.len() - 1].reference, &job.reference)
            {
                batch.push(job);
            } else {
                batches.push(vec![job]);
            }
        }
        let mut blocked = std::collections::BTreeSet::new();
        let mut first_error = None;
        for batch in batches {
            let id = batch[0].reference.branch;
            if blocked.contains(&id) {
                continue;
            }
            let activity = SyncActivity::new(
                self.runtime.clone(),
                &batch,
                self.runtime.state.sync_jobs(workspace).await?.len(),
            );
            if let Err(error) = self.publish_batch(&batch, &activity.progress).await {
                let mut commit = LocalCommit::default();
                let ids: Vec<_> = batch.iter().map(|job| job.operation).collect();
                for mut job in batch {
                    job.attempts = job.attempts.saturating_add(1);
                    job.error = Some(error.clone());
                    commit.sync_jobs.push(job);
                }
                let _gate = if error.code == ErrorCode::StaleAuthority {
                    let gate = self.runtime.lock_branch(id).await;
                    if let Some(mut branch) = self.runtime.state.branch(id).await? {
                        branch.state = BranchState::Stopped;
                        commit.branches.push(branch);
                    }
                    Some(gate)
                } else {
                    None
                };
                self.operations
                    .update(&ids, commit, |operation| {
                        operation.error = Some(error.clone());
                        operation.sync_progress = Some(activity.progress.lock().clone());
                        if operation.pending.is_none() {
                            operation.phase = OperationPhase::LocalSaved;
                        }
                    })
                    .await?;
                blocked.insert(id);
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    async fn build_job(
        &self,
        branch: &Branch,
        new_revision: Option<&Revision>,
        operation: &OperationRecord,
    ) -> Result<SyncJob> {
        let base = self.runtime.state.history_index(branch.id).await?;
        let mut records = BTreeMap::new();
        if base.is_none() {
            for revision in self.runtime.state.revisions(branch.workspace).await? {
                records.insert(
                    format!("revision/{}", revision.id),
                    HistoryRecord::Revision(revision),
                );
            }
            for mut record in self.runtime.state.operations(branch.workspace).await? {
                if record.branch == Some(branch.id) && record.local_saved {
                    record.sync_progress = None;
                    records.insert(
                        format!("operation/{}", record.id),
                        HistoryRecord::Operation(Box::new(record)),
                    );
                }
            }
        }
        if let Some(revision) = new_revision {
            let mut parents = revision.parents.clone();
            while let Some(id) = parents.pop() {
                let key = format!("revision/{id}");
                if records.contains_key(&key) {
                    continue;
                }
                if let Some(base) = &base
                    && self
                        .trees
                        .index_entry::<HistoryRecord>(branch.workspace, base, &key)
                        .await?
                        .is_some()
                {
                    continue;
                }
                let parent = self.trees.revision(branch.workspace, id).await?;
                parents.extend(parent.parents.iter().copied());
                records.insert(key, HistoryRecord::Revision(parent));
            }
            records.insert(
                format!("revision/{}", revision.id),
                HistoryRecord::Revision(revision.clone()),
            );
        }
        let mut record = operation.clone();
        record.sync_progress = None;
        records.insert(
            format!("operation/{}", operation.id),
            HistoryRecord::Operation(Box::new(record)),
        );
        let record_index = if let Some(base) = base {
            self.trees
                .update_index(
                    branch.workspace,
                    &base,
                    records
                        .into_iter()
                        .map(|(key, value)| (key, Some(value)))
                        .collect(),
                )
                .await?
        } else {
            self.trees
                .save_index(
                    branch.workspace,
                    ObjectKind::RecordIndex,
                    records
                        .into_iter()
                        .map(|(key, value)| IndexEntry { key, value })
                        .collect(),
                )
                .await?
        };
        Ok(SyncJob {
            operation: operation.id,
            reference: BranchRef {
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
                record_index,
                deleted: branch.state == BranchState::Deleted,
                write_stopped: branch.state == BranchState::Stopped,
                metadata: Some(branch.into()),
            },
            attempts: 0,
            error: None,
        })
    }

    async fn discover(&self, workspace: WorkspaceId) -> Result<Vec<BranchRef>> {
        self.discover_references(workspace, true).await
    }

    async fn refresh_history(&self, workspace: WorkspaceId) -> Result<()> {
        self.discover_references(workspace, false).await.map(|_| ())
    }
}

impl ReplicaService {
    async fn discover_references(
        &self,
        workspace: WorkspaceId,
        adopt: bool,
    ) -> Result<Vec<BranchRef>> {
        let ids = {
            let _protection = self.runtime.objects.read().await;
            self.content
                .refs()
                .ok_or_else(|| {
                    Error::new(
                        ErrorCode::RemoteRequired,
                        "remote storage is not configured",
                    )
                })?
                .list_branches(workspace)
                .await?
        };
        let known: BTreeMap<_, _> = self
            .runtime
            .state
            .remote_refs(workspace)
            .await?
            .into_iter()
            .map(|reference| (reference.branch, reference))
            .collect();
        let mut result = Vec::new();
        for id in ids {
            let _gate = self.runtime.lock_branch(id).await;
            let _protection = self.runtime.objects.read().await;
            let refs = self.content.refs().ok_or_else(|| {
                Error::new(
                    ErrorCode::RemoteRequired,
                    "remote storage is not configured",
                )
            })?;
            let Some(value) = refs
                .get(&RefKey::Branch {
                    workspace,
                    branch: id,
                })
                .await?
            else {
                continue;
            };
            let reference = decode_branch_ref(workspace, id, &value.bytes)?;
            if !adopt && known.get(&id) == Some(&reference) {
                result.push(reference);
                continue;
            }
            let observation = ObservedBranchRef::new(
                reference.clone(),
                value.version,
                self.runtime.clock.now_ns(),
            );
            let pin = OperationId::new();
            self.content
                .protect_remote(
                    workspace,
                    pin,
                    vec![
                        reference.working_root.clone(),
                        reference.record_index.clone(),
                    ],
                )
                .await?;
            self.import_history(observation).await?;
            self.content
                .metadata(workspace, &reference.working_root)
                .await?;
            let existing = self.runtime.state.branch(id).await?;
            let can_adopt = existing.as_ref().is_none_or(|branch| {
                branch.state == BranchState::Stopped
                    && branch.owner != self.runtime.location
                    && branch.mutation_seq == branch.saved_seq
                    && reference.authority_epoch > branch.authority_epoch
                    && reference.generation > branch.generation
                    && reference.source_generation > branch.source_generation
            });
            if adopt && reference.owner == self.runtime.location && !reference.deleted && can_adopt
            {
                let tree = self.trees.tree(workspace, &reference.working_root).await?;
                let next_inode = crate::runtime::increment(
                    tree.values().map(|inode| inode.id.0).max().unwrap_or(1),
                )?;
                let formal = self
                    .trees
                    .revision(workspace, reference.formal_head)
                    .await?;
                let branch = Branch {
                    id,
                    workspace,
                    name: existing.as_ref().map_or_else(
                        || {
                            reference
                                .metadata
                                .as_ref()
                                .map(|metadata| metadata.name.clone())
                                .unwrap_or_else(|| format!("adopted-{id}"))
                        },
                        |branch| branch.name.clone(),
                    ),
                    owner: reference.owner,
                    authority_epoch: reference.authority_epoch,
                    generation: reference.generation,
                    mutation_seq: reference.durable_seq,
                    saved_seq: reference.durable_seq,
                    source_generation: reference.source_generation,
                    formal_head: reference.formal_head,
                    autosave_head: reference.autosave_head,
                    working_root: reference.working_root.clone(),
                    base_revision: existing.as_ref().map_or_else(
                        || {
                            reference
                                .metadata
                                .as_ref()
                                .map_or(reference.formal_head, |metadata| metadata.base_revision)
                        },
                        |branch| branch.base_revision,
                    ),
                    source_branch: existing.as_ref().map_or_else(
                        || {
                            reference
                                .metadata
                                .as_ref()
                                .and_then(|metadata| metadata.source_branch)
                        },
                        |branch| branch.source_branch,
                    ),
                    next_inode,
                    dirty: formal.root != reference.working_root,
                    state: if reference.write_stopped {
                        BranchState::Stopped
                    } else {
                        BranchState::Writable
                    },
                    protected: existing.as_ref().is_some_and(|branch| branch.protected),
                };
                let mut commit = LocalCommit::default();
                if let Some(existing) = existing {
                    commit.guards.push(BranchGuard::from(&existing));
                    commit.branches.push(branch);
                } else {
                    commit.new_branches.push(branch);
                }
                self.runtime
                    .state
                    .commit(LocalCommit {
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
                        ..commit
                    })
                    .await?;
            }
            self.content.release_remote(workspace, pin).await?;
            result.push(reference);
        }
        Ok(result)
    }
}

#[async_trait]
impl ReplicaApi for ReplicaService {
    async fn sync(&self, context: RequestContext, request: SyncRequest) -> Result<OperationRecord> {
        self.load_workspace(&context.principal, request.workspace)
            .await?;
        let operation = match self
            .operations
            .start(
                context,
                "workspace_sync",
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
            let mut branches = Vec::new();
            if matches!(request.direction, SyncDirection::Push | SyncDirection::Both) {
                self.runtime
                    .authorize(
                        &operation.operation.principal,
                        request.workspace,
                        None,
                        Permission::Write,
                    )
                    .await?;
                if let Some(selected) = &request.branches {
                    for id in selected {
                        self.drain(request.workspace, Some(*id)).await?;
                        branches.push(*id);
                    }
                } else {
                    self.drain(request.workspace, None).await?;
                    branches.extend(
                        self.runtime
                            .state
                            .branches(request.workspace)
                            .await?
                            .into_iter()
                            .map(|branch| branch.id),
                    );
                }
            }
            if matches!(request.direction, SyncDirection::Pull | SyncDirection::Both) {
                branches.extend(
                    self.discover(request.workspace)
                        .await?
                        .into_iter()
                        .filter(|reference| {
                            request
                                .branches
                                .as_ref()
                                .is_none_or(|ids| ids.contains(&reference.branch))
                        })
                        .map(|reference| reference.branch),
                );
            }
            branches.sort();
            branches.dedup();
            self.operations
                .finish(
                    operation.clone(),
                    CommandResult::Synced { branches },
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
}

#[cfg(test)]
mod upload_tests {
    use super::*;
    use agentdrive_local::LocalBackend;
    use agentdrive_test_support::{ManualClock, MemoryRemote};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct ObservedRemote {
        remote: MemoryRemote,
        active: AtomicUsize,
        peak: AtomicUsize,
        completed: AtomicUsize,
        fail: AtomicBool,
        rendezvous: tokio::sync::Barrier,
    }
    struct Active<'a>(&'a AtomicUsize);
    impl Drop for Active<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    #[async_trait]
    impl RemoteObjectStore for ObservedRemote {
        async fn upload(&self, workspace: WorkspaceId, source: ObjectStream) -> Result<()> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            let _active = Active(&self.active);
            self.peak.fetch_max(active, Ordering::SeqCst);
            tokio::time::timeout(std::time::Duration::from_secs(30), self.rendezvous.wait())
                .await
                .expect("four uploads must reach the remote concurrently");
            self.completed.fetch_add(1, Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) {
                return Err(Error::new(
                    ErrorCode::Unavailable,
                    "injected upload failure",
                ));
            }
            self.remote.upload(workspace, source).await
        }
        async fn download(
            &self,
            workspace: WorkspaceId,
            object: &ObjectRef,
        ) -> Result<ObjectStream> {
            self.remote.download(workspace, object).await
        }
        async fn contains(&self, workspace: WorkspaceId, object: &ObjectRef) -> Result<bool> {
            self.remote.contains(workspace, object).await
        }
    }

    #[tokio::test]
    async fn uploads_share_a_limit_and_errors_release_all_permits() {
        let dir = tempfile::tempdir().unwrap();
        let local = Arc::new(LocalBackend::open(dir.path()).unwrap());
        let remote = Arc::new(ObservedRemote {
            remote: MemoryRemote::default(),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            completed: AtomicUsize::new(0),
            fail: AtomicBool::new(false),
            rendezvous: tokio::sync::Barrier::new(4),
        });
        let runtime = Runtime::new(local.clone(), Arc::new(ManualClock::default()))
            .await
            .unwrap();
        let content = ContentService::new(
            local.clone(),
            Some(remote.clone()),
            Some(Arc::new(MemoryRemote::default())),
        );
        let trees = TreeService::new(runtime.clone(), content.clone());
        let operations = OperationService::new(runtime.clone());
        let publisher = ReplicaService::new(runtime, trees, content, operations);
        let workspace = WorkspaceId::new();
        let mut roots = Vec::new();
        for i in 0..12 {
            roots.push(
                local
                    .put(workspace, ObjectKind::File, vec![i].into())
                    .await
                    .unwrap(),
            );
        }
        let (first, second) = tokio::join!(
            publisher.upload_closure(workspace, roots.clone()),
            publisher.upload_closure(workspace, roots.clone())
        );
        first.unwrap();
        second.unwrap();
        assert_eq!(remote.peak.load(Ordering::SeqCst), 4);
        assert_eq!(remote.active.load(Ordering::SeqCst), 0);
        let completed = remote.completed.load(Ordering::SeqCst);
        remote.fail.store(true, Ordering::SeqCst);
        assert!(
            publisher
                .upload_closure(workspace, roots.clone())
                .await
                .is_err()
        );
        assert_eq!(remote.active.load(Ordering::SeqCst), 0);
        assert_eq!(remote.completed.load(Ordering::SeqCst) - completed, 4);
        remote.fail.store(false, Ordering::SeqCst);
        publisher.upload_closure(workspace, roots).await.unwrap();
    }
}
