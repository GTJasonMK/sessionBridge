use std::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use sessionbridge::{
    ErrorCode,
    storage::{LATEST_SCHEMA_VERSION, SqliteStore},
};
use tempfile::tempdir;

#[test]
fn an_open_store_blocks_migration_until_its_shared_lock_is_released() {
    let directory = tempdir().unwrap();
    let database_path = directory.path().join("sessionbridge.db");
    SqliteStore::migrate(&database_path).unwrap();
    let store = SqliteStore::open(&database_path).unwrap();

    let (started_tx, started_rx) = mpsc::channel();
    let (finished_tx, finished_rx) = mpsc::channel();
    let migration_path = database_path.clone();
    let worker = thread::spawn(move || {
        started_tx.send(()).unwrap();
        let result = SqliteStore::migrate(&migration_path);
        finished_tx.send(result).unwrap();
    });

    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(
        finished_rx
            .recv_timeout(Duration::from_millis(150))
            .is_err(),
        "exclusive migration lock was acquired while a store held a shared lock"
    );

    drop(store);
    finished_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("migration did not resume after the shared lock was released")
        .unwrap();
    worker.join().unwrap();
}

#[test]
fn concurrent_migrations_are_serialized_and_both_succeed() {
    let directory = tempdir().unwrap();
    let database_path = directory.path().join("sessionbridge.db");
    let first_path = database_path.clone();
    let second_path = database_path.clone();

    let first = thread::spawn(move || SqliteStore::migrate(&first_path));
    let second = thread::spawn(move || SqliteStore::migrate(&second_path));

    first.join().unwrap().unwrap();
    second.join().unwrap().unwrap();

    let store = SqliteStore::open(&database_path).unwrap();
    assert_eq!(store.schema_version().unwrap(), LATEST_SCHEMA_VERSION);
}

#[test]
fn competing_immediate_transactions_fail_with_database_busy_after_the_timeout() {
    let directory = tempdir().unwrap();
    let database_path = directory.path().join("sessionbridge.db");
    SqliteStore::migrate(&database_path).unwrap();
    let mut first_store = SqliteStore::open(&database_path).unwrap();
    let mut second_store = SqliteStore::open(&database_path).unwrap();
    second_store
        .connection()
        .busy_timeout(Duration::from_millis(100))
        .unwrap();

    let first_transaction = first_store.begin_immediate().unwrap();
    let started_at = Instant::now();
    let error = match second_store.begin_immediate() {
        Ok(_) => panic!("the second writer must not acquire an immediate transaction"),
        Err(error) => error,
    };

    assert_eq!(error.code(), ErrorCode::DatabaseBusy);
    assert!(started_at.elapsed() >= Duration::from_millis(75));
    drop(first_transaction);
}
