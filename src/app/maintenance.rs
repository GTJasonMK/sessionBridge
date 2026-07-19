use std::{path::PathBuf, process::Command};

use rusqlite::OptionalExtension;
use serde::Serialize;

use crate::{
    Result,
    error::{ErrorCode, SessionBridgeError},
    git::GitRepository,
    logging,
    storage::{LATEST_SCHEMA_VERSION, SqliteStore},
    workspace::{LocatedWorkspace, RepositoryKind},
};

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticStatus {
    Pass,
    Warning,
    Fail,
}

#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub code: String,
    pub status: DiagnosticStatus,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorResult {
    pub healthy: bool,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MigrationResult {
    pub database_path: PathBuf,
    pub schema_version: i32,
}

pub fn migrate(workspace: &LocatedWorkspace) -> Result<MigrationResult> {
    SqliteStore::migrate_with_lock(
        &workspace.paths.database_path,
        &workspace.paths.migration_lock_path,
    )?;
    let store = SqliteStore::open_with_lock(
        &workspace.paths.database_path,
        &workspace.paths.migration_lock_path,
    )?;
    Ok(MigrationResult {
        database_path: workspace.paths.database_path.clone(),
        schema_version: store.schema_version()?,
    })
}

pub fn doctor(workspace: &LocatedWorkspace) -> Result<DoctorResult> {
    let mut diagnostics = Vec::new();
    diagnostics.push(pass(
        "WORKSPACE_CONFIG",
        "workspace configuration is readable and protocol-compatible",
        Some(workspace.paths.config_path.clone()),
    ));

    match SqliteStore::open_with_lock(
        &workspace.paths.database_path,
        &workspace.paths.migration_lock_path,
    ) {
        Ok(store) => {
            let version = store.schema_version()?;
            let integrity: String =
                store
                    .connection()
                    .query_row("PRAGMA quick_check", [], |row| row.get(0))?;
            if version == LATEST_SCHEMA_VERSION && integrity == "ok" {
                diagnostics.push(pass(
                    "DATABASE",
                    &format!("SQLite schema {version} is current and quick_check passed"),
                    Some(workspace.paths.database_path.clone()),
                ));
            } else {
                diagnostics.push(fail(
                    "DATABASE",
                    &format!(
                        "SQLite schema/integrity mismatch: version={version}, quick_check={integrity}"
                    ),
                    Some(workspace.paths.database_path.clone()),
                ));
            }
            let binding: Option<String> = store
                .connection()
                .query_row(
                    "SELECT repository_identity FROM workspaces WHERE id = ?1",
                    [workspace.config.project_id.to_string()],
                    |row| row.get(0),
                )
                .optional()?;
            if binding.as_deref() == Some(&workspace.paths.repository_instance_id) {
                diagnostics.push(pass(
                    "REPOSITORY_BINDING",
                    "database repository identity matches this repository instance",
                    None,
                ));
            } else {
                diagnostics.push(fail(
                    "REPOSITORY_BINDING",
                    "database repository identity is missing or mismatched",
                    None,
                ));
            }
        }
        Err(error) => diagnostics.push(fail(
            "DATABASE",
            &error.to_string(),
            Some(workspace.paths.database_path.clone()),
        )),
    }

    check_private_directory(&workspace.paths.state_dir, &mut diagnostics);
    check_private_directory(&workspace.paths.contexts_dir, &mut diagnostics);
    check_private_directory(&workspace.paths.invites_dir, &mut diagnostics);
    check_private_directory(&workspace.paths.prompts_dir, &mut diagnostics);
    check_private_directory(&workspace.paths.launches_dir, &mut diagnostics);
    let log_dir = logging::log_directory()?;
    check_private_directory(&log_dir, &mut diagnostics);
    diagnostics.push(pass(
        "LOG_DIRECTORY",
        &format!(
            "structured JSONL logging is enabled with daily UTC rotation and at most {} files",
            logging::MAX_LOG_FILES
        ),
        Some(log_dir),
    ));

    match git_version() {
        Ok(version) if workspace.repository.kind == RepositoryKind::Git => {
            diagnostics.push(pass(
                "GIT",
                &format!("{version}; Git repository metadata is available"),
                workspace.repository.git_common_dir.clone(),
            ));
            match GitRepository::open(&workspace.paths.root)
                .and_then(|repository| repository.require_initial_commit())
            {
                Ok(commit) => diagnostics.push(pass(
                    "GIT_HEAD",
                    &format!("Git HEAD resolves to {commit}; automatic worktrees are available"),
                    Some(workspace.paths.root.clone()),
                )),
                Err(error) => diagnostics.push(fail(
                    "GIT_HEAD",
                    &error.to_string(),
                    Some(workspace.paths.root.clone()),
                )),
            }
        }
        Ok(version) => diagnostics.push(Diagnostic {
            code: "GIT".into(),
            status: DiagnosticStatus::Warning,
            message: format!(
                "{version}; workspace is not a Git repository, so coordination is advisory and automatic worktrees are unavailable"
            ),
            path: Some(workspace.paths.root.clone()),
        }),
        Err(error) => diagnostics.push(fail("GIT", &error.to_string(), None)),
    }

    for entry in directory_entries(&workspace.paths.join_attempts_dir)? {
        diagnostics.push(Diagnostic {
            code: "JOIN_ATTEMPT_RECOVERY".into(),
            status: DiagnosticStatus::Warning,
            message: "unfinished join attempt requires inspection or recovery".into(),
            path: Some(entry),
        });
    }

    let healthy = diagnostics
        .iter()
        .all(|diagnostic| diagnostic.status != DiagnosticStatus::Fail);
    Ok(DoctorResult {
        healthy,
        diagnostics,
    })
}

fn git_version() -> Result<String> {
    let output = Command::new("git")
        .arg("--version")
        .output()
        .map_err(|source| SessionBridgeError::Io {
            path: PathBuf::from("git"),
            source,
        })?;
    if !output.status.success() {
        return Err(SessionBridgeError::domain(
            ErrorCode::Internal,
            format!("git --version exited with {}", output.status),
        ));
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_owned())
        .map_err(|error| {
            SessionBridgeError::domain(
                ErrorCode::Internal,
                format!("git --version returned non-UTF-8 output: {error}"),
            )
        })
}

fn directory_entries(path: &std::path::Path) -> Result<Vec<PathBuf>> {
    let mut entries = std::fs::read_dir(path)
        .map_err(|source| SessionBridgeError::Io {
            path: path.to_path_buf(),
            source,
        })?
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|source| SessionBridgeError::Io {
                    path: path.to_path_buf(),
                    source,
                })
        })
        .collect::<Result<Vec<_>>>()?;
    entries.sort();
    Ok(entries)
}

#[cfg(unix)]
fn check_private_directory(path: &std::path::Path, diagnostics: &mut Vec<Diagnostic>) {
    use std::os::unix::fs::PermissionsExt;

    match std::fs::metadata(path) {
        Ok(metadata) => {
            let mode = metadata.permissions().mode() & 0o777;
            if mode & 0o077 == 0 {
                diagnostics.push(pass(
                    "PRIVATE_DIRECTORY",
                    &format!("private directory permissions are {mode:03o}"),
                    Some(path.to_path_buf()),
                ));
            } else {
                diagnostics.push(fail(
                    "PRIVATE_DIRECTORY",
                    &format!("private directory permissions are too broad: {mode:03o}"),
                    Some(path.to_path_buf()),
                ));
            }
        }
        Err(error) => diagnostics.push(fail(
            "PRIVATE_DIRECTORY",
            &error.to_string(),
            Some(path.to_path_buf()),
        )),
    }
}

#[cfg(not(unix))]
fn check_private_directory(path: &std::path::Path, diagnostics: &mut Vec<Diagnostic>) {
    diagnostics.push(Diagnostic {
        code: "PRIVATE_DIRECTORY".into(),
        status: DiagnosticStatus::Warning,
        message: "private directory ACL is best-effort on this platform".into(),
        path: Some(path.to_path_buf()),
    });
}

fn pass(code: &str, message: &str, path: Option<PathBuf>) -> Diagnostic {
    Diagnostic {
        code: code.into(),
        status: DiagnosticStatus::Pass,
        message: message.into(),
        path,
    }
}

fn fail(code: &str, message: &str, path: Option<PathBuf>) -> Diagnostic {
    Diagnostic {
        code: code.into(),
        status: DiagnosticStatus::Fail,
        message: message.into(),
        path,
    }
}
