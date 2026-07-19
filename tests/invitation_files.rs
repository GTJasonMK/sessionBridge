use std::fs;

use sessionbridge::invitation::{
    BatchStaging, InvitationError, PermissionGuarantee, atomic_write_private, ensure_private_dir,
};
use tempfile::tempdir;

#[test]
fn private_file_write_is_atomic_and_does_not_overwrite() {
    let temp = tempdir().unwrap();
    let private = temp.path().join("private");
    let path = private.join("invite.json");

    let guarantee = atomic_write_private(&path, b"first").expect("write file");
    assert_eq!(fs::read(&path).unwrap(), b"first");
    assert!(matches!(
        atomic_write_private(&path, b"second"),
        Err(InvitationError::AlreadyExists(existing)) if existing == path
    ));
    assert_eq!(fs::read(&path).unwrap(), b"first");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        assert_eq!(guarantee, PermissionGuarantee::Enforced);
        assert_eq!(
            fs::metadata(&private).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn batch_staging_finalizes_with_one_directory_rename() {
    let temp = tempdir().unwrap();
    let parent = temp.path().join("batches");
    let staging = BatchStaging::new(&parent, "batch-123").expect("staging");
    let staging_path = staging.staging_path().to_path_buf();
    staging
        .stage_bytes("agent-01.prompt.txt", b"prompt one")
        .unwrap();
    staging
        .stage_bytes("agent-02.prompt.txt", b"prompt two")
        .unwrap();

    let finalized = staging.finalize().expect("finalize");
    assert!(!staging_path.exists());
    assert_eq!(
        fs::read(finalized.path().join("agent-01.prompt.txt")).unwrap(),
        b"prompt one"
    );
    assert_eq!(
        fs::read(finalized.path().join("agent-02.prompt.txt")).unwrap(),
        b"prompt two"
    );
}

#[test]
fn dropping_or_cleaning_staging_removes_partial_batch() {
    let temp = tempdir().unwrap();
    let parent = temp.path().join("batches");
    let staging_path = {
        let staging = BatchStaging::new(&parent, "batch-drop").unwrap();
        staging.stage_bytes("partial.txt", b"partial").unwrap();
        staging.staging_path().to_path_buf()
    };
    assert!(!staging_path.exists());

    let staging = BatchStaging::new(&parent, "batch-cleanup").unwrap();
    let cleanup_path = staging.staging_path().to_path_buf();
    staging.cleanup().unwrap();
    assert!(!cleanup_path.exists());
}

#[test]
fn batch_rejects_path_traversal() {
    let temp = tempdir().unwrap();
    let parent = temp.path().join("batches");
    assert!(BatchStaging::new(&parent, "../escape").is_err());

    let staging = BatchStaging::new(&parent, "batch-safe").unwrap();
    assert!(staging.stage_bytes("../escape", b"no").is_err());
}

#[test]
fn private_directory_rejects_symlinks() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let target = temp.path().join("target");
        fs::create_dir(&target).unwrap();
        let link = temp.path().join("link");
        symlink(&target, &link).unwrap();

        assert!(ensure_private_dir(&link).is_err());
    }
}
