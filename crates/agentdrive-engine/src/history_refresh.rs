use crate::RemoteStorage;
use agentdrive_model::*;
use agentdrive_ports::HistoryRefreshStatus;
use parking_lot::Mutex;
use std::{
    collections::BTreeMap,
    sync::{Arc, Weak},
};

const REFRESH_INTERVAL_MS: u64 = 15_000;

#[derive(Debug, Default)]
struct Entry {
    status: HistoryRefreshStatus,
    last_attempt_ms: Option<u64>,
    queued: bool,
    running: Option<OperationId>,
}

#[derive(Debug, Default)]
struct State {
    storage: Option<Weak<RemoteStorage>>,
    entries: BTreeMap<WorkspaceId, Entry>,
}

impl State {
    fn update_storage(&mut self, storage: Option<&Arc<RemoteStorage>>) -> bool {
        let same = match (&self.storage, storage) {
            (Some(previous), Some(current)) => previous.ptr_eq(&Arc::downgrade(current)),
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.entries.clear();
            self.storage = storage.map(Arc::downgrade);
        }
        storage.is_some()
    }
}

#[derive(Debug, Default)]
pub(crate) struct HistoryRefreshCoordinator {
    state: Mutex<State>,
}

impl HistoryRefreshCoordinator {
    pub(crate) fn request(
        &self,
        storage: Option<&Arc<RemoteStorage>>,
        workspace: WorkspaceId,
        now_ms: u64,
    ) -> HistoryRefreshStatus {
        let mut state = self.state.lock();
        if !state.update_storage(storage) {
            return HistoryRefreshStatus::default();
        }
        let entry = state.entries.entry(workspace).or_default();
        if !entry.status.refreshing
            && entry
                .last_attempt_ms
                .is_none_or(|last| now_ms.saturating_sub(last) >= REFRESH_INTERVAL_MS)
        {
            entry.queued = true;
            entry.status.refreshing = true;
        }
        entry.status.clone()
    }

    pub(crate) fn begin(
        self: &Arc<Self>,
        storage: Option<&Arc<RemoteStorage>>,
        now_ms: u64,
        now_ns: i64,
    ) -> Vec<HistoryRefreshAttempt> {
        let mut state = self.state.lock();
        if !state.update_storage(storage) {
            return Vec::new();
        }
        let running = state
            .entries
            .values()
            .filter(|entry| entry.running.is_some())
            .count();
        let mut attempts = Vec::new();
        for (workspace, entry) in state
            .entries
            .iter_mut()
            .filter(|(_, entry)| entry.queued)
            .take(2_usize.saturating_sub(running))
        {
            let id = OperationId::new();
            entry.queued = false;
            entry.running = Some(id);
            entry.last_attempt_ms = Some(now_ms);
            entry.status.last_attempt_ns = Some(now_ns);
            attempts.push(HistoryRefreshAttempt {
                coordinator: self.clone(),
                workspace: *workspace,
                id,
            });
        }
        attempts
    }
}

pub(crate) struct HistoryRefreshAttempt {
    coordinator: Arc<HistoryRefreshCoordinator>,
    pub(crate) workspace: WorkspaceId,
    id: OperationId,
}

impl HistoryRefreshAttempt {
    pub(crate) fn finish(self, result: Result<()>, now_ns: i64) {
        let mut state = self.coordinator.state.lock();
        if let Some(entry) = state.entries.get_mut(&self.workspace)
            && entry.running == Some(self.id)
        {
            entry.running = None;
            entry.status.refreshing = false;
            match result {
                Ok(()) => {
                    entry.status.last_success_ns = Some(now_ns);
                    entry.status.error = None;
                }
                Err(error) => entry.status.error = Some(error),
            }
        }
    }
}

impl Drop for HistoryRefreshAttempt {
    fn drop(&mut self) {
        let mut state = self.coordinator.state.lock();
        if let Some(entry) = state.entries.get_mut(&self.workspace)
            && entry.running == Some(self.id)
        {
            entry.running = None;
            entry.status.refreshing = false;
            entry.status.error = Some(
                Error::new(ErrorCode::Unavailable, "history refresh was interrupted").retryable(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdrive_test_support::MemoryRemote;

    #[test]
    fn refresh_queue_limits_concurrent_workspaces_and_discards_replaced_storage() {
        let remote = Arc::new(MemoryRemote::default());
        let storage = Arc::new(RemoteStorage {
            objects: remote.clone(),
            refs: remote,
        });
        let coordinator = Arc::new(HistoryRefreshCoordinator::default());
        let workspaces = [WorkspaceId::new(), WorkspaceId::new(), WorkspaceId::new()];
        for workspace in workspaces {
            assert!(coordinator.request(Some(&storage), workspace, 0).refreshing);
        }
        let mut active = coordinator.begin(Some(&storage), 0, 1);
        assert_eq!(active.len(), 2);
        assert!(coordinator.begin(Some(&storage), 0, 1).is_empty());
        active.pop().unwrap().finish(Ok(()), 2);
        let next = coordinator.begin(Some(&storage), 0, 2);
        assert_eq!(next.len(), 1);
        assert!(coordinator.begin(Some(&storage), 0, 2).is_empty());
        let replacement = Arc::new(RemoteStorage {
            objects: storage.objects.clone(),
            refs: storage.refs.clone(),
        });
        let status = coordinator.request(Some(&replacement), workspaces[0], 0);
        assert!(status.last_attempt_ns.is_none() && status.last_success_ns.is_none());
        let replacement_attempt = coordinator.begin(Some(&replacement), 0, 3).pop().unwrap();
        drop(active);
        drop(next);
        assert!(
            coordinator
                .request(Some(&replacement), workspaces[0], 0)
                .error
                .is_none()
        );
        replacement_attempt.finish(Ok(()), 4);
        assert_eq!(
            coordinator
                .request(Some(&replacement), workspaces[0], 0)
                .last_success_ns,
            Some(4)
        );
    }
}
