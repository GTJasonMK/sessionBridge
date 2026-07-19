use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{
    Result,
    app::{
        agent::{AgentService, AuthenticatedAgent},
        events::{self, EventActor, EventEntity},
        idempotency::{self, IdempotencyKey},
        task::{TaskHealth, TaskService, WorkspaceBinding},
        task_reconciler::TaskReconciler,
    },
    clock::{Clock, unix_micros},
    domain::{
        AgentConclusion, AgentConclusionInput, AssuranceFlag, CompletionAssessment,
        CompletionBasis, CompletionCohort, CompletionCohortInput, CompletionCohortInvitation,
        CompletionCohortParticipant, CompletionHead, CompletionPolicy, CompletionReducer,
        CompletionSignalSnapshot, CompletionWarning, ConclusionId, ConclusionOutcome,
        ParticipantId, ParticipantMutationBasis, ParticipantState, ParticipantWorkState,
        SessionContext, SessionId, TaskId, TaskParticipant, TaskParticipantRecord, TaskPhase,
    },
    error::{ErrorCode, SessionBridgeError},
    storage::SqliteStore,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConcludeRequest {
    pub outcome: ConclusionOutcome,
    pub basis_completion_revision: i64,
    pub basis_event_cursor: i64,
    pub basis_plan_version: i64,
    pub assessment: CompletionAssessment,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMutationRequest {
    pub reason: Option<String>,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConcludeResult {
    pub conclusion_id: ConclusionId,
    pub participation_state: ParticipantState,
    pub task_phase: TaskPhase,
    pub should_exit: bool,
    pub requires_intervention: bool,
    pub completion_created: bool,
    pub completion_id: Option<String>,
    pub assurance_flags: Vec<AssuranceFlag>,
    pub open_signal_count: usize,
    pub completion_pending_reasons: Vec<serde_json::Value>,
    pub event_sequence: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMutationResult {
    pub participation_state: ParticipantState,
    pub participant_version: i64,
    pub task_phase: TaskPhase,
    pub should_exit: bool,
    pub completion_created: bool,
    pub completion_id: Option<String>,
    pub event_sequence: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconcileResult {
    pub task_phase: TaskPhase,
    pub completion_created: bool,
    pub completion_id: Option<String>,
    pub assurance_flags: Vec<AssuranceFlag>,
    pub pending_reasons: Vec<serde_json::Value>,
    pub expired_invitations: usize,
    pub event_sequence: Option<i64>,
}

#[derive(Debug, Clone, Copy)]
pub struct TaskCompletionContext<'a> {
    pub workspace_id: crate::domain::WorkspaceId,
    pub repository_instance_id: &'a str,
    pub task_id: TaskId,
}

impl<'a> From<&'a SessionContext> for TaskCompletionContext<'a> {
    fn from(context: &'a SessionContext) -> Self {
        Self {
            workspace_id: context.workspace_id,
            repository_instance_id: &context.repository_instance_id,
            task_id: context.task_id,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CompletionTrigger<'a> {
    pub actor_type: &'a str,
    pub actor_id: Option<&'a str>,
}

pub struct CompletionService<C> {
    clock: C,
}

impl<C: Clock + Clone> CompletionService<C> {
    pub fn new(clock: C) -> Self {
        Self { clock }
    }

    pub fn conclude(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut request: ConcludeRequest,
    ) -> Result<ConcludeResult> {
        TaskReconciler::new(self.clock.clone()).reconcile_agent(store, context)?;
        validate_request_id(&request.request_id)?;
        request.assessment.expected_future_work = request.outcome == ConclusionOutcome::Blocked;
        let payload_hash = idempotency::hash_payload(&serde_json::json!({
            "operation": "agent.conclude",
            "request": request,
        }))?;
        let authenticated =
            AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let actor_id = authenticated.session.session_id.to_string();
        let key = IdempotencyKey {
            actor_type: "session",
            actor_id: &actor_id,
            request_id: &request.request_id,
        };
        if let Some(stored) = idempotency::replay(&transaction, key, &payload_hash)? {
            transaction.commit()?;
            return Ok(stored.value);
        }
        let task = load_task_head(&transaction, context.task_id)?;
        if matches!(
            authenticated.participant.state,
            ParticipantState::Concluded | ParticipantState::Blocked
        ) {
            let conclusion_id_text = authenticated
                .participant
                .current_conclusion_id
                .as_deref()
                .ok_or_else(|| {
                    SessionBridgeError::domain(
                        ErrorCode::Database,
                        "terminal Participant is missing its current conclusion",
                    )
                })?;
            let (stored_outcome, assessment_json): (String, String) = transaction.query_row(
                "SELECT outcome,assessment_json FROM agent_conclusions
                 WHERE id=?1 AND task_id=?2",
                params![conclusion_id_text, context.task_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let stored_outcome: ConclusionOutcome = stored_outcome.parse().map_err(|error| {
                SessionBridgeError::domain(
                    ErrorCode::Database,
                    format!("stored conclusion outcome is invalid: {error}"),
                )
            })?;
            if stored_outcome != request.outcome {
                return Err(SessionBridgeError::domain(
                    ErrorCode::SessionAlreadyConcluded,
                    "Participant already has a different terminal conclusion",
                )
                .with_details(serde_json::json!({
                    "current_conclusion_id": conclusion_id_text,
                    "current_outcome": stored_outcome,
                    "requested_outcome": request.outcome,
                })));
            }
            let stored_assessment: CompletionAssessment = serde_json::from_str(&assessment_json)?;
            let reconcile = reconcile_in_transaction(
                &transaction,
                TaskCompletionContext::from(context),
                self.clock.clone(),
                CompletionTrigger {
                    actor_type: "session",
                    actor_id: Some(&actor_id),
                },
            )?;
            let fallback_sequence =
                events::latest_cursor(&transaction, Some(&context.task_id.to_string()))?;
            let result = ConcludeResult {
                conclusion_id: parse_id(
                    conclusion_id_text,
                    "task_participants.current_conclusion_id",
                )?,
                participation_state: authenticated.participant.state,
                task_phase: reconcile.task_phase,
                should_exit: true,
                requires_intervention: stored_outcome == ConclusionOutcome::Blocked,
                completion_created: reconcile.completion_created,
                completion_id: reconcile.completion_id,
                assurance_flags: reconcile.assurance_flags,
                open_signal_count: stored_assessment.open_signals.len(),
                completion_pending_reasons: reconcile.pending_reasons,
                event_sequence: reconcile.event_sequence.unwrap_or(fallback_sequence),
            };
            idempotency::record(
                &transaction,
                key,
                Some(&context.task_id.to_string()),
                &payload_hash,
                &result,
                Some(result.event_sequence),
                now,
            )?;
            transaction.commit()?;
            return Ok(result);
        }
        verify_conclusion_basis(&task, &request)?;
        let work = load_participant_work(
            &transaction,
            context.task_id,
            authenticated.session.session_id,
            now,
        )?;
        let mut participant =
            load_participant(&transaction, authenticated.participant.participant_id)?;
        let health = task_health(
            &transaction,
            TaskCompletionContext::from(context),
            self.clock.clone(),
        )?;
        let signal_snapshot = signal_snapshot(&health)?;
        let warnings = completion_warnings(&health);
        let generation: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(generation), 0) + 1 FROM agent_conclusions WHERE participant_id = ?1",
            [participant.id().to_string()],
            |row| row.get(0),
        )?;
        let conclusion_id = ConclusionId::new();
        let basis = CompletionBasis {
            task_id: task.task_id,
            completion_epoch: task.completion_epoch,
            completion_revision: task.completion_revision,
        };
        let conclusion = AgentConclusion::try_new(
            AgentConclusionInput {
                id: conclusion_id,
                participant_id: participant.id(),
                generation,
                basis,
                basis_event_cursor: request.basis_event_cursor,
                basis_plan_version: request.basis_plan_version,
                assessment_schema_version: 1,
                assessment: request.assessment.clone(),
                signal_snapshot: signal_snapshot.clone(),
                warning_snapshot: warnings.clone(),
                created_at: now,
            },
            CompletionHead::try_new(
                task.task_id,
                task.completion_epoch,
                task.completion_revision,
            )
            .map_err(domain_internal)?,
        )
        .map_err(map_conclusion_error)?;
        if conclusion.outcome() != request.outcome {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "completion assessment does not match the requested outcome",
            ));
        }
        participant
            .conclude(
                participant_basis(&task, &participant, &authenticated),
                &conclusion,
                work,
            )
            .map_err(map_participant_error)?;
        transaction.execute(
            "INSERT INTO agent_conclusions (
                id, task_id, participant_id, generation, outcome,
                basis_completion_revision, basis_event_cursor, basis_plan_version,
                assessment_schema_version, assessment_json, signal_snapshot_json,
                warning_snapshot_json, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1, ?9, ?10, ?11, ?12)",
            params![
                conclusion.id().to_string(),
                task.task_id.to_string(),
                participant.id().to_string(),
                generation,
                conclusion.outcome().as_str(),
                task.completion_revision,
                request.basis_event_cursor,
                request.basis_plan_version,
                serde_json::to_string(&request.assessment)?,
                serde_json::to_string(signal_snapshot.values())?,
                serde_json::to_string(&warnings)?,
                unix_micros(now),
            ],
        )?;
        persist_participant(&transaction, &participant, now)?;
        let new_revision = increment_completion_revision(
            &transaction,
            task.task_id,
            task.completion_revision,
            now,
        )?;
        let event_type = if request.outcome == ConclusionOutcome::Blocked {
            "agent.blocked"
        } else {
            "agent.concluded"
        };
        let event_sequence = append_agent_event(
            &transaction,
            context,
            &authenticated,
            event_type,
            "conclusion",
            &conclusion.id().to_string(),
            &serde_json::json!({
                "participant_id": participant.id(),
                "outcome": conclusion.outcome(),
                "completion_revision": new_revision,
            }),
            now,
        )?;
        let reconcile = reconcile_in_transaction(
            &transaction,
            TaskCompletionContext::from(context),
            self.clock.clone(),
            CompletionTrigger {
                actor_type: "session",
                actor_id: Some(&actor_id),
            },
        )?;
        let result = ConcludeResult {
            conclusion_id,
            participation_state: participant.state(),
            task_phase: reconcile.task_phase,
            should_exit: true,
            requires_intervention: request.outcome == ConclusionOutcome::Blocked,
            completion_created: reconcile.completion_created,
            completion_id: reconcile.completion_id,
            assurance_flags: reconcile.assurance_flags,
            open_signal_count: request.assessment.open_signals.len(),
            completion_pending_reasons: reconcile.pending_reasons,
            event_sequence: reconcile.event_sequence.unwrap_or(event_sequence),
        };
        idempotency::record(
            &transaction,
            key,
            Some(&context.task_id.to_string()),
            &payload_hash,
            &result,
            Some(result.event_sequence),
            now,
        )?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn resume(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        request: AgentMutationRequest,
    ) -> Result<AgentMutationResult> {
        self.mutate_participant(store, context, request, ParticipantMutation::Resume)
    }

    pub fn leave(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        request: AgentMutationRequest,
    ) -> Result<AgentMutationResult> {
        self.mutate_participant(store, context, request, ParticipantMutation::Leave)
    }

    pub fn reconcile(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        trigger: CompletionTrigger<'_>,
    ) -> Result<ReconcileResult> {
        let transaction = store.begin_immediate()?;
        let result = reconcile_in_transaction(
            &transaction,
            TaskCompletionContext::from(context),
            self.clock.clone(),
            trigger,
        )?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn reconcile_task(
        &self,
        store: &mut SqliteStore,
        context: TaskCompletionContext<'_>,
        trigger: CompletionTrigger<'_>,
    ) -> Result<ReconcileResult> {
        let transaction = store.begin_immediate()?;
        let result = reconcile_in_transaction(&transaction, context, self.clock.clone(), trigger)?;
        transaction.commit()?;
        Ok(result)
    }

    fn mutate_participant(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        request: AgentMutationRequest,
        mutation: ParticipantMutation,
    ) -> Result<AgentMutationResult> {
        validate_request_id(&request.request_id)?;
        let operation = match mutation {
            ParticipantMutation::Resume => "agent.resume",
            ParticipantMutation::Leave => "agent.leave",
        };
        let payload_hash = idempotency::hash_payload(&serde_json::json!({
            "operation": operation,
            "request": request,
        }))?;
        let authenticated =
            AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let actor_id = authenticated.session.session_id.to_string();
        let key = IdempotencyKey {
            actor_type: "session",
            actor_id: &actor_id,
            request_id: &request.request_id,
        };
        if let Some(stored) = idempotency::replay(&transaction, key, &payload_hash)? {
            transaction.commit()?;
            return Ok(stored.value);
        }
        let active_wait: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_waits WHERE participant_id = ?1 AND status = 'active')",
            [authenticated.participant.participant_id.to_string()],
            |row| row.get(0),
        )?;
        if active_wait {
            return Err(SessionBridgeError::domain(
                ErrorCode::WaitAlreadyActive,
                "active dependency await can only end through dependency or Task state",
            ));
        }
        let task = load_task_head(&transaction, context.task_id)?;
        let mut participant =
            load_participant(&transaction, authenticated.participant.participant_id)?;
        let work = load_participant_work(
            &transaction,
            context.task_id,
            authenticated.session.session_id,
            now,
        )?;
        let basis = participant_basis(&task, &participant, &authenticated);
        match mutation {
            ParticipantMutation::Resume => participant.resume(basis),
            ParticipantMutation::Leave => participant.leave(basis, work, now),
        }
        .map_err(map_participant_error)?;
        persist_participant(&transaction, &participant, now)?;
        if mutation == ParticipantMutation::Leave {
            transaction.execute(
                "UPDATE agent_sessions SET ended_at = ?2, end_reason = ?3, updated_at = ?2
                 WHERE id = ?1 AND ended_at IS NULL",
                params![
                    authenticated.session.session_id.to_string(),
                    unix_micros(now),
                    request.reason.as_deref().unwrap_or("left"),
                ],
            )?;
        }
        increment_completion_revision(&transaction, task.task_id, task.completion_revision, now)?;
        let event_type = match mutation {
            ParticipantMutation::Resume => "agent.resumed",
            ParticipantMutation::Leave => "agent.left",
        };
        let event_sequence = append_agent_event(
            &transaction,
            context,
            &authenticated,
            event_type,
            "participant",
            &participant.id().to_string(),
            &serde_json::json!({ "reason": request.reason }),
            now,
        )?;
        let reconcile = reconcile_in_transaction(
            &transaction,
            TaskCompletionContext::from(context),
            self.clock.clone(),
            CompletionTrigger {
                actor_type: "session",
                actor_id: Some(&actor_id),
            },
        )?;
        let result = AgentMutationResult {
            participation_state: participant.state(),
            participant_version: participant.version(),
            task_phase: reconcile.task_phase,
            should_exit: mutation == ParticipantMutation::Leave,
            completion_created: reconcile.completion_created,
            completion_id: reconcile.completion_id,
            event_sequence: reconcile.event_sequence.unwrap_or(event_sequence),
        };
        idempotency::record(
            &transaction,
            key,
            Some(&context.task_id.to_string()),
            &payload_hash,
            &result,
            Some(result.event_sequence),
            now,
        )?;
        transaction.commit()?;
        Ok(result)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParticipantMutation {
    Resume,
    Leave,
}

pub fn reconcile_in_transaction<C: Clock + Clone>(
    transaction: &Transaction<'_>,
    context: TaskCompletionContext<'_>,
    clock: C,
    trigger: CompletionTrigger<'_>,
) -> Result<ReconcileResult> {
    let now = clock.now();
    let expired_ids = expire_invitations(transaction, context, now)?;
    let task = load_task_head(transaction, context.task_id)?;
    if let Some(existing) = existing_completion(transaction, task.task_id, task.completion_epoch)? {
        return Ok(ReconcileResult {
            task_phase: task.phase,
            completion_created: false,
            completion_id: Some(existing),
            assurance_flags: load_assurance_flags(
                transaction,
                task.task_id,
                task.completion_epoch,
            )?,
            pending_reasons: Vec::new(),
            expired_invitations: expired_ids.len(),
            event_sequence: None,
        });
    }
    let health = task_health(transaction, context, clock.clone())?;
    let deliverable_blockers = [
        (
            "required_deliverables_open",
            health.signals.required_deliverables_open,
        ),
        (
            "required_deliverables_unmapped",
            health.signals.required_deliverables_unmapped,
        ),
        (
            "required_deliverables_unverified",
            health.signals.required_deliverables_unverified,
        ),
    ];
    if deliverable_blockers.iter().any(|(_, count)| *count > 0) {
        return Ok(ReconcileResult {
            task_phase: task.phase,
            completion_created: false,
            completion_id: None,
            assurance_flags: Vec::new(),
            pending_reasons: deliverable_blockers
                .into_iter()
                .filter(|(_, count)| *count > 0)
                .map(|(code, count)| serde_json::json!({ "code": code, "count": count }))
                .collect(),
            expired_invitations: expired_ids.len(),
            event_sequence: None,
        });
    }
    let warnings = completion_warnings(&health);
    let cohort = load_cohort(transaction, &task, &warnings, now)?;
    let reduction = CompletionReducer::reduce(&cohort);
    let Some(ready) = reduction.ready() else {
        return Ok(ReconcileResult {
            task_phase: task.phase,
            completion_created: false,
            completion_id: None,
            assurance_flags: Vec::new(),
            pending_reasons: reduction
                .pending_reasons()
                .iter()
                .map(serde_json::to_value)
                .collect::<std::result::Result<Vec<_>, _>>()?,
            expired_invitations: expired_ids.len(),
            event_sequence: None,
        });
    };
    let completion_id = uuid::Uuid::now_v7().to_string();
    let basis_event_cursor: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(sequence), 0) FROM events WHERE task_id = ?1",
        [task.task_id.to_string()],
        |row| row.get(0),
    )?;
    let signal_snapshot = serde_json::to_string(&health)?;
    let event_sequence = events::append(
        transaction,
        context.workspace_id,
        Some(&task.task_id.to_string()),
        "task.completed",
        EventActor {
            kind: trigger.actor_type,
            id: trigger.actor_id,
        },
        EventEntity {
            kind: "task_completion",
            id: Some(&completion_id),
        },
        &serde_json::json!({
            "completion_id": completion_id,
            "completion_epoch": task.completion_epoch,
            "conclusion_ids": ready.conclusion_ids,
            "assurance_flags": ready.assurance_flags,
        }),
        now,
    )?;
    transaction.execute(
        "INSERT INTO task_completions (
            id, task_id, completion_epoch, completion_policy, trigger_actor_type,
            trigger_actor_id, conclusion_set_digest, snapshot_completion_revision,
            basis_event_cursor, basis_plan_version, completion_event_sequence,
            signal_snapshot_json, completed_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            completion_id,
            task.task_id.to_string(),
            task.completion_epoch,
            task.completion_policy.as_str(),
            trigger.actor_type,
            trigger.actor_id,
            ready.conclusion_set_digest.as_slice(),
            task.completion_revision,
            basis_event_cursor,
            task.plan_version,
            event_sequence,
            signal_snapshot,
            unix_micros(now),
        ],
    )?;
    for (ordinal, conclusion_id) in ready.conclusion_ids.iter().enumerate() {
        transaction.execute(
            "INSERT INTO task_completion_conclusions (
                task_completion_id, task_id, agent_conclusion_id, ordinal
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                completion_id,
                task.task_id.to_string(),
                conclusion_id.to_string(),
                i64::try_from(ordinal).map_err(|_| SessionBridgeError::domain(
                    ErrorCode::Internal,
                    "completion conclusion ordinal overflow"
                ))?,
            ],
        )?;
    }
    for flag in &ready.assurance_flags {
        transaction.execute(
            "INSERT INTO task_completion_assurance_flags (task_completion_id, flag)
             VALUES (?1, ?2)",
            params![completion_id, flag.as_str()],
        )?;
    }
    let updated = transaction.execute(
        "UPDATE tasks SET phase = 'completed', completed_at = ?3,
                updated_at = ?3, version = version + 1
         WHERE id = ?1 AND phase = 'active' AND completion_epoch = ?2",
        params![
            task.task_id.to_string(),
            task.completion_epoch,
            unix_micros(now)
        ],
    )?;
    if updated != 1 {
        return Err(SessionBridgeError::domain(
            ErrorCode::VersionConflict,
            "Task changed while creating TaskCompletion",
        ));
    }
    transaction.execute(
        "UPDATE agent_invitations SET state = 'revoked', revoked_at = ?2, updated_at = ?2
         WHERE task_id = ?1 AND participant_kind = 'observer'
           AND state IN ('prepared', 'pending', 'reserved')",
        params![task.task_id.to_string(), unix_micros(now)],
    )?;
    Ok(ReconcileResult {
        task_phase: TaskPhase::Completed,
        completion_created: true,
        completion_id: Some(completion_id),
        assurance_flags: ready.assurance_flags.clone(),
        pending_reasons: Vec::new(),
        expired_invitations: expired_ids.len(),
        event_sequence: Some(event_sequence),
    })
}

#[derive(Debug, Clone)]
struct TaskHead {
    task_id: TaskId,
    phase: TaskPhase,
    completion_policy: CompletionPolicy,
    completion_epoch: i64,
    completion_revision: i64,
    plan_version: i64,
}

fn load_task_head(connection: &Connection, task_id: TaskId) -> Result<TaskHead> {
    let row: Option<(String, String, i64, i64, i64)> = connection
        .query_row(
            "SELECT phase, completion_policy, completion_epoch, completion_revision, plan_version
             FROM tasks WHERE id = ?1",
            [task_id.to_string()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let Some((phase, policy, epoch, revision, plan_version)) = row else {
        return Err(SessionBridgeError::domain(
            ErrorCode::NotFound,
            "Task was not found",
        ));
    };
    Ok(TaskHead {
        task_id,
        phase: parse_enum(&phase, "tasks.phase")?,
        completion_policy: parse_enum(&policy, "tasks.completion_policy")?,
        completion_epoch: epoch,
        completion_revision: revision,
        plan_version,
    })
}

fn load_participant(
    connection: &Connection,
    participant_id: ParticipantId,
) -> Result<TaskParticipant> {
    #[allow(clippy::type_complexity)]
    let row: Option<(
        String,
        String,
        Option<String>,
        String,
        i64,
        String,
        i64,
        Option<String>,
        i64,
        Option<i64>,
        Option<i64>,
    )> = connection
        .query_row(
            "SELECT task_id, session_id, invitation_id, kind, completion_epoch, state,
                    version, current_conclusion_id, joined_at, departed_at, retired_at
             FROM task_participants WHERE id = ?1",
            [participant_id.to_string()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                ))
            },
        )
        .optional()?;
    let Some((
        task,
        session,
        invitation,
        kind,
        epoch,
        state,
        version,
        conclusion,
        joined,
        departed,
        retired,
    )) = row
    else {
        return Err(SessionBridgeError::domain(
            ErrorCode::NotFound,
            "TaskParticipant was not found",
        ));
    };
    TaskParticipant::try_from_record(TaskParticipantRecord {
        id: participant_id,
        task_id: parse_id(&task, "task_participants.task_id")?,
        session_id: parse_id(&session, "task_participants.session_id")?,
        invitation_id: invitation
            .map(|value| parse_id(&value, "task_participants.invitation_id"))
            .transpose()?,
        kind: parse_enum(&kind, "task_participants.kind")?,
        completion_epoch: epoch,
        state: parse_enum(&state, "task_participants.state")?,
        version,
        current_conclusion_id: conclusion
            .map(|value| parse_id(&value, "task_participants.current_conclusion_id"))
            .transpose()?,
        joined_at: from_micros(joined)?,
        departed_at: departed.map(from_micros).transpose()?,
        retired_at: retired.map(from_micros).transpose()?,
    })
    .map_err(domain_internal)
}

fn persist_participant(
    transaction: &Transaction<'_>,
    participant: &TaskParticipant,
    now: OffsetDateTime,
) -> Result<()> {
    let record = participant.to_record();
    let changed = transaction.execute(
        "UPDATE task_participants SET state = ?2, version = ?3,
                current_conclusion_id = ?4, departed_at = ?5, retired_at = ?6,
                updated_at = ?7
         WHERE id = ?1 AND version = ?8",
        params![
            record.id.to_string(),
            record.state.as_str(),
            record.version,
            record.current_conclusion_id.map(|id| id.to_string()),
            record.departed_at.map(unix_micros),
            record.retired_at.map(unix_micros),
            unix_micros(now),
            record.version - 1,
        ],
    )?;
    if changed != 1 {
        return Err(SessionBridgeError::domain(
            ErrorCode::ParticipantVersionChanged,
            "TaskParticipant changed concurrently",
        ));
    }
    Ok(())
}

fn participant_basis(
    task: &TaskHead,
    participant: &TaskParticipant,
    authenticated: &AuthenticatedAgent,
) -> ParticipantMutationBasis {
    ParticipantMutationBasis {
        task_id: task.task_id,
        task_phase: task.phase,
        completion_epoch: task.completion_epoch,
        expected_participant_version: participant.version(),
        session_ended: authenticated.session.ended_at.is_some(),
    }
}

fn load_participant_work(
    connection: &Connection,
    task_id: TaskId,
    session_id: SessionId,
    now: OffsetDateTime,
) -> Result<ParticipantWorkState> {
    let now_micros = unix_micros(now);
    Ok(ParticipantWorkState {
        active_claims: count_u32(
            connection,
            "SELECT COUNT(*) FROM work_claims
             WHERE task_id = ?1 AND session_id = ?2 AND closed_at IS NULL AND ?3 = ?3",
            task_id,
            session_id,
            now_micros,
        )?,
        active_plan_or_implement_intents: count_u32(
            connection,
            "SELECT COUNT(*) FROM agent_intents
             WHERE task_id = ?1 AND session_id = ?2 AND cleared_at IS NULL
               AND expires_at > ?3 AND kind IN ('plan', 'implement')",
            task_id,
            session_id,
            now_micros,
        )?,
        unsubmitted_attempts: count_u32(
            connection,
            "SELECT COUNT(*) FROM work_attempts
             WHERE task_id = ?1 AND session_id = ?2 AND status = 'active' AND ?3 = ?3",
            task_id,
            session_id,
            now_micros,
        )?,
        claim_or_attempt_writable_reservations: count_u32(
            connection,
            "SELECT COUNT(*) FROM resource_reservations r
             WHERE r.task_id = ?1 AND r.owner_type = 'claim' AND r.released_at IS NULL
               AND r.mode IN ('write', 'exclusive')
               AND EXISTS (SELECT 1 FROM work_claims c WHERE c.id = r.owner_id AND c.session_id = ?2)
               AND ?3 = ?3",
            task_id,
            session_id,
            now_micros,
        )?,
        directed_blocking_requests: count_u32(
            connection,
            "SELECT COUNT(*) FROM coordination_requests
             WHERE task_id = ?1 AND assigned_session_id = ?2 AND blocking = 1
               AND status IN ('open', 'acknowledged') AND ?3 = ?3",
            task_id,
            session_id,
            now_micros,
        )?,
    })
}

fn count_u32(
    connection: &Connection,
    sql: &str,
    task_id: TaskId,
    session_id: SessionId,
    now_micros: i64,
) -> Result<u32> {
    let count: i64 = connection.query_row(
        sql,
        params![task_id.to_string(), session_id.to_string(), now_micros],
        |row| row.get(0),
    )?;
    u32::try_from(count).map_err(|_| {
        SessionBridgeError::domain(ErrorCode::Database, "participant work count overflow")
    })
}

fn verify_conclusion_basis(task: &TaskHead, request: &ConcludeRequest) -> Result<()> {
    if task.phase != TaskPhase::Active {
        return Err(SessionBridgeError::domain(
            if task.phase == TaskPhase::Paused {
                ErrorCode::TaskPaused
            } else {
                ErrorCode::TaskNotActive
            },
            format!("cannot conclude while Task is {}", task.phase),
        ));
    }
    if request.basis_completion_revision != task.completion_revision {
        return Err(SessionBridgeError::domain(
            ErrorCode::ConclusionStale,
            "completion revision changed after the Agent's scan",
        )
        .with_details(serde_json::json!({
            "expected_completion_revision": request.basis_completion_revision,
            "actual_completion_revision": task.completion_revision,
        })));
    }
    if request.basis_plan_version != task.plan_version {
        return Err(SessionBridgeError::domain(
            ErrorCode::PlanChanged,
            "plan version changed after the Agent's scan",
        ));
    }
    if request.basis_event_cursor < 0 {
        return Err(SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            "basis event cursor cannot be negative",
        ));
    }
    Ok(())
}

fn task_health<C: Clock>(
    connection: &Connection,
    context: TaskCompletionContext<'_>,
    clock: C,
) -> Result<TaskHealth> {
    TaskService::new(clock).health(
        connection,
        WorkspaceBinding {
            id: context.workspace_id,
            repository_identity: context.repository_instance_id,
        },
        context.task_id,
    )
}

fn signal_snapshot(health: &TaskHealth) -> Result<CompletionSignalSnapshot> {
    let value = serde_json::to_value(health)?;
    CompletionSignalSnapshot::try_from(value).map_err(domain_internal)
}

fn completion_warnings(health: &TaskHealth) -> Vec<CompletionWarning> {
    health
        .completion_signal_warnings
        .iter()
        .map(|warning| CompletionWarning {
            code: warning.code.clone(),
            entity_id: None,
            message: format!("{} signal(s)", warning.count),
        })
        .collect()
}

fn expire_invitations(
    transaction: &Transaction<'_>,
    context: TaskCompletionContext<'_>,
    now: OffsetDateTime,
) -> Result<Vec<String>> {
    let now_micros = unix_micros(now);
    let invitations = transaction
        .prepare(
            "SELECT id, state FROM agent_invitations
             WHERE task_id = ?1 AND (
               (state IN ('pending', 'reserved') AND expires_at <= ?2) OR
               (state = 'prepared' AND prepare_expires_at <= ?2)
             ) ORDER BY id",
        )?
        .query_map(params![context.task_id.to_string(), now_micros], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if invitations.is_empty() {
        return Ok(Vec::new());
    }
    transaction.execute(
        "UPDATE agent_invitations
         SET state = CASE WHEN state = 'prepared' THEN 'abandoned' ELSE 'expired' END,
             reserved_by_request_id = NULL, reservation_expires_at = NULL, updated_at = ?2
         WHERE task_id = ?1 AND (
           (state IN ('pending', 'reserved') AND expires_at <= ?2) OR
           (state = 'prepared' AND prepare_expires_at <= ?2)
         )",
        params![context.task_id.to_string(), now_micros],
    )?;
    transaction.execute(
        "UPDATE tasks SET completion_revision = completion_revision + 1, updated_at = ?2
         WHERE id = ?1",
        params![context.task_id.to_string(), now_micros],
    )?;
    for (id, previous_state) in &invitations {
        let (event_type, resulting_state) = if previous_state == "prepared" {
            ("invitation.abandoned", "abandoned")
        } else {
            ("invitation.expired", "expired")
        };
        events::append(
            transaction,
            context.workspace_id,
            Some(&context.task_id.to_string()),
            event_type,
            EventActor {
                kind: "system",
                id: None,
            },
            EventEntity {
                kind: "invitation",
                id: Some(id),
            },
            &serde_json::json!({
                "invitation_id": id,
                "previous_state": previous_state,
                "state": resulting_state,
            }),
            now,
        )?;
    }
    Ok(invitations.into_iter().map(|(id, _)| id).collect())
}

fn load_cohort(
    connection: &Connection,
    task: &TaskHead,
    warnings: &[CompletionWarning],
    now: OffsetDateTime,
) -> Result<CompletionCohort> {
    let head = CompletionHead::try_new(
        task.task_id,
        task.completion_epoch,
        task.completion_revision,
    )
    .map_err(domain_internal)?;
    let participant_ids = connection
        .prepare(
            "SELECT id FROM task_participants WHERE task_id = ?1 AND completion_epoch = ?2 ORDER BY id",
        )?
        .query_map(params![task.task_id.to_string(), task.completion_epoch], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut participants = Vec::new();
    for id in participant_ids {
        let participant_id: ParticipantId = parse_id(&id, "task_participants.id")?;
        let participant = load_participant(connection, participant_id)?;
        let session_id = participant.session_id();
        let last_seen: i64 = connection.query_row(
            "SELECT last_seen_at FROM agent_sessions WHERE id = ?1",
            [session_id.to_string()],
            |row| row.get(0),
        )?;
        let is_stale = matches!(
            participant.state(),
            ParticipantState::Active | ParticipantState::Waiting | ParticipantState::Blocked
        ) && last_seen
            <= unix_micros(now - crate::app::task::PARTICIPANT_STALE_AFTER);
        let open_work = load_participant_work(connection, task.task_id, session_id, now)?;
        let current_conclusion = participant
            .current_conclusion_id()
            .map(|id| load_conclusion(connection, id))
            .transpose()?;
        participants.push(CompletionCohortParticipant {
            participant,
            current_conclusion,
            is_stale,
            open_work,
        });
    }
    let invitations = connection
        .prepare(
            "SELECT id, participant_kind, state, blocks_completion
             FROM agent_invitations WHERE task_id = ?1 AND completion_epoch = ?2 ORDER BY id",
        )?
        .query_map(
            params![task.task_id.to_string(), task.completion_epoch],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .map(|(id, kind, state, blocks)| {
            Ok(CompletionCohortInvitation {
                id: parse_id(&id, "agent_invitations.id")?,
                task_id: task.task_id,
                completion_epoch: task.completion_epoch,
                participant_kind: parse_enum(&kind, "agent_invitations.participant_kind")?,
                state: parse_enum(&state, "agent_invitations.state")?,
                blocks_completion: blocks != 0,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    CompletionCohort::try_new(CompletionCohortInput {
        head,
        task_phase: task.phase,
        completion_policy: task.completion_policy,
        participants,
        invitations,
        signal_warnings: warnings.to_vec(),
    })
    .map_err(domain_internal)
}

fn load_conclusion(connection: &Connection, id: ConclusionId) -> Result<AgentConclusion> {
    #[allow(clippy::type_complexity)]
    let row: (
        String,
        i64,
        String,
        i64,
        i64,
        i64,
        String,
        String,
        String,
        i64,
    ) = connection.query_row(
        "SELECT participant_id, generation, outcome, basis_completion_revision,
                    basis_event_cursor, basis_plan_version, assessment_json,
                    signal_snapshot_json, warning_snapshot_json, created_at
             FROM agent_conclusions WHERE id = ?1",
        [id.to_string()],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
            ))
        },
    )?;
    let task_id: String = connection.query_row(
        "SELECT task_id FROM agent_conclusions WHERE id = ?1",
        [id.to_string()],
        |row| row.get(0),
    )?;
    let participant_id: ParticipantId = parse_id(&row.0, "agent_conclusions.participant_id")?;
    let participant_epoch: i64 = connection.query_row(
        "SELECT completion_epoch FROM task_participants WHERE id = ?1",
        [participant_id.to_string()],
        |row| row.get(0),
    )?;
    let assessment: CompletionAssessment = serde_json::from_str(&row.6)?;
    let signal_value: serde_json::Value = serde_json::from_str(&row.7)?;
    let signal_snapshot =
        CompletionSignalSnapshot::try_from(signal_value).map_err(domain_internal)?;
    let warnings: Vec<CompletionWarning> = serde_json::from_str(&row.8)?;
    let conclusion = AgentConclusion::try_new(
        AgentConclusionInput {
            id,
            participant_id,
            generation: row.1,
            basis: CompletionBasis {
                task_id: parse_id(&task_id, "agent_conclusions.task_id")?,
                completion_epoch: participant_epoch,
                completion_revision: row.3,
            },
            basis_event_cursor: row.4,
            basis_plan_version: row.5,
            assessment_schema_version: 1,
            assessment,
            signal_snapshot,
            warning_snapshot: warnings,
            created_at: from_micros(row.9)?,
        },
        CompletionHead::try_new(
            parse_id(&task_id, "agent_conclusions.task_id")?,
            participant_epoch,
            row.3,
        )
        .map_err(domain_internal)?,
    )
    .map_err(map_conclusion_error)?;
    let stored_outcome: ConclusionOutcome = parse_enum(&row.2, "agent_conclusions.outcome")?;
    if conclusion.outcome() != stored_outcome {
        return Err(SessionBridgeError::domain(
            ErrorCode::Database,
            "stored conclusion outcome does not match its assessment",
        ));
    }
    Ok(conclusion)
}

fn existing_completion(
    connection: &Connection,
    task_id: TaskId,
    epoch: i64,
) -> Result<Option<String>> {
    Ok(connection
        .query_row(
            "SELECT id FROM task_completions WHERE task_id = ?1 AND completion_epoch = ?2",
            params![task_id.to_string(), epoch],
            |row| row.get(0),
        )
        .optional()?)
}

fn load_assurance_flags(
    connection: &Connection,
    task_id: TaskId,
    epoch: i64,
) -> Result<Vec<AssuranceFlag>> {
    let values = connection
        .prepare(
            "SELECT f.flag FROM task_completion_assurance_flags f
             JOIN task_completions c ON c.id = f.task_completion_id
             WHERE c.task_id = ?1 AND c.completion_epoch = ?2 ORDER BY f.flag",
        )?
        .query_map(params![task_id.to_string(), epoch], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    values
        .into_iter()
        .map(|value| parse_enum(&value, "task_completion_assurance_flags.flag"))
        .collect()
}

fn increment_completion_revision(
    transaction: &Transaction<'_>,
    task_id: TaskId,
    expected: i64,
    now: OffsetDateTime,
) -> Result<i64> {
    let changed = transaction.execute(
        "UPDATE tasks SET completion_revision = completion_revision + 1, updated_at = ?3
         WHERE id = ?1 AND completion_revision = ?2",
        params![task_id.to_string(), expected, unix_micros(now)],
    )?;
    if changed != 1 {
        return Err(SessionBridgeError::domain(
            ErrorCode::ConclusionStale,
            "Task completion revision changed concurrently",
        ));
    }
    Ok(expected + 1)
}

#[allow(clippy::too_many_arguments)]
fn append_agent_event(
    transaction: &Transaction<'_>,
    context: &SessionContext,
    authenticated: &AuthenticatedAgent,
    event_type: &str,
    entity_type: &str,
    entity_id: &str,
    payload: &serde_json::Value,
    now: OffsetDateTime,
) -> Result<i64> {
    let session_id = authenticated.session.session_id.to_string();
    events::append(
        transaction,
        context.workspace_id,
        Some(&context.task_id.to_string()),
        event_type,
        EventActor {
            kind: "session",
            id: Some(&session_id),
        },
        EventEntity {
            kind: entity_type,
            id: Some(entity_id),
        },
        payload,
        now,
    )
}

fn parse_id<T: std::str::FromStr>(value: &str, field: &str) -> Result<T>
where
    T::Err: std::fmt::Display,
{
    value.parse().map_err(|error| {
        SessionBridgeError::domain(
            ErrorCode::Database,
            format!("database field {field} contains an invalid ID: {error}"),
        )
    })
}

fn parse_enum<T: std::str::FromStr<Err = String>>(value: &str, field: &str) -> Result<T> {
    value.parse().map_err(|error| {
        SessionBridgeError::domain(
            ErrorCode::Database,
            format!("database field {field} is invalid: {error}"),
        )
    })
}

fn from_micros(value: i64) -> Result<OffsetDateTime> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(value) * 1_000).map_err(|error| {
        SessionBridgeError::domain(
            ErrorCode::Database,
            format!("database timestamp is invalid: {error}"),
        )
    })
}

fn validate_request_id(value: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        return Err(SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            "request ID is empty, too long, or contains control characters",
        ));
    }
    Ok(())
}

fn map_participant_error(error: crate::domain::ParticipantError) -> SessionBridgeError {
    use crate::domain::ParticipantError;
    let code = match error {
        ParticipantError::ParticipantVersionChanged { .. } => ErrorCode::ParticipantVersionChanged,
        ParticipantError::CompletionEpochChanged { .. } => ErrorCode::CompletionEpochChanged,
        ParticipantError::TaskNotActive {
            phase: TaskPhase::Paused,
        } => ErrorCode::TaskPaused,
        ParticipantError::TaskNotActive { .. } => ErrorCode::TaskNotActive,
        ParticipantError::SessionHasActiveWork { .. } => ErrorCode::SessionHasActiveWork,
        ParticipantError::WaitResponsibilityRequired => ErrorCode::WaitNotAllowed,
        ParticipantError::ObserverCannotPerform { .. } => ErrorCode::CapabilityNotGranted,
        ParticipantError::InvalidTransition {
            from: ParticipantState::Blocked,
            ..
        } => ErrorCode::ParticipantBlocked,
        ParticipantError::InvalidTransition {
            from: ParticipantState::Departed,
            ..
        } => ErrorCode::ParticipantDeparted,
        ParticipantError::InvalidTransition {
            from: ParticipantState::Retired,
            ..
        } => ErrorCode::ParticipantRetired,
        ParticipantError::InvalidTransition {
            from: ParticipantState::Concluded,
            ..
        } => ErrorCode::SessionAlreadyConcluded,
        _ => ErrorCode::InvalidInput,
    };
    SessionBridgeError::domain(code, error.to_string())
}

fn map_conclusion_error(error: crate::domain::ConclusionValidationError) -> SessionBridgeError {
    use crate::domain::{CompletionCasError, ConclusionValidationError};
    let code = match &error {
        ConclusionValidationError::Cas(CompletionCasError::ConclusionStale { .. }) => {
            ErrorCode::ConclusionStale
        }
        ConclusionValidationError::Cas(CompletionCasError::CompletionEpochChanged { .. }) => {
            ErrorCode::CompletionEpochChanged
        }
        _ => ErrorCode::InvalidInput,
    };
    SessionBridgeError::domain(code, error.to_string())
}

fn domain_internal(error: impl std::fmt::Display) -> SessionBridgeError {
    SessionBridgeError::domain(ErrorCode::Internal, error.to_string())
}
