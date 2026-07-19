use std::path::PathBuf;

use clap::{ArgAction, ArgGroup, Args, Parser, Subcommand, ValueEnum};

use crate::{
    Result,
    domain::{AutonomyPolicy, ClaimMode, ConclusionOutcome, ParticipantKind},
    output::OutputFormat,
};

/// SessionBridge's stable command-line protocol.
#[derive(Debug, Parser)]
#[command(name = "sessionbridge", version, about)]
pub struct Cli {
    #[arg(long, global = true, value_enum, default_value = "table")]
    pub format: OutputFormat,
    #[arg(long, global = true, env = "SESSIONBRIDGE_CONTEXT")]
    pub context: Option<PathBuf>,
    #[arg(long, global = true, env = "SESSIONBRIDGE_WORKSPACE")]
    pub workspace: Option<PathBuf>,
    #[arg(long, global = true)]
    pub non_interactive: bool,
    #[arg(long, global = true)]
    pub request_id: Option<String>,
    #[arg(long, global = true)]
    pub expected_version: Option<u64>,
    #[arg(long, global = true)]
    pub expected_plan_version: Option<u64>,
    #[arg(long, global = true)]
    pub observed_plan_version: Option<u64>,
    #[arg(long, global = true)]
    pub no_color: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Init(InitArgs),
    Gui(GuiArgs),
    Launch(LaunchArgs),
    Prompt(PromptArgs),
    Status(TaskShortcutArgs),
    Watch(WatchShortcutArgs),
    Config(ConfigArgs),
    Workspace(WorkspaceArgs),
    Task(TaskArgs),
    Agent(AgentArgs),
    Capability(CapabilityArgs),
    Participant(ParticipantArgs),
    Requirement(RequirementArgs),
    Gap(GapArgs),
    Blocker(BlockerArgs),
    Item(ItemArgs),
    Work(WorkArgs),
    Claim(ClaimArgs),
    Attempt(AttemptArgs),
    Artifact(ArtifactArgs),
    Evidence(EvidenceArgs),
    Plan(PlanArgs),
    Approval(ApprovalArgs),
    Review(ReviewArgs),
    Changeset(ChangesetArgs),
    Contract(ContractArgs),
    Decision(DecisionArgs),
    Deliverable(DeliverableArgs),
    Coord(CoordArgs),
    Invalidation(InvalidationArgs),
    Logs(LogsArgs),
    Diagnostics(DiagnosticsArgs),
    Mcp(McpArgs),
    Daemon(DaemonArgs),
    Migrate,
    Doctor,
}

#[derive(Debug, Args)]
pub struct GuiArgs {
    /// Keep the GUI attached to this terminal and return its exit status.
    #[arg(long)]
    pub foreground: bool,
}

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct LaunchArgs {
    #[command(subcommand)]
    pub command: Option<LaunchCommand>,
    #[arg(value_name = "BRIEF_TEXT", conflicts_with = "brief")]
    pub brief_text: Option<String>,
    #[arg(long)]
    pub brief: Option<PathBuf>,
    #[arg(long)]
    pub title: Option<String>,
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..))]
    pub agents: Option<u16>,
    #[arg(long)]
    pub expires_in: Option<String>,
    #[arg(long, value_enum)]
    pub autonomy: Option<AutonomyPolicy>,
    #[arg(long)]
    pub target_branch: Option<String>,
    #[arg(long)]
    pub capabilities_file: Option<PathBuf>,
    #[arg(long)]
    pub prompt_dir: Option<PathBuf>,
    #[arg(long, value_enum)]
    pub shell: Option<PromptShell>,
    #[arg(long)]
    pub profile: Option<String>,
    #[arg(long)]
    pub allow_dirty_baseline: bool,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub yes: bool,
}

#[derive(Debug, Subcommand)]
pub enum LaunchCommand {
    Resume(LaunchResumeArgs),
}

#[derive(Debug, Args)]
pub struct LaunchResumeArgs {
    pub launch: String,
    #[arg(long)]
    pub accept_baseline_change: bool,
}

#[derive(Debug, Args)]
pub struct PromptArgs {
    #[command(subcommand)]
    pub command: PromptCommand,
}

#[derive(Debug, Subcommand)]
pub enum PromptCommand {
    List(PromptListArgs),
    Show(PromptShowArgs),
}

#[derive(Debug, Args)]
pub struct PromptListArgs {
    pub task: Option<String>,
    #[arg(long)]
    pub all_batches: bool,
}

#[derive(Debug, Args)]
pub struct PromptShowArgs {
    pub task: Option<String>,
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..))]
    pub index: u16,
    #[arg(long)]
    pub batch: Option<String>,
    #[arg(long)]
    pub audit: bool,
}

#[derive(Debug, Args)]
pub struct TaskShortcutArgs {
    pub task: Option<String>,
}

#[derive(Debug, Args)]
pub struct WatchShortcutArgs {
    pub task: Option<String>,
    #[arg(long)]
    pub since: Option<u64>,
    #[arg(long, default_value = "500ms")]
    pub poll_interval: String,
    #[arg(long)]
    pub once: bool,
}

#[derive(Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    Init,
    Show,
    Validate,
}

#[derive(Debug, Args)]
pub struct LogsArgs {
    #[command(subcommand)]
    pub command: LogsCommand,
}

#[derive(Debug, Args)]
pub struct DiagnosticsArgs {
    #[command(subcommand)]
    pub command: DiagnosticsCommand,
}

#[derive(Debug, Subcommand)]
pub enum DiagnosticsCommand {
    Collaboration(TaskReferenceArgs),
}

#[derive(Debug, Args)]
pub struct TaskReferenceArgs {
    pub task: String,
}

#[derive(Debug, Args)]
pub struct McpArgs {
    #[command(subcommand)]
    pub command: McpCommand,
}

#[derive(Debug, Subcommand)]
pub enum McpCommand {
    Serve,
}

#[derive(Debug, Args)]
pub struct DaemonArgs {
    #[arg(long)]
    pub once: bool,
    #[arg(long, default_value = "1s")]
    pub interval: String,
    #[arg(long, default_value_t = 100)]
    pub limit: usize,
}

#[derive(Debug, Subcommand)]
pub enum LogsCommand {
    Path,
    Tail(LogsTailArgs),
}

#[derive(Debug, Args)]
pub struct LogsTailArgs {
    #[arg(long, default_value_t = 200)]
    pub lines: usize,
}

#[derive(Debug, Args)]
pub struct InitArgs {
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long, default_value = ".")]
    pub path: PathBuf,
}

#[derive(Debug, Args)]
pub struct WorkspaceArgs {
    #[command(subcommand)]
    pub command: WorkspaceCommand,
}

#[derive(Debug, Subcommand)]
pub enum WorkspaceCommand {
    Link(WorkspaceLinkArgs),
}

#[derive(Debug, Args)]
pub struct WorkspaceLinkArgs {
    #[arg(long)]
    pub source: PathBuf,
    #[arg(long, default_value = ".")]
    pub path: PathBuf,
}

#[derive(Debug, Args)]
pub struct TaskArgs {
    #[command(subcommand)]
    pub command: TaskCommand,
}

#[derive(Debug, Subcommand)]
pub enum TaskCommand {
    Publish(TaskPublishArgs),
    Show(TaskReadArgs),
    Watch(TaskWatchArgs),
    Health(TaskReadArgs),
    Reconcile(TaskReadArgs),
    Integrity(TaskReadArgs),
    Criterion(TaskCriterionArgs),
    Pause(TaskLifecycleArgs),
    Resume(TaskLifecycleArgs),
    Cancel(TaskLifecycleArgs),
    Reopen(TaskLifecycleArgs),
    Archive(TaskLifecycleArgs),
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("objective-source")
        .required(true)
        .multiple(false)
        .args(["objective", "objective_file"])
))]
pub struct TaskPublishArgs {
    #[arg(long)]
    pub title: String,
    #[arg(long)]
    pub objective: Option<String>,
    #[arg(long)]
    pub objective_file: Option<PathBuf>,
    #[arg(long)]
    pub constraints_file: Option<PathBuf>,
    #[arg(long)]
    pub non_goals_file: Option<PathBuf>,
    #[arg(long)]
    pub done_file: Option<PathBuf>,
    #[arg(long, value_enum, default_value = "bounded")]
    pub autonomy: AutonomyPolicy,
    #[arg(long)]
    pub target_branch: Option<String>,
}

#[derive(Debug, Args)]
pub struct TaskReadArgs {
    pub task: String,
}

#[derive(Debug, Args)]
pub struct TaskWatchArgs {
    pub task: String,
    #[arg(long)]
    pub since: Option<u64>,
    #[arg(long, default_value = "500ms")]
    pub poll_interval: String,
    #[arg(long)]
    pub once: bool,
}

#[derive(Debug, Args)]
pub struct TaskCriterionArgs {
    #[command(subcommand)]
    pub command: TaskCriterionCommand,
}

#[derive(Debug, Subcommand)]
pub enum TaskCriterionCommand {
    Add(TaskCriterionAddArgs),
    Edit(TaskCriterionEditArgs),
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("criterion-content")
        .required(true)
        .multiple(false)
        .args(["content", "content_file"])
))]
pub struct TaskCriterionAddArgs {
    pub task: String,
    #[arg(long, value_enum)]
    pub kind: CriterionKindArg,
    #[arg(long)]
    pub content: Option<String>,
    #[arg(long)]
    pub content_file: Option<PathBuf>,
    #[arg(long, value_enum)]
    pub evaluation_mode: Option<EvaluationModeArg>,
    #[arg(long, default_value_t = true, action = ArgAction::Set)]
    pub required: bool,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("criterion-content")
        .required(true)
        .multiple(false)
        .args(["content", "content_file"])
))]
pub struct TaskCriterionEditArgs {
    pub task: String,
    pub criterion: String,
    #[arg(long)]
    pub content: Option<String>,
    #[arg(long)]
    pub content_file: Option<PathBuf>,
    #[arg(long, value_enum)]
    pub evaluation_mode: Option<EvaluationModeArg>,
    #[arg(long)]
    pub reason: String,
}

#[derive(Debug, Args)]
pub struct TaskLifecycleArgs {
    pub task: String,
    #[arg(long)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CriterionKindArg {
    Objective,
    Constraint,
    NonGoal,
    DoneCondition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum EvaluationModeArg {
    PositiveEvidence,
    ContinuousGuard,
}

#[derive(Debug, Args)]
pub struct AgentArgs {
    #[command(subcommand)]
    pub command: AgentCommand,
}

#[derive(Debug, Subcommand)]
pub enum AgentCommand {
    Invite(AgentInviteArgs),
    Join(AgentJoinArgs),
    Whoami(AgentWhoamiArgs),
    Sync(AgentSyncArgs),
    Next(AgentNextArgs),
    Operations(AgentOperationsArgs),
    ProposeAction(AgentProposeActionArgs),
    CompletionChallenge(AgentProposalInputArgs),
    RequestCapability(AgentProposalInputArgs),
    Proposal(AgentProposalArgs),
    Chat(AgentChatArgs),
    Consolidate(AgentConsolidateArgs),
    Candidates(AgentCandidatesArgs),
    Inspect(AgentInspectArgs),
    Inbox(AgentInboxArgs),
    Await(AgentAwaitArgs),
    Intent(AgentIntentArgs),
    Conclude(AgentConcludeArgs),
    Resume(AgentResumeArgs),
    Leave(AgentLeaveArgs),
    List(AgentListArgs),
}

#[derive(Debug, Args)]
pub struct AgentConsolidateArgs {
    #[command(subcommand)]
    pub command: AgentConsolidateCommand,
}
#[derive(Debug, Subcommand)]
pub enum AgentConsolidateCommand {
    Claim(IdArgs),
    Resolve(AgentConsolidateResolveArgs),
}
#[derive(Debug, Args)]
pub struct AgentConsolidateResolveArgs {
    pub id: String,
    #[arg(long)]
    pub input: PathBuf,
}

#[derive(Debug, Args)]
pub struct AgentInviteArgs {
    #[command(subcommand)]
    pub command: AgentInviteCommand,
}

#[derive(Debug, Subcommand)]
pub enum AgentInviteCommand {
    Create(InviteCreateArgs),
    List(InviteListArgs),
    Show(InviteShowArgs),
    Revoke(InviteRevokeArgs),
    Rotate(InviteRotateArgs),
}

#[derive(Debug, Args)]
pub struct InviteCreateArgs {
    #[arg(long)]
    pub task: String,
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u16).range(1..))]
    pub count: u16,
    #[arg(long, default_value = "1h")]
    pub expires_in: String,
    #[arg(long)]
    pub prompt_dir: Option<PathBuf>,
    #[arg(long, value_enum, default_value = "worker")]
    pub participant_kind: ParticipantKind,
    #[arg(long)]
    pub capabilities_file: Option<PathBuf>,
    #[arg(long, value_enum, default_value = "file")]
    pub secret_delivery: SecretDelivery,
    #[arg(long, required_if_eq("secret_delivery", "inline"))]
    pub allow_secret_output: bool,
    #[arg(long, value_enum, default_value = "posix")]
    pub shell: PromptShell,
}

#[derive(Debug, Args)]
pub struct InviteListArgs {
    #[arg(long)]
    pub task: String,
    #[arg(long, value_enum)]
    pub state: Option<InvitationStateArg>,
}

#[derive(Debug, Args)]
pub struct InviteShowArgs {
    pub invite: String,
    #[arg(long)]
    pub prompt: bool,
    #[arg(long, requires = "prompt")]
    pub audit: bool,
}

#[derive(Debug, Args)]
pub struct InviteRevokeArgs {
    pub invite: String,
    #[arg(long)]
    pub reason: String,
}

#[derive(Debug, Args)]
pub struct InviteRotateArgs {
    pub invite: String,
    #[arg(long)]
    pub expires_in: Option<String>,
    #[arg(long)]
    pub prompt_dir: Option<PathBuf>,
    #[arg(long, value_enum)]
    pub secret_delivery: Option<SecretDelivery>,
    #[arg(long, required_if_eq("secret_delivery", "inline"))]
    pub allow_secret_output: bool,
    #[arg(long, value_enum, default_value = "posix")]
    pub shell: PromptShell,
    #[arg(long)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum SecretDelivery {
    #[default]
    File,
    Inline,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum PromptShell {
    #[default]
    Posix,
    Powershell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum InvitationStateArg {
    Prepared,
    Pending,
    Reserved,
    Consumed,
    Revoked,
    Expired,
    Abandoned,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("invite-source")
        .required(true)
        .multiple(false)
        .args(["invite_file", "invite_stdin"])
))]
pub struct AgentJoinArgs {
    #[arg(long)]
    pub invite_file: Option<PathBuf>,
    #[arg(long)]
    pub invite_stdin: bool,
    #[arg(long, conflicts_with = "worktree")]
    pub provision_worktree: bool,
    #[arg(long)]
    pub worktree: Option<PathBuf>,
    #[arg(long)]
    pub capabilities_file: Option<PathBuf>,
    #[arg(long)]
    pub nickname: Option<String>,
    #[arg(long)]
    pub client_name: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
}

#[derive(Debug, Args, Default)]
pub struct AgentWhoamiArgs {}

#[derive(Debug, Args)]
pub struct AgentSyncArgs {
    #[arg(long, requires_all = ["full", "snapshot_id"])]
    pub since: Option<u64>,
    #[arg(long, requires = "since")]
    pub snapshot_id: Option<String>,
    #[arg(long, required = true)]
    pub full: bool,
}

#[derive(Debug, Args, Default)]
pub struct AgentNextArgs {
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=100))]
    pub limit: Option<u32>,
    #[arg(long)]
    pub preview: bool,
}

#[derive(Debug, Args, Default)]
pub struct AgentOperationsArgs {
    pub operation: Option<String>,
}

#[derive(Debug, Args)]
pub struct AgentProposeActionArgs {
    #[arg(long)]
    pub kind: String,
    #[arg(long)]
    pub input: PathBuf,
}

#[derive(Debug, Args)]
pub struct AgentProposalInputArgs {
    #[arg(long)]
    pub input: PathBuf,
}

#[derive(Debug, Args)]
pub struct AgentProposalArgs {
    #[command(subcommand)]
    pub command: AgentProposalCommand,
}

#[derive(Debug, Args)]
pub struct AgentChatArgs {
    #[command(subcommand)]
    pub command: AgentChatCommand,
}

#[derive(Debug, Subcommand)]
pub enum AgentChatCommand {
    Send(AgentChatSendArgs),
    History(AgentChatHistoryArgs),
    Inbox(AgentChatInboxArgs),
    Ack(AgentChatAckArgs),
    Await(AgentChatAwaitArgs),
}

#[derive(Debug, Args)]
pub struct AgentChatInboxArgs {
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=200))]
    pub limit: u32,
}

#[derive(Debug, Args)]
pub struct AgentChatAckArgs {
    #[arg(long = "message", required = true, num_args = 1..=200)]
    pub messages: Vec<String>,
}

#[derive(Debug, Args)]
pub struct AgentChatAwaitArgs {
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=200))]
    pub limit: u32,
    #[arg(long)]
    pub include_broadcasts: bool,
}

#[derive(Debug, Args)]
pub struct AgentChatSendArgs {
    #[arg(long)]
    pub input: PathBuf,
}

#[derive(Debug, Args)]
pub struct AgentChatHistoryArgs {
    #[arg(long, default_value_t = 0)]
    pub since: u64,
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=200))]
    pub limit: u32,
}

#[derive(Debug, Subcommand)]
pub enum AgentProposalCommand {
    Resolve(AgentProposalResolveArgs),
}

#[derive(Debug, Args)]
pub struct AgentProposalResolveArgs {
    pub proposal: String,
    #[arg(long, value_enum)]
    pub decision: ProposalDecisionArg,
    #[arg(long)]
    pub reason: String,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ProposalDecisionArg {
    Accepted,
    Rejected,
    Superseded,
}

#[derive(Debug, Args)]
pub struct AgentCandidatesArgs {
    #[arg(long)]
    pub explain: bool,
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    pub limit: Option<u32>,
}

#[derive(Debug, Args)]
pub struct AgentInspectArgs {
    pub entity: String,
}

#[derive(Debug, Args)]
pub struct AgentInboxArgs {
    #[arg(long)]
    pub since: Option<u64>,
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    pub limit: Option<u32>,
}

#[derive(Debug, Args)]
pub struct AgentAwaitArgs {
    #[command(subcommand)]
    pub command: AgentAwaitCommand,
}

#[derive(Debug, Subcommand)]
pub enum AgentAwaitCommand {
    WorkItems(AgentAwaitWorkItemsArgs),
    External(AgentAwaitExternalArgs),
    Resume(AgentAwaitIdArgs),
    Show(AgentAwaitIdArgs),
    List,
}

#[derive(Debug, Args)]
pub struct AgentAwaitExternalArgs {
    #[arg(long)]
    pub detector: PathBuf,
    #[arg(long)]
    pub deadline_in: Option<String>,
    #[arg(long, default_value = "30s")]
    pub reassess_every: String,
}

#[derive(Debug, Args)]
pub struct AgentAwaitWorkItemsArgs {
    #[arg(long = "item", required = true)]
    pub items: Vec<String>,
    #[arg(long, value_enum, default_value = "all")]
    pub mode: crate::app::await_service::AwaitMode,
}

#[derive(Debug, Args)]
pub struct AgentAwaitIdArgs {
    pub wait: String,
}

#[derive(Debug, Args)]
pub struct AgentIntentArgs {
    #[command(subcommand)]
    pub command: AgentIntentCommand,
}

#[derive(Debug, Subcommand)]
pub enum AgentIntentCommand {
    Set(JsonInputArgs),
    Clear(AgentIntentClearArgs),
}

#[derive(Debug, Args)]
pub struct AgentIntentClearArgs {
    pub intent: Option<String>,
    #[arg(long)]
    pub reason: Option<String>,
}

#[derive(Debug, Args)]
pub struct AgentConcludeArgs {
    #[arg(long, value_enum)]
    pub outcome: ConclusionOutcome,
    #[arg(long, conflicts_with_all = ["summary", "checked", "risk"])]
    pub assessment_file: Option<PathBuf>,
    #[arg(long, requires = "checked")]
    pub summary: Option<String>,
    #[arg(long, value_name = "AREA")]
    pub checked: Vec<String>,
    #[arg(long, value_name = "RISK")]
    pub risk: Vec<String>,
    #[arg(long)]
    pub completion_token: Option<String>,
    #[arg(long)]
    pub blocker: Vec<String>,
    #[arg(long)]
    pub gap: Vec<String>,
    #[arg(long = "coord-request")]
    pub coordination_request: Vec<String>,
}

#[derive(Debug, Args)]
pub struct AgentResumeArgs {
    #[arg(long)]
    pub reason: Option<String>,
}

#[derive(Debug, Args)]
pub struct AgentLeaveArgs {
    #[arg(long)]
    pub reason: Option<String>,
}

#[derive(Debug, Args)]
pub struct AgentListArgs {
    #[arg(long)]
    pub task: String,
    #[arg(long, value_enum)]
    pub state: Option<ParticipantStateArg>,
}

#[derive(Debug, Args)]
pub struct CapabilityArgs {
    #[command(subcommand)]
    pub command: CapabilityCommand,
}

#[derive(Debug, Subcommand)]
pub enum CapabilityCommand {
    Attest(JsonInputArgs),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ParticipantStateArg {
    Active,
    Waiting,
    Concluded,
    Blocked,
    Departed,
    Retired,
    Superseded,
    Stale,
}

#[derive(Debug, Args)]
pub struct ParticipantArgs {
    #[command(subcommand)]
    pub command: ParticipantCommand,
}

#[derive(Debug, Subcommand)]
pub enum ParticipantCommand {
    List(ParticipantListArgs),
    Retire(ParticipantRetireArgs),
    Retirement(ParticipantRetirementArgs),
}

#[derive(Debug, Args)]
pub struct ParticipantListArgs {
    #[arg(long)]
    pub task: Option<String>,
    #[arg(long, value_enum)]
    pub state: Option<ParticipantStateArg>,
}

#[derive(Debug, Args)]
pub struct ParticipantRetireArgs {
    pub participant: String,
    #[arg(long)]
    pub reason: String,
    #[arg(long)]
    pub evidence_file: Option<PathBuf>,
    #[arg(long)]
    pub handoff_file: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct ParticipantRetirementArgs {
    #[command(subcommand)]
    pub command: ParticipantRetirementCommand,
}

#[derive(Debug, Subcommand)]
pub enum ParticipantRetirementCommand {
    Show(IdArgs),
}

#[derive(Debug, Args)]
pub struct RequirementArgs {
    #[command(subcommand)]
    pub command: RequirementCommand,
}

#[derive(Debug, Subcommand)]
pub enum RequirementCommand {
    Add(JsonInputArgs),
    Supersede(RequirementSupersedeArgs),
}

#[derive(Debug, Args)]
pub struct RequirementSupersedeArgs {
    pub requirement: String,
    #[arg(long)]
    pub replacement: Option<String>,
    #[arg(long)]
    pub reason: String,
    #[arg(long)]
    pub input_json: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct GapArgs {
    #[command(subcommand)]
    pub command: GapCommand,
}

#[derive(Debug, Subcommand)]
pub enum GapCommand {
    Report(JsonInputArgs),
    Resolve(GapResolveArgs),
}

#[derive(Debug, Args)]
pub struct GapResolveArgs {
    pub gap: String,
    #[arg(long)]
    pub reason: Option<String>,
    #[arg(long)]
    pub evidence_file: Option<PathBuf>,
    #[arg(long)]
    pub input_json: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct BlockerArgs {
    #[command(subcommand)]
    pub command: BlockerCommand,
}

#[derive(Debug, Subcommand)]
pub enum BlockerCommand {
    Open(JsonInputArgs),
    Resolve(BlockerResolveArgs),
}

#[derive(Debug, Args)]
pub struct BlockerResolveArgs {
    pub blocker: String,
    #[arg(long)]
    pub resolution: String,
}

#[derive(Debug, Args)]
pub struct ItemArgs {
    #[command(subcommand)]
    pub command: ItemCommand,
}

#[derive(Debug, Args)]
pub struct WorkArgs {
    #[command(subcommand)]
    pub command: WorkCommand,
}

#[derive(Debug, Subcommand)]
pub enum WorkCommand {
    Start(WorkStartArgs),
    Adopt(WorkStartArgs),
    Submit(WorkSubmitArgs),
    Review(WorkReviewArgs),
    Integrate(WorkIntegrateArgs),
    Speculate(WorkSpeculateArgs),
}

#[derive(Debug, Args)]
pub struct WorkSpeculateArgs {
    #[command(subcommand)]
    pub command: WorkSpeculateCommand,
}

#[derive(Debug, Subcommand)]
pub enum WorkSpeculateCommand {
    Start(JsonInputArgs),
    Checkpoint(WorkSpeculateCheckpointArgs),
    Adopt(IdArgs),
    Abandon(WorkSpeculateAbandonArgs),
}

#[derive(Debug, Args)]
pub struct WorkSpeculateCheckpointArgs {
    pub speculation: String,
    #[arg(long)]
    pub input: PathBuf,
}
#[derive(Debug, Args)]
pub struct WorkSpeculateAbandonArgs {
    pub speculation: String,
    #[arg(long)]
    pub reason: String,
}

#[derive(Debug, Args)]
pub struct WorkReviewArgs {
    pub item: String,
    #[arg(long, value_enum)]
    pub decision: ReviewDecisionArg,
    #[arg(long)]
    pub evidence_file: Option<PathBuf>,
    #[arg(long, default_value = "15m")]
    pub lease: String,
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct WorkIntegrateArgs {
    #[command(subcommand)]
    pub command: WorkIntegrateCommand,
}

#[derive(Debug, Subcommand)]
pub enum WorkIntegrateCommand {
    Prepare(WorkIntegratePrepareArgs),
    Confirm(WorkIntegrateConfirmArgs),
}

#[derive(Debug, Args)]
pub struct WorkIntegratePrepareArgs {
    pub item: String,
    #[arg(long, default_value = "15m")]
    pub lease: String,
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct WorkIntegrateConfirmArgs {
    pub preparation: String,
    #[arg(long)]
    pub details_file: Option<PathBuf>,
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct WorkStartArgs {
    pub item: String,
    #[arg(long)]
    pub resources_file: Option<PathBuf>,
    #[arg(long)]
    pub inputs_file: Option<PathBuf>,
    #[arg(long)]
    pub lease: Option<String>,
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct WorkSubmitArgs {
    pub item: String,
    #[arg(long)]
    pub bundle: PathBuf,
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Subcommand)]
pub enum ItemCommand {
    Propose(ItemProposeArgs),
    Split(ItemSplitArgs),
    Relate(ItemRelateArgs),
    Claim(ItemClaimArgs),
    Inspect(IdArgs),
    Submit(ItemSubmitArgs),
    Reopen(ItemReopenArgs),
}

#[derive(Debug, Args)]
pub struct ItemProposeArgs {
    #[arg(long)]
    pub input_json: PathBuf,
    #[arg(long)]
    pub claim: bool,
}

#[derive(Debug, Args)]
pub struct ItemSplitArgs {
    pub item: String,
    #[arg(long)]
    pub input_json: PathBuf,
    #[arg(long)]
    pub claim: Option<String>,
    #[arg(long)]
    pub fence: Option<u64>,
}

#[derive(Debug, Args)]
pub struct ItemRelateArgs {
    pub from: String,
    pub to: String,
    #[arg(long, value_enum)]
    pub relation: WorkRelationKind,
    #[arg(long)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum WorkRelationKind {
    DependsOn,
    Duplicates,
    Supersedes,
}

#[derive(Debug, Args)]
pub struct ItemClaimArgs {
    pub item: String,
    #[arg(long, value_enum, default_value = "execute")]
    pub mode: ClaimMode,
    #[arg(long)]
    pub resources_file: Option<PathBuf>,
    #[arg(long)]
    pub inputs_file: Option<PathBuf>,
    #[arg(long)]
    pub lease: Option<String>,
}

#[derive(Debug, Args)]
pub struct ItemSubmitArgs {
    pub item: String,
    #[arg(long)]
    pub claim: String,
    #[arg(long)]
    pub fence: u64,
    #[arg(long)]
    pub evidence_file: PathBuf,
    #[arg(long)]
    pub changeset_file: PathBuf,
}

#[derive(Debug, Args)]
pub struct ItemReopenArgs {
    pub item: String,
    #[arg(long)]
    pub reason: String,
    #[arg(long)]
    pub evidence_file: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct ClaimArgs {
    #[command(subcommand)]
    pub command: ClaimCommand,
}

#[derive(Debug, Subcommand)]
pub enum ClaimCommand {
    Renew(ClaimRenewArgs),
    Release(ClaimCloseArgs),
    Yield(ClaimCloseArgs),
    Abandon(ClaimCloseArgs),
    Reclaim(ClaimReclaimArgs),
    Scope(ClaimScopeArgs),
}

#[derive(Debug, Args)]
pub struct ClaimRenewArgs {
    pub claim: String,
    #[arg(long)]
    pub fence: u64,
    #[arg(long)]
    pub lease: Option<String>,
}

#[derive(Debug, Args)]
pub struct ClaimCloseArgs {
    pub claim: String,
    #[arg(long)]
    pub fence: u64,
    #[arg(long)]
    pub reason: Option<String>,
    #[arg(long)]
    pub handoff_file: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct ClaimReclaimArgs {
    pub claim: String,
    #[arg(long)]
    pub resources_file: Option<PathBuf>,
    #[arg(long)]
    pub handoff_file: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct ClaimScopeArgs {
    #[command(subcommand)]
    pub command: ClaimScopeCommand,
}

#[derive(Debug, Subcommand)]
pub enum ClaimScopeCommand {
    Expand(ClaimScopeChangeArgs),
    Reduce(ClaimScopeChangeArgs),
    Reconcile(ClaimScopeReconcileArgs),
}

#[derive(Debug, Args)]
pub struct ClaimScopeReconcileArgs {
    pub claim: String,
    #[arg(long)]
    pub fence: u64,
    #[arg(long, required = true)]
    pub from_git: bool,
}

#[derive(Debug, Args)]
pub struct ClaimScopeChangeArgs {
    pub claim: String,
    #[arg(long)]
    pub input_json: PathBuf,
    #[arg(long)]
    pub fence: Option<u64>,
}

#[derive(Debug, Args)]
pub struct AttemptArgs {
    #[command(subcommand)]
    pub command: AttemptCommand,
}

#[derive(Debug, Subcommand)]
pub enum AttemptCommand {
    Checkpoint(AttemptCheckpointArgs),
}

#[derive(Debug, Args)]
pub struct AttemptCheckpointArgs {
    pub attempt: String,
    #[arg(long)]
    pub input_json: PathBuf,
    #[arg(long)]
    pub fence: Option<u64>,
}

#[derive(Debug, Args)]
pub struct ArtifactArgs {
    #[command(subcommand)]
    pub command: ArtifactCommand,
}

#[derive(Debug, Subcommand)]
pub enum ArtifactCommand {
    Add(JsonInputArgs),
}

#[derive(Debug, Args)]
pub struct EvidenceArgs {
    #[command(subcommand)]
    pub command: EvidenceCommand,
}

#[derive(Debug, Subcommand)]
pub enum EvidenceCommand {
    Add(JsonInputArgs),
}

#[derive(Debug, Args)]
pub struct PlanArgs {
    #[command(subcommand)]
    pub command: PlanCommand,
}

#[derive(Debug, Subcommand)]
pub enum PlanCommand {
    Approve(PlanResolutionArgs),
    Reject(PlanResolutionArgs),
}

#[derive(Debug, Args)]
pub struct PlanResolutionArgs {
    pub plan_change: String,
    #[arg(long)]
    pub reason: Option<String>,
}

#[derive(Debug, Args)]
pub struct ApprovalArgs {
    #[command(subcommand)]
    pub command: ApprovalCommand,
}

#[derive(Debug, Subcommand)]
pub enum ApprovalCommand {
    Approve(ApprovalResolutionArgs),
    Reject(ApprovalResolutionArgs),
}

#[derive(Debug, Args)]
pub struct ApprovalResolutionArgs {
    #[arg(long, value_enum)]
    pub target_type: ApprovalTargetTypeArg,
    pub target: String,
    #[arg(long)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ApprovalTargetTypeArg {
    PlanChange,
    Decision,
    ParticipantRetirement,
    TaskReopen,
}

#[derive(Debug, Args)]
pub struct ReviewArgs {
    #[command(subcommand)]
    pub command: ReviewCommand,
}

#[derive(Debug, Subcommand)]
pub enum ReviewCommand {
    Complete(ReviewCompleteArgs),
}

#[derive(Debug, Args)]
pub struct ReviewCompleteArgs {
    pub item: String,
    #[arg(long)]
    pub claim: String,
    #[arg(long)]
    pub fence: u64,
    #[arg(long, value_enum)]
    pub decision: ReviewDecisionArg,
    #[arg(long)]
    pub evidence_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ReviewDecisionArg {
    Approved,
    ChangesRequested,
}

#[derive(Debug, Args)]
pub struct ChangesetArgs {
    #[command(subcommand)]
    pub command: ChangesetCommand,
}

#[derive(Debug, Subcommand)]
pub enum ChangesetCommand {
    Integrate(ChangesetIntegrateArgs),
}

#[derive(Debug, Args)]
pub struct ChangesetIntegrateArgs {
    pub changeset: String,
    #[arg(long)]
    pub claim: String,
    #[arg(long)]
    pub fence: u64,
    #[arg(long, value_enum)]
    pub outcome: IntegrationOutcomeArg,
    #[arg(long)]
    pub details_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum IntegrationOutcomeArg {
    Integrated,
    Failed,
}

#[derive(Debug, Args)]
pub struct ContractArgs {
    #[command(subcommand)]
    pub command: ContractCommand,
}

#[derive(Debug, Subcommand)]
pub enum ContractCommand {
    Publish(JsonInputArgs),
    Revise(ContractReviseArgs),
    Accept(ContractAcceptArgs),
}

#[derive(Debug, Args)]
pub struct ContractReviseArgs {
    pub contract: String,
    #[arg(long)]
    pub input_json: PathBuf,
}

#[derive(Debug, Args)]
pub struct ContractAcceptArgs {
    pub contract: String,
    #[arg(long)]
    pub revision: u64,
    #[arg(long)]
    pub evidence_file: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct DecisionArgs {
    #[command(subcommand)]
    pub command: DecisionCommand,
}

#[derive(Debug, Subcommand)]
pub enum DecisionCommand {
    Propose(JsonInputArgs),
    Approve(DecisionResolutionArgs),
    Reject(DecisionResolutionArgs),
}

#[derive(Debug, Args)]
pub struct DecisionResolutionArgs {
    pub decision: String,
    #[arg(long)]
    pub reason: Option<String>,
    #[arg(long)]
    pub evidence_file: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct CoordArgs {
    #[command(subcommand)]
    pub command: CoordCommand,
}

#[derive(Debug, Subcommand)]
pub enum CoordCommand {
    Request(JsonInputArgs),
    Respond(CoordRespondArgs),
}

#[derive(Debug, Args)]
pub struct CoordRespondArgs {
    pub request: String,
    #[arg(long, value_enum)]
    pub action: CoordinationResponse,
    #[arg(long)]
    pub input_json: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CoordinationResponse {
    Acknowledge,
    Resolve,
    Cancel,
}

#[derive(Debug, Args)]
pub struct InvalidationArgs {
    #[command(subcommand)]
    pub command: InvalidationCommand,
}

#[derive(Debug, Subcommand)]
pub enum InvalidationCommand {
    Resolve(InvalidationResolveArgs),
}

#[derive(Debug, Args)]
pub struct InvalidationResolveArgs {
    pub invalidation: String,
    #[arg(long)]
    pub input_json: Option<PathBuf>,
    #[arg(long)]
    pub reason: Option<String>,
    #[arg(long)]
    pub evidence_file: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct JsonInputArgs {
    #[arg(long)]
    pub input_json: PathBuf,
}

#[derive(Debug, Args)]
pub struct IdArgs {
    pub id: String,
}

#[derive(Debug, Args)]
pub struct DeliverableArgs {
    #[command(subcommand)]
    pub command: DeliverableCommand,
}

#[derive(Debug, Subcommand)]
pub enum DeliverableCommand {
    List {
        task: Option<String>,
    },
    Add {
        task: String,
        #[arg(long)]
        key: String,
        #[arg(long)]
        title: String,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long, default_value = "capability")]
        kind: String,
        #[arg(long, default_value_t = true)]
        required: bool,
    },
    Map {
        task: String,
        deliverable: String,
        item: String,
    },
    Verify {
        task: String,
        deliverable: String,
        #[arg(long)]
        evidence: String,
    },
}

impl Command {
    pub fn log_name(&self) -> &'static str {
        match self {
            Self::Init(_) => "init",
            Self::Gui(_) => "gui",
            Self::Launch(args) => match &args.command {
                Some(LaunchCommand::Resume(_)) => "launch resume",
                None => "launch",
            },
            Self::Prompt(args) => match &args.command {
                PromptCommand::List(_) => "prompt list",
                PromptCommand::Show(_) => "prompt show",
            },
            Self::Status(_) => "status",
            Self::Watch(_) => "watch",
            Self::Config(args) => match &args.command {
                ConfigCommand::Init => "config init",
                ConfigCommand::Show => "config show",
                ConfigCommand::Validate => "config validate",
            },
            Self::Workspace(args) => match &args.command {
                WorkspaceCommand::Link(_) => "workspace link",
            },
            Self::Task(args) => match &args.command {
                TaskCommand::Publish(_) => "task publish",
                TaskCommand::Show(_) => "task show",
                TaskCommand::Watch(_) => "task watch",
                TaskCommand::Health(_) => "task health",
                TaskCommand::Reconcile(_) => "task reconcile",
                TaskCommand::Integrity(_) => "task integrity",
                TaskCommand::Criterion(_) => "task criterion",
                TaskCommand::Pause(_) => "task pause",
                TaskCommand::Resume(_) => "task resume",
                TaskCommand::Cancel(_) => "task cancel",
                TaskCommand::Reopen(_) => "task reopen",
                TaskCommand::Archive(_) => "task archive",
            },
            Self::Agent(args) => match &args.command {
                AgentCommand::Invite(args) => match &args.command {
                    AgentInviteCommand::Create(_) => "agent invite create",
                    AgentInviteCommand::List(_) => "agent invite list",
                    AgentInviteCommand::Show(_) => "agent invite show",
                    AgentInviteCommand::Revoke(_) => "agent invite revoke",
                    AgentInviteCommand::Rotate(_) => "agent invite rotate",
                },
                AgentCommand::Join(_) => "agent join",
                AgentCommand::Whoami(_) => "agent whoami",
                AgentCommand::Sync(_) => "agent sync",
                AgentCommand::Next(_) => "agent next",
                AgentCommand::Operations(_) => "agent operations",
                AgentCommand::ProposeAction(_) => "agent propose-action",
                AgentCommand::CompletionChallenge(_) => "agent completion-challenge",
                AgentCommand::RequestCapability(_) => "agent request-capability",
                AgentCommand::Proposal(args) => match &args.command {
                    AgentProposalCommand::Resolve(_) => "agent proposal resolve",
                },
                AgentCommand::Chat(args) => match &args.command {
                    AgentChatCommand::Send(_) => "agent chat send",
                    AgentChatCommand::History(_) => "agent chat history",
                    AgentChatCommand::Inbox(_) => "agent chat inbox",
                    AgentChatCommand::Ack(_) => "agent chat ack",
                    AgentChatCommand::Await(_) => "agent chat await",
                },
                AgentCommand::Consolidate(args) => match &args.command {
                    AgentConsolidateCommand::Claim(_) => "agent consolidate claim",
                    AgentConsolidateCommand::Resolve(_) => "agent consolidate resolve",
                },
                AgentCommand::Candidates(_) => "agent candidates",
                AgentCommand::Inbox(_) => "agent inbox",
                AgentCommand::Inspect(_) => "agent inspect",
                AgentCommand::Await(args) => match &args.command {
                    AgentAwaitCommand::WorkItems(_) => "agent await work-items",
                    AgentAwaitCommand::External(_) => "agent await external",
                    AgentAwaitCommand::Resume(_) => "agent await resume",
                    AgentAwaitCommand::Show(_) => "agent await show",
                    AgentAwaitCommand::List => "agent await list",
                },
                AgentCommand::Intent(args) => match &args.command {
                    AgentIntentCommand::Set(_) => "agent intent set",
                    AgentIntentCommand::Clear(_) => "agent intent clear",
                },
                AgentCommand::Conclude(_) => "agent conclude",
                AgentCommand::Resume(_) => "agent resume",
                AgentCommand::Leave(_) => "agent leave",
                AgentCommand::List(_) => "agent list",
            },
            Self::Capability(_) => "capability attest",
            Self::Participant(args) => match &args.command {
                ParticipantCommand::List(_) => "participant list",
                ParticipantCommand::Retire(_) => "participant retire",
                ParticipantCommand::Retirement(args) => match &args.command {
                    ParticipantRetirementCommand::Show(_) => "participant retirement show",
                },
            },
            Self::Requirement(_) => "requirement",
            Self::Gap(_) => "gap",
            Self::Blocker(_) => "blocker",
            Self::Item(args) => match &args.command {
                ItemCommand::Propose(_) => "item propose",
                ItemCommand::Split(_) => "item split",
                ItemCommand::Relate(_) => "item relate",
                ItemCommand::Claim(_) => "item claim",
                ItemCommand::Inspect(_) => "item inspect",
                ItemCommand::Submit(_) => "item submit",
                ItemCommand::Reopen(_) => "item reopen",
            },
            Self::Work(args) => match &args.command {
                WorkCommand::Start(_) => "work start",
                WorkCommand::Adopt(_) => "work adopt",
                WorkCommand::Submit(_) => "work submit",
                WorkCommand::Review(_) => "work review",
                WorkCommand::Integrate(args) => match &args.command {
                    WorkIntegrateCommand::Prepare(_) => "work integrate prepare",
                    WorkIntegrateCommand::Confirm(_) => "work integrate confirm",
                },
                WorkCommand::Speculate(args) => match &args.command {
                    WorkSpeculateCommand::Start(_) => "work speculate start",
                    WorkSpeculateCommand::Checkpoint(_) => "work speculate checkpoint",
                    WorkSpeculateCommand::Adopt(_) => "work speculate adopt",
                    WorkSpeculateCommand::Abandon(_) => "work speculate abandon",
                },
            },
            Self::Claim(args) => match &args.command {
                ClaimCommand::Renew(_) => "claim renew",
                ClaimCommand::Release(_) => "claim release",
                ClaimCommand::Yield(_) => "claim yield",
                ClaimCommand::Abandon(_) => "claim abandon",
                ClaimCommand::Reclaim(_) => "claim reclaim",
                ClaimCommand::Scope(args) => match &args.command {
                    ClaimScopeCommand::Expand(_) => "claim scope expand",
                    ClaimScopeCommand::Reduce(_) => "claim scope reduce",
                    ClaimScopeCommand::Reconcile(_) => "claim scope reconcile",
                },
            },
            Self::Attempt(_) => "attempt checkpoint",
            Self::Artifact(_) => "artifact add",
            Self::Evidence(_) => "evidence add",
            Self::Plan(_) => "plan",
            Self::Approval(_) => "approval",
            Self::Review(_) => "review complete",
            Self::Changeset(_) => "changeset integrate",
            Self::Contract(_) => "contract",
            Self::Decision(_) => "decision",
            Self::Deliverable(_) => "deliverable",
            Self::Coord(_) => "coord",
            Self::Invalidation(_) => "invalidation resolve",
            Self::Logs(args) => match &args.command {
                LogsCommand::Path => "logs path",
                LogsCommand::Tail(_) => "logs tail",
            },
            Self::Diagnostics(args) => match &args.command {
                DiagnosticsCommand::Collaboration(_) => "diagnostics collaboration",
            },
            Self::Mcp(_) => "mcp serve",
            Self::Daemon(_) => "daemon",
            Self::Migrate => "migrate",
            Self::Doctor => "doctor",
        }
    }

    pub fn task_reference(&self) -> Option<&str> {
        match self {
            Self::Status(args) => args.task.as_deref(),
            Self::Watch(args) => args.task.as_deref(),
            Self::Task(args) => match &args.command {
                TaskCommand::Show(args)
                | TaskCommand::Health(args)
                | TaskCommand::Reconcile(args)
                | TaskCommand::Integrity(args) => Some(&args.task),
                TaskCommand::Pause(args)
                | TaskCommand::Resume(args)
                | TaskCommand::Cancel(args)
                | TaskCommand::Reopen(args)
                | TaskCommand::Archive(args) => Some(&args.task),
                TaskCommand::Publish(_) | TaskCommand::Watch(_) | TaskCommand::Criterion(_) => None,
            },
            Self::Diagnostics(args) => match &args.command {
                DiagnosticsCommand::Collaboration(args) => Some(&args.task),
            },
            _ => None,
        }
    }
}

pub fn run(cli: Cli) -> Result<()> {
    crate::app::dispatch(cli)
}
