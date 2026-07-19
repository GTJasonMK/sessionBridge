use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    Result,
    app::{
        agent::{AgentService, AuthenticatedAgent},
        await_service,
        events::{self, EventActor, EventEntity},
        idempotency::{self, IdempotencyKey},
        runtime_reconciler::reconcile_after_agent_write,
    },
    clock::{Clock, unix_micros},
    domain::{
        ArtifactId, AttemptId, ChangeSetId, ClaimId, ClaimMode, EvidenceId, ParticipantKind,
        ParticipantState, SessionContext, SessionId, TaskId, TaskPhase, WorkItemId,
    },
    error::{ErrorCode, SessionBridgeError},
    git::GitRepository,
    storage::SqliteStore,
};

const DEFAULT_LEASE: Duration = Duration::minutes(15);
const MIN_LEASE: Duration = Duration::seconds(5);
const MAX_LEASE: Duration = Duration::hours(24);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceRequest {
    pub resource_key: String,
    #[serde(default = "default_resource_mode")]
    pub mode: String,
    #[serde(default = "default_resource_policy")]
    pub policy: String,
    #[serde(default)]
    pub pinned_revision: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttemptInputPin {
    pub input_type: String,
    pub input_id: String,
    pub revision_or_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimWorkItemRequest {
    pub item_id: WorkItemId,
    pub mode: ClaimMode,
    pub expected_item_version: i64,
    pub observed_plan_version: i64,
    #[serde(default)]
    pub resources: Vec<ResourceRequest>,
    #[serde(default)]
    pub inputs: Vec<AttemptInputPin>,
    #[serde(default)]
    pub lease_seconds: Option<i64>,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimResult {
    pub claim_id: ClaimId,
    pub attempt_id: AttemptId,
    pub item_id: WorkItemId,
    pub mode: ClaimMode,
    pub generation: i64,
    pub lease_expires_at: String,
    pub base_plan_version: i64,
    pub reservations: Vec<ResourceRequest>,
    pub participant_state: ParticipantState,
    pub event_sequence: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenewClaimRequest {
    pub claim_id: ClaimId,
    pub generation: i64,
    #[serde(default)]
    pub lease_seconds: Option<i64>,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenewClaimResult {
    pub claim_id: ClaimId,
    pub generation: i64,
    pub lease_expires_at: String,
    pub event_sequence: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClaimCloseAction {
    Release,
    Yield,
    Abandon,
}

impl ClaimCloseAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Release => "released",
            Self::Yield => "yielded",
            Self::Abandon => "abandoned",
        }
    }

    fn event_type(self) -> &'static str {
        match self {
            Self::Release => "claim.released",
            Self::Yield => "claim.yielded",
            Self::Abandon => "claim.abandoned",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloseClaimRequest {
    pub claim_id: ClaimId,
    pub generation: i64,
    pub action: ClaimCloseAction,
    pub reason: Option<String>,
    pub handoff: Option<serde_json::Value>,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloseClaimResult {
    pub claim_id: ClaimId,
    pub attempt_id: AttemptId,
    pub close_reason: String,
    pub event_sequence: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReclaimClaimRequest {
    pub claim_id: ClaimId,
    #[serde(default)]
    pub resources: Vec<ResourceRequest>,
    #[serde(default)]
    pub handoff: Option<serde_json::Value>,
    #[serde(default)]
    pub lease_seconds: Option<i64>,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeChangeRequest {
    pub claim_id: ClaimId,
    pub generation: i64,
    pub resources: Vec<ResourceRequest>,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeChangeResult {
    pub claim_id: ClaimId,
    pub generation: i64,
    pub active_reservations: Vec<ResourceRequest>,
    pub event_sequence: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeReconcileRequest {
    pub claim_id: ClaimId,
    pub generation: i64,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactInput {
    #[serde(default)]
    pub attempt_id: Option<AttemptId>,
    #[serde(default)]
    pub work_item_id: Option<WorkItemId>,
    pub kind: String,
    pub value: String,
    #[serde(default)]
    pub digest: Option<String>,
    #[serde(default)]
    pub revision: Option<String>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceInput {
    #[serde(default)]
    pub attempt_id: Option<AttemptId>,
    #[serde(default)]
    pub artifact_id: Option<ArtifactId>,
    pub kind: String,
    pub result: String,
    #[serde(default)]
    pub subject_type: Option<String>,
    #[serde(default)]
    pub subject_id: Option<String>,
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default)]
    pub verifier: Option<String>,
    #[serde(default = "empty_json_object")]
    pub environment: serde_json::Value,
    #[serde(default)]
    pub details: serde_json::Value,
}

fn empty_json_object() -> serde_json::Value {
    serde_json::json!({})
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointRequest {
    pub attempt_id: AttemptId,
    pub generation: i64,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub changed_resources: Vec<String>,
    #[serde(default)]
    pub artifacts: Vec<ArtifactInput>,
    #[serde(default)]
    pub evidence: Vec<EvidenceInput>,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointResult {
    pub attempt_id: AttemptId,
    pub artifact_ids: Vec<ArtifactId>,
    pub evidence_ids: Vec<EvidenceId>,
    pub recorded_resources: Vec<String>,
    pub event_sequence: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddArtifactRequest {
    #[serde(flatten)]
    pub artifact: ArtifactInput,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddEvidenceRequest {
    #[serde(flatten)]
    pub evidence: EvidenceInput,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityCreatedResult {
    pub id: String,
    pub event_sequence: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeSetInput {
    pub base_commit: String,
    pub head_commit: String,
    #[serde(default)]
    pub changed_resources: Vec<String>,
    #[serde(default)]
    pub tests: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitWorkItemRequest {
    pub item_id: WorkItemId,
    pub claim_id: ClaimId,
    pub generation: i64,
    pub evidence: Vec<EvidenceInput>,
    pub change_set: ChangeSetInput,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitWorkItemResult {
    pub item_id: WorkItemId,
    pub item_version: i64,
    pub attempt_id: AttemptId,
    pub change_set_id: ChangeSetId,
    pub evidence_ids: Vec<EvidenceId>,
    pub changed_resources: Vec<String>,
    pub result_state: String,
    pub event_sequence: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Approved,
    ChangesRequested,
}

impl ReviewDecision {
    fn as_str(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::ChangesRequested => "changes_requested",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteReviewRequest {
    pub item_id: WorkItemId,
    pub claim_id: ClaimId,
    pub generation: i64,
    pub decision: ReviewDecision,
    #[serde(default)]
    pub evidence_ids: Vec<EvidenceId>,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteReviewResult {
    pub review_result_id: String,
    pub item_id: WorkItemId,
    pub item_version: i64,
    pub decision: ReviewDecision,
    pub result_state: String,
    pub event_sequence: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationOutcome {
    Integrated,
    Failed,
}

impl IntegrationOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Integrated => "integrated",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrateChangeSetRequest {
    pub change_set_id: ChangeSetId,
    pub claim_id: ClaimId,
    pub generation: i64,
    pub outcome: IntegrationOutcome,
    #[serde(default)]
    pub details: serde_json::Value,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrateChangeSetResult {
    pub change_set_id: ChangeSetId,
    pub integration_state: String,
    pub item_id: WorkItemId,
    pub item_version: i64,
    pub result_state: String,
    pub event_sequence: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkItemInspection {
    pub item: serde_json::Value,
    pub dependencies: Vec<serde_json::Value>,
    pub claims: Vec<serde_json::Value>,
    pub attempts: Vec<serde_json::Value>,
    pub artifacts: Vec<serde_json::Value>,
    pub change_sets: Vec<serde_json::Value>,
}

pub struct WorkService<C> {
    clock: C,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ClaimTransactionOptions {
    pub adopt_existing: bool,
    pub bump_revision: bool,
}

impl<C: Clock + Clone> WorkService<C> {
    pub fn new(clock: C) -> Self {
        Self { clock }
    }

    fn reconcile_after_write(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
    ) -> Result<()> {
        reconcile_after_agent_write(store, context, self.clock.clone())?;
        Ok(())
    }

    pub fn claim(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        request: ClaimWorkItemRequest,
    ) -> Result<ClaimResult> {
        self.claim_with_existing_work_policy(store, context, request, false)
    }

    pub(crate) fn claim_with_existing_work_policy(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut request: ClaimWorkItemRequest,
        adopt_existing: bool,
    ) -> Result<ClaimResult> {
        validate_request_id(&request.request_id)?;
        validate_nonnegative(request.expected_item_version, "expected item version")?;
        validate_nonnegative(request.observed_plan_version, "observed plan version")?;
        request.resources = normalize_resources(request.resources)?;
        request.inputs = normalize_input_pins(request.inputs)?;
        validate_lease(request.lease_seconds)?;
        let payload_hash = idempotency::hash_payload(&serde_json::json!({
            "operation": "item.claim",
            "request": request,
        }))?;
        let authenticated =
            AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let key = session_key(&authenticated, &request.request_id);
        if let Some(stored) = idempotency::replay(&transaction, key, &payload_hash)? {
            transaction.commit()?;
            return Ok(stored.value);
        }
        let result = self.claim_in_transaction(
            &transaction,
            context,
            &authenticated,
            &request,
            now,
            ClaimTransactionOptions {
                adopt_existing,
                bump_revision: true,
            },
        )?;
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
        self.reconcile_after_write(store, context)?;
        Ok(result)
    }

    pub(crate) fn claim_in_transaction(
        &self,
        transaction: &Transaction<'_>,
        context: &SessionContext,
        authenticated: &AuthenticatedAgent,
        request: &ClaimWorkItemRequest,
        now: OffsetDateTime,
        options: ClaimTransactionOptions,
    ) -> Result<ClaimResult> {
        let lease = validate_lease(request.lease_seconds)?;
        let task = load_task(transaction, context.task_id)?;
        require_active_task(task.phase)?;
        let participant_state = require_writable_participant(transaction, authenticated, now)?;
        let item = load_work_item(transaction, context.task_id, request.item_id)?;
        validate_claimable_item(transaction, &item, authenticated, request)?;

        if let Some(conflict) = load_open_claim_conflict(
            transaction,
            request.item_id,
            request.mode,
            authenticated.session.session_id,
        )? {
            let code = if conflict.lease_expires_at <= unix_micros(now) {
                ErrorCode::ClaimExpired
            } else {
                ErrorCode::ClaimConflict
            };
            return Err(SessionBridgeError::domain(
                code,
                "WorkItem already has a conflicting Claim",
            )
            .with_details(serde_json::json!({
                "claim_id": conflict.id,
                "session_id": conflict.session_id,
                "generation": conflict.generation,
                "lease_expires_at": conflict.lease_expires_at,
                "reclaimable": conflict.lease_expires_at <= unix_micros(now),
            })));
        }

        let planned: Vec<String> = serde_json::from_str(&item.planned_resources_json)?;
        let integration_change_sets = if request.mode != ClaimMode::Execute {
            load_pending_change_set_resources(transaction, item.id)?
        } else {
            Vec::new()
        };
        let integration_owner_ids: Vec<String> = integration_change_sets
            .iter()
            .map(|(id, _)| id.to_string())
            .collect();
        let mut all_resources = planned
            .into_iter()
            .map(|resource_key| ResourceRequest {
                resource_key,
                mode: default_mode_for_claim(request.mode).into(),
                policy: default_resource_policy(),
                pinned_revision: None,
            })
            .chain(
                integration_change_sets
                    .iter()
                    .flat_map(|(_, resources)| resources.iter())
                    .cloned()
                    .map(|resource_key| ResourceRequest {
                        resource_key,
                        mode: default_mode_for_claim(request.mode).into(),
                        policy: default_resource_policy(),
                        pinned_revision: None,
                    }),
            )
            .chain(request.resources.clone())
            .collect();
        all_resources = normalize_resources(all_resources)?;
        validate_resource_conflicts(
            transaction,
            context.task_id,
            None,
            &integration_owner_ids,
            &all_resources,
        )?;
        let attempt_base_commit = refresh_execute_worktree_baseline(
            transaction,
            context.task_id,
            authenticated,
            request.mode,
            options.adopt_existing,
            now,
        )?;

        let generation: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(generation), 0) + 1 FROM work_claims
             WHERE work_item_id = ?1 AND mode = ?2",
            params![request.item_id.to_string(), request.mode.as_str()],
            |row| row.get(0),
        )?;
        let claim_id = ClaimId::new();
        let attempt_id = AttemptId::new();
        let lease_expires_at = now + lease;
        transaction.execute(
            "INSERT INTO work_claims (
                id, task_id, work_item_id, session_id, mode, generation,
                claimed_at, lease_expires_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                claim_id.to_string(),
                context.task_id.to_string(),
                request.item_id.to_string(),
                authenticated.session.session_id.to_string(),
                request.mode.as_str(),
                generation,
                unix_micros(now),
                unix_micros(lease_expires_at),
            ],
        )?;
        transaction.execute(
            "INSERT INTO work_attempts (
                id, task_id, claim_id, work_item_id, session_id, base_plan_version,
                base_commit, status, started_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active', ?8)",
            params![
                attempt_id.to_string(),
                context.task_id.to_string(),
                claim_id.to_string(),
                request.item_id.to_string(),
                authenticated.session.session_id.to_string(),
                request.observed_plan_version,
                attempt_base_commit,
                unix_micros(now),
            ],
        )?;
        if matches!(request.mode, ClaimMode::Review | ClaimMode::Integrate) {
            transaction.execute(
                "UPDATE stage_handoffs SET status='consumed',consumed_by_claim_id=?3,updated_at=?4
                 WHERE work_item_id=?1 AND to_stage=?2 AND status='reserved'",
                params![
                    request.item_id.to_string(),
                    request.mode.as_str(),
                    claim_id.to_string(),
                    unix_micros(now),
                ],
            )?;
        }
        pin_attempt_inputs(transaction, attempt_id, &item, &request.inputs, now)?;
        insert_reservations(
            transaction,
            context.task_id,
            "claim",
            &claim_id.to_string(),
            &all_resources,
            now,
        )?;
        if options.bump_revision {
            bump_completion_revision(transaction, context.task_id, now)?;
        }
        let event_sequence = append_session_event(
            transaction,
            context,
            authenticated,
            "claim.acquired",
            "claim",
            &claim_id.to_string(),
            &serde_json::json!({
                "work_item_id": request.item_id,
                "attempt_id": attempt_id,
                "mode": request.mode,
                "generation": generation,
                "lease_expires_at": format_time(lease_expires_at)?,
                "resources": all_resources,
            }),
            now,
        )?;
        Ok(ClaimResult {
            claim_id,
            attempt_id,
            item_id: request.item_id,
            mode: request.mode,
            generation,
            lease_expires_at: format_time(lease_expires_at)?,
            base_plan_version: request.observed_plan_version,
            reservations: all_resources,
            participant_state,
            event_sequence,
        })
    }

    pub fn renew(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        request: RenewClaimRequest,
    ) -> Result<RenewClaimResult> {
        validate_request_id(&request.request_id)?;
        let lease = validate_lease(request.lease_seconds)?;
        let payload_hash = idempotency::hash_payload(&serde_json::json!({
            "operation": "claim.renew",
            "request": request,
        }))?;
        let authenticated =
            AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let key = session_key(&authenticated, &request.request_id);
        if let Some(stored) = idempotency::replay(&transaction, key, &payload_hash)? {
            transaction.commit()?;
            return Ok(stored.value);
        }
        let task = load_task(&transaction, context.task_id)?;
        if !matches!(task.phase, TaskPhase::Active | TaskPhase::Paused) {
            return Err(task_phase_error(task.phase));
        }
        let claim = load_claim(&transaction, context.task_id, request.claim_id)?;
        validate_claim_fence(&claim, &authenticated, request.generation, now, true)?;
        let lease_expires_at = now + lease;
        let changed = transaction.execute(
            "UPDATE work_claims SET lease_expires_at = ?4, renewed_at = ?3
             WHERE id = ?1 AND session_id = ?2 AND generation = ?5
               AND closed_at IS NULL AND lease_expires_at > ?3",
            params![
                request.claim_id.to_string(),
                authenticated.session.session_id.to_string(),
                unix_micros(now),
                unix_micros(lease_expires_at),
                request.generation,
            ],
        )?;
        if changed != 1 {
            return Err(stale_claim_error(&claim, request.generation, now));
        }
        let event_sequence = append_session_event(
            &transaction,
            context,
            &authenticated,
            "claim.renewed",
            "claim",
            &request.claim_id.to_string(),
            &serde_json::json!({
                "generation": request.generation,
                "lease_expires_at": format_time(lease_expires_at)?,
            }),
            now,
        )?;
        let result = RenewClaimResult {
            claim_id: request.claim_id,
            generation: request.generation,
            lease_expires_at: format_time(lease_expires_at)?,
            event_sequence,
        };
        idempotency::record(
            &transaction,
            key,
            Some(&context.task_id.to_string()),
            &payload_hash,
            &result,
            Some(event_sequence),
            now,
        )?;
        transaction.commit()?;
        self.reconcile_after_write(store, context)?;
        Ok(result)
    }

    pub fn close(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        request: CloseClaimRequest,
    ) -> Result<CloseClaimResult> {
        validate_request_id(&request.request_id)?;
        if request
            .reason
            .as_deref()
            .is_some_and(|reason| reason.trim().is_empty())
        {
            return Err(invalid("claim close reason cannot be blank"));
        }
        let payload_hash = idempotency::hash_payload(&serde_json::json!({
            "operation": request.action.event_type(),
            "request": request,
        }))?;
        let authenticated =
            AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let key = session_key(&authenticated, &request.request_id);
        if let Some(stored) = idempotency::replay(&transaction, key, &payload_hash)? {
            transaction.commit()?;
            return Ok(stored.value);
        }
        let claim = load_claim(&transaction, context.task_id, request.claim_id)?;
        validate_claim_fence(&claim, &authenticated, request.generation, now, false)?;
        let attempt_id = active_attempt_for_claim(&transaction, request.claim_id)?;
        transaction.execute(
            "UPDATE work_claims SET closed_at = ?2, close_reason = ?3
             WHERE id = ?1 AND closed_at IS NULL",
            params![
                request.claim_id.to_string(),
                unix_micros(now),
                request.action.as_str(),
            ],
        )?;
        transaction.execute(
            "UPDATE work_attempts SET status = 'abandoned', ended_at = ?2
             WHERE id = ?1 AND status = 'active'",
            params![attempt_id.to_string(), unix_micros(now)],
        )?;
        release_claim_reservations(&transaction, request.claim_id, now)?;
        bump_completion_revision(&transaction, context.task_id, now)?;
        let event_sequence = append_session_event(
            &transaction,
            context,
            &authenticated,
            request.action.event_type(),
            "claim",
            &request.claim_id.to_string(),
            &serde_json::json!({
                "attempt_id": attempt_id,
                "generation": request.generation,
                "reason": request.reason,
                "handoff": request.handoff,
            }),
            now,
        )?;
        let result = CloseClaimResult {
            claim_id: request.claim_id,
            attempt_id,
            close_reason: request.action.as_str().into(),
            event_sequence,
        };
        idempotency::record(
            &transaction,
            key,
            Some(&context.task_id.to_string()),
            &payload_hash,
            &result,
            Some(event_sequence),
            now,
        )?;
        transaction.commit()?;
        self.reconcile_after_write(store, context)?;
        Ok(result)
    }

    pub fn reclaim(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut request: ReclaimClaimRequest,
    ) -> Result<ClaimResult> {
        validate_request_id(&request.request_id)?;
        request.resources = normalize_resources(request.resources)?;
        let lease = validate_lease(request.lease_seconds)?;
        let payload_hash = idempotency::hash_payload(&serde_json::json!({
            "operation": "claim.reclaim",
            "request": request,
        }))?;
        let authenticated =
            AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let key = session_key(&authenticated, &request.request_id);
        if let Some(stored) = idempotency::replay(&transaction, key, &payload_hash)? {
            transaction.commit()?;
            return Ok(stored.value);
        }
        let task = load_task(&transaction, context.task_id)?;
        require_active_task(task.phase)?;
        let participant_state = require_writable_participant(&transaction, &authenticated, now)?;
        let old = load_claim(&transaction, context.task_id, request.claim_id)?;
        if old.closed_at.is_some() || old.lease_expires_at > unix_micros(now) {
            return Err(SessionBridgeError::domain(
                ErrorCode::ClaimConflict,
                "Claim is not expired and cannot be reclaimed",
            ));
        }
        let old_attempt = active_attempt_for_claim(&transaction, old.id)?;
        let inherited = load_claim_reservations(&transaction, old.id)?;
        let resources = if request.resources.is_empty() {
            inherited
        } else {
            request.resources.clone()
        };
        validate_resource_conflicts(&transaction, context.task_id, Some(old.id), &[], &resources)?;
        transaction.execute(
            "UPDATE work_claims SET closed_at = ?2, close_reason = 'reclaimed'
             WHERE id = ?1 AND closed_at IS NULL AND lease_expires_at <= ?2",
            params![old.id.to_string(), unix_micros(now)],
        )?;
        transaction.execute(
            "UPDATE work_attempts SET status = 'reclaimed', ended_at = ?2
             WHERE id = ?1 AND status = 'active'",
            params![old_attempt.to_string(), unix_micros(now)],
        )?;
        release_claim_reservations(&transaction, old.id, now)?;

        let generation = old.generation + 1;
        let claim_id = ClaimId::new();
        let attempt_id = AttemptId::new();
        let lease_expires_at = now + lease;
        transaction.execute(
            "INSERT INTO work_claims (
                id, task_id, work_item_id, session_id, mode, generation,
                claimed_at, lease_expires_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                claim_id.to_string(),
                context.task_id.to_string(),
                old.work_item_id.to_string(),
                authenticated.session.session_id.to_string(),
                old.mode.as_str(),
                generation,
                unix_micros(now),
                unix_micros(lease_expires_at),
            ],
        )?;
        transaction.execute(
            "INSERT INTO work_attempts (
                id, task_id, claim_id, work_item_id, session_id, base_plan_version,
                base_commit, status, started_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active', ?8)",
            params![
                attempt_id.to_string(),
                context.task_id.to_string(),
                claim_id.to_string(),
                old.work_item_id.to_string(),
                authenticated.session.session_id.to_string(),
                task.plan_version,
                authenticated.session.head_commit,
                unix_micros(now),
            ],
        )?;
        copy_attempt_pins(&transaction, old_attempt, attempt_id, now)?;
        insert_reservations(
            &transaction,
            context.task_id,
            "claim",
            &claim_id.to_string(),
            &resources,
            now,
        )?;
        bump_completion_revision(&transaction, context.task_id, now)?;
        let event_sequence = append_session_event(
            &transaction,
            context,
            &authenticated,
            "claim.reclaimed",
            "claim",
            &claim_id.to_string(),
            &serde_json::json!({
                "previous_claim_id": old.id,
                "previous_attempt_id": old_attempt,
                "attempt_id": attempt_id,
                "generation": generation,
                "handoff": request.handoff,
            }),
            now,
        )?;
        let result = ClaimResult {
            claim_id,
            attempt_id,
            item_id: old.work_item_id,
            mode: old.mode,
            generation,
            lease_expires_at: format_time(lease_expires_at)?,
            base_plan_version: task.plan_version,
            reservations: resources,
            participant_state,
            event_sequence,
        };
        idempotency::record(
            &transaction,
            key,
            Some(&context.task_id.to_string()),
            &payload_hash,
            &result,
            Some(event_sequence),
            now,
        )?;
        transaction.commit()?;
        self.reconcile_after_write(store, context)?;
        Ok(result)
    }

    pub fn expand_scope(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut request: ScopeChangeRequest,
    ) -> Result<ScopeChangeResult> {
        request.resources = normalize_resources(request.resources)?;
        if request.resources.is_empty() {
            return Err(invalid("scope expansion requires at least one resource"));
        }
        self.change_scope(store, context, request, true)
    }

    pub fn reduce_scope(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut request: ScopeChangeRequest,
    ) -> Result<ScopeChangeResult> {
        request.resources = normalize_resources(request.resources)?;
        if request.resources.is_empty() {
            return Err(invalid("scope reduction requires at least one resource"));
        }
        self.change_scope(store, context, request, false)
    }

    pub fn reconcile_scope_from_git(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        request: ScopeReconcileRequest,
    ) -> Result<ScopeChangeResult> {
        validate_request_id(&request.request_id)?;
        let payload_hash = idempotency::hash_payload(&serde_json::json!({
            "operation": "claim.scope.reconcile_from_git",
            "request": request,
        }))?;
        let authenticated =
            AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)?;
        let worktree_path = authenticated
            .session
            .worktree_path
            .as_deref()
            .ok_or_else(|| {
                SessionBridgeError::domain(
                    ErrorCode::WorktreeConflict,
                    "Git scope reconciliation requires a registered worktree",
                )
            })?;
        let repository = GitRepository::open(worktree_path)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let key = session_key(&authenticated, &request.request_id);
        if let Some(stored) = idempotency::replay(&transaction, key, &payload_hash)? {
            transaction.commit()?;
            return Ok(stored.value);
        }
        require_active_task(load_task(&transaction, context.task_id)?.phase)?;
        let claim = load_claim(&transaction, context.task_id, request.claim_id)?;
        validate_claim_fence(&claim, &authenticated, request.generation, now, true)?;
        if claim.mode != ClaimMode::Execute {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "Git scope reconciliation is only valid for execute Claims",
            ));
        }
        let attempt = load_attempt_by_claim(&transaction, context.task_id, claim.id)?;
        if attempt.status != "active" {
            return Err(stale("WorkAttempt is no longer active"));
        }
        let base_commit = attempt.base_commit.as_deref().ok_or_else(|| {
            SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "Git scope reconciliation requires a WorkAttempt base commit",
            )
        })?;
        let head_commit = repository.head_commit()?;
        let mut changed_paths = repository.changed_paths(base_commit, &head_commit)?;
        changed_paths.extend(repository.dirty_paths()?.into_iter().map(PathBuf::from));
        let changed_resources = normalize_resource_keys(
            changed_paths
                .into_iter()
                .map(|path| format!("path:{}", path.display()))
                .collect(),
        )?;
        let active = load_claim_reservations(&transaction, claim.id)?;
        let additions = changed_resources
            .into_iter()
            .filter(|changed| {
                !active.iter().any(|reservation| {
                    matches!(reservation.mode.as_str(), "write" | "exclusive")
                        && resources_overlap(&reservation.resource_key, changed)
                })
            })
            .map(|resource_key| ResourceRequest {
                resource_key,
                mode: "write".into(),
                policy: "enforced".into(),
                pinned_revision: None,
            })
            .collect::<Vec<_>>();
        if !additions.is_empty() {
            validate_resource_conflicts(
                &transaction,
                context.task_id,
                Some(claim.id),
                &[],
                &additions,
            )?;
            insert_reservations(
                &transaction,
                context.task_id,
                "claim",
                &claim.id.to_string(),
                &additions,
                now,
            )?;
            bump_completion_revision(&transaction, context.task_id, now)?;
        }
        let active_reservations = load_claim_reservations(&transaction, claim.id)?;
        let event_sequence = append_session_event(
            &transaction,
            context,
            &authenticated,
            "claim.scope.reconciled",
            "claim",
            &claim.id.to_string(),
            &serde_json::json!({
                "generation": claim.generation,
                "source": "git",
                "added_resources": additions,
                "active_reservations": active_reservations,
            }),
            now,
        )?;
        let result = ScopeChangeResult {
            claim_id: claim.id,
            generation: claim.generation,
            active_reservations,
            event_sequence,
        };
        idempotency::record(
            &transaction,
            key,
            Some(&context.task_id.to_string()),
            &payload_hash,
            &result,
            Some(event_sequence),
            now,
        )?;
        transaction.commit()?;
        self.reconcile_after_write(store, context)?;
        Ok(result)
    }

    fn change_scope(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        request: ScopeChangeRequest,
        expand: bool,
    ) -> Result<ScopeChangeResult> {
        validate_request_id(&request.request_id)?;
        let operation = if expand {
            "claim.scope.expand"
        } else {
            "claim.scope.reduce"
        };
        let payload_hash = idempotency::hash_payload(&serde_json::json!({
            "operation": operation,
            "request": request,
        }))?;
        let authenticated =
            AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let key = session_key(&authenticated, &request.request_id);
        if let Some(stored) = idempotency::replay(&transaction, key, &payload_hash)? {
            transaction.commit()?;
            return Ok(stored.value);
        }
        let task = load_task(&transaction, context.task_id)?;
        require_active_task(task.phase)?;
        let claim = load_claim(&transaction, context.task_id, request.claim_id)?;
        validate_claim_fence(&claim, &authenticated, request.generation, now, true)?;
        if expand {
            validate_resource_conflicts(
                &transaction,
                context.task_id,
                Some(claim.id),
                &[],
                &request.resources,
            )?;
            insert_reservations(
                &transaction,
                context.task_id,
                "claim",
                &claim.id.to_string(),
                &request.resources,
                now,
            )?;
        } else {
            let attempt_id = active_attempt_for_claim(&transaction, claim.id)?;
            for resource in &request.resources {
                let used: bool = transaction.query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM resource_usage
                        WHERE attempt_id = ?1 AND resource_key = ?2
                    )",
                    params![attempt_id.to_string(), resource.resource_key],
                    |row| row.get(0),
                )?;
                if used {
                    return Err(SessionBridgeError::domain(
                        ErrorCode::ScopeDrift,
                        "used resources cannot be removed from an active Claim",
                    )
                    .with_details(serde_json::json!({
                        "resource_key": resource.resource_key,
                        "attempt_id": attempt_id,
                    })));
                }
                let changed = transaction.execute(
                    "UPDATE resource_reservations SET released_at = ?3
                     WHERE owner_type = 'claim' AND owner_id = ?1
                       AND resource_key = ?2 AND released_at IS NULL",
                    params![
                        claim.id.to_string(),
                        resource.resource_key,
                        unix_micros(now)
                    ],
                )?;
                if changed != 1 {
                    return Err(SessionBridgeError::domain(
                        ErrorCode::NotFound,
                        "active Claim reservation was not found",
                    )
                    .with_details(serde_json::json!({
                        "resource_key": resource.resource_key,
                    })));
                }
            }
        }
        bump_completion_revision(&transaction, context.task_id, now)?;
        let active_reservations = load_claim_reservations(&transaction, claim.id)?;
        let event_sequence = append_session_event(
            &transaction,
            context,
            &authenticated,
            operation,
            "claim",
            &claim.id.to_string(),
            &serde_json::json!({
                "generation": claim.generation,
                "resources": request.resources,
                "active_reservations": active_reservations,
            }),
            now,
        )?;
        let result = ScopeChangeResult {
            claim_id: claim.id,
            generation: claim.generation,
            active_reservations,
            event_sequence,
        };
        idempotency::record(
            &transaction,
            key,
            Some(&context.task_id.to_string()),
            &payload_hash,
            &result,
            Some(event_sequence),
            now,
        )?;
        transaction.commit()?;
        self.reconcile_after_write(store, context)?;
        Ok(result)
    }

    pub fn checkpoint(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut request: CheckpointRequest,
    ) -> Result<CheckpointResult> {
        validate_request_id(&request.request_id)?;
        request.changed_resources = normalize_resource_keys(request.changed_resources)?;
        let payload_hash = idempotency::hash_payload(&serde_json::json!({
            "operation": "attempt.checkpoint",
            "request": request,
        }))?;
        let authenticated =
            AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let key = session_key(&authenticated, &request.request_id);
        if let Some(stored) = idempotency::replay(&transaction, key, &payload_hash)? {
            transaction.commit()?;
            return Ok(stored.value);
        }
        let attempt = load_attempt(&transaction, context.task_id, request.attempt_id)?;
        let claim = load_claim(&transaction, context.task_id, attempt.claim_id)?;
        validate_claim_fence(&claim, &authenticated, request.generation, now, true)?;
        if attempt.status != "active" {
            return Err(stale("WorkAttempt is no longer active"));
        }
        validate_changed_scope(&transaction, claim.id, &request.changed_resources)?;
        record_resource_usage(
            &transaction,
            context.task_id,
            attempt.id,
            &request.changed_resources,
            now,
        )?;
        let artifact_ids = insert_artifacts(
            &transaction,
            context,
            &authenticated,
            Some(attempt.id),
            Some(attempt.work_item_id),
            &request.artifacts,
            now,
        )?;
        let evidence_ids = insert_evidence(
            &transaction,
            context,
            &authenticated,
            Some(attempt.id),
            &request.evidence,
            now,
        )?;
        let renewed_lease_expires_at = unix_micros(now + DEFAULT_LEASE);
        transaction.execute(
            "UPDATE work_claims
             SET lease_expires_at=MAX(lease_expires_at,?4),renewed_at=?5
             WHERE id=?1 AND session_id=?2 AND generation=?3 AND closed_at IS NULL",
            params![
                claim.id.to_string(),
                context.session_id.to_string(),
                request.generation,
                renewed_lease_expires_at,
                unix_micros(now)
            ],
        )?;
        bump_completion_revision(&transaction, context.task_id, now)?;
        let event_sequence = append_session_event(
            &transaction,
            context,
            &authenticated,
            "attempt.checkpointed",
            "attempt",
            &attempt.id.to_string(),
            &serde_json::json!({
                "claim_id": claim.id,
                "generation": request.generation,
                "summary": request.summary,
                "changed_resources": request.changed_resources,
                "artifact_ids": artifact_ids,
                "evidence_ids": evidence_ids,
                "lease_expires_at": renewed_lease_expires_at,
                "lease_auto_renewed": renewed_lease_expires_at > claim.lease_expires_at,
            }),
            now,
        )?;
        let result = CheckpointResult {
            attempt_id: attempt.id,
            artifact_ids,
            evidence_ids,
            recorded_resources: request.changed_resources,
            event_sequence,
        };
        idempotency::record(
            &transaction,
            key,
            Some(&context.task_id.to_string()),
            &payload_hash,
            &result,
            Some(event_sequence),
            now,
        )?;
        transaction.commit()?;
        self.reconcile_after_write(store, context)?;
        Ok(result)
    }

    pub fn add_artifact(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        request: AddArtifactRequest,
    ) -> Result<EntityCreatedResult> {
        validate_request_id(&request.request_id)?;
        let payload_hash = idempotency::hash_payload(&serde_json::json!({
            "operation": "artifact.add",
            "request": request,
        }))?;
        let authenticated =
            AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let key = session_key(&authenticated, &request.request_id);
        if let Some(stored) = idempotency::replay(&transaction, key, &payload_hash)? {
            transaction.commit()?;
            return Ok(stored.value);
        }
        require_writable_participant(&transaction, &authenticated, now)?;
        let ids = insert_artifacts(
            &transaction,
            context,
            &authenticated,
            request.artifact.attempt_id,
            request.artifact.work_item_id,
            std::slice::from_ref(&request.artifact),
            now,
        )?;
        bump_completion_revision(&transaction, context.task_id, now)?;
        let id = ids[0];
        let event_sequence = append_session_event(
            &transaction,
            context,
            &authenticated,
            "artifact.added",
            "artifact",
            &id.to_string(),
            &serde_json::json!({ "artifact_id": id }),
            now,
        )?;
        let result = EntityCreatedResult {
            id: id.to_string(),
            event_sequence,
        };
        idempotency::record(
            &transaction,
            key,
            Some(&context.task_id.to_string()),
            &payload_hash,
            &result,
            Some(event_sequence),
            now,
        )?;
        transaction.commit()?;
        self.reconcile_after_write(store, context)?;
        Ok(result)
    }

    pub fn add_evidence(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        request: AddEvidenceRequest,
    ) -> Result<EntityCreatedResult> {
        validate_request_id(&request.request_id)?;
        let payload_hash = idempotency::hash_payload(&serde_json::json!({
            "operation": "evidence.add",
            "request": request,
        }))?;
        let authenticated =
            AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let key = session_key(&authenticated, &request.request_id);
        if let Some(stored) = idempotency::replay(&transaction, key, &payload_hash)? {
            transaction.commit()?;
            return Ok(stored.value);
        }
        require_writable_participant(&transaction, &authenticated, now)?;
        let ids = insert_evidence(
            &transaction,
            context,
            &authenticated,
            request.evidence.attempt_id,
            std::slice::from_ref(&request.evidence),
            now,
        )?;
        bump_completion_revision(&transaction, context.task_id, now)?;
        let id = ids[0];
        let event_sequence = append_session_event(
            &transaction,
            context,
            &authenticated,
            "evidence.added",
            "evidence",
            &id.to_string(),
            &serde_json::json!({ "evidence_id": id }),
            now,
        )?;
        let result = EntityCreatedResult {
            id: id.to_string(),
            event_sequence,
        };
        idempotency::record(
            &transaction,
            key,
            Some(&context.task_id.to_string()),
            &payload_hash,
            &result,
            Some(event_sequence),
            now,
        )?;
        transaction.commit()?;
        self.reconcile_after_write(store, context)?;
        Ok(result)
    }

    pub fn submit(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut request: SubmitWorkItemRequest,
    ) -> Result<SubmitWorkItemResult> {
        validate_request_id(&request.request_id)?;
        request.change_set.changed_resources =
            normalize_resource_keys(request.change_set.changed_resources)?;
        let payload_hash = idempotency::hash_payload(&serde_json::json!({
            "operation": "item.submit",
            "request": request,
        }))?;
        let authenticated =
            AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)?;
        let actual_resources = actual_changed_resources(&authenticated, &request.change_set)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let key = session_key(&authenticated, &request.request_id);
        if let Some(stored) = idempotency::replay(&transaction, key, &payload_hash)? {
            transaction.commit()?;
            return Ok(stored.value);
        }
        let task = load_task(&transaction, context.task_id)?;
        require_active_task(task.phase)?;
        let item = load_work_item(&transaction, context.task_id, request.item_id)?;
        let claim = load_claim(&transaction, context.task_id, request.claim_id)?;
        validate_claim_fence(&claim, &authenticated, request.generation, now, true)?;
        if claim.work_item_id != item.id || claim.mode != ClaimMode::Execute {
            return Err(SessionBridgeError::domain(
                ErrorCode::StaleClaim,
                "item submit requires the current execute Claim for this WorkItem",
            ));
        }
        let attempt = load_attempt_by_claim(&transaction, context.task_id, claim.id)?;
        if attempt.status != "active" {
            return Err(stale("WorkAttempt is no longer active"));
        }
        if attempt
            .base_commit
            .as_deref()
            .filter(|base| !base.is_empty())
            .is_some_and(|base| base != request.change_set.base_commit)
        {
            return Err(SessionBridgeError::domain(
                ErrorCode::InputInvalidated,
                "ChangeSet base commit differs from the WorkAttempt baseline",
            )
            .with_details(serde_json::json!({
                "attempt_base_commit": attempt.base_commit,
                "submitted_base_commit": request.change_set.base_commit,
            })));
        }
        validate_attempt_inputs(&transaction, &attempt)?;
        validate_changed_scope(&transaction, claim.id, &actual_resources)?;
        record_resource_usage(
            &transaction,
            context.task_id,
            attempt.id,
            &actual_resources,
            now,
        )?;
        let evidence_ids = insert_evidence(
            &transaction,
            context,
            &authenticated,
            Some(attempt.id),
            &request.evidence,
            now,
        )?;
        close_superseded_stage_claims(
            &transaction,
            context.task_id,
            item.id,
            &[ClaimMode::Review, ClaimMode::Integrate],
            None,
            now,
        )?;
        let change_set_id = ChangeSetId::new();
        transaction.execute(
            "INSERT INTO change_sets (
                id, task_id, attempt_id, base_commit, head_commit,
                changed_resources_json, tests_json, integration_state,
                created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8, ?8)",
            params![
                change_set_id.to_string(),
                context.task_id.to_string(),
                attempt.id.to_string(),
                request.change_set.base_commit,
                request.change_set.head_commit,
                serde_json::to_string(&actual_resources)?,
                serde_json::to_string(&request.change_set.tests)?,
                unix_micros(now),
            ],
        )?;
        transfer_reservations_to_change_set(
            &transaction,
            claim.id,
            change_set_id,
            &actual_resources,
            now,
        )?;
        transaction.execute(
            "UPDATE work_attempts SET status = 'submitted', ended_at = ?2
             WHERE id = ?1 AND status = 'active'",
            params![attempt.id.to_string(), unix_micros(now)],
        )?;
        transaction.execute(
            "UPDATE work_claims SET closed_at = ?2, close_reason = 'submitted'
             WHERE id = ?1 AND closed_at IS NULL",
            params![claim.id.to_string(), unix_micros(now)],
        )?;
        let changed = transaction.execute(
            "UPDATE work_items SET result_state = 'submitted', version = version + 1,
                    submitted_at = ?3, updated_at = ?3
             WHERE id = ?1 AND task_id = ?2 AND plan_state = 'active'
               AND result_state = 'open' AND version = ?4",
            params![
                item.id.to_string(),
                context.task_id.to_string(),
                unix_micros(now),
                item.version,
            ],
        )?;
        if changed != 1 {
            return Err(SessionBridgeError::domain(
                ErrorCode::VersionConflict,
                "WorkItem changed while it was being submitted",
            ));
        }
        bump_completion_revision(&transaction, context.task_id, now)?;
        let event_sequence = append_session_event(
            &transaction,
            context,
            &authenticated,
            "item.submitted",
            "work_item",
            &item.id.to_string(),
            &serde_json::json!({
                "claim_id": claim.id,
                "attempt_id": attempt.id,
                "change_set_id": change_set_id,
                "evidence_ids": evidence_ids,
                "changed_resources": actual_resources,
            }),
            now,
        )?;
        reserve_stage_handoff(
            &transaction,
            context,
            item.id,
            "execute",
            "review",
            &authenticated.session.session_id.to_string(),
            now,
        )?;
        let result = SubmitWorkItemResult {
            item_id: item.id,
            item_version: item.version + 1,
            attempt_id: attempt.id,
            change_set_id,
            evidence_ids,
            changed_resources: actual_resources,
            result_state: "submitted".into(),
            event_sequence,
        };
        idempotency::record(
            &transaction,
            key,
            Some(&context.task_id.to_string()),
            &payload_hash,
            &result,
            Some(event_sequence),
            now,
        )?;
        transaction.commit()?;
        self.reconcile_after_write(store, context)?;
        Ok(result)
    }

    pub fn complete_review(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut request: CompleteReviewRequest,
    ) -> Result<CompleteReviewResult> {
        validate_request_id(&request.request_id)?;
        request.evidence_ids.sort_by_key(ToString::to_string);
        request.evidence_ids.dedup();
        let payload_hash = idempotency::hash_payload(&serde_json::json!({
            "operation": "review.complete",
            "request": request,
        }))?;
        let authenticated =
            AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let key = session_key(&authenticated, &request.request_id);
        if let Some(stored) = idempotency::replay(&transaction, key, &payload_hash)? {
            transaction.commit()?;
            return Ok(stored.value);
        }
        let result = self.complete_review_in_transaction(
            &transaction,
            context,
            &authenticated,
            request,
            &payload_hash,
            now,
        )?;
        transaction.commit()?;
        self.reconcile_after_write(store, context)?;
        Ok(result)
    }

    pub(crate) fn complete_review_in_transaction(
        &self,
        transaction: &Transaction<'_>,
        context: &SessionContext,
        authenticated: &AuthenticatedAgent,
        request: CompleteReviewRequest,
        payload_hash: &[u8; 32],
        now: OffsetDateTime,
    ) -> Result<CompleteReviewResult> {
        let key = session_key(authenticated, &request.request_id);
        if let Some(stored) = idempotency::replay(transaction, key, payload_hash)? {
            return Ok(stored.value);
        }
        require_active_task(load_task(transaction, context.task_id)?.phase)?;
        let item = load_work_item(transaction, context.task_id, request.item_id)?;
        if item.result_state != "submitted" {
            return Err(SessionBridgeError::domain(
                ErrorCode::ItemNotReady,
                "review completion requires a submitted WorkItem",
            ));
        }
        let claim = load_claim(transaction, context.task_id, request.claim_id)?;
        validate_claim_fence(&claim, authenticated, request.generation, now, true)?;
        if claim.work_item_id != item.id || claim.mode != ClaimMode::Review {
            return Err(stale(
                "review completion requires the current review Claim for this WorkItem",
            ));
        }
        let attempt_id = active_attempt_for_claim(transaction, claim.id)?;
        validate_evidence_ids(transaction, context.task_id, &request.evidence_ids)?;
        let review_result_id = uuid::Uuid::now_v7().to_string();
        transaction.execute(
            "INSERT INTO review_results (
                id, task_id, work_item_id, claim_id, decision,
                evidence_ids_json, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                review_result_id,
                context.task_id.to_string(),
                item.id.to_string(),
                claim.id.to_string(),
                request.decision.as_str(),
                serde_json::to_string(&request.evidence_ids)?,
                unix_micros(now),
            ],
        )?;
        transaction.execute(
            "UPDATE work_claims SET closed_at = ?2, close_reason = 'submitted'
             WHERE id = ?1 AND closed_at IS NULL",
            params![claim.id.to_string(), unix_micros(now)],
        )?;
        transaction.execute(
            "UPDATE work_attempts SET status = 'submitted', ended_at = ?2
             WHERE id = ?1 AND status = 'active'",
            params![attempt_id.to_string(), unix_micros(now)],
        )?;
        release_claim_reservations(transaction, claim.id, now)?;
        close_superseded_stage_claims(
            transaction,
            context.task_id,
            item.id,
            &[ClaimMode::Review],
            Some(claim.id),
            now,
        )?;

        let result_state = match request.decision {
            ReviewDecision::Approved => "submitted",
            ReviewDecision::ChangesRequested => {
                close_superseded_stage_claims(
                    transaction,
                    context.task_id,
                    item.id,
                    &[ClaimMode::Integrate],
                    None,
                    now,
                )?;
                transaction.execute(
                    "UPDATE stage_handoffs SET status='cancelled',updated_at=?3
                     WHERE task_id=?1 AND work_item_id=?2 AND status='reserved'",
                    params![
                        context.task_id.to_string(),
                        item.id.to_string(),
                        unix_micros(now)
                    ],
                )?;
                transaction.execute(
                    "UPDATE resource_reservations SET released_at = ?3
                     WHERE owner_type = 'change_set' AND released_at IS NULL
                       AND owner_id IN (
                         SELECT c.id FROM change_sets c
                         JOIN work_attempts a ON a.id = c.attempt_id
                         WHERE c.task_id = ?1 AND a.work_item_id = ?2
                           AND c.integration_state = 'pending'
                       )",
                    params![
                        context.task_id.to_string(),
                        item.id.to_string(),
                        unix_micros(now),
                    ],
                )?;
                transaction.execute(
                    "UPDATE change_sets SET integration_state = 'superseded', updated_at = ?3
                     WHERE task_id = ?1 AND integration_state = 'pending'
                       AND attempt_id IN (
                         SELECT id FROM work_attempts WHERE task_id = ?1 AND work_item_id = ?2
                       )",
                    params![
                        context.task_id.to_string(),
                        item.id.to_string(),
                        unix_micros(now),
                    ],
                )?;
                "open"
            }
        };
        let changed = transaction.execute(
            "UPDATE work_items SET result_state = ?3, version = version + 1,
                    updated_at = ?4, submitted_at = CASE WHEN ?3 = 'open' THEN NULL ELSE submitted_at END
             WHERE id = ?1 AND task_id = ?2 AND version = ?5",
            params![
                item.id.to_string(),
                context.task_id.to_string(),
                result_state,
                unix_micros(now),
                item.version,
            ],
        )?;
        if changed != 1 {
            return Err(SessionBridgeError::domain(
                ErrorCode::VersionConflict,
                "WorkItem changed while ReviewResult was being recorded",
            ));
        }
        bump_completion_revision(transaction, context.task_id, now)?;
        let event_sequence = append_session_event(
            transaction,
            context,
            authenticated,
            "review.completed",
            "review_result",
            &review_result_id,
            &serde_json::json!({
                "work_item_id": item.id,
                "claim_id": claim.id,
                "decision": request.decision,
                "evidence_ids": request.evidence_ids,
                "result_state": result_state,
            }),
            now,
        )?;
        if request.decision == ReviewDecision::Approved {
            reserve_stage_handoff(
                transaction,
                context,
                item.id,
                "review",
                "integrate",
                &authenticated.session.session_id.to_string(),
                now,
            )?;
        }
        let result = CompleteReviewResult {
            review_result_id,
            item_id: item.id,
            item_version: item.version + 1,
            decision: request.decision,
            result_state: result_state.into(),
            event_sequence,
        };
        idempotency::record(
            transaction,
            key,
            Some(&context.task_id.to_string()),
            payload_hash,
            &result,
            Some(event_sequence),
            now,
        )?;
        Ok(result)
    }

    pub fn integrate_change_set(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        request: IntegrateChangeSetRequest,
    ) -> Result<IntegrateChangeSetResult> {
        validate_request_id(&request.request_id)?;
        if !request.details.is_object() {
            return Err(invalid("integration details must be a JSON object"));
        }
        let payload_hash = idempotency::hash_payload(&serde_json::json!({
            "operation": "changeset.integrate",
            "request": request,
        }))?;
        let authenticated =
            AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let key = session_key(&authenticated, &request.request_id);
        if let Some(stored) = idempotency::replay(&transaction, key, &payload_hash)? {
            transaction.commit()?;
            return Ok(stored.value);
        }
        require_active_task(load_task(&transaction, context.task_id)?.phase)?;
        let (item_id, integration_state, change_set_head): (String, String, String) = transaction
            .query_row(
                "SELECT a.work_item_id, c.integration_state, c.head_commit
                 FROM change_sets c JOIN work_attempts a ON a.id = c.attempt_id
                 WHERE c.id = ?1 AND c.task_id = ?2",
                params![
                    request.change_set_id.to_string(),
                    context.task_id.to_string(),
                ],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?
            .ok_or_else(|| not_found("ChangeSet"))?;
        if integration_state != "pending" {
            return Err(SessionBridgeError::domain(
                ErrorCode::ItemNotReady,
                "ChangeSet is no longer pending integration",
            ));
        }
        let item_id: WorkItemId = parse_id(&item_id, "work_attempts.work_item_id")?;
        let item = load_work_item(&transaction, context.task_id, item_id)?;
        let approved: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM review_results
                WHERE task_id = ?1 AND work_item_id = ?2 AND decision = 'approved'
            )",
            params![context.task_id.to_string(), item.id.to_string()],
            |row| row.get(0),
        )?;
        if !approved {
            return Err(SessionBridgeError::domain(
                ErrorCode::ItemNotReady,
                "ChangeSet integration requires an approved ReviewResult",
            ));
        }
        let claim = load_claim(&transaction, context.task_id, request.claim_id)?;
        validate_claim_fence(&claim, &authenticated, request.generation, now, true)?;
        if claim.work_item_id != item.id || claim.mode != ClaimMode::Integrate {
            return Err(stale(
                "integration requires the current integrate Claim for this WorkItem",
            ));
        }
        if request.outcome == IntegrationOutcome::Integrated {
            let configured_target: Option<String> = transaction.query_row(
                "SELECT COALESCE(t.target_branch, s.target_branch)
                 FROM tasks t JOIN agent_sessions s ON s.id = ?2 AND s.task_id = t.id
                 WHERE t.id = ?1",
                params![
                    context.task_id.to_string(),
                    authenticated.session.session_id.to_string()
                ],
                |row| row.get(0),
            )?;
            let repository_path =
                authenticated
                    .session
                    .worktree_path
                    .as_ref()
                    .ok_or_else(|| {
                        SessionBridgeError::domain(
                            ErrorCode::TargetBranchNotIntegrated,
                            "Git integration verification requires a registered Agent worktree",
                        )
                    })?;
            let repository = GitRepository::open(repository_path)?;
            let target_branch = match configured_target {
                Some(branch) => branch,
                None => repository.current_branch()?.ok_or_else(|| {
                    SessionBridgeError::domain(
                        ErrorCode::TargetBranchNotIntegrated,
                        "Task has no target branch and the registered worktree is detached",
                    )
                })?,
            };
            repository.require_ancestor(&change_set_head, &target_branch)?;
        }
        let attempt_id = active_attempt_for_claim(&transaction, claim.id)?;
        transaction.execute(
            "UPDATE change_sets SET integration_state = ?3, integration_claim_id = ?4,
                    updated_at = ?5
             WHERE id = ?1 AND task_id = ?2 AND integration_state = 'pending'",
            params![
                request.change_set_id.to_string(),
                context.task_id.to_string(),
                request.outcome.as_str(),
                claim.id.to_string(),
                unix_micros(now),
            ],
        )?;
        transaction.execute(
            "UPDATE work_claims SET closed_at = ?2, close_reason = 'submitted'
             WHERE id = ?1 AND closed_at IS NULL",
            params![claim.id.to_string(), unix_micros(now)],
        )?;
        transaction.execute(
            "UPDATE work_attempts SET status = 'submitted', ended_at = ?2
             WHERE id = ?1 AND status = 'active'",
            params![attempt_id.to_string(), unix_micros(now)],
        )?;
        release_claim_reservations(&transaction, claim.id, now)?;
        close_superseded_stage_claims(
            &transaction,
            context.task_id,
            item.id,
            &[ClaimMode::Integrate],
            Some(claim.id),
            now,
        )?;
        transaction.execute(
            "UPDATE resource_reservations SET released_at = ?2
             WHERE owner_type = 'change_set' AND owner_id = ?1 AND released_at IS NULL",
            params![request.change_set_id.to_string(), unix_micros(now)],
        )?;

        let all_integrated = request.outcome == IntegrationOutcome::Integrated
            && !transaction.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM change_sets c JOIN work_attempts a ON a.id = c.attempt_id
                    WHERE c.task_id = ?1 AND a.work_item_id = ?2
                      AND c.integration_state NOT IN ('integrated', 'superseded')
                )",
                params![context.task_id.to_string(), item.id.to_string()],
                |row| row.get::<_, bool>(0),
            )?;
        let result_state = if all_integrated {
            "accepted"
        } else {
            "submitted"
        };
        let changed = transaction.execute(
            "UPDATE work_items SET result_state = ?3, version = version + 1,
                    accepted_at = CASE WHEN ?3 = 'accepted' THEN ?4 ELSE accepted_at END,
                    updated_at = ?4
             WHERE id = ?1 AND task_id = ?2 AND version = ?5",
            params![
                item.id.to_string(),
                context.task_id.to_string(),
                result_state,
                unix_micros(now),
                item.version,
            ],
        )?;
        if changed != 1 {
            return Err(SessionBridgeError::domain(
                ErrorCode::VersionConflict,
                "WorkItem changed while ChangeSet integration was being recorded",
            ));
        }
        bump_completion_revision(&transaction, context.task_id, now)?;
        let event_sequence = append_session_event(
            &transaction,
            context,
            &authenticated,
            "changeset.integration_completed",
            "change_set",
            &request.change_set_id.to_string(),
            &serde_json::json!({
                "work_item_id": item.id,
                "claim_id": claim.id,
                "outcome": request.outcome,
                "details": request.details,
                "result_state": result_state,
            }),
            now,
        )?;
        if result_state == "accepted" {
            await_service::resolve_accepted_work_item(
                &transaction,
                context,
                item.id,
                event_sequence,
                &authenticated.session.session_id.to_string(),
                now,
            )?;
        }
        let result = IntegrateChangeSetResult {
            change_set_id: request.change_set_id,
            integration_state: request.outcome.as_str().into(),
            item_id: item.id,
            item_version: item.version + 1,
            result_state: result_state.into(),
            event_sequence,
        };
        idempotency::record(
            &transaction,
            key,
            Some(&context.task_id.to_string()),
            &payload_hash,
            &result,
            Some(event_sequence),
            now,
        )?;
        transaction.commit()?;
        self.reconcile_after_write(store, context)?;
        Ok(result)
    }

    pub fn inspect(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        item_id: WorkItemId,
    ) -> Result<WorkItemInspection> {
        AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)?;
        let connection = store.connection();
        let item = load_work_item(connection, context.task_id, item_id)?;
        let item_value = serde_json::json!({
            "id": item.id,
            "task_id": context.task_id,
            "kind": item.kind,
            "title": item.title,
            "description": item.description,
            "required": item.required,
            "priority": item.priority,
            "risk": item.risk,
            "plan_state": item.plan_state,
            "result_state": item.result_state,
            "version": item.version,
            "planned_resources": serde_json::from_str::<serde_json::Value>(&item.planned_resources_json)?,
        });
        Ok(WorkItemInspection {
            item: item_value,
            dependencies: query_json_rows(
                connection,
                "SELECT json_object('id', d.id, 'title', d.title, 'result_state', d.result_state,
                                    'version', d.version)
                 FROM work_relations r JOIN work_items d ON d.id = r.to_id
                 WHERE r.task_id = ?1 AND r.from_id = ?2 AND r.relation_type = 'depends_on'
                 ORDER BY d.id",
                context.task_id,
                item_id,
            )?,
            claims: query_json_rows(
                connection,
                "SELECT json_object('id', id, 'session_id', session_id, 'mode', mode,
                                    'generation', generation, 'lease_expires_at', lease_expires_at,
                                    'closed_at', closed_at, 'close_reason', close_reason)
                 FROM work_claims WHERE task_id = ?1 AND work_item_id = ?2 ORDER BY claimed_at",
                context.task_id,
                item_id,
            )?,
            attempts: query_json_rows(
                connection,
                "SELECT json_object('id', id, 'claim_id', claim_id, 'session_id', session_id,
                                    'base_plan_version', base_plan_version, 'base_commit', base_commit,
                                    'status', status, 'started_at', started_at, 'ended_at', ended_at)
                 FROM work_attempts WHERE task_id = ?1 AND work_item_id = ?2 ORDER BY started_at",
                context.task_id,
                item_id,
            )?,
            artifacts: query_json_rows(
                connection,
                "SELECT json_object('id', id, 'attempt_id', attempt_id, 'kind', kind,
                                    'value', value, 'digest', digest, 'revision', revision)
                 FROM artifacts WHERE task_id = ?1 AND work_item_id = ?2 ORDER BY created_at",
                context.task_id,
                item_id,
            )?,
            change_sets: connection
                .prepare(
                    "SELECT json_object('id', c.id, 'attempt_id', c.attempt_id,
                                        'base_commit', c.base_commit, 'head_commit', c.head_commit,
                                        'changed_resources', json(c.changed_resources_json),
                                        'tests', json(c.tests_json),
                                        'integration_state', c.integration_state)
                     FROM change_sets c JOIN work_attempts a ON a.id = c.attempt_id
                     WHERE c.task_id = ?1 AND a.work_item_id = ?2 ORDER BY c.created_at",
                )?
                .query_map(params![context.task_id.to_string(), item_id.to_string()], |row| {
                    row.get::<_, String>(0)
                })?
                .map(|row| {
                    let value = row?;
                    serde_json::from_str(&value).map_err(rusqlite_json_error)
                })
                .collect::<std::result::Result<Vec<_>, _>>()?,
        })
    }
}

#[derive(Debug)]
struct TaskHead {
    phase: TaskPhase,
    plan_version: i64,
}

#[derive(Debug)]
struct WorkItemRow {
    id: WorkItemId,
    kind: String,
    title: String,
    description: String,
    required: bool,
    priority: i64,
    risk: String,
    required_capabilities_json: String,
    planned_resources_json: String,
    plan_state: String,
    result_state: String,
    version: i64,
}

#[derive(Debug)]
struct ClaimRow {
    id: ClaimId,
    work_item_id: WorkItemId,
    session_id: SessionId,
    mode: ClaimMode,
    generation: i64,
    lease_expires_at: i64,
    closed_at: Option<i64>,
}

#[derive(Debug)]
struct AttemptRow {
    id: AttemptId,
    claim_id: ClaimId,
    work_item_id: WorkItemId,
    base_commit: Option<String>,
    status: String,
}

fn load_task(connection: &Connection, task_id: TaskId) -> Result<TaskHead> {
    let row: Option<(String, i64)> = connection
        .query_row(
            "SELECT phase, plan_version FROM tasks WHERE id = ?1",
            [task_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((phase, plan_version)) = row else {
        return Err(not_found("Task"));
    };
    Ok(TaskHead {
        phase: phase.parse().map_err(database_value_error("tasks.phase"))?,
        plan_version,
    })
}

fn load_work_item(
    connection: &Connection,
    task_id: TaskId,
    item_id: WorkItemId,
) -> Result<WorkItemRow> {
    #[allow(clippy::type_complexity)]
    let row: Option<(
        String,
        String,
        String,
        i64,
        i64,
        String,
        String,
        String,
        String,
        String,
        i64,
    )> = connection
        .query_row(
            "SELECT kind, title, description, required, priority, risk,
                    required_capabilities_json, planned_resources_json,
                    plan_state, result_state, version
             FROM work_items WHERE id = ?1 AND task_id = ?2",
            params![item_id.to_string(), task_id.to_string()],
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
    let Some(row) = row else {
        return Err(not_found("WorkItem"));
    };
    Ok(WorkItemRow {
        id: item_id,
        kind: row.0,
        title: row.1,
        description: row.2,
        required: row.3 != 0,
        priority: row.4,
        risk: row.5,
        required_capabilities_json: row.6,
        planned_resources_json: row.7,
        plan_state: row.8,
        result_state: row.9,
        version: row.10,
    })
}

fn load_claim(connection: &Connection, task_id: TaskId, claim_id: ClaimId) -> Result<ClaimRow> {
    let row: Option<(String, String, String, i64, i64, Option<i64>)> = connection
        .query_row(
            "SELECT work_item_id, session_id, mode, generation, lease_expires_at, closed_at
             FROM work_claims WHERE id = ?1 AND task_id = ?2",
            params![claim_id.to_string(), task_id.to_string()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;
    let Some(row) = row else {
        return Err(not_found("WorkClaim"));
    };
    Ok(ClaimRow {
        id: claim_id,
        work_item_id: parse_id(&row.0, "work_claims.work_item_id")?,
        session_id: parse_id(&row.1, "work_claims.session_id")?,
        mode: row
            .2
            .parse()
            .map_err(database_value_error("work_claims.mode"))?,
        generation: row.3,
        lease_expires_at: row.4,
        closed_at: row.5,
    })
}

fn load_attempt(
    connection: &Connection,
    task_id: TaskId,
    attempt_id: AttemptId,
) -> Result<AttemptRow> {
    load_attempt_where(connection, task_id, "a.id = ?2", &attempt_id.to_string())
}

fn load_attempt_by_claim(
    connection: &Connection,
    task_id: TaskId,
    claim_id: ClaimId,
) -> Result<AttemptRow> {
    load_attempt_where(
        connection,
        task_id,
        "a.claim_id = ?2",
        &claim_id.to_string(),
    )
}

fn load_attempt_where(
    connection: &Connection,
    task_id: TaskId,
    predicate: &str,
    value: &str,
) -> Result<AttemptRow> {
    let sql = format!(
        "SELECT a.id, a.claim_id, a.work_item_id, a.base_commit, a.status
         FROM work_attempts a WHERE a.task_id = ?1 AND {predicate}"
    );
    let row: Option<(String, String, String, Option<String>, String)> = connection
        .query_row(&sql, params![task_id.to_string(), value], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .optional()?;
    let Some(row) = row else {
        return Err(not_found("WorkAttempt"));
    };
    Ok(AttemptRow {
        id: parse_id(&row.0, "work_attempts.id")?,
        claim_id: parse_id(&row.1, "work_attempts.claim_id")?,
        work_item_id: parse_id(&row.2, "work_attempts.work_item_id")?,
        base_commit: row.3,
        status: row.4,
    })
}

fn validate_claimable_item(
    connection: &Connection,
    item: &WorkItemRow,
    authenticated: &AuthenticatedAgent,
    request: &ClaimWorkItemRequest,
) -> Result<()> {
    if item.version != request.expected_item_version {
        return Err(SessionBridgeError::domain(
            ErrorCode::VersionConflict,
            "WorkItem version changed before Claim acquisition",
        )
        .with_details(serde_json::json!({
            "expected_version": request.expected_item_version,
            "actual_version": item.version,
        })));
    }
    if item.plan_state != "active" {
        return Err(SessionBridgeError::domain(
            ErrorCode::ItemNotReady,
            "WorkItem is not active",
        ));
    }
    match request.mode {
        ClaimMode::Execute if item.result_state != "open" => {
            return Err(SessionBridgeError::domain(
                ErrorCode::ItemNotReady,
                "execute Claim requires an open WorkItem",
            ));
        }
        ClaimMode::Review | ClaimMode::Integrate if item.result_state != "submitted" => {
            return Err(SessionBridgeError::domain(
                ErrorCode::ItemNotReady,
                "review and integrate Claims require a submitted WorkItem",
            ));
        }
        _ => {}
    }
    let incomplete_dependency: Option<(String, String)> = connection
        .query_row(
            "SELECT d.id, d.result_state
             FROM work_relations r JOIN work_items d ON d.id = r.to_id AND d.task_id = r.task_id
             WHERE r.task_id = ?1 AND r.from_id = ?2 AND r.relation_type = 'depends_on'
               AND d.result_state <> 'accepted' LIMIT 1",
            params![
                authenticated.session.task_id.to_string(),
                item.id.to_string()
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((dependency_id, state)) = incomplete_dependency {
        return Err(SessionBridgeError::domain(
            ErrorCode::ItemNotReady,
            "WorkItem dependency is not accepted",
        )
        .with_details(serde_json::json!({
            "dependency_id": dependency_id,
            "result_state": state,
        })));
    }
    let required: Vec<String> = serde_json::from_str(&item.required_capabilities_json)?;
    let mut available = authenticated.session.declared_capabilities.clone();
    available.extend(authenticated.session.granted_capabilities.clone());
    available.sort();
    available.dedup();
    let missing: Vec<_> = required
        .into_iter()
        .filter(|required| !available.contains(required))
        .collect();
    if !missing.is_empty() {
        return Err(SessionBridgeError::domain(
            ErrorCode::CapabilityMismatch,
            "Agent Session lacks required WorkItem capabilities",
        )
        .with_details(serde_json::json!({ "missing_capabilities": missing })));
    }
    let blocker: Option<String> = connection
        .query_row(
            "SELECT id FROM blockers WHERE task_id = ?1 AND work_item_id = ?2 AND status = 'open' LIMIT 1",
            params![authenticated.session.task_id.to_string(), item.id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(blocker_id) = blocker {
        return Err(SessionBridgeError::domain(
            ErrorCode::OpenBlocker,
            "WorkItem has an open Blocker",
        )
        .with_details(serde_json::json!({ "blocker_id": blocker_id })));
    }
    let invalidation: Option<String> = connection
        .query_row(
            "SELECT id FROM invalidations
             WHERE task_id = ?1 AND target_type = 'work_item' AND target_id = ?2
               AND status = 'open' LIMIT 1",
            params![
                authenticated.session.task_id.to_string(),
                item.id.to_string()
            ],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(invalidation_id) = invalidation {
        return Err(SessionBridgeError::domain(
            ErrorCode::InputInvalidated,
            "WorkItem has an open Invalidation",
        )
        .with_details(serde_json::json!({ "invalidation_id": invalidation_id })));
    }
    Ok(())
}

fn require_writable_participant(
    transaction: &Transaction<'_>,
    authenticated: &AuthenticatedAgent,
    now: OffsetDateTime,
) -> Result<ParticipantState> {
    if authenticated.participant.kind != ParticipantKind::Worker {
        return Err(SessionBridgeError::domain(
            ErrorCode::CapabilityNotGranted,
            "observer participants cannot modify task work",
        ));
    }
    match authenticated.participant.state {
        ParticipantState::Active => Ok(ParticipantState::Active),
        ParticipantState::Waiting => {
            let changed = transaction.execute(
                "UPDATE task_participants SET state = 'active', version = version + 1,
                        updated_at = ?2
                 WHERE id = ?1 AND state = 'waiting' AND version = ?3",
                params![
                    authenticated.participant.participant_id.to_string(),
                    unix_micros(now),
                    authenticated.participant.version,
                ],
            )?;
            if changed != 1 {
                return Err(SessionBridgeError::domain(
                    ErrorCode::ParticipantVersionChanged,
                    "TaskParticipant changed before work acquisition",
                ));
            }
            Ok(ParticipantState::Active)
        }
        ParticipantState::Concluded => Err(SessionBridgeError::domain(
            ErrorCode::SessionAlreadyConcluded,
            "concluded participant must resume before modifying work",
        )),
        ParticipantState::Blocked => Err(SessionBridgeError::domain(
            ErrorCode::ParticipantBlocked,
            "blocked participant must resume before modifying work",
        )),
        ParticipantState::Departed => Err(SessionBridgeError::domain(
            ErrorCode::ParticipantDeparted,
            "departed participant cannot modify work",
        )),
        ParticipantState::Retired | ParticipantState::Superseded => {
            Err(SessionBridgeError::domain(
                ErrorCode::ParticipantRetired,
                "terminal participant cannot modify work",
            ))
        }
    }
}

fn refresh_execute_worktree_baseline(
    transaction: &Transaction<'_>,
    task_id: TaskId,
    authenticated: &AuthenticatedAgent,
    mode: ClaimMode,
    adopt_existing: bool,
    now: OffsetDateTime,
) -> Result<Option<String>> {
    if mode != ClaimMode::Execute
        || authenticated
            .session
            .base_commit
            .as_deref()
            .is_none_or(str::is_empty)
    {
        return Ok(authenticated.session.head_commit.clone());
    }
    let worktree_path = authenticated
        .session
        .worktree_path
        .as_deref()
        .ok_or_else(|| {
            SessionBridgeError::domain(
                ErrorCode::WorktreeConflict,
                "Git-backed execute Claim requires a registered worktree",
            )
        })?;
    let repository = GitRepository::open(worktree_path)?;
    repository.require_clean()?;
    let current_branch = repository.current_branch()?.ok_or_else(|| {
        SessionBridgeError::domain(
            ErrorCode::WorktreeConflict,
            "registered execute worktree is detached",
        )
    })?;
    if authenticated.session.branch.as_deref() != Some(current_branch.as_str()) {
        return Err(SessionBridgeError::domain(
            ErrorCode::WorktreeConflict,
            "registered execute worktree is on a different branch",
        )
        .with_details(serde_json::json!({
            "expected_branch": authenticated.session.branch,
            "actual_branch": current_branch,
            "worktree_path": worktree_path,
        })));
    }
    let head_commit = repository.head_commit()?;
    let target_branch = transaction
        .query_row(
            "SELECT target_branch FROM tasks WHERE id=?1",
            [task_id.to_string()],
            |row| row.get::<_, Option<String>>(0),
        )?
        .or_else(|| authenticated.session.target_branch.clone())
        .or(repository.current_branch()?)
        .ok_or_else(|| {
            SessionBridgeError::domain(
                ErrorCode::WorktreeConflict,
                "Git-backed execute work requires a Task target branch",
            )
        })?;
    let target_head = repository.resolve_revision(&target_branch)?;
    if head_commit != target_head {
        repository
            .require_ancestor(&target_head, &head_commit)
            .map_err(|error| {
                SessionBridgeError::domain(
                    ErrorCode::WorktreeConflict,
                    "registered worktree is not based on the current Task target head",
                )
                .with_details(serde_json::json!({
                    "target_branch": target_branch,
                    "target_head": target_head,
                    "worktree_head": head_commit,
                    "worktree_path": worktree_path,
                    "cause": error.to_string(),
                    "recovery_options": [{
                        "action": "rebase_worktree",
                        "safe": false,
                        "reason": "the target branch advanced or the worktree diverged"
                    }]
                }))
            })?;
        if !adopt_existing {
            return Err(SessionBridgeError::domain(
                ErrorCode::WorktreeConflict,
                "registered worktree already contains commits that are not owned by an active WorkAttempt",
            )
            .with_details(serde_json::json!({
                "target_branch": target_branch,
                "target_head": target_head,
                "worktree_head": head_commit,
                "worktree_path": worktree_path,
                "recovery_options": [{
                    "action": "adopt_existing_work",
                    "command": "sessionbridge work adopt <WORK_ITEM>",
                    "safe": true
                }]
            })));
        }
    }
    let changed = transaction.execute(
        "UPDATE agent_sessions SET head_commit = ?2, updated_at = ?3
         WHERE id = ?1 AND ended_at IS NULL",
        params![
            authenticated.session.session_id.to_string(),
            head_commit,
            unix_micros(now),
        ],
    )?;
    if changed != 1 {
        return Err(SessionBridgeError::domain(
            ErrorCode::ParticipantVersionChanged,
            "Agent Session ended before execute Claim acquisition",
        ));
    }
    Ok(Some(target_head))
}

fn require_active_task(phase: TaskPhase) -> Result<()> {
    if phase == TaskPhase::Active {
        Ok(())
    } else {
        Err(task_phase_error(phase))
    }
}

fn task_phase_error(phase: TaskPhase) -> SessionBridgeError {
    SessionBridgeError::domain(
        if phase == TaskPhase::Paused {
            ErrorCode::TaskPaused
        } else {
            ErrorCode::TaskNotActive
        },
        format!("Task is {phase} and cannot accept this work operation"),
    )
}

#[derive(Debug)]
struct ClaimConflictRow {
    id: String,
    session_id: String,
    generation: i64,
    lease_expires_at: i64,
}

fn load_open_claim_conflict(
    connection: &Connection,
    item_id: WorkItemId,
    mode: ClaimMode,
    session_id: SessionId,
) -> Result<Option<ClaimConflictRow>> {
    let sql = if mode == ClaimMode::Execute {
        "SELECT id, session_id, generation, lease_expires_at FROM work_claims
         WHERE work_item_id = ?1 AND mode = 'execute' AND closed_at IS NULL LIMIT 1"
    } else {
        "SELECT id, session_id, generation, lease_expires_at FROM work_claims
         WHERE work_item_id = ?1 AND mode = ?2 AND session_id = ?3
           AND closed_at IS NULL LIMIT 1"
    };
    let row = if mode == ClaimMode::Execute {
        connection
            .query_row(sql, [item_id.to_string()], |row| {
                Ok(ClaimConflictRow {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    generation: row.get(2)?,
                    lease_expires_at: row.get(3)?,
                })
            })
            .optional()?
    } else {
        connection
            .query_row(
                sql,
                params![item_id.to_string(), mode.as_str(), session_id.to_string()],
                |row| {
                    Ok(ClaimConflictRow {
                        id: row.get(0)?,
                        session_id: row.get(1)?,
                        generation: row.get(2)?,
                        lease_expires_at: row.get(3)?,
                    })
                },
            )
            .optional()?
    };
    Ok(row)
}

fn validate_claim_fence(
    claim: &ClaimRow,
    authenticated: &AuthenticatedAgent,
    generation: i64,
    now: OffsetDateTime,
    require_unexpired: bool,
) -> Result<()> {
    if claim.session_id != authenticated.session.session_id
        || claim.generation != generation
        || claim.closed_at.is_some()
    {
        return Err(stale_claim_error(claim, generation, now));
    }
    if require_unexpired && claim.lease_expires_at <= unix_micros(now) {
        return Err(stale_claim_error(claim, generation, now));
    }
    Ok(())
}

fn stale_claim_error(claim: &ClaimRow, generation: i64, now: OffsetDateTime) -> SessionBridgeError {
    let expired = claim.lease_expires_at <= unix_micros(now);
    SessionBridgeError::domain(
        if expired {
            ErrorCode::ClaimExpired
        } else {
            ErrorCode::StaleClaim
        },
        "Claim ownership, generation, lease, or lifecycle no longer authorizes this write",
    )
    .with_details(serde_json::json!({
        "claim_id": claim.id,
        "expected_generation": claim.generation,
        "provided_generation": generation,
        "lease_expires_at": claim.lease_expires_at,
        "closed": claim.closed_at.is_some(),
    }))
}

fn active_attempt_for_claim(connection: &Connection, claim_id: ClaimId) -> Result<AttemptId> {
    let value: Option<String> = connection
        .query_row(
            "SELECT id FROM work_attempts WHERE claim_id = ?1 AND status = 'active'",
            [claim_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    value
        .map(|value| parse_id(&value, "work_attempts.id"))
        .transpose()?
        .ok_or_else(|| stale("Claim no longer has an active WorkAttempt"))
}

fn pin_attempt_inputs(
    transaction: &Transaction<'_>,
    attempt_id: AttemptId,
    item: &WorkItemRow,
    inputs: &[AttemptInputPin],
    now: OffsetDateTime,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO attempt_input_pins (
            attempt_id, input_type, input_id, revision_or_digest, created_at
         ) VALUES (?1, 'work_item', ?2, ?3, ?4)",
        params![
            attempt_id.to_string(),
            item.id.to_string(),
            item.version.to_string(),
            unix_micros(now),
        ],
    )?;
    for input in inputs {
        transaction.execute(
            "INSERT INTO attempt_input_pins (
                attempt_id, input_type, input_id, revision_or_digest, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                attempt_id.to_string(),
                input.input_type,
                input.input_id,
                input.revision_or_digest,
                unix_micros(now),
            ],
        )?;
    }
    Ok(())
}

fn copy_attempt_pins(
    transaction: &Transaction<'_>,
    old_attempt: AttemptId,
    new_attempt: AttemptId,
    now: OffsetDateTime,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO attempt_input_pins (
            attempt_id, input_type, input_id, revision_or_digest, created_at
         ) SELECT ?2, input_type, input_id, revision_or_digest, ?3
           FROM attempt_input_pins WHERE attempt_id = ?1",
        params![
            old_attempt.to_string(),
            new_attempt.to_string(),
            unix_micros(now)
        ],
    )?;
    Ok(())
}

fn validate_attempt_inputs(connection: &Connection, attempt: &AttemptRow) -> Result<()> {
    let item_pin: Option<(String, String)> = connection
        .query_row(
            "SELECT input_id, revision_or_digest FROM attempt_input_pins
             WHERE attempt_id = ?1 AND input_type = 'work_item' LIMIT 1",
            [attempt.id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((item_id, pinned_version)) = item_pin {
        let current: i64 = connection.query_row(
            "SELECT version FROM work_items WHERE id = ?1",
            [item_id.clone()],
            |row| row.get(0),
        )?;
        if current.to_string() != pinned_version {
            return Err(SessionBridgeError::domain(
                ErrorCode::InputInvalidated,
                "WorkItem changed after the WorkAttempt started",
            )
            .with_details(serde_json::json!({
                "work_item_id": item_id,
                "pinned_version": pinned_version,
                "current_version": current,
            })));
        }
    }
    let open_invalidation: Option<String> = connection
        .query_row(
            "SELECT id FROM invalidations
             WHERE task_id = (SELECT task_id FROM work_attempts WHERE id = ?1)
               AND target_type = 'attempt' AND target_id = ?1 AND status = 'open' LIMIT 1",
            [attempt.id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = open_invalidation {
        return Err(SessionBridgeError::domain(
            ErrorCode::InputInvalidated,
            "WorkAttempt has an open Invalidation",
        )
        .with_details(serde_json::json!({ "invalidation_id": id })));
    }
    Ok(())
}

fn insert_reservations(
    transaction: &Transaction<'_>,
    task_id: TaskId,
    owner_type: &str,
    owner_id: &str,
    resources: &[ResourceRequest],
    now: OffsetDateTime,
) -> Result<()> {
    for resource in resources {
        transaction.execute(
            "INSERT INTO resource_reservations (
                id, task_id, owner_type, owner_id, resource_key, mode, policy,
                pinned_revision, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                uuid::Uuid::now_v7().to_string(),
                task_id.to_string(),
                owner_type,
                owner_id,
                resource.resource_key,
                resource.mode,
                resource.policy,
                resource.pinned_revision,
                unix_micros(now),
            ],
        )?;
    }
    Ok(())
}

fn validate_resource_conflicts(
    connection: &Connection,
    task_id: TaskId,
    excluded_claim_id: Option<ClaimId>,
    excluded_change_set_ids: &[String],
    requested: &[ResourceRequest],
) -> Result<()> {
    let rows = connection
        .prepare(
            "SELECT owner_type, owner_id, resource_key, mode, policy, pinned_revision
             FROM resource_reservations
             WHERE task_id = ?1 AND released_at IS NULL",
        )?
        .query_map([task_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for request in requested {
        for (owner_type, owner_id, key, mode, policy, pinned) in &rows {
            if owner_type == "claim"
                && excluded_claim_id.is_some_and(|id| owner_id == &id.to_string())
            {
                continue;
            }
            if owner_type == "change_set" && excluded_change_set_ids.contains(owner_id) {
                continue;
            }
            if request.policy != "enforced" || policy != "enforced" {
                continue;
            }
            if resources_overlap(&request.resource_key, key)
                && access_conflicts(
                    &request.mode,
                    request.pinned_revision.as_deref(),
                    mode,
                    pinned.as_deref(),
                )
            {
                return Err(SessionBridgeError::domain(
                    ErrorCode::ResourceConflict,
                    "requested resource conflicts with an active enforced Reservation",
                )
                .with_details(serde_json::json!({
                    "resource_key": request.resource_key,
                    "requested_mode": request.mode,
                    "owner_type": owner_type,
                    "owner_id": owner_id,
                    "conflicting_resource_key": key,
                    "conflicting_mode": mode,
                })));
            }
        }
    }
    Ok(())
}

fn access_conflicts(
    left_mode: &str,
    left_pin: Option<&str>,
    right_mode: &str,
    right_pin: Option<&str>,
) -> bool {
    if left_mode == "read" && right_mode == "read" {
        return false;
    }
    if (left_mode == "read" || right_mode == "read")
        && left_pin.is_some()
        && right_pin.is_some()
        && left_pin == right_pin
        && left_mode != "exclusive"
        && right_mode != "exclusive"
    {
        return false;
    }
    true
}

pub(crate) fn resources_overlap(left: &str, right: &str) -> bool {
    match (left.strip_prefix("path:"), right.strip_prefix("path:")) {
        (Some(left), Some(right)) => path_is_prefix(left, right) || path_is_prefix(right, left),
        _ => left == right,
    }
}

fn path_is_prefix(prefix: &str, value: &str) -> bool {
    prefix == value
        || value
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn load_claim_reservations(
    connection: &Connection,
    claim_id: ClaimId,
) -> Result<Vec<ResourceRequest>> {
    connection
        .prepare(
            "SELECT resource_key, mode, policy, pinned_revision
             FROM resource_reservations
             WHERE owner_type = 'claim' AND owner_id = ?1 AND released_at IS NULL
             ORDER BY resource_key",
        )?
        .query_map([claim_id.to_string()], |row| {
            Ok(ResourceRequest {
                resource_key: row.get(0)?,
                mode: row.get(1)?,
                policy: row.get(2)?,
                pinned_revision: row.get(3)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn release_claim_reservations(
    transaction: &Transaction<'_>,
    claim_id: ClaimId,
    now: OffsetDateTime,
) -> Result<()> {
    transaction.execute(
        "UPDATE resource_reservations SET released_at = ?2
         WHERE owner_type = 'claim' AND owner_id = ?1 AND released_at IS NULL",
        params![claim_id.to_string(), unix_micros(now)],
    )?;
    Ok(())
}

fn close_superseded_stage_claims(
    transaction: &Transaction<'_>,
    task_id: TaskId,
    work_item_id: WorkItemId,
    modes: &[ClaimMode],
    except_claim_id: Option<ClaimId>,
    now: OffsetDateTime,
) -> Result<usize> {
    let mut closed = 0;
    for mode in modes {
        let claims = transaction
            .prepare(
                "SELECT id FROM work_claims
                 WHERE task_id=?1 AND work_item_id=?2 AND mode=?3 AND closed_at IS NULL",
            )?
            .query_map(
                params![task_id.to_string(), work_item_id.to_string(), mode.as_str()],
                |row| row.get::<_, String>(0),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for claim_id in claims {
            if except_claim_id.is_some_and(|except| except.to_string() == claim_id) {
                continue;
            }
            transaction.execute(
                "UPDATE work_claims SET closed_at=?2,close_reason='revoked'
                 WHERE id=?1 AND closed_at IS NULL",
                params![claim_id, unix_micros(now)],
            )?;
            transaction.execute(
                "UPDATE work_attempts SET status='superseded',ended_at=?2
                 WHERE claim_id=?1 AND status='active'",
                params![claim_id, unix_micros(now)],
            )?;
            transaction.execute(
                "UPDATE resource_reservations SET released_at=?2
                 WHERE owner_type='claim' AND owner_id=?1 AND released_at IS NULL",
                params![claim_id, unix_micros(now)],
            )?;
            closed += 1;
        }
    }
    Ok(closed)
}

fn validate_changed_scope(
    connection: &Connection,
    claim_id: ClaimId,
    changed_resources: &[String],
) -> Result<()> {
    let reservations = load_claim_reservations(connection, claim_id)?;
    for changed in changed_resources {
        let covered = reservations.iter().any(|reservation| {
            matches!(reservation.mode.as_str(), "write" | "exclusive")
                && resources_overlap(&reservation.resource_key, changed)
        });
        if !covered {
            return Err(SessionBridgeError::domain(
                ErrorCode::ScopeDrift,
                "actual changed resource is outside the Claim's writable scope",
            )
            .with_details(serde_json::json!({
                "resource_key": changed,
                "claim_id": claim_id,
            })));
        }
    }
    Ok(())
}

fn record_resource_usage(
    transaction: &Transaction<'_>,
    task_id: TaskId,
    attempt_id: AttemptId,
    resources: &[String],
    now: OffsetDateTime,
) -> Result<()> {
    for resource in resources {
        transaction.execute(
            "INSERT OR IGNORE INTO resource_usage (
                id, task_id, attempt_id, resource_key, mode, created_at
             ) VALUES (?1, ?2, ?3, ?4, 'write', ?5)",
            params![
                uuid::Uuid::now_v7().to_string(),
                task_id.to_string(),
                attempt_id.to_string(),
                resource,
                unix_micros(now),
            ],
        )?;
    }
    Ok(())
}

fn insert_artifacts(
    transaction: &Transaction<'_>,
    context: &SessionContext,
    authenticated: &AuthenticatedAgent,
    default_attempt_id: Option<AttemptId>,
    default_item_id: Option<WorkItemId>,
    artifacts: &[ArtifactInput],
    now: OffsetDateTime,
) -> Result<Vec<ArtifactId>> {
    let mut ids = Vec::new();
    for artifact in artifacts {
        validate_artifact(artifact)?;
        let id = ArtifactId::new();
        let attempt_id = artifact.attempt_id.or(default_attempt_id);
        let item_id = artifact.work_item_id.or(default_item_id);
        transaction.execute(
            "INSERT INTO artifacts (
                id, task_id, attempt_id, work_item_id, session_id, kind, value,
                digest, revision, metadata_json, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                id.to_string(),
                context.task_id.to_string(),
                attempt_id.map(|id| id.to_string()),
                item_id.map(|id| id.to_string()),
                authenticated.session.session_id.to_string(),
                artifact.kind.trim(),
                artifact.value.trim(),
                artifact.digest,
                artifact.revision,
                serde_json::to_string(&artifact.metadata)?,
                unix_micros(now),
            ],
        )?;
        ids.push(id);
    }
    Ok(ids)
}

fn insert_evidence(
    transaction: &Transaction<'_>,
    context: &SessionContext,
    authenticated: &AuthenticatedAgent,
    default_attempt_id: Option<AttemptId>,
    evidence: &[EvidenceInput],
    now: OffsetDateTime,
) -> Result<Vec<EvidenceId>> {
    let mut ids = Vec::new();
    for input in evidence {
        validate_evidence(input)?;
        let id = EvidenceId::new();
        transaction.execute(
            "INSERT INTO evidence (
                id, task_id, attempt_id, artifact_id, kind, result,
                details_json, created_by_session_id, created_at,
                subject_type, subject_id, repository_identity, commit_hash,
                verifier, environment_json, observed_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                       ?10, ?11, ?12, ?13, ?14, ?15, ?9)",
            params![
                id.to_string(),
                context.task_id.to_string(),
                input
                    .attempt_id
                    .or(default_attempt_id)
                    .map(|id| id.to_string()),
                input.artifact_id.map(|id| id.to_string()),
                input.kind.trim(),
                input.result,
                serde_json::to_string(&input.details)?,
                authenticated.session.session_id.to_string(),
                unix_micros(now),
                input.subject_type.as_deref().map(str::trim),
                input.subject_id.as_deref().map(str::trim),
                context.repository_instance_id,
                input.commit.as_deref().map(str::trim),
                input.verifier.as_deref().map(str::trim),
                serde_json::to_string(&input.environment)?,
            ],
        )?;
        ids.push(id);
    }
    Ok(ids)
}

fn reserve_stage_handoff(
    connection: &Connection,
    context: &SessionContext,
    item_id: WorkItemId,
    from_stage: &str,
    to_stage: &str,
    coordinator_session_id: &str,
    now: OffsetDateTime,
) -> Result<()> {
    let expires_at = now + Duration::minutes(2);
    connection.execute(
        "UPDATE stage_handoffs SET status='cancelled',updated_at=?3
         WHERE work_item_id=?1 AND to_stage=?2 AND status='reserved'",
        params![item_id.to_string(), to_stage, unix_micros(now)],
    )?;
    connection.execute(
        "INSERT INTO stage_handoffs (
            id,task_id,work_item_id,from_stage,to_stage,coordinator_session_id,
            status,expires_at,created_at,updated_at
         ) VALUES (?1,?2,?3,?4,?5,?6,'reserved',?7,?8,?8)",
        params![
            uuid::Uuid::now_v7().to_string(),
            context.task_id.to_string(),
            item_id.to_string(),
            from_stage,
            to_stage,
            coordinator_session_id,
            unix_micros(expires_at),
            unix_micros(now),
        ],
    )?;
    Ok(())
}

fn validate_evidence_ids(
    connection: &Connection,
    task_id: TaskId,
    evidence_ids: &[EvidenceId],
) -> Result<()> {
    for evidence_id in evidence_ids {
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM evidence WHERE id = ?1 AND task_id = ?2)",
            params![evidence_id.to_string(), task_id.to_string()],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(SessionBridgeError::domain(
                ErrorCode::NotFound,
                "Review evidence was not found in this Task",
            )
            .with_details(serde_json::json!({ "evidence_id": evidence_id })));
        }
    }
    Ok(())
}

fn transfer_reservations_to_change_set(
    transaction: &Transaction<'_>,
    claim_id: ClaimId,
    change_set_id: ChangeSetId,
    changed_resources: &[String],
    now: OffsetDateTime,
) -> Result<()> {
    let reservations = load_claim_reservations(transaction, claim_id)?;
    for reservation in reservations {
        let retains_conflict = matches!(reservation.mode.as_str(), "write" | "exclusive")
            && changed_resources
                .iter()
                .any(|changed| resources_overlap(&reservation.resource_key, changed));
        if retains_conflict {
            transaction.execute(
                "UPDATE resource_reservations SET owner_type = 'change_set', owner_id = ?3
                 WHERE owner_type = 'claim' AND owner_id = ?1 AND resource_key = ?2
                   AND released_at IS NULL",
                params![
                    claim_id.to_string(),
                    reservation.resource_key,
                    change_set_id.to_string(),
                ],
            )?;
        } else {
            transaction.execute(
                "UPDATE resource_reservations SET released_at = ?3
                 WHERE owner_type = 'claim' AND owner_id = ?1 AND resource_key = ?2
                   AND released_at IS NULL",
                params![
                    claim_id.to_string(),
                    reservation.resource_key,
                    unix_micros(now),
                ],
            )?;
        }
    }
    Ok(())
}

fn load_pending_change_set_resources(
    connection: &Connection,
    item_id: WorkItemId,
) -> Result<Vec<(ChangeSetId, Vec<String>)>> {
    connection
        .prepare(
            "SELECT c.id, c.changed_resources_json
             FROM change_sets c JOIN work_attempts a ON a.id = c.attempt_id
             WHERE a.work_item_id = ?1 AND c.integration_state = 'pending'
             ORDER BY c.created_at, c.id",
        )?
        .query_map([item_id.to_string()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .map(|row| {
            let (id, resources) = row?;
            Ok((
                parse_id(&id, "change_sets.id")?,
                serde_json::from_str(&resources)?,
            ))
        })
        .collect()
}

fn actual_changed_resources(
    authenticated: &AuthenticatedAgent,
    submitted: &ChangeSetInput,
) -> Result<Vec<String>> {
    if submitted.base_commit.trim().is_empty() || submitted.head_commit.trim().is_empty() {
        return Err(invalid(
            "ChangeSet base_commit and head_commit are required",
        ));
    }
    let git_backed = authenticated
        .session
        .base_commit
        .as_deref()
        .is_some_and(|commit| !commit.is_empty());
    if !git_backed {
        return normalize_resource_keys(submitted.changed_resources.clone());
    }
    let worktree_path = authenticated
        .session
        .worktree_path
        .as_deref()
        .ok_or_else(|| {
            SessionBridgeError::domain(
                ErrorCode::WorktreeConflict,
                "Git-backed submit requires a registered worktree",
            )
        })?;
    let repository = GitRepository::open(worktree_path)?;
    repository.require_clean()?;
    let current_head = repository.head_commit()?;
    let expected_head = repository.resolve_revision(&submitted.head_commit)?;
    if current_head != expected_head {
        return Err(SessionBridgeError::domain(
            ErrorCode::InputInvalidated,
            "registered worktree HEAD differs from the submitted ChangeSet head",
        )
        .with_details(serde_json::json!({
            "current_head": current_head,
            "submitted_head": expected_head,
        })));
    }
    let changed = repository.changed_paths(&submitted.base_commit, &submitted.head_commit)?;
    normalize_resource_keys(
        changed
            .into_iter()
            .map(|path| format!("path:{}", path.display()))
            .collect(),
    )
}

fn normalize_resources(resources: Vec<ResourceRequest>) -> Result<Vec<ResourceRequest>> {
    let mut normalized = BTreeMap::new();
    for mut resource in resources {
        resource.resource_key = normalize_resource_key(&resource.resource_key)?;
        resource.mode = resource.mode.trim().to_ascii_lowercase();
        resource.policy = resource.policy.trim().to_ascii_lowercase();
        if resource.resource_key.starts_with("transient:") {
            resource.policy = "advisory".into();
        }
        if !matches!(resource.mode.as_str(), "read" | "write" | "exclusive") {
            return Err(invalid("resource mode must be read, write, or exclusive"));
        }
        if !matches!(resource.policy.as_str(), "enforced" | "advisory") {
            return Err(invalid("resource policy must be enforced or advisory"));
        }
        if resource
            .pinned_revision
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(invalid("pinned resource revision cannot be blank"));
        }
        match normalized.get(&resource.resource_key) {
            Some(existing) if existing != &resource => {
                return Err(invalid(
                    "duplicate resource keys must use identical mode, policy, and revision",
                ));
            }
            Some(_) => {}
            None => {
                normalized.insert(resource.resource_key.clone(), resource);
            }
        }
    }
    Ok(normalized.into_values().collect())
}

fn normalize_resource_keys(resources: Vec<String>) -> Result<Vec<String>> {
    let mut values = resources
        .into_iter()
        .map(|value| normalize_resource_key(&value))
        .collect::<Result<Vec<_>>>()?;
    values.sort();
    values.dedup();
    Ok(values)
}

fn normalize_resource_key(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(invalid(
            "resource key cannot be blank or contain control characters",
        ));
    }
    let Some(path) = value.strip_prefix("path:") else {
        return Ok(value.to_owned());
    };
    let path = Path::new(path);
    if path.is_absolute() {
        return Err(invalid("path resource keys must be repository-relative"));
    }
    let mut parts = Vec::new();
    for component in path.components() {
        use std::path::Component;
        match component {
            Component::Normal(value) => parts.push(value.to_string_lossy().into_owned()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(invalid(
                    "path resource keys cannot escape the repository root",
                ));
            }
        }
    }
    if parts.is_empty() {
        return Err(invalid("path resource key cannot resolve to an empty path"));
    }
    Ok(format!("path:{}", parts.join("/")))
}

fn normalize_input_pins(mut inputs: Vec<AttemptInputPin>) -> Result<Vec<AttemptInputPin>> {
    for input in &mut inputs {
        input.input_type = input.input_type.trim().to_ascii_lowercase();
        input.input_id = input.input_id.trim().to_owned();
        input.revision_or_digest = input.revision_or_digest.trim().to_owned();
        if !matches!(
            input.input_type.as_str(),
            "work_item" | "contract" | "artifact" | "change_set" | "target_branch"
        ) || input.input_id.is_empty()
            || input.revision_or_digest.is_empty()
        {
            return Err(invalid("Attempt input pin is invalid"));
        }
    }
    inputs.sort_by(|left, right| {
        (&left.input_type, &left.input_id).cmp(&(&right.input_type, &right.input_id))
    });
    for pair in inputs.windows(2) {
        if pair[0].input_type == pair[1].input_type && pair[0].input_id == pair[1].input_id {
            return Err(invalid("Attempt input pins contain a duplicate input"));
        }
    }
    Ok(inputs)
}

fn validate_artifact(artifact: &ArtifactInput) -> Result<()> {
    if artifact.kind.trim().is_empty() || artifact.value.trim().is_empty() {
        return Err(invalid("Artifact kind and value are required"));
    }
    if !artifact.metadata.is_object() {
        return Err(invalid("Artifact metadata must be a JSON object"));
    }
    Ok(())
}

fn validate_evidence(evidence: &EvidenceInput) -> Result<()> {
    if evidence.kind.trim().is_empty()
        || !matches!(
            evidence.result.as_str(),
            "passed" | "failed" | "inconclusive"
        )
    {
        return Err(invalid(
            "Evidence requires a kind and result passed, failed, or inconclusive",
        ));
    }
    if !evidence.details.is_object() {
        return Err(invalid("Evidence details must be a JSON object"));
    }
    if !evidence.environment.is_object() {
        return Err(invalid("Evidence environment must be a JSON object"));
    }
    if evidence.subject_type.is_some() != evidence.subject_id.is_some() {
        return Err(invalid(
            "Evidence subject_type and subject_id must be provided together",
        ));
    }
    for (name, value) in [
        ("subject_type", evidence.subject_type.as_deref()),
        ("subject_id", evidence.subject_id.as_deref()),
        ("commit", evidence.commit.as_deref()),
        ("verifier", evidence.verifier.as_deref()),
    ] {
        if value.is_some_and(|value| value.trim().is_empty()) {
            return Err(invalid(format!("Evidence {name} cannot be blank")));
        }
    }
    Ok(())
}

fn validate_lease(seconds: Option<i64>) -> Result<Duration> {
    let lease = seconds.map(Duration::seconds).unwrap_or(DEFAULT_LEASE);
    if lease < MIN_LEASE || lease > MAX_LEASE {
        return Err(invalid(
            "Claim lease must be between 5 seconds and 24 hours",
        ));
    }
    Ok(lease)
}

fn bump_completion_revision(
    transaction: &Transaction<'_>,
    task_id: TaskId,
    now: OffsetDateTime,
) -> Result<()> {
    let changed = transaction.execute(
        "UPDATE tasks SET completion_revision = completion_revision + 1, updated_at = ?2
         WHERE id = ?1",
        params![task_id.to_string(), unix_micros(now)],
    )?;
    if changed != 1 {
        return Err(not_found("Task"));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn append_session_event(
    transaction: &Transaction<'_>,
    context: &SessionContext,
    authenticated: &AuthenticatedAgent,
    event_type: &str,
    entity_type: &str,
    entity_id: &str,
    payload: &serde_json::Value,
    now: OffsetDateTime,
) -> Result<i64> {
    let actor_id = authenticated.session.session_id.to_string();
    events::append(
        transaction,
        context.workspace_id,
        Some(&context.task_id.to_string()),
        event_type,
        EventActor {
            kind: "session",
            id: Some(&actor_id),
        },
        EventEntity {
            kind: entity_type,
            id: Some(entity_id),
        },
        payload,
        now,
    )
}

fn session_key<'a>(
    authenticated: &'a AuthenticatedAgent,
    request_id: &'a str,
) -> IdempotencyKey<'a> {
    IdempotencyKey {
        actor_type: "session",
        actor_id: authenticated.session.idempotency_actor_id.as_str(),
        request_id,
    }
}

fn query_json_rows(
    connection: &Connection,
    sql: &str,
    task_id: TaskId,
    item_id: WorkItemId,
) -> Result<Vec<serde_json::Value>> {
    connection
        .prepare(sql)?
        .query_map(params![task_id.to_string(), item_id.to_string()], |row| {
            row.get::<_, String>(0)
        })?
        .map(|row| {
            let value = row?;
            serde_json::from_str(&value).map_err(rusqlite_json_error)
        })
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn rusqlite_json_error(error: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
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

fn database_value_error(field: &'static str) -> impl FnOnce(String) -> SessionBridgeError + Copy {
    move |error| {
        SessionBridgeError::domain(
            ErrorCode::Database,
            format!("database field {field} is invalid: {error}"),
        )
    }
}

fn format_time(value: OffsetDateTime) -> Result<String> {
    value.format(&Rfc3339).map_err(|error| {
        SessionBridgeError::domain(
            ErrorCode::Internal,
            format!("could not format timestamp: {error}"),
        )
    })
}

fn validate_request_id(value: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        return Err(invalid(
            "request ID is empty, too long, or contains control characters",
        ));
    }
    Ok(())
}

fn validate_nonnegative(value: i64, name: &str) -> Result<()> {
    if value < 0 {
        Err(invalid(format!("{name} cannot be negative")))
    } else {
        Ok(())
    }
}

fn default_resource_mode() -> String {
    "write".into()
}

fn default_resource_policy() -> String {
    "enforced".into()
}

fn default_mode_for_claim(mode: ClaimMode) -> &'static str {
    match mode {
        ClaimMode::Execute => "write",
        ClaimMode::Review => "read",
        ClaimMode::Integrate => "exclusive",
    }
}

fn invalid(message: impl Into<String>) -> SessionBridgeError {
    SessionBridgeError::domain(ErrorCode::InvalidInput, message)
}

fn stale(message: impl Into<String>) -> SessionBridgeError {
    SessionBridgeError::domain(ErrorCode::StaleClaim, message)
}

fn not_found(entity: &str) -> SessionBridgeError {
    SessionBridgeError::domain(ErrorCode::NotFound, format!("{entity} was not found"))
}
