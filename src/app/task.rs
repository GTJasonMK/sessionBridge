use std::{path::PathBuf, str::FromStr};

use rusqlite::{Connection, OptionalExtension, Params, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

use crate::{
    Result,
    app::{await_service, events, idempotency},
    clock::{Clock, SystemClock},
    domain::{
        AutonomyPolicy, CompletionPolicy, CriterionId, CriterionKind, CriterionSource,
        EvaluationMode, ParticipantId, Task, TaskCriterion, TaskId, TaskPhase, Workspace,
        WorkspaceId,
    },
    error::{ErrorCode, SessionBridgeError},
    workspace::LocatedWorkspace,
};

pub const RUNTIME_STALL_AFTER: Duration = Duration::minutes(10);
pub const PARTICIPANT_STALE_AFTER: Duration = Duration::minutes(10);
pub const PARTICIPANT_RETIREMENT_AFTER: Duration = Duration::minutes(30);

pub fn cutoff_micros(now_micros: i64, duration: Duration) -> i64 {
    let delta = i64::try_from(duration.whole_microseconds()).unwrap_or(i64::MAX);
    now_micros.saturating_sub(delta)
}

const USER_ACTOR_TYPE: &str = "user";

const DEFAULT_DONE_CHECKLIST: [(&str, EvaluationMode); 5] = [
    (
        "All required requirements are verified or integrated.",
        EvaluationMode::PositiveEvidence,
    ),
    (
        "All required leaf work items are accepted.",
        EvaluationMode::PositiveEvidence,
    ),
    (
        "No required gaps, blockers, invalidations, or unresolved high-risk decisions remain.",
        EvaluationMode::ContinuousGuard,
    ),
    (
        "No active claims or pending change sets remain.",
        EvaluationMode::ContinuousGuard,
    ),
    (
        "At least one task-level integration verification evidence has passed.",
        EvaluationMode::PositiveEvidence,
    ),
];

#[derive(Debug, Clone, Copy)]
pub struct WorkspaceBinding<'a> {
    pub id: WorkspaceId,
    pub repository_identity: &'a str,
}

impl<'a> From<&'a LocatedWorkspace> for WorkspaceBinding<'a> {
    fn from(workspace: &'a LocatedWorkspace) -> Self {
        Self {
            id: workspace.config.project_id,
            repository_identity: &workspace.paths.repository_instance_id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishTaskRequest {
    pub title: String,
    pub objective: String,
    #[serde(default)]
    pub constraints: Vec<String>,
    #[serde(default)]
    pub non_goals: Vec<String>,
    #[serde(default)]
    pub done_conditions: Vec<String>,
    #[serde(default)]
    pub autonomy_policy: AutonomyPolicy,
    #[serde(default)]
    pub completion_policy: CompletionPolicy,
    pub target_branch: Option<String>,
    pub request_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TaskQuery {
    pub task_id: TaskId,
    pub workspace: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenTaskSummary {
    pub id: TaskId,
    pub title: String,
    pub phase: TaskPhase,
    #[serde(with = "crate::domain::time_serde")]
    pub created_at: OffsetDateTime,
}

pub fn resolve_task_reference(
    connection: &Connection,
    workspace_id: WorkspaceId,
    reference: &str,
) -> Result<TaskId> {
    if reference != "current" {
        return TaskId::from_str(reference).map_err(|error| {
            SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                format!("invalid task ID or reference: {error}"),
            )
        });
    }

    let mut statement = connection.prepare(
        "SELECT id, title, phase, created_at
         FROM tasks
         WHERE workspace_id = ?1 AND phase IN ('active', 'paused')
         ORDER BY created_at, id",
    )?;
    let rows = statement.query_map([workspace_id.to_string()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    let mut tasks = Vec::new();
    for row in rows {
        let (id, title, phase, created_at) = row?;
        tasks.push(OpenTaskSummary {
            id: TaskId::from_str(&id).map_err(domain_internal)?,
            title,
            phase: TaskPhase::from_str(&phase).map_err(domain_internal)?,
            created_at: OffsetDateTime::from_unix_timestamp_nanos(i128::from(created_at) * 1_000)
                .map_err(domain_internal)?,
        });
    }
    match tasks.as_slice() {
        [] => Err(SessionBridgeError::domain(
            ErrorCode::NotFound,
            "no active or paused Task exists in this workspace",
        )),
        [task] => Ok(task.id),
        _ => Err(SessionBridgeError::domain(
            ErrorCode::CurrentTaskAmbiguous,
            "current Task is ambiguous because multiple active or paused Tasks exist",
        )
        .with_details(serde_json::json!({ "tasks": tasks }))),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishTaskResult {
    pub task: Task,
    pub criteria: Vec<TaskCriterion>,
    pub event_sequence: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDetails {
    pub task: Task,
    pub criteria: Vec<TaskCriterion>,
    #[serde(with = "crate::domain::time_serde::option")]
    pub paused_at: Option<OffsetDateTime>,
    #[serde(with = "crate::domain::time_serde::option")]
    pub completed_at: Option<OffsetDateTime>,
    #[serde(with = "crate::domain::time_serde::option")]
    pub cancelled_at: Option<OffsetDateTime>,
    #[serde(with = "crate::domain::time_serde::option")]
    pub archived_at: Option<OffsetDateTime>,
    pub event_cursor: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskMutationRequest {
    pub task_id: TaskId,
    pub reason: Option<String>,
    pub request_id: Option<String>,
}

impl TaskMutationRequest {
    pub fn new(task_id: TaskId) -> Self {
        Self {
            task_id,
            reason: None,
            request_id: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskLifecycleEffects {
    pub claims_suspended: i64,
    pub claim_leases_extended: i64,
    pub claims_revoked: i64,
    pub attempts_abandoned: i64,
    pub plan_changes_superseded: i64,
    pub invitations_revoked: i64,
    pub reservations_released: i64,
    pub intents_cleared: i64,
    pub coordination_requests_cancelled: i64,
    pub blockers_cancelled: i64,
    pub retirements_cancelled: i64,
    pub participants_superseded: i64,
    pub participants_migrated: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskMutationResult {
    pub task: Task,
    pub previous_phase: TaskPhase,
    pub event_sequence: i64,
    pub effects: TaskLifecycleEffects,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddCriterionRequest {
    pub task_id: TaskId,
    pub kind: CriterionKind,
    pub content: String,
    pub required: bool,
    pub evaluation_mode: EvaluationMode,
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditCriterionRequest {
    pub task_id: TaskId,
    pub criterion_id: CriterionId,
    pub content: String,
    pub evaluation_mode: Option<EvaluationMode>,
    pub reason: String,
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CriterionMutationResult {
    pub task: Task,
    pub criterion: TaskCriterion,
    pub previous_completion_epoch: i64,
    pub participants_superseded: i64,
    pub participants_migrated: i64,
    pub invitations_rebound: i64,
    pub event_sequence: i64,
}

#[derive(Debug, Clone, Copy)]
pub struct TaskHealthOptions {
    pub participant_stale_after: Duration,
}

impl Default for TaskHealthOptions {
    fn default() -> Self {
        Self {
            participant_stale_after: PARTICIPANT_STALE_AFTER,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskHealthSignals {
    pub integration_integrity_drift: i64,
    pub integration_integrity_unobservable: i64,
    pub required_deliverables_open: i64,
    pub required_deliverables_unmapped: i64,
    pub required_deliverables_unverified: i64,
    pub uncovered_user_criteria: i64,
    pub uncovered_required_requirements: i64,
    pub required_leaf_items_not_accepted: i64,
    pub open_required_gaps: i64,
    pub open_blockers: i64,
    pub open_invalidations: i64,
    pub open_completion_challenges: i64,
    pub active_claims: i64,
    pub stale_or_reclaimable_claims: i64,
    pub active_planning_intents: i64,
    pub ready_unclaimed_items: i64,
    pub proposed_work_items: i64,
    pub pending_reviews: i64,
    pub pending_integration: i64,
    pub pending_plan_changes: i64,
    pub unresolved_decisions: i64,
    pub pending_approvals: i64,
    pub open_blocking_coordination_requests: i64,
    pub pending_blocking_invitations: i64,
    pub stale_invitation_preparations: i64,
    pub stale_join_reservations: i64,
    pub active_participants: i64,
    pub waiting_participants: i64,
    pub concluded_participants: i64,
    pub blocked_participants: i64,
    pub departed_participants: i64,
    pub stale_participants: i64,
    pub observer_participants: i64,
    pub worker_participants: i64,
    pub pending_participant_retirements: i64,
    pub passed_task_integration_evidence: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionSignalSeverity {
    Info,
    Warning,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionSignalWarning {
    pub code: String,
    pub severity: CompletionSignalSeverity,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskHealth {
    pub task: Task,
    pub signals: TaskHealthSignals,
    pub planning_required: bool,
    pub completion_cohort_open: bool,
    pub completed_with_warnings: bool,
    pub single_agent_assurance: bool,
    pub no_progress_possible: bool,
    pub completion_signal_warnings: Vec<CompletionSignalWarning>,
    pub event_cursor: i64,
}

#[derive(Debug, Clone)]
pub struct TaskService<C = SystemClock> {
    clock: C,
}

impl Default for TaskService<SystemClock> {
    fn default() -> Self {
        Self::new(SystemClock)
    }
}

impl<C: Clock> TaskService<C> {
    pub fn new(clock: C) -> Self {
        Self { clock }
    }

    pub fn verify_workspace(
        &self,
        connection: &Connection,
        binding: WorkspaceBinding<'_>,
    ) -> Result<Workspace> {
        require_workspace(connection, binding)
    }

    pub fn publish(
        &self,
        connection: &mut Connection,
        binding: WorkspaceBinding<'_>,
        request: PublishTaskRequest,
    ) -> Result<PublishTaskResult> {
        let request = CanonicalPublishRequest::try_from(request)?;
        let now = self.clock.now();
        let now_micros = checked_unix_micros(now)?;
        let workspace_actor_id = binding.id.to_string();
        let idempotency_payload = PublishIdempotencyPayload {
            operation: TaskOperation::Publish.as_str(),
            request: &request,
        };
        let payload_hash = idempotency::hash_payload(&idempotency_payload)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_workspace(&transaction, binding)?;

        if let Some(request_id) = request.request_id.as_deref() {
            let key = idempotency_key(&workspace_actor_id, request_id);
            if let Some(stored) =
                idempotency::replay::<PublishTaskResult>(&transaction, key, &payload_hash)?
            {
                transaction.commit()?;
                return Ok(stored.value);
            }
        }

        let task_id = TaskId::new();
        transaction.execute(
            "INSERT INTO tasks (
                id, workspace_id, title, phase, autonomy_policy, completion_policy,
                target_branch, plan_version, completion_epoch, completion_revision,
                version, created_at, updated_at
             ) VALUES (?1, ?2, ?3, 'active', ?4, ?5, ?6, 0, 1, 0, 0, ?7, ?7)",
            params![
                task_id.to_string(),
                binding.id.to_string(),
                request.title,
                request.autonomy_policy.as_str(),
                request.completion_policy.as_str(),
                request.target_branch,
                now_micros,
            ],
        )?;

        let criteria = create_initial_criteria(&transaction, task_id, &request, now)?;
        let task = Task {
            id: task_id,
            workspace_id: binding.id,
            title: request.title.clone(),
            phase: TaskPhase::Active,
            autonomy_policy: request.autonomy_policy,
            completion_policy: request.completion_policy,
            target_branch: request.target_branch.clone(),
            plan_version: 0,
            completion_epoch: 1,
            completion_revision: 0,
            version: 0,
            created_at: now,
            updated_at: now,
        };
        let event_sequence = events::append(
            &transaction,
            binding.id,
            Some(&task_id.to_string()),
            TaskOperation::Publish.event_type(),
            events::EventActor {
                kind: USER_ACTOR_TYPE,
                id: Some(&workspace_actor_id),
            },
            events::EventEntity {
                kind: "task",
                id: Some(&task_id.to_string()),
            },
            &serde_json::json!({
                "title": request.title,
                "criteria_count": criteria.len(),
                "completion_epoch": task.completion_epoch,
                "completion_revision": task.completion_revision,
            }),
            now,
        )?;
        let result = PublishTaskResult {
            task,
            criteria,
            event_sequence,
        };

        if let Some(request_id) = request.request_id.as_deref() {
            idempotency::record(
                &transaction,
                idempotency_key(&workspace_actor_id, request_id),
                Some(&task_id.to_string()),
                &payload_hash,
                &result,
                Some(event_sequence),
                now,
            )?;
        }
        transaction.commit()?;
        Ok(result)
    }

    pub fn show(
        &self,
        connection: &Connection,
        binding: WorkspaceBinding<'_>,
        task_id: TaskId,
    ) -> Result<TaskDetails> {
        require_workspace(connection, binding)?;
        load_task_details(connection, binding.id, task_id)
    }

    pub fn health(
        &self,
        connection: &Connection,
        binding: WorkspaceBinding<'_>,
        task_id: TaskId,
    ) -> Result<TaskHealth> {
        self.health_with_options(connection, binding, task_id, TaskHealthOptions::default())
    }

    pub fn health_with_options(
        &self,
        connection: &Connection,
        binding: WorkspaceBinding<'_>,
        task_id: TaskId,
        options: TaskHealthOptions,
    ) -> Result<TaskHealth> {
        require_workspace(connection, binding)?;
        let details = load_task_details(connection, binding.id, task_id)?;
        let now_micros = checked_unix_micros(self.clock.now())?;
        let stale_after = checked_duration_micros(options.participant_stale_after)?;
        if stale_after <= 0 {
            return Err(invalid_input(
                "participant_stale_after",
                "participant stale duration must be positive",
            ));
        }
        let stale_before = now_micros.checked_sub(stale_after).ok_or_else(|| {
            invalid_input(
                "participant_stale_after",
                "participant stale threshold is outside the supported timestamp range",
            )
        })?;
        let task_id_text = task_id.to_string();
        let epoch = details.task.completion_epoch;

        let signals = TaskHealthSignals {
            integration_integrity_drift: query_count(
                connection,
                "SELECT count(*) FROM task_integration_integrity
                 WHERE task_id=?1 AND status='drift'",
                [&task_id_text],
            )?,
            integration_integrity_unobservable: query_count(
                connection,
                "SELECT CASE WHEN EXISTS(
                    SELECT 1 FROM change_sets WHERE task_id=?1 AND integration_state='integrated'
                 ) AND (
                    NOT EXISTS(SELECT 1 FROM task_integration_integrity WHERE task_id=?1)
                    OR EXISTS(SELECT 1 FROM task_integration_integrity
                              WHERE task_id=?1 AND status='unobservable')
                 ) THEN 1 ELSE 0 END",
                [&task_id_text],
            )?,
            required_deliverables_open: query_count(
                connection,
                "SELECT count(*) FROM deliverables
                 WHERE task_id = ?1 AND required = 1 AND status NOT IN ('verified', 'waived')",
                [&task_id_text],
            )?,
            required_deliverables_unmapped: query_count(
                connection,
                "SELECT count(*) FROM deliverables d
                 WHERE d.task_id = ?1 AND d.required = 1 AND d.status <> 'waived'
                   AND NOT EXISTS (SELECT 1 FROM deliverable_coverage c WHERE c.deliverable_id = d.id)",
                [&task_id_text],
            )?,
            required_deliverables_unverified: query_count(
                connection,
                "SELECT count(*) FROM deliverables d
                 WHERE d.task_id = ?1 AND d.required = 1 AND d.status <> 'waived'
                   AND NOT EXISTS (
                     SELECT 1 FROM deliverable_evidence de JOIN evidence e ON e.id = de.evidence_id
                     WHERE de.deliverable_id = d.id AND e.result = 'passed'
                   )",
                [&task_id_text],
            )?,
            uncovered_user_criteria: query_count(
                connection,
                "SELECT count(*)
                 FROM task_criteria criterion
                 WHERE criterion.task_id = ?1
                   AND criterion.source = 'user'
                   AND criterion.required = 1
                   AND NOT EXISTS (
                       SELECT 1
                       FROM criterion_coverage coverage
                       WHERE coverage.criterion_id = criterion.id
                         AND coverage.status IN ('covered', 'verified')
                   )",
                [&task_id_text],
            )?,
            uncovered_required_requirements: query_count(
                connection,
                "SELECT count(*)
                 FROM requirements requirement
                 WHERE requirement.task_id = ?1
                   AND requirement.required = 1
                   AND requirement.status = 'active'
                   AND NOT EXISTS (
                       SELECT 1
                       FROM requirement_coverage coverage
                       WHERE coverage.requirement_id = requirement.id
                         AND coverage.coverage_state IN ('verified', 'integrated')
                   )",
                [&task_id_text],
            )?,
            required_leaf_items_not_accepted: query_count(
                connection,
                "SELECT count(*)
                 FROM work_items item
                 WHERE item.task_id = ?1
                   AND item.required = 1
                   AND item.plan_state = 'active'
                   AND item.result_state <> 'accepted'
                   AND NOT EXISTS (
                       SELECT 1
                       FROM work_items child
                       WHERE child.task_id = item.task_id
                         AND child.parent_id = item.id
                         AND child.plan_state = 'active'
                   )",
                [&task_id_text],
            )?,
            open_required_gaps: query_count(
                connection,
                "SELECT count(*) FROM gaps
                 WHERE task_id = ?1 AND required = 1 AND status = 'open'",
                [&task_id_text],
            )?,
            open_blockers: query_count(
                connection,
                "SELECT count(*) FROM blockers WHERE task_id = ?1 AND status = 'open'",
                [&task_id_text],
            )?,
            open_invalidations: query_count(
                connection,
                "SELECT count(*) FROM invalidations WHERE task_id = ?1 AND status = 'open'",
                [&task_id_text],
            )?,
            open_completion_challenges: query_count(
                connection,
                "SELECT count(*) FROM agent_action_proposals
                 WHERE task_id=?1 AND kind='completion_challenge' AND status='proposed'",
                [&task_id_text],
            )?,
            active_claims: query_count(
                connection,
                "SELECT count(*) FROM work_claims
                 WHERE task_id = ?1 AND closed_at IS NULL AND lease_expires_at > ?2",
                params![task_id_text, now_micros],
            )?,
            stale_or_reclaimable_claims: query_count(
                connection,
                "SELECT count(*) FROM work_claims
                 WHERE task_id = ?1 AND closed_at IS NULL AND lease_expires_at <= ?2",
                params![task_id_text, now_micros],
            )?,
            active_planning_intents: query_count(
                connection,
                "SELECT count(*) FROM agent_intents
                 WHERE task_id = ?1
                   AND kind IN ('explore', 'plan')
                   AND cleared_at IS NULL
                   AND expires_at > ?2",
                params![task_id_text, now_micros],
            )?,
            ready_unclaimed_items: query_count(
                connection,
                "SELECT count(*)
                 FROM work_items item
                 WHERE item.task_id = ?1
                   AND item.plan_state = 'active'
                   AND item.result_state = 'open'
                   AND NOT EXISTS (
                       SELECT 1 FROM work_claims claim
                       WHERE claim.work_item_id = item.id
                         AND claim.mode = 'execute'
                         AND claim.closed_at IS NULL
                   )
                   AND NOT EXISTS (
                       SELECT 1
                       FROM work_relations relation
                       JOIN work_items dependency ON dependency.id = relation.to_id
                       WHERE relation.task_id = item.task_id
                         AND relation.from_id = item.id
                         AND relation.relation_type = 'depends_on'
                         AND dependency.result_state <> 'accepted'
                   )",
                [&task_id_text],
            )?,
            proposed_work_items: query_count(
                connection,
                "SELECT count(*) FROM work_items
                 WHERE task_id = ?1 AND plan_state = 'proposed'",
                [&task_id_text],
            )?,
            pending_reviews: query_count(
                connection,
                "SELECT count(*)
                 FROM work_items item
                 WHERE item.task_id = ?1
                   AND item.plan_state = 'active'
                   AND item.result_state = 'submitted'
                   AND NOT EXISTS (
                       SELECT 1 FROM review_results review
                       WHERE review.work_item_id = item.id
                         AND review.decision = 'approved'
                   )",
                [&task_id_text],
            )?,
            pending_integration: query_count(
                connection,
                "SELECT count(*) FROM change_sets
                 WHERE task_id = ?1
                   AND integration_state IN ('pending', 'integrating', 'failed')",
                [&task_id_text],
            )?,
            pending_plan_changes: query_count(
                connection,
                "SELECT count(*) FROM plan_changes
                 WHERE task_id = ?1 AND status IN ('proposed', 'pending_approval')",
                [&task_id_text],
            )?,
            unresolved_decisions: query_count(
                connection,
                "SELECT count(*) FROM decisions
                 WHERE task_id = ?1 AND status IN ('proposed', 'pending_approval')",
                [&task_id_text],
            )?,
            pending_approvals: query_count(
                connection,
                "SELECT
                    (SELECT count(*) FROM plan_changes
                     WHERE task_id = ?1 AND status = 'pending_approval') +
                    (SELECT count(*) FROM decisions
                     WHERE task_id = ?1 AND status = 'pending_approval') +
                    (SELECT count(*) FROM participant_retirements
                     WHERE task_id = ?1 AND status = 'pending_approval')",
                [&task_id_text],
            )?,
            open_blocking_coordination_requests: query_count(
                connection,
                "SELECT count(*) FROM coordination_requests
                 WHERE task_id = ?1
                   AND blocking = 1
                   AND status IN ('open', 'acknowledged')",
                [&task_id_text],
            )?,
            pending_blocking_invitations: query_count(
                connection,
                "SELECT count(*) FROM agent_invitations
                 WHERE task_id = ?1
                   AND completion_epoch = ?2
                   AND blocks_completion = 1
                   AND state IN ('prepared', 'pending', 'reserved')
                   AND expires_at > ?3",
                params![task_id_text, epoch, now_micros],
            )?,
            stale_invitation_preparations: query_count(
                connection,
                "SELECT count(*) FROM agent_invitations
                 WHERE task_id = ?1
                   AND completion_epoch = ?2
                   AND state = 'prepared'
                   AND prepare_expires_at <= ?3",
                params![task_id_text, epoch, now_micros],
            )?,
            stale_join_reservations: query_count(
                connection,
                "SELECT count(*) FROM agent_invitations
                 WHERE task_id = ?1
                   AND completion_epoch = ?2
                   AND state = 'reserved'
                   AND reservation_expires_at <= ?3",
                params![task_id_text, epoch, now_micros],
            )?,
            active_participants: participant_count(connection, &task_id_text, epoch, "active")?,
            waiting_participants: participant_count(connection, &task_id_text, epoch, "waiting")?,
            concluded_participants: participant_count(
                connection,
                &task_id_text,
                epoch,
                "concluded",
            )?,
            blocked_participants: participant_count(connection, &task_id_text, epoch, "blocked")?,
            departed_participants: participant_count(connection, &task_id_text, epoch, "departed")?,
            stale_participants: query_count(
                connection,
                "SELECT count(*)
                 FROM task_participants participant
                 JOIN agent_sessions session ON session.id = participant.session_id
                 WHERE participant.task_id = ?1
                   AND participant.completion_epoch = ?2
                   AND participant.state IN ('active', 'waiting', 'blocked')
                   AND session.ended_at IS NULL
                   AND session.last_seen_at <= ?3",
                params![task_id_text, epoch, stale_before],
            )?,
            observer_participants: query_count(
                connection,
                "SELECT count(*) FROM task_participants
                 WHERE task_id = ?1
                   AND completion_epoch = ?2
                   AND kind = 'observer'
                   AND state <> 'superseded'",
                params![task_id_text, epoch],
            )?,
            worker_participants: query_count(
                connection,
                "SELECT count(*) FROM task_participants
                 WHERE task_id = ?1
                   AND completion_epoch = ?2
                   AND kind = 'worker'
                   AND state NOT IN ('retired', 'superseded')",
                params![task_id_text, epoch],
            )?,
            pending_participant_retirements: query_count(
                connection,
                "SELECT count(*) FROM participant_retirements
                 WHERE task_id = ?1
                   AND status IN ('proposed', 'pending_approval', 'approved')",
                [&task_id_text],
            )?,
            passed_task_integration_evidence: query_count(
                connection,
                "SELECT count(*) FROM evidence
                 WHERE task_id = ?1 AND attempt_id IS NULL AND result = 'passed'",
                [&task_id_text],
            )?,
        };

        let requirement_count = query_count(
            connection,
            "SELECT count(*) FROM requirements
             WHERE task_id = ?1 AND status IN ('proposed', 'active')",
            [&task_id_text],
        )?;
        let work_item_count = query_count(
            connection,
            "SELECT count(*) FROM work_items
             WHERE task_id = ?1 AND plan_state IN ('proposed', 'active')",
            [&task_id_text],
        )?;
        let planning_required = requirement_count == 0 && work_item_count == 0;
        let open_worker_participants = signals.active_participants
            + signals.waiting_participants
            + signals.blocked_participants
            + signals.departed_participants;
        let completion_cohort_open = signals.worker_participants == 0
            || open_worker_participants > 0
            || signals.pending_blocking_invitations > 0;
        let completed_with_warnings = has_completion_assurance_flag(
            connection,
            &task_id_text,
            epoch,
            "completed_with_warnings",
        )?;
        let persisted_single_agent = has_completion_assurance_flag(
            connection,
            &task_id_text,
            epoch,
            "single_agent_assurance",
        )?;
        let single_agent_assurance = persisted_single_agent || signals.worker_participants == 1;

        let usable_active_participants = query_count(
            connection,
            "SELECT count(*)
             FROM task_participants participant
             JOIN agent_sessions session ON session.id = participant.session_id
             WHERE participant.task_id = ?1
               AND participant.completion_epoch = ?2
               AND participant.kind = 'worker'
               AND participant.state = 'active'
               AND session.ended_at IS NULL
               AND session.last_seen_at > ?3",
            params![task_id_text, epoch, stale_before],
        )?;
        let has_progress_path = signals.ready_unclaimed_items > 0
            || signals.active_claims > 0
            || signals.active_planning_intents > 0
            || signals.pending_reviews > 0
            || signals.pending_integration > 0
            || signals.pending_plan_changes > 0;
        let no_progress_possible = details.task.phase == TaskPhase::Active
            && !has_progress_path
            && usable_active_participants == 0;
        let completion_signal_warnings = completion_warnings(&signals, planning_required);

        Ok(TaskHealth {
            task: details.task,
            signals,
            planning_required,
            completion_cohort_open,
            completed_with_warnings,
            single_agent_assurance,
            no_progress_possible,
            completion_signal_warnings,
            event_cursor: details.event_cursor,
        })
    }

    pub fn pause(
        &self,
        connection: &mut Connection,
        binding: WorkspaceBinding<'_>,
        request: TaskMutationRequest,
    ) -> Result<TaskMutationResult> {
        self.mutate(connection, binding, request, TaskOperation::Pause)
    }

    pub fn add_criterion(
        &self,
        connection: &mut Connection,
        binding: WorkspaceBinding<'_>,
        mut request: AddCriterionRequest,
    ) -> Result<CriterionMutationResult> {
        request.content = normalize_required("criterion content", request.content)?;
        self.mutate_criterion(
            connection,
            binding,
            request.task_id,
            request.request_id,
            "task.criterion.add",
            serde_json::json!({
                "kind": request.kind,
                "content": request.content,
                "required": request.required,
                "evaluation_mode": request.evaluation_mode,
            }),
            move |transaction, now| {
                let criterion = TaskCriterion {
                    id: CriterionId::new(),
                    task_id: request.task_id,
                    kind: request.kind,
                    content: request.content,
                    required: request.required,
                    source: CriterionSource::User,
                    evaluation_mode: request.evaluation_mode,
                    immutable_to_agents: true,
                    version: 0,
                    created_at: now,
                    updated_at: now,
                };
                transaction.execute(
                    "INSERT INTO task_criteria (
                        id, task_id, kind, content, required, source, evaluation_mode,
                        immutable_to_agents, version, created_at, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, 'user', ?6, 1, 0, ?7, ?7)",
                    params![
                        criterion.id.to_string(),
                        criterion.task_id.to_string(),
                        criterion.kind.as_str(),
                        criterion.content,
                        i64::from(criterion.required),
                        criterion.evaluation_mode.as_str(),
                        checked_unix_micros(now)?,
                    ],
                )?;
                Ok(criterion.id)
            },
        )
    }

    pub fn edit_criterion(
        &self,
        connection: &mut Connection,
        binding: WorkspaceBinding<'_>,
        mut request: EditCriterionRequest,
    ) -> Result<CriterionMutationResult> {
        request.content = normalize_required("criterion content", request.content)?;
        request.reason = normalize_required("criterion edit reason", request.reason)?;
        self.mutate_criterion(
            connection,
            binding,
            request.task_id,
            request.request_id,
            "task.criterion.edit",
            serde_json::json!({
                "criterion_id": request.criterion_id,
                "content": request.content,
                "evaluation_mode": request.evaluation_mode,
                "reason": request.reason,
            }),
            move |transaction, now| {
                let changed = transaction.execute(
                    "UPDATE task_criteria SET content = ?3,
                            evaluation_mode = COALESCE(?4, evaluation_mode),
                            version = version + 1, updated_at = ?5
                     WHERE id = ?1 AND task_id = ?2",
                    params![
                        request.criterion_id.to_string(),
                        request.task_id.to_string(),
                        request.content,
                        request.evaluation_mode.map(|mode| mode.as_str()),
                        checked_unix_micros(now)?,
                    ],
                )?;
                if changed != 1 {
                    return Err(SessionBridgeError::domain(
                        ErrorCode::NotFound,
                        "TaskCriterion was not found",
                    ));
                }
                Ok(request.criterion_id)
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn mutate_criterion<F>(
        &self,
        connection: &mut Connection,
        binding: WorkspaceBinding<'_>,
        task_id: TaskId,
        request_id: Option<String>,
        operation: &'static str,
        payload: serde_json::Value,
        mutate: F,
    ) -> Result<CriterionMutationResult>
    where
        F: FnOnce(&Connection, OffsetDateTime) -> Result<CriterionId>,
    {
        let request_id = normalize_request_id(request_id)?;
        let now = self.clock.now();
        let now_micros = checked_unix_micros(now)?;
        let task_id_text = task_id.to_string();
        let actor_id = binding.id.to_string();
        let payload_hash = idempotency::hash_payload(&serde_json::json!({
            "operation": operation,
            "task_id": task_id,
            "payload": payload,
        }))?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_workspace(&transaction, binding)?;
        if let Some(request_id) = request_id.as_deref() {
            let key = idempotency_key(&actor_id, request_id);
            if let Some(stored) =
                idempotency::replay::<CriterionMutationResult>(&transaction, key, &payload_hash)?
            {
                transaction.commit()?;
                return Ok(stored.value);
            }
        }
        let before = load_task_row(&transaction, binding.id, task_id)?;
        if before.task.phase != TaskPhase::Active {
            return Err(task_not_active_error(before.task.phase));
        }
        let criterion_id = mutate(&transaction, now)?;
        let (participants_superseded, participants_migrated, invitations_rebound) =
            advance_active_completion_epoch(&transaction, &before, now_micros, binding.id)?;
        let after = load_task_row(&transaction, binding.id, task_id)?;
        let criterion = load_criteria(&transaction, task_id)?
            .into_iter()
            .find(|criterion| criterion.id == criterion_id)
            .ok_or_else(|| {
                SessionBridgeError::domain(
                    ErrorCode::Internal,
                    "criterion disappeared after mutation",
                )
            })?;
        let event_sequence = events::append(
            &transaction,
            binding.id,
            Some(&task_id_text),
            operation,
            events::EventActor {
                kind: USER_ACTOR_TYPE,
                id: Some(&actor_id),
            },
            events::EventEntity {
                kind: "task_criterion",
                id: Some(&criterion_id.to_string()),
            },
            &serde_json::json!({
                "criterion_id": criterion_id,
                "previous_completion_epoch": before.task.completion_epoch,
                "completion_epoch": after.task.completion_epoch,
                "plan_version": after.task.plan_version,
                "participants_superseded": participants_superseded,
                "participants_migrated": participants_migrated,
                "invitations_rebound": invitations_rebound,
            }),
            now,
        )?;
        let result = CriterionMutationResult {
            task: after.task,
            criterion,
            previous_completion_epoch: before.task.completion_epoch,
            participants_superseded,
            participants_migrated,
            invitations_rebound,
            event_sequence,
        };
        if let Some(request_id) = request_id.as_deref() {
            idempotency::record(
                &transaction,
                idempotency_key(&actor_id, request_id),
                Some(&task_id_text),
                &payload_hash,
                &result,
                Some(event_sequence),
                now,
            )?;
        }
        transaction.commit()?;
        Ok(result)
    }

    pub fn resume(
        &self,
        connection: &mut Connection,
        binding: WorkspaceBinding<'_>,
        request: TaskMutationRequest,
    ) -> Result<TaskMutationResult> {
        self.mutate(connection, binding, request, TaskOperation::Resume)
    }

    pub fn cancel(
        &self,
        connection: &mut Connection,
        binding: WorkspaceBinding<'_>,
        request: TaskMutationRequest,
    ) -> Result<TaskMutationResult> {
        self.mutate(connection, binding, request, TaskOperation::Cancel)
    }

    pub fn reopen(
        &self,
        connection: &mut Connection,
        binding: WorkspaceBinding<'_>,
        request: TaskMutationRequest,
    ) -> Result<TaskMutationResult> {
        self.mutate(connection, binding, request, TaskOperation::Reopen)
    }

    pub fn archive(
        &self,
        connection: &mut Connection,
        binding: WorkspaceBinding<'_>,
        request: TaskMutationRequest,
    ) -> Result<TaskMutationResult> {
        self.mutate(connection, binding, request, TaskOperation::Archive)
    }

    fn mutate(
        &self,
        connection: &mut Connection,
        binding: WorkspaceBinding<'_>,
        request: TaskMutationRequest,
        operation: TaskOperation,
    ) -> Result<TaskMutationResult> {
        let reason = normalize_optional("reason", request.reason)?;
        let request_id = normalize_request_id(request.request_id)?;
        let now = self.clock.now();
        let now_micros = checked_unix_micros(now)?;
        let task_id_text = request.task_id.to_string();
        let workspace_actor_id = binding.id.to_string();
        let idempotency_payload = MutationIdempotencyPayload {
            operation: operation.as_str(),
            task_id: request.task_id,
            reason: reason.as_deref(),
        };
        let payload_hash = idempotency::hash_payload(&idempotency_payload)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_workspace(&transaction, binding)?;

        if let Some(request_id) = request_id.as_deref() {
            let key = idempotency_key(&workspace_actor_id, request_id);
            if let Some(stored) =
                idempotency::replay::<TaskMutationResult>(&transaction, key, &payload_hash)?
            {
                transaction.commit()?;
                return Ok(stored.value);
            }
        }

        let before = load_task_row(&transaction, binding.id, request.task_id)?;
        let previous_phase = before.task.phase;
        let effects = apply_operation(&transaction, operation, &before, now_micros, binding.id)?;
        let after = load_task_row(&transaction, binding.id, request.task_id)?;
        let event_sequence = events::append(
            &transaction,
            binding.id,
            Some(&task_id_text),
            operation.event_type(),
            events::EventActor {
                kind: USER_ACTOR_TYPE,
                id: Some(&workspace_actor_id),
            },
            events::EventEntity {
                kind: "task",
                id: Some(&task_id_text),
            },
            &serde_json::json!({
                "previous_phase": previous_phase,
                "phase": after.task.phase,
                "reason": reason,
                "previous_completion_epoch": before.task.completion_epoch,
                "completion_epoch": after.task.completion_epoch,
                "completion_revision": after.task.completion_revision,
                "effects": effects,
            }),
            now,
        )?;
        if matches!(after.task.phase, TaskPhase::Cancelled | TaskPhase::Archived) {
            await_service::resolve_task_terminal(
                &transaction,
                binding.id,
                after.task.id,
                after.task.phase,
                event_sequence,
                now,
            )?;
            crate::app::external_wait::resolve_task_terminal(
                &transaction,
                binding.id,
                after.task.id,
                after.task.phase,
                now,
            )?;
        }
        let result = TaskMutationResult {
            task: after.task,
            previous_phase,
            event_sequence,
            effects,
        };

        if let Some(request_id) = request_id.as_deref() {
            idempotency::record(
                &transaction,
                idempotency_key(&workspace_actor_id, request_id),
                Some(&task_id_text),
                &payload_hash,
                &result,
                Some(event_sequence),
                now,
            )?;
        }
        transaction.commit()?;
        Ok(result)
    }
}

#[derive(Debug, Clone, Serialize)]
struct CanonicalPublishRequest {
    title: String,
    objective: String,
    constraints: Vec<String>,
    non_goals: Vec<String>,
    done_conditions: Vec<String>,
    autonomy_policy: AutonomyPolicy,
    completion_policy: CompletionPolicy,
    target_branch: Option<String>,
    #[serde(skip)]
    request_id: Option<String>,
}

impl TryFrom<PublishTaskRequest> for CanonicalPublishRequest {
    type Error = SessionBridgeError;

    fn try_from(request: PublishTaskRequest) -> Result<Self> {
        Ok(Self {
            title: normalize_required("title", request.title)?,
            objective: normalize_required("objective", request.objective)?,
            constraints: normalize_list("constraints", request.constraints)?,
            non_goals: normalize_list("non_goals", request.non_goals)?,
            done_conditions: normalize_list("done_conditions", request.done_conditions)?,
            autonomy_policy: request.autonomy_policy,
            completion_policy: request.completion_policy,
            target_branch: normalize_optional("target_branch", request.target_branch)?,
            request_id: normalize_request_id(request.request_id)?,
        })
    }
}

#[derive(Serialize)]
struct PublishIdempotencyPayload<'a> {
    operation: &'static str,
    request: &'a CanonicalPublishRequest,
}

#[derive(Serialize)]
struct MutationIdempotencyPayload<'a> {
    operation: &'static str,
    task_id: TaskId,
    reason: Option<&'a str>,
}

#[derive(Debug, Clone, Copy)]
enum TaskOperation {
    Publish,
    Pause,
    Resume,
    Cancel,
    Reopen,
    Archive,
}

impl TaskOperation {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Publish => "task.publish",
            Self::Pause => "task.pause",
            Self::Resume => "task.resume",
            Self::Cancel => "task.cancel",
            Self::Reopen => "task.reopen",
            Self::Archive => "task.archive",
        }
    }

    const fn event_type(self) -> &'static str {
        match self {
            Self::Publish => "task.published",
            Self::Pause => "task.paused",
            Self::Resume => "task.resumed",
            Self::Cancel => "task.cancelled",
            Self::Reopen => "task.completion_epoch_changed",
            Self::Archive => "task.archived",
        }
    }
}

#[derive(Debug)]
struct TaskRow {
    task: Task,
    paused_at: Option<OffsetDateTime>,
    completed_at: Option<OffsetDateTime>,
    cancelled_at: Option<OffsetDateTime>,
    archived_at: Option<OffsetDateTime>,
}

#[derive(Debug)]
struct RawTaskRow {
    id: String,
    workspace_id: String,
    title: String,
    phase: String,
    autonomy_policy: String,
    completion_policy: String,
    target_branch: Option<String>,
    plan_version: i64,
    completion_epoch: i64,
    completion_revision: i64,
    version: i64,
    paused_at: Option<i64>,
    completed_at: Option<i64>,
    cancelled_at: Option<i64>,
    archived_at: Option<i64>,
    created_at: i64,
    updated_at: i64,
}

fn require_workspace(connection: &Connection, binding: WorkspaceBinding<'_>) -> Result<Workspace> {
    if binding.repository_identity.trim().is_empty() {
        return Err(invalid_input(
            "repository_identity",
            "repository identity cannot be empty",
        ));
    }
    let workspace_id = binding.id.to_string();
    let row = connection
        .query_row(
            "SELECT id, name, repository_identity, created_at
             FROM workspaces WHERE id = ?1",
            [&workspace_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((stored_id, name, stored_repository_identity, created_at)) = row else {
        return Err(SessionBridgeError::domain(
            ErrorCode::WorkspaceNotFound,
            "workspace is not registered in this state database",
        )
        .with_details(serde_json::json!({
            "workspace_id": workspace_id,
            "repository_identity": binding.repository_identity,
        })));
    };
    if stored_repository_identity != binding.repository_identity {
        return Err(SessionBridgeError::domain(
            ErrorCode::WorkspaceNotFound,
            "workspace database is bound to a different repository instance",
        )
        .with_details(serde_json::json!({
            "workspace_id": workspace_id,
            "expected_repository_identity": binding.repository_identity,
            "actual_repository_identity": stored_repository_identity,
        })));
    }
    Ok(Workspace {
        id: parse_database_value("workspaces.id", &stored_id)?,
        name,
        repository_identity: stored_repository_identity,
        created_at: timestamp_from_micros("workspaces.created_at", created_at)?,
    })
}

fn load_task_details(
    connection: &Connection,
    workspace_id: WorkspaceId,
    task_id: TaskId,
) -> Result<TaskDetails> {
    let row = load_task_row(connection, workspace_id, task_id)?;
    let criteria = load_criteria(connection, task_id)?;
    let task_id_text = task_id.to_string();
    Ok(TaskDetails {
        task: row.task,
        criteria,
        paused_at: row.paused_at,
        completed_at: row.completed_at,
        cancelled_at: row.cancelled_at,
        archived_at: row.archived_at,
        event_cursor: events::latest_cursor(connection, Some(&task_id_text))?,
    })
}

fn load_task_row(
    connection: &Connection,
    workspace_id: WorkspaceId,
    task_id: TaskId,
) -> Result<TaskRow> {
    let task_id_text = task_id.to_string();
    let workspace_id_text = workspace_id.to_string();
    let raw = connection
        .query_row(
            "SELECT id, workspace_id, title, phase, autonomy_policy, completion_policy,
                    target_branch, plan_version, completion_epoch, completion_revision,
                    version, paused_at, completed_at, cancelled_at, archived_at,
                    created_at, updated_at
             FROM tasks
             WHERE id = ?1 AND workspace_id = ?2",
            params![task_id_text, workspace_id_text],
            |row| {
                Ok(RawTaskRow {
                    id: row.get(0)?,
                    workspace_id: row.get(1)?,
                    title: row.get(2)?,
                    phase: row.get(3)?,
                    autonomy_policy: row.get(4)?,
                    completion_policy: row.get(5)?,
                    target_branch: row.get(6)?,
                    plan_version: row.get(7)?,
                    completion_epoch: row.get(8)?,
                    completion_revision: row.get(9)?,
                    version: row.get(10)?,
                    paused_at: row.get(11)?,
                    completed_at: row.get(12)?,
                    cancelled_at: row.get(13)?,
                    archived_at: row.get(14)?,
                    created_at: row.get(15)?,
                    updated_at: row.get(16)?,
                })
            },
        )
        .optional()?;
    let Some(raw) = raw else {
        return Err(SessionBridgeError::domain(
            ErrorCode::NotFound,
            "task was not found in the current workspace",
        )
        .with_details(serde_json::json!({
            "task_id": task_id,
            "workspace_id": workspace_id,
        })));
    };
    task_row_from_raw(raw)
}

fn task_row_from_raw(raw: RawTaskRow) -> Result<TaskRow> {
    if raw.plan_version < 0
        || raw.completion_epoch < 1
        || raw.completion_revision < 0
        || raw.version < 0
    {
        return Err(database_value_error(
            "tasks",
            "task version counters violate their storage invariants",
            serde_json::json!({
                "plan_version": raw.plan_version,
                "completion_epoch": raw.completion_epoch,
                "completion_revision": raw.completion_revision,
                "version": raw.version,
            }),
        ));
    }
    let paused_at = optional_timestamp_from_micros("tasks.paused_at", raw.paused_at)?;
    let completed_at = optional_timestamp_from_micros("tasks.completed_at", raw.completed_at)?;
    let cancelled_at = optional_timestamp_from_micros("tasks.cancelled_at", raw.cancelled_at)?;
    let archived_at = optional_timestamp_from_micros("tasks.archived_at", raw.archived_at)?;
    Ok(TaskRow {
        task: Task {
            id: parse_database_value("tasks.id", &raw.id)?,
            workspace_id: parse_database_value("tasks.workspace_id", &raw.workspace_id)?,
            title: raw.title,
            phase: parse_database_enum("tasks.phase", &raw.phase)?,
            autonomy_policy: parse_database_enum("tasks.autonomy_policy", &raw.autonomy_policy)?,
            completion_policy: parse_database_enum(
                "tasks.completion_policy",
                &raw.completion_policy,
            )?,
            target_branch: raw.target_branch,
            plan_version: raw.plan_version,
            completion_epoch: raw.completion_epoch,
            completion_revision: raw.completion_revision,
            version: raw.version,
            created_at: timestamp_from_micros("tasks.created_at", raw.created_at)?,
            updated_at: timestamp_from_micros("tasks.updated_at", raw.updated_at)?,
        },
        paused_at,
        completed_at,
        cancelled_at,
        archived_at,
    })
}

fn load_criteria(connection: &Connection, task_id: TaskId) -> Result<Vec<TaskCriterion>> {
    let task_id_text = task_id.to_string();
    let mut statement = connection.prepare(
        "SELECT id, task_id, kind, content, required, source, evaluation_mode,
                immutable_to_agents, version, created_at, updated_at
         FROM task_criteria
         WHERE task_id = ?1
         ORDER BY CASE kind
                    WHEN 'objective' THEN 0
                    WHEN 'constraint' THEN 1
                    WHEN 'non_goal' THEN 2
                    WHEN 'done_condition' THEN 3
                    ELSE 4
                  END,
                  created_at,
                  id",
    )?;
    let rows = statement.query_map([&task_id_text], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, i64>(7)?,
            row.get::<_, i64>(8)?,
            row.get::<_, i64>(9)?,
            row.get::<_, i64>(10)?,
        ))
    })?;
    let mut criteria = Vec::new();
    for row in rows {
        let (
            id,
            task_id,
            kind,
            content,
            required,
            source,
            evaluation_mode,
            immutable_to_agents,
            version,
            created_at,
            updated_at,
        ) = row?;
        criteria.push(TaskCriterion {
            id: parse_database_value("task_criteria.id", &id)?,
            task_id: parse_database_value("task_criteria.task_id", &task_id)?,
            kind: parse_database_enum("task_criteria.kind", &kind)?,
            content,
            required: parse_database_bool("task_criteria.required", required)?,
            source: parse_database_enum("task_criteria.source", &source)?,
            evaluation_mode: parse_database_enum(
                "task_criteria.evaluation_mode",
                &evaluation_mode,
            )?,
            immutable_to_agents: parse_database_bool(
                "task_criteria.immutable_to_agents",
                immutable_to_agents,
            )?,
            version,
            created_at: timestamp_from_micros("task_criteria.created_at", created_at)?,
            updated_at: timestamp_from_micros("task_criteria.updated_at", updated_at)?,
        });
    }
    Ok(criteria)
}

fn create_initial_criteria(
    connection: &Connection,
    task_id: TaskId,
    request: &CanonicalPublishRequest,
    now: OffsetDateTime,
) -> Result<Vec<TaskCriterion>> {
    let mut criteria = Vec::new();
    criteria.push(new_criterion(
        task_id,
        CriterionKind::Objective,
        request.objective.clone(),
        CriterionSource::User,
        EvaluationMode::PositiveEvidence,
        now,
    ));
    criteria.extend(request.constraints.iter().cloned().map(|content| {
        new_criterion(
            task_id,
            CriterionKind::Constraint,
            content,
            CriterionSource::User,
            EvaluationMode::ContinuousGuard,
            now,
        )
    }));
    criteria.extend(request.non_goals.iter().cloned().map(|content| {
        new_criterion(
            task_id,
            CriterionKind::NonGoal,
            content,
            CriterionSource::User,
            EvaluationMode::ContinuousGuard,
            now,
        )
    }));
    if request.done_conditions.is_empty() {
        criteria.extend(DEFAULT_DONE_CHECKLIST.map(|(content, evaluation_mode)| {
            new_criterion(
                task_id,
                CriterionKind::DoneCondition,
                content.to_owned(),
                CriterionSource::System,
                evaluation_mode,
                now,
            )
        }));
    } else {
        criteria.extend(request.done_conditions.iter().cloned().map(|content| {
            new_criterion(
                task_id,
                CriterionKind::DoneCondition,
                content,
                CriterionSource::User,
                EvaluationMode::PositiveEvidence,
                now,
            )
        }));
    }

    let now_micros = checked_unix_micros(now)?;
    for criterion in &criteria {
        connection.execute(
            "INSERT INTO task_criteria (
                id, task_id, kind, content, required, source, evaluation_mode,
                immutable_to_agents, version, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
            params![
                criterion.id.to_string(),
                criterion.task_id.to_string(),
                criterion.kind.as_str(),
                criterion.content,
                i64::from(criterion.required),
                criterion.source.as_str(),
                criterion.evaluation_mode.as_str(),
                i64::from(criterion.immutable_to_agents),
                criterion.version,
                now_micros,
            ],
        )?;
    }
    Ok(criteria)
}

fn new_criterion(
    task_id: TaskId,
    kind: CriterionKind,
    content: String,
    source: CriterionSource,
    evaluation_mode: EvaluationMode,
    now: OffsetDateTime,
) -> TaskCriterion {
    TaskCriterion {
        id: CriterionId::new(),
        task_id,
        kind,
        content,
        required: true,
        source,
        evaluation_mode,
        immutable_to_agents: true,
        version: 0,
        created_at: now,
        updated_at: now,
    }
}

fn advance_active_completion_epoch(
    connection: &Connection,
    before: &TaskRow,
    now_micros: i64,
    workspace_id: WorkspaceId,
) -> Result<(i64, i64, i64)> {
    let task_id = before.task.id.to_string();
    let next_epoch = before
        .task
        .completion_epoch
        .checked_add(1)
        .ok_or_else(completion_counter_overflow)?;
    let next_revision = before
        .task
        .completion_revision
        .checked_add(1)
        .ok_or_else(completion_counter_overflow)?;
    let next_plan_version = before
        .task
        .plan_version
        .checked_add(1)
        .ok_or_else(completion_counter_overflow)?;
    let migratable_sessions =
        load_migratable_participants(connection, &task_id, before.task.completion_epoch)?;
    let participants_superseded = usize_to_i64(connection.execute(
        "UPDATE task_participants
         SET state = 'superseded', current_conclusion_id = NULL,
             superseded_at = ?3, version = version + 1, updated_at = ?3
         WHERE task_id = ?1 AND completion_epoch = ?2
           AND state IN ('active', 'waiting', 'concluded', 'blocked', 'departed')",
        params![task_id, before.task.completion_epoch, now_micros],
    )?)?;
    for session_id in &migratable_sessions {
        connection.execute(
            "INSERT INTO task_participants (
                id, task_id, session_id, invitation_id, kind, completion_epoch,
                state, version, current_conclusion_id, joined_at, created_at, updated_at
             ) VALUES (?1, ?2, ?3, NULL, 'worker', ?4, 'active', 0, NULL, ?5, ?5, ?5)",
            params![
                ParticipantId::new().to_string(),
                task_id,
                session_id,
                next_epoch,
                now_micros,
            ],
        )?;
    }
    let participants_migrated = usize_to_i64(migratable_sessions.len())?;
    let invitations_rebound = usize_to_i64(connection.execute(
        "UPDATE agent_invitations SET completion_epoch = ?3, updated_at = ?4
         WHERE task_id = ?1 AND completion_epoch = ?2
           AND state IN ('prepared', 'pending', 'reserved') AND expires_at > ?4",
        params![
            task_id,
            before.task.completion_epoch,
            next_epoch,
            now_micros
        ],
    )?)?;
    let changed = connection.execute(
        "UPDATE tasks SET completion_epoch = ?3, completion_revision = ?4,
                plan_version = ?5, version = version + 1, updated_at = ?6
         WHERE id = ?1 AND workspace_id = ?2 AND phase = 'active' AND version = ?7",
        params![
            task_id,
            workspace_id.to_string(),
            next_epoch,
            next_revision,
            next_plan_version,
            now_micros,
            before.task.version,
        ],
    )?;
    if changed != 1 {
        return Err(SessionBridgeError::domain(
            ErrorCode::VersionConflict,
            "Task changed while opening a new completion epoch",
        ));
    }
    Ok((
        participants_superseded,
        participants_migrated,
        invitations_rebound,
    ))
}

fn task_not_active_error(phase: TaskPhase) -> SessionBridgeError {
    SessionBridgeError::domain(
        if phase == TaskPhase::Paused {
            ErrorCode::TaskPaused
        } else {
            ErrorCode::TaskNotActive
        },
        format!("cannot change Task criteria while Task is {phase}"),
    )
}

fn apply_operation(
    connection: &Connection,
    operation: TaskOperation,
    before: &TaskRow,
    now_micros: i64,
    workspace_id: WorkspaceId,
) -> Result<TaskLifecycleEffects> {
    let task_id = before.task.id.to_string();
    let mut effects = TaskLifecycleEffects::default();
    match operation {
        TaskOperation::Pause => {
            require_phase(operation, before.task.phase, &[TaskPhase::Active])?;
            effects.claims_suspended = query_count(
                connection,
                "SELECT count(*) FROM work_claims WHERE task_id = ?1 AND closed_at IS NULL",
                [&task_id],
            )?;
            update_task_phase(
                connection,
                workspace_id,
                before,
                TaskPhase::Paused,
                "paused_at",
                now_micros,
            )?;
        }
        TaskOperation::Resume => {
            require_phase(operation, before.task.phase, &[TaskPhase::Paused])?;
            let paused_at = before.paused_at.ok_or_else(|| {
                database_value_error(
                    "tasks.paused_at",
                    "paused task does not record paused_at",
                    serde_json::json!({ "task_id": before.task.id }),
                )
            })?;
            let paused_at_micros = checked_unix_micros(paused_at)?;
            let paused_duration = now_micros.checked_sub(paused_at_micros).ok_or_else(|| {
                SessionBridgeError::domain(
                    ErrorCode::Internal,
                    "clock moved backwards while the task was paused",
                )
                .with_details(serde_json::json!({
                    "task_id": before.task.id,
                    "paused_at": paused_at,
                    "resumed_at_micros": now_micros,
                }))
            })?;
            let maximum_lease: Option<i64> = connection.query_row(
                "SELECT max(lease_expires_at) FROM work_claims
                 WHERE task_id = ?1 AND closed_at IS NULL",
                [&task_id],
                |row| row.get(0),
            )?;
            if maximum_lease.is_some()
                && maximum_lease
                    .and_then(|lease| lease.checked_add(paused_duration))
                    .is_none()
            {
                return Err(database_value_error(
                    "work_claims.lease_expires_at",
                    "claim lease extension would overflow the timestamp range",
                    serde_json::json!({
                        "task_id": before.task.id,
                        "paused_duration_micros": paused_duration,
                    }),
                ));
            }
            effects.claim_leases_extended = usize_to_i64(connection.execute(
                "UPDATE work_claims
                 SET lease_expires_at = lease_expires_at + ?2
                 WHERE task_id = ?1 AND closed_at IS NULL",
                params![task_id, paused_duration],
            )?)?;
            update_task_phase(
                connection,
                workspace_id,
                before,
                TaskPhase::Active,
                "paused_at",
                now_micros,
            )?;
        }
        TaskOperation::Cancel => {
            require_phase(
                operation,
                before.task.phase,
                &[TaskPhase::Active, TaskPhase::Paused],
            )?;
            effects.attempts_abandoned = usize_to_i64(connection.execute(
                "UPDATE work_attempts
                 SET status = 'abandoned', ended_at = ?2
                 WHERE task_id = ?1 AND status = 'active'",
                params![task_id, now_micros],
            )?)?;
            effects.claims_revoked = usize_to_i64(connection.execute(
                "UPDATE work_claims
                 SET closed_at = ?2, close_reason = 'revoked'
                 WHERE task_id = ?1 AND closed_at IS NULL",
                params![task_id, now_micros],
            )?)?;
            effects.plan_changes_superseded = usize_to_i64(connection.execute(
                "UPDATE plan_changes
                 SET status = 'superseded', version = version + 1, updated_at = ?2
                 WHERE task_id = ?1 AND status IN ('proposed', 'pending_approval')",
                params![task_id, now_micros],
            )?)?;
            effects.invitations_revoked = usize_to_i64(connection.execute(
                "UPDATE agent_invitations
                 SET state = 'revoked', revoked_at = ?2, updated_at = ?2
                 WHERE task_id = ?1 AND state IN ('prepared', 'pending', 'reserved')",
                params![task_id, now_micros],
            )?)?;
            effects.reservations_released = usize_to_i64(connection.execute(
                "UPDATE resource_reservations
                 SET released_at = ?2
                 WHERE task_id = ?1 AND released_at IS NULL",
                params![task_id, now_micros],
            )?)?;
            effects.intents_cleared = usize_to_i64(connection.execute(
                "UPDATE agent_intents
                 SET cleared_at = ?2, updated_at = ?2
                 WHERE task_id = ?1 AND cleared_at IS NULL",
                params![task_id, now_micros],
            )?)?;
            effects.coordination_requests_cancelled = usize_to_i64(connection.execute(
                "UPDATE coordination_requests
                 SET status = 'cancelled', updated_at = ?2
                 WHERE task_id = ?1 AND status IN ('open', 'acknowledged')",
                params![task_id, now_micros],
            )?)?;
            effects.blockers_cancelled = usize_to_i64(connection.execute(
                "UPDATE blockers SET status = 'cancelled'
                 WHERE task_id = ?1 AND status = 'open'",
                [&task_id],
            )?)?;
            effects.retirements_cancelled = usize_to_i64(connection.execute(
                "UPDATE participant_retirements
                 SET status = 'cancelled', version = version + 1, updated_at = ?2
                 WHERE task_id = ?1
                   AND status IN ('proposed', 'pending_approval', 'approved')",
                params![task_id, now_micros],
            )?)?;
            update_task_phase(
                connection,
                workspace_id,
                before,
                TaskPhase::Cancelled,
                "cancelled_at",
                now_micros,
            )?;
        }
        TaskOperation::Reopen => {
            require_phase(operation, before.task.phase, &[TaskPhase::Completed])?;
            let next_epoch = before
                .task
                .completion_epoch
                .checked_add(1)
                .ok_or_else(completion_counter_overflow)?;
            let next_revision = before
                .task
                .completion_revision
                .checked_add(1)
                .ok_or_else(completion_counter_overflow)?;
            let migratable_sessions =
                load_migratable_participants(connection, &task_id, before.task.completion_epoch)?;
            effects.participants_superseded = usize_to_i64(connection.execute(
                "UPDATE task_participants
                 SET state = 'superseded', current_conclusion_id = NULL,
                     superseded_at = ?3, version = version + 1, updated_at = ?3
                 WHERE task_id = ?1
                   AND completion_epoch = ?2
                   AND state IN ('active', 'waiting', 'concluded', 'blocked', 'departed')",
                params![task_id, before.task.completion_epoch, now_micros],
            )?)?;
            for session_id in &migratable_sessions {
                connection.execute(
                    "INSERT INTO task_participants (
                        id, task_id, session_id, invitation_id, kind, completion_epoch,
                        state, version, current_conclusion_id, joined_at, created_at, updated_at
                     ) VALUES (?1, ?2, ?3, NULL, 'worker', ?4, 'active', 0, NULL, ?5, ?5, ?5)",
                    params![
                        ParticipantId::new().to_string(),
                        task_id,
                        session_id,
                        next_epoch,
                        now_micros,
                    ],
                )?;
            }
            effects.participants_migrated = usize_to_i64(migratable_sessions.len())?;
            effects.invitations_revoked = usize_to_i64(connection.execute(
                "UPDATE agent_invitations
                 SET state = 'revoked', revoked_at = ?2, updated_at = ?2
                 WHERE task_id = ?1 AND state IN ('prepared', 'pending', 'reserved')",
                params![task_id, now_micros],
            )?)?;
            let affected = connection.execute(
                "UPDATE tasks
                 SET phase = 'active', completion_epoch = ?3,
                     completion_revision = ?4, version = version + 1,
                     completed_at = NULL, updated_at = ?5
                 WHERE id = ?1 AND workspace_id = ?2 AND phase = 'completed' AND version = ?6",
                params![
                    task_id,
                    workspace_id.to_string(),
                    next_epoch,
                    next_revision,
                    now_micros,
                    before.task.version,
                ],
            )?;
            ensure_single_task_update(affected, operation, before)?;
        }
        TaskOperation::Archive => {
            require_phase(
                operation,
                before.task.phase,
                &[TaskPhase::Completed, TaskPhase::Cancelled],
            )?;
            update_task_phase(
                connection,
                workspace_id,
                before,
                TaskPhase::Archived,
                "archived_at",
                now_micros,
            )?;
        }
        TaskOperation::Publish => unreachable!("publish uses its own transaction"),
    }
    Ok(effects)
}

fn update_task_phase(
    connection: &Connection,
    workspace_id: WorkspaceId,
    before: &TaskRow,
    target: TaskPhase,
    timestamp_column: &'static str,
    now_micros: i64,
) -> Result<()> {
    let next_revision = before
        .task
        .completion_revision
        .checked_add(1)
        .ok_or_else(completion_counter_overflow)?;
    let sql = match (target, timestamp_column) {
        (TaskPhase::Paused, "paused_at") => {
            "UPDATE tasks
             SET phase = 'paused', paused_at = ?3, completion_revision = ?4,
                 version = version + 1, updated_at = ?3
             WHERE id = ?1 AND workspace_id = ?2 AND phase = 'active' AND version = ?5"
        }
        (TaskPhase::Active, "paused_at") => {
            "UPDATE tasks
             SET phase = 'active', paused_at = NULL, completion_revision = ?4,
                 version = version + 1, updated_at = ?3
             WHERE id = ?1 AND workspace_id = ?2 AND phase = 'paused' AND version = ?5"
        }
        (TaskPhase::Cancelled, "cancelled_at") => {
            "UPDATE tasks
             SET phase = 'cancelled', cancelled_at = ?3, completion_revision = ?4,
                 version = version + 1, updated_at = ?3
             WHERE id = ?1 AND workspace_id = ?2
               AND phase IN ('active', 'paused') AND version = ?5"
        }
        (TaskPhase::Archived, "archived_at") => {
            "UPDATE tasks
             SET phase = 'archived', archived_at = ?3, completion_revision = ?4,
                 version = version + 1, updated_at = ?3
             WHERE id = ?1 AND workspace_id = ?2
               AND phase IN ('completed', 'cancelled') AND version = ?5"
        }
        _ => unreachable!("unsupported task phase update"),
    };
    let affected = connection.execute(
        sql,
        params![
            before.task.id.to_string(),
            workspace_id.to_string(),
            now_micros,
            next_revision,
            before.task.version,
        ],
    )?;
    ensure_single_task_update(affected, operation_for_target(target), before)
}

fn operation_for_target(target: TaskPhase) -> TaskOperation {
    match target {
        TaskPhase::Paused => TaskOperation::Pause,
        TaskPhase::Active => TaskOperation::Resume,
        TaskPhase::Cancelled => TaskOperation::Cancel,
        TaskPhase::Archived => TaskOperation::Archive,
        TaskPhase::Completed => unreachable!("completion is owned by the completion reducer"),
    }
}

fn ensure_single_task_update(
    affected: usize,
    operation: TaskOperation,
    before: &TaskRow,
) -> Result<()> {
    if affected == 1 {
        return Ok(());
    }
    Err(SessionBridgeError::retryable(
        ErrorCode::VersionConflict,
        "task changed while applying the lifecycle transition",
    )
    .with_details(serde_json::json!({
        "operation": operation.as_str(),
        "task_id": before.task.id,
        "expected_version": before.task.version,
    })))
}

fn require_phase(operation: TaskOperation, actual: TaskPhase, allowed: &[TaskPhase]) -> Result<()> {
    if allowed.contains(&actual) {
        return Ok(());
    }
    let code = if actual == TaskPhase::Paused && !matches!(operation, TaskOperation::Resume) {
        ErrorCode::TaskPaused
    } else {
        ErrorCode::TaskNotActive
    };
    Err(SessionBridgeError::domain(
        code,
        format!(
            "cannot {} a task in phase {}",
            operation.as_str(),
            actual.as_str()
        ),
    )
    .with_details(serde_json::json!({
        "operation": operation.as_str(),
        "actual_phase": actual,
        "allowed_phases": allowed,
    })))
}

fn load_migratable_participants(
    connection: &Connection,
    task_id: &str,
    completion_epoch: i64,
) -> Result<Vec<String>> {
    let mut statement = connection.prepare(
        "SELECT participant.session_id
         FROM task_participants participant
         JOIN agent_sessions session ON session.id = participant.session_id
         WHERE participant.task_id = ?1
           AND participant.completion_epoch = ?2
           AND participant.kind = 'worker'
           AND participant.state IN ('active', 'waiting')
           AND session.ended_at IS NULL
         ORDER BY participant.session_id",
    )?;
    let rows = statement.query_map(params![task_id, completion_epoch], |row| row.get(0))?;
    rows.collect::<std::result::Result<Vec<String>, _>>()
        .map_err(Into::into)
}

fn participant_count(
    connection: &Connection,
    task_id: &str,
    completion_epoch: i64,
    state: &str,
) -> Result<i64> {
    query_count(
        connection,
        "SELECT count(*) FROM task_participants
         WHERE task_id = ?1
           AND completion_epoch = ?2
           AND kind = 'worker'
           AND state = ?3",
        params![task_id, completion_epoch, state],
    )
}

fn has_completion_assurance_flag(
    connection: &Connection,
    task_id: &str,
    completion_epoch: i64,
    flag: &str,
) -> Result<bool> {
    Ok(query_count(
        connection,
        "SELECT count(*)
         FROM task_completion_assurance_flags assurance
         JOIN task_completions completion ON completion.id = assurance.task_completion_id
         WHERE completion.task_id = ?1
           AND completion.completion_epoch = ?2
           AND assurance.flag = ?3",
        params![task_id, completion_epoch, flag],
    )? > 0)
}

fn completion_warnings(
    signals: &TaskHealthSignals,
    planning_required: bool,
) -> Vec<CompletionSignalWarning> {
    let mut warnings = Vec::new();
    push_signal(
        &mut warnings,
        "INTEGRATED_CHANGESET_DRIFT",
        CompletionSignalSeverity::Critical,
        signals.integration_integrity_drift,
    );
    push_signal(
        &mut warnings,
        "INTEGRATION_INTEGRITY_UNOBSERVABLE",
        CompletionSignalSeverity::Critical,
        signals.integration_integrity_unobservable,
    );
    if planning_required {
        push_signal(
            &mut warnings,
            "PLANNING_REQUIRED",
            CompletionSignalSeverity::Info,
            1,
        );
    }
    for (code, count) in [
        (
            "REQUIRED_DELIVERABLES_OPEN",
            signals.required_deliverables_open,
        ),
        (
            "REQUIRED_DELIVERABLES_UNMAPPED",
            signals.required_deliverables_unmapped,
        ),
        (
            "REQUIRED_DELIVERABLES_UNVERIFIED",
            signals.required_deliverables_unverified,
        ),
        ("UNCOVERED_USER_CRITERIA", signals.uncovered_user_criteria),
        (
            "UNCOVERED_REQUIRED_REQUIREMENTS",
            signals.uncovered_required_requirements,
        ),
        (
            "REQUIRED_LEAF_ITEMS_NOT_ACCEPTED",
            signals.required_leaf_items_not_accepted,
        ),
        ("OPEN_REQUIRED_GAPS", signals.open_required_gaps),
        ("OPEN_BLOCKERS", signals.open_blockers),
        ("OPEN_INVALIDATIONS", signals.open_invalidations),
        (
            "OPEN_COMPLETION_CHALLENGES",
            signals.open_completion_challenges,
        ),
        ("ACTIVE_CLAIMS", signals.active_claims),
        (
            "STALE_OR_RECLAIMABLE_CLAIMS",
            signals.stale_or_reclaimable_claims,
        ),
        ("PROPOSED_WORK_ITEMS", signals.proposed_work_items),
        ("PENDING_REVIEWS", signals.pending_reviews),
        ("PENDING_INTEGRATION", signals.pending_integration),
        ("PENDING_PLAN_CHANGES", signals.pending_plan_changes),
        ("UNRESOLVED_DECISIONS", signals.unresolved_decisions),
        ("PENDING_APPROVALS", signals.pending_approvals),
        (
            "OPEN_BLOCKING_COORDINATION_REQUESTS",
            signals.open_blocking_coordination_requests,
        ),
        (
            "PENDING_BLOCKING_INVITATIONS",
            signals.pending_blocking_invitations,
        ),
        (
            "STALE_INVITATION_PREPARATIONS",
            signals.stale_invitation_preparations,
        ),
        ("STALE_JOIN_RESERVATIONS", signals.stale_join_reservations),
        ("ACTIVE_PARTICIPANTS", signals.active_participants),
        ("WAITING_PARTICIPANTS", signals.waiting_participants),
        ("BLOCKED_PARTICIPANTS", signals.blocked_participants),
        ("DEPARTED_PARTICIPANTS", signals.departed_participants),
        ("STALE_PARTICIPANTS", signals.stale_participants),
        (
            "PENDING_PARTICIPANT_RETIREMENTS",
            signals.pending_participant_retirements,
        ),
    ] {
        push_signal(
            &mut warnings,
            code,
            CompletionSignalSeverity::Warning,
            count,
        );
    }
    if signals.passed_task_integration_evidence == 0 {
        push_signal(
            &mut warnings,
            "MISSING_TASK_INTEGRATION_EVIDENCE",
            CompletionSignalSeverity::Warning,
            1,
        );
    }
    warnings
}

fn push_signal(
    signals: &mut Vec<CompletionSignalWarning>,
    code: &str,
    severity: CompletionSignalSeverity,
    count: i64,
) {
    if count > 0 {
        signals.push(CompletionSignalWarning {
            code: code.to_owned(),
            severity,
            count,
        });
    }
}

fn query_count<P: Params>(connection: &Connection, sql: &str, params: P) -> Result<i64> {
    Ok(connection.query_row(sql, params, |row| row.get(0))?)
}

fn domain_internal(error: impl std::fmt::Display) -> SessionBridgeError {
    SessionBridgeError::Internal(anyhow::anyhow!(error.to_string()))
}

fn idempotency_key<'a>(actor_id: &'a str, request_id: &'a str) -> idempotency::IdempotencyKey<'a> {
    idempotency::IdempotencyKey {
        actor_type: USER_ACTOR_TYPE,
        actor_id,
        request_id,
    }
}

fn normalize_required(field: &'static str, value: String) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(invalid_input(field, format!("{field} cannot be empty")));
    }
    Ok(value.to_owned())
}

fn normalize_optional(field: &'static str, value: Option<String>) -> Result<Option<String>> {
    value
        .map(|value| normalize_required(field, value))
        .transpose()
}

fn normalize_list(field: &'static str, values: Vec<String>) -> Result<Vec<String>> {
    values
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            normalize_required(field, value).map_err(|error| {
                error.with_details(serde_json::json!({
                    "field": field,
                    "index": index,
                }))
            })
        })
        .collect()
}

fn normalize_request_id(value: Option<String>) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.trim().is_empty() {
        return Err(invalid_input("request_id", "request ID cannot be empty"));
    }
    Ok(Some(value))
}

fn checked_duration_micros(duration: Duration) -> Result<i64> {
    i64::try_from(duration.whole_microseconds()).map_err(|_| {
        invalid_input(
            "duration",
            "duration is outside the supported microsecond range",
        )
    })
}

fn checked_unix_micros(value: OffsetDateTime) -> Result<i64> {
    i64::try_from(value.unix_timestamp_nanos() / 1_000).map_err(|_| {
        invalid_input(
            "timestamp",
            "timestamp is outside the supported Unix microsecond range",
        )
    })
}

fn timestamp_from_micros(field: &'static str, value: i64) -> Result<OffsetDateTime> {
    let nanoseconds = i128::from(value) * 1_000;
    OffsetDateTime::from_unix_timestamp_nanos(nanoseconds).map_err(|error| {
        database_value_error(
            field,
            "database timestamp is outside the supported range",
            serde_json::json!({
                "value_micros": value,
                "source": error.to_string(),
            }),
        )
    })
}

fn optional_timestamp_from_micros(
    field: &'static str,
    value: Option<i64>,
) -> Result<Option<OffsetDateTime>> {
    value
        .map(|value| timestamp_from_micros(field, value))
        .transpose()
}

fn parse_database_bool(field: &'static str, value: i64) -> Result<bool> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(database_value_error(
            field,
            "database boolean must be encoded as 0 or 1",
            serde_json::json!({ "value": value }),
        )),
    }
}

fn parse_database_enum<T>(field: &'static str, value: &str) -> Result<T>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    parse_database_value(field, value)
}

fn parse_database_value<T>(field: &'static str, value: &str) -> Result<T>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    value.parse::<T>().map_err(|error| {
        database_value_error(
            field,
            "database contains an unsupported value",
            serde_json::json!({
                "value": value,
                "source": error.to_string(),
            }),
        )
    })
}

fn usize_to_i64(value: usize) -> Result<i64> {
    i64::try_from(value).map_err(|_| {
        SessionBridgeError::domain(
            ErrorCode::Internal,
            "database affected-row count is outside the supported range",
        )
    })
}

fn completion_counter_overflow() -> SessionBridgeError {
    SessionBridgeError::domain(
        ErrorCode::Internal,
        "task completion counter reached its maximum value",
    )
}

fn invalid_input(field: &'static str, message: impl Into<String>) -> SessionBridgeError {
    SessionBridgeError::domain(ErrorCode::InvalidInput, message)
        .with_details(serde_json::json!({ "field": field }))
}

fn database_value_error(
    field: &'static str,
    message: impl Into<String>,
    details: serde_json::Value,
) -> SessionBridgeError {
    SessionBridgeError::domain(ErrorCode::Database, message).with_details(serde_json::json!({
        "field": field,
        "value_details": details,
    }))
}
