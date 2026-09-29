use crate::error::{Result, error};
use std::{
    fs,
    io::{ErrorKind, Write},
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::Path,
};

pub fn ensure_private_dir(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
            return Err(error(
                "INVALID_INPUT",
                format!(
                    "private directory is not a real directory: {}",
                    path.display()
                ),
            ));
        }
        Ok(_) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(path)?;
        }
        Err(e) => return Err(e.into()),
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}
pub fn atomic_write_private(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| error("INVALID_INPUT", "file has no parent"))?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| error("INVALID_INPUT", "file name must be UTF-8"))?;
    if name.chars().any(char::is_control) {
        return Err(error(
            "INVALID_INPUT",
            "file name contains control characters",
        ));
    }
    ensure_private_dir(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(contents)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}
