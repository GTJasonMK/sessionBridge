use rusqlite::params;
use sessionbridge::{
    app::diagnostics,
    clock::FakeClock,
    domain::{TaskId, WorkspaceId},
    storage::SqliteStore,
};
use tempfile::tempdir;
use time::OffsetDateTime;

#[test]
fn collaboration_diagnostics_reports_integration_drift() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("state.db");
    SqliteStore::migrate(&database).unwrap();
    let store = SqliteStore::open(&database).unwrap();
    let workspace_id = WorkspaceId::new();
    let task_id = TaskId::new();
    store
        .connection()
        .execute(
            "INSERT INTO workspaces(id,name,repository_identity,created_at)
             VALUES(?1,'test','repo',0)",
            [workspace_id.to_string()],
        )
        .unwrap();
    store
        .connection()
        .execute(
            "INSERT INTO tasks(
                id,workspace_id,title,phase,autonomy_policy,completion_policy,
                target_branch,created_at,updated_at
             ) VALUES(?1,?2,'test','active','open','all_workers_concluded','main',0,0)",
            params![task_id.to_string(), workspace_id.to_string()],
        )
        .unwrap();
    store
        .connection()
        .execute(
            "INSERT INTO task_integration_integrity(
                task_id,target_branch,target_head,integrated_set_digest,status,
                missing_change_sets_json,details_json,checked_at
             ) VALUES(?1,'main','rewritten','digest','drift',?2,'{}',0)",
            params![
                task_id.to_string(),
                serde_json::json!([{
                    "change_set_id":"change-set-1",
                    "head_commit":"missing-head",
                    "work_item_id":"work-item-1"
                }])
                .to_string()
            ],
        )
        .unwrap();

    let result = diagnostics::analyze(
        store.connection(),
        task_id,
        &FakeClock::new(OffsetDateTime::UNIX_EPOCH),
    )
    .unwrap();

    assert_eq!(result.critical_count, 1);
    assert!(
        result
            .findings
            .iter()
            .any(|finding| finding.code == "INTEGRATED_CHANGESET_DRIFT")
    );
    assert!(
        result.findings[0]
            .message
            .contains("change-set-1@missing-head")
    );
}
