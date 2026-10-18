use crate::{
    ContentService, Runtime, TreeService, index_reader::IndexReader, tree_queries::IndexedTreeQuery,
};
use agentdrive_local::LocalBackend;
use agentdrive_model::*;
use agentdrive_ports::*;
use agentdrive_test_support::ManualClock;
use async_trait::async_trait;
use bytes::Bytes;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

struct CountedContent {
    inner: Arc<ContentService>,
    reads: AtomicUsize,
}
#[async_trait]
impl ContentResolver for CountedContent {
    fn remote_available(&self) -> bool {
        self.inner.remote_available()
    }
    async fn open(&self, w: WorkspaceId, r: &ObjectRef) -> Result<ObjectStream> {
        self.inner.open(w, r).await
    }
    async fn read_range(
        &self,
        w: WorkspaceId,
        r: &ObjectRef,
        offset: u64,
        size: u32,
    ) -> Result<Bytes> {
        self.inner.read_range(w, r, offset, size).await
    }
    async fn metadata(&self, w: WorkspaceId, r: &ObjectRef) -> Result<Bytes> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.inner.metadata(w, r).await
    }
    async fn protect_remote(
        &self,
        w: WorkspaceId,
        op: OperationId,
        roots: Vec<ObjectRef>,
    ) -> Result<()> {
        self.inner.protect_remote(w, op, roots).await
    }
    async fn release_remote(&self, w: WorkspaceId, op: OperationId) -> Result<()> {
        self.inner.release_remote(w, op).await
    }
}

fn path(p: &str) -> WorkspacePath {
    p.to_owned().try_into().unwrap()
}
fn inode(id: u64, content: Option<ObjectRef>) -> Inode {
    Inode {
        id: InodeId(id),
        kind: if content.is_some() {
            FileKind::File
        } else {
            FileKind::Directory
        },
        mode: 0o755,
        uid: 1,
        gid: 2,
        size: content.as_ref().map_or(0, |c| c.size),
        created_ns: 0,
        modified_ns: 0,
        content,
    }
}

static PARSED_VALUES: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Debug, PartialEq)]
struct ParsedValue(u64);

impl<'de> serde::Deserialize<'de> for ParsedValue {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let value = <u64 as serde::Deserialize>::deserialize(deserializer)?;
        PARSED_VALUES.fetch_add(1, Ordering::Relaxed);
        Ok(Self(value))
    }
}

#[tokio::test]
async fn repeated_index_lookups_decode_each_cached_page_once() {
    let temp = tempfile::tempdir().unwrap();
    let local = Arc::new(LocalBackend::open(temp.path()).unwrap());
    let workspace = WorkspaceId::new();
    let page = IndexPage::Leaf {
        entries: (0..100u64)
            .map(|i| IndexEntry {
                key: format!("{i:04}"),
                value: i,
            })
            .collect(),
    };
    let root = local
        .put(
            workspace,
            ObjectKind::InodeIndex,
            encode(&page).unwrap().into(),
        )
        .await
        .unwrap();
    let content = ContentService::new(local, None, None);
    let reader =
        IndexReader::<ParsedValue>::new(content.as_ref(), workspace, ObjectKind::InodeIndex, 100);
    for i in 0..100 {
        assert_eq!(
            reader.lookup(&root, &format!("{i:04}")).await.unwrap(),
            Some(ParsedValue(i))
        );
    }
    assert_eq!(PARSED_VALUES.load(Ordering::Relaxed), 100);
}

#[tokio::test]
async fn indexed_queries_skip_shared_pages_and_preserve_old_roots() {
    let temp = tempfile::tempdir().unwrap();
    let local = Arc::new(LocalBackend::open(temp.path()).unwrap());
    let workspace = WorkspaceId::new();
    local
        .commit(LocalCommit {
            new_workspaces: vec![Workspace {
                id: workspace,
                name: "indexed".into(),
                owner: "owner".to_owned().try_into().unwrap(),
                initial_revision: RevisionId::new(),
                format_version: FORMAT_VERSION,
                name_policy: "portable_v1".into(),
                grants: vec![],
                max_file_bytes: u64::MAX,
                max_working_bytes: u64::MAX,
                max_inodes: 100_000,
            }],
            ..Default::default()
        })
        .await
        .unwrap();
    let runtime = Runtime::new(local.clone(), Arc::new(ManualClock::default()))
        .await
        .unwrap();
    let content = ContentService::new(local.clone(), None, None);
    let trees = TreeService::new(runtime, content.clone());
    let counted = Arc::new(CountedContent {
        inner: content,
        reads: AtomicUsize::new(0),
    });
    let query = IndexedTreeQuery::new(counted.clone(), local.clone());
    let file = local
        .put(workspace, ObjectKind::File, Bytes::from_static(b"before\n"))
        .await
        .unwrap();
    let mut tree = FileTree::new();
    tree.insert(WorkspacePath::root(), inode(InodeId::ROOT.0, None));
    for i in 0..10_000 {
        tree.insert(
            path(&format!("/File-{i:05}")),
            inode(i + 2, Some(file.clone())),
        );
    }
    let before = trees.save_tree(workspace, &tree).await.unwrap();
    let target = path("/File-09999");
    let mut updated = tree[&target].clone();
    updated.content = Some(
        local
            .put(workspace, ObjectKind::File, Bytes::from_static(b"after\n"))
            .await
            .unwrap(),
    );
    updated.size = 6;
    let batch = local.begin_batch(workspace).await.unwrap();
    let after = trees
        .edit_tree(
            workspace,
            &before,
            TreeChanges {
                inodes: [(crate::trees::inode_key(updated.id), Some(updated.clone()))].into(),
                ..Default::default()
            },
            batch.as_ref(),
        )
        .await
        .unwrap();
    batch.finish().await.unwrap();
    let start = std::time::Instant::now();
    assert_eq!(
        query
            .lookup(workspace, &after, &path("/fILE-09999"))
            .await
            .unwrap(),
        Some(updated)
    );
    let lookup_reads = counted.reads.swap(0, Ordering::Relaxed);
    assert!(lookup_reads <= 10, "{lookup_reads}");
    let lookup_us = start.elapsed().as_micros();
    let start = std::time::Instant::now();
    let changes = query
        .differences(workspace, Some(&before), &after)
        .await
        .unwrap();
    let cold_reads = counted.reads.swap(0, Ordering::Relaxed);
    let cold_us = start.elapsed().as_micros();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].path, target);
    assert_eq!(changes[0].kind, FileChangeKind::Modified);
    let start = std::time::Instant::now();
    assert_eq!(
        query
            .differences(workspace, Some(&before), &after)
            .await
            .unwrap(),
        changes
    );
    let warm_reads = counted.reads.swap(0, Ordering::Relaxed);
    let warm_us = start.elapsed().as_micros();
    assert!(warm_reads <= 12, "{warm_reads}");
    assert!(
        cold_reads > warm_reads * 4,
        "cold={cold_reads} warm={warm_reads}"
    );
    let start = std::time::Instant::now();
    let full_after = trees.tree(workspace, &after).await.unwrap();
    let full_tree_us = start.elapsed().as_micros();
    assert_eq!(
        full_after.get(&target),
        query
            .lookup(workspace, &after, &target)
            .await
            .unwrap()
            .as_ref()
    );
    let start = std::time::Instant::now();
    let full_before = trees.tree(workspace, &before).await.unwrap();
    let full_after = trees.tree(workspace, &after).await.unwrap();
    assert_eq!(
        full_before
            .iter()
            .filter(|(path, inode)| full_after.get(*path) != Some(*inode))
            .count(),
        1
    );
    let full_comparison_us = start.elapsed().as_micros();
    println!(
        "indexed-query files=10000 lookup_reads={lookup_reads} lookup_us={lookup_us} cold_reads={cold_reads} cold_us={cold_us} warm_reads={warm_reads} warm_us={warm_us} full_tree_us={full_tree_us} full_comparison_us={full_comparison_us}"
    );
    assert_eq!(
        query
            .lookup(workspace, &before, &target)
            .await
            .unwrap()
            .unwrap()
            .content,
        Some(file)
    );
    assert!(
        query
            .lookup(workspace, &after, &path("/missing"))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        query
            .differences(workspace, Some(&after), &after)
            .await
            .unwrap()
            .is_empty()
    );
    let mut moved = tree.clone();
    moved.insert(path("/Moved"), inode(10_002, None));
    let value = moved.remove(&target).unwrap();
    moved.insert(path("/Moved/File-09999"), value);
    let changed = moved.get_mut(&path("/File-00000")).unwrap();
    changed.uid = 9;
    changed.gid = 10;
    // Timestamp-only changes do not create a visible file difference.
    moved.get_mut(&path("/File-00001")).unwrap().modified_ns = 99;
    let moved_root = trees.save_tree(workspace, &moved).await.unwrap();
    let changes = query
        .differences(workspace, Some(&before), &moved_root)
        .await
        .unwrap();
    assert_eq!(changes.len(), 3);
    assert_eq!(changes[0].kind, FileChangeKind::Metadata);
    assert_eq!(changes[1].kind, FileChangeKind::Added);
    assert_eq!(changes[2].kind, FileChangeKind::Renamed);
    assert_eq!(changes[2].previous_path.as_ref(), Some(&target));
    assert!(
        query
            .lookup(workspace, &moved_root, &target)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        query
            .lookup(workspace, &moved_root, &path("/moved/file-09999"))
            .await
            .unwrap()
            .is_some()
    );
    let value = moved.remove(&path("/File-09998")).unwrap();
    moved.insert(path("/Moved/File-09998"), value);
    let ambiguous_root = trees.save_tree(workspace, &moved).await.unwrap();
    let changes = query
        .differences(workspace, Some(&before), &ambiguous_root)
        .await
        .unwrap();
    assert_eq!(changes.len(), 6);
    assert!(
        !changes
            .iter()
            .any(|entry| entry.kind == FileChangeKind::Renamed)
    );
    let mut nested = FileTree::new();
    nested.insert(WorkspacePath::root(), inode(InodeId::ROOT.0, None));
    nested.insert(path("/old"), inode(10_002, None));
    for (name, value) in tree
        .iter()
        .filter(|(p, _)| **p != WorkspacePath::root())
        .take(1000)
    {
        nested.insert(path(&format!("/old{}", name.as_str())), value.clone());
    }
    let old_directory = trees.save_tree(workspace, &nested).await.unwrap();
    let relocated: FileTree = nested
        .into_iter()
        .map(|(name, value)| (path(&name.as_str().replacen("/old", "/new", 1)), value))
        .collect();
    let new_directory = trees.save_tree(workspace, &relocated).await.unwrap();
    let start = std::time::Instant::now();
    let changes = query
        .differences(workspace, Some(&old_directory), &new_directory)
        .await
        .unwrap();
    let directory_query_us = start.elapsed().as_micros();
    assert_eq!(changes.len(), 2002);
    let start = std::time::Instant::now();
    trees.tree(workspace, &old_directory).await.unwrap();
    trees.tree(workspace, &new_directory).await.unwrap();
    println!(
        "directory-query files=1000 indexed_us={directory_query_us} full_tree_us={}",
        start.elapsed().as_micros()
    );
}

#[tokio::test]
async fn index_diff_handles_rebalanced_pages_and_height_changes() {
    let temp = tempfile::tempdir().unwrap();
    let local = Arc::new(LocalBackend::open(temp.path()).unwrap());
    let runtime = Runtime::new(local.clone(), Arc::new(ManualClock::default()))
        .await
        .unwrap();
    let content = ContentService::new(local, None, None);
    let trees = TreeService::new(runtime, content.clone());
    let w = WorkspaceId::new();
    let mut old: BTreeMap<String, u64> = (0..17000).map(|i| (format!("{i:08}"), i)).collect();
    let mut root = trees
        .save_index(
            w,
            ObjectKind::InodeIndex,
            old.iter()
                .map(|(key, value)| IndexEntry {
                    key: key.clone(),
                    value: *value,
                })
                .collect(),
        )
        .await
        .unwrap();
    for size in [7000, 65, 300, 0, 170] {
        let new: BTreeMap<_, _> = (0..size)
            .map(|i| (format!("{:08}", i * 2), i + 1))
            .collect();
        let edits: BTreeMap<_, _> = old
            .keys()
            .chain(new.keys())
            .map(|key| (key.clone(), new.get(key).copied()))
            .collect();
        let next = trees.update_index(w, &root, edits).await.unwrap();
        let reader = IndexReader::new(content.as_ref(), w, ObjectKind::InodeIndex, 20000);
        let expected: BTreeMap<_, _> = old
            .keys()
            .chain(new.keys())
            .filter(|key| old.get(*key) != new.get(*key))
            .map(|key| (key.clone(), (old.get(key).copied(), new.get(key).copied())))
            .collect();
        assert_eq!(
            reader.differences(Some(&root), &next).await.unwrap(),
            expected
        );
        root = next;
        old = new;
    }
}
