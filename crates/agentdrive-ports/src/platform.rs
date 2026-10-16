use crate::FileSystem;
use agentdrive_model::*;
use async_trait::async_trait;
use std::{path::PathBuf, sync::Arc};

pub trait Clock: Send + Sync {
    fn now_ns(&self) -> i64;
    fn monotonic_ms(&self) -> u64;
}

#[derive(Clone, Debug)]
pub struct MountSpec {
    pub binding: MountBinding,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeMountState {
    Mounted,
    Absent,
    Unknown,
}

#[async_trait]
pub trait MountDriver: Send + Sync {
    async fn normalize_path(&self, path: &std::path::Path) -> Result<PathBuf>;
    async fn mount(&self, spec: MountSpec, file_system: Arc<dyn FileSystem>) -> Result<()>;
    async fn unmount(&self, binding: &MountBinding) -> Result<()>;
    async fn status(&self, binding: &MountBinding) -> Result<NativeMountState>;
    fn capability(&self) -> &'static str;
}

#[async_trait]
pub trait DirectoryAccess: Send + Sync {
    async fn import_tree(
        &self,
        workspace: &Workspace,
        source: &std::path::Path,
        objects: Arc<dyn crate::LocalObjectStore>,
    ) -> Result<FileTree>;
    async fn export_tree(
        &self,
        workspace: WorkspaceId,
        tree: FileTree,
        destination: &std::path::Path,
        content: Arc<dyn crate::ContentResolver>,
    ) -> Result<()>;
}
