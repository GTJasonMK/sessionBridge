use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::OffsetDateTime;

use super::participant::{ParticipantWorkState, TaskParticipant};
use super::{
    AssuranceFlag, CompletionAssessment, CompletionPolicy, ConclusionId, ConclusionOutcome,
    InvitationId, InvitationState, ParticipantId, ParticipantKind, ParticipantState, TaskId,
    TaskPhase,
};

const ASSESSMENT_SCHEMA_VERSION: u32 = 1;
const CONCLUSION_DIGEST_DOMAIN: &[u8] = b"sessionbridge:completion-conclusions:v1\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionBasis {
    pub task_id: TaskId,
    pub completion_epoch: i64,
    pub completion_revision: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionHead {
    task_id: TaskId,
    completion_epoch: i64,
    completion_revision: i64,
}

impl CompletionHead {
    pub fn try_new(
        task_id: TaskId,
        completion_epoch: i64,
        completion_revision: i64,
    ) -> Result<Self, CompletionCasError> {
        if completion_epoch <= 0 {
            return Err(CompletionCasError::InvalidEpoch(completion_epoch));
        }
        if completion_revision < 0 {
            return Err(CompletionCasError::InvalidRevision(completion_revision));
        }
        Ok(Self {
            task_id,
            completion_epoch,
            completion_revision,
        })
    }

    pub fn task_id(self) -> TaskId {
        self.task_id
    }

    pub fn completion_epoch(self) -> i64 {
        self.completion_epoch
    }

    pub fn completion_revision(self) -> i64 {
        self.completion_revision
    }

    pub fn basis(self) -> CompletionBasis {
        CompletionBasis {
            task_id: self.task_id,
            completion_epoch: self.completion_epoch,
            completion_revision: self.completion_revision,
        }
    }

    pub fn verify(self, basis: CompletionBasis) -> Result<(), CompletionCasError> {
        if basis.completion_epoch <= 0 {
            return Err(CompletionCasError::InvalidEpoch(basis.completion_epoch));
        }
        if basis.completion_revision < 0 {
            return Err(CompletionCasError::InvalidRevision(
                basis.completion_revision,
            ));
        }
        if basis.task_id != self.task_id {
            return Err(CompletionCasError::TaskMismatch {
                expected: self.task_id,
                actual: basis.task_id,
            });
        }
        if basis.completion_epoch != self.completion_epoch {
            return Err(CompletionCasError::CompletionEpochChanged {
                expected: basis.completion_epoch,
                actual: self.completion_epoch,
            });
        }
        if basis.completion_revision != self.completion_revision {
            return Err(CompletionCasError::ConclusionStale {
                expected: basis.completion_revision,
                actual: self.completion_revision,
            });
        }
        Ok(())
    }

    pub fn advance_revision(self) -> Result<Self, CompletionCasError> {
        let completion_revision = self
            .completion_revision
            .checked_add(1)
            .ok_or(CompletionCasError::RevisionOverflow)?;
        Ok(Self {
            completion_revision,
            ..self
        })
    }

    pub fn advance_epoch(self) -> Result<Self, CompletionCasError> {
        let completion_epoch = self
            .completion_epoch
            .checked_add(1)
            .ok_or(CompletionCasError::EpochOverflow)?;
        let completion_revision = self
            .completion_revision
            .checked_add(1)
            .ok_or(CompletionCasError::RevisionOverflow)?;
        Ok(Self {
            completion_epoch,
            completion_revision,
            ..self
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CompletionCasError {
    #[error("completion epoch must be positive, got {0}")]
    InvalidEpoch(i64),
    #[error("completion revision cannot be negative, got {0}")]
    InvalidRevision(i64),
    #[error("completion task mismatch: expected {expected}, got {actual}")]
    TaskMismatch { expected: TaskId, actual: TaskId },
    #[error("completion epoch changed: expected {expected}, current {actual}")]
    CompletionEpochChanged { expected: i64, actual: i64 },
    #[error("completion conclusion is stale: expected revision {expected}, current {actual}")]
    ConclusionStale { expected: i64, actual: i64 },
    #[error("completion epoch overflow")]
    EpochOverflow,
    #[error("completion revision overflow")]
    RevisionOverflow,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct CompletionSignalSnapshot(Map<String, Value>);

impl CompletionSignalSnapshot {
    pub fn new(values: Map<String, Value>) -> Self {
        Self(values)
    }

    pub fn values(&self) -> &Map<String, Value> {
        &self.0
    }

    pub fn into_values(self) -> Map<String, Value> {
        self.0
    }
}

impl TryFrom<Value> for CompletionSignalSnapshot {
    type Error = SignalSnapshotError;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::Object(values) => Ok(Self(values)),
            other => Err(SignalSnapshotError::ExpectedObject {
                actual: json_type_name(&other),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SignalSnapshotError {
    #[error("completion signal snapshot must be a JSON object, got {actual}")]
    ExpectedObject { actual: &'static str },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionWarning {
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct AgentConclusionInput {
    pub id: ConclusionId,
    pub participant_id: ParticipantId,
    pub generation: i64,
    pub basis: CompletionBasis,
    pub basis_event_cursor: i64,
    pub basis_plan_version: i64,
    pub assessment_schema_version: u32,
    pub assessment: CompletionAssessment,
    pub signal_snapshot: CompletionSignalSnapshot,
    pub warning_snapshot: Vec<CompletionWarning>,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentConclusion {
    id: ConclusionId,
    participant_id: ParticipantId,
    generation: i64,
    outcome: ConclusionOutcome,
    basis: CompletionBasis,
    basis_event_cursor: i64,
    basis_plan_version: i64,
    assessment_schema_version: u32,
    assessment: CompletionAssessment,
    signal_snapshot: CompletionSignalSnapshot,
    warning_snapshot: Vec<CompletionWarning>,
    #[serde(with = "super::time_serde")]
    created_at: OffsetDateTime,
}

impl AgentConclusion {
    pub fn try_new(
        input: AgentConclusionInput,
        current_head: CompletionHead,
    ) -> Result<Self, ConclusionValidationError> {
        current_head.verify(input.basis)?;
        if input.generation <= 0 {
            return Err(ConclusionValidationError::InvalidGeneration(
                input.generation,
            ));
        }
        if input.basis_event_cursor < 0 {
            return Err(ConclusionValidationError::InvalidEventCursor(
                input.basis_event_cursor,
            ));
        }
        if input.basis_plan_version < 0 {
            return Err(ConclusionValidationError::InvalidPlanVersion(
                input.basis_plan_version,
            ));
        }
        if input.assessment_schema_version != ASSESSMENT_SCHEMA_VERSION {
            return Err(ConclusionValidationError::UnsupportedAssessmentSchema {
                expected: ASSESSMENT_SCHEMA_VERSION,
                actual: input.assessment_schema_version,
            });
        }

        validate_assessment(input.assessment_schema_version, &input.assessment)?;
        validate_warnings(&input.warning_snapshot)?;

        let outcome = if input.assessment.expected_future_work {
            ConclusionOutcome::Blocked
        } else {
            ConclusionOutcome::NoFurtherWork
        };
        if outcome == ConclusionOutcome::Blocked
            && !input
                .assessment
                .open_signals
                .iter()
                .any(|signal| signal.entity_id.as_deref().is_some_and(is_non_blank))
        {
            return Err(ConclusionValidationError::BlockedWithoutReferencedSignal);
        }

        Ok(Self {
            id: input.id,
            participant_id: input.participant_id,
            generation: input.generation,
            outcome,
            basis: input.basis,
            basis_event_cursor: input.basis_event_cursor,
            basis_plan_version: input.basis_plan_version,
            assessment_schema_version: input.assessment_schema_version,
            assessment: input.assessment,
            signal_snapshot: input.signal_snapshot,
            warning_snapshot: input.warning_snapshot,
            created_at: input.created_at,
        })
    }

    pub fn id(&self) -> ConclusionId {
        self.id
    }

    pub fn participant_id(&self) -> ParticipantId {
        self.participant_id
    }

    pub fn generation(&self) -> i64 {
        self.generation
    }

    pub fn outcome(&self) -> ConclusionOutcome {
        self.outcome
    }

    pub fn task_id(&self) -> TaskId {
        self.basis.task_id
    }

    pub fn completion_epoch(&self) -> i64 {
        self.basis.completion_epoch
    }

    pub fn basis_completion_revision(&self) -> i64 {
        self.basis.completion_revision
    }

    pub fn basis(&self) -> CompletionBasis {
        self.basis
    }

    pub fn basis_event_cursor(&self) -> i64 {
        self.basis_event_cursor
    }

    pub fn basis_plan_version(&self) -> i64 {
        self.basis_plan_version
    }

    pub fn assessment_schema_version(&self) -> u32 {
        self.assessment_schema_version
    }

    pub fn assessment(&self) -> &CompletionAssessment {
        &self.assessment
    }

    pub fn signal_snapshot(&self) -> &CompletionSignalSnapshot {
        &self.signal_snapshot
    }

    pub fn warning_snapshot(&self) -> &[CompletionWarning] {
        &self.warning_snapshot
    }

    pub fn created_at(&self) -> OffsetDateTime {
        self.created_at
    }

    pub fn has_warnings(&self) -> bool {
        !self.warning_snapshot.is_empty()
            || !self.assessment.open_signals.is_empty()
            || !self.assessment.remaining_risks.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ConclusionValidationError {
    #[error(transparent)]
    Cas(#[from] CompletionCasError),
    #[error("conclusion generation must be positive, got {0}")]
    InvalidGeneration(i64),
    #[error("basis event cursor cannot be negative, got {0}")]
    InvalidEventCursor(i64),
    #[error("basis plan version cannot be negative, got {0}")]
    InvalidPlanVersion(i64),
    #[error("unsupported assessment schema: expected {expected}, got {actual}")]
    UnsupportedAssessmentSchema { expected: u32, actual: u32 },
    #[error("completion assessment summary cannot be blank")]
    EmptySummary,
    #[error("completion assessment must record at least one checked area")]
    EmptyCheckedAreas,
    #[error("checked area at index {index} cannot be blank")]
    BlankCheckedArea { index: usize },
    #[error("checked area is duplicated: {value}")]
    DuplicateCheckedArea { value: String },
    #[error("open signal field {field} at index {index} cannot be blank")]
    BlankOpenSignalField { index: usize, field: &'static str },
    #[error("open signal is duplicated: {code} ({entity_id:?})")]
    DuplicateOpenSignal {
        code: String,
        entity_id: Option<String>,
    },
    #[error("remaining risk at index {index} cannot be blank")]
    BlankRemainingRisk { index: usize },
    #[error("blocked conclusion requires an open signal with an entity reference")]
    BlockedWithoutReferencedSignal,
    #[error("warning field {field} at index {index} cannot be blank")]
    BlankWarningField { index: usize, field: &'static str },
    #[error("warning is duplicated: {code} ({entity_id:?})")]
    DuplicateWarning {
        code: String,
        entity_id: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct CompletionCohortParticipant {
    pub participant: TaskParticipant,
    pub current_conclusion: Option<AgentConclusion>,
    pub is_stale: bool,
    pub open_work: ParticipantWorkState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionCohortInvitation {
    pub id: InvitationId,
    pub task_id: TaskId,
    pub completion_epoch: i64,
    pub participant_kind: ParticipantKind,
    pub state: InvitationState,
    pub blocks_completion: bool,
}

impl CompletionCohortInvitation {
    pub fn is_open_blocking_worker_seat(&self) -> bool {
        self.participant_kind == ParticipantKind::Worker
            && self.blocks_completion
            && matches!(
                self.state,
                InvitationState::Prepared | InvitationState::Pending | InvitationState::Reserved
            )
    }
}

#[derive(Debug, Clone)]
pub struct CompletionCohortInput {
    pub head: CompletionHead,
    pub task_phase: TaskPhase,
    pub completion_policy: CompletionPolicy,
    pub participants: Vec<CompletionCohortParticipant>,
    pub invitations: Vec<CompletionCohortInvitation>,
    pub signal_warnings: Vec<CompletionWarning>,
}

#[derive(Debug, Clone)]
pub struct CompletionCohort {
    head: CompletionHead,
    task_phase: TaskPhase,
    completion_policy: CompletionPolicy,
    participants: Vec<CompletionCohortParticipant>,
    invitations: Vec<CompletionCohortInvitation>,
    signal_warnings: Vec<CompletionWarning>,
}

impl CompletionCohort {
    pub fn try_new(input: CompletionCohortInput) -> Result<Self, CompletionSnapshotError> {
        validate_warnings(&input.signal_warnings)
            .map_err(CompletionSnapshotError::InvalidSignalWarnings)?;

        let mut participant_ids = HashSet::new();
        let mut conclusion_ids = HashSet::new();
        for entry in &input.participants {
            let participant = &entry.participant;
            if participant.task_id() != input.head.task_id {
                return Err(CompletionSnapshotError::ParticipantTaskMismatch {
                    participant_id: participant.id(),
                    expected: input.head.task_id,
                    actual: participant.task_id(),
                });
            }
            if !participant_ids.insert(participant.id()) {
                return Err(CompletionSnapshotError::DuplicateParticipant(
                    participant.id(),
                ));
            }
            validate_participant_epoch(input.head, participant)?;
            validate_stale_marker(entry)?;
            validate_participant_conclusion(input.head, entry, &mut conclusion_ids)?;
            if matches!(
                participant.state(),
                ParticipantState::Retired | ParticipantState::Superseded
            ) && !entry.open_work.is_empty()
            {
                return Err(CompletionSnapshotError::TerminalParticipantOwnsWork {
                    participant_id: participant.id(),
                    state: participant.state(),
                });
            }
        }

        let mut invitation_ids = HashSet::new();
        for invitation in &input.invitations {
            if invitation.task_id != input.head.task_id {
                return Err(CompletionSnapshotError::InvitationTaskMismatch {
                    invitation_id: invitation.id,
                    expected: input.head.task_id,
                    actual: invitation.task_id,
                });
            }
            if !invitation_ids.insert(invitation.id) {
                return Err(CompletionSnapshotError::DuplicateInvitation(invitation.id));
            }
            if invitation.completion_epoch <= 0
                || invitation.completion_epoch > input.head.completion_epoch
            {
                return Err(CompletionSnapshotError::InvitationEpochInvalid {
                    invitation_id: invitation.id,
                    invitation_epoch: invitation.completion_epoch,
                    current_epoch: input.head.completion_epoch,
                });
            }
            if invitation.participant_kind == ParticipantKind::Worker
                && !invitation.blocks_completion
            {
                return Err(CompletionSnapshotError::WorkerInvitationMustBlock(
                    invitation.id,
                ));
            }
            if invitation.participant_kind == ParticipantKind::Observer
                && invitation.blocks_completion
            {
                return Err(CompletionSnapshotError::ObserverInvitationCannotBlock(
                    invitation.id,
                ));
            }
            if invitation.completion_epoch < input.head.completion_epoch
                && invitation.is_open_blocking_worker_seat()
            {
                return Err(CompletionSnapshotError::OpenInvitationFromPriorEpoch {
                    invitation_id: invitation.id,
                    invitation_epoch: invitation.completion_epoch,
                    current_epoch: input.head.completion_epoch,
                });
            }
        }

        Ok(Self {
            head: input.head,
            task_phase: input.task_phase,
            completion_policy: input.completion_policy,
            participants: input.participants,
            invitations: input.invitations,
            signal_warnings: input.signal_warnings,
        })
    }

    pub fn head(&self) -> CompletionHead {
        self.head
    }

    pub fn task_phase(&self) -> TaskPhase {
        self.task_phase
    }

    pub fn completion_policy(&self) -> CompletionPolicy {
        self.completion_policy
    }

    pub fn participants(&self) -> &[CompletionCohortParticipant] {
        &self.participants
    }

    pub fn invitations(&self) -> &[CompletionCohortInvitation] {
        &self.invitations
    }

    pub fn signal_warnings(&self) -> &[CompletionWarning] {
        &self.signal_warnings
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CompletionSnapshotError {
    #[error("participant {participant_id} belongs to task {actual}, expected {expected}")]
    ParticipantTaskMismatch {
        participant_id: ParticipantId,
        expected: TaskId,
        actual: TaskId,
    },
    #[error("duplicate participant in completion cohort: {0}")]
    DuplicateParticipant(ParticipantId),
    #[error(
        "participant {participant_id} has invalid epoch {participant_epoch} for current epoch {current_epoch}"
    )]
    ParticipantEpochInvalid {
        participant_id: ParticipantId,
        participant_epoch: i64,
        current_epoch: i64,
    },
    #[error("prior-epoch participant {participant_id} is not retired or superseded ({state:?})")]
    PriorEpochParticipantNotTerminal {
        participant_id: ParticipantId,
        state: ParticipantState,
    },
    #[error("current-epoch participant {0} cannot already be superseded")]
    CurrentEpochParticipantSuperseded(ParticipantId),
    #[error("participant {participant_id} has an invalid stale marker for state {state:?}")]
    InvalidStaleMarker {
        participant_id: ParticipantId,
        state: ParticipantState,
    },
    #[error("participant {participant_id} current conclusion pointer does not match snapshot")]
    CurrentConclusionPointerMismatch { participant_id: ParticipantId },
    #[error("conclusion {conclusion_id} belongs to participant {actual}, expected {expected}")]
    ConclusionParticipantMismatch {
        conclusion_id: ConclusionId,
        expected: ParticipantId,
        actual: ParticipantId,
    },
    #[error("conclusion {conclusion_id} belongs to task {actual}, expected {expected}")]
    ConclusionTaskMismatch {
        conclusion_id: ConclusionId,
        expected: TaskId,
        actual: TaskId,
    },
    #[error("conclusion {conclusion_id} belongs to epoch {actual}, expected {expected}")]
    ConclusionEpochMismatch {
        conclusion_id: ConclusionId,
        expected: i64,
        actual: i64,
    },
    #[error(
        "conclusion {conclusion_id} basis revision {basis_revision} is ahead of snapshot {snapshot_revision}"
    )]
    ConclusionFromFuture {
        conclusion_id: ConclusionId,
        basis_revision: i64,
        snapshot_revision: i64,
    },
    #[error("conclusion {conclusion_id} outcome {outcome:?} is invalid for state {state:?}")]
    ConclusionOutcomeStateMismatch {
        conclusion_id: ConclusionId,
        outcome: ConclusionOutcome,
        state: ParticipantState,
    },
    #[error("duplicate current conclusion in completion cohort: {0}")]
    DuplicateConclusion(ConclusionId),
    #[error("terminal participant {participant_id} in state {state:?} still owns work")]
    TerminalParticipantOwnsWork {
        participant_id: ParticipantId,
        state: ParticipantState,
    },
    #[error("invitation {invitation_id} belongs to task {actual}, expected {expected}")]
    InvitationTaskMismatch {
        invitation_id: InvitationId,
        expected: TaskId,
        actual: TaskId,
    },
    #[error("duplicate invitation in completion cohort: {0}")]
    DuplicateInvitation(InvitationId),
    #[error(
        "invitation {invitation_id} has invalid epoch {invitation_epoch} for current epoch {current_epoch}"
    )]
    InvitationEpochInvalid {
        invitation_id: InvitationId,
        invitation_epoch: i64,
        current_epoch: i64,
    },
    #[error("worker invitation must block completion: {0}")]
    WorkerInvitationMustBlock(InvitationId),
    #[error("observer invitation cannot block completion: {0}")]
    ObserverInvitationCannotBlock(InvitationId),
    #[error(
        "open invitation {invitation_id} remained in prior epoch {invitation_epoch}; current epoch is {current_epoch}"
    )]
    OpenInvitationFromPriorEpoch {
        invitation_id: InvitationId,
        invitation_epoch: i64,
        current_epoch: i64,
    },
    #[error("invalid final completion signal warnings: {0}")]
    InvalidSignalWarnings(ConclusionValidationError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum CompletionPendingReason {
    TaskNotActive {
        phase: TaskPhase,
    },
    NoWorkerParticipant,
    NoNoFurtherWorkConclusion,
    BlockingWorkerInvitation {
        invitation_id: InvitationId,
        state: InvitationState,
    },
    ActiveParticipant {
        participant_id: ParticipantId,
    },
    WaitingParticipant {
        participant_id: ParticipantId,
    },
    BlockedParticipant {
        participant_id: ParticipantId,
    },
    DepartedParticipant {
        participant_id: ParticipantId,
    },
    StaleParticipant {
        participant_id: ParticipantId,
    },
    ParticipantOwnsOpenWork {
        participant_id: ParticipantId,
        work: ParticipantWorkState,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompletionReady {
    pub conclusion_ids: Vec<ConclusionId>,
    pub conclusion_set_digest: [u8; 32],
    pub assurance_flags: Vec<AssuranceFlag>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompletionPending {
    pub reasons: Vec<CompletionPendingReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum CompletionReduction {
    Ready(CompletionReady),
    Pending(CompletionPending),
}

impl CompletionReduction {
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready(_))
    }

    pub fn ready(&self) -> Option<&CompletionReady> {
        match self {
            Self::Ready(ready) => Some(ready),
            Self::Pending(_) => None,
        }
    }

    pub fn pending_reasons(&self) -> &[CompletionPendingReason] {
        match self {
            Self::Ready(_) => &[],
            Self::Pending(pending) => &pending.reasons,
        }
    }
}

#[derive(Debug, Default)]
pub struct CompletionReducer;

impl CompletionReducer {
    pub fn reduce(cohort: &CompletionCohort) -> CompletionReduction {
        if cohort.task_phase != TaskPhase::Active {
            return CompletionReduction::Pending(CompletionPending {
                reasons: vec![CompletionPendingReason::TaskNotActive {
                    phase: cohort.task_phase,
                }],
            });
        }

        match cohort.completion_policy {
            CompletionPolicy::AllWorkersConcluded => reduce_all_workers_concluded(cohort),
        }
    }
}

fn reduce_all_workers_concluded(cohort: &CompletionCohort) -> CompletionReduction {
    let current_epoch = cohort.head.completion_epoch;
    let mut entries: Vec<_> = cohort
        .participants
        .iter()
        .filter(|entry| {
            entry.participant.completion_epoch() == current_epoch
                && entry.participant.kind() == ParticipantKind::Worker
        })
        .collect();
    entries.sort_by_key(|entry| entry.participant.id().0.as_u128());

    let retired_worker_count = entries
        .iter()
        .filter(|entry| entry.participant.state() == ParticipantState::Retired)
        .count();
    let unretired_entries: Vec<_> = entries
        .into_iter()
        .filter(|entry| entry.participant.state() != ParticipantState::Retired)
        .collect();

    let mut reasons = Vec::new();
    if unretired_entries.is_empty() {
        reasons.push(CompletionPendingReason::NoWorkerParticipant);
    }

    let mut invitations: Vec<_> = cohort
        .invitations
        .iter()
        .filter(|invitation| {
            invitation.completion_epoch == current_epoch
                && invitation.is_open_blocking_worker_seat()
        })
        .collect();
    invitations.sort_by_key(|invitation| invitation.id.0.as_u128());
    reasons.extend(invitations.into_iter().map(|invitation| {
        CompletionPendingReason::BlockingWorkerInvitation {
            invitation_id: invitation.id,
            state: invitation.state,
        }
    }));

    let mut conclusions = Vec::new();
    let mut conclusion_has_warnings = false;
    for entry in &unretired_entries {
        let participant = &entry.participant;
        if entry.is_stale {
            reasons.push(CompletionPendingReason::StaleParticipant {
                participant_id: participant.id(),
            });
        } else {
            match participant.state() {
                ParticipantState::Active => {
                    reasons.push(CompletionPendingReason::ActiveParticipant {
                        participant_id: participant.id(),
                    });
                }
                ParticipantState::Waiting => {
                    reasons.push(CompletionPendingReason::WaitingParticipant {
                        participant_id: participant.id(),
                    });
                }
                ParticipantState::Blocked => {
                    reasons.push(CompletionPendingReason::BlockedParticipant {
                        participant_id: participant.id(),
                    });
                }
                ParticipantState::Departed => {
                    reasons.push(CompletionPendingReason::DepartedParticipant {
                        participant_id: participant.id(),
                    });
                }
                ParticipantState::Concluded => {
                    let conclusion = entry
                        .current_conclusion
                        .as_ref()
                        .expect("validated concluded participant has a conclusion");
                    conclusions.push(conclusion.id());
                    conclusion_has_warnings |= conclusion.has_warnings();
                }
                ParticipantState::Retired | ParticipantState::Superseded => {
                    unreachable!("validated current unretired cohort excludes terminal states")
                }
            }
        }
        if !entry.open_work.is_empty() {
            reasons.push(CompletionPendingReason::ParticipantOwnsOpenWork {
                participant_id: participant.id(),
                work: entry.open_work,
            });
        }
    }

    if conclusions.is_empty() {
        reasons.push(CompletionPendingReason::NoNoFurtherWorkConclusion);
    }
    if !reasons.is_empty() {
        return CompletionReduction::Pending(CompletionPending { reasons });
    }

    conclusions.sort_by_key(|id| id.0.as_u128());
    let mut assurance_flags = Vec::new();
    if conclusions.len() == 1 {
        assurance_flags.push(AssuranceFlag::SingleAgentAssurance);
    }
    if !cohort.signal_warnings.is_empty() || conclusion_has_warnings {
        assurance_flags.push(AssuranceFlag::CompletedWithWarnings);
    }
    if retired_worker_count > 0 {
        assurance_flags.push(AssuranceFlag::RetiredParticipantAssurance);
    }

    CompletionReduction::Ready(CompletionReady {
        conclusion_set_digest: conclusion_set_digest(&conclusions),
        conclusion_ids: conclusions,
        assurance_flags,
    })
}

pub fn conclusion_set_digest(conclusion_ids: &[ConclusionId]) -> [u8; 32] {
    let mut sorted_ids = conclusion_ids.to_vec();
    sorted_ids.sort_by_key(|id| id.0.as_u128());

    let mut hasher = Sha256::new();
    hasher.update(CONCLUSION_DIGEST_DOMAIN);
    hasher.update((sorted_ids.len() as u64).to_be_bytes());
    for id in sorted_ids {
        hasher.update(id.0.as_bytes());
    }
    hasher.finalize().into()
}

fn validate_assessment(
    _schema_version: u32,
    assessment: &CompletionAssessment,
) -> Result<(), ConclusionValidationError> {
    if !is_non_blank(&assessment.summary) {
        return Err(ConclusionValidationError::EmptySummary);
    }
    if assessment.checked.is_empty() {
        return Err(ConclusionValidationError::EmptyCheckedAreas);
    }

    let mut checked = HashSet::new();
    for (index, value) in assessment.checked.iter().enumerate() {
        if !is_non_blank(value) {
            return Err(ConclusionValidationError::BlankCheckedArea { index });
        }
        if !checked.insert(value.as_str()) {
            return Err(ConclusionValidationError::DuplicateCheckedArea {
                value: value.clone(),
            });
        }
    }

    let mut signals = HashSet::new();
    for (index, signal) in assessment.open_signals.iter().enumerate() {
        for (field, value) in [
            ("code", signal.code.as_str()),
            ("disposition", signal.disposition.as_str()),
            ("rationale", signal.rationale.as_str()),
        ] {
            if !is_non_blank(value) {
                return Err(ConclusionValidationError::BlankOpenSignalField { index, field });
            }
        }
        if signal
            .entity_id
            .as_deref()
            .is_some_and(|id| !is_non_blank(id))
        {
            return Err(ConclusionValidationError::BlankOpenSignalField {
                index,
                field: "entity_id",
            });
        }
        let key = (signal.code.as_str(), signal.entity_id.as_deref());
        if !signals.insert(key) {
            return Err(ConclusionValidationError::DuplicateOpenSignal {
                code: signal.code.clone(),
                entity_id: signal.entity_id.clone(),
            });
        }
    }

    for (index, risk) in assessment.remaining_risks.iter().enumerate() {
        if !is_non_blank(risk) {
            return Err(ConclusionValidationError::BlankRemainingRisk { index });
        }
    }
    Ok(())
}

fn validate_warnings(warnings: &[CompletionWarning]) -> Result<(), ConclusionValidationError> {
    let mut seen = HashSet::new();
    for (index, warning) in warnings.iter().enumerate() {
        for (field, value) in [
            ("code", warning.code.as_str()),
            ("message", warning.message.as_str()),
        ] {
            if !is_non_blank(value) {
                return Err(ConclusionValidationError::BlankWarningField { index, field });
            }
        }
        if warning
            .entity_id
            .as_deref()
            .is_some_and(|id| !is_non_blank(id))
        {
            return Err(ConclusionValidationError::BlankWarningField {
                index,
                field: "entity_id",
            });
        }
        let key = (warning.code.as_str(), warning.entity_id.as_deref());
        if !seen.insert(key) {
            return Err(ConclusionValidationError::DuplicateWarning {
                code: warning.code.clone(),
                entity_id: warning.entity_id.clone(),
            });
        }
    }
    Ok(())
}

fn validate_participant_epoch(
    head: CompletionHead,
    participant: &TaskParticipant,
) -> Result<(), CompletionSnapshotError> {
    let participant_epoch = participant.completion_epoch();
    if participant_epoch <= 0 || participant_epoch > head.completion_epoch {
        return Err(CompletionSnapshotError::ParticipantEpochInvalid {
            participant_id: participant.id(),
            participant_epoch,
            current_epoch: head.completion_epoch,
        });
    }
    if participant_epoch < head.completion_epoch
        && !matches!(
            participant.state(),
            ParticipantState::Retired | ParticipantState::Superseded
        )
    {
        return Err(CompletionSnapshotError::PriorEpochParticipantNotTerminal {
            participant_id: participant.id(),
            state: participant.state(),
        });
    }
    if participant_epoch == head.completion_epoch
        && participant.state() == ParticipantState::Superseded
    {
        return Err(CompletionSnapshotError::CurrentEpochParticipantSuperseded(
            participant.id(),
        ));
    }
    Ok(())
}

fn validate_stale_marker(
    entry: &CompletionCohortParticipant,
) -> Result<(), CompletionSnapshotError> {
    if entry.is_stale
        && !matches!(
            entry.participant.state(),
            ParticipantState::Active | ParticipantState::Waiting | ParticipantState::Blocked
        )
    {
        return Err(CompletionSnapshotError::InvalidStaleMarker {
            participant_id: entry.participant.id(),
            state: entry.participant.state(),
        });
    }
    Ok(())
}

fn validate_participant_conclusion(
    head: CompletionHead,
    entry: &CompletionCohortParticipant,
    conclusion_ids: &mut HashSet<ConclusionId>,
) -> Result<(), CompletionSnapshotError> {
    let participant = &entry.participant;
    let snapshot_id = entry.current_conclusion.as_ref().map(AgentConclusion::id);
    if participant.current_conclusion_id() != snapshot_id {
        return Err(CompletionSnapshotError::CurrentConclusionPointerMismatch {
            participant_id: participant.id(),
        });
    }
    let Some(conclusion) = &entry.current_conclusion else {
        return Ok(());
    };

    if !conclusion_ids.insert(conclusion.id()) {
        return Err(CompletionSnapshotError::DuplicateConclusion(
            conclusion.id(),
        ));
    }
    if conclusion.participant_id() != participant.id() {
        return Err(CompletionSnapshotError::ConclusionParticipantMismatch {
            conclusion_id: conclusion.id(),
            expected: participant.id(),
            actual: conclusion.participant_id(),
        });
    }
    if conclusion.task_id() != participant.task_id() {
        return Err(CompletionSnapshotError::ConclusionTaskMismatch {
            conclusion_id: conclusion.id(),
            expected: participant.task_id(),
            actual: conclusion.task_id(),
        });
    }
    if conclusion.completion_epoch() != participant.completion_epoch() {
        return Err(CompletionSnapshotError::ConclusionEpochMismatch {
            conclusion_id: conclusion.id(),
            expected: participant.completion_epoch(),
            actual: conclusion.completion_epoch(),
        });
    }
    if conclusion.basis_completion_revision() > head.completion_revision {
        return Err(CompletionSnapshotError::ConclusionFromFuture {
            conclusion_id: conclusion.id(),
            basis_revision: conclusion.basis_completion_revision(),
            snapshot_revision: head.completion_revision,
        });
    }

    let valid_state = matches!(
        (participant.state(), conclusion.outcome()),
        (
            ParticipantState::Concluded,
            ConclusionOutcome::NoFurtherWork
        ) | (ParticipantState::Blocked, ConclusionOutcome::Blocked)
    );
    if !valid_state {
        return Err(CompletionSnapshotError::ConclusionOutcomeStateMismatch {
            conclusion_id: conclusion.id(),
            outcome: conclusion.outcome(),
            state: participant.state(),
        });
    }
    Ok(())
}

fn is_non_blank(value: &str) -> bool {
    !value.trim().is_empty()
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use time::{Duration, OffsetDateTime};

    use super::*;
    use crate::domain::participant::{
        NewTaskParticipant, ParticipantMutationBasis, TaskParticipant,
    };
    use crate::domain::{OpenSignalDisposition, SessionId};

    fn now() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::hours(2)
    }

    fn participant(task_id: TaskId, kind: ParticipantKind, epoch: i64) -> TaskParticipant {
        TaskParticipant::new(NewTaskParticipant {
            id: ParticipantId::new(),
            task_id,
            session_id: SessionId::new(),
            invitation_id: None,
            kind,
            completion_epoch: epoch,
            joined_at: now(),
        })
        .unwrap()
    }

    fn mutation_basis(participant: &TaskParticipant) -> ParticipantMutationBasis {
        ParticipantMutationBasis {
            task_id: participant.task_id(),
            task_phase: TaskPhase::Active,
            completion_epoch: participant.completion_epoch(),
            expected_participant_version: participant.version(),
            session_ended: false,
        }
    }

    fn conclusion(
        participant: &TaskParticipant,
        revision: i64,
        outcome: ConclusionOutcome,
        warnings: Vec<CompletionWarning>,
    ) -> AgentConclusion {
        let open_signals = if outcome == ConclusionOutcome::Blocked {
            vec![OpenSignalDisposition {
                code: "BLOCKER".into(),
                entity_id: Some("blocker-1".into()),
                disposition: "waiting_for_external_input".into(),
                rationale: "required credential is unavailable".into(),
            }]
        } else {
            Vec::new()
        };
        let head = CompletionHead::try_new(
            participant.task_id(),
            participant.completion_epoch(),
            revision,
        )
        .unwrap();
        AgentConclusion::try_new(
            AgentConclusionInput {
                id: ConclusionId::new(),
                participant_id: participant.id(),
                generation: 1,
                basis: head.basis(),
                basis_event_cursor: revision + 20,
                basis_plan_version: 3,
                assessment_schema_version: 1,
                assessment: CompletionAssessment {
                    summary: "checked the complete task state".into(),
                    checked: vec!["task_criteria".into(), "repository_and_tests".into()],
                    open_signals,
                    remaining_risks: Vec::new(),
                    expected_future_work: outcome == ConclusionOutcome::Blocked,
                },
                signal_snapshot: CompletionSignalSnapshot::try_from(json!({
                    "completion_revision": revision
                }))
                .unwrap(),
                warning_snapshot: warnings,
                created_at: now() + Duration::minutes(1),
            },
            head,
        )
        .unwrap()
    }

    fn concluded_entry(task_id: TaskId, epoch: i64, revision: i64) -> CompletionCohortParticipant {
        let mut participant = participant(task_id, ParticipantKind::Worker, epoch);
        let conclusion = conclusion(
            &participant,
            revision,
            ConclusionOutcome::NoFurtherWork,
            Vec::new(),
        );
        participant
            .conclude(
                mutation_basis(&participant),
                &conclusion,
                ParticipantWorkState::default(),
            )
            .unwrap();
        CompletionCohortParticipant {
            participant,
            current_conclusion: Some(conclusion),
            is_stale: false,
            open_work: ParticipantWorkState::default(),
        }
    }

    fn cohort(
        task_id: TaskId,
        epoch: i64,
        revision: i64,
        participants: Vec<CompletionCohortParticipant>,
        invitations: Vec<CompletionCohortInvitation>,
        signal_warnings: Vec<CompletionWarning>,
    ) -> CompletionCohort {
        CompletionCohort::try_new(CompletionCohortInput {
            head: CompletionHead::try_new(task_id, epoch, revision).unwrap(),
            task_phase: TaskPhase::Active,
            completion_policy: CompletionPolicy::AllWorkersConcluded,
            participants,
            invitations,
            signal_warnings,
        })
        .unwrap()
    }

    fn pending_reason(
        reduction: &CompletionReduction,
        predicate: impl Fn(&CompletionPendingReason) -> bool,
    ) -> bool {
        reduction.pending_reasons().iter().any(predicate)
    }

    #[test]
    fn completion_basis_cas_distinguishes_epoch_from_revision_changes() {
        let task_id = TaskId::new();
        let head = CompletionHead::try_new(task_id, 3, 10).unwrap();
        assert_eq!(
            head.verify(CompletionBasis {
                task_id,
                completion_epoch: 2,
                completion_revision: 10,
            })
            .unwrap_err(),
            CompletionCasError::CompletionEpochChanged {
                expected: 2,
                actual: 3,
            }
        );
        assert_eq!(
            head.verify(CompletionBasis {
                task_id,
                completion_epoch: 3,
                completion_revision: 9,
            })
            .unwrap_err(),
            CompletionCasError::ConclusionStale {
                expected: 9,
                actual: 10,
            }
        );
        assert_eq!(head.advance_epoch().unwrap().basis().completion_epoch, 4);
        assert_eq!(
            head.advance_epoch().unwrap().basis().completion_revision,
            11
        );
    }

    #[test]
    fn conclusion_input_rejects_blank_assessment_and_unreferenced_blocker() {
        let task_id = TaskId::new();
        let participant = participant(task_id, ParticipantKind::Worker, 1);
        let head = CompletionHead::try_new(task_id, 1, 1).unwrap();
        let mut input = AgentConclusionInput {
            id: ConclusionId::new(),
            participant_id: participant.id(),
            generation: 1,
            basis: head.basis(),
            basis_event_cursor: 1,
            basis_plan_version: 0,
            assessment_schema_version: 1,
            assessment: CompletionAssessment {
                summary: " ".into(),
                checked: vec!["repository".into()],
                open_signals: Vec::new(),
                remaining_risks: Vec::new(),
                expected_future_work: false,
            },
            signal_snapshot: CompletionSignalSnapshot::default(),
            warning_snapshot: Vec::new(),
            created_at: now(),
        };
        assert_eq!(
            AgentConclusion::try_new(input.clone(), head).unwrap_err(),
            ConclusionValidationError::EmptySummary
        );

        input.assessment.summary = "work remains".into();
        input.assessment.expected_future_work = true;
        assert_eq!(
            AgentConclusion::try_new(input, head).unwrap_err(),
            ConclusionValidationError::BlockedWithoutReferencedSignal
        );
    }

    #[test]
    fn conclusions_from_different_revisions_can_complete_the_same_epoch() {
        let task_id = TaskId::new();
        let first = concluded_entry(task_id, 2, 5);
        let second = concluded_entry(task_id, 2, 8);
        let first_id = first.current_conclusion.as_ref().unwrap().id();
        let second_id = second.current_conclusion.as_ref().unwrap().id();
        let cohort = cohort(task_id, 2, 9, vec![second, first], Vec::new(), Vec::new());

        let reduction = CompletionReducer::reduce(&cohort);
        let ready = reduction.ready().unwrap();
        assert_eq!(ready.conclusion_ids.len(), 2);
        assert!(ready.conclusion_ids.contains(&first_id));
        assert!(ready.conclusion_ids.contains(&second_id));
        assert!(ready.assurance_flags.is_empty());
    }

    #[test]
    fn blocked_participant_never_counts_as_completed() {
        let task_id = TaskId::new();
        let mut participant = participant(task_id, ParticipantKind::Worker, 1);
        let conclusion = conclusion(&participant, 4, ConclusionOutcome::Blocked, Vec::new());
        participant
            .conclude(
                mutation_basis(&participant),
                &conclusion,
                ParticipantWorkState::default(),
            )
            .unwrap();
        let cohort = cohort(
            task_id,
            1,
            5,
            vec![CompletionCohortParticipant {
                participant,
                current_conclusion: Some(conclusion),
                is_stale: false,
                open_work: ParticipantWorkState::default(),
            }],
            Vec::new(),
            Vec::new(),
        );
        let reduction = CompletionReducer::reduce(&cohort);
        assert!(pending_reason(&reduction, |reason| matches!(
            reason,
            CompletionPendingReason::BlockedParticipant { .. }
        )));
        assert!(pending_reason(&reduction, |reason| matches!(
            reason,
            CompletionPendingReason::NoNoFurtherWorkConclusion
        )));
    }

    #[test]
    fn stale_active_participant_blocks_completion_without_becoming_concluded() {
        let task_id = TaskId::new();
        let active = participant(task_id, ParticipantKind::Worker, 1);
        let cohort = cohort(
            task_id,
            1,
            2,
            vec![CompletionCohortParticipant {
                participant: active,
                current_conclusion: None,
                is_stale: true,
                open_work: ParticipantWorkState::default(),
            }],
            Vec::new(),
            Vec::new(),
        );
        let reduction = CompletionReducer::reduce(&cohort);
        assert!(pending_reason(&reduction, |reason| matches!(
            reason,
            CompletionPendingReason::StaleParticipant { .. }
        )));
    }

    #[test]
    fn departed_participant_blocks_completion() {
        let task_id = TaskId::new();
        let mut departed = participant(task_id, ParticipantKind::Worker, 1);
        departed
            .leave(
                mutation_basis(&departed),
                ParticipantWorkState::default(),
                now() + Duration::minutes(1),
            )
            .unwrap();
        let cohort = cohort(
            task_id,
            1,
            2,
            vec![CompletionCohortParticipant {
                participant: departed,
                current_conclusion: None,
                is_stale: false,
                open_work: ParticipantWorkState::default(),
            }],
            Vec::new(),
            Vec::new(),
        );
        let reduction = CompletionReducer::reduce(&cohort);
        assert!(pending_reason(&reduction, |reason| matches!(
            reason,
            CompletionPendingReason::DepartedParticipant { .. }
        )));
    }

    #[test]
    fn pending_worker_invitation_blocks_but_pending_observer_invitation_does_not() {
        let task_id = TaskId::new();
        let participant = concluded_entry(task_id, 1, 2);
        let worker_invitation = CompletionCohortInvitation {
            id: InvitationId::new(),
            task_id,
            completion_epoch: 1,
            participant_kind: ParticipantKind::Worker,
            state: InvitationState::Pending,
            blocks_completion: true,
        };
        let observer_invitation = CompletionCohortInvitation {
            id: InvitationId::new(),
            task_id,
            completion_epoch: 1,
            participant_kind: ParticipantKind::Observer,
            state: InvitationState::Pending,
            blocks_completion: false,
        };
        let blocked = cohort(
            task_id,
            1,
            3,
            vec![participant.clone()],
            vec![worker_invitation],
            Vec::new(),
        );
        assert!(pending_reason(
            &CompletionReducer::reduce(&blocked),
            |reason| matches!(
                reason,
                CompletionPendingReason::BlockingWorkerInvitation { .. }
            )
        ));

        let ready = cohort(
            task_id,
            1,
            3,
            vec![participant],
            vec![observer_invitation],
            Vec::new(),
        );
        assert!(CompletionReducer::reduce(&ready).is_ready());
    }

    #[test]
    fn observer_participants_do_not_join_or_block_the_worker_barrier() {
        let task_id = TaskId::new();
        let worker = concluded_entry(task_id, 1, 2);
        let observer = participant(task_id, ParticipantKind::Observer, 1);
        let cohort = cohort(
            task_id,
            1,
            3,
            vec![
                CompletionCohortParticipant {
                    participant: observer,
                    current_conclusion: None,
                    is_stale: true,
                    open_work: ParticipantWorkState::default(),
                },
                worker,
            ],
            Vec::new(),
            Vec::new(),
        );
        assert!(CompletionReducer::reduce(&cohort).is_ready());
    }

    #[test]
    fn retired_worker_is_excluded_and_adds_assurance_flags() {
        let task_id = TaskId::new();
        let concluded = concluded_entry(task_id, 1, 2);
        let mut retired = participant(task_id, ParticipantKind::Worker, 1);
        retired
            .retire(
                mutation_basis(&retired),
                concluded.participant.id(),
                ParticipantWorkState::default(),
                now() + Duration::minutes(1),
            )
            .unwrap();
        let cohort = cohort(
            task_id,
            1,
            4,
            vec![
                concluded,
                CompletionCohortParticipant {
                    participant: retired,
                    current_conclusion: None,
                    is_stale: false,
                    open_work: ParticipantWorkState::default(),
                },
            ],
            Vec::new(),
            Vec::new(),
        );
        let ready = CompletionReducer::reduce(&cohort).ready().unwrap().clone();
        assert_eq!(
            ready.assurance_flags,
            vec![
                AssuranceFlag::SingleAgentAssurance,
                AssuranceFlag::RetiredParticipantAssurance
            ]
        );
    }

    #[test]
    fn all_retired_workers_cannot_complete_without_a_final_scanner() {
        let task_id = TaskId::new();
        let mut retired = participant(task_id, ParticipantKind::Worker, 1);
        retired
            .retire(
                mutation_basis(&retired),
                ParticipantId::new(),
                ParticipantWorkState::default(),
                now() + Duration::minutes(1),
            )
            .unwrap();
        let cohort = cohort(
            task_id,
            1,
            2,
            vec![CompletionCohortParticipant {
                participant: retired,
                current_conclusion: None,
                is_stale: false,
                open_work: ParticipantWorkState::default(),
            }],
            Vec::new(),
            Vec::new(),
        );
        assert!(pending_reason(
            &CompletionReducer::reduce(&cohort),
            |reason| matches!(reason, CompletionPendingReason::NoWorkerParticipant)
        ));
    }

    #[test]
    fn open_session_work_blocks_even_a_valid_no_further_work_conclusion() {
        let task_id = TaskId::new();
        let mut entry = concluded_entry(task_id, 1, 2);
        entry.open_work.active_claims = 1;
        let cohort = cohort(task_id, 1, 3, vec![entry], Vec::new(), Vec::new());
        assert!(pending_reason(
            &CompletionReducer::reduce(&cohort),
            |reason| matches!(
                reason,
                CompletionPendingReason::ParticipantOwnsOpenWork { .. }
            )
        ));
    }

    #[test]
    fn prior_epoch_superseded_participant_is_ignored() {
        let task_id = TaskId::new();
        let mut old = participant(task_id, ParticipantKind::Worker, 1);
        old.supersede(old.version(), 2).unwrap();
        let current = concluded_entry(task_id, 2, 4);
        let cohort = cohort(
            task_id,
            2,
            5,
            vec![
                CompletionCohortParticipant {
                    participant: old,
                    current_conclusion: None,
                    is_stale: false,
                    open_work: ParticipantWorkState::default(),
                },
                current,
            ],
            Vec::new(),
            Vec::new(),
        );
        assert!(CompletionReducer::reduce(&cohort).is_ready());
    }

    #[test]
    fn warnings_and_single_agent_are_preserved_as_assurance_flags() {
        let task_id = TaskId::new();
        let warning = CompletionWarning {
            code: "LIMITED_PLATFORM_COVERAGE".into(),
            entity_id: None,
            message: "tests only ran on Linux".into(),
        };
        let cohort = cohort(
            task_id,
            1,
            3,
            vec![concluded_entry(task_id, 1, 2)],
            Vec::new(),
            vec![warning],
        );
        assert_eq!(
            CompletionReducer::reduce(&cohort)
                .ready()
                .unwrap()
                .assurance_flags,
            vec![
                AssuranceFlag::SingleAgentAssurance,
                AssuranceFlag::CompletedWithWarnings
            ]
        );
    }

    #[test]
    fn conclusion_digest_is_independent_of_input_order() {
        let first = ConclusionId::new();
        let second = ConclusionId::new();
        assert_eq!(
            conclusion_set_digest(&[first, second]),
            conclusion_set_digest(&[second, first])
        );
    }
}
