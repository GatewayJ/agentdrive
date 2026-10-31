//! Workspace services. Storage, host integration and transport remain behind ports.
mod branch_files;
mod cache;
mod content;
mod engine;
mod files;
mod history_refresh;
mod index_reader;
mod indexes;
mod merge;
mod mounts;
mod operations;
mod ownership;
#[cfg(test)]
mod query_tests;
mod relationships;
mod replica;
mod revision_history;
mod revisions;
mod runtime;
mod snapshots;
mod storage;
mod text_diff;
mod tree_queries;
mod trees;
mod workspaces;

pub use content::{ContentService, RemoteStorage};
pub(crate) use operations::OperationService;
pub use runtime::Runtime;
pub(crate) use trees::TreeService;

pub(crate) use files::FileService;
pub(crate) use snapshots::SnapshotService;

pub(crate) use replica::ReplicaService;

pub(crate) use revisions::RevisionService;

pub(crate) use revisions::VersionService;
pub(crate) use workspaces::WorkspaceService;

pub(crate) use mounts::MountService;

pub(crate) use cache::CacheService;

pub(crate) use ownership::OwnershipService;

pub(crate) use merge::MergeService;

pub use engine::{Adapters, Engine, EngineConfig};
