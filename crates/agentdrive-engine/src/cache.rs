use crate::{OperationService, Runtime};
use agentdrive_config::LiveConfig;
use agentdrive_model::*;
use agentdrive_ports::*;
use async_trait::async_trait;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

const MIN_FREE_BYTES: u64 = 64 * 1024 * 1024;

pub struct CacheService {
    runtime: Arc<Runtime>,
    trees: Arc<dyn RevisionReader>,
    graph: Arc<dyn ObjectGraph>,
    content: Arc<dyn ContentResolver>,
    objects: Arc<dyn LocalObjectStore>,
    operations: Arc<OperationService>,
    config: Arc<LiveConfig>,
    collecting: tokio::sync::Mutex<()>,
}

impl CacheService {
    pub fn new(
        runtime: Arc<Runtime>,
        trees: Arc<dyn RevisionReader>,
        graph: Arc<dyn ObjectGraph>,
        content: Arc<dyn ContentResolver>,
        objects: Arc<dyn LocalObjectStore>,
        operations: Arc<OperationService>,
        config: Arc<LiveConfig>,
    ) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            trees,
            graph,
            content,
            objects,
            operations,
            config,
            collecting: tokio::sync::Mutex::new(()),
        })
    }

    async fn selection(
        &self,
        range: &RevisionRange,
    ) -> Result<(Vec<WorkspacePath>, Vec<ObjectRef>)> {
        let revision = self.trees.revision(range.workspace, range.revision).await?;
        let tree = self.trees.tree(range.workspace, &revision.root).await?;
        let mut paths = range
            .paths
            .clone()
            .unwrap_or_else(|| vec![WorkspacePath::root()]);
        if paths.is_empty() {
            return Err(Error::invalid("revision paths must not be empty"));
        }
        for path in &mut paths {
            *path = tree
                .keys()
                .find(|entry| entry.lookup_key() == path.lookup_key())
                .cloned()
                .ok_or_else(|| Error::new(ErrorCode::NotFound, "revision path does not exist"))?;
        }
        paths.sort();
        paths.dedup();
        let mut objects: Vec<_> = self
            .graph
            .closure(range.workspace, vec![revision.root])
            .await?
            .into_iter()
            .filter(|object| object.kind != ObjectKind::File)
            .collect();
        objects.extend(
            tree.iter()
                .filter(|(path, _)| paths.iter().any(|selected| selected.contains(path)))
                .filter_map(|(_, inode)| inode.content.clone()),
        );
        objects.sort();
        objects.dedup();
        Ok((paths, objects))
    }

    async fn inspect(
        &self,
        range: &RevisionRange,
        paths: Vec<WorkspacePath>,
        objects: &[ObjectRef],
    ) -> Result<CacheStatus> {
        let pins = self.runtime.state.cache_pins(range.workspace).await?;
        let pinned: BTreeSet<_> = pins
            .iter()
            .flat_map(|pin| pin.objects.iter().map(|object| object.id.clone()))
            .collect();
        let mut missing = Vec::new();
        let mut completed_bytes = 0u64;
        for object in objects {
            if self.objects.contains(range.workspace, object).await? {
                completed_bytes = completed_bytes
                    .checked_add(object.size)
                    .ok_or_else(|| Error::integrity("cache byte count overflow"))?;
            } else {
                missing.push(object.id.clone());
            }
        }
        Ok(CacheStatus {
            revision: range.revision,
            paths,
            complete: missing.is_empty(),
            offline_ready: missing.is_empty()
                && objects.iter().all(|object| pinned.contains(&object.id)),
            completed_bytes,
            missing,
        })
    }

    async fn fetch(&self, range: &RevisionRange, objects: &[ObjectRef]) -> Result<()> {
        for object in objects {
            let _reader = self.content.open(range.workspace, object).await?;
        }
        Ok(())
    }
}

#[async_trait]
impl CacheApi for CacheService {
    async fn prefetch(
        &self,
        context: RequestContext,
        request: RevisionRange,
    ) -> Result<OperationRecord> {
        self.runtime
            .authorize(
                &context.principal,
                request.workspace,
                None,
                Permission::Read,
            )
            .await?;
        let operation = match self
            .operations
            .start(
                context,
                "revision_prefetch",
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
            let _protection = self.runtime.objects.read().await;
            let revision = self
                .trees
                .revision(request.workspace, request.revision)
                .await?;
            let (paths, objects) = self.selection(&request).await?;
            let cached = self.inspect(&request, paths.clone(), &objects).await?;
            let protected = self.content.remote_available() && !cached.complete;
            if protected {
                self.content
                    .protect_remote(
                        request.workspace,
                        operation.operation.id,
                        vec![revision.root],
                    )
                    .await?;
            }
            self.fetch(&request, &objects).await?;
            let status = self.inspect(&request, paths, &objects).await?;
            let record = self
                .operations
                .finish(
                    operation.clone(),
                    CommandResult::Prefetch { status },
                    LocalCommit::default(),
                )
                .await?;
            if protected {
                self.content
                    .release_remote(request.workspace, operation.operation.id)
                    .await?;
            }
            Ok(record)
        }
        .await;
        match result {
            Ok(record) => Ok(record),
            Err(error) => self.operations.fail(&operation, error).await,
        }
    }

    async fn pin(&self, context: RequestContext, request: PinRequest) -> Result<OperationRecord> {
        self.runtime
            .authorize(
                &context.principal,
                request.range.workspace,
                None,
                Permission::Read,
            )
            .await?;
        let operation = match self
            .operations
            .start(
                context,
                "cache_pin",
                &request,
                Some(request.range.workspace),
                None,
            )
            .await?
        {
            OperationStart::Existing(record) => return Ok(record),
            OperationStart::New(context) => context,
        };
        let result = async {
            let _protection = self.runtime.objects.read().await;
            let (paths, objects) = self.selection(&request.range).await?;
            if request.enabled {
                self.fetch(&request.range, &objects).await?;
            }
            let pin = CachePin {
                workspace: request.range.workspace,
                revision: request.range.revision,
                paths,
                objects,
            };
            self.operations
                .finish(
                    operation.clone(),
                    CommandResult::Updated,
                    LocalCommit {
                        pin_updates: vec![PinUpdate {
                            pin,
                            enabled: request.enabled,
                        }],
                        ..Default::default()
                    },
                )
                .await
        }
        .await;
        match result {
            Ok(record) => Ok(record),
            Err(error) => self.operations.fail(&operation, error).await,
        }
    }

    async fn status(&self, actor: &Principal, request: RevisionRange) -> Result<CacheStatus> {
        self.runtime
            .authorize(actor, request.workspace, None, Permission::Read)
            .await?;
        let _protection = self.runtime.objects.read().await;
        let (paths, objects) = self.selection(&request).await?;
        self.inspect(&request, paths, &objects).await
    }

    async fn collect(&self) -> Result<u64> {
        let Ok(_collection) = self.collecting.try_lock() else {
            return Ok(0);
        };
        let roots = RetentionRoots::from_state(self.runtime.state.retention_state().await?);
        let candidates = self.collection_candidates(&roots).await?;
        let mut reclaimed = 0;
        for batch in candidates.chunks(32) {
            // Do not queue an exclusive lock ahead of active checkpoints.
            let Ok(exclusive) = self.runtime.objects.try_write() else {
                break;
            };
            let current = RetentionRoots::from_state(self.runtime.state.retention_state().await?);
            if current != roots
                || (batch.iter().any(|entry| entry.remote_confirmed)
                    && !self.content.remote_available())
            {
                break;
            }
            reclaimed += self.objects.remove_batch(batch.to_vec()).await?;
            drop(exclusive);
            tokio::task::yield_now().await;
        }
        Ok(reclaimed)
    }
}

impl std::fmt::Debug for CacheService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheService").finish_non_exhaustive()
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct WorkspaceRoots {
    current: BTreeSet<ObjectRef>,
    histories: BTreeSet<ObjectRef>,
    revisions: BTreeSet<ObjectRef>,
    candidates: BTreeSet<RevisionId>,
    pins: BTreeSet<ObjectId>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct RetentionRoots(BTreeMap<WorkspaceId, WorkspaceRoots>);

impl RetentionRoots {
    fn from_state(state: RetentionState) -> Self {
        let mut result = Self::default();
        for workspace in state.workspaces {
            result.0.entry(workspace).or_default();
        }
        let histories: BTreeMap<_, _> = state.histories.into_iter().collect();
        for branch in state.branches {
            if branch.state == BranchState::Deleted {
                continue;
            }
            let roots = result.0.entry(branch.workspace).or_default();
            roots.current.insert(branch.working_root);
            if let Some(history) = histories.get(&branch.id) {
                roots.histories.insert(history.clone());
            }
        }
        for job in state.jobs {
            result
                .0
                .entry(job.reference.workspace)
                .or_default()
                .current
                .extend([job.reference.working_root, job.reference.record_index]);
        }
        for revision in state.revisions {
            result
                .0
                .entry(revision.workspace)
                .or_default()
                .revisions
                .insert(revision.root);
        }
        for candidate in state.merges {
            if candidate.applied_revision.is_none() {
                result
                    .0
                    .entry(candidate.workspace)
                    .or_default()
                    .candidates
                    .insert(candidate.revision);
            }
        }
        for pin in state.pins {
            result
                .0
                .entry(pin.workspace)
                .or_default()
                .pins
                .extend(pin.objects.into_iter().map(|object| object.id));
        }
        result
    }
}

impl CacheService {
    async fn collection_candidates(&self, roots: &RetentionRoots) -> Result<Vec<CachedObject>> {
        let mut unreachable = Vec::new();
        let mut evictable = Vec::new();
        let mut total = 0u64;
        for (workspace, roots) in &roots.0 {
            let mut protected = roots.pins.clone();
            let mut current = roots.current.clone();
            for candidate in &roots.candidates {
                current.insert(self.trees.revision(*workspace, *candidate).await?.root);
            }
            for index in &roots.histories {
                protected.extend(
                    self.graph
                        .index_pages(*workspace, index.clone())
                        .await?
                        .into_iter()
                        .map(|object| object.id),
                );
            }
            protected.extend(
                self.graph
                    .closure(*workspace, current.into_iter().collect())
                    .await?
                    .into_iter()
                    .map(|object| object.id),
            );
            protected.extend(roots.revisions.iter().map(|object| object.id.clone()));
            let cached = self.objects.objects(*workspace).await?;
            let mut retained = protected.clone();
            if cached.iter().any(|object| !object.remote_confirmed) {
                retained.extend(
                    self.graph
                        .closure(*workspace, roots.revisions.iter().cloned().collect())
                        .await?
                        .into_iter()
                        .map(|object| object.id),
                );
            }
            for entry in cached {
                if !entry.remote_confirmed && !retained.contains(&entry.reference.id) {
                    unreachable.push(entry);
                } else {
                    total = total
                        .checked_add(entry.reference.size)
                        .ok_or_else(|| Error::integrity("cache size overflow"))?;
                    if entry.remote_confirmed && !protected.contains(&entry.reference.id) {
                        evictable.push(entry);
                    }
                }
            }
        }
        if !self.content.remote_available() {
            return Ok(unreachable);
        }
        let stats = self.objects.stats().await?;
        let config = self.config.load();
        let limit = config.values.cache.max_bytes;
        if total <= limit && stats.available_bytes >= MIN_FREE_BYTES {
            return Ok(unreachable);
        }
        let target = limit - limit.div_ceil(4);
        let mut free = stats.available_bytes;
        evictable.sort_by_key(|entry| entry.last_access_ns);
        for entry in evictable {
            if total <= target && free >= MIN_FREE_BYTES {
                break;
            }
            total = total.saturating_sub(entry.reference.size);
            free = free.saturating_add(entry.reference.size);
            unreachable.push(entry);
        }
        Ok(unreachable)
    }
}

#[cfg(test)]
mod retention_tests {
    use super::*;
    use crate::{ContentService, TreeService};
    use agentdrive_config::RuntimeConfig;
    use agentdrive_local::LocalBackend;
    use agentdrive_test_support::ManualClock;
    use bytes::Bytes;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::Notify;

    struct PausingGraph {
        graph: Arc<dyn ObjectGraph>,
        pause: AtomicBool,
        entered: Notify,
        proceed: Notify,
    }
    #[async_trait]
    impl ObjectGraph for PausingGraph {
        async fn closure(
            &self,
            workspace: WorkspaceId,
            roots: Vec<ObjectRef>,
        ) -> Result<Vec<ObjectRef>> {
            if self.pause.swap(false, Ordering::SeqCst) {
                self.entered.notify_one();
                self.proceed.notified().await;
            }
            self.graph.closure(workspace, roots).await
        }
        async fn index_pages(
            &self,
            workspace: WorkspaceId,
            root: ObjectRef,
        ) -> Result<Vec<ObjectRef>> {
            self.graph.index_pages(workspace, root).await
        }
    }

    #[tokio::test]
    async fn traversal_releases_retention_barrier_and_new_pins_cancel_deletion() {
        let directory = tempfile::tempdir().unwrap();
        let local = Arc::new(LocalBackend::open(directory.path()).unwrap());
        let runtime = Runtime::new(local.clone(), Arc::new(ManualClock::default()))
            .await
            .unwrap();
        let content = ContentService::new(local.clone(), None, None);
        let trees = TreeService::new(runtime.clone(), content.clone());
        let workspace = WorkspaceId::new();
        let revision = RevisionId::new();
        let actor: Principal = "owner".to_owned().try_into().unwrap();
        let tree = trees
            .save_tree(
                workspace,
                &BTreeMap::from([(
                    WorkspacePath::root(),
                    Inode {
                        id: InodeId::ROOT,
                        kind: FileKind::Directory,
                        mode: 0o755,
                        uid: 1000,
                        gid: 1000,
                        size: 0,
                        created_ns: 0,
                        modified_ns: 0,
                        content: None,
                    },
                )]),
            )
            .await
            .unwrap();
        local
            .commit(LocalCommit {
                new_workspaces: vec![Workspace {
                    id: workspace,
                    name: "retention".into(),
                    owner: actor.clone(),
                    initial_revision: revision,
                    format_version: FORMAT_VERSION,
                    name_policy: "portable_v1".into(),
                    grants: vec![],
                    max_file_bytes: 1024,
                    max_working_bytes: 4096,
                    max_inodes: 100,
                }],
                revisions: vec![Revision {
                    message: None,
                    id: revision,
                    workspace,
                    branch: None,
                    parents: vec![],
                    root: tree,
                    kind: RevisionKind::Commit,
                    actor,
                    created_ns: 0,
                    operation: OperationId::new(),
                }],
                ..Default::default()
            })
            .await
            .unwrap();
        let orphan = local
            .put(
                workspace,
                ObjectKind::File,
                Bytes::from_static(b"newly pinned"),
            )
            .await
            .unwrap();
        let graph = Arc::new(PausingGraph {
            graph: trees.clone(),
            pause: AtomicBool::new(true),
            entered: Notify::new(),
            proceed: Notify::new(),
        });
        let cache = CacheService::new(
            runtime.clone(),
            trees,
            graph.clone(),
            content,
            local.clone(),
            OperationService::new(runtime.clone()),
            LiveConfig::new(RuntimeConfig::default()).unwrap(),
        );
        let task = tokio::spawn({
            let cache = cache.clone();
            async move { cache.collect().await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), graph.entered.notified())
            .await
            .unwrap();
        let guard = tokio::time::timeout(std::time::Duration::from_secs(5), runtime.objects.read())
            .await
            .expect("graph traversal must not hold the exclusive object barrier");
        let pin = CachePin {
            workspace,
            revision,
            paths: vec![WorkspacePath::root()],
            objects: vec![orphan.clone()],
        };
        local
            .commit(LocalCommit {
                pin_updates: vec![PinUpdate {
                    pin: pin.clone(),
                    enabled: true,
                }],
                ..Default::default()
            })
            .await
            .unwrap();
        drop(guard);
        graph.proceed.notify_one();
        assert_eq!(task.await.unwrap().unwrap(), 0);
        assert!(local.contains(workspace, &orphan).await.unwrap());
        assert_eq!(cache.collect().await.unwrap(), 0);
        local
            .commit(LocalCommit {
                pin_updates: vec![PinUpdate {
                    pin,
                    enabled: false,
                }],
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(cache.collect().await.unwrap(), orphan.size);
        assert!(!local.contains(workspace, &orphan).await.unwrap());
    }
}
