//! Read-only tree queries; object references identify immutable cached projections.
use crate::index_reader::IndexReader;
use crate::trees::{directory_key, inode_key};
use agentdrive_model::*;
use agentdrive_ports::{ContentResolver, LocalStateStore, TreeQuery};
use async_trait::async_trait;
use parking_lot::Mutex;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
};

#[derive(Default)]
struct Paths {
    by_id: BTreeMap<InodeId, WorkspacePath>,
    by_path: BTreeMap<WorkspacePath, InodeId>,
}

type PathCache = VecDeque<(WorkspaceId, ObjectRef, Arc<Paths>, usize)>;

pub(crate) struct IndexedTreeQuery {
    content: Arc<dyn ContentResolver>,
    state: Arc<dyn LocalStateStore>,
    paths: Mutex<PathCache>,
}

impl IndexedTreeQuery {
    pub(crate) fn new(
        content: Arc<dyn ContentResolver>,
        state: Arc<dyn LocalStateStore>,
    ) -> Arc<Self> {
        Arc::new(Self {
            content,
            state,
            paths: Mutex::new(VecDeque::new()),
        })
    }

    async fn root(&self, workspace: WorkspaceId, object: &ObjectRef) -> Result<TreeRoot> {
        if object.kind != ObjectKind::Tree {
            return Err(Error::integrity("expected a tree object"));
        }
        let root: TreeRoot = decode(&self.content.metadata(workspace, object).await?)?;
        if root.format_version != FORMAT_VERSION
            || root.directories.kind != ObjectKind::DirectoryIndex
            || root.inodes.kind != ObjectKind::InodeIndex
        {
            return Err(Error::integrity("unsupported tree format"));
        }
        Ok(root)
    }

    async fn inode(
        reader: &IndexReader<'_, Inode>,
        root: &ObjectRef,
        id: InodeId,
    ) -> Result<Inode> {
        let inode: Inode = reader
            .lookup(root, &inode_key(id))
            .await?
            .ok_or_else(|| Error::integrity("directory inode is missing"))?;
        if inode.id != id {
            return Err(Error::integrity("inode key mismatch"));
        }
        Ok(inode)
    }

    async fn paths(
        &self,
        workspace: WorkspaceId,
        root: &ObjectRef,
        limit: usize,
    ) -> Result<Arc<Paths>> {
        {
            let mut cache = self.paths.lock();
            if let Some(index) = cache
                .iter()
                .position(|(w, key, _, _)| *w == workspace && key == root)
            {
                let entry = cache.remove(index).expect("located cache entry");
                let paths = entry.2.clone();
                cache.push_back(entry);
                if paths.by_id.len() > limit {
                    return Err(Error::new(
                        ErrorCode::CapacityExceeded,
                        "tree exceeds inode limit",
                    ));
                }
                return Ok(paths);
            }
        }
        let reader = IndexReader::<DirectoryEntry>::new(
            self.content.as_ref(),
            workspace,
            ObjectKind::DirectoryIndex,
            limit,
        );
        let mut directories = BTreeMap::new();
        for entry in reader.entries(root).await? {
            if directory_key(&entry.value)? != entry.key
                || entry.value.inode == InodeId::ROOT
                || directories.insert(entry.value.inode, entry.value).is_some()
            {
                return Err(Error::integrity("invalid directory index"));
            }
        }
        let mut paths = Paths::default();
        paths.by_id.insert(InodeId::ROOT, WorkspacePath::root());
        for id in directories.keys() {
            let mut current = *id;
            let mut chain = Vec::new();
            let mut seen = BTreeSet::new();
            while !paths.by_id.contains_key(&current) {
                if !seen.insert(current) || chain.len() >= 2048 {
                    return Err(Error::integrity("directory cycle or excessive path depth"));
                }
                let entry = directories
                    .get(&current)
                    .ok_or_else(|| Error::integrity("directory parent is missing"))?;
                chain.push(entry);
                current = entry.parent;
            }
            let mut path = paths.by_id[&current].clone();
            for entry in chain.into_iter().rev() {
                path = path.join(&entry.name)?;
                paths.by_id.insert(entry.inode, path.clone());
            }
        }
        if paths.by_id.len() > limit {
            return Err(Error::new(
                ErrorCode::CapacityExceeded,
                "tree exceeds inode limit",
            ));
        }
        paths.by_path = paths
            .by_id
            .iter()
            .map(|(id, path)| (path.clone(), *id))
            .collect();
        let charge = paths
            .by_id
            .values()
            .map(|p| 128 + p.as_str().len() * 2)
            .sum::<usize>();
        let paths = Arc::new(paths);
        // Bound both the number and approximate allocated size of derived projections.
        const MAX_BYTES: usize = 16 * 1024 * 1024;
        if charge <= MAX_BYTES {
            let evicted = {
                let mut cache = self.paths.lock();
                let mut evicted = Vec::new();
                if let Some(index) = cache
                    .iter()
                    .position(|(w, key, _, _)| *w == workspace && key == root)
                {
                    evicted.extend(cache.remove(index));
                }
                while cache.len() >= 4
                    || cache.iter().map(|e| e.3).sum::<usize>() + charge > MAX_BYTES
                {
                    evicted.extend(cache.pop_front());
                }
                cache.push_back((workspace, root.clone(), paths.clone(), charge));
                evicted
            };
            // Free potentially large path maps after releasing the shared cache lock.
            drop(evicted);
        }
        Ok(paths)
    }
}

#[async_trait]
impl TreeQuery for IndexedTreeQuery {
    async fn lookup(
        &self,
        workspace: WorkspaceId,
        object: &ObjectRef,
        path: &WorkspacePath,
    ) -> Result<Option<Inode>> {
        let root = self.root(workspace, object).await?;
        let directories = IndexReader::<DirectoryEntry>::new(
            self.content.as_ref(),
            workspace,
            ObjectKind::DirectoryIndex,
            usize::MAX,
        );
        let inodes = IndexReader::<Inode>::new(
            self.content.as_ref(),
            workspace,
            ObjectKind::InodeIndex,
            usize::MAX,
        );
        let mut inode = Self::inode(&inodes, &root.inodes, InodeId::ROOT).await?;
        for name in path.as_str().split('/').filter(|name| !name.is_empty()) {
            if inode.kind != FileKind::Directory {
                return Ok(None);
            }
            let key = directory_key(&DirectoryEntry {
                parent: inode.id,
                name: name.to_owned(),
                inode: InodeId::ROOT,
            })?;
            let Some(entry) = directories.lookup(&root.directories, &key).await? else {
                return Ok(None);
            };
            if entry.parent != inode.id || directory_key(&entry)? != key {
                return Err(Error::integrity("directory key mismatch"));
            }
            inode = Self::inode(&inodes, &root.inodes, entry.inode).await?;
        }
        Ok(Some(inode))
    }

    async fn differences(
        &self,
        workspace: WorkspaceId,
        before: Option<&ObjectRef>,
        after: &ObjectRef,
    ) -> Result<Vec<FileDifference>> {
        if before == Some(after) {
            return Ok(Vec::new());
        }
        let max_inodes = self
            .state
            .workspace(workspace)
            .await?
            .ok_or_else(|| Error::new(ErrorCode::NotFound, "workspace does not exist"))?
            .max_inodes;
        let limit = usize::try_from(max_inodes).unwrap_or(usize::MAX);
        let after = self.root(workspace, after).await?;
        let before = match before {
            Some(root) => Some(self.root(workspace, root).await?),
            None => None,
        };
        let inodes = IndexReader::<Inode>::new(
            self.content.as_ref(),
            workspace,
            ObjectKind::InodeIndex,
            limit,
        );
        let changed = inodes
            .differences(before.as_ref().map(|root| &root.inodes), &after.inodes)
            .await?;
        let after_paths = self.paths(workspace, &after.directories, limit).await?;
        let before_paths = match &before {
            Some(root) => self.paths(workspace, &root.directories, limit).await?,
            None => Arc::new(Paths::default()),
        };
        let mut candidates = BTreeSet::new();
        if !Arc::ptr_eq(&before_paths, &after_paths) {
            for (id, path) in &after_paths.by_id {
                if before_paths.by_id.get(id) != Some(path) {
                    candidates.insert(path.clone());
                }
            }
            for (id, path) in &before_paths.by_id {
                if after_paths.by_id.get(id) != Some(path) {
                    candidates.insert(path.clone());
                }
            }
        }
        let mut old_inodes = BTreeMap::new();
        let mut new_inodes = BTreeMap::new();
        for (key, (old, new)) in changed {
            for (inode, paths) in [(old.as_ref(), &before_paths), (new.as_ref(), &after_paths)] {
                if let Some(inode) = inode {
                    if inode_key(inode.id) != key {
                        return Err(Error::integrity("inode key mismatch"));
                    }
                    let path = paths
                        .by_id
                        .get(&inode.id)
                        .ok_or_else(|| Error::integrity("inode has no directory entry"))?;
                    candidates.insert(path.clone());
                }
            }
            if let Some(inode) = old {
                old_inodes.insert(inode.id, inode);
            }
            if let Some(inode) = new {
                new_inodes.insert(inode.id, inode);
            }
        }
        let mut result = Vec::new();
        for path in candidates {
            let mut sides = Vec::new();
            for (paths, cache, root) in [
                (
                    &before_paths,
                    &mut old_inodes,
                    before.as_ref().map(|r| &r.inodes),
                ),
                (&after_paths, &mut new_inodes, Some(&after.inodes)),
            ] {
                let value = match (paths.by_path.get(&path), root) {
                    (Some(id), Some(root)) => {
                        if !cache.contains_key(id) {
                            cache.insert(*id, Self::inode(&inodes, root, *id).await?);
                        }
                        cache.get(id).cloned()
                    }
                    _ => None,
                };
                sides.push(value);
            }
            let new = sides.pop().expect("after inode");
            let old = sides.pop().expect("before inode");
            if let Some(kind) = change_kind(old.as_ref(), new.as_ref()) {
                result.push(FileDifference {
                    path,
                    previous_path: None,
                    kind,
                    before: old,
                    after: new,
                });
            }
        }
        detect_renames(&mut result);
        Ok(result)
    }
}

fn change_kind(before: Option<&Inode>, after: Option<&Inode>) -> Option<FileChangeKind> {
    match (before, after) {
        (None, Some(_)) => Some(FileChangeKind::Added),
        (Some(_), None) => Some(FileChangeKind::Deleted),
        (Some(a), Some(b)) if a.kind != b.kind || a.content != b.content || a.size != b.size => {
            Some(FileChangeKind::Modified)
        }
        (Some(a), Some(b)) if (a.mode, a.uid, a.gid) != (b.mode, b.uid, b.gid) => {
            Some(FileChangeKind::Metadata)
        }
        _ => None,
    }
}

fn detect_renames(entries: &mut Vec<FileDifference>) {
    // Only unique exact-content pairs are identified; inode numbers can collide across branches.
    let mut deleted = BTreeMap::<ObjectRef, Vec<usize>>::new();
    let mut added = BTreeMap::<ObjectRef, Vec<usize>>::new();
    for (index, entry) in entries.iter().enumerate() {
        let (inode, map) = match entry.kind {
            FileChangeKind::Added => (entry.after.as_ref(), &mut added),
            FileChangeKind::Deleted => (entry.before.as_ref(), &mut deleted),
            _ => continue,
        };
        if let Some(content) = inode
            .filter(|i| i.kind == FileKind::File)
            .and_then(|i| i.content.clone())
        {
            map.entry(content).or_default().push(index);
        }
    }
    let mut removed = BTreeSet::new();
    for (content, old) in deleted {
        if let Some(new) = added
            .get(&content)
            .filter(|new| new.len() == 1 && old.len() == 1)
        {
            let old_entry = entries[old[0]].clone();
            let new_entry = &mut entries[new[0]];
            new_entry.kind = FileChangeKind::Renamed;
            new_entry.previous_path = Some(old_entry.path);
            new_entry.before = old_entry.before;
            removed.insert(old[0]);
        }
    }
    let mut index = 0;
    entries.retain(|_| {
        let keep = !removed.contains(&index);
        index += 1;
        keep
    });
}
