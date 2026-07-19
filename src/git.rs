use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Instant,
};

use serde::Serialize;

use crate::{
    Result,
    error::{ErrorCode, SessionBridgeError},
};

#[derive(Debug, Clone, Serialize)]
pub struct WorktreeMetadata {
    pub path: PathBuf,
    pub branch: String,
    pub base_commit: String,
    pub head_commit: String,
}

#[derive(Debug, Clone)]
pub struct GitRepository {
    root: PathBuf,
}

impl GitRepository {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root =
            std::fs::canonicalize(root.as_ref()).map_err(|source| SessionBridgeError::Io {
                path: root.as_ref().to_path_buf(),
                source,
            })?;
        let repository = Self { root };
        repository.git_text(["rev-parse", "--show-toplevel"])?;
        Ok(repository)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn head_commit(&self) -> Result<String> {
        self.git_text(["rev-parse", "HEAD"])
    }

    pub fn require_initial_commit(&self) -> Result<String> {
        self.head_commit().map_err(|error| {
            SessionBridgeError::domain(
                ErrorCode::WorktreeConflict,
                "automatic worktree provisioning requires at least one Git commit",
            )
            .with_details(serde_json::json!({
                "repository": self.root,
                "recovery": "commit the project baseline before creating worker invitations",
                "cause": error.to_string(),
            }))
        })
    }

    pub fn current_branch(&self) -> Result<Option<String>> {
        let output = self.git_output(["symbolic-ref", "--quiet", "--short", "HEAD"])?;
        if output.status.success() {
            Ok(Some(decode_trimmed_stdout(output, "current Git branch")?))
        } else if output.status.code() == Some(1) {
            Ok(None)
        } else {
            Err(git_failure("read current branch", &output))
        }
    }

    pub fn common_dir(&self) -> Result<PathBuf> {
        let value = self.git_text(["rev-parse", "--git-common-dir"])?;
        let path = PathBuf::from(value);
        let path = if path.is_absolute() {
            path
        } else {
            self.root.join(path)
        };
        std::fs::canonicalize(&path).map_err(|source| SessionBridgeError::Io { path, source })
    }

    pub fn resolve_revision(&self, revision: &str) -> Result<String> {
        if revision.trim().is_empty() || revision.chars().any(char::is_control) {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "Git revision cannot be empty or contain control characters",
            ));
        }
        self.git_text(["rev-parse", "--verify", revision])
    }

    pub fn require_ancestor(&self, ancestor: &str, target: &str) -> Result<(String, String)> {
        let ancestor_commit = self.resolve_revision(ancestor).map_err(|error| {
            SessionBridgeError::domain(
                ErrorCode::TargetBranchNotIntegrated,
                "ChangeSet head commit is not available in the target repository",
            )
            .with_details(serde_json::json!({
                "changeset_head": ancestor,
                "target_branch": target,
                "repository_path": self.root,
                "cause": error.to_string(),
            }))
        })?;
        let target_commit = self.resolve_revision(target).map_err(|error| {
            SessionBridgeError::domain(
                ErrorCode::TargetBranchNotIntegrated,
                "Task target branch cannot be resolved",
            )
            .with_details(serde_json::json!({
                "changeset_head": ancestor_commit,
                "target_branch": target,
                "repository_path": self.root,
                "cause": error.to_string(),
            }))
        })?;
        let output = self.git_output([
            OsStr::new("merge-base"),
            OsStr::new("--is-ancestor"),
            OsStr::new(&ancestor_commit),
            OsStr::new(&target_commit),
        ])?;
        if output.status.success() {
            return Ok((ancestor_commit, target_commit));
        }
        if output.status.code() == Some(1) {
            return Err(SessionBridgeError::domain(
                ErrorCode::TargetBranchNotIntegrated,
                "target branch does not contain the ChangeSet head commit",
            )
            .with_details(serde_json::json!({
                "changeset_head": ancestor_commit,
                "target_branch": target,
                "target_head": target_commit,
                "repository_path": self.root,
                "recovery": "merge or cherry-pick the ChangeSet into the target branch, then retry changeset integrate",
            })));
        }
        Err(git_failure("verify ChangeSet target ancestry", &output))
    }

    pub fn is_clean(&self) -> Result<bool> {
        let output =
            self.git_output(["status", "--porcelain=v1", "-z", "--untracked-files=all"])?;
        if !output.status.success() {
            return Err(git_failure("inspect worktree status", &output));
        }
        Ok(output.stdout.is_empty())
    }

    pub fn require_clean(&self) -> Result<()> {
        if !self.is_clean()? {
            return Err(SessionBridgeError::domain(
                ErrorCode::WorktreeDirty,
                format!("Git worktree is not clean: {}", self.root.display()),
            )
            .with_details(serde_json::json!({ "worktree_path": self.root })));
        }
        Ok(())
    }

    pub fn dirty_paths(&self) -> Result<Vec<String>> {
        let output = self.git_output([
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--no-renames",
        ])?;
        if !output.status.success() {
            return Err(git_failure("inspect worktree status", &output));
        }
        output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
            .map(|record| {
                let path = record.get(3..).ok_or_else(|| {
                    SessionBridgeError::domain(
                        ErrorCode::InvalidInput,
                        "Git status returned a malformed porcelain record",
                    )
                })?;
                String::from_utf8(path.to_vec()).map_err(|error| {
                    SessionBridgeError::domain(
                        ErrorCode::InvalidInput,
                        format!("dirty Git path is not valid UTF-8: {error}"),
                    )
                })
            })
            .collect()
    }

    pub fn changed_paths(&self, base_revision: &str, head_revision: &str) -> Result<Vec<PathBuf>> {
        let base = self.resolve_revision(base_revision)?;
        let head = self.resolve_revision(head_revision)?;
        let range = format!("{base}..{head}");
        let output = self.git_output([
            OsStr::new("diff"),
            OsStr::new("--name-only"),
            OsStr::new("-z"),
            OsStr::new("--no-renames"),
            OsStr::new(&range),
        ])?;
        if !output.status.success() {
            return Err(git_failure("list changed Git paths", &output));
        }
        output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|value| !value.is_empty())
            .map(|value| {
                String::from_utf8(value.to_vec())
                    .map(PathBuf::from)
                    .map_err(|error| {
                        SessionBridgeError::domain(
                            ErrorCode::InvalidInput,
                            format!("changed Git path is not valid UTF-8: {error}"),
                        )
                    })
            })
            .collect()
    }

    pub fn provision_worktree(
        &self,
        path: &Path,
        branch: &str,
        base_revision: Option<&str>,
    ) -> Result<WorktreeMetadata> {
        validate_branch_name(branch)?;
        if path.exists() {
            return Err(SessionBridgeError::domain(
                ErrorCode::WorktreeConflict,
                format!("worktree destination already exists: {}", path.display()),
            ));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| SessionBridgeError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let base_commit = match base_revision {
            Some(revision) => self.resolve_revision(revision)?,
            None => self.head_commit()?,
        };
        let arguments: Vec<OsString> = vec![
            OsString::from("worktree"),
            OsString::from("add"),
            OsString::from("--no-track"),
            OsString::from("-b"),
            OsString::from(branch),
            path.as_os_str().to_owned(),
            OsString::from(&base_commit),
        ];
        let output = self.git_output(arguments)?;
        if !output.status.success() {
            return Err(
                git_failure("create worktree", &output).with_details(serde_json::json!({
                    "worktree_path": path,
                    "branch": branch,
                    "base_commit": base_commit,
                })),
            );
        }

        let child = Self::open(path)?;
        let head_commit = child.head_commit()?;
        Ok(WorktreeMetadata {
            path: path.to_path_buf(),
            branch: branch.to_owned(),
            base_commit,
            head_commit,
        })
    }

    pub fn remove_worktree(&self, path: &Path) -> Result<()> {
        let output = self.git_output([
            OsStr::new("worktree"),
            OsStr::new("remove"),
            OsStr::new("--force"),
            path.as_os_str(),
        ])?;
        if !output.status.success() {
            return Err(git_failure("remove worktree", &output)
                .with_details(serde_json::json!({ "worktree_path": path })));
        }
        Ok(())
    }

    pub fn delete_branch(&self, branch: &str) -> Result<()> {
        validate_branch_name(branch)?;
        let output = self.git_output(["branch", "-D", branch])?;
        if !output.status.success() {
            return Err(git_failure("delete worktree branch", &output)
                .with_details(serde_json::json!({ "branch": branch })));
        }
        Ok(())
    }

    fn git_text<I, S>(&self, arguments: I) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self.git_output(arguments)?;
        if !output.status.success() {
            return Err(git_failure("run Git command", &output));
        }
        decode_trimmed_stdout(output, "Git output")
    }

    fn git_output<I, S>(&self, arguments: I) -> Result<Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let arguments = arguments
            .into_iter()
            .map(|value| value.as_ref().to_os_string())
            .collect::<Vec<_>>();
        let started = Instant::now();
        tracing::debug!(
            event = "git_command_started",
            repository = %self.root.display(),
            arguments = ?arguments,
            "Git command started"
        );
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(&arguments)
            .output()
            .map_err(|source| SessionBridgeError::Io {
                path: PathBuf::from("git"),
                source,
            })?;
        tracing::debug!(
            event = "git_command_completed",
            repository = %self.root.display(),
            arguments = ?arguments,
            success = output.status.success(),
            exit_code = output.status.code().unwrap_or(-1),
            elapsed_ms = started.elapsed().as_millis(),
            "Git command completed"
        );
        Ok(output)
    }
}

fn validate_branch_name(branch: &str) -> Result<()> {
    if branch.is_empty() || branch.chars().any(char::is_control) {
        return Err(SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            "Git branch name cannot be empty or contain control characters",
        ));
    }
    let output = Command::new("git")
        .args(["check-ref-format", "--branch", branch])
        .output()
        .map_err(|source| SessionBridgeError::Io {
            path: PathBuf::from("git"),
            source,
        })?;
    if !output.status.success() {
        return Err(SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            format!("invalid Git branch name: {branch}"),
        ));
    }
    Ok(())
}

fn decode_trimmed_stdout(output: Output, purpose: &str) -> Result<String> {
    let text = String::from_utf8(output.stdout).map_err(|error| {
        SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            format!("{purpose} is not valid UTF-8: {error}"),
        )
    })?;
    let text = text.trim();
    if text.is_empty() {
        return Err(SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            format!("{purpose} is empty"),
        ));
    }
    Ok(text.to_owned())
}

fn git_failure(operation: &str, output: &Output) -> SessionBridgeError {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    SessionBridgeError::domain(
        ErrorCode::WorktreeConflict,
        if stderr.is_empty() {
            format!("failed to {operation}: Git exited with {}", output.status)
        } else {
            format!("failed to {operation}: {stderr}")
        },
    )
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use tempfile::TempDir;

    use super::*;

    fn repository() -> (TempDir, GitRepository) {
        let directory = TempDir::new().unwrap();
        for arguments in [
            vec!["init", "--initial-branch=main"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "SessionBridge Test"],
        ] {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(directory.path())
                    .args(arguments)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        std::fs::write(directory.path().join("README.md"), "test\n").unwrap();
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(directory.path())
                .args(["add", "README.md"])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(directory.path())
                .args(["commit", "-m", "initial"])
                .status()
                .unwrap()
                .success()
        );
        let repository = GitRepository::open(directory.path()).unwrap();
        (directory, repository)
    }

    #[test]
    fn provisions_unique_clean_worktree() {
        let (directory, repository) = repository();
        let worktree = directory.path().join("agent-worktree");

        let metadata = repository
            .provision_worktree(&worktree, "sessionbridge/task/agent", Some("main"))
            .unwrap();

        assert_eq!(metadata.path, worktree);
        assert_eq!(metadata.branch, "sessionbridge/task/agent");
        assert_eq!(metadata.base_commit, metadata.head_commit);
        assert!(
            GitRepository::open(&metadata.path)
                .unwrap()
                .is_clean()
                .unwrap()
        );
    }

    #[test]
    fn dirty_worktree_is_rejected() {
        let (directory, repository) = repository();
        std::fs::write(directory.path().join("untracked"), "dirty").unwrap();

        let error = repository.require_clean().unwrap_err();

        assert_eq!(error.code(), ErrorCode::WorktreeDirty);
    }

    #[test]
    fn empty_repository_cannot_provision_worktrees() {
        let directory = TempDir::new().unwrap();
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(directory.path())
                .arg("init")
                .status()
                .unwrap()
                .success()
        );
        let repository = GitRepository::open(directory.path()).unwrap();

        let error = repository.require_initial_commit().unwrap_err();

        assert_eq!(error.code(), ErrorCode::WorktreeConflict);
        assert_eq!(
            error.details()["recovery"],
            "commit the project baseline before creating worker invitations"
        );
    }
}
