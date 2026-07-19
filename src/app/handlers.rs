use std::{
    fs,
    io::{BufRead, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, Stdio},
    time::{Duration as StdDuration, Instant},
};

use serde::{Deserialize, Serialize};
use time::Duration;
use uuid::Uuid;

use crate::{
    Result,
    app::{
        agent::{AgentService, ConsolidationResolution, ProposeActionInput},
        await_service::{AwaitService, RegisterAwaitRequest},
        chat::{ChatService, SendChatMessageRequest},
        collaboration_service::{
            AcceptContractRequest, ApprovalDecision, AttestCapabilityRequest, ClearIntentRequest,
            CollaborationService, CoordinationAction, CoordinationRequestInput,
            CoordinationResponseInput, DecisionResolution, OpenBlockerRequest,
            ProposeDecisionRequest, PublishContractRequest, RecordApprovalRequest,
            ResolveBlockerRequest, ResolveDecisionRequest, ResolveInvalidationRequest,
            ResolvePlanChangeRequest, RetireParticipantRequest, ReviseContractRequest,
            SetIntentRequest, inspect_entity, list_participants, show_retirement,
        },
        completion_service::{
            AgentMutationRequest, CompletionService, CompletionTrigger, ConcludeRequest,
            TaskCompletionContext,
        },
        deliverable::{AddDeliverableRequest, DeliverableService},
        external_wait::{ExternalDetector, ExternalWaitService, RegisterExternalWaitRequest},
        invite::{
            CreateInvitationBatchRequest, InvitationService, RevokeInvitationRequest,
            RotateInvitationRequest, ShowInvitationRequest,
        },
        join::{JoinRequest, JoinService},
        launch::{LaunchRequest, LaunchService, LaunchShell},
        maintenance,
        planning::{
            AddRequirementRequest, PlanningService, ProposeWorkItemRequest, RelateWorkItemsRequest,
            ReopenWorkItemRequest, ReportGapRequest, ResolveGapRequest, SplitChildInput,
            SplitWorkItemRequest, SupersedeRequirementRequest,
        },
        runtime::{AgentRuntime, locate_workspace},
        settings,
        speculative::{CheckpointSpeculativeRequest, SpeculativeService, StartSpeculativeRequest},
        task::{
            AddCriterionRequest, EditCriterionRequest, PublishTaskRequest, TaskMutationRequest,
            TaskService, WorkspaceBinding, resolve_task_reference,
        },
        work::{
            AddArtifactRequest, AddEvidenceRequest, ArtifactInput, ChangeSetInput,
            CheckpointRequest, ClaimCloseAction, ClaimWorkItemRequest, CloseClaimRequest,
            CompleteReviewRequest, EvidenceInput, IntegrateChangeSetRequest, IntegrationOutcome,
            ReclaimClaimRequest, RenewClaimRequest, ResourceRequest, ReviewDecision,
            ScopeChangeRequest, ScopeReconcileRequest, SubmitWorkItemRequest, WorkService,
        },
        workflow::{ReviewWorkflowRequest, StartWorkflowRequest, SubmitBundle, WorkflowService},
        workspace_service::WorkspaceService,
    },
    cli::{
        AgentAwaitCommand, AgentChatCommand, AgentCommand, AgentConsolidateCommand,
        AgentIntentCommand, AgentInviteCommand, ApprovalCommand, ApprovalTargetTypeArg,
        ArtifactCommand, AttemptCommand, BlockerCommand, CapabilityCommand, ChangesetCommand,
        ClaimCommand, ClaimScopeCommand, Cli, Command, ConfigCommand, ContractCommand,
        CoordCommand, CoordinationResponse, DecisionCommand, DeliverableCommand,
        DiagnosticsCommand, EvidenceCommand, GapCommand, IntegrationOutcomeArg,
        InvalidationCommand, InviteCreateArgs, ItemCommand, LaunchCommand, LogsCommand, McpCommand,
        ParticipantCommand, ParticipantRetirementCommand, PlanCommand, PromptCommand,
        PromptShell as CliShell, ProposalDecisionArg, RequirementCommand, ReviewCommand,
        ReviewDecisionArg, SecretDelivery, TaskCommand, TaskCriterionCommand, WorkCommand,
        WorkIntegrateCommand, WorkSpeculateCommand, WorkspaceCommand,
    },
    clock::{Clock, SystemClock, unix_micros},
    domain::{
        CompletionAssessment, CompletionPolicy, ConclusionOutcome, CriterionKind, EvaluationMode,
        OpenSignalDisposition, TaskId, TaskPhase, WorkItemId,
    },
    error::{ErrorCode, SessionBridgeError},
    git::GitRepository,
    invitation::{
        InviteSecret, PromptDelivery, PromptRendererV1, PromptShell, PromptSpec,
        atomic_write_private, parse_duration as parse_compact_duration,
    },
    logging,
    output::{self, OutputFormat},
    storage::SqliteStore,
    workspace::{LocatedWorkspace, link as link_workspace},
};

#[derive(Debug, Clone)]
struct Globals {
    format: OutputFormat,
    context: Option<PathBuf>,
    workspace: Option<PathBuf>,
    request_id: Option<String>,
    expected_version: Option<u64>,
    expected_plan_version: Option<u64>,
    observed_plan_version: Option<u64>,
    non_interactive: bool,
}

pub fn dispatch(cli: Cli) -> Result<()> {
    let Cli {
        format,
        context,
        workspace,
        request_id,
        expected_version,
        expected_plan_version,
        observed_plan_version,
        non_interactive,
        command,
        ..
    } = cli;
    let globals = Globals {
        format,
        context,
        workspace,
        request_id,
        expected_version,
        expected_plan_version,
        observed_plan_version,
        non_interactive,
    };

    match command {
        Command::Init(args) => handle_init(&globals, args),
        Command::Gui(args) => handle_gui(&globals, args.foreground),
        Command::Launch(args) => handle_launch(&globals, args),
        Command::Prompt(args) => handle_prompt(&globals, args.command),
        Command::Status(args) => handle_status(&globals, args.task.as_deref().unwrap_or("current")),
        Command::Watch(args) => watch_task(
            &globals,
            crate::cli::TaskWatchArgs {
                task: args.task.unwrap_or_else(|| "current".into()),
                since: args.since,
                poll_interval: args.poll_interval,
                once: args.once,
            },
        ),
        Command::Config(args) => handle_config(&globals, args.command),
        Command::Workspace(args) => handle_workspace(&globals, args.command),
        Command::Task(args) => handle_task(&globals, args.command),
        Command::Agent(args) => handle_agent(&globals, args.command),
        Command::Capability(args) => handle_capability(&globals, args.command),
        Command::Migrate => handle_migrate(&globals),
        Command::Doctor => handle_doctor(&globals),
        Command::Participant(args) => handle_participant(&globals, args.command),
        Command::Requirement(args) => handle_requirement(&globals, args.command),
        Command::Gap(args) => handle_gap(&globals, args.command),
        Command::Blocker(args) => handle_blocker(&globals, args.command),
        Command::Item(args) => handle_item(&globals, args.command),
        Command::Work(args) => handle_work(&globals, args.command),
        Command::Claim(args) => handle_claim(&globals, args.command),
        Command::Attempt(args) => handle_attempt(&globals, args.command),
        Command::Artifact(args) => handle_artifact(&globals, args.command),
        Command::Evidence(args) => handle_evidence(&globals, args.command),
        Command::Plan(args) => handle_plan(&globals, args.command),
        Command::Approval(args) => handle_approval(&globals, args.command),
        Command::Review(args) => handle_review(&globals, args.command),
        Command::Changeset(args) => handle_changeset(&globals, args.command),
        Command::Contract(args) => handle_contract(&globals, args.command),
        Command::Decision(args) => handle_decision(&globals, args.command),
        Command::Deliverable(args) => handle_deliverable(&globals, args.command),
        Command::Coord(args) => handle_coordination(&globals, args.command),
        Command::Invalidation(args) => handle_invalidation(&globals, args.command),
        Command::Logs(args) => handle_logs(&globals, args.command),
        Command::Diagnostics(args) => match args.command {
            DiagnosticsCommand::Collaboration(args) => {
                let workspace = locate_workspace(globals.workspace.as_deref())?;
                let mut store = open_workspace_store(&workspace)?;
                let task_id = resolve_task_reference(
                    store.connection(),
                    workspace.config.project_id,
                    &args.task,
                )?;
                reconcile_workspace_task(&mut store, &workspace, task_id)?;
                let result =
                    crate::app::diagnostics::analyze(store.connection(), task_id, &SystemClock)?;
                output::write_value(globals.format, &result, None, || human_json(&result))
            }
        },
        Command::Mcp(args) => match args.command {
            McpCommand::Serve => {
                let (runtime, mut store) = open_agent_store(&globals)?;
                crate::app::mcp::serve(&mut store, &runtime.context)
            }
        },
        Command::Daemon(args) => {
            let workspace = locate_workspace(globals.workspace.as_deref())?;
            let mut store = open_workspace_store(&workspace)?;
            let interval = parse_time_duration(&args.interval)?;
            if interval.whole_milliseconds() <= 0 {
                return Err(SessionBridgeError::domain(
                    ErrorCode::InvalidInput,
                    "daemon interval must be positive",
                ));
            }
            let service = ExternalWaitService::new(SystemClock);
            loop {
                let result = service.scan_due(&mut store, args.limit)?;
                if args.once {
                    return output::write_value(globals.format, &result, None, || {
                        human_json(&result)
                    });
                }
                std::thread::sleep(StdDuration::from_millis(
                    interval.whole_milliseconds() as u64
                ));
            }
        }
    }
}

#[derive(Debug, Serialize)]
struct GuiLaunchResult {
    executable: String,
    mode: &'static str,
    pid: Option<u32>,
    exit_code: Option<i32>,
}

fn handle_gui(globals: &Globals, foreground: bool) -> Result<()> {
    let executable = locate_gui_executable()?;
    let mut command = ProcessCommand::new(&executable);
    if foreground {
        let status = command.status().map_err(|source| SessionBridgeError::Io {
            path: executable.clone(),
            source,
        })?;
        if !status.success() {
            return Err(SessionBridgeError::domain(
                ErrorCode::Internal,
                format!("GUI exited unsuccessfully with status {status}"),
            ));
        }
        let result = GuiLaunchResult {
            executable: executable.display().to_string(),
            mode: "foreground",
            pid: None,
            exit_code: status.code(),
        };
        return output::write_value(globals.format, &result, None, || human_json(&result));
    }
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|source| SessionBridgeError::Io {
            path: executable.clone(),
            source,
        })?;
    let result = GuiLaunchResult {
        executable: executable.display().to_string(),
        mode: "background",
        pid: Some(child.id()),
        exit_code: None,
    };
    output::write_value(globals.format, &result, None, || human_json(&result))
}

fn locate_gui_executable() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("SESSIONBRIDGE_GUI").map(PathBuf::from) {
        if path.is_file() {
            return Ok(path);
        }
        return Err(SessionBridgeError::domain(
            ErrorCode::NotFound,
            format!(
                "SESSIONBRIDGE_GUI does not point to a file: {}",
                path.display()
            ),
        ));
    }
    if let Ok(current) = std::env::current_exe()
        && let Some(directory) = current.parent()
    {
        let sibling = directory.join("sessionbridge-gui");
        if sibling.is_file() {
            return Ok(sibling);
        }
    }
    if let Some(path) = find_in_path("sessionbridge-gui") {
        return Ok(path);
    }
    Err(SessionBridgeError::domain(
        ErrorCode::NotFound,
        "SessionBridge GUI executable was not found; install sessionbridge-gui beside sessionbridge, add it to PATH, or set SESSIONBRIDGE_GUI",
    ))
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|directory| directory.join(name))
        .find(|path| path.is_file())
}

fn handle_work(globals: &Globals, command: WorkCommand) -> Result<()> {
    let adopt_existing = matches!(&command, WorkCommand::Adopt(_));
    match command {
        WorkCommand::Start(args) | WorkCommand::Adopt(args) => {
            let resources = args
                .resources_file
                .as_deref()
                .map(read_json_file::<Vec<ResourceRequest>>)
                .transpose()?
                .unwrap_or_default();
            let inputs = args
                .inputs_file
                .as_deref()
                .map(read_json_file::<Vec<crate::app::work::AttemptInputPin>>)
                .transpose()?
                .unwrap_or_default();
            let lease_seconds = args
                .lease
                .as_deref()
                .map(parse_time_duration)
                .transpose()?
                .map(|duration| duration.whole_seconds());
            let (runtime, mut store, _plan_dir) = open_agent_store_for_plan(globals, args.dry_run)?;
            let result = WorkflowService::new(SystemClock).start(
                &mut store,
                &runtime.context,
                StartWorkflowRequest {
                    item_id: parse_id(&args.item, "WorkItem ID")?,
                    resources,
                    inputs,
                    lease_seconds,
                    adopt_existing,
                    request_id: request_id(globals),
                },
            )?;
            emit_workflow_result(globals, args.dry_run, &result, Some(result.event_sequence))
        }
        WorkCommand::Submit(args) => {
            let bundle: SubmitBundle = read_json_file(&args.bundle)?;
            let (runtime, mut store, _plan_dir) = open_agent_store_for_plan(globals, args.dry_run)?;
            let result = WorkflowService::new(SystemClock).submit(
                &mut store,
                &runtime.context,
                parse_id(&args.item, "WorkItem ID")?,
                bundle,
                request_id(globals),
            )?;
            emit_workflow_result(globals, args.dry_run, &result, Some(result.event_sequence))
        }
        WorkCommand::Review(args) => {
            let evidence_ids = args
                .evidence_file
                .as_deref()
                .map(read_json_file::<Vec<String>>)
                .transpose()?
                .unwrap_or_default()
                .into_iter()
                .map(|value| parse_id(&value, "Evidence ID"))
                .collect::<Result<Vec<_>>>()?;
            let lease = parse_time_duration(&args.lease)?.whole_seconds();
            let (runtime, mut store, _plan_dir) = open_agent_store_for_plan(globals, args.dry_run)?;
            let result = WorkflowService::new(SystemClock).review(
                &mut store,
                &runtime.context,
                ReviewWorkflowRequest {
                    item_id: parse_id(&args.item, "WorkItem ID")?,
                    decision: match args.decision {
                        ReviewDecisionArg::Approved => ReviewDecision::Approved,
                        ReviewDecisionArg::ChangesRequested => ReviewDecision::ChangesRequested,
                    },
                    evidence_ids,
                    lease_seconds: lease,
                    request_id: request_id(globals),
                },
            )?;
            emit_workflow_result(
                globals,
                args.dry_run,
                &result,
                Some(result.review.event_sequence),
            )
        }
        WorkCommand::Integrate(args) => match args.command {
            WorkIntegrateCommand::Prepare(args) => {
                let lease = parse_time_duration(&args.lease)?.whole_seconds();
                let (runtime, mut store, _plan_dir) =
                    open_agent_store_for_plan(globals, args.dry_run)?;
                let result = WorkflowService::new(SystemClock).prepare_integration(
                    &mut store,
                    &runtime.context,
                    parse_id(&args.item, "WorkItem ID")?,
                    lease,
                    request_id(globals),
                )?;
                emit_workflow_result(globals, args.dry_run, &result, None)
            }
            WorkIntegrateCommand::Confirm(args) => {
                let details = read_optional_json_value(args.details_file.as_deref())?
                    .unwrap_or_else(|| serde_json::json!({}));
                let (runtime, mut store, _plan_dir) =
                    open_agent_store_for_plan(globals, args.dry_run)?;
                let result = WorkflowService::new(SystemClock).confirm_integration(
                    &mut store,
                    &runtime.context,
                    &args.preparation,
                    details,
                    request_id(globals),
                )?;
                emit_workflow_result(
                    globals,
                    args.dry_run,
                    &result,
                    Some(result.integration.event_sequence),
                )
            }
        },
        WorkCommand::Speculate(args) => match args.command {
            WorkSpeculateCommand::Start(args) => {
                let mut input: StartSpeculativeRequest = read_json_input(&args.input_json)?;
                input.request_id = request_id(globals);
                let (runtime, mut store) = open_agent_store(globals)?;
                let result = SpeculativeService::new(SystemClock).start(
                    &mut store,
                    &runtime.context,
                    input,
                )?;
                output::write_value(globals.format, &result, None, || human_json(&result))
            }
            WorkSpeculateCommand::Checkpoint(args) => {
                let input: CheckpointSpeculativeRequest = read_json_file(&args.input)?;
                let (runtime, mut store) = open_agent_store(globals)?;
                let result = SpeculativeService::new(SystemClock).checkpoint(
                    &mut store,
                    &runtime.context,
                    &args.speculation,
                    input,
                )?;
                output::write_value(globals.format, &result, None, || human_json(&result))
            }
            WorkSpeculateCommand::Adopt(args) => {
                let (runtime, mut store) = open_agent_store(globals)?;
                let result = SpeculativeService::new(SystemClock).propose_adoption(
                    &mut store,
                    &runtime.context,
                    &args.id,
                    request_id(globals),
                )?;
                output::write_value(globals.format, &result, Some(result.event_sequence), || {
                    human_json(&result)
                })
            }
            WorkSpeculateCommand::Abandon(args) => {
                let (runtime, mut store) = open_agent_store(globals)?;
                let result = SpeculativeService::new(SystemClock).abandon(
                    &mut store,
                    &runtime.context,
                    &args.speculation,
                    &args.reason,
                )?;
                output::write_value(globals.format, &result, None, || human_json(&result))
            }
        },
    }
}

fn handle_logs(globals: &Globals, command: LogsCommand) -> Result<()> {
    match command {
        LogsCommand::Path => {
            let result = logging::location()?;
            output::write_value(globals.format, &result, None, || {
                format!(
                    "Log directory: {}\nRotation: {}\nRetained files: {}\n",
                    result.log_dir.display(),
                    result.rotation,
                    result.max_log_files,
                )
            })
        }
        LogsCommand::Tail(args) => {
            let result = logging::tail(args.lines)?;
            output::write_value(globals.format, &result, None, || human_json(&result))
        }
    }
}

fn handle_deliverable(globals: &Globals, command: DeliverableCommand) -> Result<()> {
    let workspace = locate_workspace(globals.workspace.as_deref())?;
    let mut store = open_workspace_store(&workspace)?;
    let binding = WorkspaceBinding::from(&workspace);
    let service = DeliverableService::new(SystemClock);
    let task_reference = match &command {
        DeliverableCommand::List { task } => task.as_deref().unwrap_or("current"),
        DeliverableCommand::Add { task, .. }
        | DeliverableCommand::Map { task, .. }
        | DeliverableCommand::Verify { task, .. } => task,
    };
    let task_id = resolve_task_reference(
        store.connection(),
        workspace.config.project_id,
        task_reference,
    )?;
    let value = match command {
        DeliverableCommand::List { .. } => {
            serde_json::to_value(service.list(store.connection(), binding, task_id)?)?
        }
        DeliverableCommand::Add {
            key,
            title,
            description,
            kind,
            required,
            ..
        } => serde_json::to_value(service.add(
            store.connection_mut(),
            binding,
            AddDeliverableRequest {
                task_id,
                key,
                title,
                description,
                kind,
                required,
            },
        )?)?,
        DeliverableCommand::Map {
            deliverable, item, ..
        } => {
            let item_id = parse_id(&item, "WorkItem ID")?;
            serde_json::to_value(service.map(
                store.connection_mut(),
                binding,
                task_id,
                &deliverable,
                item_id,
            )?)?
        }
        DeliverableCommand::Verify {
            deliverable,
            evidence,
            ..
        } => serde_json::to_value(service.verify(
            store.connection_mut(),
            binding,
            task_id,
            &deliverable,
            &evidence,
        )?)?,
    };
    output::write_value(globals.format, &value, None, || human_json(&value))
}

fn handle_init(globals: &Globals, args: crate::cli::InitArgs) -> Result<()> {
    let root = fs::canonicalize(&args.path).map_err(|source| SessionBridgeError::Io {
        path: args.path.clone(),
        source,
    })?;
    let name = args.name.unwrap_or_else(|| {
        root.file_name()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .unwrap_or("SessionBridge")
            .to_owned()
    });
    let result = WorkspaceService::new(SystemClock).initialize(&root, name, None)?;
    output::write_value(globals.format, &result, result.event_cursor, || {
        format!(
            "Workspace {} ({})\nRoot: {}\nDatabase: {}\nSchema: {}\n",
            result.name,
            result.project_id,
            result.root.display(),
            result.database_path.display(),
            result.schema_version
        )
    })
}

fn handle_launch(globals: &Globals, mut args: crate::cli::LaunchArgs) -> Result<()> {
    let workspace = locate_workspace(globals.workspace.as_deref())?;
    let service = LaunchService::new(SystemClock);
    if let Some(command) = args.command.take() {
        let LaunchCommand::Resume(resume) = command;
        let mut store = open_workspace_store(&workspace)?;
        let result = service.resume(
            &mut store,
            &workspace,
            &resume.launch,
            resume.accept_baseline_change,
        )?;
        return output::write_value(globals.format, &result, None, || launch_human(&result));
    }

    let loaded_settings = settings::load(&workspace)?;
    let interactive = args.brief_text.is_none() && args.brief.is_none();
    if interactive {
        require_interactive_launch(globals)?;
        prompt_launch_source(&mut args)?;
    }
    let (objective, derived_title) = launch_source(&args)?;
    if interactive && args.profile.is_none() {
        args.profile = Some(prompt_with_default(
            "Profile",
            &loaded_settings.settings.default_profile,
        )?);
    }
    let (profile_name, profile) = loaded_settings.settings.profile(args.profile.as_deref())?;
    if interactive {
        if args.title.is_none() {
            args.title = Some(prompt_with_default("Title", &derived_title)?);
        }
        if args.agents.is_none() {
            let value = prompt_with_default("Agent count", &profile.agents.to_string())?;
            args.agents = Some(value.parse().map_err(|error| {
                SessionBridgeError::domain(
                    ErrorCode::InvalidInput,
                    format!("invalid Agent count: {error}"),
                )
            })?);
        }
        if args.expires_in.is_none() {
            args.expires_in = Some(prompt_with_default(
                "Invitation expiry",
                &profile.expires_in,
            )?);
        }
        if args.autonomy.is_none() {
            let value = prompt_with_default("Autonomy", profile.autonomy.as_str())?;
            args.autonomy = Some(value.parse().map_err(|error: String| {
                SessionBridgeError::domain(ErrorCode::InvalidInput, error)
            })?);
        }
        if args.target_branch.is_none() {
            let current = GitRepository::open(&workspace.paths.root)?
                .current_branch()?
                .or_else(|| profile.target_branch.clone())
                .unwrap_or_default();
            let value = prompt_optional("Target branch", Some(&current))?;
            args.target_branch = (!value.is_empty()).then_some(value);
        }
        if args.capabilities_file.is_none() {
            let value = prompt_optional("Capabilities file", None)?;
            args.capabilities_file = (!value.is_empty()).then(|| PathBuf::from(value));
        }
        if args.shell.is_none() {
            let default = match profile.shell {
                settings::SettingsShell::Posix => "posix",
                settings::SettingsShell::Powershell => "powershell",
            };
            let value = prompt_with_default("Prompt shell", default)?;
            args.shell = Some(match value.as_str() {
                "posix" => CliShell::Posix,
                "powershell" => CliShell::Powershell,
                _ => {
                    return Err(SessionBridgeError::domain(
                        ErrorCode::InvalidInput,
                        "prompt shell must be posix or powershell",
                    ));
                }
            });
        }
    }

    let title = args
        .title
        .as_deref()
        .unwrap_or(&derived_title)
        .trim()
        .to_owned();
    let prompt_root = absolute_path(
        args.prompt_dir
            .as_deref()
            .unwrap_or(&workspace.paths.prompts_dir),
    )?;
    let request = LaunchRequest {
        title,
        objective,
        agents: args.agents.unwrap_or(profile.agents),
        expires_in_seconds: parse_time_duration(
            args.expires_in.as_deref().unwrap_or(&profile.expires_in),
        )?
        .whole_seconds(),
        autonomy_policy: args.autonomy.unwrap_or(profile.autonomy),
        target_branch: args.target_branch.or_else(|| profile.target_branch.clone()),
        capability_limits: read_optional_list(args.capabilities_file.as_deref())?,
        prompt_root,
        shell: match args.shell {
            Some(CliShell::Posix) => LaunchShell::Posix,
            Some(CliShell::Powershell) => LaunchShell::Powershell,
            None => match profile.shell {
                settings::SettingsShell::Posix => LaunchShell::Posix,
                settings::SettingsShell::Powershell => LaunchShell::Powershell,
            },
        },
        profile: profile_name.to_owned(),
        allow_dirty_baseline: args.allow_dirty_baseline,
    };
    let preflight = service.preflight(&workspace, &request)?;
    if args.dry_run {
        return output::write_value(globals.format, &preflight, None, || human_json(&preflight));
    }
    if interactive && !args.yes && !confirm_launch(&preflight)? {
        return Err(SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            "launch cancelled before publication",
        ));
    }
    let mut store = open_workspace_store(&workspace)?;
    let result = service.launch(
        &mut store,
        &workspace,
        request,
        globals.request_id.as_deref(),
        false,
    )?;
    output::write_value(globals.format, &result, None, || launch_human(&result))
}

fn handle_config(globals: &Globals, command: ConfigCommand) -> Result<()> {
    let workspace = locate_workspace(globals.workspace.as_deref())?;
    let result = match command {
        ConfigCommand::Init => settings::initialize(&workspace)?,
        ConfigCommand::Show | ConfigCommand::Validate => settings::load(&workspace)?,
    };
    output::write_value(globals.format, &result, None, || human_json(&result))
}

fn handle_status(globals: &Globals, task_reference: &str) -> Result<()> {
    let workspace = locate_workspace(globals.workspace.as_deref())?;
    let mut store = open_workspace_store(&workspace)?;
    let task_id = resolve_task_reference(
        store.connection(),
        workspace.config.project_id,
        task_reference,
    )?;
    reconcile_workspace_task(&mut store, &workspace, task_id)?;
    let service = TaskService::new(SystemClock);
    let binding = WorkspaceBinding::from(&workspace);
    let details = service.show(store.connection(), binding, task_id)?;
    let health = service.health(store.connection(), binding, task_id)?;
    let result = serde_json::json!({ "details": details, "health": health });
    output::write_value(globals.format, &result, Some(health.event_cursor), || {
        human_json(&result)
    })
}

fn handle_prompt(globals: &Globals, command: PromptCommand) -> Result<()> {
    let workspace = locate_workspace(globals.workspace.as_deref())?;
    let mut store = open_workspace_store(&workspace)?;
    let clock = SystemClock;
    match command {
        PromptCommand::List(args) => {
            let task_id = resolve_task_reference(
                store.connection(),
                workspace.config.project_id,
                args.task.as_deref().unwrap_or("current"),
            )?;
            reconcile_workspace_task(&mut store, &workspace, task_id)?;
            let service = InvitationService::new(
                &mut store,
                &clock,
                workspace.config.project_id,
                &workspace.paths.prompts_dir,
            )?;
            let mut prompts = service.list(task_id)?;
            if !args.all_batches {
                let batch_id = prompts.last().map(|prompt| prompt.batch_id.clone());
                prompts.retain(|prompt| Some(&prompt.batch_id) == batch_id.as_ref());
            }
            output::write_value(globals.format, &prompts, None, || {
                prompt_list_human(&prompts)
            })
        }
        PromptCommand::Show(args) => {
            let task_id = resolve_task_reference(
                store.connection(),
                workspace.config.project_id,
                args.task.as_deref().unwrap_or("current"),
            )?;
            reconcile_workspace_task(&mut store, &workspace, task_id)?;
            let service = InvitationService::new(
                &mut store,
                &clock,
                workspace.config.project_id,
                &workspace.paths.prompts_dir,
            )?;
            let prompts = service.list(task_id)?;
            let batch_id = args
                .batch
                .or_else(|| prompts.last().map(|prompt| prompt.batch_id.clone()))
                .ok_or_else(|| {
                    SessionBridgeError::domain(
                        ErrorCode::InviteNotFound,
                        "Task has no invitation prompt batch",
                    )
                })?;
            let invitation = prompts
                .iter()
                .find(|prompt| {
                    prompt.batch_id == batch_id && prompt.batch_index == usize::from(args.index)
                })
                .ok_or_else(|| {
                    SessionBridgeError::domain(
                        ErrorCode::InviteNotFound,
                        format!(
                            "prompt index {} was not found in invitation batch {}",
                            args.index, batch_id
                        ),
                    )
                    .with_details(serde_json::json!({
                        "task_id": task_id,
                        "batch_id": batch_id,
                        "batch_index": args.index,
                    }))
                })?;
            let result = service.show(ShowInvitationRequest {
                invitation_id: invitation.invite_id,
                include_prompt: true,
                audit: args.audit,
            })?;
            if globals.format == OutputFormat::Text {
                return output::write_text(result.prompt.as_deref().unwrap_or(""));
            }
            output::write_value(globals.format, &result, None, || human_json(&result))
        }
    }
}

fn handle_workspace(globals: &Globals, command: WorkspaceCommand) -> Result<()> {
    match command {
        WorkspaceCommand::Link(args) => {
            let source = LocatedWorkspace::discover(&args.source)?;
            let linked = link_workspace(&source, &args.path)?;
            let result = serde_json::json!({
                "project_id": linked.config.project_id,
                "name": linked.config.name,
                "root": linked.paths.root,
                "database_path": linked.paths.database_path,
                "repository_instance_id": linked.paths.repository_instance_id,
            });
            output::write_value(globals.format, &result, None, || human_json(&result))
        }
    }
}

fn handle_task(globals: &Globals, command: TaskCommand) -> Result<()> {
    match command {
        TaskCommand::Watch(args) => watch_task(globals, args),
        command => {
            let workspace = locate_workspace(globals.workspace.as_deref())?;
            let mut store = open_workspace_store(&workspace)?;
            let service = TaskService::new(SystemClock);
            let binding = WorkspaceBinding::from(&workspace);
            match command {
                TaskCommand::Publish(args) => {
                    let objective = read_text_source(args.objective, args.objective_file)?;
                    let result = service.publish(
                        store.connection_mut(),
                        binding,
                        PublishTaskRequest {
                            title: args.title,
                            objective,
                            constraints: read_optional_list(args.constraints_file.as_deref())?,
                            non_goals: read_optional_list(args.non_goals_file.as_deref())?,
                            done_conditions: read_optional_list(args.done_file.as_deref())?,
                            autonomy_policy: args.autonomy,
                            completion_policy: CompletionPolicy::AllWorkersConcluded,
                            target_branch: args.target_branch,
                            request_id: globals.request_id.clone(),
                        },
                    )?;
                    output::write_value(
                        globals.format,
                        &result,
                        Some(result.event_sequence),
                        || task_human(&result.task, Some(result.criteria.len())),
                    )
                }
                TaskCommand::Show(args) => {
                    let task_id = resolve_task_reference(
                        store.connection(),
                        workspace.config.project_id,
                        &args.task,
                    )?;
                    reconcile_workspace_task(&mut store, &workspace, task_id)?;
                    let result = service.show(store.connection(), binding, task_id)?;
                    output::write_value(globals.format, &result, Some(result.event_cursor), || {
                        task_human(&result.task, Some(result.criteria.len()))
                    })
                }
                TaskCommand::Health(args) => {
                    let task_id = resolve_task_reference(
                        store.connection(),
                        workspace.config.project_id,
                        &args.task,
                    )?;
                    reconcile_workspace_task(&mut store, &workspace, task_id)?;
                    let result = service.health(store.connection(), binding, task_id)?;
                    output::write_value(globals.format, &result, Some(result.event_cursor), || {
                        human_json(&result)
                    })
                }
                TaskCommand::Reconcile(args) => {
                    let task_id = resolve_task_reference(
                        store.connection(),
                        workspace.config.project_id,
                        &args.task,
                    )?;
                    let result = crate::app::task_reconciler::TaskReconciler::new(SystemClock)
                        .reconcile_workspace(&mut store, &workspace, task_id)?;
                    output::write_value(globals.format, &result, result.event_sequence, || {
                        human_json(&result)
                    })
                }
                TaskCommand::Integrity(args) => {
                    let task_id = resolve_task_reference(
                        store.connection(),
                        workspace.config.project_id,
                        &args.task,
                    )?;
                    crate::app::task_reconciler::TaskReconciler::new(SystemClock)
                        .reconcile_workspace(&mut store, &workspace, task_id)?;
                    let result =
                        crate::app::task_reconciler::load_integrity(store.connection(), task_id)?;
                    output::write_value(globals.format, &result, None, || human_json(&result))
                }
                TaskCommand::Pause(args) => mutate_task(
                    globals,
                    &service,
                    store.connection_mut(),
                    binding,
                    args,
                    TaskMutation::Pause,
                ),
                TaskCommand::Resume(args) => mutate_task(
                    globals,
                    &service,
                    store.connection_mut(),
                    binding,
                    args,
                    TaskMutation::Resume,
                ),
                TaskCommand::Cancel(args) => mutate_task(
                    globals,
                    &service,
                    store.connection_mut(),
                    binding,
                    args,
                    TaskMutation::Cancel,
                ),
                TaskCommand::Reopen(args) => mutate_task(
                    globals,
                    &service,
                    store.connection_mut(),
                    binding,
                    args,
                    TaskMutation::Reopen,
                ),
                TaskCommand::Archive(args) => mutate_task(
                    globals,
                    &service,
                    store.connection_mut(),
                    binding,
                    args,
                    TaskMutation::Archive,
                ),
                TaskCommand::Criterion(args) => match args.command {
                    TaskCriterionCommand::Add(args) => {
                        let kind = criterion_kind(args.kind);
                        let content = read_text_source(args.content, args.content_file)?;
                        let evaluation_mode = args
                            .evaluation_mode
                            .map(evaluation_mode)
                            .unwrap_or_else(|| default_evaluation_mode(kind));
                        let task_id = resolve_task_reference(
                            store.connection(),
                            workspace.config.project_id,
                            &args.task,
                        )?;
                        let result = service.add_criterion(
                            store.connection_mut(),
                            binding,
                            AddCriterionRequest {
                                task_id,
                                kind,
                                content,
                                required: args.required,
                                evaluation_mode,
                                request_id: globals.request_id.clone(),
                            },
                        )?;
                        emit_event_result(globals, &result, result.event_sequence)
                    }
                    TaskCriterionCommand::Edit(args) => {
                        let content = read_text_source(args.content, args.content_file)?;
                        let task_id = resolve_task_reference(
                            store.connection(),
                            workspace.config.project_id,
                            &args.task,
                        )?;
                        let result = service.edit_criterion(
                            store.connection_mut(),
                            binding,
                            EditCriterionRequest {
                                task_id,
                                criterion_id: parse_id(&args.criterion, "TaskCriterion ID")?,
                                content,
                                evaluation_mode: args.evaluation_mode.map(evaluation_mode),
                                reason: args.reason,
                                request_id: globals.request_id.clone(),
                            },
                        )?;
                        emit_event_result(globals, &result, result.event_sequence)
                    }
                },
                TaskCommand::Watch(_) => {
                    unreachable!("watch handled before opening a long-lived store")
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum TaskMutation {
    Pause,
    Resume,
    Cancel,
    Reopen,
    Archive,
}

fn mutate_task<C: crate::clock::Clock>(
    globals: &Globals,
    service: &TaskService<C>,
    connection: &mut rusqlite::Connection,
    binding: WorkspaceBinding<'_>,
    args: crate::cli::TaskLifecycleArgs,
    mutation: TaskMutation,
) -> Result<()> {
    let request = TaskMutationRequest {
        task_id: resolve_task_reference(connection, binding.id, &args.task)?,
        reason: args.reason,
        request_id: globals.request_id.clone(),
    };
    let result = match mutation {
        TaskMutation::Pause => service.pause(connection, binding, request),
        TaskMutation::Resume => service.resume(connection, binding, request),
        TaskMutation::Cancel => service.cancel(connection, binding, request),
        TaskMutation::Reopen => service.reopen(connection, binding, request),
        TaskMutation::Archive => service.archive(connection, binding, request),
    }?;
    output::write_value(globals.format, &result, Some(result.event_sequence), || {
        task_human(&result.task, None)
    })
}

fn watch_task(globals: &Globals, args: crate::cli::TaskWatchArgs) -> Result<()> {
    if !args.once && globals.format == OutputFormat::Json {
        return Err(SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            "continuous watch requires --format jsonl or a human format",
        ));
    }
    let workspace = locate_workspace(globals.workspace.as_deref())?;
    let task_id = {
        let store = open_workspace_store(&workspace)?;
        resolve_task_reference(store.connection(), workspace.config.project_id, &args.task)?
    };
    let interval = parse_poll_interval(&args.poll_interval)?;
    let mut previous = args.since.map(|value| value as i64);
    loop {
        let mut store = open_workspace_store(&workspace)?;
        reconcile_workspace_task(&mut store, &workspace, task_id)?;
        let service = TaskService::new(SystemClock);
        let binding = WorkspaceBinding::from(&workspace);
        let health = service.health(store.connection(), binding, task_id)?;
        if previous != Some(health.event_cursor) || args.once {
            match globals.format {
                OutputFormat::Jsonl => output::write_jsonl(&health)?,
                _ => {
                    output::write_value(globals.format, &health, Some(health.event_cursor), || {
                        human_json(&health)
                    })?
                }
            }
            previous = Some(health.event_cursor);
        }
        if args.once
            || matches!(
                health.task.phase,
                TaskPhase::Completed | TaskPhase::Cancelled | TaskPhase::Archived
            )
        {
            return Ok(());
        }
        drop(store);
        std::thread::sleep(interval);
    }
}

fn handle_agent(globals: &Globals, command: AgentCommand) -> Result<()> {
    match command {
        AgentCommand::Invite(args) => handle_invite(globals, args.command),
        AgentCommand::Join(args) => handle_join(globals, args),
        AgentCommand::Whoami(_) => {
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = AgentService::new(SystemClock).whoami(&mut store, &runtime.context)?;
            output::write_value(globals.format, &result, None, || human_json(&result))
        }
        AgentCommand::Sync(args) => {
            let (runtime, mut store) = open_agent_store(globals)?;
            if let Some(since) = args.since {
                let snapshot_id = args.snapshot_id.as_deref().ok_or_else(|| {
                    SessionBridgeError::full_resync_required(
                        "incremental sync requires the prior snapshot ID",
                    )
                })?;
                AgentService::new(SystemClock).verify_snapshot_token(
                    &runtime.context,
                    snapshot_id,
                    since as i64,
                )?;
            }
            let result = AgentService::new(SystemClock).sync(
                &mut store,
                &runtime.context,
                args.since.map(|value| value as i64),
            )?;
            output::write_value(globals.format, &result, Some(result.next_cursor), || {
                human_json(&result)
            })
        }
        AgentCommand::Next(args) => {
            let (runtime, mut store) = open_agent_store(globals)?;
            let service = AgentService::new(SystemClock);
            let result = if args.preview {
                service.next_preview(
                    &mut store,
                    &runtime.context,
                    args.limit.unwrap_or(20) as usize,
                )?
            } else {
                service.next(
                    &mut store,
                    &runtime.context,
                    args.limit.unwrap_or(20) as usize,
                )?
            };
            output::write_value(globals.format, &result, Some(result.next_cursor), || {
                human_json(&result)
            })
        }
        AgentCommand::Operations(args) => {
            let result = match args.operation.as_deref() {
                Some(operation) => {
                    serde_json::to_value(crate::app::operation_contract::get(operation)?)?
                }
                None => serde_json::to_value(crate::app::operation_contract::list())?,
            };
            output::write_value(globals.format, &result, None, || human_json(&result))
        }
        AgentCommand::ProposeAction(args) => {
            let input: ProposeActionInput = read_json_file(&args.input)?;
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = AgentService::new(SystemClock).propose_action(
                &mut store,
                &runtime.context,
                args.kind,
                input,
                request_id(globals),
            )?;
            output::write_value(globals.format, &result, Some(result.event_sequence), || {
                human_json(&result)
            })
        }
        AgentCommand::CompletionChallenge(args) => {
            let mut input: ProposeActionInput = read_json_file(&args.input)?;
            input.safety_relevant = true;
            input.provenance["proposal_type"] = serde_json::json!("completion_challenge");
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = AgentService::new(SystemClock).propose_action(
                &mut store,
                &runtime.context,
                "completion_challenge".into(),
                input,
                request_id(globals),
            )?;
            output::write_value(globals.format, &result, Some(result.event_sequence), || {
                human_json(&result)
            })
        }
        AgentCommand::RequestCapability(args) => {
            let mut input: ProposeActionInput = read_json_file(&args.input)?;
            input.provenance["proposal_type"] = serde_json::json!("capability_request");
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = AgentService::new(SystemClock).propose_action(
                &mut store,
                &runtime.context,
                "capability_request".into(),
                input,
                request_id(globals),
            )?;
            output::write_value(globals.format, &result, Some(result.event_sequence), || {
                human_json(&result)
            })
        }
        AgentCommand::Proposal(args) => {
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = match args.command {
                crate::cli::AgentProposalCommand::Resolve(args) => AgentService::new(SystemClock)
                    .resolve_action_proposal(
                    &mut store,
                    &runtime.context,
                    &args.proposal,
                    match args.decision {
                        ProposalDecisionArg::Accepted => "accepted",
                        ProposalDecisionArg::Rejected => "rejected",
                        ProposalDecisionArg::Superseded => "superseded",
                    },
                    args.reason,
                )?,
            };
            output::write_value(globals.format, &result, Some(result.event_sequence), || {
                human_json(&result)
            })
        }
        AgentCommand::Chat(args) => {
            let (runtime, mut store) = open_agent_store(globals)?;
            let service = ChatService::new(SystemClock);
            match args.command {
                AgentChatCommand::Send(args) => {
                    let mut request: SendChatMessageRequest = read_json_file(&args.input)?;
                    request.request_id = request_id(globals);
                    let result = service.send_agent(&mut store, &runtime.context, request)?;
                    output::write_value(
                        globals.format,
                        &result,
                        Some(result.event_sequence),
                        || human_json(&result),
                    )
                }
                AgentChatCommand::History(args) => {
                    AgentService::new(SystemClock).whoami(&mut store, &runtime.context)?;
                    let result = service.history(
                        store.connection(),
                        runtime.context.task_id,
                        args.since as i64,
                        args.limit as usize,
                    )?;
                    output::write_value(globals.format, &result, Some(result.next_cursor), || {
                        human_json(&result)
                    })
                }
                AgentChatCommand::Inbox(args) => {
                    AgentService::new(SystemClock).whoami(&mut store, &runtime.context)?;
                    let result =
                        service.inbox(store.connection(), &runtime.context, args.limit as usize)?;
                    output::write_value(globals.format, &result, None, || human_json(&result))
                }
                AgentChatCommand::Ack(args) => {
                    let result = service.ack_agent(&mut store, &runtime.context, args.messages)?;
                    output::write_value(globals.format, &result, None, || human_json(&result))
                }
                AgentChatCommand::Await(args) => {
                    let result = service.await_agent(
                        &mut store,
                        &runtime.context,
                        args.limit as usize,
                        !args.include_broadcasts,
                    )?;
                    output::write_value(globals.format, &result, None, || human_json(&result))
                }
            }
        }
        AgentCommand::Consolidate(args) => {
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = match args.command {
                AgentConsolidateCommand::Claim(args) => AgentService::new(SystemClock)
                    .claim_consolidation(&mut store, &runtime.context, &args.id)?,
                AgentConsolidateCommand::Resolve(args) => {
                    let resolution: ConsolidationResolution = read_json_file(&args.input)?;
                    AgentService::new(SystemClock).resolve_consolidation(
                        &mut store,
                        &runtime.context,
                        &args.id,
                        resolution,
                    )?
                }
            };
            output::write_value(globals.format, &result, Some(result.event_sequence), || {
                human_json(&result)
            })
        }
        AgentCommand::Candidates(_) => {
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = AgentService::new(SystemClock).sync(&mut store, &runtime.context, None)?;
            output::write_value(
                globals.format,
                &result.candidates,
                Some(result.next_cursor),
                || human_json(&result.candidates),
            )
        }
        AgentCommand::Inbox(args) => {
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = AgentService::new(SystemClock).sync(
                &mut store,
                &runtime.context,
                args.since.map(|value| value as i64),
            )?;
            output::write_value(
                globals.format,
                &result.inbox,
                Some(result.next_cursor),
                || human_json(&result.inbox),
            )
        }
        AgentCommand::Inspect(args) => {
            let (runtime, mut store) = open_agent_store(globals)?;
            AgentService::new(SystemClock).whoami(&mut store, &runtime.context)?;
            let result = inspect_entity(store.connection(), runtime.context.task_id, &args.entity)?;
            output::write_value(globals.format, &result, None, || human_json(&result))
        }
        AgentCommand::Await(args) => await_agent(globals, args.command),
        AgentCommand::Intent(args) => handle_intent(globals, args.command),
        AgentCommand::Conclude(args) => conclude_agent(globals, args),
        AgentCommand::Resume(args) => {
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = CompletionService::new(SystemClock).resume(
                &mut store,
                &runtime.context,
                AgentMutationRequest {
                    reason: args.reason,
                    request_id: request_id(globals),
                },
            )?;
            output::write_value(globals.format, &result, Some(result.event_sequence), || {
                human_json(&result)
            })
        }
        AgentCommand::Leave(args) => {
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = CompletionService::new(SystemClock).leave(
                &mut store,
                &runtime.context,
                AgentMutationRequest {
                    reason: args.reason,
                    request_id: request_id(globals),
                },
            )?;
            output::write_value(globals.format, &result, Some(result.event_sequence), || {
                human_json(&result)
            })
        }
        AgentCommand::List(args) => {
            let workspace = locate_workspace(globals.workspace.as_deref())?;
            let store = open_workspace_store(&workspace)?;
            let task_id = resolve_task_reference(
                store.connection(),
                workspace.config.project_id,
                &args.task,
            )?;
            let mut result = list_participants(store.connection(), task_id)?;
            if let Some(state) = args.state {
                let state = participant_state_arg(state);
                if state == "stale" {
                    let stale_before =
                        unix_micros(SystemClock.now() - crate::app::task::PARTICIPANT_STALE_AFTER);
                    result.retain(|participant| {
                        participant.ended_at_micros.is_none()
                            && participant.last_seen_at_micros <= stale_before
                    });
                } else {
                    result.retain(|participant| participant.state == state);
                }
            }
            output::write_value(globals.format, &result, None, || human_json(&result))
        }
    }
}

fn conclude_agent(globals: &Globals, args: crate::cli::AgentConcludeArgs) -> Result<()> {
    let (runtime, mut store) = open_agent_store(globals)?;
    let mut assessment: CompletionAssessment = match args.assessment_file.as_deref() {
        Some(path) => read_json_file(path)?,
        None => {
            let summary = args.summary.ok_or_else(|| {
                SessionBridgeError::domain(
                    ErrorCode::InvalidInput,
                    "agent conclude requires either --assessment-file or --summary with at least one --checked area",
                )
            })?;
            if args.checked.is_empty() {
                return Err(SessionBridgeError::domain(
                    ErrorCode::InvalidInput,
                    "agent conclude requires at least one --checked area",
                ));
            }
            CompletionAssessment {
                summary,
                checked: args.checked,
                open_signals: Vec::new(),
                remaining_risks: args.risk,
                expected_future_work: args.outcome == ConclusionOutcome::Blocked,
            }
        }
    };
    append_signal_references(
        &mut assessment,
        "BLOCKER",
        &args.blocker,
        "referenced by agent conclude --blocker",
    );
    append_signal_references(
        &mut assessment,
        "GAP",
        &args.gap,
        "referenced by agent conclude --gap",
    );
    append_signal_references(
        &mut assessment,
        "COORDINATION_REQUEST",
        &args.coordination_request,
        "referenced by agent conclude --coord-request",
    );
    let completion_token = match args.completion_token {
        Some(token) => token,
        None => {
            AgentService::new(SystemClock)
                .next(&mut store, &runtime.context, 20)?
                .tokens
                .completion
        }
    };
    let token_basis = AgentService::new(SystemClock)
        .verify_completion_token(&runtime.context, &completion_token)?;
    let result = CompletionService::new(SystemClock).conclude(
        &mut store,
        &runtime.context,
        ConcludeRequest {
            outcome: args.outcome,
            basis_completion_revision: token_basis.completion_revision,
            basis_event_cursor: token_basis.event_cursor,
            basis_plan_version: token_basis.plan_version,
            assessment,
            request_id: request_id(globals),
        },
    )?;
    output::write_value(globals.format, &result, Some(result.event_sequence), || {
        human_json(&result)
    })
}

fn append_signal_references(
    assessment: &mut CompletionAssessment,
    code: &str,
    entity_ids: &[String],
    rationale: &str,
) {
    for entity_id in entity_ids {
        if assessment.open_signals.iter().any(|signal| {
            signal.code == code && signal.entity_id.as_deref() == Some(entity_id.as_str())
        }) {
            continue;
        }
        assessment.open_signals.push(OpenSignalDisposition {
            code: code.to_owned(),
            entity_id: Some(entity_id.clone()),
            disposition: "requires_intervention".into(),
            rationale: rationale.to_owned(),
        });
    }
}

fn await_agent(globals: &Globals, command: AgentAwaitCommand) -> Result<()> {
    let (runtime, mut store) = open_agent_store(globals)?;
    let service = AwaitService::new(SystemClock);
    match command {
        AgentAwaitCommand::WorkItems(args) => {
            let work_item_ids = args
                .items
                .iter()
                .map(|value| parse_id::<WorkItemId>(value, "WorkItem ID"))
                .collect::<Result<Vec<_>>>()?;
            let view = service.register(
                &mut store,
                &runtime.context,
                RegisterAwaitRequest {
                    work_item_ids,
                    mode: args.mode,
                    request_id: request_id(globals),
                },
            )?;
            block_on_await(globals, &service, &runtime, &mut store, view)
        }
        AgentAwaitCommand::External(args) => {
            let detector: ExternalDetector = read_json_file(&args.detector)?;
            let deadline_seconds = args
                .deadline_in
                .as_deref()
                .map(parse_time_duration)
                .transpose()?
                .map(|duration| duration.whole_seconds());
            let reassess_interval_seconds =
                parse_time_duration(&args.reassess_every)?.whole_seconds();
            let external = ExternalWaitService::new(SystemClock);
            let mut view = external.register(
                &mut store,
                &runtime.context,
                RegisterExternalWaitRequest {
                    detector,
                    deadline_seconds,
                    reassess_interval_seconds,
                    request_id: request_id(globals),
                },
            )?;
            while view.status == "active" {
                std::thread::sleep(StdDuration::from_secs(1));
                view = external.poll(&mut store, &runtime.context, &view.wait_id)?;
            }
            output::write_value(globals.format, &view, None, || human_json(&view))
        }
        AgentAwaitCommand::Resume(args) => {
            let view = service.show(&mut store, &runtime.context, &args.wait)?;
            block_on_await(globals, &service, &runtime, &mut store, view)
        }
        AgentAwaitCommand::Show(args) => {
            let view = service.show(&mut store, &runtime.context, &args.wait)?;
            output::write_value(
                globals.format,
                &view,
                view.completion_event_cursor
                    .or(Some(view.created_event_cursor)),
                || human_json(&view),
            )
        }
        AgentAwaitCommand::List => {
            let views = service.list(&mut store, &runtime.context)?;
            output::write_value(globals.format, &views, None, || human_json(&views))
        }
    }
}

fn block_on_await<C: Clock + Clone>(
    globals: &Globals,
    service: &AwaitService<C>,
    runtime: &AgentRuntime,
    store: &mut SqliteStore,
    mut view: crate::app::await_service::AwaitView,
) -> Result<()> {
    let mut last_touch = Instant::now();
    while view.status == "active" {
        std::thread::sleep(StdDuration::from_secs(1));
        view = service.poll(store, &runtime.context, &view.wait_id)?;
        if last_touch.elapsed() >= StdDuration::from_secs(60) {
            service.touch(store, &runtime.context)?;
            last_touch = Instant::now();
        }
    }
    let result = view.result.ok_or_else(|| {
        SessionBridgeError::domain(
            ErrorCode::Internal,
            "terminal await does not contain a callback result",
        )
    })?;
    output::write_value(
        globals.format,
        &result,
        Some(result.trigger_event_cursor),
        || human_json(&result),
    )
}

fn handle_intent(globals: &Globals, command: AgentIntentCommand) -> Result<()> {
    match command {
        AgentIntentCommand::Set(args) => {
            let request: SetIntentRequest =
                read_json_with_request_id(&args.input_json, request_id(globals))?;
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = CollaborationService::new(SystemClock).set_intent(
                &mut store,
                &runtime.context,
                request,
            )?;
            emit_event_result(globals, &result, result.event_sequence)
        }
        AgentIntentCommand::Clear(args) => {
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = CollaborationService::new(SystemClock).clear_intent(
                &mut store,
                &runtime.context,
                ClearIntentRequest {
                    intent_id: args.intent,
                    reason: args.reason,
                    request_id: request_id(globals),
                },
            )?;
            emit_event_result(globals, &result, result.event_sequence)
        }
    }
}

fn handle_requirement(globals: &Globals, command: RequirementCommand) -> Result<()> {
    match command {
        RequirementCommand::Add(args) => {
            let (runtime, mut store) = open_agent_store(globals)?;
            let mut request: AddRequirementRequest = read_json_input(&args.input_json)?;
            apply_plan_protocol(
                globals,
                &mut request.request_id,
                &mut request.base_plan_version,
            )?;
            let result = PlanningService::new(SystemClock).add_requirement(
                &mut store,
                &runtime.context,
                request,
            )?;
            output::write_value(globals.format, &result, Some(result.event_sequence), || {
                human_json(&result)
            })
        }
        RequirementCommand::Supersede(args) => {
            if args.input_json.is_some() {
                return Err(SessionBridgeError::domain(
                    ErrorCode::InvalidInput,
                    "requirement supersede uses typed flags; --input-json is not supported",
                ));
            }
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = PlanningService::new(SystemClock).supersede_requirement(
                &mut store,
                &runtime.context,
                SupersedeRequirementRequest {
                    requirement_id: parse_id(&args.requirement, "Requirement ID")?,
                    replacement_id: args
                        .replacement
                        .as_deref()
                        .map(|value| parse_id(value, "replacement Requirement ID"))
                        .transpose()?,
                    base_plan_version: required_global_i64(
                        globals.expected_plan_version,
                        "--expected-plan-version",
                    )?,
                    reason: args.reason,
                    request_id: request_id(globals),
                },
            )?;
            emit_event_result(globals, &result, result.event_sequence)
        }
    }
}

fn handle_gap(globals: &Globals, command: GapCommand) -> Result<()> {
    match command {
        GapCommand::Report(args) => {
            let (runtime, mut store) = open_agent_store(globals)?;
            let mut request: ReportGapRequest = read_json_input(&args.input_json)?;
            if request.request_id.is_empty() {
                request.request_id = request_id(globals);
            } else if let Some(global) = &globals.request_id {
                request.request_id.clone_from(global);
            }
            let result = PlanningService::new(SystemClock).report_gap(
                &mut store,
                &runtime.context,
                request,
            )?;
            output::write_value(globals.format, &result, Some(result.event_sequence), || {
                human_json(&result)
            })
        }
        GapCommand::Resolve(args) => {
            if args.input_json.is_some() {
                return Err(SessionBridgeError::domain(
                    ErrorCode::InvalidInput,
                    "gap resolve uses typed flags; --input-json is not supported",
                ));
            }
            let reason = args.reason.ok_or_else(|| {
                SessionBridgeError::domain(ErrorCode::InvalidInput, "gap resolve requires --reason")
            })?;
            let evidence_ids: Vec<String> = match args.evidence_file.as_deref() {
                Some(path) => read_json_input(path)?,
                None => Vec::new(),
            };
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = PlanningService::new(SystemClock).resolve_gap(
                &mut store,
                &runtime.context,
                ResolveGapRequest {
                    gap_id: args.gap,
                    reason,
                    evidence_ids,
                    request_id: request_id(globals),
                },
            )?;
            emit_event_result(globals, &result, result.event_sequence)
        }
    }
}

fn handle_blocker(globals: &Globals, command: BlockerCommand) -> Result<()> {
    match command {
        BlockerCommand::Open(args) => {
            let request: OpenBlockerRequest =
                read_json_with_request_id(&args.input_json, request_id(globals))?;
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = CollaborationService::new(SystemClock).open_blocker(
                &mut store,
                &runtime.context,
                request,
            )?;
            emit_event_result(globals, &result, result.event_sequence)
        }
        BlockerCommand::Resolve(args) => {
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = CollaborationService::new(SystemClock).resolve_blocker(
                &mut store,
                &runtime.context,
                ResolveBlockerRequest {
                    blocker_id: args.blocker,
                    resolution: args.resolution,
                    request_id: request_id(globals),
                },
            )?;
            emit_event_result(globals, &result, result.event_sequence)
        }
    }
}

fn handle_item(globals: &Globals, command: ItemCommand) -> Result<()> {
    match command {
        ItemCommand::Propose(args) => {
            let (runtime, mut store) = open_agent_store(globals)?;
            let mut request: ProposeWorkItemRequest = read_json_input(&args.input_json)?;
            apply_plan_protocol(
                globals,
                &mut request.request_id,
                &mut request.base_plan_version,
            )?;
            let service = PlanningService::new(SystemClock);
            if args.claim {
                let result = service.propose_and_claim(&mut store, &runtime.context, request)?;
                output::write_value(
                    globals.format,
                    &result,
                    Some(result.claim.event_sequence),
                    || human_json(&result),
                )
            } else {
                let result = service.propose_work_item(&mut store, &runtime.context, request)?;
                output::write_value(globals.format, &result, Some(result.event_sequence), || {
                    human_json(&result)
                })
            }
        }
        ItemCommand::Split(args) => split_item(globals, args),
        ItemCommand::Relate(args) => {
            let relation_type = match args.relation {
                crate::cli::WorkRelationKind::DependsOn => "depends_on",
                crate::cli::WorkRelationKind::Duplicates => "duplicates",
                crate::cli::WorkRelationKind::Supersedes => "supersedes",
            };
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = PlanningService::new(SystemClock).relate_work_items(
                &mut store,
                &runtime.context,
                RelateWorkItemsRequest {
                    from_id: parse_id(&args.from, "source WorkItem ID")?,
                    to_id: parse_id(&args.to, "target WorkItem ID")?,
                    relation_type: relation_type.into(),
                    base_plan_version: required_global_i64(
                        globals.expected_plan_version,
                        "--expected-plan-version",
                    )?,
                    reason: args.reason.unwrap_or_else(|| "Relate WorkItems".into()),
                    request_id: request_id(globals),
                },
            )?;
            emit_event_result(globals, &result, result.event_sequence)
        }
        ItemCommand::Claim(args) => {
            let (runtime, mut store) = open_agent_store(globals)?;
            let resources = read_optional_json_array(args.resources_file.as_deref())?;
            let inputs = read_optional_json_array(args.inputs_file.as_deref())?;
            let result = WorkService::new(SystemClock).claim(
                &mut store,
                &runtime.context,
                ClaimWorkItemRequest {
                    item_id: parse_id(&args.item, "WorkItem ID")?,
                    mode: args.mode,
                    expected_item_version: required_global_i64(
                        globals.expected_version,
                        "--expected-version",
                    )?,
                    observed_plan_version: required_global_i64(
                        globals.observed_plan_version,
                        "--observed-plan-version",
                    )?,
                    resources,
                    inputs,
                    lease_seconds: args
                        .lease
                        .as_deref()
                        .map(parse_duration_seconds)
                        .transpose()?,
                    request_id: request_id(globals),
                },
            )?;
            output::write_value(globals.format, &result, Some(result.event_sequence), || {
                human_json(&result)
            })
        }
        ItemCommand::Inspect(args) => {
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = WorkService::new(SystemClock).inspect(
                &mut store,
                &runtime.context,
                parse_id(&args.id, "WorkItem ID")?,
            )?;
            output::write_value(globals.format, &result, None, || human_json(&result))
        }
        ItemCommand::Submit(args) => {
            let (runtime, mut store) = open_agent_store(globals)?;
            let evidence = read_evidence_file(&args.evidence_file)?;
            let change_set: ChangeSetInput = read_json_input(&args.changeset_file)?;
            let result = WorkService::new(SystemClock).submit(
                &mut store,
                &runtime.context,
                SubmitWorkItemRequest {
                    item_id: parse_id(&args.item, "WorkItem ID")?,
                    claim_id: parse_id(&args.claim, "Claim ID")?,
                    generation: to_i64(args.fence, "Claim fence")?,
                    evidence,
                    change_set,
                    request_id: request_id(globals),
                },
            )?;
            output::write_value(globals.format, &result, Some(result.event_sequence), || {
                human_json(&result)
            })
        }
        ItemCommand::Reopen(args) => {
            let evidence_ids: Vec<String> = match args.evidence_file.as_deref() {
                Some(path) => read_json_input(path)?,
                None => Vec::new(),
            };
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = PlanningService::new(SystemClock).reopen_work_item(
                &mut store,
                &runtime.context,
                ReopenWorkItemRequest {
                    item_id: parse_id(&args.item, "WorkItem ID")?,
                    expected_item_version: required_global_i64(
                        globals.expected_version,
                        "--expected-version",
                    )?,
                    base_plan_version: required_global_i64(
                        globals.expected_plan_version,
                        "--expected-plan-version",
                    )?,
                    reason: args.reason,
                    evidence_ids,
                    request_id: request_id(globals),
                },
            )?;
            emit_event_result(globals, &result, result.event_sequence)
        }
    }
}

#[derive(Debug, Deserialize)]
struct SplitPayload {
    children: Vec<SplitChildInput>,
    reason: String,
}

fn split_item(globals: &Globals, args: crate::cli::ItemSplitArgs) -> Result<()> {
    if args.claim.is_some() || args.fence.is_some() {
        return Err(SessionBridgeError::domain(
            ErrorCode::SessionHasActiveWork,
            "release the active Claim before splitting a WorkItem",
        ));
    }
    let payload: SplitPayload = read_json_input(&args.input_json)?;
    let (runtime, mut store) = open_agent_store(globals)?;
    let result = PlanningService::new(SystemClock).split_work_item(
        &mut store,
        &runtime.context,
        SplitWorkItemRequest {
            item_id: parse_id(&args.item, "WorkItem ID")?,
            expected_item_version: required_global_i64(
                globals.expected_version,
                "--expected-version",
            )?,
            base_plan_version: required_global_i64(
                globals.expected_plan_version,
                "--expected-plan-version",
            )?,
            children: payload.children,
            reason: payload.reason,
            request_id: request_id(globals),
        },
    )?;
    emit_event_result(globals, &result, result.event_sequence)
}

fn handle_claim(globals: &Globals, command: ClaimCommand) -> Result<()> {
    match command {
        ClaimCommand::Renew(args) => {
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = WorkService::new(SystemClock).renew(
                &mut store,
                &runtime.context,
                RenewClaimRequest {
                    claim_id: parse_id(&args.claim, "Claim ID")?,
                    generation: to_i64(args.fence, "Claim fence")?,
                    lease_seconds: args
                        .lease
                        .as_deref()
                        .map(parse_duration_seconds)
                        .transpose()?,
                    request_id: request_id(globals),
                },
            )?;
            emit_event_result(globals, &result, result.event_sequence)
        }
        ClaimCommand::Release(args) => {
            close_claim_from_cli(globals, args, ClaimCloseAction::Release)
        }
        ClaimCommand::Yield(args) => close_claim_from_cli(globals, args, ClaimCloseAction::Yield),
        ClaimCommand::Abandon(args) => {
            close_claim_from_cli(globals, args, ClaimCloseAction::Abandon)
        }
        ClaimCommand::Reclaim(args) => {
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = WorkService::new(SystemClock).reclaim(
                &mut store,
                &runtime.context,
                ReclaimClaimRequest {
                    claim_id: parse_id(&args.claim, "Claim ID")?,
                    resources: read_optional_json_array(args.resources_file.as_deref())?,
                    handoff: read_optional_json_value(args.handoff_file.as_deref())?,
                    lease_seconds: None,
                    request_id: request_id(globals),
                },
            )?;
            emit_event_result(globals, &result, result.event_sequence)
        }
        ClaimCommand::Scope(args) => match args.command {
            ClaimScopeCommand::Expand(args) => change_claim_scope(globals, args, true),
            ClaimScopeCommand::Reduce(args) => change_claim_scope(globals, args, false),
            ClaimScopeCommand::Reconcile(args) => {
                let (runtime, mut store) = open_agent_store(globals)?;
                let result = WorkService::new(SystemClock).reconcile_scope_from_git(
                    &mut store,
                    &runtime.context,
                    ScopeReconcileRequest {
                        claim_id: parse_id(&args.claim, "Claim ID")?,
                        generation: to_i64(args.fence, "Claim fence")?,
                        request_id: request_id(globals),
                    },
                )?;
                emit_event_result(globals, &result, result.event_sequence)
            }
        },
    }
}

fn close_claim_from_cli(
    globals: &Globals,
    args: crate::cli::ClaimCloseArgs,
    action: ClaimCloseAction,
) -> Result<()> {
    let (runtime, mut store) = open_agent_store(globals)?;
    let result = WorkService::new(SystemClock).close(
        &mut store,
        &runtime.context,
        CloseClaimRequest {
            claim_id: parse_id(&args.claim, "Claim ID")?,
            generation: to_i64(args.fence, "Claim fence")?,
            action,
            reason: args.reason,
            handoff: read_optional_json_value(args.handoff_file.as_deref())?,
            request_id: request_id(globals),
        },
    )?;
    emit_event_result(globals, &result, result.event_sequence)
}

#[derive(Debug, Deserialize)]
struct ScopePayload {
    resources: Vec<ResourceRequest>,
}

fn change_claim_scope(
    globals: &Globals,
    args: crate::cli::ClaimScopeChangeArgs,
    expand: bool,
) -> Result<()> {
    let payload: ScopePayload = read_json_input(&args.input_json)?;
    let generation = args
        .fence
        .ok_or_else(|| {
            SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "claim scope change requires --fence",
            )
        })
        .and_then(|value| to_i64(value, "Claim fence"))?;
    let (runtime, mut store) = open_agent_store(globals)?;
    let request = ScopeChangeRequest {
        claim_id: parse_id(&args.claim, "Claim ID")?,
        generation,
        resources: payload.resources,
        request_id: request_id(globals),
    };
    let service = WorkService::new(SystemClock);
    let result = if expand {
        service.expand_scope(&mut store, &runtime.context, request)
    } else {
        service.reduce_scope(&mut store, &runtime.context, request)
    }?;
    emit_event_result(globals, &result, result.event_sequence)
}

#[derive(Debug, Deserialize)]
struct CheckpointPayload {
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    changed_resources: Vec<String>,
    #[serde(default)]
    artifacts: Vec<ArtifactInput>,
    #[serde(default)]
    evidence: Vec<EvidenceInput>,
}

fn handle_attempt(globals: &Globals, command: AttemptCommand) -> Result<()> {
    match command {
        AttemptCommand::Checkpoint(args) => {
            let payload: CheckpointPayload = read_json_input(&args.input_json)?;
            let generation = args
                .fence
                .ok_or_else(|| {
                    SessionBridgeError::domain(
                        ErrorCode::InvalidInput,
                        "attempt checkpoint requires --fence",
                    )
                })
                .and_then(|value| to_i64(value, "Claim fence"))?;
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = WorkService::new(SystemClock).checkpoint(
                &mut store,
                &runtime.context,
                CheckpointRequest {
                    attempt_id: parse_id(&args.attempt, "Attempt ID")?,
                    generation,
                    summary: payload.summary,
                    changed_resources: payload.changed_resources,
                    artifacts: payload.artifacts,
                    evidence: payload.evidence,
                    request_id: request_id(globals),
                },
            )?;
            emit_event_result(globals, &result, result.event_sequence)
        }
    }
}

fn handle_artifact(globals: &Globals, command: ArtifactCommand) -> Result<()> {
    match command {
        ArtifactCommand::Add(args) => {
            let artifact: ArtifactInput = read_json_input(&args.input_json)?;
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = WorkService::new(SystemClock).add_artifact(
                &mut store,
                &runtime.context,
                AddArtifactRequest {
                    artifact,
                    request_id: request_id(globals),
                },
            )?;
            emit_event_result(globals, &result, result.event_sequence)
        }
    }
}

fn handle_evidence(globals: &Globals, command: EvidenceCommand) -> Result<()> {
    match command {
        EvidenceCommand::Add(args) => {
            let evidence: EvidenceInput = read_json_input(&args.input_json)?;
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = WorkService::new(SystemClock).add_evidence(
                &mut store,
                &runtime.context,
                AddEvidenceRequest {
                    evidence,
                    request_id: request_id(globals),
                },
            )?;
            emit_event_result(globals, &result, result.event_sequence)
        }
    }
}

fn handle_review(globals: &Globals, command: ReviewCommand) -> Result<()> {
    match command {
        ReviewCommand::Complete(args) => {
            let evidence_ids: Vec<String> = match args.evidence_file.as_deref() {
                Some(path) => read_json_input(path)?,
                None => Vec::new(),
            };
            let evidence_ids = evidence_ids
                .into_iter()
                .map(|value| parse_id(&value, "Evidence ID"))
                .collect::<Result<Vec<_>>>()?;
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = WorkService::new(SystemClock).complete_review(
                &mut store,
                &runtime.context,
                CompleteReviewRequest {
                    item_id: parse_id(&args.item, "WorkItem ID")?,
                    claim_id: parse_id(&args.claim, "Claim ID")?,
                    generation: to_i64(args.fence, "Claim fence")?,
                    decision: match args.decision {
                        ReviewDecisionArg::Approved => ReviewDecision::Approved,
                        ReviewDecisionArg::ChangesRequested => ReviewDecision::ChangesRequested,
                    },
                    evidence_ids,
                    request_id: request_id(globals),
                },
            )?;
            emit_event_result(globals, &result, result.event_sequence)
        }
    }
}

fn handle_changeset(globals: &Globals, command: ChangesetCommand) -> Result<()> {
    match command {
        ChangesetCommand::Integrate(args) => {
            let details = read_optional_json_value(args.details_file.as_deref())?
                .unwrap_or_else(|| serde_json::json!({}));
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = WorkService::new(SystemClock).integrate_change_set(
                &mut store,
                &runtime.context,
                IntegrateChangeSetRequest {
                    change_set_id: parse_id(&args.changeset, "ChangeSet ID")?,
                    claim_id: parse_id(&args.claim, "Claim ID")?,
                    generation: to_i64(args.fence, "Claim fence")?,
                    outcome: match args.outcome {
                        IntegrationOutcomeArg::Integrated => IntegrationOutcome::Integrated,
                        IntegrationOutcomeArg::Failed => IntegrationOutcome::Failed,
                    },
                    details,
                    request_id: request_id(globals),
                },
            )?;
            emit_event_result(globals, &result, result.event_sequence)
        }
    }
}

#[derive(Debug, Deserialize)]
struct ReviseContractPayload {
    #[serde(default = "default_unknown_compatibility")]
    compatibility: String,
    content: serde_json::Value,
}

fn handle_contract(globals: &Globals, command: ContractCommand) -> Result<()> {
    match command {
        ContractCommand::Publish(args) => {
            let request: PublishContractRequest =
                read_json_with_request_id(&args.input_json, request_id(globals))?;
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = CollaborationService::new(SystemClock).publish_contract(
                &mut store,
                &runtime.context,
                request,
            )?;
            emit_event_result(globals, &result, result.event_sequence)
        }
        ContractCommand::Revise(args) => {
            let payload: ReviseContractPayload = read_json_input(&args.input_json)?;
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = CollaborationService::new(SystemClock).revise_contract(
                &mut store,
                &runtime.context,
                ReviseContractRequest {
                    contract_id: args.contract,
                    compatibility: payload.compatibility,
                    content: payload.content,
                    request_id: request_id(globals),
                },
            )?;
            emit_event_result(globals, &result, result.event_sequence)
        }
        ContractCommand::Accept(args) => {
            let evidence_ids: Vec<String> = match args.evidence_file.as_deref() {
                Some(path) => read_json_input(path)?,
                None => Vec::new(),
            };
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = CollaborationService::new(SystemClock).accept_contract(
                &mut store,
                &runtime.context,
                AcceptContractRequest {
                    contract_id: args.contract,
                    revision: to_i64(args.revision, "Contract revision")?,
                    evidence_ids,
                    request_id: request_id(globals),
                },
            )?;
            emit_event_result(globals, &result, result.event_sequence)
        }
    }
}

fn handle_capability(globals: &Globals, command: CapabilityCommand) -> Result<()> {
    match command {
        CapabilityCommand::Attest(args) => {
            let request: AttestCapabilityRequest =
                read_json_with_request_id(&args.input_json, request_id(globals))?;
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = CollaborationService::new(SystemClock).attest_capability(
                &mut store,
                &runtime.context,
                request,
            )?;
            emit_event_result(globals, &result, result.event_sequence)
        }
    }
}

fn handle_plan(globals: &Globals, command: PlanCommand) -> Result<()> {
    let (args, decision) = match command {
        PlanCommand::Approve(args) => (args, ApprovalDecision::Approve),
        PlanCommand::Reject(args) => (args, ApprovalDecision::Reject),
    };
    let (runtime, mut store) = open_agent_store(globals)?;
    let result = CollaborationService::new(SystemClock).resolve_plan_change(
        &mut store,
        &runtime.context,
        ResolvePlanChangeRequest {
            plan_change_id: args.plan_change,
            decision,
            reason: args.reason,
            request_id: request_id(globals),
        },
    )?;
    emit_event_result(globals, &result, result.event_sequence)
}

fn handle_approval(globals: &Globals, command: ApprovalCommand) -> Result<()> {
    let (args, decision) = match command {
        ApprovalCommand::Approve(args) => (args, ApprovalDecision::Approve),
        ApprovalCommand::Reject(args) => (args, ApprovalDecision::Reject),
    };
    if args.target_type == ApprovalTargetTypeArg::PlanChange {
        let (runtime, mut store) = open_agent_store(globals)?;
        let result = CollaborationService::new(SystemClock).resolve_plan_change(
            &mut store,
            &runtime.context,
            ResolvePlanChangeRequest {
                plan_change_id: args.target,
                decision,
                reason: args.reason,
                request_id: request_id(globals),
            },
        )?;
        return emit_event_result(globals, &result, result.event_sequence);
    }
    let target_type = match args.target_type {
        ApprovalTargetTypeArg::PlanChange => unreachable!(),
        ApprovalTargetTypeArg::Decision => "decision",
        ApprovalTargetTypeArg::ParticipantRetirement => "participant_retirement",
        ApprovalTargetTypeArg::TaskReopen => "task_reopen",
    };
    let (runtime, mut store) = open_agent_store(globals)?;
    let result = CollaborationService::new(SystemClock).record_approval(
        &mut store,
        &runtime.context,
        RecordApprovalRequest {
            target_type: target_type.into(),
            target_id: args.target,
            decision,
            reason: args.reason,
            request_id: request_id(globals),
        },
    )?;
    emit_event_result(globals, &result, result.event_sequence)
}

fn handle_decision(globals: &Globals, command: DecisionCommand) -> Result<()> {
    match command {
        DecisionCommand::Propose(args) => {
            let request: ProposeDecisionRequest =
                read_json_with_request_id(&args.input_json, request_id(globals))?;
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = CollaborationService::new(SystemClock).propose_decision(
                &mut store,
                &runtime.context,
                request,
            )?;
            emit_event_result(globals, &result, result.event_sequence)
        }
        DecisionCommand::Approve(args) => {
            resolve_decision_from_cli(globals, args, DecisionResolution::Approve)
        }
        DecisionCommand::Reject(args) => {
            resolve_decision_from_cli(globals, args, DecisionResolution::Reject)
        }
    }
}

fn resolve_decision_from_cli(
    globals: &Globals,
    args: crate::cli::DecisionResolutionArgs,
    resolution: DecisionResolution,
) -> Result<()> {
    let evidence_ids: Vec<String> = match args.evidence_file.as_deref() {
        Some(path) => read_json_input(path)?,
        None => Vec::new(),
    };
    let (runtime, mut store) = open_agent_store(globals)?;
    let result = CollaborationService::new(SystemClock).resolve_decision(
        &mut store,
        &runtime.context,
        ResolveDecisionRequest {
            decision_id: args.decision,
            resolution,
            reason: args.reason,
            evidence_ids,
            request_id: request_id(globals),
        },
    )?;
    emit_event_result(globals, &result, result.event_sequence)
}

fn handle_coordination(globals: &Globals, command: CoordCommand) -> Result<()> {
    match command {
        CoordCommand::Request(args) => {
            let request: CoordinationRequestInput =
                read_json_with_request_id(&args.input_json, request_id(globals))?;
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = CollaborationService::new(SystemClock).request_coordination(
                &mut store,
                &runtime.context,
                request,
            )?;
            emit_event_result(globals, &result, result.event_sequence)
        }
        CoordCommand::Respond(args) => {
            let response = read_optional_json_value(args.input_json.as_deref())?
                .unwrap_or_else(|| serde_json::json!({}));
            let action = match args.action {
                CoordinationResponse::Acknowledge => CoordinationAction::Acknowledge,
                CoordinationResponse::Resolve => CoordinationAction::Resolve,
                CoordinationResponse::Cancel => CoordinationAction::Cancel,
            };
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = CollaborationService::new(SystemClock).respond_coordination(
                &mut store,
                &runtime.context,
                CoordinationResponseInput {
                    coordination_request_id: args.request,
                    action,
                    response,
                    request_id: request_id(globals),
                },
            )?;
            emit_event_result(globals, &result, result.event_sequence)
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct InvalidationResolutionPayload {
    #[serde(default)]
    evidence_ids: Vec<String>,
}

fn handle_invalidation(globals: &Globals, command: InvalidationCommand) -> Result<()> {
    match command {
        InvalidationCommand::Resolve(args) => {
            let payload: InvalidationResolutionPayload = match args.input_json.as_deref() {
                Some(path) => read_json_input(path)?,
                None => InvalidationResolutionPayload::default(),
            };
            let reason = args.reason.ok_or_else(|| {
                SessionBridgeError::domain(
                    ErrorCode::InvalidInput,
                    "invalidation resolve requires --reason",
                )
            })?;
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = CollaborationService::new(SystemClock).resolve_invalidation(
                &mut store,
                &runtime.context,
                ResolveInvalidationRequest {
                    invalidation_id: args.invalidation,
                    reason,
                    evidence_ids: payload.evidence_ids,
                    request_id: request_id(globals),
                },
            )?;
            emit_event_result(globals, &result, result.event_sequence)
        }
    }
}

fn handle_participant(globals: &Globals, command: ParticipantCommand) -> Result<()> {
    match command {
        ParticipantCommand::List(args) => {
            let (store, task_id) = if let Some(task) = args.task {
                let workspace = locate_workspace(globals.workspace.as_deref())?;
                let store = open_workspace_store(&workspace)?;
                let task_id =
                    resolve_task_reference(store.connection(), workspace.config.project_id, &task)?;
                (store, task_id)
            } else {
                let (runtime, store) = open_agent_store(globals)?;
                (store, runtime.context.task_id)
            };
            let mut result = list_participants(store.connection(), task_id)?;
            if let Some(state) = args.state {
                let state = participant_state_arg(state);
                if state == "stale" {
                    let stale_before =
                        unix_micros(SystemClock.now() - crate::app::task::PARTICIPANT_STALE_AFTER);
                    result.retain(|participant| {
                        participant.ended_at_micros.is_none()
                            && participant.last_seen_at_micros <= stale_before
                    });
                } else {
                    result.retain(|participant| participant.state == state);
                }
            }
            output::write_value(globals.format, &result, None, || human_json(&result))
        }
        ParticipantCommand::Retire(args) => {
            let handoff = read_optional_json_value(args.handoff_file.as_deref())?
                .unwrap_or_else(|| serde_json::json!({}));
            let evidence_ids: Vec<String> = match args.evidence_file.as_deref() {
                Some(path) => read_json_input(path)?,
                None => Vec::new(),
            };
            let (runtime, mut store) = open_agent_store(globals)?;
            let result = CollaborationService::new(SystemClock).retire_participant(
                &mut store,
                &runtime.context,
                RetireParticipantRequest {
                    participant_id: args.participant,
                    reason: args.reason,
                    handoff,
                    evidence_ids,
                    request_id: request_id(globals),
                },
            )?;
            emit_event_result(globals, &result, result.event_sequence)
        }
        ParticipantCommand::Retirement(args) => match args.command {
            ParticipantRetirementCommand::Show(args) => {
                let workspace = locate_workspace(globals.workspace.as_deref())?;
                let store = open_workspace_store(&workspace)?;
                let result = show_retirement(store.connection(), &args.id)?;
                output::write_value(globals.format, &result, None, || human_json(&result))
            }
        },
    }
}

fn emit_event_result<T: Serialize>(
    globals: &Globals,
    result: &T,
    event_sequence: i64,
) -> Result<()> {
    output::write_value(globals.format, result, Some(event_sequence), || {
        human_json(result)
    })
}

fn handle_invite(globals: &Globals, command: AgentInviteCommand) -> Result<()> {
    let workspace = locate_workspace(globals.workspace.as_deref())?;
    let mut store = open_workspace_store(&workspace)?;
    let clock = SystemClock;
    match command {
        AgentInviteCommand::Create(args) => {
            create_invites(globals, &workspace, &mut store, &clock, args)
        }
        AgentInviteCommand::List(args) => {
            let task_id = resolve_task_reference(
                store.connection(),
                workspace.config.project_id,
                &args.task,
            )?;
            let service = InvitationService::new(
                &mut store,
                &clock,
                workspace.config.project_id,
                &workspace.paths.prompts_dir,
            )?;
            let mut result = service.list(task_id)?;
            if let Some(state) = args.state {
                let expected = invitation_state_arg(state);
                result.retain(|invitation| invitation.status.as_str() == expected);
            }
            output::write_value(globals.format, &result, None, || human_json(&result))
        }
        AgentInviteCommand::Show(args) => {
            let service = InvitationService::new(
                &mut store,
                &clock,
                workspace.config.project_id,
                &workspace.paths.prompts_dir,
            )?;
            let result = service.show(ShowInvitationRequest {
                invitation_id: parse_id(&args.invite, "invitation ID")?,
                include_prompt: args.prompt,
                audit: args.audit,
            })?;
            if args.prompt && globals.format == OutputFormat::Text {
                return output::write_text(result.prompt.as_deref().unwrap_or(""));
            }
            output::write_value(globals.format, &result, None, || human_json(&result))
        }
        AgentInviteCommand::Revoke(args) => {
            let mut service = InvitationService::new(
                &mut store,
                &clock,
                workspace.config.project_id,
                &workspace.paths.prompts_dir,
            )?;
            let result = service.revoke(RevokeInvitationRequest {
                invitation_id: parse_id(&args.invite, "invitation ID")?,
                reason: args.reason,
                request_id: request_id(globals),
            })?;
            output::write_value(globals.format, &result, None, || human_json(&result))
        }
        AgentInviteCommand::Rotate(args) => {
            let inline = matches!(args.secret_delivery, Some(SecretDelivery::Inline));
            if inline && globals.format != OutputFormat::Text {
                return Err(SessionBridgeError::domain(
                    ErrorCode::SecretOutputNotAllowed,
                    "inline invitation secrets may only be emitted in text format",
                ));
            }
            let shell = args.shell;
            let root = args
                .prompt_dir
                .unwrap_or_else(|| workspace.paths.prompts_dir.clone());
            let mut service =
                InvitationService::new(&mut store, &clock, workspace.config.project_id, root)?;
            let result = service.rotate(RotateInvitationRequest {
                invitation_id: parse_id(&args.invite, "invitation ID")?,
                expires_in: parse_time_duration(args.expires_in.as_deref().unwrap_or("1h"))?,
                request_id: request_id(globals),
            })?;
            if inline {
                return emit_inline_invitation(&result, shell, true);
            }
            emit_invitation_batch(globals.format, &result)
        }
    }
}

fn create_invites(
    globals: &Globals,
    workspace: &LocatedWorkspace,
    store: &mut SqliteStore,
    clock: &SystemClock,
    args: InviteCreateArgs,
) -> Result<()> {
    let inline = args.secret_delivery == SecretDelivery::Inline;
    if inline && (args.count != 1 || globals.format != OutputFormat::Text) {
        return Err(SessionBridgeError::domain(
            ErrorCode::SecretOutputNotAllowed,
            "inline invitation secrets require a single invitation and text output",
        ));
    }
    if args.count > 1 && globals.format == OutputFormat::Text && args.prompt_dir.is_none() {
        return Err(SessionBridgeError::domain(
            ErrorCode::InviteOutputRequired,
            "batch text output requires --prompt-dir",
        ));
    }
    let provision_worktree = worker_invitation_provisioning(workspace, args.participant_kind)?;
    let artifact_root = args
        .prompt_dir
        .clone()
        .unwrap_or_else(|| workspace.paths.prompts_dir.clone());
    let task_id =
        resolve_task_reference(store.connection(), workspace.config.project_id, &args.task)?;
    let mut service =
        InvitationService::new(store, clock, workspace.config.project_id, artifact_root)?;
    let shell = args.shell;
    let result = service.create_batch(CreateInvitationBatchRequest {
        task_id,
        count: usize::from(args.count),
        participant_kind: args.participant_kind,
        expires_in: parse_time_duration(&args.expires_in)?,
        capability_limits: read_optional_list(args.capabilities_file.as_deref())?,
        request_id: request_id(globals),
        shell: match shell {
            CliShell::Posix => PromptShell::Posix,
            CliShell::Powershell => PromptShell::PowerShell,
        },
        provision_worktree,
    })?;
    if inline {
        return emit_inline_invitation(&result, shell, provision_worktree);
    }
    emit_invitation_batch(globals.format, &result)
}

fn worker_invitation_provisioning(
    workspace: &LocatedWorkspace,
    participant_kind: crate::domain::ParticipantKind,
) -> Result<bool> {
    if participant_kind == crate::domain::ParticipantKind::Observer {
        return Ok(false);
    }
    if workspace.repository.kind != crate::workspace::RepositoryKind::Git {
        return Err(SessionBridgeError::domain(
            ErrorCode::WorktreeConflict,
            "worker invitations require a Git repository for isolated worktrees",
        )
        .with_details(serde_json::json!({
            "workspace_root": workspace.paths.root,
            "recovery": "initialize Git and commit the project baseline before creating worker invitations",
        })));
    }
    GitRepository::open(&workspace.paths.root)?.require_initial_commit()?;
    Ok(true)
}

fn emit_invitation_batch(
    format: OutputFormat,
    result: &crate::app::invite::InvitationBatchManifest,
) -> Result<()> {
    match format {
        OutputFormat::Text if result.prompts.len() == 1 => {
            output::write_text(result.prompts[0].prompt.as_deref().unwrap_or(""))
        }
        OutputFormat::Text => output::write_text(
            &result
                .prompts
                .iter()
                .map(|prompt| prompt.prompt_path.display().to_string())
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        OutputFormat::Json => output::write_json(result, None),
        OutputFormat::Jsonl => {
            for prompt in &result.prompts {
                output::write_jsonl(prompt)?;
            }
            Ok(())
        }
        OutputFormat::Table => output::write_text(&human_json(result)),
    }
}

fn emit_inline_invitation(
    result: &crate::app::invite::InvitationBatchManifest,
    shell: CliShell,
    provision_worktree: bool,
) -> Result<()> {
    if result.prompts.len() != 1 {
        return Err(SessionBridgeError::domain(
            ErrorCode::SecretOutputNotAllowed,
            "inline invitation output requires exactly one invitation",
        ));
    }
    let invitation = &result.prompts[0];
    let invite_path = match &invitation.secret_delivery {
        crate::app::invite::PublicSecretDelivery::File { invite_path } => invite_path,
    };
    let secret: InviteSecret = crate::workspace::read_json_private(invite_path)?;
    let protocol_version = invitation.protocol_version.to_string();
    let task_id = invitation.task_id.to_string();
    let invite_id = invitation.invite_id.to_string();
    let rendered = PromptRendererV1.render(&PromptSpec {
        task_id: &task_id,
        invite_id: &invite_id,
        expires_at: &invitation.expires_at,
        protocol_version: &protocol_version,
        shell: match shell {
            CliShell::Posix => PromptShell::Posix,
            CliShell::Powershell => PromptShell::PowerShell,
        },
        delivery: PromptDelivery::Inline { secret: &secret },
        provision_worktree,
    })?;
    output::write_text(&rendered.text)
}

fn handle_join(globals: &Globals, args: crate::cli::AgentJoinArgs) -> Result<()> {
    let workspace = locate_workspace(globals.workspace.as_deref())?;
    let invite_file = match args.invite_file {
        Some(path) => path,
        None if args.invite_stdin => {
            let mut bytes = Vec::new();
            std::io::stdin()
                .lock()
                .read_to_end(&mut bytes)
                .map_err(|source| SessionBridgeError::Io {
                    path: PathBuf::from("<stdin>"),
                    source,
                })?;
            crate::invitation::InviteSecret::from_json_slice(&bytes)?;
            let path = workspace
                .paths
                .invites_dir
                .join(format!(".stdin-{}.json", Uuid::now_v7()));
            atomic_write_private(&path, &bytes)?;
            path
        }
        None => {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "an invitation source is required",
            ));
        }
    };
    let mut store = open_workspace_store(&workspace)?;
    let result = JoinService::new(SystemClock).join(
        &mut store,
        &workspace,
        JoinRequest {
            invite_file,
            provision_worktree: args.provision_worktree,
            worktree: args.worktree,
            declared_capabilities: read_optional_list(args.capabilities_file.as_deref())?,
            label: args.nickname,
            client_name: args.client_name,
            model: args.model,
            request_id: globals.request_id.clone(),
        },
    )?;
    output::write_value(globals.format, &result, Some(result.event_cursor), || {
        human_json(&result)
    })
}

fn handle_migrate(globals: &Globals) -> Result<()> {
    let workspace = locate_workspace(globals.workspace.as_deref())?;
    let result = maintenance::migrate(&workspace)?;
    output::write_value(globals.format, &result, None, || human_json(&result))
}

fn handle_doctor(globals: &Globals) -> Result<()> {
    let workspace = locate_workspace(globals.workspace.as_deref())?;
    let result = maintenance::doctor(&workspace)?;
    output::write_value(globals.format, &result, None, || human_json(&result))
}

fn open_workspace_store(workspace: &LocatedWorkspace) -> Result<SqliteStore> {
    SqliteStore::open_with_lock(
        &workspace.paths.database_path,
        &workspace.paths.migration_lock_path,
    )
}

fn open_agent_store(globals: &Globals) -> Result<(AgentRuntime, SqliteStore)> {
    let context_path = globals.context.as_ref().ok_or_else(|| {
        SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            "Agent command requires --context or SESSIONBRIDGE_CONTEXT",
        )
    })?;
    let runtime = AgentRuntime::load(context_path)?;
    let store = SqliteStore::open(Path::new(&runtime.context.database_path))?;
    Ok((runtime, store))
}

fn open_agent_store_for_plan(
    globals: &Globals,
    dry_run: bool,
) -> Result<(AgentRuntime, SqliteStore, Option<tempfile::TempDir>)> {
    let (runtime, store) = open_agent_store(globals)?;
    if !dry_run {
        return Ok((runtime, store, None));
    }
    let directory = tempfile::tempdir().map_err(|source| SessionBridgeError::Io {
        path: std::env::temp_dir(),
        source,
    })?;
    let database_path = directory.path().join("dry-run.db");
    store.connection().backup("main", &database_path, None)?;
    drop(store);
    let planned_store = SqliteStore::open(&database_path)?;
    Ok((runtime, planned_store, Some(directory)))
}

fn emit_workflow_result<T: Serialize>(
    globals: &Globals,
    dry_run: bool,
    result: &T,
    event_cursor: Option<i64>,
) -> Result<()> {
    if dry_run {
        let output = serde_json::json!({
            "dry_run": true,
            "database_mutated": false,
            "would_result": result,
        });
        output::write_value(globals.format, &output, None, || human_json(&output))
    } else {
        output::write_value(globals.format, result, event_cursor, || human_json(result))
    }
}

fn reconcile_workspace_task(
    store: &mut SqliteStore,
    workspace: &LocatedWorkspace,
    task_id: TaskId,
) -> Result<()> {
    crate::app::task_reconciler::TaskReconciler::new(SystemClock)
        .reconcile_workspace(store, workspace, task_id)?;
    CompletionService::new(SystemClock).reconcile_task(
        store,
        TaskCompletionContext {
            workspace_id: workspace.config.project_id,
            repository_instance_id: &workspace.paths.repository_instance_id,
            task_id,
        },
        CompletionTrigger {
            actor_type: "system",
            actor_id: None,
        },
    )?;
    Ok(())
}

fn require_interactive_launch(globals: &Globals) -> Result<()> {
    if globals.non_interactive
        || matches!(globals.format, OutputFormat::Json | OutputFormat::Jsonl)
        || !std::io::stdin().is_terminal()
        || !std::io::stdout().is_terminal()
    {
        return Err(SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            "launch requires BRIEF_TEXT or --brief outside an interactive terminal",
        ));
    }
    Ok(())
}

fn prompt_launch_source(args: &mut crate::cli::LaunchArgs) -> Result<()> {
    let path = prompt_optional(
        "Brief file path (leave blank for a one-line Objective)",
        None,
    )?;
    if path.is_empty() {
        args.brief_text = Some(prompt_required("Objective")?);
    } else {
        args.brief = Some(PathBuf::from(path));
    }
    Ok(())
}

fn launch_source(args: &crate::cli::LaunchArgs) -> Result<(String, String)> {
    match (&args.brief_text, &args.brief) {
        (Some(value), None) if !value.trim().is_empty() => {
            let objective = value.trim().to_owned();
            let title = first_nonempty_line(&objective)
                .map(strip_h1)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    SessionBridgeError::domain(
                        ErrorCode::InvalidInput,
                        "launch brief text cannot be empty",
                    )
                })?
                .to_owned();
            Ok((objective, title))
        }
        (None, Some(path)) => {
            let objective = read_nonempty_file(path)?;
            let title = objective
                .lines()
                .find_map(|line| line.trim().strip_prefix("# ").map(str::trim))
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .or_else(|| {
                    path.file_stem()
                        .and_then(|value| value.to_str())
                        .filter(|value| !value.trim().is_empty())
                        .map(str::to_owned)
                })
                .ok_or_else(|| {
                    SessionBridgeError::domain(
                        ErrorCode::InvalidInput,
                        "cannot derive a launch title; provide --title",
                    )
                })?;
            Ok((objective, title))
        }
        _ => Err(SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            "exactly one launch source is required: BRIEF_TEXT or --brief",
        )),
    }
}

fn first_nonempty_line(value: &str) -> Option<&str> {
    value.lines().map(str::trim).find(|line| !line.is_empty())
}

fn strip_h1(value: &str) -> &str {
    value.strip_prefix("# ").unwrap_or(value).trim()
}

fn prompt_required(label: &str) -> Result<String> {
    let value = prompt_optional(label, None)?;
    if value.is_empty() {
        return Err(SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            format!("{label} cannot be empty"),
        ));
    }
    Ok(value)
}

fn prompt_with_default(label: &str, default: &str) -> Result<String> {
    let value = prompt_optional(label, Some(default))?;
    Ok(if value.is_empty() {
        default.to_owned()
    } else {
        value
    })
}

fn prompt_optional(label: &str, default: Option<&str>) -> Result<String> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    LaunchPrompter::new(stdin.lock(), stdout.lock()).optional(label, default)
}

fn confirm_launch(preflight: &crate::app::launch::LaunchPreflight) -> Result<bool> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    LaunchPrompter::new(stdin.lock(), stdout.lock()).confirm(preflight)
}

struct LaunchPrompter<R, W> {
    reader: R,
    writer: W,
}

impl<R: BufRead, W: Write> LaunchPrompter<R, W> {
    fn new(reader: R, writer: W) -> Self {
        Self { reader, writer }
    }

    fn optional(&mut self, label: &str, default: Option<&str>) -> Result<String> {
        match default.filter(|value| !value.is_empty()) {
            Some(default) => write!(self.writer, "{label} [{default}]: "),
            None => write!(self.writer, "{label}: "),
        }
        .and_then(|_| self.writer.flush())
        .map_err(|source| SessionBridgeError::Io {
            path: PathBuf::from("<stdout>"),
            source,
        })?;
        let mut value = String::new();
        let bytes = self
            .reader
            .read_line(&mut value)
            .map_err(|source| SessionBridgeError::Io {
                path: PathBuf::from("<stdin>"),
                source,
            })?;
        if bytes == 0 {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "interactive launch ended before confirmation",
            ));
        }
        Ok(value.trim().to_owned())
    }

    fn confirm(&mut self, preflight: &crate::app::launch::LaunchPreflight) -> Result<bool> {
        writeln!(self.writer, "\nLaunch summary")
            .and_then(|_| writeln!(self.writer, "  Title: {}", preflight.title))
            .and_then(|_| writeln!(self.writer, "  Agents: {}", preflight.agents))
            .and_then(|_| {
                writeln!(
                    self.writer,
                    "  Invitation expiry: {}s",
                    preflight.expires_in_seconds
                )
            })
            .and_then(|_| writeln!(self.writer, "  Autonomy: {}", preflight.autonomy_policy))
            .and_then(|_| writeln!(self.writer, "  Target branch: {}", preflight.target_branch))
            .and_then(|_| writeln!(self.writer, "  Baseline: {}", preflight.baseline_commit))
            .and_then(|_| write!(self.writer, "Publish task and create invitations? [y/N]: "))
            .and_then(|_| self.writer.flush())
            .map_err(|source| SessionBridgeError::Io {
                path: PathBuf::from("<stdout>"),
                source,
            })?;
        let mut answer = String::new();
        let bytes =
            self.reader
                .read_line(&mut answer)
                .map_err(|source| SessionBridgeError::Io {
                    path: PathBuf::from("<stdin>"),
                    source,
                })?;
        Ok(bytes > 0 && matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
    }
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(std::env::current_dir()
        .map_err(|source| SessionBridgeError::Io {
            path: PathBuf::from("."),
            source,
        })?
        .join(path))
}

fn launch_human(result: &crate::app::launch::LaunchResult) -> String {
    let mut output = format!(
        "Task published\n  ID: {}\n  Title: {}\n  Launch: {}\n  Baseline: {}\n\nAgent prompts\n",
        result.task.task.id, result.task.task.title, result.launch_id, result.baseline_commit,
    );
    for command in &result.prompt_commands {
        output.push_str(&format!(
            "  Agent {}: {}\n",
            command.batch_index,
            command.argv.join(" ")
        ));
    }
    output.push_str(&format!("\nWatch\n  {}\n", result.watch_argv.join(" ")));
    for warning in &result.warnings {
        output.push_str(&format!("\nWarning: {warning}\n"));
    }
    output
}

fn prompt_list_human(prompts: &[crate::app::invite::InvitationManifest]) -> String {
    if prompts.is_empty() {
        return "No invitation prompts found.\n".into();
    }
    prompts
        .iter()
        .map(|prompt| {
            format!(
                "Batch {} index {} [{}] sendable={}\n  sessionbridge prompt show {} --batch {} --index {}",
                prompt.batch_id,
                prompt.batch_index,
                prompt.status,
                prompt.sendable,
                prompt.task_id,
                prompt.batch_id,
                prompt.batch_index,
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn read_text_source(inline: Option<String>, path: Option<PathBuf>) -> Result<String> {
    match (inline, path) {
        (Some(value), None) if !value.trim().is_empty() => Ok(value),
        (None, Some(path)) => read_nonempty_file(&path),
        _ => Err(SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            "exactly one non-empty objective source is required",
        )),
    }
}

fn read_nonempty_file(path: &Path) -> Result<String> {
    let value = fs::read_to_string(path).map_err(|source| SessionBridgeError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if value.trim().is_empty() {
        return Err(SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            format!("input file is empty: {}", path.display()),
        ));
    }
    Ok(value.trim().to_owned())
}

fn read_json_file<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let value = fs::read(path).map_err(|source| SessionBridgeError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_slice(&value).map_err(Into::into)
}

fn read_json_input<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    if path == Path::new("-") {
        let mut value = Vec::new();
        std::io::stdin()
            .lock()
            .read_to_end(&mut value)
            .map_err(|source| SessionBridgeError::Io {
                path: PathBuf::from("<stdin>"),
                source,
            })?;
        return serde_json::from_slice(&value).map_err(Into::into);
    }
    read_json_file(path)
}

fn read_json_with_request_id<T: serde::de::DeserializeOwned>(
    path: &Path,
    request_id: String,
) -> Result<T> {
    let mut value: serde_json::Value = read_json_input(path)?;
    let object = value.as_object_mut().ok_or_else(|| {
        SessionBridgeError::domain(ErrorCode::InvalidInput, "JSON input must be an object")
    })?;
    object.insert("request_id".into(), serde_json::Value::String(request_id));
    serde_json::from_value(value).map_err(Into::into)
}

fn read_optional_json_array<T: serde::de::DeserializeOwned>(path: Option<&Path>) -> Result<Vec<T>> {
    match path {
        Some(path) => read_json_input(path),
        None => Ok(Vec::new()),
    }
}

fn read_optional_json_value(path: Option<&Path>) -> Result<Option<serde_json::Value>> {
    path.map(read_json_input).transpose()
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum EvidenceFile {
    Array(Vec<EvidenceInput>),
    Object { evidence: Vec<EvidenceInput> },
}

fn read_evidence_file(path: &Path) -> Result<Vec<EvidenceInput>> {
    match read_json_input(path)? {
        EvidenceFile::Array(values) => Ok(values),
        EvidenceFile::Object { evidence } => Ok(evidence),
    }
}

fn apply_plan_protocol(
    globals: &Globals,
    request_id_field: &mut String,
    base_plan_version: &mut i64,
) -> Result<()> {
    if let Some(request_id) = &globals.request_id {
        request_id_field.clone_from(request_id);
    } else if request_id_field.is_empty() {
        *request_id_field = Uuid::now_v7().to_string();
    }
    if let Some(expected) = globals.expected_plan_version {
        *base_plan_version = i64::try_from(expected).map_err(|_| {
            SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "expected plan version exceeds the supported range",
            )
        })?;
    }
    Ok(())
}

fn read_optional_list(path: Option<&Path>) -> Result<Vec<String>> {
    let Some(path) = path else {
        return Ok(Vec::new());
    };
    let value = read_nonempty_file(path)?;
    if value.trim_start().starts_with('[') {
        let values: Vec<String> = serde_json::from_str(&value)?;
        return normalize_list(values);
    }

    let nonempty_lines: Vec<&str> = value
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let all_bullets = !nonempty_lines.is_empty()
        && nonempty_lines.iter().all(|line| {
            let line = line.trim_start();
            line.starts_with("- ")
                || line.starts_with("* ")
                || line
                    .split_once(". ")
                    .is_some_and(|(number, _)| number.chars().all(|value| value.is_ascii_digit()))
        });
    let values = if all_bullets {
        nonempty_lines
            .into_iter()
            .map(|line| strip_list_prefix(line).to_owned())
            .collect()
    } else {
        value
            .split("\n\n")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect()
    };
    normalize_list(values)
}

fn strip_list_prefix(value: &str) -> &str {
    let value = value.trim();
    if let Some(value) = value
        .strip_prefix("- ")
        .or_else(|| value.strip_prefix("* "))
    {
        return value.trim();
    }
    value
        .split_once(". ")
        .map(|(_, value)| value.trim())
        .unwrap_or(value)
}

fn normalize_list(values: Vec<String>) -> Result<Vec<String>> {
    let mut normalized = Vec::new();
    for value in values {
        let value = value.trim();
        if value.is_empty() {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "list inputs cannot contain empty values",
            ));
        }
        if !normalized.iter().any(|existing| existing == value) {
            normalized.push(value.to_owned());
        }
    }
    Ok(normalized)
}

fn parse_time_duration(value: &str) -> Result<Duration> {
    let duration = parse_compact_duration(value)?;
    let seconds = i64::try_from(duration.as_secs()).map_err(|_| {
        SessionBridgeError::domain(ErrorCode::InvalidInput, "duration is too large")
    })?;
    Ok(Duration::seconds(seconds))
}

fn parse_duration_seconds(value: &str) -> Result<i64> {
    Ok(parse_time_duration(value)?.whole_seconds())
}

fn parse_poll_interval(value: &str) -> Result<StdDuration> {
    if let Some(milliseconds) = value.strip_suffix("ms") {
        let milliseconds = milliseconds.parse::<u64>().map_err(|error| {
            SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                format!("invalid millisecond interval: {error}"),
            )
        })?;
        if milliseconds == 0 {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "poll interval must be greater than zero",
            ));
        }
        return Ok(StdDuration::from_millis(milliseconds));
    }
    parse_compact_duration(value).map_err(Into::into)
}

fn request_id(globals: &Globals) -> String {
    globals
        .request_id
        .clone()
        .unwrap_or_else(|| Uuid::now_v7().to_string())
}

fn required_global_i64(value: Option<u64>, flag: &str) -> Result<i64> {
    value
        .ok_or_else(|| {
            SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                format!("agent conclude requires {flag}"),
            )
        })
        .and_then(|value| to_i64(value, flag))
}

fn to_i64(value: u64, name: &str) -> Result<i64> {
    i64::try_from(value).map_err(|_| {
        SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            format!("{name} exceeds the supported range"),
        )
    })
}

fn parse_id<T: std::str::FromStr>(value: &str, name: &str) -> Result<T>
where
    T::Err: std::fmt::Display,
{
    value.parse().map_err(|error| {
        SessionBridgeError::domain(ErrorCode::InvalidInput, format!("invalid {name}: {error}"))
    })
}

fn task_human(task: &crate::domain::Task, criterion_count: Option<usize>) -> String {
    format!(
        "Task {} [{}]\n{}\nPlan version: {}\nCompletion epoch/revision: {}/{}{}\n",
        task.id,
        task.phase,
        task.title,
        task.plan_version,
        task.completion_epoch,
        task.completion_revision,
        criterion_count
            .map(|count| format!("\nCriteria: {count}"))
            .unwrap_or_default()
    )
}

fn human_json<T: Serialize>(value: &T) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| "<serialization failed>".to_owned())
}

fn invitation_state_arg(value: crate::cli::InvitationStateArg) -> &'static str {
    use crate::cli::InvitationStateArg;
    match value {
        InvitationStateArg::Prepared => "prepared",
        InvitationStateArg::Pending => "pending",
        InvitationStateArg::Reserved => "reserved",
        InvitationStateArg::Consumed => "consumed",
        InvitationStateArg::Revoked => "revoked",
        InvitationStateArg::Expired => "expired",
        InvitationStateArg::Abandoned => "abandoned",
    }
}

fn participant_state_arg(value: crate::cli::ParticipantStateArg) -> &'static str {
    match value {
        crate::cli::ParticipantStateArg::Active => "active",
        crate::cli::ParticipantStateArg::Waiting => "waiting",
        crate::cli::ParticipantStateArg::Concluded => "concluded",
        crate::cli::ParticipantStateArg::Blocked => "blocked",
        crate::cli::ParticipantStateArg::Departed => "departed",
        crate::cli::ParticipantStateArg::Retired => "retired",
        crate::cli::ParticipantStateArg::Superseded => "superseded",
        crate::cli::ParticipantStateArg::Stale => "stale",
    }
}

fn default_unknown_compatibility() -> String {
    "unknown".into()
}

fn criterion_kind(value: crate::cli::CriterionKindArg) -> CriterionKind {
    match value {
        crate::cli::CriterionKindArg::Objective => CriterionKind::Objective,
        crate::cli::CriterionKindArg::Constraint => CriterionKind::Constraint,
        crate::cli::CriterionKindArg::NonGoal => CriterionKind::NonGoal,
        crate::cli::CriterionKindArg::DoneCondition => CriterionKind::DoneCondition,
    }
}

fn evaluation_mode(value: crate::cli::EvaluationModeArg) -> EvaluationMode {
    match value {
        crate::cli::EvaluationModeArg::PositiveEvidence => EvaluationMode::PositiveEvidence,
        crate::cli::EvaluationModeArg::ContinuousGuard => EvaluationMode::ContinuousGuard,
    }
}

fn default_evaluation_mode(kind: CriterionKind) -> EvaluationMode {
    match kind {
        CriterionKind::Objective | CriterionKind::DoneCondition => EvaluationMode::PositiveEvidence,
        CriterionKind::Constraint | CriterionKind::NonGoal => EvaluationMode::ContinuousGuard,
    }
}

#[cfg(test)]
mod tests {
    use std::{io::Cursor, process::Command};

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn worker_invitations_require_git_with_an_initial_commit() {
        let project = TempDir::new().unwrap();
        let state = TempDir::new().unwrap();
        let directory =
            crate::workspace::initialize(project.path(), "demo", Some(state.path())).unwrap();

        assert!(
            !worker_invitation_provisioning(&directory, crate::domain::ParticipantKind::Observer,)
                .unwrap()
        );
        let error =
            worker_invitation_provisioning(&directory, crate::domain::ParticipantKind::Worker)
                .unwrap_err();
        assert_eq!(error.code(), ErrorCode::WorktreeConflict);

        assert!(
            Command::new("git")
                .arg("-C")
                .arg(project.path())
                .arg("init")
                .status()
                .unwrap()
                .success()
        );
        let git_workspace = crate::workspace::LocatedWorkspace::discover_with_state_root(
            project.path(),
            state.path(),
        )
        .unwrap();
        let error =
            worker_invitation_provisioning(&git_workspace, crate::domain::ParticipantKind::Worker)
                .unwrap_err();
        assert_eq!(error.code(), ErrorCode::WorktreeConflict);
        assert_eq!(
            error.details()["recovery"],
            "commit the project baseline before creating worker invitations"
        );
    }

    #[test]
    fn launch_prompter_is_driven_by_injected_io() {
        let input = Cursor::new(b"\ncustom\ny\n".to_vec());
        let mut output = Vec::new();
        {
            let mut prompter = LaunchPrompter::new(input, &mut output);
            assert_eq!(prompter.optional("Agents", Some("4")).unwrap(), "");
            assert_eq!(prompter.optional("Title", None).unwrap(), "custom");
            assert!(
                prompter
                    .confirm(&crate::app::launch::LaunchPreflight {
                        title: "demo".into(),
                        objective_sha256: "digest".into(),
                        agents: 4,
                        expires_in_seconds: 7200,
                        autonomy_policy: crate::domain::AutonomyPolicy::Bounded,
                        target_branch: "main".into(),
                        baseline_commit: "abc".into(),
                        dirty_paths: Vec::new(),
                        prompt_root: PathBuf::from("/prompts"),
                        ready: true,
                        warnings: Vec::new(),
                    })
                    .unwrap()
            );
        }
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("Agents [4]:"));
        assert!(output.contains("Launch summary"));
        assert!(output.contains("[y/N]"));
    }
}
