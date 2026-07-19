use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};

use fs4::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::Duration;
use uuid::Uuid;

use crate::{
    Result,
    app::{
        invite::{CreateInvitationBatchRequest, InvitationBatchManifest, InvitationService},
        task::{PublishTaskRequest, PublishTaskResult, TaskService, WorkspaceBinding},
    },
    clock::{Clock, unix_micros},
    domain::{AutonomyPolicy, CompletionPolicy, ParticipantKind},
    error::{ErrorCode, SessionBridgeError},
    git::GitRepository,
    invitation::PromptShell,
    storage::SqliteStore,
    workspace::{
        LocatedWorkspace, atomic_create_private, atomic_write_json_private, ensure_private_dir,
        read_json_private,
    },
};

const MARKER_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchShell {
    Posix,
    Powershell,
}

impl From<LaunchShell> for PromptShell {
    fn from(value: LaunchShell) -> Self {
        match value {
            LaunchShell::Posix => PromptShell::Posix,
            LaunchShell::Powershell => PromptShell::PowerShell,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchRequest {
    pub title: String,
    pub objective: String,
    pub agents: u16,
    pub expires_in_seconds: i64,
    pub autonomy_policy: AutonomyPolicy,
    pub target_branch: Option<String>,
    pub capability_limits: Vec<String>,
    pub prompt_root: PathBuf,
    pub shell: LaunchShell,
    pub profile: String,
    pub allow_dirty_baseline: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchPreflight {
    pub title: String,
    pub objective_sha256: String,
    pub agents: u16,
    pub expires_in_seconds: i64,
    pub autonomy_policy: AutonomyPolicy,
    pub target_branch: String,
    pub baseline_commit: String,
    pub dirty_paths: Vec<String>,
    pub prompt_root: PathBuf,
    pub ready: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptCommand {
    pub batch_index: usize,
    pub argv: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchResult {
    pub launch_id: String,
    pub phase: String,
    pub baseline_commit: String,
    pub task: PublishTaskResult,
    pub invitations: InvitationBatchManifest,
    pub prompt_commands: Vec<PromptCommand>,
    pub watch_argv: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LaunchPhase {
    Prepared,
    TaskPublished,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LaunchMarker {
    schema_version: u32,
    launch_id: String,
    payload_hash: String,
    canonical_request_hash: String,
    request: LaunchRequest,
    task_request_id: String,
    invitation_request_id: String,
    phase: LaunchPhase,
    baseline_commit: String,
    task_id: Option<String>,
    batch_id: Option<String>,
    created_at_micros: i64,
    updated_at_micros: i64,
    completed_result: Option<LaunchResult>,
    warnings: Vec<String>,
}

pub struct LaunchService<C> {
    clock: C,
}

impl<C: Clock + Clone> LaunchService<C> {
    pub fn new(clock: C) -> Self {
        Self { clock }
    }

    pub fn preflight(
        &self,
        workspace: &LocatedWorkspace,
        request: &LaunchRequest,
    ) -> Result<LaunchPreflight> {
        validate_request(request)?;
        let repository = GitRepository::open(&workspace.paths.root)?;
        repository.require_initial_commit()?;
        let target_branch = match request.target_branch.as_deref() {
            Some(branch) => branch.to_owned(),
            None => repository.current_branch()?.ok_or_else(|| {
                SessionBridgeError::domain(
                    ErrorCode::WorktreeConflict,
                    "launch from a detached HEAD requires --target-branch",
                )
            })?,
        };
        let baseline_commit = repository.resolve_revision(&target_branch)?;
        let dirty_paths = repository.dirty_paths()?;
        if !dirty_paths.is_empty() && !request.allow_dirty_baseline {
            return Err(SessionBridgeError::domain(
                ErrorCode::WorktreeDirty,
                "launch requires a clean Git baseline",
            )
            .with_details(serde_json::json!({
                "worktree_path": workspace.paths.root,
                "dirty_paths": dirty_paths,
                "recovery": "commit or stash the baseline, or explicitly pass --allow-dirty-baseline",
            })));
        }
        let warnings = if dirty_paths.is_empty() {
            Vec::new()
        } else {
            vec![
                "dirty baseline accepted explicitly; uncommitted files are not present in Agent worktrees"
                    .into(),
            ]
        };
        Ok(LaunchPreflight {
            title: request.title.clone(),
            objective_sha256: hex(&Sha256::digest(request.objective.as_bytes())),
            agents: request.agents,
            expires_in_seconds: request.expires_in_seconds,
            autonomy_policy: request.autonomy_policy,
            target_branch,
            baseline_commit,
            dirty_paths,
            prompt_root: request.prompt_root.clone(),
            ready: true,
            warnings,
        })
    }

    pub fn launch(
        &self,
        store: &mut SqliteStore,
        workspace: &LocatedWorkspace,
        request: LaunchRequest,
        launch_id: Option<&str>,
        accept_baseline_change: bool,
    ) -> Result<LaunchResult> {
        validate_request(&request)?;
        let launch_id = normalize_launch_id(launch_id)?;
        let marker_path = marker_path(workspace, &launch_id)?;
        let _lock = acquire_lock(&marker_path.with_extension("lock"))?;
        let input_payload_hash = payload_hash(&request)?;

        if marker_path.exists() {
            let marker: LaunchMarker = read_json_private(&marker_path)?;
            validate_marker(&marker, &launch_id, &input_payload_hash)?;
            return self.resume_marker(
                store,
                workspace,
                marker,
                &marker_path,
                accept_baseline_change,
            );
        }

        let preflight = self.preflight(workspace, &request)?;
        let mut canonical_request = request;
        canonical_request.target_branch = Some(preflight.target_branch.clone());
        let now = unix_micros(self.clock.now());
        let marker = LaunchMarker {
            schema_version: MARKER_SCHEMA_VERSION,
            launch_id: launch_id.clone(),
            payload_hash: input_payload_hash,
            canonical_request_hash: payload_hash(&canonical_request)?,
            request: canonical_request,
            task_request_id: Uuid::now_v7().to_string(),
            invitation_request_id: Uuid::now_v7().to_string(),
            phase: LaunchPhase::Prepared,
            baseline_commit: preflight.baseline_commit,
            task_id: None,
            batch_id: None,
            created_at_micros: now,
            updated_at_micros: now,
            completed_result: None,
            warnings: preflight.warnings,
        };
        create_marker(&marker_path, &marker)?;
        self.resume_marker(
            store,
            workspace,
            marker,
            &marker_path,
            accept_baseline_change,
        )
    }

    pub fn resume(
        &self,
        store: &mut SqliteStore,
        workspace: &LocatedWorkspace,
        launch_id: &str,
        accept_baseline_change: bool,
    ) -> Result<LaunchResult> {
        let launch_id = normalize_launch_id(Some(launch_id))?;
        let marker_path = marker_path(workspace, &launch_id)?;
        if !marker_path.is_file() {
            return Err(SessionBridgeError::domain(
                ErrorCode::LaunchNotFound,
                format!("launch operation {launch_id} was not found"),
            ));
        }
        let _lock = acquire_lock(&marker_path.with_extension("lock"))?;
        let marker: LaunchMarker = read_json_private(&marker_path)?;
        validate_marker(&marker, &launch_id, &marker.payload_hash)?;
        self.resume_marker(
            store,
            workspace,
            marker,
            &marker_path,
            accept_baseline_change,
        )
    }

    fn resume_marker(
        &self,
        store: &mut SqliteStore,
        workspace: &LocatedWorkspace,
        mut marker: LaunchMarker,
        marker_path: &Path,
        accept_baseline_change: bool,
    ) -> Result<LaunchResult> {
        if let Some(result) = marker.completed_result.clone() {
            return Ok(result);
        }
        let preflight = self
            .preflight(workspace, &marker.request)
            .map_err(|error| recover_if_published(&marker, error))?;
        if preflight.baseline_commit != marker.baseline_commit {
            if !accept_baseline_change {
                return Err(SessionBridgeError::domain(
                    ErrorCode::LaunchBaselineChanged,
                    "launch target branch changed since the operation was prepared",
                )
                .with_details(serde_json::json!({
                    "launch_id": marker.launch_id,
                    "previous_baseline_commit": marker.baseline_commit,
                    "current_baseline_commit": preflight.baseline_commit,
                    "resume_argv": [
                        "sessionbridge", "launch", "resume", &marker.launch_id,
                        "--accept-baseline-change"
                    ],
                })));
            }
            marker.baseline_commit = preflight.baseline_commit;
            marker
                .warnings
                .push("launch baseline changed during recovery and was accepted explicitly".into());
            if let Err(error) = update_marker(marker_path, &mut marker, self.clock.now()) {
                return Err(recover_if_published(&marker, error));
            }
        }

        let task_result = TaskService::new(self.clock.clone())
            .publish(
                store.connection_mut(),
                WorkspaceBinding::from(workspace),
                PublishTaskRequest {
                    title: marker.request.title.clone(),
                    objective: marker.request.objective.clone(),
                    constraints: Vec::new(),
                    non_goals: Vec::new(),
                    done_conditions: Vec::new(),
                    autonomy_policy: marker.request.autonomy_policy,
                    completion_policy: CompletionPolicy::AllWorkersConcluded,
                    target_branch: Some(preflight.target_branch.clone()),
                    request_id: Some(marker.task_request_id.clone()),
                },
            )
            .map_err(|error| recover_if_published(&marker, error))?;
        marker.phase = LaunchPhase::TaskPublished;
        marker.task_id = Some(task_result.task.id.to_string());
        if let Err(error) = update_marker(marker_path, &mut marker, self.clock.now()) {
            return Err(recovery_error(&marker, error));
        }

        let mut invitations = InvitationService::new(
            store,
            &self.clock,
            workspace.config.project_id,
            &marker.request.prompt_root,
        )
        .and_then(|mut service| {
            service.create_batch(CreateInvitationBatchRequest {
                task_id: task_result.task.id,
                count: usize::from(marker.request.agents),
                participant_kind: ParticipantKind::Worker,
                expires_in: Duration::seconds(marker.request.expires_in_seconds),
                capability_limits: marker.request.capability_limits.clone(),
                request_id: marker.invitation_request_id.clone(),
                shell: marker.request.shell.into(),
                provision_worktree: true,
            })
        })
        .map_err(|error| recovery_error(&marker, error))?;

        let task_id_text = task_result.task.id.to_string();
        let prompt_commands = invitations
            .prompts
            .iter()
            .map(|prompt| PromptCommand {
                batch_index: prompt.batch_index,
                argv: vec![
                    "sessionbridge".into(),
                    "prompt".into(),
                    "show".into(),
                    task_id_text.clone(),
                    "--batch".into(),
                    invitations.batch_id.clone(),
                    "--index".into(),
                    prompt.batch_index.to_string(),
                ],
            })
            .collect();
        for prompt in &mut invitations.prompts {
            prompt.prompt = None;
        }
        let result = LaunchResult {
            launch_id: marker.launch_id.clone(),
            phase: "completed".into(),
            baseline_commit: marker.baseline_commit.clone(),
            task: task_result,
            invitations,
            prompt_commands,
            watch_argv: vec!["sessionbridge".into(), "watch".into(), task_id_text],
            warnings: marker.warnings.clone(),
        };
        marker.phase = LaunchPhase::Completed;
        marker.batch_id = Some(result.invitations.batch_id.clone());
        marker.completed_result = Some(result.clone());
        if let Err(error) = update_marker(marker_path, &mut marker, self.clock.now()) {
            return Err(recovery_error(&marker, error));
        }
        Ok(result)
    }
}

fn validate_request(request: &LaunchRequest) -> Result<()> {
    if request.title.trim().is_empty() || request.objective.trim().is_empty() {
        return Err(invalid("launch title and objective cannot be empty"));
    }
    if request.agents == 0 || usize::from(request.agents) > 256 {
        return Err(invalid("launch agents must be between 1 and 256"));
    }
    if request.expires_in_seconds <= 0 {
        return Err(invalid(
            "launch invitation expiry must be greater than zero",
        ));
    }
    if request.profile.trim().is_empty() {
        return Err(invalid("launch profile cannot be empty"));
    }
    Ok(())
}

fn normalize_launch_id(value: Option<&str>) -> Result<String> {
    match value {
        Some(value) => Uuid::parse_str(value)
            .map(|id| id.to_string())
            .map_err(|error| invalid(format!("invalid launch request ID: {error}"))),
        None => Ok(Uuid::now_v7().to_string()),
    }
}

fn marker_path(workspace: &LocatedWorkspace, launch_id: &str) -> Result<PathBuf> {
    let directory = workspace
        .paths
        .launches_dir
        .join(workspace.config.project_id.to_string())
        .join(&workspace.paths.repository_instance_id);
    ensure_private_dir(&directory)?;
    Ok(directory.join(format!("{launch_id}.json")))
}

fn create_marker(path: &Path, marker: &LaunchMarker) -> Result<()> {
    let mut encoded = serde_json::to_vec_pretty(marker)?;
    encoded.push(b'\n');
    atomic_create_private(path, &encoded)
}

fn update_marker(path: &Path, marker: &mut LaunchMarker, now: time::OffsetDateTime) -> Result<()> {
    marker.updated_at_micros = unix_micros(now);
    atomic_write_json_private(path, marker)
}

fn acquire_lock(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)?;
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(path)
        .map_err(|source| SessionBridgeError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    FileExt::lock(&file).map_err(|source| SessionBridgeError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(file)
}

fn validate_marker(
    marker: &LaunchMarker,
    launch_id: &str,
    expected_payload_hash: &str,
) -> Result<()> {
    if marker.schema_version != MARKER_SCHEMA_VERSION || marker.launch_id != launch_id {
        return Err(invalid("launch marker identity or schema is invalid"));
    }
    if marker.payload_hash != expected_payload_hash {
        return Err(SessionBridgeError::domain(
            ErrorCode::IdempotencyKeyReused,
            "launch request ID was already used with a different payload",
        )
        .with_details(serde_json::json!({ "launch_id": launch_id })));
    }
    if marker.canonical_request_hash != payload_hash(&marker.request)? {
        return Err(invalid("launch marker request hash is invalid"));
    }
    Ok(())
}

fn payload_hash(request: &LaunchRequest) -> Result<String> {
    Ok(hex(&Sha256::digest(serde_json::to_vec(request)?)))
}

fn recovery_error(marker: &LaunchMarker, error: SessionBridgeError) -> SessionBridgeError {
    SessionBridgeError::retryable(
        ErrorCode::LaunchRecoveryRequired,
        "task was published but invitation creation requires launch recovery",
    )
    .with_details(serde_json::json!({
        "launch_id": marker.launch_id,
        "phase": marker_phase_str(marker.phase),
        "task_id": marker.task_id,
        "cause_code": error.code(),
        "cause_message": error.to_string(),
        "cause_retryable": error.retryable_flag(),
        "cause_details": error.details(),
        "resume_argv": ["sessionbridge", "launch", "resume", &marker.launch_id],
    }))
}

fn recover_if_published(marker: &LaunchMarker, error: SessionBridgeError) -> SessionBridgeError {
    if marker.task_id.is_some() {
        recovery_error(marker, error)
    } else {
        error
    }
}

const fn marker_phase_str(phase: LaunchPhase) -> &'static str {
    match phase {
        LaunchPhase::Prepared => "prepared",
        LaunchPhase::TaskPublished => "task_published",
        LaunchPhase::Completed => "completed",
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

fn invalid(message: impl Into<String>) -> SessionBridgeError {
    SessionBridgeError::domain(ErrorCode::InvalidInput, message)
}
