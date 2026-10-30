use agentdrive_model::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct RelationshipFilter {
    pub branch: Option<BranchId>,
    pub mount: Option<MountId>,
    pub owner_location: Option<LocationId>,
    pub mount_location: Option<LocationId>,
    pub mount_state: Option<MountState>,
    pub limit: Option<usize>,
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRelationshipsQuery {
    pub workspace: WorkspaceId,
    #[serde(flatten)]
    pub filter: RelationshipFilter,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkspaceRelationships {
    pub workspace: WorkspaceId,
    pub observing_location: LocationId,
    pub selected_branch: Option<BranchId>,
    /// Remote mount and selection inventories are not replicated.
    pub remote_mounts_known: bool,
    pub remote_selections_known: bool,
    pub entries: Vec<WorkspaceRelationship>,
    /// Number of authorized, known relationships matching the filters.
    pub total: usize,
    pub next: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkspaceRelationship {
    pub branch: RelationshipBranch,
    pub mount: Option<MountBinding>,
    pub mount_location: Option<LocationId>,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RelationshipBranch {
    pub id: BranchId,
    pub name: Option<String>,
    pub ownership: RelationshipOwnership,
    pub state: BranchState,
    pub origin: RelationshipOrigin,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RelationshipOwnership {
    pub location: LocationId,
    pub authority_epoch: u64,
    pub generation: u64,
    pub source: OwnershipSource,
    pub storage_version: Option<String>,
    pub observed_ns: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OwnershipSource {
    LocalAuthority,
    RemoteReference,
    LocalRecord,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RelationshipOrigin {
    pub workspace_root: bool,
    pub source_branch: Option<BranchId>,
    pub source_name: Option<String>,
    pub base_revision: Option<RevisionId>,
    /// The root recorded at branch creation, independent of its current contents.
    pub captured_working_root: Option<ObjectRef>,
    pub status: OriginStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OriginStatus {
    Available,
    Unavailable,
    Restricted,
}
