use agentdrive_model::{CommandResult, HistoryRecord, PendingAction};
use serde_json::json;

#[test]
fn persisted_session_results_and_history_remain_readable() {
    let binding = json!({
        "key": {"session_id": "editor", "app_namespace": "acp"},
        "state": "closed",
        "binding_generation": 3,
        "mount": null
    });
    let record = json!({"session": binding["key"], "turn_id": "turn-1", "status": "completed"});
    for value in [
        json!({"kind": "session", "binding": binding}),
        json!({"kind": "turn", "record": record}),
    ] {
        let result: CommandResult = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(result).unwrap(), value);
    }
    let history = json!({"kind": "turn", "record": record});
    let decoded: HistoryRecord = serde_json::from_value(history.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), history);
    let pending: PendingAction = serde_json::from_value(json!({
        "kind": "mount_change", "old": null, "new": null, "session": binding
    }))
    .unwrap();
    assert!(matches!(
        pending,
        PendingAction::MountChange {
            old: None,
            new: None
        }
    ));
}
