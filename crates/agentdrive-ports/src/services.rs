use crate::{ObjectStream, RequestContext};
use agentdrive_model::*;
use async_trait::async_trait;
use bytes::Bytes;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default)]
pub struct TreeChanges {
    pub directories: BTreeMap<String, Option<DirectoryEntry>>,
    pub inodes: BTreeMap<String, Option<Inode>>,
}

#[async_trait]
pub trait TreeEditor: Send + Sync {
    async fn save_tree(&self, workspace: WorkspaceId, tree: &FileTree) -> Result<ObjectRef>;
    async fn edit_tree(
        &self,
        workspace: WorkspaceId,
        root: &ObjectRef,
        changes: TreeChanges,
        writer: &dyn crate::ObjectWriter,
    ) -> Result<ObjectRef>;
}

#[derive(Clone, Debug)]
pub struct OperationContext {
    pub operation: OperationRecord,
}

#[derive(Clone, Debug)]
pub enum OperationStart {
    New(OperationContext),
    Existing(OperationRecord),
}

#[async_trait]
pub trait OperationCoordinator: Send + Sync {
    async fn begin(
        &self,
        context: RequestContext,
        name: &str,
        fingerprint: ObjectId,
        workspace: Option<WorkspaceId>,
        branch: Option<BranchId>,
    ) -> Result<OperationStart>;
    async fn fail(&self, context: &OperationContext, error: Error) -> Result<OperationRecord>;
}

#[async_trait]
pub trait ContentResolver: Send + Sync {
    fn remote_available(&self) -> bool;
    async fn open(&self, workspace: WorkspaceId, reference: &ObjectRef) -> Result<ObjectStream>;
    async fn read_range(
        &self,
        workspace: WorkspaceId,
        reference: &ObjectRef,
        offset: u64,
        size: u32,
    ) -> Result<Bytes>;
    async fn metadata(&self, workspace: WorkspaceId, reference: &ObjectRef) -> Result<Bytes>;
    async fn protect_remote(
        &self,
        workspace: WorkspaceId,
        operation: OperationId,
        roots: Vec<ObjectRef>,
    ) -> Result<()>;
    async fn release_remote(&self, workspace: WorkspaceId, operation: OperationId) -> Result<()>;
}

#[async_trait]
pub trait ObjectGraph: Send + Sync {
    async fn closure(
        &self,
        workspace: WorkspaceId,
        roots: Vec<ObjectRef>,
    ) -> Result<Vec<ObjectRef>>;
    async fn index_pages(&self, workspace: WorkspaceId, root: ObjectRef) -> Result<Vec<ObjectRef>>;
}

#[async_trait]
pub trait RevisionReader: Send + Sync {
    async fn revision(&self, workspace: WorkspaceId, revision: RevisionId) -> Result<Revision>;
    async fn tree(&self, workspace: WorkspaceId, root: &ObjectRef) -> Result<FileTree>;
}

/// Indexed, immutable tree queries. Implementations own traversal and derived caches.
#[async_trait]
pub trait TreeQuery: Send + Sync {
    async fn lookup(
        &self,
        workspace: WorkspaceId,
        root: &ObjectRef,
        path: &WorkspacePath,
    ) -> Result<Option<Inode>>;
    async fn differences(
        &self,
        workspace: WorkspaceId,
        before: Option<&ObjectRef>,
        after: &ObjectRef,
    ) -> Result<Vec<FileDifference>>;
}

#[derive(Clone, Debug)]
pub struct CapturedTree {
    pub branch: Branch,
    pub root: ObjectRef,
    pub nodes: Vec<Node>,
    pub captured_seq: u64,
}

/// The caller owns the branch mutation barrier until its local commit completes.
#[async_trait]
pub trait SnapshotCapture: Send + Sync {
    async fn capture(&self, branch: &Branch) -> Result<CapturedTree>;
}

#[derive(Clone, Debug)]
pub struct SaveRevision {
    pub message: Option<String>,
    pub guard: BranchGuard,
    pub kind: RevisionKind,
    pub parents: Option<Vec<RevisionId>>,
    pub replace_root: Option<ObjectRef>,
    pub durability: Durability,
}

#[async_trait]
pub trait RevisionWriter: Send + Sync {
    async fn save(
        &self,
        context: OperationContext,
        request: SaveRevision,
    ) -> Result<OperationRecord>;
}

#[async_trait]
pub trait RevisionTransition: Send + Sync {
    async fn replace_revision(
        &self,
        context: OperationContext,
        target: BranchGuard,
        revision: RevisionId,
        merge: Option<MergeId>,
        durability: Durability,
    ) -> Result<OperationRecord>;
}

#[async_trait]
pub trait ReplicaPublisher: Send + Sync {
    async fn load_workspace(&self, actor: &Principal, workspace: WorkspaceId) -> Result<()>;
    async fn publish_workspace(&self, workspace: WorkspaceId) -> Result<()>;
    async fn drain(&self, workspace: WorkspaceId, branch: Option<BranchId>) -> Result<()>;
    async fn build_job(
        &self,
        branch: &Branch,
        new_revision: Option<&Revision>,
        operation: &OperationRecord,
    ) -> Result<SyncJob>;
    async fn discover(&self, workspace: WorkspaceId) -> Result<Vec<BranchRef>>;
    async fn refresh_history(&self, workspace: WorkspaceId) -> Result<()>;
    fn request_history_refresh(&self, workspace: WorkspaceId) -> crate::HistoryRefreshStatus;
}

#[async_trait]
pub trait DurabilityService: Send + Sync {
    async fn flush(
        &self,
        branch: BranchId,
        actor: Principal,
        durability: Durability,
    ) -> Result<crate::DurabilityReceipt>;
}

#[async_trait]
pub trait WorkingTree: Send + Sync {
    async fn replace(&self, branch: BranchId, root: &ObjectRef) -> Result<Vec<Node>>;
    fn open_handles(&self, mount: MountId) -> usize;
}
