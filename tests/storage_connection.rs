use sessionbridge::storage::SqliteStore;
use tempfile::tempdir;

fn migrated_store() -> (tempfile::TempDir, SqliteStore) {
    let directory = tempdir().unwrap();
    let database_path = directory.path().join("sessionbridge.db");
    SqliteStore::migrate(&database_path).unwrap();
    let store = SqliteStore::open(&database_path).unwrap();
    (directory, store)
}

#[test]
fn every_store_connection_enforces_required_sqlite_pragmas() {
    let (_directory, store) = migrated_store();
    let connection = store.connection();

    let journal_mode: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    let foreign_keys: i64 = connection
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .unwrap();
    let synchronous: i64 = connection
        .query_row("PRAGMA synchronous", [], |row| row.get(0))
        .unwrap();
    let busy_timeout: i64 = connection
        .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
        .unwrap();

    assert_eq!(journal_mode, "wal");
    assert_eq!(foreign_keys, 1);
    assert_eq!(synchronous, 2);
    assert_eq!(busy_timeout, 5_000);
}

#[test]
fn foreign_keys_are_enforced_instead_of_only_configured() {
    let (_directory, store) = migrated_store();

    let error = store
        .connection()
        .execute(
            "INSERT INTO tasks (
                id, workspace_id, title, phase, autonomy_policy, completion_policy,
                created_at, updated_at
             ) VALUES ('task', 'missing', 'Task', 'active', 'manual',
                       'all_workers_concluded', 1, 1)",
            [],
        )
        .unwrap_err();

    assert_eq!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::ConstraintViolation)
    );
}

#[test]
fn immediate_transactions_support_commit_and_rollback() {
    let (_directory, mut store) = migrated_store();

    {
        let transaction = store.begin_immediate().unwrap();
        transaction
            .execute(
                "INSERT INTO workspaces (id, name, repository_identity, created_at)
                 VALUES ('committed', 'Committed', 'repo:committed', 1)",
                [],
            )
            .unwrap();
        transaction.commit().unwrap();
    }

    {
        let transaction = store.begin_immediate().unwrap();
        transaction
            .execute(
                "INSERT INTO workspaces (id, name, repository_identity, created_at)
                 VALUES ('rolled-back', 'Rolled Back', 'repo:rolled-back', 1)",
                [],
            )
            .unwrap();
    }

    let committed: i64 = store
        .connection()
        .query_row(
            "SELECT count(*) FROM workspaces WHERE id = 'committed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let rolled_back: i64 = store
        .connection()
        .query_row(
            "SELECT count(*) FROM workspaces WHERE id = 'rolled-back'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(committed, 1);
    assert_eq!(rolled_back, 0);
}
