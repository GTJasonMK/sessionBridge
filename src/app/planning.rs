use std::collections::BTreeSet;

use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

use crate::{
    Result,
    app::{
        agent::{AgentService, AuthenticatedAgent},
        events::{self, EventActor, EventEntity},
        idempotency::{self, IdempotencyKey},
        runtime_reconciler::reconcile_after_agent_write,
        work::{ClaimResult, ClaimWorkItemRequest, WorkService},
    },
    clock::{Clock, unix_micros},
    domain::{
        ClaimMode, CriterionId, ParticipantKind, ParticipantState, RequirementId, SessionContext,
        TaskPhase, WorkItemId,
    },
    error::{ErrorCode, SessionBridgeError},
    storage::SqliteStore,
};

const REQUIREMENT_KINDS: &[&str] = &[
    "functional",
    "quality",
    "security",
    "compatibility",
    "verification",
    "documentation",
    "coordination",
];
const GAP_SEVERITIES: &[&str] = &["low", "medium", "high", "critical"];
const WORK_KINDS: &[&str] = &[
    "discovery",
    "design",
    "contract",
    "implementation",
    "verification",
    "documentation",
    "coordination",
];
const RISK_LEVELS: &[&str] = &["low", "medium", "high", "critical"];
const REVIEW_POLICIES: &[&str] = &[
    "self_review_allowed",
    "independent_review_required",
    "capability_review_required",
    "risk_based_review",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddRequirementRequest {
    pub kind: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_true")]
    pub required: bool,
    #[serde(default)]
    pub criterion_ids: Vec<CriterionId>,
    #[serde(default)]
    pub base_plan_version: i64,
    pub reason: String,
    #[serde(default)]
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportGapRequest {
    pub kind: String,
    pub severity: String,
    #[serde(default = "default_true")]
    pub required: bool,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub dedupe_key: Option<String>,
    #[serde(default)]
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposeWorkItemRequest {
    pub kind: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_true")]
    pub required: bool,
    #[serde(default)]
    pub priority: i64,
    #[serde(default = "default_risk")]
    pub risk: String,
    #[serde(default = "default_review_policy")]
    pub review_policy: String,
    #[serde(default)]
    pub required_capabilities: Vec<String>,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub expected_artifacts: Vec<String>,
    #[serde(default)]
    pub planned_resources: Vec<String>,
    pub parent_id: Option<WorkItemId>,
    pub dedupe_key: Option<String>,
    #[serde(default)]
    pub requirement_ids: Vec<RequirementId>,
    #[serde(default)]
    pub depends_on: Vec<WorkItemId>,
    #[serde(default)]
    pub base_plan_version: i64,
    pub reason: String,
    #[serde(default)]
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolveGapRequest {
    pub gap_id: String,
    pub reason: String,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupersedeRequirementRequest {
    pub requirement_id: RequirementId,
    pub replacement_id: Option<RequirementId>,
    pub base_plan_version: i64,
    pub reason: String,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelateWorkItemsRequest {
    pub from_id: WorkItemId,
    pub to_id: WorkItemId,
    pub relation_type: String,
    pub base_plan_version: i64,
    pub reason: String,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SplitChildInput {
    pub kind: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_true")]
    pub required: bool,
    #[serde(default)]
    pub priority: i64,
    #[serde(default = "default_risk")]
    pub risk: String,
    #[serde(default = "default_review_policy")]
    pub review_policy: String,
    #[serde(default)]
    pub required_capabilities: Vec<String>,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub expected_artifacts: Vec<String>,
    #[serde(default)]
    pub planned_resources: Vec<String>,
    pub dedupe_key: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<WorkItemId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SplitWorkItemRequest {
    pub item_id: WorkItemId,
    pub expected_item_version: i64,
    pub base_plan_version: i64,
    pub children: Vec<SplitChildInput>,
    pub reason: String,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SplitWorkItemResult {
    pub parent_id: WorkItemId,
    pub parent_plan_state: String,
    pub child_ids: Vec<WorkItemId>,
    pub plan_change_id: String,
    pub plan_change_status: String,
    pub plan_version: i64,
    pub completion_revision: i64,
    pub event_sequence: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReopenWorkItemRequest {
    pub item_id: WorkItemId,
    pub expected_item_version: i64,
    pub base_plan_version: i64,
    pub reason: String,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationMutationResult {
    pub relation_id: String,
    pub from_id: WorkItemId,
    pub to_id: WorkItemId,
    pub relation_type: String,
    pub plan_change_id: String,
    pub plan_change_status: String,
    pub plan_version: i64,
    pub completion_revision: i64,
    pub event_sequence: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequirementView {
    pub id: RequirementId,
    pub kind: String,
    pub title: String,
    pub description: String,
    pub required: bool,
    pub status: String,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GapMutationResult {
    pub gap_id: String,
    pub status: String,
    pub created: bool,
    pub completion_revision: i64,
    pub event_sequence: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkItemView {
    pub id: WorkItemId,
    pub parent_id: Option<WorkItemId>,
    pub kind: String,
    pub title: String,
    pub description: String,
    pub required: bool,
    pub priority: i64,
    pub risk: String,
    pub review_policy: String,
    pub required_capabilities: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub expected_artifacts: Vec<String>,
    pub planned_resources: Vec<String>,
    pub plan_state: String,
    pub result_state: String,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanMutationResult<T> {
    pub entity: T,
    pub plan_change_id: String,
    pub plan_change_status: String,
    pub plan_version: i64,
    pub completion_revision: i64,
    pub event_sequence: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposeAndClaimResult {
    pub plan: PlanMutationResult<WorkItemView>,
    pub claim: ClaimResult,
}

#[derive(Debug, Clone)]
struct TaskPlanningHead {
    autonomy_policy: String,
    plan_version: i64,
    completion_revision: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyMode {
    Applied,
    Proposed,
}

pub struct PlanningService<C> {
    clock: C,
}

impl<C: Clock + Clone> PlanningService<C> {
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

    pub fn add_requirement(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut request: AddRequirementRequest,
    ) -> Result<PlanMutationResult<RequirementView>> {
        canonical_enum(&mut request.kind, "requirement kind")?;
        if !REQUIREMENT_KINDS.contains(&request.kind.as_str()) {
            return invalid_enum("requirement kind", &request.kind, REQUIREMENT_KINDS);
        }
        required_text(&mut request.title, "requirement title")?;
        request.description = request.description.trim().to_owned();
        required_text(&mut request.reason, "plan change reason")?;
        validate_request_id(&request.request_id)?;
        request.criterion_ids.sort_by_key(ToString::to_string);
        request.criterion_ids.dedup();

        let payload_hash = idempotency::hash_payload(&request)?;
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
        let head = require_planner(&transaction, context, &authenticated)?;
        require_plan_version(&head, request.base_plan_version)?;
        validate_criteria(&transaction, context, &request.criterion_ids)?;
        let mode = apply_mode(&head.autonomy_policy)?;
        let requirement_id = RequirementId::new();
        let status = if mode == ApplyMode::Applied {
            "active"
        } else {
            "proposed"
        };
        let now_micros = unix_micros(now);
        transaction.execute(
            "INSERT INTO requirements (
                id, task_id, kind, title, description, required, status, source,
                version, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'agent', 0, ?8, ?8)",
            params![
                requirement_id.to_string(),
                context.task_id.to_string(),
                request.kind,
                request.title,
                request.description,
                bool_i64(request.required),
                status,
                now_micros,
            ],
        )?;
        for criterion_id in &request.criterion_ids {
            transaction.execute(
                "INSERT INTO criterion_coverage (
                    id, task_id, criterion_id, covered_by_type, covered_by_id,
                    status, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, 'requirement', ?4, 'covered', ?5, ?5)",
                params![
                    UuidText::generate(),
                    context.task_id.to_string(),
                    criterion_id.to_string(),
                    requirement_id.to_string(),
                    now_micros,
                ],
            )?;
        }
        let operation = serde_json::json!({
            "op": "requirement.add",
            "requirement_id": requirement_id,
            "criterion_ids": request.criterion_ids,
        });
        let (plan_change_id, plan_change_status, plan_version, completion_revision) =
            finalize_plan_change(
                &transaction,
                context,
                &authenticated,
                &head,
                mode,
                &request.reason,
                &operation,
                now_micros,
            )?;
        activate_waiting_participant(&transaction, &authenticated, now_micros)?;
        let event_sequence = append_entity_event(
            &transaction,
            context,
            &authenticated,
            "requirement.created",
            "requirement",
            &requirement_id.to_string(),
            &serde_json::json!({
                "status": status,
                "plan_version": plan_version,
                "completion_revision": completion_revision,
            }),
            now,
        )?;
        let result = PlanMutationResult {
            entity: RequirementView {
                id: requirement_id,
                kind: request.kind,
                title: request.title,
                description: request.description,
                required: request.required,
                status: status.to_owned(),
                version: 0,
            },
            plan_change_id,
            plan_change_status,
            plan_version,
            completion_revision,
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

    pub fn supersede_requirement(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut request: SupersedeRequirementRequest,
    ) -> Result<PlanMutationResult<RequirementView>> {
        required_text(&mut request.reason, "plan change reason")?;
        validate_request_id(&request.request_id)?;
        if request.replacement_id == Some(request.requirement_id) {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "Requirement cannot supersede itself",
            ));
        }
        let payload_hash = idempotency::hash_payload(&request)?;
        let authenticated =
            AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)?;
        let now = self.clock.now();
        let now_micros = unix_micros(now);
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
        let head = require_planner(&transaction, context, &authenticated)?;
        require_plan_version(&head, request.base_plan_version)?;
        let mut requirement =
            load_requirement_view(&transaction, context.task_id, request.requirement_id)?;
        if !matches!(requirement.status.as_str(), "active" | "proposed") {
            return Err(SessionBridgeError::domain(
                ErrorCode::VersionConflict,
                "Requirement is no longer active",
            ));
        }
        if requirement.required && request.replacement_id.is_none() {
            return Err(SessionBridgeError::domain(
                ErrorCode::ApprovalRequired,
                "required Requirement cannot be removed without a replacement",
            ));
        }
        if let Some(replacement_id) = request.replacement_id {
            let replacement = load_requirement_view(&transaction, context.task_id, replacement_id)?;
            if !matches!(replacement.status.as_str(), "active" | "proposed") {
                return Err(SessionBridgeError::domain(
                    ErrorCode::InvalidInput,
                    "replacement Requirement is not active",
                ));
            }
        }
        let mode = apply_mode(&head.autonomy_policy)?;
        if mode == ApplyMode::Applied {
            let changed = transaction.execute(
                "UPDATE requirements SET status = 'superseded', version = version + 1,
                        updated_at = ?3
                 WHERE id = ?1 AND task_id = ?2 AND status IN ('active', 'proposed')
                   AND version = ?4",
                params![
                    request.requirement_id.to_string(),
                    context.task_id.to_string(),
                    now_micros,
                    requirement.version,
                ],
            )?;
            if changed != 1 {
                return Err(SessionBridgeError::domain(
                    ErrorCode::VersionConflict,
                    "Requirement changed concurrently",
                ));
            }
            transaction.execute(
                "UPDATE criterion_coverage SET status = 'invalidated', updated_at = ?3
                 WHERE task_id = ?1 AND covered_by_type = 'requirement' AND covered_by_id = ?2",
                params![
                    context.task_id.to_string(),
                    request.requirement_id.to_string(),
                    now_micros,
                ],
            )?;
            if let Some(replacement_id) = request.replacement_id {
                transaction.execute(
                    "INSERT OR IGNORE INTO criterion_coverage (
                        id, task_id, criterion_id, covered_by_type, covered_by_id,
                        status, created_at, updated_at
                     ) SELECT ?4 || '-' || criterion_id, task_id, criterion_id,
                              'requirement', ?3, 'covered', ?5, ?5
                       FROM criterion_coverage
                       WHERE task_id = ?1 AND covered_by_type = 'requirement'
                         AND covered_by_id = ?2",
                    params![
                        context.task_id.to_string(),
                        request.requirement_id.to_string(),
                        replacement_id.to_string(),
                        UuidText::generate(),
                        now_micros,
                    ],
                )?;
            }
            requirement.status = "superseded".into();
            requirement.version += 1;
        }
        let operation = serde_json::json!({
            "op": "requirement.supersede",
            "requirement_id": request.requirement_id,
            "replacement_id": request.replacement_id,
        });
        let (plan_change_id, plan_change_status, plan_version, completion_revision) =
            finalize_plan_change(
                &transaction,
                context,
                &authenticated,
                &head,
                mode,
                &request.reason,
                &operation,
                now_micros,
            )?;
        activate_waiting_participant(&transaction, &authenticated, now_micros)?;
        let event_sequence = append_entity_event(
            &transaction,
            context,
            &authenticated,
            "requirement.superseded",
            "requirement",
            &request.requirement_id.to_string(),
            &serde_json::json!({
                "replacement_id": request.replacement_id,
                "applied": mode == ApplyMode::Applied,
                "plan_version": plan_version,
            }),
            now,
        )?;
        let result = PlanMutationResult {
            entity: requirement,
            plan_change_id,
            plan_change_status,
            plan_version,
            completion_revision,
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

    pub fn report_gap(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut request: ReportGapRequest,
    ) -> Result<GapMutationResult> {
        canonical_enum(&mut request.kind, "gap kind")?;
        canonical_enum(&mut request.severity, "gap severity")?;
        if !GAP_SEVERITIES.contains(&request.severity.as_str()) {
            return invalid_enum("gap severity", &request.severity, GAP_SEVERITIES);
        }
        required_text(&mut request.title, "gap title")?;
        request.description = request.description.trim().to_owned();
        request.dedupe_key = canonical_optional(request.dedupe_key, "gap dedupe key")?;
        validate_request_id(&request.request_id)?;
        let payload_hash = idempotency::hash_payload(&request)?;
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
        let head = require_planner(&transaction, context, &authenticated)?;
        if let Some(dedupe_key) = request.dedupe_key.as_deref()
            && let Some(existing) = transaction
                .query_row(
                    "SELECT id FROM gaps WHERE task_id = ?1 AND dedupe_key = ?2 AND status = 'open'",
                    params![context.task_id.to_string(), dedupe_key],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
        {
            return Err(SessionBridgeError::domain(
                ErrorCode::VersionConflict,
                "an open Gap already uses this dedupe key",
            )
            .with_details(serde_json::json!({ "existing_gap_id": existing })));
        }
        let gap_id = UuidText::generate();
        let now_micros = unix_micros(now);
        transaction.execute(
            "INSERT INTO gaps (
                id, task_id, kind, severity, required, dedupe_key, status,
                title, description, reported_by_session_id, version, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'open', ?7, ?8, ?9, 0, ?10, ?10)",
            params![
                gap_id,
                context.task_id.to_string(),
                request.kind,
                request.severity,
                bool_i64(request.required),
                request.dedupe_key,
                request.title,
                request.description,
                authenticated.session.session_id.to_string(),
                now_micros,
            ],
        )?;
        let completion_revision = increment_completion_revision(
            &transaction,
            context,
            head.completion_revision,
            now_micros,
        )?;
        activate_waiting_participant(&transaction, &authenticated, now_micros)?;
        let event_sequence = append_entity_event(
            &transaction,
            context,
            &authenticated,
            "gap.reported",
            "gap",
            &gap_id,
            &serde_json::json!({
                "severity": request.severity,
                "required": request.required,
                "completion_revision": completion_revision,
            }),
            now,
        )?;
        let result = GapMutationResult {
            gap_id,
            status: "open".into(),
            created: true,
            completion_revision,
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

    pub fn resolve_gap(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut request: ResolveGapRequest,
    ) -> Result<GapMutationResult> {
        required_text(&mut request.gap_id, "Gap ID")?;
        required_text(&mut request.reason, "Gap resolution reason")?;
        request.evidence_ids.sort();
        request.evidence_ids.dedup();
        validate_request_id(&request.request_id)?;
        let payload_hash = idempotency::hash_payload(&request)?;
        let authenticated =
            AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)?;
        let now = self.clock.now();
        let now_micros = unix_micros(now);
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
        let head = require_planner(&transaction, context, &authenticated)?;
        validate_string_evidence_ids(&transaction, context.task_id, &request.evidence_ids)?;
        let changed = transaction.execute(
            "UPDATE gaps SET status = 'resolved', version = version + 1,
                    resolved_at = ?3, updated_at = ?3
             WHERE id = ?1 AND task_id = ?2 AND status = 'open'",
            params![request.gap_id, context.task_id.to_string(), now_micros],
        )?;
        if changed != 1 {
            let exists: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM gaps WHERE id = ?1 AND task_id = ?2)",
                params![request.gap_id, context.task_id.to_string()],
                |row| row.get(0),
            )?;
            return Err(SessionBridgeError::domain(
                if exists {
                    ErrorCode::VersionConflict
                } else {
                    ErrorCode::NotFound
                },
                if exists {
                    "Gap is no longer open"
                } else {
                    "Gap was not found"
                },
            ));
        }
        let completion_revision = increment_completion_revision(
            &transaction,
            context,
            head.completion_revision,
            now_micros,
        )?;
        activate_waiting_participant(&transaction, &authenticated, now_micros)?;
        let event_sequence = append_entity_event(
            &transaction,
            context,
            &authenticated,
            "gap.resolved",
            "gap",
            &request.gap_id,
            &serde_json::json!({
                "reason": request.reason,
                "evidence_ids": request.evidence_ids,
                "completion_revision": completion_revision,
            }),
            now,
        )?;
        let result = GapMutationResult {
            gap_id: request.gap_id,
            status: "resolved".into(),
            created: false,
            completion_revision,
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

    pub fn propose_work_item(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut request: ProposeWorkItemRequest,
    ) -> Result<PlanMutationResult<WorkItemView>> {
        normalize_propose_work_item_request(&mut request)?;

        let payload_hash = idempotency::hash_payload(&request)?;
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
        let result = self.propose_work_item_in_transaction(
            &transaction,
            context,
            &authenticated,
            &request,
            now,
            false,
            true,
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

    pub fn propose_and_claim(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut request: ProposeWorkItemRequest,
    ) -> Result<ProposeAndClaimResult> {
        normalize_propose_work_item_request(&mut request)?;
        let payload_hash = idempotency::hash_payload(&serde_json::json!({
            "operation": "work_item.propose_and_claim",
            "request": &request,
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

        let plan = self.propose_work_item_in_transaction(
            &transaction,
            context,
            &authenticated,
            &request,
            now,
            true,
            false,
        )?;
        let claim_request = ClaimWorkItemRequest {
            item_id: plan.entity.id,
            mode: ClaimMode::Execute,
            expected_item_version: plan.entity.version,
            observed_plan_version: plan.plan_version,
            resources: Vec::new(),
            inputs: Vec::new(),
            lease_seconds: None,
            request_id: request.request_id.clone(),
        };
        let claim = WorkService::new(self.clock.clone()).claim_in_transaction(
            &transaction,
            context,
            &authenticated,
            &claim_request,
            now,
            crate::app::work::ClaimTransactionOptions {
                adopt_existing: false,
                bump_revision: false,
            },
        )?;
        let result = ProposeAndClaimResult { plan, claim };
        idempotency::record(
            &transaction,
            key,
            Some(&context.task_id.to_string()),
            &payload_hash,
            &result,
            Some(result.claim.event_sequence),
            now,
        )?;
        transaction.commit()?;
        self.reconcile_after_write(store, context)?;
        Ok(result)
    }

    #[allow(clippy::too_many_arguments)]
    fn propose_work_item_in_transaction(
        &self,
        transaction: &Transaction<'_>,
        context: &SessionContext,
        authenticated: &AuthenticatedAgent,
        request: &ProposeWorkItemRequest,
        now: time::OffsetDateTime,
        require_applied: bool,
        activate_participant: bool,
    ) -> Result<PlanMutationResult<WorkItemView>> {
        let head = require_planner(transaction, context, authenticated)?;
        require_plan_version(&head, request.base_plan_version)?;
        validate_requirements(transaction, context, &request.requirement_ids)?;
        validate_work_items(transaction, context, request.parent_id, &request.depends_on)?;
        if let Some(dedupe_key) = request.dedupe_key.as_deref()
            && let Some(existing) = transaction
                .query_row(
                    "SELECT id FROM work_items
                     WHERE task_id = ?1 AND dedupe_key = ?2
                       AND plan_state IN ('proposed', 'active')",
                    params![context.task_id.to_string(), dedupe_key],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
        {
            return Err(SessionBridgeError::domain(
                ErrorCode::VersionConflict,
                "an active WorkItem already uses this dedupe key",
            )
            .with_details(serde_json::json!({ "existing_work_item_id": existing })));
        }

        let mode = apply_mode(&head.autonomy_policy)?;
        if require_applied && mode != ApplyMode::Applied {
            return Err(SessionBridgeError::domain(
                ErrorCode::ProposalReviewRequired,
                "atomic propose-and-claim requires bounded or open autonomy policy",
            ));
        }
        let plan_state = if mode == ApplyMode::Applied {
            "active"
        } else {
            "proposed"
        };
        let item_id = WorkItemId::new();
        let now_micros = unix_micros(now);
        transaction.execute(
            "INSERT INTO work_items (
                id, task_id, parent_id, kind, title, description, required, priority,
                risk, review_policy, required_capabilities_json, acceptance_criteria_json,
                expected_artifacts_json, planned_resources_json, plan_state, result_state,
                created_by_session_id, dedupe_key, version, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                       ?14, ?15, 'open', ?16, ?17, 0, ?18, ?18)",
            params![
                item_id.to_string(),
                context.task_id.to_string(),
                request.parent_id.map(|id| id.to_string()),
                &request.kind,
                &request.title,
                &request.description,
                bool_i64(request.required),
                request.priority,
                &request.risk,
                &request.review_policy,
                serde_json::to_string(&request.required_capabilities)?,
                serde_json::to_string(&request.acceptance_criteria)?,
                serde_json::to_string(&request.expected_artifacts)?,
                serde_json::to_string(&request.planned_resources)?,
                plan_state,
                authenticated.session.session_id.to_string(),
                request.dedupe_key.as_deref(),
                now_micros,
            ],
        )?;
        for requirement_id in &request.requirement_ids {
            transaction.execute(
                "INSERT INTO requirement_coverage (
                    id, task_id, requirement_id, covered_by_type, covered_by_id,
                    coverage_state, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, 'work_item', ?4, 'planned', ?5, ?5)",
                params![
                    UuidText::generate(),
                    context.task_id.to_string(),
                    requirement_id.to_string(),
                    item_id.to_string(),
                    now_micros,
                ],
            )?;
        }
        for dependency_id in &request.depends_on {
            transaction.execute(
                "INSERT INTO work_relations (
                    id, task_id, from_id, to_id, relation_type,
                    created_by_session_id, created_at
                 ) VALUES (?1, ?2, ?3, ?4, 'depends_on', ?5, ?6)",
                params![
                    UuidText::generate(),
                    context.task_id.to_string(),
                    item_id.to_string(),
                    dependency_id.to_string(),
                    authenticated.session.session_id.to_string(),
                    now_micros,
                ],
            )?;
        }
        let operation = serde_json::json!({
            "op": "work_item.propose",
            "work_item_id": item_id,
            "requirement_ids": &request.requirement_ids,
            "depends_on": &request.depends_on,
        });
        let (plan_change_id, plan_change_status, plan_version, completion_revision) =
            finalize_plan_change(
                transaction,
                context,
                authenticated,
                &head,
                mode,
                &request.reason,
                &operation,
                now_micros,
            )?;
        if activate_participant {
            activate_waiting_participant(transaction, authenticated, now_micros)?;
        }
        let event_sequence = append_entity_event(
            transaction,
            context,
            authenticated,
            "work.proposed",
            "work_item",
            &item_id.to_string(),
            &serde_json::json!({
                "plan_state": plan_state,
                "plan_version": plan_version,
                "completion_revision": completion_revision,
            }),
            now,
        )?;
        let result = PlanMutationResult {
            entity: WorkItemView {
                id: item_id,
                parent_id: request.parent_id,
                kind: request.kind.clone(),
                title: request.title.clone(),
                description: request.description.clone(),
                required: request.required,
                priority: request.priority,
                risk: request.risk.clone(),
                review_policy: request.review_policy.clone(),
                required_capabilities: request.required_capabilities.clone(),
                acceptance_criteria: request.acceptance_criteria.clone(),
                expected_artifacts: request.expected_artifacts.clone(),
                planned_resources: request.planned_resources.clone(),
                plan_state: plan_state.into(),
                result_state: "open".into(),
                version: 0,
            },
            plan_change_id,
            plan_change_status,
            plan_version,
            completion_revision,
            event_sequence,
        };
        Ok(result)
    }

    pub fn relate_work_items(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut request: RelateWorkItemsRequest,
    ) -> Result<RelationMutationResult> {
        canonical_enum(&mut request.relation_type, "work relation type")?;
        if !matches!(
            request.relation_type.as_str(),
            "depends_on" | "duplicates" | "supersedes"
        ) {
            return invalid_enum(
                "work relation type",
                &request.relation_type,
                &["depends_on", "duplicates", "supersedes"],
            );
        }
        if request.from_id == request.to_id {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "WorkItem relation cannot reference the same item twice",
            ));
        }
        required_text(&mut request.reason, "plan change reason")?;
        validate_request_id(&request.request_id)?;
        let payload_hash = idempotency::hash_payload(&request)?;
        let authenticated =
            AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)?;
        let now = self.clock.now();
        let now_micros = unix_micros(now);
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
        let head = require_planner(&transaction, context, &authenticated)?;
        require_plan_version(&head, request.base_plan_version)?;
        validate_work_items(
            &transaction,
            context,
            None,
            &[request.from_id, request.to_id],
        )?;
        if request.relation_type == "depends_on"
            && dependency_path_exists(
                &transaction,
                context.task_id,
                request.to_id,
                request.from_id,
            )?
        {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "depends_on relation would create a WorkItem cycle",
            ));
        }
        let mode = apply_mode(&head.autonomy_policy)?;
        let relation_id = UuidText::generate();
        if mode == ApplyMode::Applied {
            transaction.execute(
                "INSERT INTO work_relations (
                    id, task_id, from_id, to_id, relation_type,
                    created_by_session_id, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    relation_id,
                    context.task_id.to_string(),
                    request.from_id.to_string(),
                    request.to_id.to_string(),
                    request.relation_type,
                    authenticated.session.session_id.to_string(),
                    now_micros,
                ],
            )?;
        }
        let operation = serde_json::json!({
            "op": "work_item.relate",
            "relation_id": relation_id,
            "from_id": request.from_id,
            "to_id": request.to_id,
            "relation_type": request.relation_type,
        });
        let (plan_change_id, plan_change_status, plan_version, completion_revision) =
            finalize_plan_change(
                &transaction,
                context,
                &authenticated,
                &head,
                mode,
                &request.reason,
                &operation,
                now_micros,
            )?;
        activate_waiting_participant(&transaction, &authenticated, now_micros)?;
        let event_sequence = append_entity_event(
            &transaction,
            context,
            &authenticated,
            "work.related",
            "work_relation",
            &relation_id,
            &serde_json::json!({
                "from_id": request.from_id,
                "to_id": request.to_id,
                "relation_type": request.relation_type,
                "applied": mode == ApplyMode::Applied,
            }),
            now,
        )?;
        let result = RelationMutationResult {
            relation_id,
            from_id: request.from_id,
            to_id: request.to_id,
            relation_type: request.relation_type,
            plan_change_id,
            plan_change_status,
            plan_version,
            completion_revision,
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

    pub fn reopen_work_item(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut request: ReopenWorkItemRequest,
    ) -> Result<PlanMutationResult<WorkItemView>> {
        required_text(&mut request.reason, "work item reopen reason")?;
        validate_request_id(&request.request_id)?;
        request.evidence_ids.sort();
        request.evidence_ids.dedup();
        let payload_hash = idempotency::hash_payload(&request)?;
        let authenticated =
            AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)?;
        let now = self.clock.now();
        let now_micros = unix_micros(now);
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
        let head = require_planner(&transaction, context, &authenticated)?;
        require_plan_version(&head, request.base_plan_version)?;
        let mut item = load_work_item_view(&transaction, context.task_id, request.item_id)?;
        if item.version != request.expected_item_version {
            return Err(SessionBridgeError::domain(
                ErrorCode::VersionConflict,
                "WorkItem version changed before reopen",
            ));
        }
        if item.plan_state != "active" || item.result_state == "open" {
            return Err(SessionBridgeError::domain(
                ErrorCode::ItemNotReady,
                "only submitted or accepted active WorkItems can be reopened",
            ));
        }
        require_no_open_claims(&transaction, item.id)?;
        validate_string_evidence_ids(&transaction, context.task_id, &request.evidence_ids)?;
        let mode = apply_mode(&head.autonomy_policy)?;
        if mode == ApplyMode::Applied {
            transaction.execute(
                "UPDATE resource_reservations SET released_at = ?3
                 WHERE owner_type = 'change_set' AND released_at IS NULL
                   AND owner_id IN (
                     SELECT c.id FROM change_sets c JOIN work_attempts a ON a.id = c.attempt_id
                     WHERE c.task_id = ?1 AND a.work_item_id = ?2
                   )",
                params![context.task_id.to_string(), item.id.to_string(), now_micros],
            )?;
            transaction.execute(
                "UPDATE change_sets SET integration_state = 'superseded', updated_at = ?3
                 WHERE task_id = ?1 AND integration_state IN ('pending', 'failed', 'integrated')
                   AND attempt_id IN (
                     SELECT id FROM work_attempts WHERE task_id = ?1 AND work_item_id = ?2
                   )",
                params![context.task_id.to_string(), item.id.to_string(), now_micros],
            )?;
            let changed = transaction.execute(
                "UPDATE work_items SET result_state = 'open', version = version + 1,
                        submitted_at = NULL, accepted_at = NULL, updated_at = ?3
                 WHERE id = ?1 AND task_id = ?2 AND version = ?4",
                params![
                    item.id.to_string(),
                    context.task_id.to_string(),
                    now_micros,
                    item.version,
                ],
            )?;
            if changed != 1 {
                return Err(SessionBridgeError::domain(
                    ErrorCode::VersionConflict,
                    "WorkItem changed concurrently while reopening",
                ));
            }
            create_dependent_invalidations(
                &transaction,
                context.task_id,
                item.id,
                item.version,
                item.version + 1,
                &request.reason,
                now_micros,
            )?;
            item.result_state = "open".into();
            item.version += 1;
        }
        let operation = serde_json::json!({
            "op": "work_item.reopen",
            "work_item_id": item.id,
            "evidence_ids": request.evidence_ids,
        });
        let (plan_change_id, plan_change_status, plan_version, completion_revision) =
            finalize_plan_change(
                &transaction,
                context,
                &authenticated,
                &head,
                mode,
                &request.reason,
                &operation,
                now_micros,
            )?;
        activate_waiting_participant(&transaction, &authenticated, now_micros)?;
        let event_sequence = append_entity_event(
            &transaction,
            context,
            &authenticated,
            "work.reopened",
            "work_item",
            &item.id.to_string(),
            &serde_json::json!({
                "applied": mode == ApplyMode::Applied,
                "evidence_ids": request.evidence_ids,
                "plan_version": plan_version,
            }),
            now,
        )?;
        let result = PlanMutationResult {
            entity: item,
            plan_change_id,
            plan_change_status,
            plan_version,
            completion_revision,
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

    pub fn split_work_item(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut request: SplitWorkItemRequest,
    ) -> Result<SplitWorkItemResult> {
        if !(2..=32).contains(&request.children.len()) {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "WorkItem split requires between 2 and 32 children",
            ));
        }
        required_text(&mut request.reason, "plan change reason")?;
        validate_request_id(&request.request_id)?;
        for child in &mut request.children {
            normalize_split_child(child)?;
        }
        let mut child_dedupe = BTreeSet::new();
        for child in &request.children {
            if let Some(key) = &child.dedupe_key
                && !child_dedupe.insert(key.clone())
            {
                return Err(SessionBridgeError::domain(
                    ErrorCode::InvalidInput,
                    "split children contain duplicate dedupe keys",
                ));
            }
        }
        let payload_hash = idempotency::hash_payload(&request)?;
        let authenticated =
            AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)?;
        let now = self.clock.now();
        let now_micros = unix_micros(now);
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
        let head = require_planner(&transaction, context, &authenticated)?;
        require_plan_version(&head, request.base_plan_version)?;
        let parent = load_work_item_view(&transaction, context.task_id, request.item_id)?;
        if parent.version != request.expected_item_version
            || parent.plan_state != "active"
            || parent.result_state != "open"
        {
            return Err(SessionBridgeError::domain(
                ErrorCode::VersionConflict,
                "WorkItem is no longer an active open item at the expected version",
            ));
        }
        require_no_open_claims(&transaction, parent.id)?;
        for child in &request.children {
            validate_work_items(&transaction, context, None, &child.depends_on)?;
            if let Some(dedupe_key) = child.dedupe_key.as_deref()
                && transaction.query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM work_items WHERE task_id = ?1 AND dedupe_key = ?2
                          AND plan_state IN ('active', 'proposed')
                    )",
                    params![context.task_id.to_string(), dedupe_key],
                    |row| row.get::<_, bool>(0),
                )?
            {
                return Err(SessionBridgeError::domain(
                    ErrorCode::VersionConflict,
                    "a split child dedupe key already exists",
                )
                .with_details(serde_json::json!({ "dedupe_key": dedupe_key })));
            }
        }
        let inherited_dependencies = transaction
            .prepare(
                "SELECT to_id FROM work_relations
                 WHERE task_id = ?1 AND from_id = ?2 AND relation_type = 'depends_on'",
            )?
            .query_map(
                params![context.task_id.to_string(), parent.id.to_string()],
                |row| row.get::<_, String>(0),
            )?
            .map(|row| {
                let value = row?;
                parse_id(&value, "work_relations.to_id")
            })
            .collect::<Result<Vec<WorkItemId>>>()?;
        let mode = apply_mode(&head.autonomy_policy)?;
        let child_plan_state = if mode == ApplyMode::Applied {
            "active"
        } else {
            "proposed"
        };
        let mut child_ids = Vec::new();
        for child in &request.children {
            let child_id = WorkItemId::new();
            transaction.execute(
                "INSERT INTO work_items (
                    id, task_id, parent_id, kind, title, description, required, priority,
                risk, review_policy, required_capabilities_json, acceptance_criteria_json,
                    expected_artifacts_json, planned_resources_json, plan_state, result_state,
                    created_by_session_id, dedupe_key, version, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                           ?14, ?15, 'open', ?16, ?17, 0, ?18, ?18)",
                params![
                    child_id.to_string(),
                    context.task_id.to_string(),
                    parent.id.to_string(),
                    child.kind,
                    child.title,
                    child.description,
                    bool_i64(child.required),
                    child.priority,
                    child.risk,
                    child.review_policy,
                    serde_json::to_string(&child.required_capabilities)?,
                    serde_json::to_string(&child.acceptance_criteria)?,
                    serde_json::to_string(&child.expected_artifacts)?,
                    serde_json::to_string(&child.planned_resources)?,
                    child_plan_state,
                    authenticated.session.session_id.to_string(),
                    child.dedupe_key,
                    now_micros,
                ],
            )?;
            let mut dependencies = inherited_dependencies.clone();
            dependencies.extend(child.depends_on.iter().copied());
            dependencies.sort_by_key(ToString::to_string);
            dependencies.dedup();
            for dependency_id in dependencies {
                transaction.execute(
                    "INSERT OR IGNORE INTO work_relations (
                        id, task_id, from_id, to_id, relation_type,
                        created_by_session_id, created_at
                     ) VALUES (?1, ?2, ?3, ?4, 'depends_on', ?5, ?6)",
                    params![
                        UuidText::generate(),
                        context.task_id.to_string(),
                        child_id.to_string(),
                        dependency_id.to_string(),
                        authenticated.session.session_id.to_string(),
                        now_micros,
                    ],
                )?;
            }
            transaction.execute(
                "INSERT OR IGNORE INTO requirement_coverage (
                    id, task_id, requirement_id, covered_by_type, covered_by_id,
                    coverage_state, created_at, updated_at
                 ) SELECT ?3 || '-' || requirement_id, task_id, requirement_id,
                          'work_item', ?2, 'planned', ?4, ?4
                   FROM requirement_coverage
                   WHERE task_id = ?1 AND covered_by_type = 'work_item' AND covered_by_id = ?5",
                params![
                    context.task_id.to_string(),
                    child_id.to_string(),
                    UuidText::generate(),
                    now_micros,
                    parent.id.to_string(),
                ],
            )?;
            child_ids.push(child_id);
        }
        if mode == ApplyMode::Applied {
            let dependents = transaction
                .prepare(
                    "SELECT from_id FROM work_relations
                     WHERE task_id = ?1 AND to_id = ?2 AND relation_type = 'depends_on'",
                )?
                .query_map(
                    params![context.task_id.to_string(), parent.id.to_string()],
                    |row| row.get::<_, String>(0),
                )?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            transaction.execute(
                "DELETE FROM work_relations
                 WHERE task_id = ?1 AND to_id = ?2 AND relation_type = 'depends_on'",
                params![context.task_id.to_string(), parent.id.to_string()],
            )?;
            for dependent in dependents {
                for child_id in &child_ids {
                    transaction.execute(
                        "INSERT OR IGNORE INTO work_relations (
                            id, task_id, from_id, to_id, relation_type,
                            created_by_session_id, created_at
                         ) VALUES (?1, ?2, ?3, ?4, 'depends_on', ?5, ?6)",
                        params![
                            UuidText::generate(),
                            context.task_id.to_string(),
                            dependent,
                            child_id.to_string(),
                            authenticated.session.session_id.to_string(),
                            now_micros,
                        ],
                    )?;
                }
            }
            let changed = transaction.execute(
                "UPDATE work_items SET plan_state = 'superseded', version = version + 1,
                        updated_at = ?3
                 WHERE id = ?1 AND task_id = ?2 AND version = ?4",
                params![
                    parent.id.to_string(),
                    context.task_id.to_string(),
                    now_micros,
                    parent.version,
                ],
            )?;
            if changed != 1 {
                return Err(SessionBridgeError::domain(
                    ErrorCode::VersionConflict,
                    "parent WorkItem changed concurrently while splitting",
                ));
            }
        }
        let operation = serde_json::json!({
            "op": "work_item.split",
            "parent_id": parent.id,
            "child_ids": child_ids,
        });
        let (plan_change_id, plan_change_status, plan_version, completion_revision) =
            finalize_plan_change(
                &transaction,
                context,
                &authenticated,
                &head,
                mode,
                &request.reason,
                &operation,
                now_micros,
            )?;
        activate_waiting_participant(&transaction, &authenticated, now_micros)?;
        let event_sequence = append_entity_event(
            &transaction,
            context,
            &authenticated,
            "work.split",
            "work_item",
            &parent.id.to_string(),
            &serde_json::json!({
                "child_ids": child_ids,
                "applied": mode == ApplyMode::Applied,
                "plan_version": plan_version,
            }),
            now,
        )?;
        let result = SplitWorkItemResult {
            parent_id: parent.id,
            parent_plan_state: if mode == ApplyMode::Applied {
                "superseded".into()
            } else {
                parent.plan_state
            },
            child_ids,
            plan_change_id,
            plan_change_status,
            plan_version,
            completion_revision,
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
}

fn load_requirement_view(
    connection: &Transaction<'_>,
    task_id: crate::domain::TaskId,
    requirement_id: RequirementId,
) -> Result<RequirementView> {
    let row: Option<(String, String, String, i64, String, i64)> = connection
        .query_row(
            "SELECT kind, title, description, required, status, version
             FROM requirements WHERE id = ?1 AND task_id = ?2",
            params![requirement_id.to_string(), task_id.to_string()],
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
        return Err(SessionBridgeError::domain(
            ErrorCode::NotFound,
            "Requirement was not found",
        ));
    };
    Ok(RequirementView {
        id: requirement_id,
        kind: row.0,
        title: row.1,
        description: row.2,
        required: row.3 != 0,
        status: row.4,
        version: row.5,
    })
}

fn load_work_item_view(
    connection: &Transaction<'_>,
    task_id: crate::domain::TaskId,
    item_id: WorkItemId,
) -> Result<WorkItemView> {
    #[allow(clippy::type_complexity)]
    let row: Option<(
        Option<String>,
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
        String,
        String,
        String,
        i64,
    )> = connection
        .query_row(
            "SELECT parent_id, kind, title, description, required, priority, risk, review_policy,
                    required_capabilities_json, acceptance_criteria_json,
                    expected_artifacts_json, planned_resources_json,
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
                    row.get(11)?,
                    row.get(12)?,
                    row.get(13)?,
                    row.get(14)?,
                ))
            },
        )
        .optional()?;
    let Some(row) = row else {
        return Err(SessionBridgeError::domain(
            ErrorCode::NotFound,
            "WorkItem was not found",
        ));
    };
    Ok(WorkItemView {
        id: item_id,
        parent_id: row
            .0
            .map(|value| parse_id(&value, "work_items.parent_id"))
            .transpose()?,
        kind: row.1,
        title: row.2,
        description: row.3,
        required: row.4 != 0,
        priority: row.5,
        risk: row.6,
        review_policy: row.7,
        required_capabilities: serde_json::from_str(&row.8)?,
        acceptance_criteria: serde_json::from_str(&row.9)?,
        expected_artifacts: serde_json::from_str(&row.10)?,
        planned_resources: serde_json::from_str(&row.11)?,
        plan_state: row.12,
        result_state: row.13,
        version: row.14,
    })
}

fn dependency_path_exists(
    connection: &Transaction<'_>,
    task_id: crate::domain::TaskId,
    start: WorkItemId,
    target: WorkItemId,
) -> Result<bool> {
    Ok(connection.query_row(
        "WITH RECURSIVE reachable(id) AS (
            SELECT to_id FROM work_relations
             WHERE task_id = ?1 AND from_id = ?2 AND relation_type = 'depends_on'
            UNION
            SELECT r.to_id FROM work_relations r JOIN reachable p ON r.from_id = p.id
             WHERE r.task_id = ?1 AND r.relation_type = 'depends_on'
         ) SELECT EXISTS(SELECT 1 FROM reachable WHERE id = ?3)",
        params![task_id.to_string(), start.to_string(), target.to_string()],
        |row| row.get(0),
    )?)
}

fn require_no_open_claims(connection: &Transaction<'_>, item_id: WorkItemId) -> Result<()> {
    let claim_id: Option<String> = connection
        .query_row(
            "SELECT id FROM work_claims WHERE work_item_id = ?1 AND closed_at IS NULL LIMIT 1",
            [item_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(claim_id) = claim_id {
        return Err(SessionBridgeError::domain(
            ErrorCode::SessionHasActiveWork,
            "WorkItem plan mutation requires all Claims to be closed first",
        )
        .with_details(serde_json::json!({ "claim_id": claim_id })));
    }
    Ok(())
}

fn validate_string_evidence_ids(
    connection: &Transaction<'_>,
    task_id: crate::domain::TaskId,
    evidence_ids: &[String],
) -> Result<()> {
    for evidence_id in evidence_ids {
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM evidence WHERE id = ?1 AND task_id = ?2)",
            params![evidence_id, task_id.to_string()],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(SessionBridgeError::domain(
                ErrorCode::NotFound,
                format!("Evidence {evidence_id} was not found in this Task"),
            ));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn create_dependent_invalidations(
    connection: &Transaction<'_>,
    task_id: crate::domain::TaskId,
    source_id: WorkItemId,
    old_version: i64,
    new_version: i64,
    reason: &str,
    now_micros: i64,
) -> Result<()> {
    let dependents = connection
        .prepare(
            "SELECT from_id FROM work_relations
             WHERE task_id = ?1 AND to_id = ?2 AND relation_type = 'depends_on'",
        )?
        .query_map(params![task_id.to_string(), source_id.to_string()], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for dependent in dependents {
        connection.execute(
            "INSERT INTO invalidations (
                id, task_id, source_type, source_id, target_type, target_id,
                old_revision, new_revision, status, reason, created_at
             ) VALUES (?1, ?2, 'work_item', ?3, 'work_item', ?4, ?5, ?6,
                       'open', ?7, ?8)
             ON CONFLICT(task_id, source_type, source_id, target_type, target_id)
             WHERE status = 'open'
             DO UPDATE SET old_revision = excluded.old_revision,
                           new_revision = excluded.new_revision,
                           reason = excluded.reason",
            params![
                UuidText::generate(),
                task_id.to_string(),
                source_id.to_string(),
                dependent,
                old_version.to_string(),
                new_version.to_string(),
                reason,
                now_micros,
            ],
        )?;
    }
    Ok(())
}

fn normalize_split_child(child: &mut SplitChildInput) -> Result<()> {
    canonical_enum(&mut child.kind, "work item kind")?;
    if !WORK_KINDS.contains(&child.kind.as_str()) {
        return invalid_enum("work item kind", &child.kind, WORK_KINDS);
    }
    required_text(&mut child.title, "work item title")?;
    child.description = child.description.trim().to_owned();
    canonical_enum(&mut child.risk, "work item risk")?;
    if !RISK_LEVELS.contains(&child.risk.as_str()) {
        return invalid_enum("work item risk", &child.risk, RISK_LEVELS);
    }
    canonical_enum(&mut child.review_policy, "review policy")?;
    if !REVIEW_POLICIES.contains(&child.review_policy.as_str()) {
        return invalid_enum("review policy", &child.review_policy, REVIEW_POLICIES);
    }
    child.dedupe_key = canonical_optional(child.dedupe_key.take(), "work item dedupe key")?;
    normalize_string_set(&mut child.required_capabilities, "required capability")?;
    normalize_string_set(&mut child.acceptance_criteria, "acceptance criterion")?;
    normalize_string_set(&mut child.expected_artifacts, "expected artifact")?;
    normalize_string_set(&mut child.planned_resources, "planned resource")?;
    child.depends_on.sort_by_key(ToString::to_string);
    child.depends_on.dedup();
    Ok(())
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

fn normalize_propose_work_item_request(request: &mut ProposeWorkItemRequest) -> Result<()> {
    canonical_enum(&mut request.kind, "work item kind")?;
    if !WORK_KINDS.contains(&request.kind.as_str()) {
        return invalid_enum("work item kind", &request.kind, WORK_KINDS);
    }
    required_text(&mut request.title, "work item title")?;
    request.description = request.description.trim().to_owned();
    canonical_enum(&mut request.risk, "work item risk")?;
    if !RISK_LEVELS.contains(&request.risk.as_str()) {
        return invalid_enum("work item risk", &request.risk, RISK_LEVELS);
    }
    canonical_enum(&mut request.review_policy, "review policy")?;
    if !REVIEW_POLICIES.contains(&request.review_policy.as_str()) {
        return invalid_enum("review policy", &request.review_policy, REVIEW_POLICIES);
    }
    required_text(&mut request.reason, "plan change reason")?;
    request.dedupe_key = canonical_optional(request.dedupe_key.take(), "work item dedupe key")?;
    normalize_string_set(&mut request.required_capabilities, "required capability")?;
    normalize_string_set(&mut request.acceptance_criteria, "acceptance criterion")?;
    normalize_string_set(&mut request.expected_artifacts, "expected artifact")?;
    normalize_string_set(&mut request.planned_resources, "planned resource")?;
    request.requirement_ids.sort_by_key(ToString::to_string);
    request.requirement_ids.dedup();
    request.depends_on.sort_by_key(ToString::to_string);
    request.depends_on.dedup();
    validate_request_id(&request.request_id)
}

fn require_planner(
    transaction: &Transaction<'_>,
    context: &SessionContext,
    authenticated: &AuthenticatedAgent,
) -> Result<TaskPlanningHead> {
    if authenticated.participant.kind != ParticipantKind::Worker {
        return Err(SessionBridgeError::domain(
            ErrorCode::CapabilityNotGranted,
            "observer participants cannot change the task plan",
        ));
    }
    if !matches!(
        authenticated.participant.state,
        ParticipantState::Active | ParticipantState::Waiting
    ) {
        return Err(participant_state_error(authenticated.participant.state));
    }
    let row: Option<(String, String, i64, i64, i64)> = transaction
        .query_row(
            "SELECT phase, autonomy_policy, plan_version, completion_epoch, completion_revision
             FROM tasks WHERE id = ?1 AND workspace_id = ?2",
            params![
                context.task_id.to_string(),
                context.workspace_id.to_string()
            ],
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
    let Some((phase, autonomy_policy, plan_version, completion_epoch, completion_revision)) = row
    else {
        return Err(SessionBridgeError::domain(
            ErrorCode::NotFound,
            "Task was not found",
        ));
    };
    if phase != TaskPhase::Active.as_str() {
        let code = if phase == TaskPhase::Paused.as_str() {
            ErrorCode::TaskPaused
        } else {
            ErrorCode::TaskNotActive
        };
        return Err(SessionBridgeError::domain(
            code,
            format!("cannot change plan while Task is {phase}"),
        ));
    }
    if completion_epoch != authenticated.participant.completion_epoch {
        return Err(SessionBridgeError::domain(
            ErrorCode::CompletionEpochChanged,
            "participant is not in the current completion epoch",
        ));
    }
    let state: Option<(String, i64)> = transaction
        .query_row(
            "SELECT state, version FROM task_participants WHERE id = ?1 AND task_id = ?2",
            params![
                authenticated.participant.participant_id.to_string(),
                context.task_id.to_string(),
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if state.as_ref().map(|row| row.0.as_str()) != Some(authenticated.participant.state.as_str())
        || state.as_ref().map(|row| row.1) != Some(authenticated.participant.version)
    {
        return Err(SessionBridgeError::domain(
            ErrorCode::ParticipantVersionChanged,
            "participant state changed after authentication",
        ));
    }
    Ok(TaskPlanningHead {
        autonomy_policy,
        plan_version,
        completion_revision,
    })
}

fn apply_mode(policy: &str) -> Result<ApplyMode> {
    match policy {
        "bounded" | "open" => Ok(ApplyMode::Applied),
        "proposal" => Ok(ApplyMode::Proposed),
        "manual" => Err(SessionBridgeError::domain(
            ErrorCode::ProposalReviewRequired,
            "manual autonomy policy does not allow Agent-created plan changes",
        )),
        value => Err(SessionBridgeError::domain(
            ErrorCode::Database,
            format!("unknown autonomy policy in database: {value}"),
        )),
    }
}

#[allow(clippy::too_many_arguments)]
fn finalize_plan_change(
    transaction: &Transaction<'_>,
    context: &SessionContext,
    authenticated: &AuthenticatedAgent,
    head: &TaskPlanningHead,
    mode: ApplyMode,
    reason: &str,
    operation: &serde_json::Value,
    now_micros: i64,
) -> Result<(String, String, i64, i64)> {
    let plan_change_id = UuidText::generate();
    let (status, applied_at) = if mode == ApplyMode::Applied {
        ("applied", Some(now_micros))
    } else {
        ("pending_approval", None)
    };
    transaction.execute(
        "INSERT INTO plan_changes (
            id, task_id, proposer_session_id, base_plan_version, operations_json,
            risk_class, status, reason, version, applied_at, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, 'safe', ?6, ?7, 0, ?8, ?9, ?9)",
        params![
            plan_change_id,
            context.task_id.to_string(),
            authenticated.session.session_id.to_string(),
            head.plan_version,
            serde_json::to_string(operation)?,
            status,
            reason,
            applied_at,
            now_micros,
        ],
    )?;
    let (plan_version, completion_revision) = if mode == ApplyMode::Applied {
        let changed = transaction.execute(
            "UPDATE tasks SET plan_version = plan_version + 1,
                    completion_revision = completion_revision + 1,
                    updated_at = ?4
             WHERE id = ?1 AND plan_version = ?2 AND completion_revision = ?3",
            params![
                context.task_id.to_string(),
                head.plan_version,
                head.completion_revision,
                now_micros,
            ],
        )?;
        if changed != 1 {
            return Err(SessionBridgeError::domain(
                ErrorCode::PlanChanged,
                "task plan changed concurrently",
            ));
        }
        (head.plan_version + 1, head.completion_revision + 1)
    } else {
        let revision = increment_completion_revision(
            transaction,
            context,
            head.completion_revision,
            now_micros,
        )?;
        (head.plan_version, revision)
    };
    Ok((
        plan_change_id,
        status.into(),
        plan_version,
        completion_revision,
    ))
}

fn increment_completion_revision(
    transaction: &Transaction<'_>,
    context: &SessionContext,
    expected: i64,
    now_micros: i64,
) -> Result<i64> {
    let changed = transaction.execute(
        "UPDATE tasks SET completion_revision = completion_revision + 1, updated_at = ?3
         WHERE id = ?1 AND completion_revision = ?2",
        params![context.task_id.to_string(), expected, now_micros],
    )?;
    if changed != 1 {
        return Err(SessionBridgeError::domain(
            ErrorCode::ConclusionStale,
            "task completion revision changed concurrently",
        ));
    }
    Ok(expected + 1)
}

fn activate_waiting_participant(
    transaction: &Transaction<'_>,
    authenticated: &AuthenticatedAgent,
    now_micros: i64,
) -> Result<()> {
    if authenticated.participant.state == ParticipantState::Waiting {
        let changed = transaction.execute(
            "UPDATE task_participants
             SET state = 'active', version = version + 1, updated_at = ?3
             WHERE id = ?1 AND version = ?2 AND state = 'waiting'",
            params![
                authenticated.participant.participant_id.to_string(),
                authenticated.participant.version,
                now_micros,
            ],
        )?;
        if changed != 1 {
            return Err(SessionBridgeError::domain(
                ErrorCode::ParticipantVersionChanged,
                "waiting participant changed concurrently",
            ));
        }
    }
    Ok(())
}

fn validate_criteria(
    transaction: &Transaction<'_>,
    context: &SessionContext,
    ids: &[CriterionId],
) -> Result<()> {
    for id in ids {
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM task_criteria WHERE id = ?1 AND task_id = ?2)",
            params![id.to_string(), context.task_id.to_string()],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(SessionBridgeError::domain(
                ErrorCode::NotFound,
                format!("TaskCriterion {id} was not found in this Task"),
            ));
        }
    }
    Ok(())
}

fn validate_requirements(
    transaction: &Transaction<'_>,
    context: &SessionContext,
    ids: &[RequirementId],
) -> Result<()> {
    for id in ids {
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM requirements
                WHERE id = ?1 AND task_id = ?2 AND status IN ('active', 'proposed')
             )",
            params![id.to_string(), context.task_id.to_string()],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(SessionBridgeError::domain(
                ErrorCode::NotFound,
                format!("Requirement {id} was not found or is not active"),
            ));
        }
    }
    Ok(())
}

fn validate_work_items(
    transaction: &Transaction<'_>,
    context: &SessionContext,
    parent: Option<WorkItemId>,
    dependencies: &[WorkItemId],
) -> Result<()> {
    let mut ids = dependencies.to_vec();
    if let Some(parent) = parent {
        ids.push(parent);
    }
    ids.sort_by_key(ToString::to_string);
    ids.dedup();
    for id in ids {
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM work_items
                WHERE id = ?1 AND task_id = ?2 AND plan_state IN ('active', 'proposed')
             )",
            params![id.to_string(), context.task_id.to_string()],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(SessionBridgeError::domain(
                ErrorCode::NotFound,
                format!("WorkItem {id} was not found or is not active"),
            ));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn append_entity_event(
    transaction: &Transaction<'_>,
    context: &SessionContext,
    authenticated: &AuthenticatedAgent,
    event_type: &str,
    entity_type: &str,
    entity_id: &str,
    payload: &serde_json::Value,
    now: time::OffsetDateTime,
) -> Result<i64> {
    events::append(
        transaction,
        context.workspace_id,
        Some(&context.task_id.to_string()),
        event_type,
        EventActor {
            kind: "session",
            id: Some(&authenticated.session.session_id.to_string()),
        },
        EventEntity {
            kind: entity_type,
            id: Some(entity_id),
        },
        payload,
        now,
    )
}

fn require_plan_version(head: &TaskPlanningHead, expected: i64) -> Result<()> {
    if expected < 0 {
        return Err(SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            "base plan version cannot be negative",
        ));
    }
    if expected != head.plan_version {
        return Err(SessionBridgeError::domain(
            ErrorCode::PlanChanged,
            format!(
                "base plan version {expected} does not match current version {}",
                head.plan_version
            ),
        )
        .with_details(serde_json::json!({
            "expected_plan_version": expected,
            "actual_plan_version": head.plan_version,
        })));
    }
    Ok(())
}

fn participant_state_error(state: ParticipantState) -> SessionBridgeError {
    let code = match state {
        ParticipantState::Blocked => ErrorCode::ParticipantBlocked,
        ParticipantState::Departed => ErrorCode::ParticipantDeparted,
        ParticipantState::Retired => ErrorCode::ParticipantRetired,
        ParticipantState::Concluded => ErrorCode::SessionAlreadyConcluded,
        _ => ErrorCode::ParticipantVersionChanged,
    };
    SessionBridgeError::domain(code, format!("participant cannot plan while {state}"))
}

fn canonical_enum(value: &mut String, name: &str) -> Result<()> {
    *value = value.trim().to_lowercase();
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            format!("{name} cannot be empty or contain control characters"),
        ));
    }
    Ok(())
}

fn required_text(value: &mut String, name: &str) -> Result<()> {
    *value = value.trim().to_owned();
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            format!("{name} cannot be empty or contain control characters"),
        ));
    }
    Ok(())
}

fn canonical_optional(value: Option<String>, name: &str) -> Result<Option<String>> {
    value
        .map(|mut value| {
            canonical_enum(&mut value, name)?;
            Ok(value)
        })
        .transpose()
}

fn normalize_string_set(values: &mut Vec<String>, name: &str) -> Result<()> {
    let mut normalized = BTreeSet::new();
    for value in values.drain(..) {
        let value = value.trim();
        if value.is_empty() || value.chars().any(char::is_control) {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                format!("{name} cannot be empty or contain control characters"),
            ));
        }
        normalized.insert(value.to_owned());
    }
    values.extend(normalized);
    Ok(())
}

fn invalid_enum<T>(name: &str, value: &str, allowed: &[&str]) -> Result<T> {
    Err(SessionBridgeError::domain(
        ErrorCode::InvalidInput,
        format!(
            "unsupported {name} '{value}'; expected one of {}",
            allowed.join(", ")
        ),
    ))
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

fn bool_i64(value: bool) -> i64 {
    if value { 1 } else { 0 }
}

fn default_true() -> bool {
    true
}

fn default_risk() -> String {
    "medium".into()
}

fn default_review_policy() -> String {
    "risk_based_review".into()
}

struct UuidText;

impl UuidText {
    fn generate() -> String {
        uuid::Uuid::now_v7().to_string()
    }
}
