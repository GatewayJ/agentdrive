//! Copy-on-write index editing; the storage writer owns object durability.
use agentdrive_model::*;
use agentdrive_ports::{ContentResolver, ObjectWriter};
use futures::{FutureExt, future::BoxFuture};
use serde::{Serialize, de::DeserializeOwned};
use std::collections::BTreeMap;

const MIN_ENTRIES: usize = INDEX_PAGE_ENTRIES / 2;

enum Edited<T> {
    Existing(IndexChild),
    Pending(IndexPage<T>),
}

fn page_len<T>(page: &IndexPage<T>) -> usize {
    match page {
        IndexPage::Leaf { entries } => entries.len(),
        IndexPage::Branch { children } => children.len(),
    }
}

pub(super) fn balanced<T: Clone>(values: &[T]) -> Vec<Vec<T>> {
    if values.is_empty() {
        return Vec::new();
    }
    let pages = values.len().div_ceil(INDEX_PAGE_ENTRIES);
    let width = values.len() / pages;
    let extra = values.len() % pages;
    let mut offset = 0;
    (0..pages)
        .map(|index| {
            let end = offset + width + usize::from(index < extra);
            let page = values[offset..end].to_vec();
            offset = end;
            page
        })
        .collect()
}

pub(crate) struct IndexEditor<'a, T> {
    content: &'a dyn ContentResolver,
    writer: &'a dyn ObjectWriter,
    workspace: WorkspaceId,
    kind: ObjectKind,
    // New pages remain staged until the caller commits the object batch.
    written: BTreeMap<ObjectId, IndexPage<T>>,
}

impl<'a, T> IndexEditor<'a, T>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    pub(crate) fn new(
        content: &'a dyn ContentResolver,
        writer: &'a dyn ObjectWriter,
        workspace: WorkspaceId,
        kind: ObjectKind,
    ) -> Self {
        Self {
            content,
            writer,
            workspace,
            kind,
            written: BTreeMap::new(),
        }
    }

    async fn load(&self, reference: &ObjectRef) -> Result<IndexPage<T>> {
        if reference.kind != self.kind {
            return Err(Error::integrity("index page has a different kind"));
        }
        if let Some(page) = self.written.get(&reference.id) {
            return Ok(page.clone());
        }
        decode(&self.content.metadata(self.workspace, reference).await?)
    }

    async fn persist(&mut self, edited: Edited<T>) -> Result<IndexChild> {
        match edited {
            Edited::Existing(child) => Ok(child),
            Edited::Pending(page) => {
                let first_key = match &page {
                    IndexPage::Leaf { entries } => entries.first().map(|entry| entry.key.clone()),
                    IndexPage::Branch { children } => {
                        children.first().map(|child| child.first_key.clone())
                    }
                }
                .unwrap_or_default();
                let object = self
                    .writer
                    .put(self.workspace, self.kind, encode(&page)?.into())
                    .await?;
                self.written.insert(object.id.clone(), page);
                Ok(IndexChild { first_key, object })
            }
        }
    }

    async fn page(&self, edited: Edited<T>) -> Result<IndexPage<T>> {
        match edited {
            Edited::Existing(child) => self.load(&child.object).await,
            Edited::Pending(page) => Ok(page),
        }
    }

    async fn rebalance(&self, pages: &mut Vec<Edited<T>>) -> Result<()> {
        let mut i = 0;
        while pages.len() > 1 && i < pages.len() {
            if !matches!(&pages[i], Edited::Pending(page) if page_len(page) < MIN_ENTRIES) {
                i += 1;
                continue;
            }
            let left = i.saturating_sub(1);
            let first = self.page(pages.remove(left)).await?;
            let second = self.page(pages.remove(left)).await?;
            let merged = match (first, second) {
                (IndexPage::Leaf { mut entries }, IndexPage::Leaf { entries: right }) => {
                    entries.extend(right);
                    balanced(&entries)
                        .into_iter()
                        .map(|entries| Edited::Pending(IndexPage::Leaf { entries }))
                        .collect::<Vec<_>>()
                }
                (IndexPage::Branch { mut children }, IndexPage::Branch { children: right }) => {
                    children.extend(right);
                    balanced(&children)
                        .into_iter()
                        .map(|children| Edited::Pending(IndexPage::Branch { children }))
                        .collect()
                }
                _ => {
                    return Err(Error::integrity(
                        "adjacent index pages have different levels",
                    ));
                }
            };
            pages.splice(left..left, merged);
            i = left;
        }
        Ok(())
    }

    fn edit_page(
        &mut self,
        root: ObjectRef,
        mut changes: BTreeMap<String, Option<T>>,
        depth: u32,
    ) -> BoxFuture<'_, Result<Vec<Edited<T>>>> {
        async move {
            if depth > 32 {
                return Err(Error::integrity("index exceeds maximum depth"));
            }
            let page = self.load(&root).await?;
            let updated = match page {
                IndexPage::Leaf { entries } => {
                    let mut values: BTreeMap<_, _> = entries
                        .into_iter()
                        .map(|entry| (entry.key, entry.value))
                        .collect();
                    for (key, value) in changes {
                        if let Some(value) = value {
                            values.insert(key, value);
                        } else {
                            values.remove(&key);
                        }
                    }
                    let entries: Vec<_> = values
                        .into_iter()
                        .map(|(key, value)| IndexEntry { key, value })
                        .collect();
                    balanced(&entries)
                        .into_iter()
                        .map(|entries| IndexPage::Leaf { entries })
                        .collect::<Vec<_>>()
                }
                IndexPage::Branch { children } => {
                    if children.is_empty()
                        || children
                            .windows(2)
                            .any(|pair| pair[0].first_key >= pair[1].first_key)
                    {
                        return Err(Error::integrity("invalid index children"));
                    }
                    let mut edited = Vec::new();
                    for (index, child) in children.iter().enumerate() {
                        let rest = children
                            .get(index + 1)
                            .map(|next| changes.split_off(&next.first_key))
                            .unwrap_or_default();
                        let selected = std::mem::replace(&mut changes, rest);
                        if selected.is_empty() {
                            edited.push(Edited::Existing(child.clone()));
                        } else {
                            edited.extend(
                                self.edit_page(child.object.clone(), selected, depth + 1)
                                    .await?,
                            );
                        }
                    }
                    self.rebalance(&mut edited).await?;
                    let mut children = Vec::with_capacity(edited.len());
                    for page in edited {
                        children.push(self.persist(page).await?);
                    }
                    balanced(&children)
                        .into_iter()
                        .map(|children| IndexPage::Branch { children })
                        .collect()
                }
            };
            if updated.len() == 1 && object_ref(self.kind, &encode(&updated[0])?) == root {
                let first_key = match &updated[0] {
                    IndexPage::Leaf { entries } => entries[0].key.clone(),
                    IndexPage::Branch { children } => children[0].first_key.clone(),
                };
                return Ok(vec![Edited::Existing(IndexChild {
                    first_key,
                    object: root,
                })]);
            }
            Ok(updated.into_iter().map(Edited::Pending).collect())
        }
        .boxed()
    }

    pub(crate) async fn update(
        mut self,
        root: &ObjectRef,
        changes: BTreeMap<String, Option<T>>,
    ) -> Result<ObjectRef> {
        if changes.is_empty() {
            return Ok(root.clone());
        }
        let mut pages = self.edit_page(root.clone(), changes, 0).await?;
        if pages.is_empty() {
            pages.push(Edited::Pending(IndexPage::Leaf {
                entries: Vec::new(),
            }));
        }
        while pages.len() > 1 {
            let mut children = Vec::with_capacity(pages.len());
            for page in pages {
                children.push(self.persist(page).await?);
            }
            pages = balanced(&children)
                .into_iter()
                .map(|children| Edited::Pending(IndexPage::Branch { children }))
                .collect();
        }
        let mut root = self.persist(pages.remove(0)).await?.object;
        for _ in 0..=32 {
            match self.load(&root).await? {
                IndexPage::Branch { children } if children.len() == 1 => {
                    root = children[0].object.clone()
                }
                _ => return Ok(root),
            }
        }
        Err(Error::integrity("index exceeds maximum depth"))
    }
}
