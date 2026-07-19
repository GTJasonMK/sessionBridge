use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;

use super::completion::AgentConclusion;
use super::{
    ConclusionId, ConclusionOutcome, InvitationId, ParticipantId, ParticipantKind,
    ParticipantState, SessionId, TaskId, TaskPhase,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParticipantMutationBasis {
    pub task_id: TaskId,
    pub task_phase: TaskPhase,
    pub completion_epoch: i64,
    pub expected_participant_version: i64,
    pub session_ended: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ParticipantWorkState {
    pub active_claims: u32,
    pub active_plan_or_implement_intents: u32,
    pub unsubmitted_attempts: u32,
    pub claim_or_attempt_writable_reservations: u32,
    pub directed_blocking_requests: u32,
}

impl ParticipantWorkState {
    pub fn is_clear_for_wait(self) -> bool {
        self.active_claims == 0
            && self.active_plan_or_implement_intents == 0
            && self.unsubmitted_attempts == 0
            && self.claim_or_attempt_writable_reservations == 0
    }

    pub fn is_clear_for_conclusion(self) -> bool {
        self.is_clear_for_wait() && self.directed_blocking_requests == 0
    }

    pub fn is_empty(self) -> bool {
        self.is_clear_for_conclusion()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitReadiness {
    pub work: ParticipantWorkState,
    pub has_directed_responsibility: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewTaskParticipant {
    pub id: ParticipantId,
    pub task_id: TaskId,
    pub session_id: SessionId,
    pub invitation_id: Option<InvitationId>,
    pub kind: ParticipantKind,
    pub completion_epoch: i64,
    pub joined_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskParticipantRecord {
    pub id: ParticipantId,
    pub task_id: TaskId,
    pub session_id: SessionId,
    pub invitation_id: Option<InvitationId>,
    pub kind: ParticipantKind,
    pub completion_epoch: i64,
    pub state: ParticipantState,
    pub version: i64,
    pub current_conclusion_id: Option<ConclusionId>,
    #[serde(with = "super::time_serde")]
    pub joined_at: OffsetDateTime,
    #[serde(with = "super::time_serde::option")]
    pub departed_at: Option<OffsetDateTime>,
    #[serde(with = "super::time_serde::option")]
    pub retired_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskParticipant {
    id: ParticipantId,
    task_id: TaskId,
    session_id: SessionId,
    invitation_id: Option<InvitationId>,
    kind: ParticipantKind,
    completion_epoch: i64,
    state: ParticipantState,
    version: i64,
    current_conclusion_id: Option<ConclusionId>,
    #[serde(with = "super::time_serde")]
    joined_at: OffsetDateTime,
    #[serde(with = "super::time_serde::option")]
    departed_at: Option<OffsetDateTime>,
    #[serde(with = "super::time_serde::option")]
    retired_at: Option<OffsetDateTime>,
}

impl TaskParticipant {
    pub fn new(input: NewTaskParticipant) -> Result<Self, ParticipantError> {
        Self::try_from_record(TaskParticipantRecord {
            id: input.id,
            task_id: input.task_id,
            session_id: input.session_id,
            invitation_id: input.invitation_id,
            kind: input.kind,
            completion_epoch: input.completion_epoch,
            state: ParticipantState::Active,
            version: 1,
            current_conclusion_id: None,
            joined_at: input.joined_at,
            departed_at: None,
            retired_at: None,
        })
    }

    pub fn try_from_record(record: TaskParticipantRecord) -> Result<Self, ParticipantError> {
        validate_record(&record)?;
        Ok(Self {
            id: record.id,
            task_id: record.task_id,
            session_id: record.session_id,
            invitation_id: record.invitation_id,
            kind: record.kind,
            completion_epoch: record.completion_epoch,
            state: record.state,
            version: record.version,
            current_conclusion_id: record.current_conclusion_id,
            joined_at: record.joined_at,
            departed_at: record.departed_at,
            retired_at: record.retired_at,
        })
    }

    pub fn to_record(&self) -> TaskParticipantRecord {
        TaskParticipantRecord {
            id: self.id,
            task_id: self.task_id,
            session_id: self.session_id,
            invitation_id: self.invitation_id,
            kind: self.kind,
            completion_epoch: self.completion_epoch,
            state: self.state,
            version: self.version,
            current_conclusion_id: self.current_conclusion_id,
            joined_at: self.joined_at,
            departed_at: self.departed_at,
            retired_at: self.retired_at,
        }
    }

    pub fn id(&self) -> ParticipantId {
        self.id
    }

    pub fn task_id(&self) -> TaskId {
        self.task_id
    }

    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub fn invitation_id(&self) -> Option<InvitationId> {
        self.invitation_id
    }

    pub fn kind(&self) -> ParticipantKind {
        self.kind
    }

    pub fn completion_epoch(&self) -> i64 {
        self.completion_epoch
    }

    pub fn state(&self) -> ParticipantState {
        self.state
    }

    pub fn version(&self) -> i64 {
        self.version
    }

    pub fn current_conclusion_id(&self) -> Option<ConclusionId> {
        self.current_conclusion_id
    }

    pub fn joined_at(&self) -> OffsetDateTime {
        self.joined_at
    }

    pub fn departed_at(&self) -> Option<OffsetDateTime> {
        self.departed_at
    }

    pub fn retired_at(&self) -> Option<OffsetDateTime> {
        self.retired_at
    }

    pub fn wait(
        &mut self,
        basis: ParticipantMutationBasis,
        readiness: WaitReadiness,
    ) -> Result<(), ParticipantError> {
        self.validate_active_worker_mutation(basis, ParticipantOperation::Wait)?;
        self.require_state(ParticipantOperation::Wait, &[ParticipantState::Active])?;
        if !readiness.work.is_clear_for_wait() {
            return Err(ParticipantError::SessionHasActiveWork {
                operation: ParticipantOperation::Wait,
                work: readiness.work,
            });
        }
        if !readiness.has_directed_responsibility {
            return Err(ParticipantError::WaitResponsibilityRequired);
        }

        self.state = ParticipantState::Waiting;
        self.version = self.next_version()?;
        Ok(())
    }

    pub fn resume(&mut self, basis: ParticipantMutationBasis) -> Result<(), ParticipantError> {
        self.validate_active_worker_mutation(basis, ParticipantOperation::Resume)?;
        self.require_state(
            ParticipantOperation::Resume,
            &[
                ParticipantState::Waiting,
                ParticipantState::Concluded,
                ParticipantState::Blocked,
            ],
        )?;

        self.state = ParticipantState::Active;
        self.current_conclusion_id = None;
        self.version = self.next_version()?;
        Ok(())
    }

    pub fn conclude(
        &mut self,
        basis: ParticipantMutationBasis,
        conclusion: &AgentConclusion,
        work: ParticipantWorkState,
    ) -> Result<(), ParticipantError> {
        self.validate_active_worker_mutation(basis, ParticipantOperation::Conclude)?;
        self.require_state(
            ParticipantOperation::Conclude,
            &[ParticipantState::Active, ParticipantState::Waiting],
        )?;
        if !work.is_clear_for_conclusion() {
            return Err(ParticipantError::SessionHasActiveWork {
                operation: ParticipantOperation::Conclude,
                work,
            });
        }
        if conclusion.participant_id() != self.id {
            return Err(ParticipantError::ConclusionParticipantMismatch {
                expected: self.id,
                actual: conclusion.participant_id(),
            });
        }
        if conclusion.task_id() != self.task_id {
            return Err(ParticipantError::ConclusionTaskMismatch {
                expected: self.task_id,
                actual: conclusion.task_id(),
            });
        }
        if conclusion.completion_epoch() != self.completion_epoch {
            return Err(ParticipantError::ConclusionEpochMismatch {
                expected: self.completion_epoch,
                actual: conclusion.completion_epoch(),
            });
        }
        if conclusion.created_at() < self.joined_at {
            return Err(ParticipantError::ConclusionPredatesParticipant);
        }

        self.state = match conclusion.outcome() {
            ConclusionOutcome::NoFurtherWork => ParticipantState::Concluded,
            ConclusionOutcome::Blocked => ParticipantState::Blocked,
        };
        self.current_conclusion_id = Some(conclusion.id());
        self.version = self.next_version()?;
        Ok(())
    }

    pub fn leave(
        &mut self,
        basis: ParticipantMutationBasis,
        work: ParticipantWorkState,
        departed_at: OffsetDateTime,
    ) -> Result<(), ParticipantError> {
        self.validate_identity_version_and_epoch(basis)?;
        if basis.session_ended {
            return Err(ParticipantError::SessionAlreadyEnded);
        }
        self.require_state(
            ParticipantOperation::Leave,
            &[
                ParticipantState::Active,
                ParticipantState::Waiting,
                ParticipantState::Blocked,
            ],
        )?;
        if !work.is_clear_for_conclusion() {
            return Err(ParticipantError::SessionHasActiveWork {
                operation: ParticipantOperation::Leave,
                work,
            });
        }
        self.validate_lifecycle_timestamp(departed_at)?;

        self.state = ParticipantState::Departed;
        self.current_conclusion_id = None;
        self.departed_at = Some(departed_at);
        self.version = self.next_version()?;
        Ok(())
    }

    pub fn retire(
        &mut self,
        basis: ParticipantMutationBasis,
        proposer_participant_id: ParticipantId,
        work: ParticipantWorkState,
        retired_at: OffsetDateTime,
    ) -> Result<(), ParticipantError> {
        self.validate_active_task_mutation(basis, ParticipantOperation::Retire)?;
        if proposer_participant_id == self.id {
            return Err(ParticipantError::SelfRetirementNotAllowed);
        }
        self.require_state(
            ParticipantOperation::Retire,
            &[
                ParticipantState::Active,
                ParticipantState::Waiting,
                ParticipantState::Concluded,
                ParticipantState::Blocked,
                ParticipantState::Departed,
            ],
        )?;
        if !work.is_clear_for_conclusion() {
            return Err(ParticipantError::SessionHasActiveWork {
                operation: ParticipantOperation::Retire,
                work,
            });
        }
        self.validate_lifecycle_timestamp(retired_at)?;

        self.state = ParticipantState::Retired;
        self.current_conclusion_id = None;
        self.retired_at = Some(retired_at);
        self.version = self.next_version()?;
        Ok(())
    }

    pub fn supersede(
        &mut self,
        expected_participant_version: i64,
        new_completion_epoch: i64,
    ) -> Result<(), ParticipantError> {
        self.validate_version(expected_participant_version)?;
        if new_completion_epoch <= self.completion_epoch {
            return Err(ParticipantError::CompletionEpochNotAdvanced {
                current: self.completion_epoch,
                proposed: new_completion_epoch,
            });
        }
        self.require_state(
            ParticipantOperation::Supersede,
            &[
                ParticipantState::Active,
                ParticipantState::Waiting,
                ParticipantState::Concluded,
                ParticipantState::Blocked,
                ParticipantState::Departed,
            ],
        )?;

        self.state = ParticipantState::Superseded;
        self.current_conclusion_id = None;
        self.version = self.next_version()?;
        Ok(())
    }

    fn validate_active_worker_mutation(
        &self,
        basis: ParticipantMutationBasis,
        operation: ParticipantOperation,
    ) -> Result<(), ParticipantError> {
        self.validate_active_task_mutation(basis, operation)?;
        if self.kind != ParticipantKind::Worker {
            return Err(ParticipantError::ObserverCannotPerform { operation });
        }
        if basis.session_ended {
            return Err(ParticipantError::SessionAlreadyEnded);
        }
        Ok(())
    }

    fn validate_active_task_mutation(
        &self,
        basis: ParticipantMutationBasis,
        _operation: ParticipantOperation,
    ) -> Result<(), ParticipantError> {
        self.validate_identity_version_and_epoch(basis)?;
        if basis.task_phase != TaskPhase::Active {
            return Err(ParticipantError::TaskNotActive {
                phase: basis.task_phase,
            });
        }
        Ok(())
    }

    fn validate_identity_version_and_epoch(
        &self,
        basis: ParticipantMutationBasis,
    ) -> Result<(), ParticipantError> {
        if basis.task_id != self.task_id {
            return Err(ParticipantError::TaskMismatch {
                expected: self.task_id,
                actual: basis.task_id,
            });
        }
        self.validate_version(basis.expected_participant_version)?;
        if basis.completion_epoch != self.completion_epoch {
            return Err(ParticipantError::CompletionEpochChanged {
                expected: self.completion_epoch,
                actual: basis.completion_epoch,
            });
        }
        Ok(())
    }

    fn validate_version(&self, expected: i64) -> Result<(), ParticipantError> {
        if expected != self.version {
            return Err(ParticipantError::ParticipantVersionChanged {
                expected,
                actual: self.version,
            });
        }
        Ok(())
    }

    fn require_state(
        &self,
        operation: ParticipantOperation,
        allowed: &[ParticipantState],
    ) -> Result<(), ParticipantError> {
        if !allowed.contains(&self.state) {
            return Err(ParticipantError::InvalidTransition {
                operation,
                from: self.state,
            });
        }
        Ok(())
    }

    fn validate_lifecycle_timestamp(&self, value: OffsetDateTime) -> Result<(), ParticipantError> {
        if value < self.joined_at {
            return Err(ParticipantError::LifecycleTimestampBeforeJoin);
        }
        Ok(())
    }

    fn next_version(&self) -> Result<i64, ParticipantError> {
        self.version
            .checked_add(1)
            .ok_or(ParticipantError::ParticipantVersionOverflow)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParticipantOperation {
    Wait,
    Resume,
    Conclude,
    Leave,
    Retire,
    Supersede,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ParticipantError {
    #[error("completion epoch must be positive, got {0}")]
    InvalidCompletionEpoch(i64),
    #[error("participant version must be positive, got {0}")]
    InvalidParticipantVersion(i64),
    #[error("task participant invariant violated: {0}")]
    InvalidRecord(&'static str),
    #[error("task mismatch: expected {expected}, got {actual}")]
    TaskMismatch { expected: TaskId, actual: TaskId },
    #[error("participant version changed: expected {expected}, got {actual}")]
    ParticipantVersionChanged { expected: i64, actual: i64 },
    #[error("completion epoch changed: expected {expected}, got {actual}")]
    CompletionEpochChanged { expected: i64, actual: i64 },
    #[error("task is not active (phase: {phase:?})")]
    TaskNotActive { phase: TaskPhase },
    #[error("session has already ended")]
    SessionAlreadyEnded,
    #[error("observer participant cannot perform {operation:?}")]
    ObserverCannotPerform { operation: ParticipantOperation },
    #[error("cannot perform {operation:?} from participant state {from:?}")]
    InvalidTransition {
        operation: ParticipantOperation,
        from: ParticipantState,
    },
    #[error(
        "waiting requires a directed review, handoff, request, or short-term unlock responsibility"
    )]
    WaitResponsibilityRequired,
    #[error("cannot perform {operation:?} while session-owned work remains: {work:?}")]
    SessionHasActiveWork {
        operation: ParticipantOperation,
        work: ParticipantWorkState,
    },
    #[error("conclusion participant mismatch: expected {expected}, got {actual}")]
    ConclusionParticipantMismatch {
        expected: ParticipantId,
        actual: ParticipantId,
    },
    #[error("conclusion task mismatch: expected {expected}, got {actual}")]
    ConclusionTaskMismatch { expected: TaskId, actual: TaskId },
    #[error("conclusion epoch mismatch: expected {expected}, got {actual}")]
    ConclusionEpochMismatch { expected: i64, actual: i64 },
    #[error("conclusion was created before the participant joined")]
    ConclusionPredatesParticipant,
    #[error("a participant cannot retire itself")]
    SelfRetirementNotAllowed,
    #[error("completion epoch must advance beyond {current}, got {proposed}")]
    CompletionEpochNotAdvanced { current: i64, proposed: i64 },
    #[error("lifecycle timestamp cannot precede joined_at")]
    LifecycleTimestampBeforeJoin,
    #[error("participant version overflow")]
    ParticipantVersionOverflow,
}

fn validate_record(record: &TaskParticipantRecord) -> Result<(), ParticipantError> {
    if record.completion_epoch <= 0 {
        return Err(ParticipantError::InvalidCompletionEpoch(
            record.completion_epoch,
        ));
    }
    if record.version <= 0 {
        return Err(ParticipantError::InvalidParticipantVersion(record.version));
    }

    let needs_conclusion = matches!(
        record.state,
        ParticipantState::Concluded | ParticipantState::Blocked
    );
    if needs_conclusion != record.current_conclusion_id.is_some() {
        return Err(ParticipantError::InvalidRecord(
            "concluded/blocked must have a current conclusion and all other states must not",
        ));
    }
    if needs_conclusion && record.kind != ParticipantKind::Worker {
        return Err(ParticipantError::InvalidRecord(
            "observer cannot be concluded or blocked",
        ));
    }
    if record.state == ParticipantState::Departed && record.departed_at.is_none() {
        return Err(ParticipantError::InvalidRecord(
            "departed participant must have departed_at",
        ));
    }
    if record.retired_at.is_some() != (record.state == ParticipantState::Retired) {
        return Err(ParticipantError::InvalidRecord(
            "retired_at must be set exactly for retired participants",
        ));
    }
    if record
        .departed_at
        .is_some_and(|departed_at| departed_at < record.joined_at)
        || record
            .retired_at
            .is_some_and(|retired_at| retired_at < record.joined_at)
    {
        return Err(ParticipantError::LifecycleTimestampBeforeJoin);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use time::{Duration, OffsetDateTime};

    use super::*;
    use crate::domain::completion::{
        AgentConclusion, AgentConclusionInput, CompletionBasis, CompletionHead,
        CompletionSignalSnapshot, CompletionWarning,
    };
    use crate::domain::{CompletionAssessment, OpenSignalDisposition};

    fn joined_at() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::hours(1)
    }

    fn participant(kind: ParticipantKind) -> TaskParticipant {
        TaskParticipant::new(NewTaskParticipant {
            id: ParticipantId::new(),
            task_id: TaskId::new(),
            session_id: SessionId::new(),
            invitation_id: None,
            kind,
            completion_epoch: 2,
            joined_at: joined_at(),
        })
        .unwrap()
    }

    fn basis(participant: &TaskParticipant) -> ParticipantMutationBasis {
        ParticipantMutationBasis {
            task_id: participant.task_id(),
            task_phase: TaskPhase::Active,
            completion_epoch: participant.completion_epoch(),
            expected_participant_version: participant.version(),
            session_ended: false,
        }
    }

    fn conclusion(participant: &TaskParticipant, outcome: ConclusionOutcome) -> AgentConclusion {
        let head = CompletionHead::try_new(participant.task_id(), 2, 8).unwrap();
        let (expected_future_work, open_signals) = match outcome {
            ConclusionOutcome::NoFurtherWork => (false, Vec::new()),
            ConclusionOutcome::Blocked => (
                true,
                vec![OpenSignalDisposition {
                    code: "EXTERNAL_BLOCKER".into(),
                    entity_id: Some("blocker-1".into()),
                    disposition: "awaiting_external_input".into(),
                    rationale: "credentials are unavailable".into(),
                }],
            ),
        };
        AgentConclusion::try_new(
            AgentConclusionInput {
                id: ConclusionId::new(),
                participant_id: participant.id(),
                generation: 1,
                basis: CompletionBasis {
                    task_id: participant.task_id(),
                    completion_epoch: 2,
                    completion_revision: 8,
                },
                basis_event_cursor: 11,
                basis_plan_version: 3,
                assessment_schema_version: 1,
                assessment: CompletionAssessment {
                    summary: "global scan complete".into(),
                    checked: vec!["repository_and_tests".into()],
                    open_signals,
                    remaining_risks: Vec::new(),
                    expected_future_work,
                },
                signal_snapshot: CompletionSignalSnapshot::try_from(json!({})).unwrap(),
                warning_snapshot: Vec::<CompletionWarning>::new(),
                created_at: joined_at() + Duration::minutes(1),
            },
            head,
        )
        .unwrap()
    }

    #[test]
    fn wait_requires_a_directed_responsibility_and_resume_is_versioned() {
        let mut participant = participant(ParticipantKind::Worker);
        let error = participant
            .wait(
                basis(&participant),
                WaitReadiness {
                    work: ParticipantWorkState::default(),
                    has_directed_responsibility: false,
                },
            )
            .unwrap_err();
        assert_eq!(error, ParticipantError::WaitResponsibilityRequired);

        participant
            .wait(
                basis(&participant),
                WaitReadiness {
                    work: ParticipantWorkState::default(),
                    has_directed_responsibility: true,
                },
            )
            .unwrap();
        assert_eq!(participant.state(), ParticipantState::Waiting);
        assert_eq!(participant.version(), 2);

        let stale_basis = ParticipantMutationBasis {
            expected_participant_version: 1,
            ..basis(&participant)
        };
        assert_eq!(
            participant.resume(stale_basis).unwrap_err(),
            ParticipantError::ParticipantVersionChanged {
                expected: 1,
                actual: 2
            }
        );
        participant.resume(basis(&participant)).unwrap();
        assert_eq!(participant.state(), ParticipantState::Active);
    }

    #[test]
    fn conclusion_and_resume_keep_the_conclusion_immutable_but_clear_current_pointer() {
        let mut participant = participant(ParticipantKind::Worker);
        let conclusion = conclusion(&participant, ConclusionOutcome::NoFurtherWork);

        participant
            .conclude(
                basis(&participant),
                &conclusion,
                ParticipantWorkState::default(),
            )
            .unwrap();
        assert_eq!(participant.state(), ParticipantState::Concluded);
        assert_eq!(participant.current_conclusion_id(), Some(conclusion.id()));

        participant.resume(basis(&participant)).unwrap();
        assert_eq!(participant.state(), ParticipantState::Active);
        assert_eq!(participant.current_conclusion_id(), None);
        assert_eq!(conclusion.outcome(), ConclusionOutcome::NoFurtherWork);
    }

    #[test]
    fn blocked_is_not_a_no_further_work_conclusion_and_requires_resume() {
        let mut participant = participant(ParticipantKind::Worker);
        let blocked = conclusion(&participant, ConclusionOutcome::Blocked);
        participant
            .conclude(
                basis(&participant),
                &blocked,
                ParticipantWorkState::default(),
            )
            .unwrap();
        assert_eq!(participant.state(), ParticipantState::Blocked);

        let second = conclusion(&participant, ConclusionOutcome::NoFurtherWork);
        assert!(matches!(
            participant.conclude(
                basis(&participant),
                &second,
                ParticipantWorkState::default()
            ),
            Err(ParticipantError::InvalidTransition {
                operation: ParticipantOperation::Conclude,
                from: ParticipantState::Blocked
            })
        ));
        participant.resume(basis(&participant)).unwrap();
        participant
            .conclude(
                basis(&participant),
                &second,
                ParticipantWorkState::default(),
            )
            .unwrap();
        assert_eq!(participant.state(), ParticipantState::Concluded);
        assert_eq!(participant.current_conclusion_id(), Some(second.id()));
    }

    #[test]
    fn leave_retire_and_supersede_are_distinct_transitions() {
        let mut departed = participant(ParticipantKind::Worker);
        departed
            .leave(
                basis(&departed),
                ParticipantWorkState::default(),
                joined_at() + Duration::minutes(2),
            )
            .unwrap();
        assert_eq!(departed.state(), ParticipantState::Departed);
        assert!(departed.departed_at().is_some());

        let proposer = ParticipantId::new();
        departed
            .retire(
                basis(&departed),
                proposer,
                ParticipantWorkState::default(),
                joined_at() + Duration::minutes(3),
            )
            .unwrap();
        assert_eq!(departed.state(), ParticipantState::Retired);

        let mut superseded = participant(ParticipantKind::Worker);
        superseded.supersede(superseded.version(), 3).unwrap();
        assert_eq!(superseded.state(), ParticipantState::Superseded);
        assert!(matches!(
            superseded.supersede(superseded.version(), 4),
            Err(ParticipantError::InvalidTransition { .. })
        ));
    }

    #[test]
    fn observer_cannot_wait_or_conclude() {
        let mut observer = participant(ParticipantKind::Observer);
        assert_eq!(
            observer
                .wait(
                    basis(&observer),
                    WaitReadiness {
                        work: ParticipantWorkState::default(),
                        has_directed_responsibility: true,
                    }
                )
                .unwrap_err(),
            ParticipantError::ObserverCannotPerform {
                operation: ParticipantOperation::Wait
            }
        );

        // The conclusion belongs to a worker because invalid observer conclusions cannot be built.
        let worker = participant(ParticipantKind::Worker);
        let conclusion = conclusion(&worker, ConclusionOutcome::NoFurtherWork);
        assert!(matches!(
            observer.conclude(
                basis(&observer),
                &conclusion,
                ParticipantWorkState::default()
            ),
            Err(ParticipantError::ObserverCannotPerform {
                operation: ParticipantOperation::Conclude
            })
        ));
    }

    #[test]
    fn epoch_mismatch_and_active_work_fail_without_mutating_state() {
        let mut participant = participant(ParticipantKind::Worker);
        let old_record = participant.to_record();
        let wrong_epoch = ParticipantMutationBasis {
            completion_epoch: 3,
            ..basis(&participant)
        };
        assert!(matches!(
            participant.resume(wrong_epoch),
            Err(ParticipantError::CompletionEpochChanged { .. })
        ));
        assert_eq!(participant.to_record(), old_record);

        let work = ParticipantWorkState {
            active_claims: 1,
            ..ParticipantWorkState::default()
        };
        assert!(matches!(
            participant.wait(
                basis(&participant),
                WaitReadiness {
                    work,
                    has_directed_responsibility: true
                }
            ),
            Err(ParticipantError::SessionHasActiveWork { .. })
        ));
        assert_eq!(participant.to_record(), old_record);
    }
}
