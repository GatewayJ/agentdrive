use agentdrive_model::*;
use serde::{Deserialize, Serialize};

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SynchronizationState {
    #[default]
    Disabled,
    Unsaved,
    Pending,
    Unconfirmed,
    Confirmed,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BranchSynchronization {
    pub branch: BranchId,
    pub local_saved: bool,
    pub remote_confirmed: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkspaceSynchronization {
    pub state: SynchronizationState,
    pub branches: Vec<BranchSynchronization>,
}

impl WorkspaceSynchronization {
    pub fn new(
        location: LocationId,
        branches: &[Branch],
        references: &[BranchRef],
        configured: bool,
        pending: usize,
    ) -> Self {
        let references: std::collections::BTreeMap<_, _> = references
            .iter()
            .map(|reference| (reference.branch, reference))
            .collect();
        let states: Vec<_> = branches
            .iter()
            .map(|branch| {
                let local_saved = branch.mutation_seq == branch.saved_seq;
                let remote_confirmed = configured
                    && local_saved
                    && references.get(&branch.id).is_some_and(|reference| {
                        !reference.deleted
                            && reference.owner == branch.owner
                            && reference.authority_epoch == branch.authority_epoch
                            && reference.generation == branch.generation
                            && reference.durable_seq == branch.saved_seq
                            && reference.working_root == branch.working_root
                            && reference.formal_head == branch.formal_head
                            && reference.autosave_head == branch.autosave_head
                    });
                BranchSynchronization {
                    branch: branch.id,
                    local_saved,
                    remote_confirmed,
                }
            })
            .collect();
        let local: Vec<_> = branches
            .iter()
            .zip(&states)
            .filter(|(branch, _)| branch.owner == location && branch.state != BranchState::Deleted)
            .map(|(_, state)| state)
            .collect();
        let state = if !configured {
            SynchronizationState::Disabled
        } else if local.iter().any(|state| !state.local_saved) {
            SynchronizationState::Unsaved
        } else if pending > 0 {
            SynchronizationState::Pending
        } else if local.iter().any(|state| !state.remote_confirmed) {
            SynchronizationState::Unconfirmed
        } else {
            SynchronizationState::Confirmed
        };
        Self {
            state,
            branches: states,
        }
    }
}
