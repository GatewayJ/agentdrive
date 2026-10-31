use agentdrive_model::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HistoryRefreshStatus {
    pub refreshing: bool,
    pub last_attempt_ns: Option<i64>,
    pub last_success_ns: Option<i64>,
    pub error: Option<Error>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionHistoryQuery {
    pub workspace: WorkspaceId,
    pub branch: BranchId,
    pub search: Option<String>,
    pub limit: Option<usize>,
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RevisionHistoryPage {
    pub entries: Vec<Revision>,
    pub total: usize,
    pub next: Option<String>,
}

#[derive(Clone, Debug)]
pub struct RevisionHistoryScan {
    pub workspace: WorkspaceId,
    pub branch: BranchId,
    pub search: String,
    pub before: Option<(i64, RevisionId)>,
    pub limit: usize,
}
