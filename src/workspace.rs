use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
};

use directories::ProjectDirs;
use fs4::FileExt;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    Result,
    domain::WorkspaceId,
    error::{ErrorCode, SessionBridgeError},
};

pub const CONFIG_DIR_NAME: &str = ".sessionbridge";
pub const CONFIG_FILE_NAME: &str = "config.toml";
pub const CONFIG_SCHEMA_VERSION: u32 = 1;
pub const PROTOCOL_VERSION: u32 = 1;
pub const STATE_HOME_ENV: &str = "SESSIONBRIDGE_STATE_HOME";

const INSTANCE_FILE_NAME: &str = "sessionbridge-instance-id";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectConfig {
    pub schema_version: u32,
    pub protocol_version: u32,
    pub project_id: WorkspaceId,
    pub name: String,
}

impl ProjectConfig {
    pub fn new(name: impl Into<String>) -> Result<Self> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "workspace name cannot be empty",
            ));
        }

        Ok(Self {
            schema_version: CONFIG_SCHEMA_VERSION,
            protocol_version: PROTOCOL_VERSION,
            project_id: WorkspaceId::new(),
            name,
        })
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version > CONFIG_SCHEMA_VERSION {
            return Err(SessionBridgeError::domain(
                ErrorCode::SchemaTooNew,
                format!(
                    "workspace config schema {} is newer than supported schema {}",
                    self.schema_version, CONFIG_SCHEMA_VERSION
                ),
            ));
        }
        if self.schema_version < CONFIG_SCHEMA_VERSION {
            return Err(SessionBridgeError::domain(
                ErrorCode::SchemaUpgradeRequired,
                format!(
                    "workspace config schema {} must be upgraded to {}",
                    self.schema_version, CONFIG_SCHEMA_VERSION
                ),
            ));
        }
        if self.protocol_version != PROTOCOL_VERSION {
            return Err(SessionBridgeError::domain(
                ErrorCode::ProtocolVersionMismatch,
                format!(
                    "workspace protocol {} does not match CLI protocol {}",
                    self.protocol_version, PROTOCOL_VERSION
                ),
            ));
        }
        if self.name.trim().is_empty() {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "workspace config contains an empty name",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryKind {
    Git,
    Directory,
}

#[derive(Debug, Clone)]
pub struct RepositoryIdentity {
    pub kind: RepositoryKind,
    pub project_root: PathBuf,
    pub git_worktree_root: Option<PathBuf>,
    pub git_common_dir: Option<PathBuf>,
    pub instance_id: String,
}

#[derive(Debug, Clone)]
pub struct WorkspacePaths {
    pub root: PathBuf,
    pub config_path: PathBuf,
    pub repository_instance_id: String,
    pub state_root: PathBuf,
    pub state_dir: PathBuf,
    pub database_path: PathBuf,
    pub migration_lock_path: PathBuf,
    pub contexts_dir: PathBuf,
    pub invites_dir: PathBuf,
    pub prompts_dir: PathBuf,
    pub worktrees_dir: PathBuf,
    pub join_attempts_dir: PathBuf,
    pub launches_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct LocatedWorkspace {
    pub config: ProjectConfig,
    pub repository: RepositoryIdentity,
    pub paths: WorkspacePaths,
}

impl LocatedWorkspace {
    pub fn discover(start: impl AsRef<Path>) -> Result<Self> {
        let state_root = default_state_root()?;
        Self::discover_with_state_root(start, state_root)
    }

    pub fn discover_with_state_root(
        start: impl AsRef<Path>,
        state_root: impl AsRef<Path>,
    ) -> Result<Self> {
        let config_path = find_config(start.as_ref())?;
        Self::from_config_path_with_state_root(config_path, state_root)
    }

    pub fn from_config_path_with_state_root(
        config_path: impl AsRef<Path>,
        state_root: impl AsRef<Path>,
    ) -> Result<Self> {
        let config_path = canonicalize_file(config_path.as_ref())?;
        let root = project_root_for_config(&config_path)?;
        let config: ProjectConfig = read_toml(&config_path)?;
        config.validate()?;

        let state_root = absolutize_state_root(state_root.as_ref())?;
        ensure_private_dir(&state_root)?;
        let repository = resolve_repository_identity(&root, &state_root, config.project_id)?;
        let paths = build_paths(&config, &repository, state_root);
        ensure_state_layout(&paths)?;

        Ok(Self {
            config,
            repository,
            paths,
        })
    }
}

pub fn initialize(
    root: impl AsRef<Path>,
    name: impl Into<String>,
    state_root: Option<&Path>,
) -> Result<LocatedWorkspace> {
    let root = canonicalize_directory(root.as_ref())?;
    let name = name.into();
    let config_path = root.join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME);
    let state_root = match state_root {
        Some(path) => path.to_path_buf(),
        None => default_state_root()?,
    };
    if config_path.exists() {
        let located =
            LocatedWorkspace::from_config_path_with_state_root(&config_path, &state_root)?;
        if located.config.name != name {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                format!(
                    "workspace at {} is named '{}', not '{}'",
                    config_path.display(),
                    located.config.name,
                    name
                ),
            ));
        }
        return Ok(located);
    }

    let config = ProjectConfig::new(name)?;
    let encoded = toml::to_string_pretty(&config)?;
    ensure_private_dir(&root.join(CONFIG_DIR_NAME))?;
    atomic_create_private(&config_path, encoded.as_bytes())?;

    LocatedWorkspace::from_config_path_with_state_root(config_path, state_root)
}

pub fn link(source: &LocatedWorkspace, target_root: impl AsRef<Path>) -> Result<LocatedWorkspace> {
    let target_root = canonicalize_directory(target_root.as_ref())?;
    let source_common_dir = source.repository.git_common_dir.as_ref().ok_or_else(|| {
        SessionBridgeError::domain(
            ErrorCode::WorktreeConflict,
            "workspace link requires a Git repository",
        )
    })?;
    let Some((_, target_common_dir)) = git_repository_paths(&target_root)? else {
        return Err(SessionBridgeError::domain(
            ErrorCode::WorktreeConflict,
            "workspace link target must be a Git worktree",
        ));
    };
    if &target_common_dir != source_common_dir {
        return Err(SessionBridgeError::domain(
            ErrorCode::WorktreeConflict,
            "workspace link target belongs to a different Git repository",
        )
        .with_details(serde_json::json!({
            "source_git_common_dir": source_common_dir,
            "target_git_common_dir": target_common_dir,
        })));
    }

    let config_path = target_root.join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME);
    if config_path.exists() {
        let existing: ProjectConfig = read_toml(&config_path)?;
        if existing != source.config {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "workspace link target already contains a different SessionBridge config",
            ));
        }
    } else {
        ensure_private_dir(&target_root.join(CONFIG_DIR_NAME))?;
        let encoded = toml::to_string_pretty(&source.config)?;
        atomic_create_private(&config_path, encoded.as_bytes())?;
    }
    ensure_git_local_exclude(&target_common_dir, ".sessionbridge/")?;

    LocatedWorkspace::from_config_path_with_state_root(config_path, &source.paths.state_root)
}

fn ensure_git_local_exclude(common_dir: &Path, pattern: &str) -> Result<()> {
    let info_dir = common_dir.join("info");
    fs::create_dir_all(&info_dir).map_err(|source| io_error(&info_dir, source))?;
    let exclude_path = info_dir.join("exclude");
    let existing = match fs::read_to_string(&exclude_path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => return Err(io_error(&exclude_path, source)),
    };
    if existing.lines().any(|line| line.trim() == pattern) {
        return Ok(());
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&exclude_path)
        .map_err(|source| io_error(&exclude_path, source))?;
    if !existing.is_empty() && !existing.ends_with('\n') {
        file.write_all(b"\n")
            .map_err(|source| io_error(&exclude_path, source))?;
    }
    file.write_all(format!("{pattern}\n").as_bytes())
        .map_err(|source| io_error(&exclude_path, source))?;
    file.sync_all()
        .map_err(|source| io_error(&exclude_path, source))?;
    Ok(())
}

pub fn default_state_root() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(STATE_HOME_ENV) {
        if path.is_empty() {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                format!("{STATE_HOME_ENV} cannot be empty"),
            ));
        }
        return absolutize_state_root(Path::new(&path));
    }

    let dirs = ProjectDirs::from("dev", "SessionBridge", "SessionBridge").ok_or_else(|| {
        SessionBridgeError::domain(
            ErrorCode::WorkspaceNotFound,
            "cannot determine the user state directory",
        )
    })?;
    let path = dirs
        .state_dir()
        .unwrap_or_else(|| dirs.data_local_dir())
        .to_path_buf();
    absolutize_state_root(&path)
}

pub fn find_config(start: &Path) -> Result<PathBuf> {
    let start = if start.as_os_str().is_empty() {
        std::env::current_dir().map_err(|source| io_error(".", source))?
    } else {
        start.to_path_buf()
    };

    if start.is_file() {
        if start.file_name().and_then(|name| name.to_str()) == Some(CONFIG_FILE_NAME) {
            return Ok(start);
        }
        return find_config(start.parent().ok_or_else(|| workspace_not_found(&start))?);
    }

    let start = canonicalize_directory(&start)?;
    for directory in start.ancestors() {
        let candidate = directory.join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    Err(workspace_not_found(&start))
}

pub fn read_json_private<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = fs::read(path).map_err(|source| io_error(path, source))?;
    serde_json::from_slice(&bytes).map_err(Into::into)
}

pub fn ensure_private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(|source| io_error(path, source))?;
    set_dir_private(path)?;
    Ok(())
}

pub fn atomic_write_private(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            format!("path has no parent: {}", path.display()),
        )
    })?;
    ensure_private_dir(parent)?;
    let temporary = temporary_path(path);
    let mut file = create_private_new(&temporary)?;
    let result = (|| {
        file.write_all(contents)
            .map_err(|source| io_error(&temporary, source))?;
        file.sync_all()
            .map_err(|source| io_error(&temporary, source))?;
        drop(file);
        fs::rename(&temporary, path).map_err(|source| io_error(path, source))?;
        sync_directory(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub fn atomic_write_json_private<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut encoded = serde_json::to_vec_pretty(value)?;
    encoded.push(b'\n');
    atomic_write_private(path, &encoded)
}

fn build_paths(
    config: &ProjectConfig,
    repository: &RepositoryIdentity,
    state_root: PathBuf,
) -> WorkspacePaths {
    let state_dir = state_root
        .join("workspaces")
        .join(config.project_id.to_string())
        .join(&repository.instance_id);
    WorkspacePaths {
        root: repository.project_root.clone(),
        config_path: repository
            .project_root
            .join(CONFIG_DIR_NAME)
            .join(CONFIG_FILE_NAME),
        repository_instance_id: repository.instance_id.clone(),
        database_path: state_dir.join("state.db"),
        migration_lock_path: state_dir.join("migration.lock"),
        contexts_dir: state_root.join("contexts"),
        invites_dir: state_root.join("invites"),
        prompts_dir: state_root.join("prompts"),
        worktrees_dir: state_root.join("worktrees"),
        join_attempts_dir: state_root.join("join-attempts"),
        launches_dir: state_root.join("launches"),
        state_root,
        state_dir,
    }
}

fn ensure_state_layout(paths: &WorkspacePaths) -> Result<()> {
    for directory in [
        &paths.state_dir,
        &paths.contexts_dir,
        &paths.invites_dir,
        &paths.prompts_dir,
        &paths.worktrees_dir,
        &paths.join_attempts_dir,
        &paths.launches_dir,
    ] {
        ensure_private_dir(directory)?;
    }
    Ok(())
}

fn resolve_repository_identity(
    root: &Path,
    state_root: &Path,
    project_id: WorkspaceId,
) -> Result<RepositoryIdentity> {
    if let Some((worktree_root, common_dir)) = git_repository_paths(root)? {
        let identity_path = common_dir.join(INSTANCE_FILE_NAME);
        let instance_id =
            resolve_git_instance_id(root, state_root, project_id, &common_dir, &identity_path)?;
        return Ok(RepositoryIdentity {
            kind: RepositoryKind::Git,
            project_root: root.to_path_buf(),
            git_worktree_root: Some(worktree_root),
            git_common_dir: Some(common_dir),
            instance_id,
        });
    }

    let identity_path = directory_instance_path(root, state_root)?;
    let instance_id = read_or_create_instance_id(&identity_path)?;
    Ok(RepositoryIdentity {
        kind: RepositoryKind::Directory,
        project_root: root.to_path_buf(),
        git_worktree_root: None,
        git_common_dir: None,
        instance_id,
    })
}

fn resolve_git_instance_id(
    root: &Path,
    state_root: &Path,
    project_id: WorkspaceId,
    common_dir: &Path,
    identity_path: &Path,
) -> Result<String> {
    let _lock = acquire_instance_lock(&common_dir.join("sessionbridge-instance-id.lock"))?;
    let legacy_identity_path = directory_instance_path(root, state_root)?;
    let legacy_id = read_optional_instance_id(&legacy_identity_path)?;
    let current_id = read_optional_instance_id(identity_path)?;

    let Some(current_id) = current_id else {
        let instance_id = legacy_id.unwrap_or_else(|| Uuid::now_v7().to_string());
        atomic_create_private(identity_path, format!("{instance_id}\n").as_bytes())?;
        return Ok(instance_id);
    };
    let Some(legacy_id) = legacy_id else {
        return Ok(current_id);
    };
    if current_id == legacy_id {
        return Ok(current_id);
    }

    let current_database = workspace_database_path(state_root, project_id, &current_id);
    let legacy_database = workspace_database_path(state_root, project_id, &legacy_id);
    if !legacy_database.is_file() {
        return Ok(current_id);
    }
    if current_database.is_file() {
        return Err(repository_identity_conflict(
            identity_path,
            &current_id,
            &legacy_id,
            &current_database,
            &legacy_database,
        ));
    }
    if let Some(database_path) = find_database_for_repository_identity(state_root, &current_id)? {
        return Err(repository_identity_conflict(
            identity_path,
            &current_id,
            &legacy_id,
            &database_path,
            &legacy_database,
        ));
    }

    atomic_write_private(identity_path, format!("{legacy_id}\n").as_bytes())?;
    tracing::warn!(
        event = "repository_identity_inherited",
        project_id = %project_id,
        previous_repository_instance_id = %current_id,
        repository_instance_id = %legacy_id,
        database_path = %legacy_database.display(),
        "inherited the existing directory identity after Git initialization"
    );
    Ok(legacy_id)
}

fn directory_instance_path(root: &Path, state_root: &Path) -> Result<PathBuf> {
    let registry = state_root.join("repository-instances");
    ensure_private_dir(&registry)?;
    let key = Sha256::digest(root.as_os_str().to_string_lossy().as_bytes());
    Ok(registry.join(format!("{}.id", hex(&key))))
}

fn workspace_database_path(
    state_root: &Path,
    project_id: WorkspaceId,
    repository_instance_id: &str,
) -> PathBuf {
    state_root
        .join("workspaces")
        .join(project_id.to_string())
        .join(repository_instance_id)
        .join("state.db")
}

fn find_database_for_repository_identity(
    state_root: &Path,
    repository_instance_id: &str,
) -> Result<Option<PathBuf>> {
    let workspaces_dir = state_root.join("workspaces");
    let entries = match fs::read_dir(&workspaces_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io_error(&workspaces_dir, source)),
    };
    for entry in entries {
        let entry = entry.map_err(|source| io_error(&workspaces_dir, source))?;
        let candidate = entry.path().join(repository_instance_id).join("state.db");
        if candidate.is_file() {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

fn repository_identity_conflict(
    identity_path: &Path,
    current_id: &str,
    legacy_id: &str,
    current_database: &Path,
    legacy_database: &Path,
) -> SessionBridgeError {
    SessionBridgeError::domain(
        ErrorCode::InvalidInput,
        "Git initialization exposed conflicting SessionBridge repository identities",
    )
    .with_details(serde_json::json!({
        "identity_path": identity_path,
        "git_repository_instance_id": current_id,
        "directory_repository_instance_id": legacy_id,
        "git_identity_database_path": current_database,
        "directory_identity_database_path": legacy_database,
        "recovery_required": true,
    }))
}

fn read_optional_instance_id(path: &Path) -> Result<Option<String>> {
    match read_instance_id(path) {
        Ok(id) => Ok(Some(id)),
        Err(SessionBridgeError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn acquire_instance_lock(path: &Path) -> Result<File> {
    let parent = path.parent().ok_or_else(|| {
        SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            format!("path has no parent: {}", path.display()),
        )
    })?;
    ensure_private_dir(parent)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(path)
        .map_err(|source| io_error(path, source))?;
    FileExt::lock(&file).map_err(|source| io_error(path, source))?;
    Ok(file)
}

fn git_repository_paths(root: &Path) -> Result<Option<(PathBuf, PathBuf)>> {
    let worktree = match git_rev_parse(root, &["--show-toplevel"])? {
        Some(path) => PathBuf::from(path),
        None => return Ok(None),
    };
    let common = match git_rev_parse(root, &["--path-format=absolute", "--git-common-dir"])? {
        Some(common) => Some(common),
        None => git_rev_parse(root, &["--git-common-dir"])?,
    }
    .ok_or_else(|| {
        SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            format!("cannot resolve Git common directory for {}", root.display()),
        )
    })?;
    let common = PathBuf::from(common);
    let common = if common.is_absolute() {
        common
    } else {
        root.join(common)
    };
    Ok(Some((
        canonicalize_directory(&worktree)?,
        canonicalize_directory(&common)?,
    )))
}

fn git_rev_parse(root: &Path, args: &[&str]) -> Result<Option<String>> {
    let output = match Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("rev-parse")
        .args(args)
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io_error("git", source)),
    };
    if !output.status.success() {
        return Ok(None);
    }
    let value = String::from_utf8(output.stdout).map_err(|error| {
        SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            format!("git returned non-UTF-8 repository metadata: {error}"),
        )
    })?;
    let value = value.trim();
    if value.is_empty() {
        Ok(None)
    } else {
        Ok(Some(value.to_owned()))
    }
}

fn read_or_create_instance_id(path: &Path) -> Result<String> {
    match read_instance_id(path) {
        Ok(id) => return Ok(id),
        Err(SessionBridgeError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)?;
    }
    let id = Uuid::now_v7().to_string();
    match atomic_create_private(path, format!("{id}\n").as_bytes()) {
        Ok(()) => Ok(id),
        Err(SessionBridgeError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::AlreadyExists =>
        {
            read_instance_id(path)
        }
        Err(error) if path.exists() => read_instance_id(path).or(Err(error)),
        Err(error) => Err(error),
    }
}

fn read_instance_id(path: &Path) -> Result<String> {
    let mut value = String::new();
    File::open(path)
        .map_err(|source| io_error(path, source))?
        .read_to_string(&mut value)
        .map_err(|source| io_error(path, source))?;
    let value = value.trim();
    Uuid::parse_str(value).map_err(|error| {
        SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            format!(
                "invalid repository instance ID in {}: {error}",
                path.display()
            ),
        )
    })?;
    Ok(value.to_owned())
}

pub(crate) fn atomic_create_private(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            format!("path has no parent: {}", path.display()),
        )
    })?;
    let temporary = temporary_path(path);
    let mut file = create_private_new(&temporary)?;
    let result = (|| {
        file.write_all(contents)
            .map_err(|source| io_error(&temporary, source))?;
        file.sync_all()
            .map_err(|source| io_error(&temporary, source))?;
        drop(file);
        fs::hard_link(&temporary, path).map_err(|source| io_error(path, source))?;
        fs::remove_file(&temporary).map_err(|source| io_error(&temporary, source))?;
        sync_directory(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn create_private_new(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(|source| io_error(path, source))
}

fn read_toml<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let value = fs::read_to_string(path).map_err(|source| io_error(path, source))?;
    toml::from_str(&value).map_err(Into::into)
}

fn project_root_for_config(config_path: &Path) -> Result<PathBuf> {
    let config_dir = config_path
        .parent()
        .ok_or_else(|| workspace_not_found(config_path))?;
    if config_dir.file_name().and_then(|name| name.to_str()) != Some(CONFIG_DIR_NAME) {
        return Err(SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            format!(
                "workspace config must be located at {CONFIG_DIR_NAME}/{CONFIG_FILE_NAME}: {}",
                config_path.display()
            ),
        ));
    }
    let root = config_dir
        .parent()
        .ok_or_else(|| workspace_not_found(config_path))?;
    canonicalize_directory(root)
}

fn canonicalize_file(path: &Path) -> Result<PathBuf> {
    if !path.is_file() {
        return Err(workspace_not_found(path));
    }
    fs::canonicalize(path).map_err(|source| io_error(path, source))
}

fn canonicalize_directory(path: &Path) -> Result<PathBuf> {
    let path = fs::canonicalize(path).map_err(|source| io_error(path, source))?;
    if !path.is_dir() {
        return Err(SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            format!("expected a directory: {}", path.display()),
        ));
    }
    Ok(path)
}

fn absolutize_state_root(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let cwd = std::env::current_dir().map_err(|source| io_error(".", source))?;
    Ok(cwd.join(path))
}

fn temporary_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("sessionbridge");
    path.with_file_name(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        Uuid::now_v7()
    ))
}

fn workspace_not_found(path: &Path) -> SessionBridgeError {
    SessionBridgeError::domain(
        ErrorCode::WorkspaceNotFound,
        format!(
            "no {CONFIG_DIR_NAME}/{CONFIG_FILE_NAME} found from {}",
            path.display()
        ),
    )
}

fn io_error(path: impl Into<PathBuf>, source: std::io::Error) -> SessionBridgeError {
    SessionBridgeError::Io {
        path: path.into(),
        source,
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(DIGITS[(byte >> 4) as usize] as char);
        result.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    result
}

#[cfg(unix)]
fn set_dir_private(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|source| io_error(path, source))
}

#[cfg(not(unix))]
fn set_dir_private(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error(path, source))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, process::Command};

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn discovers_workspace_from_nested_directory() {
        let project = TempDir::new().unwrap();
        let state = TempDir::new().unwrap();
        let initialized = initialize(project.path(), "demo", Some(state.path())).unwrap();
        let nested = project.path().join("src/deep");
        fs::create_dir_all(&nested).unwrap();

        let discovered = LocatedWorkspace::discover_with_state_root(&nested, state.path()).unwrap();

        assert_eq!(discovered.config, initialized.config);
        assert_eq!(
            discovered.paths.repository_instance_id,
            initialized.paths.repository_instance_id
        );
        assert_eq!(
            discovered.paths.database_path,
            initialized.paths.database_path
        );
    }

    #[test]
    fn directory_instance_is_stable_and_bound_to_canonical_root() {
        let state = TempDir::new().unwrap();
        let first = TempDir::new().unwrap();
        let second = TempDir::new().unwrap();
        let one = initialize(first.path(), "one", Some(state.path())).unwrap();
        let one_again =
            LocatedWorkspace::discover_with_state_root(first.path(), state.path()).unwrap();
        let two = initialize(second.path(), "two", Some(state.path())).unwrap();

        assert_eq!(one.repository.instance_id, one_again.repository.instance_id);
        assert_ne!(one.repository.instance_id, two.repository.instance_id);
    }

    #[test]
    fn directory_identity_is_preserved_when_git_is_initialized_later() {
        let project = TempDir::new().unwrap();
        let state = TempDir::new().unwrap();
        let initialized = initialize(project.path(), "demo", Some(state.path())).unwrap();
        fs::write(&initialized.paths.database_path, b"existing state").unwrap();
        git_init(project.path());

        let discovered =
            LocatedWorkspace::discover_with_state_root(project.path(), state.path()).unwrap();

        assert_eq!(
            discovered.paths.repository_instance_id,
            initialized.paths.repository_instance_id
        );
        assert_eq!(
            fs::read_to_string(project.path().join(".git/sessionbridge-instance-id"))
                .unwrap()
                .trim(),
            initialized.paths.repository_instance_id
        );
        assert_eq!(
            discovered.paths.database_path,
            initialized.paths.database_path
        );
    }

    #[test]
    fn unused_git_identity_is_repaired_after_legacy_database_is_detected() {
        let project = TempDir::new().unwrap();
        let state = TempDir::new().unwrap();
        let initialized = initialize(project.path(), "demo", Some(state.path())).unwrap();
        fs::write(&initialized.paths.database_path, b"existing state").unwrap();
        git_init(project.path());
        let unused_git_id = Uuid::now_v7().to_string();
        fs::write(
            project.path().join(".git/sessionbridge-instance-id"),
            format!("{unused_git_id}\n"),
        )
        .unwrap();

        let discovered =
            LocatedWorkspace::discover_with_state_root(project.path(), state.path()).unwrap();

        assert_eq!(
            discovered.paths.repository_instance_id,
            initialized.paths.repository_instance_id
        );
        assert_eq!(
            fs::read_to_string(project.path().join(".git/sessionbridge-instance-id"))
                .unwrap()
                .trim(),
            initialized.paths.repository_instance_id
        );
    }

    #[test]
    fn conflicting_git_and_directory_databases_require_explicit_recovery() {
        let project = TempDir::new().unwrap();
        let state = TempDir::new().unwrap();
        let initialized = initialize(project.path(), "demo", Some(state.path())).unwrap();
        fs::write(&initialized.paths.database_path, b"directory state").unwrap();
        git_init(project.path());
        let git_id = Uuid::now_v7().to_string();
        fs::write(
            project.path().join(".git/sessionbridge-instance-id"),
            format!("{git_id}\n"),
        )
        .unwrap();
        let git_database = state
            .path()
            .join("workspaces")
            .join(initialized.config.project_id.to_string())
            .join(&git_id)
            .join("state.db");
        fs::create_dir_all(git_database.parent().unwrap()).unwrap();
        fs::write(git_database, b"git state").unwrap();

        let error =
            LocatedWorkspace::discover_with_state_root(project.path(), state.path()).unwrap_err();

        assert_eq!(error.code(), ErrorCode::InvalidInput);
        assert_eq!(error.details()["recovery_required"], true);
    }

    #[test]
    fn rejects_config_with_newer_protocol() {
        let project = TempDir::new().unwrap();
        let state = TempDir::new().unwrap();
        let config_dir = project.path().join(CONFIG_DIR_NAME);
        fs::create_dir(&config_dir).unwrap();
        let config = ProjectConfig {
            schema_version: CONFIG_SCHEMA_VERSION,
            protocol_version: PROTOCOL_VERSION + 1,
            project_id: WorkspaceId::new(),
            name: "future".into(),
        };
        fs::write(
            config_dir.join(CONFIG_FILE_NAME),
            toml::to_string(&config).unwrap(),
        )
        .unwrap();

        let error =
            LocatedWorkspace::discover_with_state_root(project.path(), state.path()).unwrap_err();
        assert_eq!(error.code(), ErrorCode::ProtocolVersionMismatch);
    }

    #[cfg(unix)]
    #[test]
    fn private_artifacts_have_restrictive_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let project = TempDir::new().unwrap();
        let state = TempDir::new().unwrap();
        let workspace = initialize(project.path(), "private", Some(state.path())).unwrap();

        let config_mode = fs::metadata(&workspace.paths.config_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let state_mode = fs::metadata(&workspace.paths.state_dir)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(config_mode, 0o600);
        assert_eq!(state_mode, 0o700);
    }

    fn git_init(path: &Path) {
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(path)
                .arg("init")
                .status()
                .unwrap()
                .success()
        );
    }
}
