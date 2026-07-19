use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use crate::{
    Result,
    app::events::{self, EventActor, EventEntity},
    clock::{Clock, unix_micros},
    error::{ErrorCode, SessionBridgeError},
    storage::{LATEST_SCHEMA_VERSION, SqliteStore},
    workspace::{LocatedWorkspace, RepositoryKind, initialize},
};

#[derive(Debug, Clone, Serialize)]
pub struct InitResult {
    pub project_id: crate::domain::WorkspaceId,
    pub name: String,
    pub root: PathBuf,
    pub config_path: PathBuf,
    pub repository_kind: RepositoryKind,
    pub repository_instance_id: String,
    pub state_dir: PathBuf,
    pub database_path: PathBuf,
    pub schema_version: i32,
    pub created: bool,
    pub event_cursor: Option<i64>,
}

pub struct WorkspaceService<C> {
    clock: C,
}

impl<C: Clock> WorkspaceService<C> {
    pub fn new(clock: C) -> Self {
        Self { clock }
    }

    pub fn initialize(
        &self,
        root: impl AsRef<Path>,
        name: impl Into<String>,
        state_root: Option<&Path>,
    ) -> Result<InitResult> {
        let workspace = initialize(root, name, state_root)?;
        SqliteStore::migrate_with_lock(
            &workspace.paths.database_path,
            &workspace.paths.migration_lock_path,
        )?;
        let mut store = SqliteStore::open_with_lock(
            &workspace.paths.database_path,
            &workspace.paths.migration_lock_path,
        )?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let existing: Option<(String, String)> = transaction
            .query_row(
                "SELECT name, repository_identity FROM workspaces WHERE id = ?1",
                [workspace.config.project_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        let (created, event_cursor) = match existing {
            Some((name, repository_identity)) => {
                if name != workspace.config.name
                    || repository_identity != workspace.paths.repository_instance_id
                {
                    return Err(SessionBridgeError::domain(
                        ErrorCode::InvalidInput,
                        "workspace database binding does not match project configuration",
                    )
                    .with_details(serde_json::json!({
                        "project_id": workspace.config.project_id,
                        "config_name": workspace.config.name,
                        "database_name": name,
                        "config_repository_identity": workspace.paths.repository_instance_id,
                        "database_repository_identity": repository_identity,
                    })));
                }
                (false, None)
            }
            None => {
                transaction.execute(
                    "INSERT INTO workspaces (id, name, repository_identity, created_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        workspace.config.project_id.to_string(),
                        workspace.config.name,
                        workspace.paths.repository_instance_id,
                        unix_micros(now),
                    ],
                )?;
                let cursor = events::append(
                    &transaction,
                    workspace.config.project_id,
                    None,
                    "workspace.initialized",
                    EventActor {
                        kind: "user",
                        id: Some(&workspace.config.project_id.to_string()),
                    },
                    EventEntity {
                        kind: "workspace",
                        id: Some(&workspace.config.project_id.to_string()),
                    },
                    &serde_json::json!({
                        "name": workspace.config.name,
                        "repository_identity": workspace.paths.repository_instance_id,
                    }),
                    now,
                )?;
                (true, Some(cursor))
            }
        };
        transaction.commit()?;

        Ok(result(&workspace, created, event_cursor))
    }
}

fn result(workspace: &LocatedWorkspace, created: bool, event_cursor: Option<i64>) -> InitResult {
    InitResult {
        project_id: workspace.config.project_id,
        name: workspace.config.name.clone(),
        root: workspace.paths.root.clone(),
        config_path: workspace.paths.config_path.clone(),
        repository_kind: workspace.repository.kind,
        repository_instance_id: workspace.paths.repository_instance_id.clone(),
        state_dir: workspace.paths.state_dir.clone(),
        database_path: workspace.paths.database_path.clone(),
        schema_version: LATEST_SCHEMA_VERSION,
        created,
        event_cursor,
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;
    use time::OffsetDateTime;

    use crate::clock::FakeClock;

    use super::*;

    #[test]
    fn initialization_is_recoverable_and_idempotent() {
        let project = TempDir::new().unwrap();
        let state = TempDir::new().unwrap();
        let service = WorkspaceService::new(FakeClock::new(OffsetDateTime::UNIX_EPOCH));

        let first = service
            .initialize(project.path(), "demo", Some(state.path()))
            .unwrap();
        let second = service
            .initialize(project.path(), "demo", Some(state.path()))
            .unwrap();

        assert!(first.created);
        assert!(!second.created);
        assert_eq!(first.project_id, second.project_id);
        assert_eq!(first.database_path, second.database_path);
        assert_eq!(first.event_cursor, Some(1));
        assert_eq!(second.event_cursor, None);
    }
}
