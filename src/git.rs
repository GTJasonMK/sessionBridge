use crate::error::{Result, error};
use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::{Command, Output},
};

pub struct GitRepository {
    root: PathBuf,
}
impl GitRepository {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().canonicalize()?;
        let repo = Self { root };
        let top = PathBuf::from(repo.git_text(["rev-parse", "--show-toplevel"])?).canonicalize()?;
        if top != repo.root {
            return Err(error(
                "GIT_ROOT",
                "select the Git worktree root, not a subdirectory",
            ));
        }
        Ok(repo)
    }
    pub fn head_commit(&self) -> Result<String> {
        self.resolve_revision("HEAD")
    }
    pub fn require_initial_commit(&self) -> Result<String> {
        self.head_commit()
    }
    pub fn current_branch(&self) -> Result<Option<String>> {
        let output = self.git_output(["symbolic-ref", "--quiet", "--short", "HEAD"])?;
        match output.status.code() {
            Some(0) => Ok(Some(decode(output)?)),
            Some(1) => Ok(None),
            _ => Err(failure(&output)),
        }
    }
    pub fn common_dir(&self) -> Result<PathBuf> {
        let path = PathBuf::from(self.git_text(["rev-parse", "--git-common-dir"])?);
        Ok(self.root.join(path).canonicalize()?)
    }
    fn resolve_revision(&self, revision: &str) -> Result<String> {
        if revision.trim().is_empty() || revision.chars().any(char::is_control) {
            return Err(error("INVALID_INPUT", "invalid Git revision"));
        }
        self.git_text([
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{revision}^{{commit}}"),
        ])
    }
    pub fn is_ancestor(&self, ancestor: &str, target: &str) -> Result<bool> {
        let ancestor = self.resolve_revision(ancestor)?;
        let target = self.resolve_revision(target)?;
        let output = self.git_output(["merge-base", "--is-ancestor", &ancestor, &target])?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(failure(&output)),
        }
    }
    pub fn require_ancestor(&self, ancestor: &str, target: &str) -> Result<()> {
        if !self.is_ancestor(ancestor, target)? {
            return Err(error(
                "NOT_INTEGRATED",
                format!("{target} does not contain {ancestor}"),
            ));
        }
        Ok(())
    }
    pub fn is_clean(&self) -> Result<bool> {
        let output =
            self.git_output(["status", "--porcelain=v1", "-z", "--untracked-files=all"])?;
        if !output.status.success() {
            return Err(failure(&output));
        }
        Ok(output.stdout.is_empty())
    }
    pub fn require_clean(&self) -> Result<()> {
        if !self.is_clean()? {
            return Err(error(
                "WORKTREE_DIRTY",
                format!("Git worktree is not clean: {}", self.root.display()),
            ));
        }
        Ok(())
    }
    pub fn provision_worktree(&self, path: &Path, branch: &str, base: Option<&str>) -> Result<()> {
        if path.exists() {
            return Err(error(
                "WORKTREE_EXISTS",
                format!("worktree destination already exists: {}", path.display()),
            ));
        }
        let valid = self.git_output(["check-ref-format", "--branch", branch])?;
        if !valid.status.success() {
            return Err(failure(&valid));
        }
        let base = self.resolve_revision(base.unwrap_or("HEAD"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let args: Vec<OsString> = vec![
            "worktree".into(),
            "add".into(),
            "--no-track".into(),
            "-b".into(),
            branch.into(),
            path.as_os_str().into(),
            base.into(),
        ];
        let output = self.git_output(args)?;
        if !output.status.success() {
            return Err(failure(&output));
        }
        Ok(())
    }
    fn git_text<I, S>(&self, args: I) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self.git_output(args)?;
        if !output.status.success() {
            return Err(failure(&output));
        }
        decode(output)
    }
    fn git_output<I, S>(&self, args: I) -> Result<Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        Ok(Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .output()?)
    }
}
fn decode(output: Output) -> Result<String> {
    let value = String::from_utf8(output.stdout).map_err(|e| error("GIT", e.to_string()))?;
    let value = value.trim();
    if value.is_empty() {
        return Err(error("GIT", "Git returned an empty result"));
    }
    Ok(value.to_owned())
}
fn failure(output: &Output) -> crate::error::Error {
    error(
        "GIT",
        format!(
            "Git exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ),
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

        repository
            .provision_worktree(&worktree, "sessionbridge/task/agent", Some("main"))
            .unwrap();

        let child = GitRepository::open(&worktree).unwrap();
        assert_eq!(
            child.current_branch().unwrap().as_deref(),
            Some("sessionbridge/task/agent")
        );
        assert_eq!(
            child.head_commit().unwrap(),
            repository.head_commit().unwrap()
        );
        assert!(GitRepository::open(&worktree).unwrap().is_clean().unwrap());
    }

    #[test]
    fn dirty_worktree_is_rejected() {
        let (directory, repository) = repository();
        std::fs::write(directory.path().join("untracked"), "dirty").unwrap();

        let error = repository.require_clean().unwrap_err();

        assert_eq!(error.code, "WORKTREE_DIRTY");
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

        assert_eq!(error.code, "GIT");
        assert!(!error.message.is_empty());
    }
}
