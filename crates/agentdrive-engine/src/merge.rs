use crate::{
    OperationService, RevisionService, Runtime, revisions::same_entry, runtime::check_guard,
};
use agentdrive_model::*;
use agentdrive_ports::*;
use async_trait::async_trait;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use tokio::sync::Mutex;

pub struct MergeService {
    runtime: Arc<Runtime>,
    revisions: Arc<RevisionService>,
    operations: Arc<OperationService>,
    transitions: Arc<dyn RevisionTransition>,
    candidates: Mutex<()>,
}

impl std::fmt::Debug for MergeService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MergeService").finish_non_exhaustive()
    }
}

impl MergeService {
    pub fn new(
        runtime: Arc<Runtime>,
        revisions: Arc<RevisionService>,
        operations: Arc<OperationService>,
        transitions: Arc<dyn RevisionTransition>,
    ) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            revisions,
            operations,
            transitions,
            candidates: Mutex::new(()),
        })
    }

    async fn candidate(&self, actor: &Principal, id: MergeId) -> Result<MergeCandidate> {
        let candidate = self
            .runtime
            .state
            .merge(id)
            .await?
            .ok_or_else(|| Error::new(ErrorCode::NotFound, "merge candidate does not exist"))?;
        self.runtime
            .authorize(
                actor,
                candidate.workspace,
                Some(candidate.target.branch),
                Permission::Merge,
            )
            .await?;
        Ok(candidate)
    }

    async fn common_base(
        &self,
        workspace: WorkspaceId,
        source: RevisionId,
        target: RevisionId,
        explicit: Option<RevisionId>,
    ) -> Result<Option<RevisionId>> {
        let revisions: BTreeMap<_, _> = self
            .runtime
            .state
            .revisions(workspace)
            .await?
            .into_iter()
            .map(|revision| (revision.id, revision))
            .collect();
        let source_ancestors = ancestors(source, &revisions)?;
        let target_ancestors = ancestors(target, &revisions)?;
        if target_ancestors.contains(&source) {
            return Ok(None);
        }
        let common: BTreeSet<_> = source_ancestors
            .intersection(&target_ancestors)
            .copied()
            .collect();
        if common.is_empty() {
            return Err(Error::new(
                ErrorCode::NoCommonBase,
                "revisions do not share a known ancestor",
            ));
        }
        let mut dominated = BTreeSet::new();
        let mut pending: Vec<_> = common
            .iter()
            .flat_map(|id| revisions[id].parents.iter().copied())
            .collect();
        while let Some(id) = pending.pop() {
            if dominated.insert(id) {
                pending.extend(
                    revisions
                        .get(&id)
                        .ok_or_else(|| {
                            Error::new(ErrorCode::BaseUnavailable, "ancestor history is incomplete")
                        })?
                        .parents
                        .iter()
                        .copied(),
                );
            }
        }
        let best: Vec<_> = common.difference(&dominated).copied().collect();
        if let Some(explicit) = explicit {
            if !best.contains(&explicit) {
                return Err(Error::invalid(
                    "explicit base must be a nearest common ancestor",
                ));
            }
            return Ok(Some(explicit));
        }
        if best.len() != 1 {
            return Err(Error::new(
                ErrorCode::AmbiguousBase,
                "multiple nearest common ancestors require an explicit base",
            ));
        }
        Ok(Some(best[0]))
    }

    async fn prepare_candidate(
        &self,
        operation: OperationContext,
        request: PrepareMerge,
    ) -> Result<OperationRecord> {
        let _gate = self.runtime.lock_branch(request.target.branch).await;
        let _protection = self.runtime.objects.read().await;
        let branch = self.runtime.branch(request.target.branch).await?;
        check_guard(&branch, &request.target)?;
        if branch.workspace != request.workspace {
            return Err(Error::invalid("target branch belongs to another workspace"));
        }
        if branch.owner != self.runtime.location || branch.state != BranchState::Writable {
            return Err(Error::new(
                ErrorCode::CoordinatorRequired,
                "merge must run on the target owner's service",
            ));
        }
        if branch.dirty {
            return Err(Error::new(
                ErrorCode::DirtyWorktree,
                "merge target has uncommitted changes",
            ));
        }
        let Some(base_id) = self
            .common_base(
                request.workspace,
                request.source,
                branch.formal_head,
                request.base,
            )
            .await?
        else {
            return self
                .operations
                .finish(
                    operation,
                    CommandResult::Revision {
                        revision: branch.formal_head,
                        branch: branch.id,
                    },
                    LocalCommit::default(),
                )
                .await;
        };
        let base = self
            .revisions
            .trees
            .revision(branch.workspace, base_id)
            .await?;
        let source = self
            .revisions
            .trees
            .revision(branch.workspace, request.source)
            .await?;
        let target = self
            .revisions
            .trees
            .revision(branch.workspace, branch.formal_head)
            .await?;
        let base_tree = self
            .revisions
            .trees
            .tree(branch.workspace, &base.root)
            .await?;
        let source_tree = self
            .revisions
            .trees
            .tree(branch.workspace, &source.root)
            .await?;
        let target_tree = self
            .revisions
            .trees
            .tree(branch.workspace, &target.root)
            .await?;
        let (tree, conflicts) = merge_trees(&base_tree, &source_tree, &target_tree)?;
        let root = self
            .revisions
            .trees
            .save_tree(branch.workspace, &tree)
            .await?;
        let revision = Revision {
            message: None,
            id: RevisionId::new(),
            workspace: branch.workspace,
            branch: None,
            parents: vec![branch.formal_head, source.id],
            root,
            kind: RevisionKind::Candidate,
            actor: operation.operation.principal.clone(),
            created_ns: self.runtime.clock.now_ns(),
            operation: operation.operation.id,
        };
        let candidate = MergeCandidate {
            id: MergeId::new(),
            workspace: branch.workspace,
            base: base_id,
            source: source.id,
            target: request.target,
            revision: revision.id,
            conflicts,
            applied_revision: None,
        };
        self.operations
            .finish(
                operation,
                CommandResult::Merge {
                    candidate: candidate.clone(),
                },
                LocalCommit {
                    revisions: vec![revision],
                    merges: vec![candidate],
                    ..Default::default()
                },
            )
            .await
    }
}

fn ancestors(
    start: RevisionId,
    revisions: &BTreeMap<RevisionId, Revision>,
) -> Result<BTreeSet<RevisionId>> {
    let mut visiting = BTreeSet::new();
    let mut complete = BTreeSet::new();
    let mut pending = vec![(start, false)];
    while let Some((id, leaving)) = pending.pop() {
        if leaving {
            visiting.remove(&id);
            complete.insert(id);
            continue;
        }
        if complete.contains(&id) {
            continue;
        }
        if !visiting.insert(id) {
            return Err(Error::integrity("revision history contains a cycle"));
        }
        let revision = revisions.get(&id).ok_or_else(|| {
            Error::new(ErrorCode::BaseUnavailable, "ancestor history is incomplete")
        })?;
        pending.push((id, true));
        pending.extend(revision.parents.iter().map(|parent| (*parent, false)));
    }
    Ok(complete)
}

type NamedTree = BTreeMap<String, (WorkspacePath, Inode)>;
fn named_tree(tree: &FileTree) -> NamedTree {
    tree.iter()
        .map(|(path, inode)| (path.lookup_key(), (path.clone(), inode.clone())))
        .collect()
}
fn same_named(
    left: Option<&(WorkspacePath, Inode)>,
    right: Option<&(WorkspacePath, Inode)>,
) -> bool {
    match (left, right) {
        (Some((a, left)), Some((b, right))) => a == b && same_entry(Some(left), Some(right)),
        (None, None) => true,
        _ => false,
    }
}
fn conflict(
    path: WorkspacePath,
    key: &str,
    base: &NamedTree,
    source: &NamedTree,
    target: &NamedTree,
) -> MergeConflict {
    MergeConflict {
        path,
        base: base.get(key).map(|(_, inode)| inode.clone()),
        source: source.get(key).map(|(_, inode)| inode.clone()),
        target: target.get(key).map(|(_, inode)| inode.clone()),
    }
}
fn replace_subtree(tree: &mut NamedTree, path: &WorkspacePath, source: &NamedTree) {
    tree.retain(|_, (entry, _)| !path.contains(entry));
    tree.extend(
        source
            .iter()
            .filter(|(_, (entry, _))| path.contains(entry))
            .map(|(key, entry)| (key.clone(), entry.clone())),
    );
}

fn canonical_tree(tree: NamedTree) -> Result<FileTree> {
    let mut result = FileTree::new();
    let mut names = BTreeMap::<String, WorkspacePath>::new();
    let mut next_inode = 1u64;
    for (key, (path, mut inode)) in tree {
        let path = if let Some(parent) = path.parent() {
            names
                .get(&parent.lookup_key())
                .ok_or_else(|| Error::integrity("merged tree has a missing parent"))?
                .join(path.name())?
        } else {
            WorkspacePath::root()
        };
        inode.id = InodeId(next_inode);
        next_inode = crate::runtime::increment(next_inode)?;
        names.insert(key, path.clone());
        result.insert(path, inode);
    }
    Ok(result)
}

fn merge_trees(
    base: &FileTree,
    source: &FileTree,
    target: &FileTree,
) -> Result<(FileTree, Vec<MergeConflict>)> {
    let base = named_tree(base);
    let source = named_tree(source);
    let target = named_tree(target);
    let keys: BTreeSet<_> = base
        .keys()
        .chain(source.keys())
        .chain(target.keys())
        .cloned()
        .collect();
    let mut tree = NamedTree::new();
    let mut conflicts = Vec::new();
    for key in keys {
        let a = base.get(&key);
        let b = source.get(&key);
        let c = target.get(&key);
        let chosen = if same_named(b, a) {
            c
        } else if same_named(c, a) {
            b
        } else if same_named(b, c) {
            c
        } else {
            let path = c
                .or(b)
                .or(a)
                .ok_or_else(|| Error::integrity("merge entry disappeared"))?
                .0
                .clone();
            conflicts.push(conflict(path, &key, &base, &source, &target));
            c
        };
        if let Some(entry) = chosen {
            tree.insert(key, entry.clone());
        }
    }
    for entry in &conflicts {
        replace_subtree(&mut tree, &entry.path, &target);
    }
    loop {
        let invalid = tree.values().find_map(|(path, _)| {
            path.parent().filter(|parent| {
                tree.get(&parent.lookup_key())
                    .is_none_or(|(_, inode)| inode.kind != FileKind::Directory)
            })
        });
        let Some(parent) = invalid else {
            break;
        };
        let key = parent.lookup_key();
        conflicts.push(conflict(parent.clone(), &key, &base, &source, &target));
        replace_subtree(&mut tree, &parent, &target);
    }
    conflicts.sort_by_key(|entry| {
        (
            entry.path.as_str().matches('/').count(),
            entry.path.lookup_key(),
        )
    });
    let mut minimal = Vec::<MergeConflict>::new();
    for entry in conflicts {
        if !minimal
            .iter()
            .any(|parent| parent.path.contains(&entry.path))
        {
            minimal.push(entry);
        }
    }
    Ok((canonical_tree(tree)?, minimal))
}

#[async_trait]
impl MergeApi for MergeService {
    async fn prepare(
        &self,
        context: RequestContext,
        request: PrepareMerge,
    ) -> Result<OperationRecord> {
        self.runtime
            .authorize(
                &context.principal,
                request.workspace,
                Some(request.target.branch),
                Permission::Merge,
            )
            .await?;
        let operation = match self
            .operations
            .start(
                context,
                "merge_prepare",
                &request,
                Some(request.workspace),
                Some(request.target.branch),
            )
            .await?
        {
            OperationStart::Existing(record) => return Ok(record),
            OperationStart::New(context) => context,
        };
        match self.prepare_candidate(operation.clone(), request).await {
            Ok(record) => Ok(record),
            Err(error) => self.operations.fail(&operation, error).await,
        }
    }

    async fn get(&self, actor: &Principal, merge: MergeId) -> Result<MergeCandidate> {
        self.candidate(actor, merge).await
    }

    async fn resolve(
        &self,
        context: RequestContext,
        request: ResolveMerge,
    ) -> Result<OperationRecord> {
        let _candidate_gate = self.candidates.lock().await;
        let mut candidate = self.candidate(&context.principal, request.merge).await?;
        let operation = match self
            .operations
            .start(
                context,
                "merge_resolve",
                &request,
                Some(candidate.workspace),
                Some(candidate.target.branch),
            )
            .await?
        {
            OperationStart::Existing(record) => return Ok(record),
            OperationStart::New(context) => context,
        };
        let result = async {
            if candidate.revision != request.candidate || candidate.applied_revision.is_some() {
                return Err(Error::new(
                    ErrorCode::CandidateMoved,
                    "merge candidate changed or was applied",
                ));
            }
            if request.resolutions.is_empty() {
                return Err(Error::invalid("provide at least one file edit"));
            }
            let _protection = self.runtime.objects.read().await;
            let revision = self
                .revisions
                .trees
                .revision(candidate.workspace, candidate.revision)
                .await?;
            let mut tree = named_tree(
                &self
                    .revisions
                    .trees
                    .tree(candidate.workspace, &revision.root)
                    .await?,
            );
            let source = self
                .revisions
                .trees
                .revision(candidate.workspace, candidate.source)
                .await?;
            let target = self
                .revisions
                .trees
                .revision(candidate.workspace, candidate.target.expected_head)
                .await?;
            let source = named_tree(
                &self
                    .revisions
                    .trees
                    .tree(candidate.workspace, &source.root)
                    .await?,
            );
            let target = named_tree(
                &self
                    .revisions
                    .trees
                    .tree(candidate.workspace, &target.root)
                    .await?,
            );
            for resolution in request.resolutions {
                let selected = resolution.path().clone();
                if candidate.conflicts.iter().any(|conflict| {
                    conflict.path.contains(&selected)
                        && conflict.path.lookup_key() != selected.lookup_key()
                }) {
                    return Err(Error::new(
                        ErrorCode::UnresolvedConflict,
                        "resolve the parent conflict before editing its children",
                    ));
                }
                candidate
                    .conflicts
                    .retain(|conflict| !selected.contains(&conflict.path));
                match resolution {
                    Resolution::Source { .. } => replace_subtree(&mut tree, &selected, &source),
                    Resolution::Target { .. } => replace_subtree(&mut tree, &selected, &target),
                    Resolution::Delete { .. } => {
                        if selected.is_root() {
                            return Err(Error::invalid("cannot delete the workspace root"));
                        }
                        tree.retain(|_, (path, _)| !selected.contains(path));
                    }
                    Resolution::Copy {
                        revision,
                        source_path,
                        ..
                    } => {
                        if selected.is_root() {
                            return Err(Error::invalid(
                                "cannot replace the workspace root with a file",
                            ));
                        }
                        self.runtime
                            .authorize(
                                &operation.operation.principal,
                                candidate.workspace,
                                None,
                                Permission::Read,
                            )
                            .await?;
                        let source_revision = self
                            .revisions
                            .trees
                            .revision(candidate.workspace, revision)
                            .await?;
                        let source_tree = self
                            .revisions
                            .trees
                            .tree(candidate.workspace, &source_revision.root)
                            .await?;
                        let inode = source_tree
                            .iter()
                            .find(|(path, _)| path.lookup_key() == source_path.lookup_key())
                            .map(|(_, inode)| inode.clone())
                            .ok_or_else(|| {
                                Error::new(ErrorCode::NotFound, "copy source does not exist")
                            })?;
                        if inode.kind != FileKind::File {
                            return Err(Error::new(
                                ErrorCode::IsDirectory,
                                "copy source must be a file",
                            ));
                        }
                        tree.retain(|_, (path, _)| !selected.contains(path));
                        tree.insert(selected.lookup_key(), (selected, inode));
                    }
                    Resolution::Replace { content, mode, .. } => {
                        if selected.is_root() || content.len() > 1024 * 1024 || mode & !0o777 != 0 {
                            return Err(Error::invalid("invalid replacement file, mode or size"));
                        }
                        let object = self
                            .revisions
                            .trees
                            .content
                            .local
                            .put(candidate.workspace, ObjectKind::File, content.into())
                            .await?;
                        tree.retain(|_, (path, _)| !selected.contains(path));
                        let now = self.runtime.clock.now_ns();
                        tree.insert(
                            selected.lookup_key(),
                            (
                                selected,
                                Inode {
                                    id: InodeId(0),
                                    kind: FileKind::File,
                                    mode,
                                    uid: 0,
                                    gid: 0,
                                    size: object.size,
                                    created_ns: now,
                                    modified_ns: now,
                                    content: Some(object),
                                },
                            ),
                        );
                    }
                }
            }
            let tree = canonical_tree(tree)?;
            let root = self
                .revisions
                .trees
                .save_tree(candidate.workspace, &tree)
                .await?;
            let revision = Revision {
                message: None,
                id: RevisionId::new(),
                workspace: candidate.workspace,
                branch: None,
                parents: vec![candidate.revision],
                root,
                kind: RevisionKind::Candidate,
                actor: operation.operation.principal.clone(),
                created_ns: self.runtime.clock.now_ns(),
                operation: operation.operation.id,
            };
            candidate.revision = revision.id;
            self.operations
                .finish(
                    operation.clone(),
                    CommandResult::Merge {
                        candidate: candidate.clone(),
                    },
                    LocalCommit {
                        revisions: vec![revision],
                        merges: vec![candidate],
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

    async fn apply(&self, context: RequestContext, request: ApplyMerge) -> Result<OperationRecord> {
        let _candidate_gate = self.candidates.lock().await;
        let candidate = self.candidate(&context.principal, request.merge).await?;
        let operation = match self
            .operations
            .start(
                context,
                "merge_apply",
                &request,
                Some(candidate.workspace),
                Some(candidate.target.branch),
            )
            .await?
        {
            OperationStart::Existing(record) => return Ok(record),
            OperationStart::New(context) => context,
        };
        let result = async {
            if candidate.revision != request.candidate || candidate.target != request.target {
                return Err(Error::new(
                    ErrorCode::CandidateMoved,
                    "merge candidate or target guard changed",
                ));
            }
            if let Some(revision) = candidate.applied_revision {
                return self
                    .operations
                    .finish(
                        operation.clone(),
                        CommandResult::Revision {
                            revision,
                            branch: candidate.target.branch,
                        },
                        LocalCommit::default(),
                    )
                    .await;
            }
            if !candidate.conflicts.is_empty() {
                return Err(Error::new(
                    ErrorCode::UnresolvedConflict,
                    "merge has unresolved conflicts",
                ));
            }
            self.transitions
                .replace_revision(
                    operation.clone(),
                    candidate.target,
                    candidate.revision,
                    Some(candidate.id),
                    request.durability,
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
mod tests {
    use super::*;
    fn directory(id: u64) -> Inode {
        Inode {
            id: InodeId(id),
            kind: FileKind::Directory,
            mode: 0o755,
            uid: 0,
            gid: 0,
            size: 0,
            created_ns: 0,
            modified_ns: 0,
            content: None,
        }
    }
    fn file(id: u64, data: &[u8]) -> Inode {
        let object = object_ref(ObjectKind::File, data);
        Inode {
            kind: FileKind::File,
            mode: 0o644,
            size: object.size,
            content: Some(object),
            ..directory(id)
        }
    }
    fn path(value: &str) -> WorkspacePath {
        value.to_owned().try_into().unwrap()
    }

    #[test]
    fn independent_file_changes_merge_without_inode_collisions() {
        let base = FileTree::from([(path("/"), directory(1))]);
        let mut source = base.clone();
        source.insert(path("/a"), file(2, b"a"));
        let mut target = base.clone();
        target.insert(path("/b"), file(2, b"b"));
        let (merged, conflicts) = merge_trees(&base, &source, &target).unwrap();
        assert!(conflicts.is_empty());
        assert_eq!(merged.len(), 3);
        assert_ne!(merged[&path("/a")].id, merged[&path("/b")].id);
    }

    #[test]
    fn deleting_directory_conflicts_with_modifying_a_descendant() {
        let base = FileTree::from([
            (path("/"), directory(1)),
            (path("/folder"), directory(2)),
            (path("/folder/file"), file(3, b"before")),
        ]);
        let source = FileTree::from([(path("/"), directory(1))]);
        let mut target = base.clone();
        target.insert(path("/folder/file"), file(3, b"changed"));
        let (merged, conflicts) = merge_trees(&base, &source, &target).unwrap();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].path, path("/folder"));
        assert_eq!(
            merged[&path("/folder/file")].content,
            target[&path("/folder/file")].content
        );
    }
}
