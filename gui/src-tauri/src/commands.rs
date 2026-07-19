use std::{path::PathBuf, str::FromStr};

use serde::Deserialize;
use sessionbridge::{
    app::{
        chat::{ChatMessageView, ChatService, SendChatMessageRequest},
        diagnostics::{self, CollaborationDiagnostics},
        invite::{InvitationManifest, InvitationService, ShowInvitationRequest},
        launch::{LaunchPreflight, LaunchRequest, LaunchResult, LaunchService, LaunchShell},
        maintenance::{self, DoctorResult},
        operator::{OperatorService, TaskDashboard, TaskSummary, WorkspaceView},
        settings::{self, SettingsShell},
        task::{TaskMutationRequest, TaskMutationResult, TaskService, WorkspaceBinding},
        workspace_service::WorkspaceService,
    },
    clock::SystemClock,
    domain::{InvitationId, TaskId},
    invitation::parse_duration,
    logging::LogTailResult,
    workspace::LocatedWorkspace,
};
use tauri::State;
use uuid::Uuid;

use crate::{
    AppState,
    error::{GuiError, GuiResult},
};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiLaunchInput {
    pub title: String,
    pub objective: String,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub agents: Option<u16>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiChatInput {
    pub kind: String,
    pub body: String,
    #[serde(default)]
    pub reply_to_message_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskAction {
    Pause,
    Resume,
}

#[tauri::command]
pub async fn workspace_restore(state: State<'_, AppState>) -> GuiResult<Option<WorkspaceView>> {
    let path = state
        .preferences
        .lock()
        .map_err(|_| internal("GUI preferences lock is poisoned"))?
        .last_workspace
        .clone();
    let Some(path) = path else { return Ok(None) };
    match open_workspace(path.clone()) {
        Ok(workspace) => Ok(Some(WorkspaceView::from(&workspace))),
        Err(error) if error.code == sessionbridge::ErrorCode::WorkspaceNotFound => {
            state
                .preferences
                .lock()
                .map_err(|_| internal("GUI preferences lock is poisoned"))?
                .forget(&path)?;
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[tauri::command]
pub async fn workspace_open(path: String, state: State<'_, AppState>) -> GuiResult<WorkspaceView> {
    let workspace = open_workspace(PathBuf::from(path))?;
    state
        .preferences
        .lock()
        .map_err(|_| internal("GUI preferences lock is poisoned"))?
        .remember(workspace.paths.root.clone())?;
    Ok(WorkspaceView::from(&workspace))
}

#[tauri::command]
pub async fn workspace_initialize(
    path: String,
    state: State<'_, AppState>,
) -> GuiResult<WorkspaceView> {
    let root = PathBuf::from(path);
    let name = root
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("SessionBridge Workspace")
        .to_owned();
    WorkspaceService::new(SystemClock).initialize(&root, name, None)?;
    let workspace = open_workspace(root)?;
    state
        .preferences
        .lock()
        .map_err(|_| internal("GUI preferences lock is poisoned"))?
        .remember(workspace.paths.root.clone())?;
    Ok(WorkspaceView::from(&workspace))
}

#[tauri::command]
pub async fn task_list(workspace: String) -> GuiResult<Vec<TaskSummary>> {
    let workspace = open_workspace(workspace)?;
    let service = OperatorService::default();
    let store = service.open_store(&workspace)?;
    Ok(service.list_tasks(store.connection(), &workspace)?)
}

#[tauri::command]
pub async fn task_snapshot(workspace: String, task_id: String) -> GuiResult<TaskDashboard> {
    let workspace = open_workspace(workspace)?;
    let service = OperatorService::default();
    let mut store = service.open_store(&workspace)?;
    Ok(service.dashboard(&mut store, &workspace, parse_task_id(&task_id)?)?)
}

#[tauri::command]
pub async fn chat_send(
    workspace: String,
    task_id: String,
    input: GuiChatInput,
) -> GuiResult<ChatMessageView> {
    let workspace = open_workspace(workspace)?;
    let operator = OperatorService::default();
    let mut store = operator.open_store(&workspace)?;
    Ok(ChatService::new(SystemClock).send_operator(
        &mut store,
        workspace.config.project_id,
        parse_task_id(&task_id)?,
        SendChatMessageRequest {
            kind: input.kind,
            body: input.body,
            reply_to_message_id: input.reply_to_message_id,
            mentions: Vec::new(),
            related_entities: Vec::new(),
            request_id: Uuid::now_v7().to_string(),
        },
    )?)
}

#[tauri::command]
pub async fn launch_preflight(
    workspace: String,
    input: GuiLaunchInput,
) -> GuiResult<LaunchPreflight> {
    let workspace = open_workspace(workspace)?;
    let request = launch_request(&workspace, input)?;
    Ok(LaunchService::new(SystemClock).preflight(&workspace, &request)?)
}

#[tauri::command]
pub async fn launch_execute(workspace: String, input: GuiLaunchInput) -> GuiResult<LaunchResult> {
    let workspace = open_workspace(workspace)?;
    let request = launch_request(&workspace, input)?;
    let service = LaunchService::new(SystemClock);
    let operator = OperatorService::default();
    let mut store = operator.open_store(&workspace)?;
    Ok(service.launch(&mut store, &workspace, request, None, false)?)
}

#[tauri::command]
pub async fn prompt_list(workspace: String, task_id: String) -> GuiResult<Vec<InvitationManifest>> {
    let workspace = open_workspace(workspace)?;
    let operator = OperatorService::default();
    let mut store = operator.open_store(&workspace)?;
    let clock = SystemClock;
    let service = InvitationService::new(
        &mut store,
        &clock,
        workspace.config.project_id,
        &workspace.paths.prompts_dir,
    )?;
    Ok(service.list(parse_task_id(&task_id)?)?)
}

#[tauri::command]
pub async fn prompt_show(
    workspace: String,
    invitation_id: String,
) -> GuiResult<InvitationManifest> {
    let workspace = open_workspace(workspace)?;
    let operator = OperatorService::default();
    let mut store = operator.open_store(&workspace)?;
    let clock = SystemClock;
    let service = InvitationService::new(
        &mut store,
        &clock,
        workspace.config.project_id,
        &workspace.paths.prompts_dir,
    )?;
    Ok(service.show(ShowInvitationRequest {
        invitation_id: InvitationId::from_str(&invitation_id).map_err(invalid_id)?,
        include_prompt: true,
        audit: false,
    })?)
}

#[tauri::command]
pub async fn task_transition(
    workspace: String,
    task_id: String,
    action: TaskAction,
    reason: Option<String>,
) -> GuiResult<TaskMutationResult> {
    let workspace = open_workspace(workspace)?;
    let operator = OperatorService::default();
    let mut store = operator.open_store(&workspace)?;
    let service = TaskService::new(SystemClock);
    let request = TaskMutationRequest {
        task_id: parse_task_id(&task_id)?,
        reason,
        request_id: Some(Uuid::now_v7().to_string()),
    };
    let binding = WorkspaceBinding::from(&workspace);
    Ok(match action {
        TaskAction::Pause => service.pause(store.connection_mut(), binding, request)?,
        TaskAction::Resume => service.resume(store.connection_mut(), binding, request)?,
    })
}

#[tauri::command]
pub async fn doctor_run(workspace: String) -> GuiResult<DoctorResult> {
    Ok(maintenance::doctor(&open_workspace(workspace)?)?)
}

#[tauri::command]
pub async fn collaboration_diagnostics(
    workspace: String,
    task_id: String,
) -> GuiResult<CollaborationDiagnostics> {
    let workspace = open_workspace(workspace)?;
    let operator = OperatorService::default();
    let mut store = operator.open_store(&workspace)?;
    let task_id = parse_task_id(&task_id)?;
    operator.reconcile(&mut store, &workspace, task_id)?;
    Ok(diagnostics::analyze(
        store.connection(),
        task_id,
        &SystemClock,
    )?)
}

#[tauri::command]
pub async fn logs_tail(lines: Option<usize>) -> GuiResult<LogTailResult> {
    Ok(sessionbridge::logging::tail(lines.unwrap_or(200))?)
}

fn launch_request(workspace: &LocatedWorkspace, input: GuiLaunchInput) -> GuiResult<LaunchRequest> {
    let loaded = settings::load(workspace)?;
    let (profile_name, profile) = loaded.settings.profile(input.profile.as_deref())?;
    let duration = parse_duration(&profile.expires_in).map_err(|error| {
        sessionbridge::SessionBridgeError::domain(
            sessionbridge::ErrorCode::InvalidInput,
            error.to_string(),
        )
    })?;
    let expires_in_seconds = i64::try_from(duration.as_secs()).map_err(|_| {
        sessionbridge::SessionBridgeError::domain(
            sessionbridge::ErrorCode::InvalidInput,
            "invitation expiry is too large",
        )
    })?;
    let shell = match profile.shell {
        SettingsShell::Posix => LaunchShell::Posix,
        SettingsShell::Powershell => LaunchShell::Powershell,
    };
    Ok(LaunchRequest {
        title: input.title,
        objective: input.objective,
        agents: input.agents.unwrap_or(profile.agents),
        expires_in_seconds,
        autonomy_policy: profile.autonomy,
        target_branch: profile.target_branch.clone(),
        capability_limits: Vec::new(),
        prompt_root: workspace.paths.prompts_dir.clone(),
        shell,
        profile: profile_name.to_owned(),
        allow_dirty_baseline: false,
    })
}

fn open_workspace(path: impl Into<PathBuf>) -> GuiResult<LocatedWorkspace> {
    Ok(LocatedWorkspace::discover(path.into())?)
}

fn parse_task_id(value: &str) -> GuiResult<TaskId> {
    TaskId::from_str(value).map_err(invalid_id)
}

fn invalid_id(error: impl std::fmt::Display) -> GuiError {
    sessionbridge::SessionBridgeError::domain(
        sessionbridge::ErrorCode::InvalidInput,
        format!("invalid identifier: {error}"),
    )
    .into()
}

fn internal(message: &str) -> GuiError {
    sessionbridge::SessionBridgeError::domain(sessionbridge::ErrorCode::Internal, message).into()
}
