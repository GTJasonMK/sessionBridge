use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};

use rusqlite::{
    Connection, OpenFlags, Transaction, TransactionBehavior,
    backup::{Backup, StepResult},
};

use crate::{ErrorCode, Result, SessionBridgeError};

use super::{
    migration_lock::{MigrationLockGuard, migration_lock_path_for},
    migrations::{LATEST_SCHEMA_VERSION, MIGRATIONS},
};

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

pub struct SqliteStore {
    connection: Connection,
    _migration_lock: MigrationLockGuard,
}

impl SqliteStore {
    pub fn open(path: &Path) -> Result<Self> {
        let lock_path = migration_lock_path_for(path);
        Self::open_with_lock(path, &lock_path)
    }

    pub fn open_with_lock(path: &Path, lock_path: &Path) -> Result<Self> {
        tracing::debug!(
            event = "database_open_started",
            database_path = %path.display(),
            migration_lock_path = %lock_path.display(),
            "opening SessionBridge database"
        );
        let migration_lock = MigrationLockGuard::shared(lock_path)?;
        let connection = open_connection(path, false)?;
        configure_connection(&connection)?;
        require_current_schema(&connection, path)?;
        tracing::debug!(
            event = "database_open_completed",
            database_path = %path.display(),
            schema_version = LATEST_SCHEMA_VERSION,
            "SessionBridge database opened"
        );

        Ok(Self {
            connection,
            _migration_lock: migration_lock,
        })
    }

    pub fn migrate(path: &Path) -> Result<()> {
        let lock_path = migration_lock_path_for(path);
        Self::migrate_with_lock(path, &lock_path)
    }

    pub fn migrate_with_lock(path: &Path, lock_path: &Path) -> Result<()> {
        tracing::info!(
            event = "database_migration_started",
            database_path = %path.display(),
            migration_lock_path = %lock_path.display(),
            target_schema_version = LATEST_SCHEMA_VERSION,
            "database migration check started"
        );
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| SessionBridgeError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let _migration_lock = MigrationLockGuard::exclusive(lock_path)?;
        let database_existed = path.is_file();
        let mut connection = open_connection(path, true)?;
        configure_connection(&connection)?;

        let current = schema_version(&connection)?;
        if current > LATEST_SCHEMA_VERSION {
            return Err(schema_too_new(path, current));
        }

        MIGRATIONS.validate()?;
        if current < LATEST_SCHEMA_VERSION {
            if database_existed {
                back_up_before_migration(&connection, path, current)?;
            }
            connection.set_transaction_behavior(TransactionBehavior::Exclusive);
            MIGRATIONS.to_latest(&mut connection)?;
        }

        let migrated = schema_version(&connection)?;
        if migrated != LATEST_SCHEMA_VERSION {
            return Err(SessionBridgeError::domain(
                ErrorCode::MigrationFailed,
                format!(
                    "database migration stopped at schema version {migrated}; expected {LATEST_SCHEMA_VERSION}"
                ),
            )
            .with_details(serde_json::json!({
                "database_path": path,
                "actual_schema_version": migrated,
                "expected_schema_version": LATEST_SCHEMA_VERSION,
            })));
        }

        tracing::info!(
            event = "database_migration_completed",
            database_path = %path.display(),
            previous_schema_version = current,
            schema_version = migrated,
            "database migration check completed"
        );
        Ok(())
    }

    pub fn begin_immediate(&mut self) -> Result<Transaction<'_>> {
        tracing::trace!(event = "database_transaction_begin", behavior = "immediate");
        Ok(self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?)
    }

    pub fn schema_version(&self) -> Result<i32> {
        schema_version(&self.connection)
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    pub fn connection_mut(&mut self) -> &mut Connection {
        &mut self.connection
    }
}

fn open_connection(path: &Path, create: bool) -> Result<Connection> {
    let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    if create {
        flags |= OpenFlags::SQLITE_OPEN_CREATE;
    }
    Ok(Connection::open_with_flags(path, flags)?)
}

fn configure_connection(connection: &Connection) -> Result<()> {
    connection.busy_timeout(BUSY_TIMEOUT)?;

    let journal_mode: String =
        connection.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    if !journal_mode.eq_ignore_ascii_case("wal") {
        return Err(SessionBridgeError::domain(
            ErrorCode::Database,
            format!("SQLite refused WAL journal mode and returned '{journal_mode}'"),
        )
        .with_details(serde_json::json!({
            "expected_journal_mode": "wal",
            "actual_journal_mode": journal_mode,
        })));
    }

    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.pragma_update(None, "synchronous", "FULL")?;

    let foreign_keys: i32 = connection.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    if foreign_keys != 1 {
        return Err(SessionBridgeError::domain(
            ErrorCode::Database,
            "SQLite foreign key enforcement could not be enabled",
        ));
    }

    let synchronous: i32 = connection.query_row("PRAGMA synchronous", [], |row| row.get(0))?;
    if synchronous != 2 {
        return Err(SessionBridgeError::domain(
            ErrorCode::Database,
            format!("SQLite synchronous mode is {synchronous}; expected FULL (2)"),
        ));
    }

    Ok(())
}

fn schema_version(connection: &Connection) -> Result<i32> {
    Ok(connection.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

fn require_current_schema(connection: &Connection, path: &Path) -> Result<()> {
    let current = schema_version(connection)?;
    if current < LATEST_SCHEMA_VERSION {
        return Err(SessionBridgeError::domain(
            ErrorCode::SchemaUpgradeRequired,
            format!(
                "database schema version {current} is older than required version {LATEST_SCHEMA_VERSION}; run sessionbridge migrate"
            ),
        )
        .with_details(serde_json::json!({
            "database_path": path,
            "actual_schema_version": current,
            "expected_schema_version": LATEST_SCHEMA_VERSION,
        })));
    }
    if current > LATEST_SCHEMA_VERSION {
        return Err(schema_too_new(path, current));
    }
    Ok(())
}

fn schema_too_new(path: &Path, current: i32) -> SessionBridgeError {
    SessionBridgeError::domain(
        ErrorCode::SchemaTooNew,
        format!(
            "database schema version {current} is newer than supported version {LATEST_SCHEMA_VERSION}"
        ),
    )
    .with_details(serde_json::json!({
        "database_path": path,
        "actual_schema_version": current,
        "expected_schema_version": LATEST_SCHEMA_VERSION,
    }))
}

fn back_up_before_migration(
    source: &Connection,
    database_path: &Path,
    schema_version: i32,
) -> Result<()> {
    let backup_path = migration_backup_path(database_path, schema_version);
    let temporary_path = temporary_backup_path(&backup_path);
    remove_file_if_present(&temporary_path)?;

    let backup_result = (|| {
        let mut destination = Connection::open(&temporary_path)?;
        {
            let backup = Backup::new(source, &mut destination)?;
            match backup.step(-1)? {
                StepResult::Done => {}
                StepResult::Busy | StepResult::Locked => {
                    return Err(SessionBridgeError::retryable(
                        ErrorCode::DatabaseBusy,
                        "database became busy while creating the pre-migration backup",
                    )
                    .with_details(serde_json::json!({
                        "database_path": database_path,
                        "backup_path": backup_path,
                    })));
                }
                _ => {
                    return Err(SessionBridgeError::domain(
                        ErrorCode::MigrationFailed,
                        "SQLite did not finish the pre-migration backup",
                    )
                    .with_details(serde_json::json!({
                        "database_path": database_path,
                        "backup_path": backup_path,
                    })));
                }
            }
        }

        let integrity: String =
            destination.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if integrity != "ok" {
            return Err(SessionBridgeError::domain(
                ErrorCode::MigrationFailed,
                format!("pre-migration backup failed integrity_check: {integrity}"),
            )
            .with_details(serde_json::json!({
                "database_path": database_path,
                "backup_path": backup_path,
            })));
        }

        drop(destination);
        remove_file_if_present(&backup_path)?;
        std::fs::rename(&temporary_path, &backup_path).map_err(|source| {
            SessionBridgeError::Io {
                path: backup_path.clone(),
                source,
            }
        })?;
        Ok(())
    })();

    if backup_result.is_err() {
        let _ = std::fs::remove_file(&temporary_path);
    }
    backup_result
}

fn migration_backup_path(database_path: &Path, schema_version: i32) -> PathBuf {
    let mut file_name = database_path
        .file_name()
        .map_or_else(|| OsString::from("sessionbridge.db"), OsString::from);
    file_name.push(format!(".v{schema_version}.backup"));
    database_path.with_file_name(file_name)
}

fn temporary_backup_path(backup_path: &Path) -> PathBuf {
    let mut file_name = backup_path
        .file_name()
        .map_or_else(|| OsString::from("sessionbridge.backup"), OsString::from);
    file_name.push(".tmp");
    backup_path.with_file_name(file_name)
}

fn remove_file_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(SessionBridgeError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}
