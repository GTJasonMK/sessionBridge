use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Serialize;

use super::{InvitationError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionGuarantee {
    Enforced,
    BestEffort,
}

pub fn ensure_private_dir(path: &Path) -> Result<PermissionGuarantee> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(InvitationError::UnsafePathComponent(
                path.display().to_string(),
            ));
        }
    } else {
        create_private_dir_all(path).map_err(|error| InvitationError::io(path, error))?;
    }

    set_dir_permissions(path)
}

pub fn atomic_write_private(path: &Path, bytes: &[u8]) -> Result<PermissionGuarantee> {
    let parent = path
        .parent()
        .ok_or_else(|| InvitationError::UnsafePathComponent(path.display().to_string()))?;
    let guarantee = ensure_private_dir(parent)?;

    if fs::symlink_metadata(path).is_ok() {
        return Err(InvitationError::AlreadyExists(path.to_path_buf()));
    }

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| InvitationError::UnsafePathComponent(path.display().to_string()))?;
    validate_component(file_name)?;

    let temp_path = parent.join(format!(".{file_name}.{}.tmp", random_suffix()?));
    let result = write_and_commit(&temp_path, path, bytes, guarantee);
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

pub fn write_json_private<T: Serialize>(path: &Path, value: &T) -> Result<PermissionGuarantee> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    atomic_write_private(path, &bytes)
}

#[derive(Debug)]
pub struct BatchStaging {
    parent: PathBuf,
    staging_dir: PathBuf,
    final_dir: PathBuf,
    finished: bool,
}

impl BatchStaging {
    pub fn new(parent: &Path, batch_id: &str) -> Result<Self> {
        validate_component(batch_id)?;
        ensure_private_dir(parent)?;

        let final_dir = parent.join(batch_id);
        if fs::symlink_metadata(&final_dir).is_ok() {
            return Err(InvitationError::AlreadyExists(final_dir));
        }

        let staging_dir = parent.join(format!(".{batch_id}.{}.staging", random_suffix()?));
        create_private_dir(&staging_dir)
            .map_err(|error| InvitationError::io(&staging_dir, error))?;
        set_dir_permissions(&staging_dir)?;

        Ok(Self {
            parent: parent.to_path_buf(),
            staging_dir,
            final_dir,
            finished: false,
        })
    }

    pub fn staging_path(&self) -> &Path {
        &self.staging_dir
    }

    pub fn final_path(&self) -> &Path {
        &self.final_dir
    }

    pub fn stage_bytes(&self, name: &str, bytes: &[u8]) -> Result<PathBuf> {
        validate_component(name)?;
        let path = self.staging_dir.join(name);
        atomic_write_private(&path, bytes)?;
        Ok(path)
    }

    pub fn stage_json<T: Serialize>(&self, name: &str, value: &T) -> Result<PathBuf> {
        validate_component(name)?;
        let path = self.staging_dir.join(name);
        write_json_private(&path, value)?;
        Ok(path)
    }

    pub fn finalize(mut self) -> Result<FinalizedBatch> {
        sync_directory(&self.staging_dir)?;
        if fs::symlink_metadata(&self.final_dir).is_ok() {
            return Err(InvitationError::AlreadyExists(self.final_dir.clone()));
        }
        fs::rename(&self.staging_dir, &self.final_dir)
            .map_err(|error| InvitationError::io(&self.final_dir, error))?;
        sync_directory(&self.parent)?;
        self.finished = true;
        Ok(FinalizedBatch {
            path: self.final_dir.clone(),
        })
    }

    pub fn cleanup(mut self) -> Result<()> {
        if self.staging_dir.exists() {
            fs::remove_dir_all(&self.staging_dir)
                .map_err(|error| InvitationError::io(&self.staging_dir, error))?;
            sync_directory(&self.parent)?;
        }
        self.finished = true;
        Ok(())
    }
}

impl Drop for BatchStaging {
    fn drop(&mut self) {
        if !self.finished {
            let _ = fs::remove_dir_all(&self.staging_dir);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalizedBatch {
    path: PathBuf,
}

impl FinalizedBatch {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn write_and_commit(
    temp_path: &Path,
    final_path: &Path,
    bytes: &[u8],
    guarantee: PermissionGuarantee,
) -> Result<PermissionGuarantee> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    set_file_creation_mode(&mut options);

    let mut file = options
        .open(temp_path)
        .map_err(|error| InvitationError::io(temp_path, error))?;
    set_file_permissions(temp_path)?;
    file.write_all(bytes)
        .map_err(|error| InvitationError::io(temp_path, error))?;
    file.sync_all()
        .map_err(|error| InvitationError::io(temp_path, error))?;
    drop(file);

    if fs::symlink_metadata(final_path).is_ok() {
        return Err(InvitationError::AlreadyExists(final_path.to_path_buf()));
    }
    fs::rename(temp_path, final_path).map_err(|error| InvitationError::io(final_path, error))?;
    if let Some(parent) = final_path.parent() {
        sync_directory(parent)?;
    }
    Ok(guarantee)
}

fn random_suffix() -> Result<String> {
    let mut bytes = [0_u8; 12];
    getrandom::fill(&mut bytes).map_err(|error| InvitationError::Random(error.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn validate_component(value: &str) -> Result<()> {
    let path = Path::new(value);
    let mut components = path.components();
    let valid = matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none()
        && value != "."
        && value != "..";
    if !valid || value.is_empty() || value.chars().any(char::is_control) {
        return Err(InvitationError::UnsafePathComponent(value.to_owned()));
    }
    Ok(())
}

#[cfg(unix)]
fn set_dir_permissions(path: &Path) -> Result<PermissionGuarantee> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| InvitationError::io(path, error))?;
    Ok(PermissionGuarantee::Enforced)
}

#[cfg(not(unix))]
fn set_dir_permissions(_path: &Path) -> Result<PermissionGuarantee> {
    Ok(PermissionGuarantee::BestEffort)
}

#[cfg(unix)]
fn create_private_dir_all(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;

    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700).create(path)
}

#[cfg(not(unix))]
fn create_private_dir_all(path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path)
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;

    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700).create(path)
}

#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    fs::create_dir(path)
}

#[cfg(unix)]
fn set_file_creation_mode(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt as _;

    options.mode(0o600);
}

#[cfg(not(unix))]
fn set_file_creation_mode(_options: &mut OpenOptions) {}

#[cfg(unix)]
fn set_file_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| InvitationError::io(path, error))
}

#[cfg(not(unix))]
fn set_file_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    let directory = File::open(path).map_err(|error| InvitationError::io(path, error))?;
    directory
        .sync_all()
        .map_err(|error| InvitationError::io(path, error))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}
