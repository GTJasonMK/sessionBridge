use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime};

use crate::{
    Result,
    app::{
        agent::{AgentService, AuthenticatedAgent},
        await_service,
        completion_service::{CompletionTrigger, TaskCompletionContext, reconcile_in_transaction},
        events::{self, EventActor, EventEntity},
        idempotency::{self, IdempotencyKey},
    },
    clock::{Clock, unix_micros},
    domain::{ParticipantKind, ParticipantState, SessionContext, TaskId, TaskPhase},
    error::{ErrorCode, SessionBridgeError},
    storage::SqliteStore,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetIntentRequest {
    pub kind: String,
    pub target_type: String,
    #[serde(default)]
    pub target_id: Option<String>,
    pub summary: String,
    #[serde(default)]
    pub expected_outputs: Vec<String>,
    #[serde(default)]
    pub ttl_seconds: Option<i64>,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClearIntentRequest {
    pub intent_id: Option<String>,
    pub reason: Option<String>,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentMutationResult {
    pub intent_ids: Vec<String>,
    pub state: String,
    pub event_sequence: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinationRequestInput {
    pub kind: String,
    pub target_type: String,
    #[serde(default)]
    pub target_id: Option<String>,
    #[serde(default)]
    pub assigned_session_id: Option<String>,
    #[serde(default)]
    pub required_capability: Option<String>,
    #[serde(default)]
    pub blocking: bool,
    #[serde(default)]
    pub correlation_id: Option<String>,
    #[serde(default)]
    pub payload: serde_json::Value,
    pub request_id: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CoordinationAction {
    Acknowledge,
    Resolve,
    Cancel,
}

impl CoordinationAction {
    fn status(self) -> &'static str {
        match self {
            Self::Acknowledge => "acknowledged",
            Self::Resolve => "resolved",
            Self::Cancel => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinationResponseInput {
    pub coordination_request_id: String,
    pub action: CoordinationAction,
    #[serde(default)]
    pub response: serde_json::Value,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinationMutationResult {
    pub coordination_request_id: String,
    pub status: String,
    pub event_sequence: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishContractRequest {
    pub name: String,
    pub kind: String,
    pub owner_type: String,
    pub owner_id: String,
    #[serde(default = "default_compatibility")]
    pub compatibility: String,
    pub content: serde_json::Value,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviseContractRequest {
    pub contract_id: String,
    #[serde(default = "default_compatibility")]
    pub compatibility: String,
    pub content: serde_json::Value,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptContractRequest {
    pub contract_id: String,
    pub revision: i64,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractMutationResult {
    pub contract_id: String,
    pub revision_id: String,
    pub revision: i64,
    pub state: String,
    pub digest: String,
    pub event_sequence: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposeDecisionRequest {
    pub topic: String,
    pub proposal: serde_json::Value,
    #[serde(default)]
    pub supersedes_id: Option<String>,
    pub request_id: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DecisionResolution {
    Approve,
    Reject,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolveDecisionRequest {
    pub decision_id: String,
    pub resolution: DecisionResolution,
    pub reason: Option<String>,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionMutationResult {
    pub decision_id: String,
    pub status: String,
    pub event_sequence: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolveInvalidationRequest {
    pub invalidation_id: String,
    pub reason: String,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetireParticipantRequest {
    pub participant_id: String,
    pub reason: String,
    #[serde(default)]
    pub handoff: serde_json::Value,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParticipantRetirementResult {
    pub retirement_id: String,
    pub participant_id: String,
    pub status: String,
    pub task_phase: TaskPhase,
    pub completion_created: bool,
    pub event_sequence: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParticipantRecordView {
    pub participant_id: String,
    pub session_id: String,
    pub label: String,
    pub kind: String,
    pub state: String,
    pub completion_epoch: i64,
    pub version: i64,
    pub last_seen_at_micros: i64,
    pub ended_at_micros: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttestCapabilityRequest {
    pub capability: String,
    #[serde(default)]
    pub scope: Vec<String>,
    #[serde(default)]
    pub expires_in_seconds: Option<i64>,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityAttestationResult {
    pub grant_id: String,
    pub capability: String,
    pub expires_at_micros: Option<i64>,
    pub event_sequence: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approve,
    Reject,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvePlanChangeRequest {
    pub plan_change_id: String,
    pub decision: ApprovalDecision,
    pub reason: Option<String>,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordApprovalRequest {
    pub target_type: String,
    pub target_id: String,
    pub decision: ApprovalDecision,
    pub reason: Option<String>,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalMutationResult {
    pub approval_id: String,
    pub target_type: String,
    pub target_id: String,
    pub decision: String,
    pub target_status: Option<String>,
    pub event_sequence: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenBlockerRequest {
    pub work_item_id: String,
    pub waits_for_type: String,
    #[serde(default)]
    pub waits_for_id: Option<String>,
    pub reason: String,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolveBlockerRequest {
    pub blocker_id: String,
    pub resolution: String,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockerMutationResult {
    pub blocker_id: String,
    pub status: String,
    pub event_sequence: i64,
}

pub struct CollaborationService<C> {
    clock: C,
}

impl<C: Clock + Clone> CollaborationService<C> {
    pub fn new(clock: C) -> Self {
        Self { clock }
    }

    pub fn set_intent(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut request: SetIntentRequest,
    ) -> Result<IntentMutationResult> {
        canonical_enum(&mut request.kind, "Intent kind")?;
        if !matches!(
            request.kind.as_str(),
            "explore" | "plan" | "implement" | "review" | "integrate"
        ) {
            return Err(invalid("Intent kind is invalid"));
        }
        required_text(&mut request.target_type, "Intent target type")?;
        required_text(&mut request.summary, "Intent summary")?;
        normalize_strings(&mut request.expected_outputs, "expected output")?;
        validate_request_id(&request.request_id)?;
        let ttl = request.ttl_seconds.unwrap_or(900);
        if !(5..=86_400).contains(&ttl) {
            return Err(invalid("Intent TTL must be between 5 seconds and 24 hours"));
        }
        let payload_hash = idempotency::hash_payload(&request)?;
        let authenticated = self.authenticate(store, context)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let actor_id = authenticated.session.session_id.to_string();
        let key = session_key(&actor_id, &request.request_id);
        if let Some(stored) = idempotency::replay(&transaction, key, &payload_hash)? {
            transaction.commit()?;
            return Ok(stored.value);
        }
        require_active_worker(&transaction, context, &authenticated, now)?;
        let intent_id = uuid::Uuid::now_v7().to_string();
        transaction.execute(
            "INSERT INTO agent_intents (
                id, task_id, session_id, participant_id, kind, target_type, target_id,
                summary, expected_outputs_json, expires_at, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
            params![
                intent_id,
                context.task_id.to_string(),
                authenticated.session.session_id.to_string(),
                authenticated.participant.participant_id.to_string(),
                request.kind,
                request.target_type,
                request.target_id,
                request.summary,
                serde_json::to_string(&request.expected_outputs)?,
                unix_micros(now + Duration::seconds(ttl)),
                unix_micros(now),
            ],
        )?;
        bump_completion_revision(&transaction, context.task_id, now)?;
        let event_sequence = append_event(
            &transaction,
            context,
            &authenticated,
            "agent.intent_set",
            "agent_intent",
            &intent_id,
            &serde_json::json!({ "kind": request.kind, "target_type": request.target_type }),
            now,
        )?;
        let result = IntentMutationResult {
            intent_ids: vec![intent_id],
            state: "active".into(),
            event_sequence,
        };
        record_result(
            &transaction,
            key,
            context.task_id,
            &payload_hash,
            &result,
            now,
        )?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn clear_intent(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        request: ClearIntentRequest,
    ) -> Result<IntentMutationResult> {
        validate_request_id(&request.request_id)?;
        let payload_hash = idempotency::hash_payload(&request)?;
        let authenticated = self.authenticate(store, context)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let actor_id = authenticated.session.session_id.to_string();
        let key = session_key(&actor_id, &request.request_id);
        if let Some(stored) = idempotency::replay(&transaction, key, &payload_hash)? {
            transaction.commit()?;
            return Ok(stored.value);
        }
        let intent_ids = transaction
            .prepare(
                "SELECT id FROM agent_intents
                 WHERE task_id = ?1 AND session_id = ?2 AND cleared_at IS NULL
                   AND (?3 IS NULL OR id = ?3) ORDER BY id",
            )?
            .query_map(
                params![
                    context.task_id.to_string(),
                    authenticated.session.session_id.to_string(),
                    request.intent_id,
                ],
                |row| row.get::<_, String>(0),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if intent_ids.is_empty() {
            return Err(SessionBridgeError::domain(
                ErrorCode::NotFound,
                "active AgentIntent was not found",
            ));
        }
        transaction.execute(
            "UPDATE agent_intents SET cleared_at = ?3, updated_at = ?3
             WHERE task_id = ?1 AND session_id = ?2 AND cleared_at IS NULL
               AND (?4 IS NULL OR id = ?4)",
            params![
                context.task_id.to_string(),
                authenticated.session.session_id.to_string(),
                unix_micros(now),
                request.intent_id,
            ],
        )?;
        bump_completion_revision(&transaction, context.task_id, now)?;
        let event_sequence = append_event(
            &transaction,
            context,
            &authenticated,
            "agent.intent_cleared",
            "agent_intent",
            request.intent_id.as_deref().unwrap_or("all"),
            &serde_json::json!({ "intent_ids": intent_ids, "reason": request.reason }),
            now,
        )?;
        let result = IntentMutationResult {
            intent_ids,
            state: "cleared".into(),
            event_sequence,
        };
        record_result(
            &transaction,
            key,
            context.task_id,
            &payload_hash,
            &result,
            now,
        )?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn request_coordination(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut request: CoordinationRequestInput,
    ) -> Result<CoordinationMutationResult> {
        canonical_enum(&mut request.kind, "CoordinationRequest kind")?;
        if !matches!(
            request.kind.as_str(),
            "question" | "review" | "unblock" | "decision" | "integration" | "handoff"
        ) {
            return Err(invalid("CoordinationRequest kind is invalid"));
        }
        required_text(&mut request.target_type, "CoordinationRequest target type")?;
        if !request.payload.is_object() {
            return Err(invalid("CoordinationRequest payload must be a JSON object"));
        }
        validate_request_id(&request.request_id)?;
        let payload_hash = idempotency::hash_payload(&request)?;
        let authenticated = self.authenticate(store, context)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let actor_id = authenticated.session.session_id.to_string();
        let key = session_key(&actor_id, &request.request_id);
        if let Some(stored) = idempotency::replay(&transaction, key, &payload_hash)? {
            transaction.commit()?;
            return Ok(stored.value);
        }
        require_active_worker(&transaction, context, &authenticated, now)?;
        validate_assigned_session(
            &transaction,
            context.task_id,
            request.assigned_session_id.as_deref(),
        )?;
        let id = uuid::Uuid::now_v7().to_string();
        transaction.execute(
            "INSERT INTO coordination_requests (
                id, task_id, kind, target_type, target_id, requested_by_session_id,
                assigned_session_id, required_capability, blocking, status,
                correlation_id, payload_json, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'open', ?10, ?11, ?12, ?12)",
            params![
                id,
                context.task_id.to_string(),
                request.kind,
                request.target_type,
                request.target_id,
                authenticated.session.session_id.to_string(),
                request.assigned_session_id,
                request.required_capability,
                i64::from(request.blocking),
                request.correlation_id,
                serde_json::to_string(&request.payload)?,
                unix_micros(now),
            ],
        )?;
        bump_completion_revision(&transaction, context.task_id, now)?;
        let event_sequence = append_event(
            &transaction,
            context,
            &authenticated,
            "coordination.requested",
            "coordination_request",
            &id,
            &serde_json::json!({
                "kind": request.kind,
                "assigned_session_id": request.assigned_session_id,
                "blocking": request.blocking,
            }),
            now,
        )?;
        let result = CoordinationMutationResult {
            coordination_request_id: id,
            status: "open".into(),
            event_sequence,
        };
        record_result(
            &transaction,
            key,
            context.task_id,
            &payload_hash,
            &result,
            now,
        )?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn respond_coordination(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        request: CoordinationResponseInput,
    ) -> Result<CoordinationMutationResult> {
        validate_request_id(&request.request_id)?;
        if !request.response.is_object() {
            return Err(invalid("Coordination response must be a JSON object"));
        }
        let payload_hash = idempotency::hash_payload(&request)?;
        let authenticated = self.authenticate(store, context)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let actor_id = authenticated.session.session_id.to_string();
        let key = session_key(&actor_id, &request.request_id);
        if let Some(stored) = idempotency::replay(&transaction, key, &payload_hash)? {
            transaction.commit()?;
            return Ok(stored.value);
        }
        let row: Option<(String, Option<String>, String)> = transaction
            .query_row(
                "SELECT requested_by_session_id, assigned_session_id, status
                 FROM coordination_requests WHERE id = ?1 AND task_id = ?2",
                params![request.coordination_request_id, context.task_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((requested_by, assigned, status)) = row else {
            return Err(not_found("CoordinationRequest"));
        };
        if !matches!(status.as_str(), "open" | "acknowledged") {
            return Err(SessionBridgeError::domain(
                ErrorCode::VersionConflict,
                "CoordinationRequest is already terminal",
            ));
        }
        let session_id = authenticated.session.session_id.to_string();
        let authorized = match request.action {
            CoordinationAction::Cancel => requested_by == session_id,
            CoordinationAction::Acknowledge => assigned.as_deref() == Some(&session_id),
            CoordinationAction::Resolve => {
                requested_by == session_id || assigned.as_deref() == Some(&session_id)
            }
        };
        if !authorized {
            return Err(SessionBridgeError::domain(
                ErrorCode::CapabilityNotGranted,
                "Session is not authorized to perform this coordination response",
            ));
        }
        let new_status = request.action.status();
        transaction.execute(
            "UPDATE coordination_requests SET status = ?3, response_json = ?4,
                    resolved_at = CASE WHEN ?3 IN ('resolved', 'cancelled') THEN ?5 ELSE NULL END,
                    updated_at = ?5
             WHERE id = ?1 AND task_id = ?2 AND status IN ('open', 'acknowledged')",
            params![
                request.coordination_request_id,
                context.task_id.to_string(),
                new_status,
                serde_json::to_string(&request.response)?,
                unix_micros(now),
            ],
        )?;
        bump_completion_revision(&transaction, context.task_id, now)?;
        let event_sequence = append_event(
            &transaction,
            context,
            &authenticated,
            "coordination.responded",
            "coordination_request",
            &request.coordination_request_id,
            &serde_json::json!({ "action": request.action, "status": new_status }),
            now,
        )?;
        let result = CoordinationMutationResult {
            coordination_request_id: request.coordination_request_id,
            status: new_status.into(),
            event_sequence,
        };
        record_result(
            &transaction,
            key,
            context.task_id,
            &payload_hash,
            &result,
            now,
        )?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn publish_contract(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut request: PublishContractRequest,
    ) -> Result<ContractMutationResult> {
        normalize_contract_input(
            &mut request.name,
            &mut request.kind,
            &mut request.owner_type,
            &mut request.owner_id,
            &mut request.compatibility,
            &request.content,
        )?;
        validate_request_id(&request.request_id)?;
        let payload_hash = idempotency::hash_payload(&request)?;
        let authenticated = self.authenticate(store, context)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let actor_id = authenticated.session.session_id.to_string();
        let key = session_key(&actor_id, &request.request_id);
        if let Some(stored) = idempotency::replay(&transaction, key, &payload_hash)? {
            transaction.commit()?;
            return Ok(stored.value);
        }
        require_active_worker(&transaction, context, &authenticated, now)?;
        validate_contract_owner(
            &transaction,
            context,
            &authenticated,
            &request.owner_type,
            &request.owner_id,
        )?;
        let contract_id = uuid::Uuid::now_v7().to_string();
        let revision_id = uuid::Uuid::now_v7().to_string();
        let digest = digest_json(&request.content)?;
        transaction.execute(
            "INSERT INTO contracts (
                id, task_id, name, kind, owner_type, owner_id, version, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?7)",
            params![
                contract_id,
                context.task_id.to_string(),
                request.name,
                request.kind,
                request.owner_type,
                request.owner_id,
                unix_micros(now),
            ],
        )?;
        insert_contract_revision(
            &transaction,
            context.task_id,
            &contract_id,
            &revision_id,
            1,
            &digest,
            &request.compatibility,
            &request.content,
            authenticated.session.session_id.to_string().as_str(),
            now,
        )?;
        bump_completion_revision(&transaction, context.task_id, now)?;
        let event_sequence = append_event(
            &transaction,
            context,
            &authenticated,
            "contract.published",
            "contract",
            &contract_id,
            &serde_json::json!({ "revision": 1, "digest": digest }),
            now,
        )?;
        let result = ContractMutationResult {
            contract_id,
            revision_id,
            revision: 1,
            state: "published".into(),
            digest,
            event_sequence,
        };
        record_result(
            &transaction,
            key,
            context.task_id,
            &payload_hash,
            &result,
            now,
        )?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn revise_contract(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut request: ReviseContractRequest,
    ) -> Result<ContractMutationResult> {
        required_text(&mut request.contract_id, "Contract ID")?;
        canonical_enum(&mut request.compatibility, "Contract compatibility")?;
        validate_compatibility(&request.compatibility)?;
        if !request.content.is_object() {
            return Err(invalid("Contract content must be a JSON object"));
        }
        validate_request_id(&request.request_id)?;
        let payload_hash = idempotency::hash_payload(&request)?;
        let authenticated = self.authenticate(store, context)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let actor_id = authenticated.session.session_id.to_string();
        let key = session_key(&actor_id, &request.request_id);
        if let Some(stored) = idempotency::replay(&transaction, key, &payload_hash)? {
            transaction.commit()?;
            return Ok(stored.value);
        }
        require_active_worker(&transaction, context, &authenticated, now)?;
        let revision: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(r.revision), 0) + 1
                 FROM contracts c LEFT JOIN contract_revisions r ON r.contract_id = c.id
                 WHERE c.id = ?1 AND c.task_id = ?2",
            params![request.contract_id, context.task_id.to_string()],
            |row| row.get(0),
        )?;
        if revision == 1 {
            return Err(not_found("Contract"));
        }
        transaction.execute(
            "UPDATE contract_revisions SET state = 'superseded'
             WHERE contract_id = ?1 AND state = 'published'",
            [request.contract_id.as_str()],
        )?;
        let revision_id = uuid::Uuid::now_v7().to_string();
        let digest = digest_json(&request.content)?;
        insert_contract_revision(
            &transaction,
            context.task_id,
            &request.contract_id,
            &revision_id,
            revision,
            &digest,
            &request.compatibility,
            &request.content,
            &actor_id,
            now,
        )?;
        transaction.execute(
            "UPDATE contracts SET version = version + 1, updated_at = ?2 WHERE id = ?1",
            params![request.contract_id, unix_micros(now)],
        )?;
        create_contract_invalidations(
            &transaction,
            context.task_id,
            &request.contract_id,
            revision,
            now,
        )?;
        bump_completion_revision(&transaction, context.task_id, now)?;
        let event_sequence = append_event(
            &transaction,
            context,
            &authenticated,
            "contract.revised",
            "contract",
            &request.contract_id,
            &serde_json::json!({
                "revision": revision,
                "digest": digest,
                "compatibility": request.compatibility,
            }),
            now,
        )?;
        let result = ContractMutationResult {
            contract_id: request.contract_id,
            revision_id,
            revision,
            state: "published".into(),
            digest,
            event_sequence,
        };
        record_result(
            &transaction,
            key,
            context.task_id,
            &payload_hash,
            &result,
            now,
        )?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn accept_contract(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut request: AcceptContractRequest,
    ) -> Result<ContractMutationResult> {
        required_text(&mut request.contract_id, "Contract ID")?;
        if request.revision < 1 {
            return Err(invalid("Contract revision must be positive"));
        }
        request.evidence_ids.sort();
        request.evidence_ids.dedup();
        validate_request_id(&request.request_id)?;
        let payload_hash = idempotency::hash_payload(&request)?;
        let authenticated = self.authenticate(store, context)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let actor_id = authenticated.session.session_id.to_string();
        let key = session_key(&actor_id, &request.request_id);
        if let Some(stored) = idempotency::replay(&transaction, key, &payload_hash)? {
            transaction.commit()?;
            return Ok(stored.value);
        }
        require_active_worker(&transaction, context, &authenticated, now)?;
        validate_evidence_ids(&transaction, context.task_id, &request.evidence_ids)?;
        let row: Option<(String, String)> = transaction
            .query_row(
                "SELECT id, digest FROM contract_revisions
                 WHERE contract_id = ?1 AND task_id = ?2 AND revision = ?3
                   AND state = 'published'",
                params![
                    request.contract_id,
                    context.task_id.to_string(),
                    request.revision,
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((revision_id, digest)) = row else {
            return Err(SessionBridgeError::domain(
                ErrorCode::ContractNotAccepted,
                "published ContractRevision was not found",
            ));
        };
        transaction.execute(
            "UPDATE contract_revisions SET state = CASE WHEN revision = ?2 THEN 'accepted'
                                                        WHEN state = 'published' THEN 'superseded'
                                                        ELSE state END
             WHERE contract_id = ?1",
            params![request.contract_id, request.revision],
        )?;
        bump_completion_revision(&transaction, context.task_id, now)?;
        let event_sequence = append_event(
            &transaction,
            context,
            &authenticated,
            "contract.accepted",
            "contract",
            &request.contract_id,
            &serde_json::json!({
                "revision": request.revision,
                "evidence_ids": request.evidence_ids,
            }),
            now,
        )?;
        let result = ContractMutationResult {
            contract_id: request.contract_id,
            revision_id,
            revision: request.revision,
            state: "accepted".into(),
            digest,
            event_sequence,
        };
        record_result(
            &transaction,
            key,
            context.task_id,
            &payload_hash,
            &result,
            now,
        )?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn propose_decision(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut request: ProposeDecisionRequest,
    ) -> Result<DecisionMutationResult> {
        required_text(&mut request.topic, "Decision topic")?;
        if !request.proposal.is_object() {
            return Err(invalid("Decision proposal must be a JSON object"));
        }
        validate_request_id(&request.request_id)?;
        let payload_hash = idempotency::hash_payload(&request)?;
        let authenticated = self.authenticate(store, context)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let actor_id = authenticated.session.session_id.to_string();
        let key = session_key(&actor_id, &request.request_id);
        if let Some(stored) = idempotency::replay(&transaction, key, &payload_hash)? {
            transaction.commit()?;
            return Ok(stored.value);
        }
        require_active_worker(&transaction, context, &authenticated, now)?;
        if let Some(supersedes_id) = request.supersedes_id.as_deref() {
            let exists: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM decisions WHERE id = ?1 AND task_id = ?2)",
                params![supersedes_id, context.task_id.to_string()],
                |row| row.get(0),
            )?;
            if !exists {
                return Err(not_found("superseded Decision"));
            }
        }
        let id = uuid::Uuid::now_v7().to_string();
        transaction.execute(
            "INSERT INTO decisions (
                id, task_id, topic, proposal_json, status, supersedes_id,
                proposed_by_session_id, version, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, 'proposed', ?5, ?6, 0, ?7, ?7)",
            params![
                id,
                context.task_id.to_string(),
                request.topic,
                serde_json::to_string(&request.proposal)?,
                request.supersedes_id,
                actor_id,
                unix_micros(now),
            ],
        )?;
        bump_completion_revision(&transaction, context.task_id, now)?;
        let event_sequence = append_event(
            &transaction,
            context,
            &authenticated,
            "decision.proposed",
            "decision",
            &id,
            &serde_json::json!({ "topic": request.topic }),
            now,
        )?;
        let result = DecisionMutationResult {
            decision_id: id,
            status: "proposed".into(),
            event_sequence,
        };
        record_result(
            &transaction,
            key,
            context.task_id,
            &payload_hash,
            &result,
            now,
        )?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn resolve_decision(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut request: ResolveDecisionRequest,
    ) -> Result<DecisionMutationResult> {
        required_text(&mut request.decision_id, "Decision ID")?;
        request.evidence_ids.sort();
        request.evidence_ids.dedup();
        validate_request_id(&request.request_id)?;
        let payload_hash = idempotency::hash_payload(&request)?;
        let authenticated = self.authenticate(store, context)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let actor_id = authenticated.session.session_id.to_string();
        let key = session_key(&actor_id, &request.request_id);
        if let Some(stored) = idempotency::replay(&transaction, key, &payload_hash)? {
            transaction.commit()?;
            return Ok(stored.value);
        }
        require_active_worker(&transaction, context, &authenticated, now)?;
        validate_evidence_ids(&transaction, context.task_id, &request.evidence_ids)?;
        let proposed_by: Option<String> = transaction
            .query_row(
                "SELECT proposed_by_session_id FROM decisions
                 WHERE id = ?1 AND task_id = ?2 AND status IN ('proposed', 'pending_approval')",
                params![request.decision_id, context.task_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let Some(proposed_by) = proposed_by else {
            return Err(SessionBridgeError::domain(
                ErrorCode::DecisionRequired,
                "open Decision was not found",
            ));
        };
        let other_worker_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM task_participants
             WHERE task_id = ?1 AND completion_epoch = ?2 AND kind = 'worker'
               AND state IN ('active', 'waiting') AND session_id <> ?3",
            params![
                context.task_id.to_string(),
                authenticated.participant.completion_epoch,
                proposed_by,
            ],
            |row| row.get(0),
        )?;
        if proposed_by == actor_id && other_worker_count > 0 {
            return Err(SessionBridgeError::domain(
                ErrorCode::ApprovalRequired,
                "Decision proposer cannot self-approve while another active worker is available",
            ));
        }
        let approval_decision = match request.resolution {
            DecisionResolution::Approve => "approved",
            DecisionResolution::Reject => "rejected",
        };
        transaction.execute(
            "INSERT INTO approvals (
                id, task_id, target_type, target_id, session_id, decision, created_at
             ) VALUES (?1, ?2, 'decision', ?3, ?4, ?5, ?6)",
            params![
                uuid::Uuid::now_v7().to_string(),
                context.task_id.to_string(),
                request.decision_id,
                actor_id,
                approval_decision,
                unix_micros(now),
            ],
        )?;
        let status = match request.resolution {
            DecisionResolution::Approve => "accepted",
            DecisionResolution::Reject => "rejected",
        };
        transaction.execute(
            "UPDATE decisions SET status = ?3, version = version + 1, updated_at = ?4
             WHERE id = ?1 AND task_id = ?2 AND status IN ('proposed', 'pending_approval')",
            params![
                request.decision_id,
                context.task_id.to_string(),
                status,
                unix_micros(now),
            ],
        )?;
        bump_completion_revision(&transaction, context.task_id, now)?;
        let event_sequence = append_event(
            &transaction,
            context,
            &authenticated,
            "decision.resolved",
            "decision",
            &request.decision_id,
            &serde_json::json!({
                "status": status,
                "reason": request.reason,
                "evidence_ids": request.evidence_ids,
            }),
            now,
        )?;
        let result = DecisionMutationResult {
            decision_id: request.decision_id,
            status: status.into(),
            event_sequence,
        };
        record_result(
            &transaction,
            key,
            context.task_id,
            &payload_hash,
            &result,
            now,
        )?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn resolve_invalidation(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut request: ResolveInvalidationRequest,
    ) -> Result<DecisionMutationResult> {
        required_text(&mut request.invalidation_id, "Invalidation ID")?;
        required_text(&mut request.reason, "Invalidation resolution reason")?;
        request.evidence_ids.sort();
        request.evidence_ids.dedup();
        validate_request_id(&request.request_id)?;
        let payload_hash = idempotency::hash_payload(&request)?;
        let authenticated = self.authenticate(store, context)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let actor_id = authenticated.session.session_id.to_string();
        let key = session_key(&actor_id, &request.request_id);
        if let Some(stored) = idempotency::replay(&transaction, key, &payload_hash)? {
            transaction.commit()?;
            return Ok(stored.value);
        }
        require_active_worker(&transaction, context, &authenticated, now)?;
        validate_evidence_ids(&transaction, context.task_id, &request.evidence_ids)?;
        let management_mode: String = transaction
            .query_row(
                "SELECT management_mode FROM invalidations
                 WHERE id=?1 AND task_id=?2 AND status='open'",
                params![request.invalidation_id, context.task_id.to_string()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| not_found("open Invalidation"))?;
        if management_mode == "system_reconciled" {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "system-reconciled Invalidation can only be resolved by restoring the external state and running task reconciliation",
            )
            .with_details(serde_json::json!({
                "invalidation_id": request.invalidation_id,
                "recovery_options": [{
                    "action": "restore_external_state_and_reconcile",
                    "command": format!("sessionbridge task reconcile {}", context.task_id),
                    "safe": true
                }]
            })));
        }
        let changed = transaction.execute(
            "UPDATE invalidations SET status = 'resolved', resolved_at = ?3
             WHERE id = ?1 AND task_id = ?2 AND status = 'open'",
            params![
                request.invalidation_id,
                context.task_id.to_string(),
                unix_micros(now),
            ],
        )?;
        if changed != 1 {
            return Err(not_found("open Invalidation"));
        }
        bump_completion_revision(&transaction, context.task_id, now)?;
        let event_sequence = append_event(
            &transaction,
            context,
            &authenticated,
            "invalidation.resolved",
            "invalidation",
            &request.invalidation_id,
            &serde_json::json!({
                "reason": request.reason,
                "evidence_ids": request.evidence_ids,
            }),
            now,
        )?;
        let result = DecisionMutationResult {
            decision_id: request.invalidation_id,
            status: "resolved".into(),
            event_sequence,
        };
        record_result(
            &transaction,
            key,
            context.task_id,
            &payload_hash,
            &result,
            now,
        )?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn retire_participant(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut request: RetireParticipantRequest,
    ) -> Result<ParticipantRetirementResult> {
        required_text(&mut request.participant_id, "Participant ID")?;
        required_text(&mut request.reason, "retirement reason")?;
        if !request.handoff.is_object() {
            return Err(invalid("retirement handoff must be a JSON object"));
        }
        request.evidence_ids.sort();
        request.evidence_ids.dedup();
        validate_request_id(&request.request_id)?;
        crate::app::task_reconciler::TaskReconciler::new(self.clock.clone())
            .reconcile_agent(store, context)?;
        let payload_hash = idempotency::hash_payload(&request)?;
        let authenticated = self.authenticate(store, context)?;
        if request.participant_id == authenticated.participant.participant_id.to_string() {
            return Err(SessionBridgeError::domain(
                ErrorCode::RetirementNotAllowed,
                "participant cannot retire itself",
            ));
        }
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let actor_id = authenticated.session.session_id.to_string();
        let key = session_key(&actor_id, &request.request_id);
        if let Some(stored) = idempotency::replay(&transaction, key, &payload_hash)? {
            transaction.commit()?;
            return Ok(stored.value);
        }
        require_active_worker(&transaction, context, &authenticated, now)?;
        validate_evidence_ids(&transaction, context.task_id, &request.evidence_ids)?;
        let target: Option<(String, String, i64, String)> = transaction
            .query_row(
                "SELECT p.session_id, p.state, p.version, s.label
                 FROM task_participants p JOIN agent_sessions s ON s.id = p.session_id
                 WHERE p.id = ?1 AND p.task_id = ?2 AND p.completion_epoch = ?3
                   AND p.kind = 'worker'",
                params![
                    request.participant_id,
                    context.task_id.to_string(),
                    authenticated.participant.completion_epoch,
                ],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let Some((target_session_id, target_state, target_version, _label)) = target else {
            return Err(not_found("current worker TaskParticipant"));
        };
        if !matches!(
            target_state.as_str(),
            "blocked" | "departed" | "active" | "waiting"
        ) {
            return Err(SessionBridgeError::domain(
                ErrorCode::RetirementNotAllowed,
                "only active, waiting, blocked, or departed participants can be retired",
            ));
        }
        let last_seen: i64 = transaction.query_row(
            "SELECT last_seen_at FROM agent_sessions WHERE id = ?1",
            [target_session_id.as_str()],
            |row| row.get(0),
        )?;
        if matches!(target_state.as_str(), "active" | "waiting")
            && last_seen > unix_micros(now - crate::app::task::PARTICIPANT_RETIREMENT_AFTER)
        {
            return Err(SessionBridgeError::domain(
                ErrorCode::RetirementNotAllowed,
                "active participant is not stale",
            ));
        }
        ensure_session_has_no_open_work(&transaction, context.task_id, &target_session_id)?;
        let active_scanners: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM task_participants
             WHERE task_id = ?1 AND completion_epoch = ?2 AND kind = 'worker'
               AND state IN ('active', 'waiting') AND id <> ?3",
            params![
                context.task_id.to_string(),
                authenticated.participant.completion_epoch,
                request.participant_id,
            ],
            |row| row.get(0),
        )?;
        if active_scanners < 1 {
            return Err(SessionBridgeError::domain(
                ErrorCode::RetirementNotAllowed,
                "retirement must leave an active worker for the final global scan",
            ));
        }
        let retirement_id = uuid::Uuid::now_v7().to_string();
        transaction.execute(
            "INSERT INTO participant_retirements (
                id, task_id, participant_id, proposer_participant_id, reason,
                handoff_json, status, version, applied_at, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'applied', 1, ?7, ?7, ?7)",
            params![
                retirement_id,
                context.task_id.to_string(),
                request.participant_id,
                authenticated.participant.participant_id.to_string(),
                request.reason,
                serde_json::to_string(&request.handoff)?,
                unix_micros(now),
            ],
        )?;
        for evidence_id in &request.evidence_ids {
            transaction.execute(
                "INSERT INTO participant_retirement_evidence (
                    participant_retirement_id, evidence_id
                 ) VALUES (?1, ?2)",
                params![retirement_id, evidence_id],
            )?;
        }
        let changed = transaction.execute(
            "UPDATE task_participants SET state = 'retired', current_conclusion_id = NULL,
                    retired_at = ?3, version = version + 1, updated_at = ?3
             WHERE id = ?1 AND task_id = ?2 AND version = ?4",
            params![
                request.participant_id,
                context.task_id.to_string(),
                unix_micros(now),
                target_version,
            ],
        )?;
        if changed != 1 {
            return Err(SessionBridgeError::domain(
                ErrorCode::ParticipantVersionChanged,
                "participant changed during retirement",
            ));
        }
        transaction.execute(
            "UPDATE agent_sessions SET ended_at = COALESCE(ended_at, ?2),
                    end_reason = COALESCE(end_reason, 'retired'), updated_at = ?2
             WHERE id = ?1",
            params![target_session_id, unix_micros(now)],
        )?;
        bump_completion_revision(&transaction, context.task_id, now)?;
        let event_sequence = append_event(
            &transaction,
            context,
            &authenticated,
            "participant.retired",
            "participant_retirement",
            &retirement_id,
            &serde_json::json!({
                "participant_id": request.participant_id,
                "reason": request.reason,
                "evidence_ids": request.evidence_ids,
            }),
            now,
        )?;
        await_service::resolve_participant_retired(
            &transaction,
            context,
            &request.participant_id,
            event_sequence,
            now,
        )?;
        crate::app::external_wait::resolve_participant_retired(
            &transaction,
            context,
            &request.participant_id,
            now,
        )?;
        let repository_identity = load_repository_identity(&transaction, context.workspace_id)?;
        let reconciliation = reconcile_in_transaction(
            &transaction,
            TaskCompletionContext {
                workspace_id: context.workspace_id,
                repository_instance_id: &repository_identity,
                task_id: context.task_id,
            },
            self.clock.clone(),
            CompletionTrigger {
                actor_type: "session",
                actor_id: Some(&actor_id),
            },
        )?;
        let final_event = reconciliation.event_sequence.unwrap_or(event_sequence);
        let result = ParticipantRetirementResult {
            retirement_id,
            participant_id: request.participant_id,
            status: "applied".into(),
            task_phase: reconciliation.task_phase,
            completion_created: reconciliation.completion_created,
            event_sequence: final_event,
        };
        record_result(
            &transaction,
            key,
            context.task_id,
            &payload_hash,
            &result,
            now,
        )?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn attest_capability(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut request: AttestCapabilityRequest,
    ) -> Result<CapabilityAttestationResult> {
        required_text(&mut request.capability, "capability")?;
        normalize_strings(&mut request.scope, "capability scope")?;
        validate_request_id(&request.request_id)?;
        let expires_in = request.expires_in_seconds.unwrap_or(3_600);
        if !(60..=2_592_000).contains(&expires_in) {
            return Err(invalid(
                "capability attestation expiry must be between 1 minute and 30 days",
            ));
        }
        let payload_hash = idempotency::hash_payload(&request)?;
        let authenticated = self.authenticate(store, context)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let actor_id = authenticated.session.session_id.to_string();
        let key = session_key(&actor_id, &request.request_id);
        if let Some(stored) = idempotency::replay(&transaction, key, &payload_hash)? {
            transaction.commit()?;
            return Ok(stored.value);
        }
        require_active_worker(&transaction, context, &authenticated, now)?;
        transaction.execute(
            "UPDATE capability_grants SET revoked_at = ?4
             WHERE task_id = ?1 AND session_id = ?2 AND capability = ?3
               AND issuer_type = 'session' AND revoked_at IS NULL",
            params![
                context.task_id.to_string(),
                actor_id,
                request.capability,
                unix_micros(now),
            ],
        )?;
        let grant_id = uuid::Uuid::now_v7().to_string();
        let expires_at = now + Duration::seconds(expires_in);
        transaction.execute(
            "INSERT INTO capability_grants (
                id, task_id, session_id, capability, issuer_type, issuer_id,
                scope_json, expires_at, created_at
             ) VALUES (?1, ?2, ?3, ?4, 'session', ?3, ?5, ?6, ?7)",
            params![
                grant_id,
                context.task_id.to_string(),
                actor_id,
                request.capability,
                serde_json::to_string(&request.scope)?,
                unix_micros(expires_at),
                unix_micros(now),
            ],
        )?;
        bump_completion_revision(&transaction, context.task_id, now)?;
        let event_sequence = append_event(
            &transaction,
            context,
            &authenticated,
            "capability.attested",
            "capability_grant",
            &grant_id,
            &serde_json::json!({
                "capability": request.capability,
                "scope": request.scope,
                "expires_at_micros": unix_micros(expires_at),
            }),
            now,
        )?;
        let result = CapabilityAttestationResult {
            grant_id,
            capability: request.capability,
            expires_at_micros: Some(unix_micros(expires_at)),
            event_sequence,
        };
        record_result(
            &transaction,
            key,
            context.task_id,
            &payload_hash,
            &result,
            now,
        )?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn open_blocker(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut request: OpenBlockerRequest,
    ) -> Result<BlockerMutationResult> {
        required_text(&mut request.work_item_id, "WorkItem ID")?;
        required_text(&mut request.waits_for_type, "Blocker waits_for_type")?;
        required_text(&mut request.reason, "Blocker reason")?;
        validate_request_id(&request.request_id)?;
        let payload_hash = idempotency::hash_payload(&request)?;
        let authenticated = self.authenticate(store, context)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let actor_id = authenticated.session.session_id.to_string();
        let key = session_key(&actor_id, &request.request_id);
        if let Some(stored) = idempotency::replay(&transaction, key, &payload_hash)? {
            transaction.commit()?;
            return Ok(stored.value);
        }
        require_active_worker(&transaction, context, &authenticated, now)?;
        let item_exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM work_items WHERE id = ?1 AND task_id = ?2)",
            params![request.work_item_id, context.task_id.to_string()],
            |row| row.get(0),
        )?;
        if !item_exists {
            return Err(not_found("WorkItem"));
        }
        let blocker_id = uuid::Uuid::now_v7().to_string();
        transaction.execute(
            "INSERT INTO blockers (
                id, task_id, work_item_id, waits_for_type, waits_for_id, status,
                reason, opened_by_session_id, opened_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6, ?7, ?8)",
            params![
                blocker_id,
                context.task_id.to_string(),
                request.work_item_id,
                request.waits_for_type,
                request.waits_for_id,
                request.reason,
                actor_id,
                unix_micros(now),
            ],
        )?;
        bump_completion_revision(&transaction, context.task_id, now)?;
        let event_sequence = append_event(
            &transaction,
            context,
            &authenticated,
            "blocker.opened",
            "blocker",
            &blocker_id,
            &serde_json::json!({ "work_item_id": request.work_item_id }),
            now,
        )?;
        let result = BlockerMutationResult {
            blocker_id,
            status: "open".into(),
            event_sequence,
        };
        record_result(
            &transaction,
            key,
            context.task_id,
            &payload_hash,
            &result,
            now,
        )?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn resolve_blocker(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut request: ResolveBlockerRequest,
    ) -> Result<BlockerMutationResult> {
        required_text(&mut request.blocker_id, "Blocker ID")?;
        required_text(&mut request.resolution, "Blocker resolution")?;
        validate_request_id(&request.request_id)?;
        let payload_hash = idempotency::hash_payload(&request)?;
        let authenticated = self.authenticate(store, context)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let actor_id = authenticated.session.session_id.to_string();
        let key = session_key(&actor_id, &request.request_id);
        if let Some(stored) = idempotency::replay(&transaction, key, &payload_hash)? {
            transaction.commit()?;
            return Ok(stored.value);
        }
        require_active_worker(&transaction, context, &authenticated, now)?;
        let changed = transaction.execute(
            "UPDATE blockers SET status = 'resolved', resolution = ?3,
                    resolved_by_session_id = ?4, resolved_at = ?5
             WHERE id = ?1 AND task_id = ?2 AND status = 'open'",
            params![
                request.blocker_id,
                context.task_id.to_string(),
                request.resolution,
                actor_id,
                unix_micros(now),
            ],
        )?;
        if changed != 1 {
            return Err(not_found("open Blocker"));
        }
        bump_completion_revision(&transaction, context.task_id, now)?;
        let event_sequence = append_event(
            &transaction,
            context,
            &authenticated,
            "blocker.resolved",
            "blocker",
            &request.blocker_id,
            &serde_json::json!({ "resolution": request.resolution }),
            now,
        )?;
        let result = BlockerMutationResult {
            blocker_id: request.blocker_id,
            status: "resolved".into(),
            event_sequence,
        };
        record_result(
            &transaction,
            key,
            context.task_id,
            &payload_hash,
            &result,
            now,
        )?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn resolve_plan_change(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut request: ResolvePlanChangeRequest,
    ) -> Result<ApprovalMutationResult> {
        required_text(&mut request.plan_change_id, "PlanChange ID")?;
        validate_request_id(&request.request_id)?;
        let payload_hash = idempotency::hash_payload(&request)?;
        let authenticated = self.authenticate(store, context)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let actor_id = authenticated.session.session_id.to_string();
        let key = session_key(&actor_id, &request.request_id);
        if let Some(stored) = idempotency::replay(&transaction, key, &payload_hash)? {
            transaction.commit()?;
            return Ok(stored.value);
        }
        require_active_worker(&transaction, context, &authenticated, now)?;
        let row: Option<(String, i64, String, String)> = transaction
            .query_row(
                "SELECT proposer_session_id, base_plan_version, operations_json, status
                 FROM plan_changes WHERE id = ?1 AND task_id = ?2",
                params![request.plan_change_id, context.task_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let Some((proposer, base_plan_version, operations, status)) = row else {
            return Err(not_found("PlanChange"));
        };
        if status != "pending_approval" {
            return Err(SessionBridgeError::domain(
                ErrorCode::VersionConflict,
                "PlanChange is not pending approval",
            ));
        }
        require_peer_when_available(
            &transaction,
            context,
            &authenticated,
            &proposer,
            "PlanChange",
        )?;
        let approval_id = insert_approval(
            &transaction,
            context.task_id,
            "plan_change",
            &request.plan_change_id,
            &actor_id,
            request.decision,
            now,
        )?;
        let target_status = match request.decision {
            ApprovalDecision::Reject => {
                reject_plan_operation(&transaction, context.task_id, &operations, now)?;
                transaction.execute(
                    "UPDATE plan_changes SET status = 'rejected', version = version + 1,
                            updated_at = ?3
                     WHERE id = ?1 AND task_id = ?2 AND status = 'pending_approval'",
                    params![
                        request.plan_change_id,
                        context.task_id.to_string(),
                        unix_micros(now),
                    ],
                )?;
                bump_completion_revision(&transaction, context.task_id, now)?;
                "rejected"
            }
            ApprovalDecision::Approve => {
                let current_plan_version: i64 = transaction.query_row(
                    "SELECT plan_version FROM tasks WHERE id = ?1",
                    [context.task_id.to_string()],
                    |row| row.get(0),
                )?;
                if current_plan_version != base_plan_version {
                    return Err(SessionBridgeError::domain(
                        ErrorCode::PlanChanged,
                        "PlanChange base version is no longer current",
                    )
                    .with_details(serde_json::json!({
                        "base_plan_version": base_plan_version,
                        "current_plan_version": current_plan_version,
                    })));
                }
                apply_plan_operation(&transaction, context, &authenticated, &operations, now)?;
                transaction.execute(
                    "UPDATE plan_changes SET status = 'applied', version = version + 1,
                            applied_at = ?3, updated_at = ?3
                     WHERE id = ?1 AND task_id = ?2 AND status = 'pending_approval'",
                    params![
                        request.plan_change_id,
                        context.task_id.to_string(),
                        unix_micros(now),
                    ],
                )?;
                transaction.execute(
                    "UPDATE tasks SET plan_version = plan_version + 1,
                            completion_revision = completion_revision + 1, updated_at = ?2
                     WHERE id = ?1 AND plan_version = ?3",
                    params![
                        context.task_id.to_string(),
                        unix_micros(now),
                        base_plan_version,
                    ],
                )?;
                "applied"
            }
        };
        let event_sequence = append_event(
            &transaction,
            context,
            &authenticated,
            "plan_change.resolved",
            "plan_change",
            &request.plan_change_id,
            &serde_json::json!({
                "decision": request.decision,
                "status": target_status,
                "reason": request.reason,
                "approval_id": approval_id,
            }),
            now,
        )?;
        let result = ApprovalMutationResult {
            approval_id,
            target_type: "plan_change".into(),
            target_id: request.plan_change_id,
            decision: approval_decision_str(request.decision).into(),
            target_status: Some(target_status.into()),
            event_sequence,
        };
        record_result(
            &transaction,
            key,
            context.task_id,
            &payload_hash,
            &result,
            now,
        )?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn record_approval(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut request: RecordApprovalRequest,
    ) -> Result<ApprovalMutationResult> {
        canonical_enum(&mut request.target_type, "approval target type")?;
        required_text(&mut request.target_id, "approval target ID")?;
        if !matches!(
            request.target_type.as_str(),
            "decision" | "participant_retirement" | "task_reopen"
        ) {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "use plan approve/reject for PlanChange targets",
            ));
        }
        validate_request_id(&request.request_id)?;
        let payload_hash = idempotency::hash_payload(&request)?;
        let authenticated = self.authenticate(store, context)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let actor_id = authenticated.session.session_id.to_string();
        let key = session_key(&actor_id, &request.request_id);
        if let Some(stored) = idempotency::replay(&transaction, key, &payload_hash)? {
            transaction.commit()?;
            return Ok(stored.value);
        }
        require_active_worker(&transaction, context, &authenticated, now)?;
        validate_approval_target(
            &transaction,
            context.task_id,
            &request.target_type,
            &request.target_id,
        )?;
        let approval_id = insert_approval(
            &transaction,
            context.task_id,
            &request.target_type,
            &request.target_id,
            &actor_id,
            request.decision,
            now,
        )?;
        let target_status = match request.target_type.as_str() {
            "participant_retirement" => {
                let status = if request.decision == ApprovalDecision::Approve {
                    "approved"
                } else {
                    "rejected"
                };
                transaction.execute(
                    "UPDATE participant_retirements SET status = ?3, version = version + 1,
                            updated_at = ?4
                     WHERE id = ?1 AND task_id = ?2
                       AND status IN ('proposed', 'pending_approval')",
                    params![
                        request.target_id,
                        context.task_id.to_string(),
                        status,
                        unix_micros(now),
                    ],
                )?;
                Some(status.to_owned())
            }
            "decision" => {
                let status = if request.decision == ApprovalDecision::Approve {
                    "accepted"
                } else {
                    "rejected"
                };
                transaction.execute(
                    "UPDATE decisions SET status = ?3, version = version + 1, updated_at = ?4
                     WHERE id = ?1 AND task_id = ?2
                       AND status IN ('proposed', 'pending_approval')",
                    params![
                        request.target_id,
                        context.task_id.to_string(),
                        status,
                        unix_micros(now),
                    ],
                )?;
                Some(status.to_owned())
            }
            "task_reopen" => None,
            _ => unreachable!("approval target type was validated"),
        };
        bump_completion_revision(&transaction, context.task_id, now)?;
        let event_sequence = append_event(
            &transaction,
            context,
            &authenticated,
            "approval.recorded",
            "approval",
            &approval_id,
            &serde_json::json!({
                "target_type": request.target_type,
                "target_id": request.target_id,
                "decision": request.decision,
                "reason": request.reason,
            }),
            now,
        )?;
        let result = ApprovalMutationResult {
            approval_id,
            target_type: request.target_type,
            target_id: request.target_id,
            decision: approval_decision_str(request.decision).into(),
            target_status,
            event_sequence,
        };
        record_result(
            &transaction,
            key,
            context.task_id,
            &payload_hash,
            &result,
            now,
        )?;
        transaction.commit()?;
        Ok(result)
    }

    fn authenticate(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
    ) -> Result<AuthenticatedAgent> {
        AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)
    }
}

pub fn list_participants(
    connection: &Connection,
    task_id: TaskId,
) -> Result<Vec<ParticipantRecordView>> {
    connection
        .prepare(
            "SELECT p.id, p.session_id, s.label, p.kind, p.state, p.completion_epoch,
                    p.version, s.last_seen_at, s.ended_at
             FROM task_participants p JOIN agent_sessions s ON s.id = p.session_id
             WHERE p.task_id = ?1 ORDER BY p.completion_epoch DESC, p.created_at, p.id",
        )?
        .query_map([task_id.to_string()], |row| {
            Ok(ParticipantRecordView {
                participant_id: row.get(0)?,
                session_id: row.get(1)?,
                label: row.get(2)?,
                kind: row.get(3)?,
                state: row.get(4)?,
                completion_epoch: row.get(5)?,
                version: row.get(6)?,
                last_seen_at_micros: row.get(7)?,
                ended_at_micros: row.get(8)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn inspect_entity(
    connection: &Connection,
    task_id: TaskId,
    entity_id: &str,
) -> Result<serde_json::Value> {
    const TABLES: &[(&str, &str)] = &[
        ("requirements", "requirement"),
        ("gaps", "gap"),
        ("blockers", "blocker"),
        ("work_items", "work_item"),
        ("work_claims", "claim"),
        ("work_attempts", "attempt"),
        ("artifacts", "artifact"),
        ("evidence", "evidence"),
        ("contracts", "contract"),
        ("decisions", "decision"),
        ("coordination_requests", "coordination_request"),
        ("invalidations", "invalidation"),
        ("participant_retirements", "participant_retirement"),
        ("task_participants", "participant"),
    ];
    for (table, entity_type) in TABLES {
        let sql = format!(
            "SELECT json_object('entity_type', ?3, 'data', json_object('id', id, 'task_id', task_id))
             FROM {table} WHERE id = ?1 AND task_id = ?2"
        );
        let value: Option<String> = connection
            .query_row(
                &sql,
                params![entity_id, task_id.to_string(), entity_type],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(value) = value {
            return serde_json::from_str(&value).map_err(Into::into);
        }
    }
    Err(not_found("entity"))
}

pub fn show_retirement(connection: &Connection, retirement_id: &str) -> Result<serde_json::Value> {
    let value: Option<String> = connection
        .query_row(
            "SELECT json_object(
                'id', r.id, 'task_id', r.task_id, 'participant_id', r.participant_id,
                'proposer_participant_id', r.proposer_participant_id, 'reason', r.reason,
                'handoff', json(r.handoff_json), 'status', r.status, 'version', r.version,
                'applied_at', r.applied_at, 'created_at', r.created_at, 'updated_at', r.updated_at
             ) FROM participant_retirements r WHERE r.id = ?1",
            [retirement_id],
            |row| row.get(0),
        )
        .optional()?;
    match value {
        Some(value) => Ok(serde_json::from_str(&value)?),
        None => Err(not_found("ParticipantRetirement")),
    }
}

fn require_active_worker(
    transaction: &Transaction<'_>,
    context: &SessionContext,
    authenticated: &AuthenticatedAgent,
    now: OffsetDateTime,
) -> Result<()> {
    if authenticated.participant.kind != ParticipantKind::Worker {
        return Err(SessionBridgeError::domain(
            ErrorCode::CapabilityNotGranted,
            "observer participant cannot perform this collaboration write",
        ));
    }
    let phase: String = transaction.query_row(
        "SELECT phase FROM tasks WHERE id = ?1",
        [context.task_id.to_string()],
        |row| row.get(0),
    )?;
    if phase != "active" {
        return Err(SessionBridgeError::domain(
            if phase == "paused" {
                ErrorCode::TaskPaused
            } else {
                ErrorCode::TaskNotActive
            },
            format!("Task is {phase}"),
        ));
    }
    match authenticated.participant.state {
        ParticipantState::Active => Ok(()),
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
                    "waiting participant changed concurrently",
                ));
            }
            Ok(())
        }
        ParticipantState::Concluded => Err(SessionBridgeError::domain(
            ErrorCode::SessionAlreadyConcluded,
            "concluded participant must resume first",
        )),
        ParticipantState::Blocked => Err(SessionBridgeError::domain(
            ErrorCode::ParticipantBlocked,
            "blocked participant must resume first",
        )),
        ParticipantState::Departed => Err(SessionBridgeError::domain(
            ErrorCode::ParticipantDeparted,
            "departed participant cannot write collaboration state",
        )),
        ParticipantState::Retired | ParticipantState::Superseded => {
            Err(SessionBridgeError::domain(
                ErrorCode::ParticipantRetired,
                "terminal participant cannot write collaboration state",
            ))
        }
    }
}

fn require_peer_when_available(
    connection: &Connection,
    context: &SessionContext,
    authenticated: &AuthenticatedAgent,
    proposer_session_id: &str,
    target_name: &str,
) -> Result<()> {
    if proposer_session_id != authenticated.session.session_id.to_string() {
        return Ok(());
    }
    let other_workers: i64 = connection.query_row(
        "SELECT COUNT(*) FROM task_participants
         WHERE task_id = ?1 AND completion_epoch = ?2 AND kind = 'worker'
           AND state IN ('active', 'waiting') AND session_id <> ?3",
        params![
            context.task_id.to_string(),
            authenticated.participant.completion_epoch,
            proposer_session_id,
        ],
        |row| row.get(0),
    )?;
    if other_workers > 0 {
        Err(SessionBridgeError::domain(
            ErrorCode::ApprovalRequired,
            format!("{target_name} proposer cannot self-approve while a peer is available"),
        ))
    } else {
        Ok(())
    }
}

fn insert_approval(
    connection: &Connection,
    task_id: TaskId,
    target_type: &str,
    target_id: &str,
    session_id: &str,
    decision: ApprovalDecision,
    now: OffsetDateTime,
) -> Result<String> {
    let id = uuid::Uuid::now_v7().to_string();
    connection.execute(
        "INSERT INTO approvals (
            id, task_id, target_type, target_id, session_id, decision, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            id,
            task_id.to_string(),
            target_type,
            target_id,
            session_id,
            approval_decision_str(decision),
            unix_micros(now),
        ],
    )?;
    Ok(id)
}

fn approval_decision_str(decision: ApprovalDecision) -> &'static str {
    match decision {
        ApprovalDecision::Approve => "approved",
        ApprovalDecision::Reject => "rejected",
    }
}

fn apply_plan_operation(
    connection: &Connection,
    context: &SessionContext,
    authenticated: &AuthenticatedAgent,
    operations_json: &str,
    now: OffsetDateTime,
) -> Result<()> {
    let operation: serde_json::Value = serde_json::from_str(operations_json)?;
    let op = operation
        .get("op")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| invalid("PlanChange operation is missing op"))?;
    let now_micros = unix_micros(now);
    match op {
        "requirement.add" => {
            let id = operation_id(&operation, "requirement_id")?;
            connection.execute(
                "UPDATE requirements SET status = 'active', version = version + 1,
                        updated_at = ?3
                 WHERE id = ?1 AND task_id = ?2 AND status = 'proposed'",
                params![id, context.task_id.to_string(), now_micros],
            )?;
        }
        "work_item.propose" => {
            let id = operation_id(&operation, "work_item_id")?;
            connection.execute(
                "UPDATE work_items SET plan_state = 'active', version = version + 1,
                        updated_at = ?3
                 WHERE id = ?1 AND task_id = ?2 AND plan_state = 'proposed'",
                params![id, context.task_id.to_string(), now_micros],
            )?;
        }
        "requirement.supersede" => {
            let id = operation_id(&operation, "requirement_id")?;
            connection.execute(
                "UPDATE requirements SET status = 'superseded', version = version + 1,
                        updated_at = ?3
                 WHERE id = ?1 AND task_id = ?2 AND status IN ('active', 'proposed')",
                params![id, context.task_id.to_string(), now_micros],
            )?;
        }
        "work_item.relate" => {
            let relation_id = operation_id(&operation, "relation_id")?;
            let from_id = operation_id(&operation, "from_id")?;
            let to_id = operation_id(&operation, "to_id")?;
            let relation_type = operation_id(&operation, "relation_type")?;
            if relation_type == "depends_on"
                && dependency_path_exists(connection, context.task_id, &to_id, &from_id)?
            {
                return Err(invalid("approved relation would create a WorkItem cycle"));
            }
            connection.execute(
                "INSERT INTO work_relations (
                    id, task_id, from_id, to_id, relation_type,
                    created_by_session_id, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    relation_id,
                    context.task_id.to_string(),
                    from_id,
                    to_id,
                    relation_type,
                    authenticated.session.session_id.to_string(),
                    now_micros,
                ],
            )?;
        }
        "work_item.reopen" => {
            let id = operation_id(&operation, "work_item_id")?;
            connection.execute(
                "UPDATE work_items SET result_state = 'open', version = version + 1,
                        submitted_at = NULL, accepted_at = NULL, updated_at = ?3
                 WHERE id = ?1 AND task_id = ?2 AND plan_state = 'active'
                   AND result_state IN ('submitted', 'accepted')",
                params![id, context.task_id.to_string(), now_micros],
            )?;
        }
        "work_item.split" => {
            let parent_id = operation_id(&operation, "parent_id")?;
            let child_ids = operation
                .get("child_ids")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| invalid("split PlanChange is missing child_ids"))?;
            connection.execute(
                "UPDATE work_items SET plan_state = 'superseded', version = version + 1,
                        updated_at = ?3
                 WHERE id = ?1 AND task_id = ?2 AND plan_state = 'active'",
                params![parent_id, context.task_id.to_string(), now_micros],
            )?;
            for child_id in child_ids {
                let child_id = child_id
                    .as_str()
                    .ok_or_else(|| invalid("split child ID is invalid"))?;
                connection.execute(
                    "UPDATE work_items SET plan_state = 'active', version = version + 1,
                            updated_at = ?3
                     WHERE id = ?1 AND task_id = ?2 AND plan_state = 'proposed'",
                    params![child_id, context.task_id.to_string(), now_micros],
                )?;
            }
        }
        _ => {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                format!("unsupported PlanChange operation: {op}"),
            ));
        }
    }
    Ok(())
}

fn reject_plan_operation(
    connection: &Connection,
    task_id: TaskId,
    operations_json: &str,
    now: OffsetDateTime,
) -> Result<()> {
    let operation: serde_json::Value = serde_json::from_str(operations_json)?;
    let op = operation
        .get("op")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    match op {
        "requirement.add" => {
            connection.execute(
                "UPDATE requirements SET status = 'rejected', version = version + 1,
                        updated_at = ?3
                 WHERE id = ?1 AND task_id = ?2 AND status = 'proposed'",
                params![
                    operation_id(&operation, "requirement_id")?,
                    task_id.to_string(),
                    unix_micros(now),
                ],
            )?;
        }
        "work_item.propose" => {
            connection.execute(
                "UPDATE work_items SET plan_state = 'cancelled', version = version + 1,
                        updated_at = ?3
                 WHERE id = ?1 AND task_id = ?2 AND plan_state = 'proposed'",
                params![
                    operation_id(&operation, "work_item_id")?,
                    task_id.to_string(),
                    unix_micros(now),
                ],
            )?;
        }
        "work_item.split" => {
            if let Some(child_ids) = operation
                .get("child_ids")
                .and_then(serde_json::Value::as_array)
            {
                for child_id in child_ids.iter().filter_map(serde_json::Value::as_str) {
                    connection.execute(
                        "UPDATE work_items SET plan_state = 'cancelled', version = version + 1,
                                updated_at = ?3
                         WHERE id = ?1 AND task_id = ?2 AND plan_state = 'proposed'",
                        params![child_id, task_id.to_string(), unix_micros(now)],
                    )?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn operation_id(operation: &serde_json::Value, field: &str) -> Result<String> {
    operation
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| invalid(format!("PlanChange operation is missing {field}")))
}

fn dependency_path_exists(
    connection: &Connection,
    task_id: TaskId,
    start: &str,
    target: &str,
) -> Result<bool> {
    Ok(connection.query_row(
        "WITH RECURSIVE reachable(id) AS (
            SELECT to_id FROM work_relations
             WHERE task_id = ?1 AND from_id = ?2 AND relation_type = 'depends_on'
            UNION
            SELECT r.to_id FROM work_relations r JOIN reachable p ON r.from_id = p.id
             WHERE r.task_id = ?1 AND r.relation_type = 'depends_on'
         ) SELECT EXISTS(SELECT 1 FROM reachable WHERE id = ?3)",
        params![task_id.to_string(), start, target],
        |row| row.get(0),
    )?)
}

fn validate_approval_target(
    connection: &Connection,
    task_id: TaskId,
    target_type: &str,
    target_id: &str,
) -> Result<()> {
    let sql = match target_type {
        "decision" => "SELECT EXISTS(SELECT 1 FROM decisions WHERE id = ?1 AND task_id = ?2)",
        "participant_retirement" => {
            "SELECT EXISTS(SELECT 1 FROM participant_retirements WHERE id = ?1 AND task_id = ?2)"
        }
        "task_reopen" => "SELECT EXISTS(SELECT 1 FROM tasks WHERE id = ?1 AND id = ?2)",
        _ => return Err(invalid("approval target type is invalid")),
    };
    let exists: bool =
        connection.query_row(sql, params![target_id, task_id.to_string()], |row| {
            row.get(0)
        })?;
    if exists {
        Ok(())
    } else {
        Err(not_found("approval target"))
    }
}

fn validate_assigned_session(
    connection: &Connection,
    task_id: TaskId,
    session_id: Option<&str>,
) -> Result<()> {
    let Some(session_id) = session_id else {
        return Ok(());
    };
    let exists: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM agent_sessions WHERE id = ?1 AND task_id = ?2 AND ended_at IS NULL
        )",
        params![session_id, task_id.to_string()],
        |row| row.get(0),
    )?;
    if exists {
        Ok(())
    } else {
        Err(not_found("active assigned AgentSession"))
    }
}

fn validate_contract_owner(
    connection: &Connection,
    context: &SessionContext,
    authenticated: &AuthenticatedAgent,
    owner_type: &str,
    owner_id: &str,
) -> Result<()> {
    let exists = match owner_type {
        "task" => owner_id == context.task_id.to_string(),
        "session" => owner_id == authenticated.session.session_id.to_string(),
        "work_item" => connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM work_items WHERE id = ?1 AND task_id = ?2)",
            params![owner_id, context.task_id.to_string()],
            |row| row.get(0),
        )?,
        _ => false,
    };
    if exists {
        Ok(())
    } else {
        Err(SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            "Contract owner does not exist or is not owned by this Session",
        ))
    }
}

#[allow(clippy::too_many_arguments)]
fn insert_contract_revision(
    connection: &Connection,
    task_id: TaskId,
    contract_id: &str,
    revision_id: &str,
    revision: i64,
    digest: &str,
    compatibility: &str,
    content: &serde_json::Value,
    session_id: &str,
    now: OffsetDateTime,
) -> Result<()> {
    connection.execute(
        "INSERT INTO contract_revisions (
            id, task_id, contract_id, revision, state, digest, compatibility,
            content_json, created_by_session_id, created_at
         ) VALUES (?1, ?2, ?3, ?4, 'published', ?5, ?6, ?7, ?8, ?9)",
        params![
            revision_id,
            task_id.to_string(),
            contract_id,
            revision,
            digest,
            compatibility,
            serde_json::to_string(content)?,
            session_id,
            unix_micros(now),
        ],
    )?;
    Ok(())
}

fn create_contract_invalidations(
    connection: &Connection,
    task_id: TaskId,
    contract_id: &str,
    new_revision: i64,
    now: OffsetDateTime,
) -> Result<()> {
    let targets = connection
        .prepare(
            "SELECT work_item_id, revision FROM work_item_contract_requirements
             WHERE contract_id = ?1 AND revision < ?2",
        )?
        .query_map(params![contract_id, new_revision], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (work_item_id, old_revision) in targets {
        connection.execute(
            "INSERT INTO invalidations (
                id, task_id, source_type, source_id, target_type, target_id,
                old_revision, new_revision, status, reason, created_at
             ) VALUES (?1, ?2, 'contract', ?3, 'work_item', ?4, ?5, ?6,
                       'open', 'Contract revision changed', ?7)
             ON CONFLICT(task_id, source_type, source_id, target_type, target_id)
             WHERE status = 'open'
             DO UPDATE SET old_revision = excluded.old_revision,
                           new_revision = excluded.new_revision,
                           reason = excluded.reason",
            params![
                uuid::Uuid::now_v7().to_string(),
                task_id.to_string(),
                contract_id,
                work_item_id,
                old_revision.to_string(),
                new_revision.to_string(),
                unix_micros(now),
            ],
        )?;
    }
    Ok(())
}

fn ensure_session_has_no_open_work(
    connection: &Connection,
    task_id: TaskId,
    session_id: &str,
) -> Result<()> {
    let counts: (i64, i64, i64) = connection.query_row(
        "SELECT
            (SELECT COUNT(*) FROM work_claims WHERE task_id = ?1 AND session_id = ?2 AND closed_at IS NULL),
            (SELECT COUNT(*) FROM agent_intents WHERE task_id = ?1 AND session_id = ?2 AND cleared_at IS NULL),
            (SELECT COUNT(*) FROM resource_reservations r
             WHERE r.task_id = ?1 AND r.owner_type = 'claim' AND r.released_at IS NULL
               AND EXISTS(SELECT 1 FROM work_claims c WHERE c.id = r.owner_id AND c.session_id = ?2))",
        params![task_id.to_string(), session_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if counts == (0, 0, 0) {
        Ok(())
    } else {
        Err(SessionBridgeError::domain(
            ErrorCode::SessionHasActiveWork,
            "participant still owns Claim, Intent, or Reservation state",
        )
        .with_details(serde_json::json!({
            "active_claims": counts.0,
            "active_intents": counts.1,
            "active_reservations": counts.2,
        })))
    }
}

fn validate_evidence_ids(
    connection: &Connection,
    task_id: TaskId,
    evidence_ids: &[String],
) -> Result<()> {
    for id in evidence_ids {
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM evidence WHERE id = ?1 AND task_id = ?2)",
            params![id, task_id.to_string()],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(not_found("Evidence"));
        }
    }
    Ok(())
}

fn load_repository_identity(
    connection: &Connection,
    workspace_id: crate::domain::WorkspaceId,
) -> Result<String> {
    connection
        .query_row(
            "SELECT repository_identity FROM workspaces WHERE id = ?1",
            [workspace_id.to_string()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| not_found("Workspace"))
}

fn bump_completion_revision(
    connection: &Connection,
    task_id: TaskId,
    now: OffsetDateTime,
) -> Result<()> {
    let changed = connection.execute(
        "UPDATE tasks SET completion_revision = completion_revision + 1, updated_at = ?2
         WHERE id = ?1",
        params![task_id.to_string(), unix_micros(now)],
    )?;
    if changed == 1 {
        Ok(())
    } else {
        Err(not_found("Task"))
    }
}

#[allow(clippy::too_many_arguments)]
fn append_event(
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

fn record_result<T: Serialize>(
    transaction: &Transaction<'_>,
    key: IdempotencyKey<'_>,
    task_id: TaskId,
    payload_hash: &[u8; 32],
    result: &T,
    now: OffsetDateTime,
) -> Result<()> {
    let event_sequence = serde_json::to_value(result)?
        .get("event_sequence")
        .and_then(serde_json::Value::as_i64);
    idempotency::record(
        transaction,
        key,
        Some(&task_id.to_string()),
        payload_hash,
        result,
        event_sequence,
        now,
    )
}

fn session_key<'a>(actor_id: &'a str, request_id: &'a str) -> IdempotencyKey<'a> {
    IdempotencyKey {
        actor_type: "session",
        actor_id,
        request_id,
    }
}

fn normalize_contract_input(
    name: &mut String,
    kind: &mut String,
    owner_type: &mut String,
    owner_id: &mut String,
    compatibility: &mut String,
    content: &serde_json::Value,
) -> Result<()> {
    required_text(name, "Contract name")?;
    canonical_enum(kind, "Contract kind")?;
    canonical_enum(owner_type, "Contract owner type")?;
    required_text(owner_id, "Contract owner ID")?;
    canonical_enum(compatibility, "Contract compatibility")?;
    if !matches!(
        kind.as_str(),
        "api" | "schema" | "file_format" | "behavior" | "build"
    ) {
        return Err(invalid("Contract kind is invalid"));
    }
    if !matches!(owner_type.as_str(), "task" | "work_item" | "session") {
        return Err(invalid("Contract owner type is invalid"));
    }
    validate_compatibility(compatibility)?;
    if !content.is_object() {
        return Err(invalid("Contract content must be a JSON object"));
    }
    Ok(())
}

fn validate_compatibility(value: &str) -> Result<()> {
    if matches!(value, "compatible" | "breaking" | "unknown") {
        Ok(())
    } else {
        Err(invalid("Contract compatibility is invalid"))
    }
}

fn digest_json(value: &serde_json::Value) -> Result<String> {
    let digest = Sha256::digest(serde_json::to_vec(value)?);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn canonical_enum(value: &mut String, field: &str) -> Result<()> {
    *value = value.trim().to_ascii_lowercase();
    if value.is_empty() || value.chars().any(char::is_control) {
        Err(invalid(format!("{field} is invalid")))
    } else {
        Ok(())
    }
}

fn required_text(value: &mut String, field: &str) -> Result<()> {
    *value = value.trim().to_owned();
    if value.is_empty() || value.chars().any(char::is_control) {
        Err(invalid(format!("{field} is required")))
    } else {
        Ok(())
    }
}

fn normalize_strings(values: &mut Vec<String>, field: &str) -> Result<()> {
    for value in values.iter_mut() {
        required_text(value, field)?;
    }
    values.sort();
    values.dedup();
    Ok(())
}

fn validate_request_id(value: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        Err(invalid("request ID is invalid"))
    } else {
        Ok(())
    }
}

fn default_compatibility() -> String {
    "unknown".into()
}

fn invalid(message: impl Into<String>) -> SessionBridgeError {
    SessionBridgeError::domain(ErrorCode::InvalidInput, message)
}

fn not_found(entity: &str) -> SessionBridgeError {
    SessionBridgeError::domain(ErrorCode::NotFound, format!("{entity} was not found"))
}
