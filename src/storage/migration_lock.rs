use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};

use fs4::FileExt;

use crate::{Result, SessionBridgeError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LockMode {
    Shared,
    Exclusive,
}

#[derive(Debug)]
pub struct MigrationLockGuard {
    file: File,
}

impl MigrationLockGuard {
    pub fn shared(path: &Path) -> Result<Self> {
        Self::acquire(path, LockMode::Shared)
    }

    pub fn exclusive(path: &Path) -> Result<Self> {
        Self::acquire(path, LockMode::Exclusive)
    }

    fn acquire(path: &Path, mode: LockMode) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| SessionBridgeError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|source| SessionBridgeError::Io {
                path: path.to_path_buf(),
                source,
            })?;

        match mode {
            LockMode::Shared => FileExt::lock_shared(&file),
            LockMode::Exclusive => FileExt::lock(&file),
        }
        .map_err(|source| SessionBridgeError::Io {
            path: path.to_path_buf(),
            source,
        })?;

        Ok(Self { file })
    }
}

impl Drop for MigrationLockGuard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

pub fn migration_lock_path_for(database_path: &Path) -> PathBuf {
    database_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("migration.lock")
}
