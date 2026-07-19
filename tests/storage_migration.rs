use std::collections::HashSet;

use rusqlite::Connection;
use sessionbridge::{
    ErrorCode,
    storage::{LATEST_SCHEMA_VERSION, SqliteStore, migrations::MIGRATIONS},
};
use tempfile::tempdir;

#[test]
fn migration_validates_and_creates_the_complete_v1_schema() {
    MIGRATIONS.validate().expect("migration SQL must validate");

    let directory = tempdir().unwrap();
    let database_path = directory.path().join("sessionbridge.db");

    SqliteStore::migrate(&database_path).unwrap();
    let store = SqliteStore::open(&database_path).unwrap();

    assert_eq!(store.schema_version().unwrap(), LATEST_SCHEMA_VERSION);

    let mut statement = store
        .connection()
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table'")
        .unwrap();
    let tables: HashSet<String> = statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();

    for table in [
        "workspaces",
        "tasks",
        "task_criteria",
        "requirements",
        "agent_invitations",
        "agent_sessions",
        "task_participants",
        "agent_conclusions",
        "work_items",
        "work_claims",
        "work_attempts",
        "artifacts",
        "evidence",
        "events",
        "task_completions",
        "idempotency_records",
        "agent_action_proposals",
        "integration_preparations",
        "await_delivery_outbox",
        "blocking_fingerprints",
        "stage_handoffs",
        "external_waits",
        "external_wait_delivery_outbox",
        "proposal_consolidation_requests",
        "speculative_work",
        "chat_messages",
        "chat_message_receipts",
        "chat_inbox_states",
        "task_integration_integrity",
    ] {
        assert!(tables.contains(table), "missing schema table {table}");
    }

    let mut evidence_columns_statement = store
        .connection()
        .prepare("SELECT name FROM pragma_table_info('evidence')")
        .unwrap();
    let evidence_columns: HashSet<String> = evidence_columns_statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    for column in [
        "subject_type",
        "subject_id",
        "repository_identity",
        "commit_hash",
        "verifier",
        "environment_json",
        "created_by_session_id",
        "observed_at",
    ] {
        assert!(
            evidence_columns.contains(column),
            "missing evidence provenance column {column}"
        );
    }

    let session_columns: HashSet<String> = store
        .connection()
        .prepare("SELECT name FROM pragma_table_info('agent_sessions')")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(session_columns.contains("executor_identity"));
    assert!(session_columns.contains("executor_identity_source"));

    let work_item_columns: HashSet<String> = store
        .connection()
        .prepare("SELECT name FROM pragma_table_info('work_items')")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(work_item_columns.contains("review_policy"));

    let invalidation_columns: HashSet<String> = store
        .connection()
        .prepare("SELECT name FROM pragma_table_info('invalidations')")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(invalidation_columns.contains("management_mode"));
}

#[test]
fn migration_is_idempotent() {
    let directory = tempdir().unwrap();
    let database_path = directory.path().join("sessionbridge.db");

    SqliteStore::migrate(&database_path).unwrap();
    SqliteStore::migrate(&database_path).unwrap();

    let store = SqliteStore::open(&database_path).unwrap();
    assert_eq!(store.schema_version().unwrap(), LATEST_SCHEMA_VERSION);
}

#[test]
fn migration_backs_up_an_existing_older_database_before_upgrading_it() {
    let directory = tempdir().unwrap();
    let database_path = directory.path().join("sessionbridge.db");
    let connection = Connection::open(&database_path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE legacy_data (value TEXT NOT NULL);
             INSERT INTO legacy_data (value) VALUES ('preserved');",
        )
        .unwrap();
    drop(connection);

    SqliteStore::migrate(&database_path).unwrap();

    let backup_path = directory.path().join("sessionbridge.db.v0.backup");
    let backup =
        Connection::open_with_flags(&backup_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let value: String = backup
        .query_row("SELECT value FROM legacy_data", [], |row| row.get(0))
        .unwrap();
    let backup_schema_version: i32 = backup
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();

    assert_eq!(value, "preserved");
    assert_eq!(backup_schema_version, 0);
}

#[test]
fn open_rejects_an_unmigrated_database() {
    let directory = tempdir().unwrap();
    let database_path = directory.path().join("sessionbridge.db");
    Connection::open(&database_path).unwrap();

    let error = match SqliteStore::open(&database_path) {
        Ok(_) => panic!("unmigrated database must be rejected"),
        Err(error) => error,
    };

    assert_eq!(error.code(), ErrorCode::SchemaUpgradeRequired);
    assert_eq!(error.details()["actual_schema_version"], 0);
    assert_eq!(
        error.details()["expected_schema_version"],
        LATEST_SCHEMA_VERSION
    );
}

#[test]
fn open_and_migrate_reject_a_schema_newer_than_the_binary() {
    let directory = tempdir().unwrap();
    let database_path = directory.path().join("sessionbridge.db");
    let connection = Connection::open(&database_path).unwrap();
    connection
        .pragma_update(None, "user_version", LATEST_SCHEMA_VERSION + 1)
        .unwrap();
    drop(connection);

    let open_error = match SqliteStore::open(&database_path) {
        Ok(_) => panic!("newer schema must be rejected on open"),
        Err(error) => error,
    };
    assert_eq!(open_error.code(), ErrorCode::SchemaTooNew);

    let migrate_error = SqliteStore::migrate(&database_path).unwrap_err();
    assert_eq!(migrate_error.code(), ErrorCode::SchemaTooNew);
}
