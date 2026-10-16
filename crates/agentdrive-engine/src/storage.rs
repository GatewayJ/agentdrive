use crate::{ContentService, Engine, RemoteStorage, replica::decode_branch_ref};
use agentdrive_model::*;
use agentdrive_ports::*;
use std::{collections::BTreeMap, sync::Arc};

impl Engine {
    /// 准备全部历史内容后保存配置，并在持有保护锁期间更换远程适配器。
    pub async fn switch_storage(
        &self,
        next: Option<RemoteStorage>,
        persist: &mut dyn FnMut() -> Result<()>,
    ) -> Result<()> {
        let _lifecycle = self.runtime.lifecycle.lock().await;
        let _publication = self.publisher.publication.lock().await;
        let _objects = self.runtime.objects.write().await;
        let trees = &self.revisions.trees;
        let content = &trees.content;
        let mut retained = Vec::new();
        for workspace in self.runtime.state.workspaces().await? {
            if self
                .runtime
                .state
                .operations(workspace.id)
                .await?
                .iter()
                .any(|record| matches!(record.pending, Some(PendingAction::Transfer { .. })))
            {
                return Err(Error::new(
                    ErrorCode::Busy,
                    "complete the pending ownership transfer before changing storage",
                ));
            }
            let mut roots: Vec<_> = self
                .runtime
                .state
                .revisions(workspace.id)
                .await?
                .into_iter()
                .map(|revision| revision.root)
                .collect();
            roots.extend(
                self.runtime
                    .state
                    .branches(workspace.id)
                    .await?
                    .into_iter()
                    .map(|branch| branch.working_root),
            );
            let mut references = BTreeMap::<BranchId, BranchRef>::new();
            for reference in self
                .runtime
                .state
                .remote_refs(workspace.id)
                .await?
                .into_iter()
                .chain(
                    self.runtime
                        .state
                        .sync_jobs(workspace.id)
                        .await?
                        .into_iter()
                        .map(|job| job.reference),
                )
            {
                roots.extend([
                    reference.working_root.clone(),
                    reference.record_index.clone(),
                ]);
                let selected = references
                    .entry(reference.branch)
                    .or_insert_with(|| reference.clone());
                if (reference.authority_epoch, reference.source_generation)
                    > (selected.authority_epoch, selected.source_generation)
                {
                    *selected = reference;
                }
            }
            for pin in self.runtime.state.cache_pins(workspace.id).await? {
                roots.extend(pin.objects);
            }
            let objects = trees.closure(workspace.id, roots.clone()).await?;
            for object in &objects {
                // install 会检查下载内容的大小与摘要。
                let _reader = content.open(workspace.id, object).await?;
            }
            retained.push((workspace, roots, objects, references));
        }
        if let Some(next) = &next {
            let destination = ContentService::new(
                content.local.clone(),
                Some(next.objects.clone()),
                Some(next.refs.clone()),
            );
            let pin: OperationId = self.runtime.location.to_string().parse()?;
            for (workspace, roots, objects, references) in &retained {
                let initial = trees
                    .revision(workspace.id, workspace.initial_revision)
                    .await?;
                for object in trees
                    .closure(workspace.id, vec![initial.root.clone()])
                    .await?
                {
                    next.objects
                        .upload(
                            workspace.id,
                            content.local.open(workspace.id, &object).await?,
                        )
                        .await?;
                }
                publish_workspace(next, workspace.clone(), initial).await?;
                destination
                    .protect_remote(workspace.id, pin, roots.clone())
                    .await?;
                let result: Result<()> = async {
                    for object in objects {
                        next.objects
                            .upload(
                                workspace.id,
                                content.local.open(workspace.id, object).await?,
                            )
                            .await?;
                    }
                    for reference in references.values() {
                        publish_branch(next, reference, self.runtime.location).await?;
                    }
                    Ok(())
                }
                .await;
                let release = destination.release_remote(workspace.id, pin).await;
                result?;
                release?;
            }
        }
        persist()?;
        let enabled = next.is_some();
        content.storage.store(next.map(Arc::new));
        if enabled {
            for (workspace, _, objects, _) in retained {
                for object in objects {
                    if let Err(error) = content.local.confirm_remote(workspace.id, &object).await {
                        tracing::warn!(%error, "storage applied; local cache confirmation will require a later upload");
                    }
                }
            }
        }
        Ok(())
    }
}

async fn publish_workspace(
    storage: &RemoteStorage,
    workspace: Workspace,
    initial_revision: Revision,
) -> Result<()> {
    let key = RefKey::Workspace(workspace.id);
    for _ in 0..8 {
        if let Some(existing) = storage.refs.get(&key).await? {
            if existing.bytes.len() > MAX_METADATA_BYTES {
                return Err(Error::integrity("workspace control exceeds format limit"));
            }
            let control: WorkspaceControl = decode(&existing.bytes)?;
            if control.workspace != workspace || control.initial_revision != initial_revision {
                return Err(Error::integrity(
                    "destination workspace identity or policy differs",
                ));
            }
            if control.phase != GcPhase::Open {
                return Err(Error::new(
                    ErrorCode::Busy,
                    "destination reclamation is active",
                ));
            }
            return Ok(());
        }
        let control = WorkspaceControl {
            workspace: workspace.clone(),
            initial_revision: initial_revision.clone(),
            gc_epoch: 0,
            phase: GcPhase::Open,
            active_operations: BTreeMap::new(),
            history_pins: BTreeMap::new(),
        };
        storage
            .refs
            .compare_exchange(RefUpdate {
                key: key.clone(),
                expected_version: None,
                bytes: encode(&control)?.into(),
            })
            .await?;
    }
    Err(Error::new(
        ErrorCode::RemoteUnknown,
        "destination workspace publication remains unconfirmed",
    )
    .retryable())
}

async fn publish_branch(
    storage: &RemoteStorage,
    reference: &BranchRef,
    location: LocationId,
) -> Result<()> {
    let key = RefKey::Branch {
        workspace: reference.workspace,
        branch: reference.branch,
    };
    for _ in 0..8 {
        let current = storage.refs.get(&key).await?;
        if let Some(current) = &current {
            let remote = decode_branch_ref(reference.workspace, reference.branch, &current.bytes)?;
            if remote == *reference {
                return Ok(());
            }
            if reference.owner != location {
                return Ok(());
            }
            if remote.owner != reference.owner
                || remote.authority_epoch != reference.authority_epoch
            {
                return Err(Error::new(
                    ErrorCode::StaleAuthority,
                    "destination branch ownership differs; synchronize before changing storage",
                ));
            }
            if remote.source_generation >= reference.source_generation {
                return Err(Error::new(
                    ErrorCode::TargetMoved,
                    "destination branch is ahead; synchronize before changing storage",
                ));
            }
        }
        storage
            .refs
            .compare_exchange(RefUpdate {
                key: key.clone(),
                expected_version: current.map(|value| value.version),
                bytes: encode(reference)?.into(),
            })
            .await?;
    }
    Err(Error::new(
        ErrorCode::RemoteUnknown,
        "destination branch publication remains unconfirmed",
    )
    .retryable())
}
