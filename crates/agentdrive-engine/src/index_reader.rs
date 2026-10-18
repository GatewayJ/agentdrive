//! Ordered index traversal validates ranges and skips shared immutable pages.
use agentdrive_model::*;
use agentdrive_ports::ContentResolver;
use parking_lot::Mutex;
use serde::de::DeserializeOwned;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

#[derive(Clone)]
struct Page {
    object: ObjectRef,
    lower: Option<String>,
    upper: Option<String>,
    depth: usize,
}

enum Item<T> {
    Page(Page),
    Entry(IndexEntry<T>),
}

struct Cursor<T> {
    pending: VecDeque<Item<T>>,
    visited: BTreeSet<ObjectId>,
    entries: usize,
}

impl<T> Cursor<T> {
    fn new(root: Option<&ObjectRef>) -> Self {
        Self {
            pending: root
                .into_iter()
                .map(|root| {
                    Item::Page(Page {
                        object: root.clone(),
                        lower: None,
                        upper: None,
                        depth: 0,
                    })
                })
                .collect(),
            visited: BTreeSet::new(),
            entries: 0,
        }
    }
}

struct CachedPage<T> {
    value: Arc<IndexPage<T>>,
    encoded_size: usize,
}

pub(crate) struct IndexReader<'a, T> {
    content: &'a dyn ContentResolver,
    workspace: WorkspaceId,
    kind: ObjectKind,
    limit: usize,
    pages: Mutex<BTreeMap<ObjectId, CachedPage<T>>>,
}

impl<'a, T: DeserializeOwned + Clone> IndexReader<'a, T> {
    pub(crate) fn new(
        content: &'a dyn ContentResolver,
        workspace: WorkspaceId,
        kind: ObjectKind,
        limit: usize,
    ) -> Self {
        Self {
            content,
            workspace,
            kind,
            limit,
            pages: Mutex::new(BTreeMap::new()),
        }
    }

    async fn load(&self, page: &Page) -> Result<Arc<IndexPage<T>>> {
        if page.depth > 32 || page.object.kind != self.kind {
            return Err(Error::integrity("invalid index page kind or depth"));
        }
        let cached = self
            .pages
            .lock()
            .get(&page.object.id)
            .map(|p| p.value.clone());
        let value = match cached {
            Some(value) => value,
            None => {
                let bytes = self.content.metadata(self.workspace, &page.object).await?;
                let value = Arc::new(decode::<IndexPage<T>>(&bytes)?);
                let mut pages = self.pages.lock();
                const MAX_CACHE_BYTES: usize = 4 * 1024 * 1024;
                if pages.len() >= 128
                    || pages.values().map(|p| p.encoded_size).sum::<usize>() + bytes.len()
                        > MAX_CACHE_BYTES
                {
                    pages.clear();
                }
                if bytes.len() <= MAX_CACHE_BYTES {
                    pages.insert(
                        page.object.id.clone(),
                        CachedPage {
                            value: value.clone(),
                            encoded_size: bytes.len(),
                        },
                    );
                }
                value
            }
        };
        let keys: Vec<_> = match value.as_ref() {
            IndexPage::Leaf { entries } => entries.iter().map(|entry| &entry.key).collect(),
            IndexPage::Branch { children } => {
                children.iter().map(|child| &child.first_key).collect()
            }
        };
        if keys.len() > INDEX_PAGE_ENTRIES
            || (keys.is_empty()
                && (page.depth > 0 || matches!(value.as_ref(), IndexPage::Branch { .. })))
            || keys.windows(2).any(|pair| pair[0] >= pair[1])
            || page
                .lower
                .as_ref()
                .is_some_and(|lower| keys.first().copied() != Some(lower))
            || page
                .upper
                .as_ref()
                .is_some_and(|upper| keys.last().is_some_and(|last| *last >= upper))
        {
            return Err(Error::integrity("invalid index key range"));
        }
        Ok(value)
    }

    fn children(&self, page: &Page, children: &[IndexChild]) -> Vec<Item<T>> {
        children
            .iter()
            .enumerate()
            .map(|(index, child)| {
                Item::Page(Page {
                    object: child.object.clone(),
                    lower: Some(child.first_key.clone()),
                    upper: children
                        .get(index + 1)
                        .map(|next| next.first_key.clone())
                        .or_else(|| page.upper.clone()),
                    depth: page.depth + 1,
                })
            })
            .collect()
    }

    async fn expand(&self, cursor: &mut Cursor<T>) -> Result<()> {
        let Some(Item::Page(page)) = cursor.pending.pop_front() else {
            return Err(Error::integrity("expected an index page"));
        };
        if !cursor.visited.insert(page.object.id.clone())
            || cursor.visited.len() > self.limit.saturating_mul(2).max(64)
        {
            return Err(Error::integrity("invalid index graph"));
        }
        let items = match self.load(&page).await?.as_ref() {
            IndexPage::Leaf { entries } => {
                cursor.entries += entries.len();
                if cursor.entries > self.limit {
                    return Err(Error::new(
                        ErrorCode::CapacityExceeded,
                        "index exceeds entry limit",
                    ));
                }
                entries.iter().cloned().map(Item::Entry).collect()
            }
            IndexPage::Branch { children } => self.children(&page, children),
        };
        for item in items.into_iter().rev() {
            cursor.pending.push_front(item);
        }
        Ok(())
    }

    pub(crate) async fn lookup(&self, root: &ObjectRef, key: &str) -> Result<Option<T>> {
        let mut page = Page {
            object: root.clone(),
            lower: None,
            upper: None,
            depth: 0,
        };
        loop {
            match self.load(&page).await?.as_ref() {
                IndexPage::Leaf { entries } => {
                    return Ok(entries
                        .iter()
                        .find(|entry| entry.key == key)
                        .map(|entry| entry.value.clone()));
                }
                IndexPage::Branch { children } => {
                    let index = children.partition_point(|child| child.first_key.as_str() <= key);
                    if index == 0 {
                        return Ok(None);
                    }
                    let Item::Page(next) = self.children(&page, children).remove(index - 1) else {
                        unreachable!()
                    };
                    page = next;
                }
            }
        }
    }

    pub(crate) async fn entries(&self, root: &ObjectRef) -> Result<Vec<IndexEntry<T>>> {
        let mut cursor = Cursor::new(Some(root));
        let mut result = Vec::new();
        while let Some(item) = cursor.pending.front() {
            if matches!(item, Item::Page(_)) {
                self.expand(&mut cursor).await?;
            } else if let Some(Item::Entry(entry)) = cursor.pending.pop_front() {
                result.push(entry);
            }
        }
        Ok(result)
    }

    pub(crate) async fn differences(
        &self,
        before: Option<&ObjectRef>,
        after: &ObjectRef,
    ) -> Result<BTreeMap<String, (Option<T>, Option<T>)>>
    where
        T: PartialEq,
    {
        let mut left = Cursor::<T>::new(before);
        let mut right = Cursor::<T>::new(Some(after));
        let mut changed = BTreeMap::new();
        while !left.pending.is_empty() || !right.pending.is_empty() {
            match (left.pending.front(), right.pending.front()) {
                (Some(Item::Page(a)), Some(Item::Page(b))) if a.object == b.object => {
                    left.pending.pop_front();
                    right.pending.pop_front();
                }
                (Some(Item::Page(_)), _) => self.expand(&mut left).await?,
                (_, Some(Item::Page(_))) => self.expand(&mut right).await?,
                (Some(Item::Entry(a)), Some(Item::Entry(b))) if a.key == b.key => {
                    let Some(Item::Entry(a)) = left.pending.pop_front() else {
                        unreachable!()
                    };
                    let Some(Item::Entry(b)) = right.pending.pop_front() else {
                        unreachable!()
                    };
                    if a.value != b.value {
                        changed.insert(a.key, (Some(a.value), Some(b.value)));
                    }
                }
                (Some(Item::Entry(a)), Some(Item::Entry(b))) if a.key > b.key => {
                    let Some(Item::Entry(entry)) = right.pending.pop_front() else {
                        unreachable!()
                    };
                    changed.insert(entry.key, (None, Some(entry.value)));
                }
                (Some(Item::Entry(_)), _) => {
                    let Some(Item::Entry(entry)) = left.pending.pop_front() else {
                        unreachable!()
                    };
                    changed.insert(entry.key, (Some(entry.value), None));
                }
                (_, Some(Item::Entry(_))) => {
                    let Some(Item::Entry(entry)) = right.pending.pop_front() else {
                        unreachable!()
                    };
                    changed.insert(entry.key, (None, Some(entry.value)));
                }
                (None, None) => break,
            }
        }
        Ok(changed)
    }
}
