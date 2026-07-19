use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::{ConclusionId, InvitationId, SessionId, TaskId, WorkspaceId};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum AutonomyPolicy {
    Manual,
    Proposal,
    #[default]
    Bounded,
    Open,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskPhase {
    Active,
    Paused,
    Completed,
    Cancelled,
    Archived,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionPolicy {
    #[default]
    AllWorkersConcluded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CriterionKind {
    Objective,
    Constraint,
    NonGoal,
    DoneCondition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationMode {
    PositiveEvidence,
    ContinuousGuard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CriterionSource {
    User,
    System,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum ParticipantKind {
    #[default]
    Worker,
    Observer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParticipantState {
    Active,
    Waiting,
    Concluded,
    Blocked,
    Departed,
    Retired,
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvitationState {
    Prepared,
    Pending,
    Reserved,
    Consumed,
    Revoked,
    Expired,
    Abandoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum ConclusionOutcome {
    NoFurtherWork,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum ClaimMode {
    Execute,
    Review,
    Integrate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssuranceFlag {
    SingleAgentAssurance,
    CompletedWithWarnings,
    RetiredParticipantAssurance,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub name: String,
    pub repository_identity: String,
    #[serde(with = "super::time_serde")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub workspace_id: WorkspaceId,
    pub title: String,
    pub phase: TaskPhase,
    pub autonomy_policy: AutonomyPolicy,
    pub completion_policy: CompletionPolicy,
    pub target_branch: Option<String>,
    pub plan_version: i64,
    pub completion_epoch: i64,
    pub completion_revision: i64,
    pub version: i64,
    #[serde(with = "super::time_serde")]
    pub created_at: OffsetDateTime,
    #[serde(with = "super::time_serde")]
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCriterion {
    pub id: super::CriterionId,
    pub task_id: TaskId,
    pub kind: CriterionKind,
    pub content: String,
    pub required: bool,
    pub source: CriterionSource,
    pub evaluation_mode: EvaluationMode,
    pub immutable_to_agents: bool,
    pub version: i64,
    #[serde(with = "super::time_serde")]
    pub created_at: OffsetDateTime,
    #[serde(with = "super::time_serde")]
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvitationSummary {
    pub id: InvitationId,
    pub task_id: TaskId,
    pub state: InvitationState,
    pub participant_kind: ParticipantKind,
    pub blocks_completion: bool,
    #[serde(with = "super::time_serde")]
    pub expires_at: OffsetDateTime,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SessionContext {
    pub schema_version: u32,
    pub protocol_version: u32,
    pub workspace_id: WorkspaceId,
    pub repository_instance_id: String,
    pub task_id: TaskId,
    pub session_id: SessionId,
    pub token: String,
    pub database_path: String,
}

impl std::fmt::Debug for SessionContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionContext")
            .field("schema_version", &self.schema_version)
            .field("protocol_version", &self.protocol_version)
            .field("workspace_id", &self.workspace_id)
            .field("repository_instance_id", &self.repository_instance_id)
            .field("task_id", &self.task_id)
            .field("session_id", &self.session_id)
            .field("token", &"[REDACTED]")
            .field("database_path", &self.database_path)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionAssessment {
    pub summary: String,
    #[serde(default)]
    pub checked: Vec<String>,
    #[serde(default)]
    pub open_signals: Vec<OpenSignalDisposition>,
    #[serde(default)]
    pub remaining_risks: Vec<String>,
    pub expected_future_work: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenSignalDisposition {
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
    pub disposition: String,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParticipantView {
    pub session_id: SessionId,
    pub kind: ParticipantKind,
    pub state: ParticipantState,
    pub completion_epoch: i64,
    pub version: i64,
    pub current_conclusion_id: Option<ConclusionId>,
}

macro_rules! string_enum {
    ($name:ty { $($variant:path => $value:literal),+ $(,)? }) => {
        impl $name {
            pub const fn as_str(self) -> &'static str {
                match self {
                    $($variant => $value),+
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl std::str::FromStr for $name {
            type Err = String;

            fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
                match value {
                    $($value => Ok($variant)),+,
                    _ => Err(format!("unknown {} value: {value}", stringify!($name))),
                }
            }
        }
    };
}

string_enum!(AutonomyPolicy {
    AutonomyPolicy::Manual => "manual",
    AutonomyPolicy::Proposal => "proposal",
    AutonomyPolicy::Bounded => "bounded",
    AutonomyPolicy::Open => "open",
});
string_enum!(TaskPhase {
    TaskPhase::Active => "active",
    TaskPhase::Paused => "paused",
    TaskPhase::Completed => "completed",
    TaskPhase::Cancelled => "cancelled",
    TaskPhase::Archived => "archived",
});
string_enum!(CompletionPolicy {
    CompletionPolicy::AllWorkersConcluded => "all_workers_concluded",
});
string_enum!(CriterionKind {
    CriterionKind::Objective => "objective",
    CriterionKind::Constraint => "constraint",
    CriterionKind::NonGoal => "non_goal",
    CriterionKind::DoneCondition => "done_condition",
});
string_enum!(EvaluationMode {
    EvaluationMode::PositiveEvidence => "positive_evidence",
    EvaluationMode::ContinuousGuard => "continuous_guard",
});
string_enum!(CriterionSource {
    CriterionSource::User => "user",
    CriterionSource::System => "system",
});
string_enum!(ParticipantKind {
    ParticipantKind::Worker => "worker",
    ParticipantKind::Observer => "observer",
});
string_enum!(ParticipantState {
    ParticipantState::Active => "active",
    ParticipantState::Waiting => "waiting",
    ParticipantState::Concluded => "concluded",
    ParticipantState::Blocked => "blocked",
    ParticipantState::Departed => "departed",
    ParticipantState::Retired => "retired",
    ParticipantState::Superseded => "superseded",
});
string_enum!(InvitationState {
    InvitationState::Prepared => "prepared",
    InvitationState::Pending => "pending",
    InvitationState::Reserved => "reserved",
    InvitationState::Consumed => "consumed",
    InvitationState::Revoked => "revoked",
    InvitationState::Expired => "expired",
    InvitationState::Abandoned => "abandoned",
});
string_enum!(ConclusionOutcome {
    ConclusionOutcome::NoFurtherWork => "no_further_work",
    ConclusionOutcome::Blocked => "blocked",
});
string_enum!(ClaimMode {
    ClaimMode::Execute => "execute",
    ClaimMode::Review => "review",
    ClaimMode::Integrate => "integrate",
});
string_enum!(AssuranceFlag {
    AssuranceFlag::SingleAgentAssurance => "single_agent_assurance",
    AssuranceFlag::CompletedWithWarnings => "completed_with_warnings",
    AssuranceFlag::RetiredParticipantAssurance => "retired_participant_assurance",
});
