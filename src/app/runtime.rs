use std::path::{Path, PathBuf};

use crate::{
    Result,
    domain::SessionContext,
    error::{ErrorCode, SessionBridgeError},
    workspace::{LocatedWorkspace, PROTOCOL_VERSION, read_json_private},
};

pub const CONTEXT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct AgentRuntime {
    pub context_path: PathBuf,
    pub context: SessionContext,
}

impl AgentRuntime {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let context_path =
            std::fs::canonicalize(path).map_err(|source| SessionBridgeError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        let context: SessionContext = read_json_private(&context_path)?;
        validate_context(&context)?;
        tracing::debug!(
            event = "session_context_loaded",
            context_path = %context_path.display(),
            workspace_id = %context.workspace_id,
            task_id = %context.task_id,
            session_id = %context.session_id,
            database_path = %context.database_path,
            "private Agent session context loaded"
        );
        Ok(Self {
            context_path,
            context,
        })
    }
}

pub fn locate_workspace(explicit: Option<&Path>) -> Result<LocatedWorkspace> {
    let start = match explicit {
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir().map_err(|source| SessionBridgeError::Io {
            path: PathBuf::from("."),
            source,
        })?,
    };
    LocatedWorkspace::discover(start)
}

fn validate_context(context: &SessionContext) -> Result<()> {
    if context.schema_version != CONTEXT_SCHEMA_VERSION {
        let code = if context.schema_version > CONTEXT_SCHEMA_VERSION {
            ErrorCode::SchemaTooNew
        } else {
            ErrorCode::SchemaUpgradeRequired
        };
        return Err(SessionBridgeError::domain(
            code,
            format!(
                "session context schema {} does not match supported schema {}",
                context.schema_version, CONTEXT_SCHEMA_VERSION
            ),
        ));
    }
    if context.protocol_version != PROTOCOL_VERSION {
        return Err(SessionBridgeError::domain(
            ErrorCode::ProtocolVersionMismatch,
            format!(
                "session context protocol {} does not match CLI protocol {}",
                context.protocol_version, PROTOCOL_VERSION
            ),
        ));
    }
    if context.token.is_empty() {
        return Err(SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            "session context token is empty",
        ));
    }
    let database_path = Path::new(&context.database_path);
    if !database_path.is_absolute() {
        return Err(SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            "session context database path must be absolute",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use crate::{
        domain::{SessionId, TaskId, WorkspaceId},
        workspace::atomic_write_json_private,
    };

    use super::*;

    fn context(database_path: &Path) -> SessionContext {
        SessionContext {
            schema_version: CONTEXT_SCHEMA_VERSION,
            protocol_version: PROTOCOL_VERSION,
            workspace_id: WorkspaceId::new(),
            repository_instance_id: "repository-instance".into(),
            task_id: TaskId::new(),
            session_id: SessionId::new(),
            token: "secret".into(),
            database_path: database_path.display().to_string(),
        }
    }

    #[test]
    fn loads_valid_context() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("context.json");
        atomic_write_json_private(&path, &context(&directory.path().join("state.db"))).unwrap();

        let runtime = AgentRuntime::load(&path).unwrap();

        assert_eq!(runtime.context_path, path);
    }

    #[test]
    fn rejects_relative_database_path() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("context.json");
        atomic_write_json_private(&path, &context(Path::new("state.db"))).unwrap();

        let error = AgentRuntime::load(&path).unwrap_err();

        assert_eq!(error.code(), ErrorCode::InvalidInput);
    }
}
