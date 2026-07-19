use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    str::FromStr,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    Result,
    app::{
        chat::{ChatInboxView, ChatService},
        collaboration_projection::load_participant_liveness,
        completion_service::CompletionTrigger,
        deliverable::{DeliverableService, DeliverableView},
        runtime_reconciler::reconcile_agent_runtime,
        task::{TaskDetails, TaskHealth, TaskService, WorkspaceBinding},
        work::resources_overlap,
    },
    clock::{Clock, unix_micros},
    domain::{
        InvitationId, ParticipantId, ParticipantKind, ParticipantState, SessionContext, SessionId,
        TaskId, TaskPhase,
    },
    error::{ErrorCode, SessionBridgeError},
    storage::SqliteStore,
    workspace::PROTOCOL_VERSION,
};

const MAX_SYNC_EVENTS: i64 = 1_000;
const PROPOSAL_SOFT_BUDGET: i64 = 50;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSessionView {
    pub session_id: SessionId,
    #[serde(skip)]
    pub(crate) idempotency_actor_id: String,
    pub task_id: TaskId,
    pub executor_identity: String,
    pub executor_identity_source: String,
    pub label: String,
    pub client_name: Option<String>,
    pub model: Option<String>,
    pub declared_capabilities: Vec<String>,
    pub granted_capabilities: Vec<String>,
    pub worktree_path: Option<PathBuf>,
    pub branch: Option<String>,
    pub base_commit: Option<String>,
    pub head_commit: Option<String>,
    pub target_branch: Option<String>,
    pub protocol_version: i64,
    pub last_seen_at: String,
    pub ended_at: Option<String>,
    pub end_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentParticipantView {
    pub participant_id: ParticipantId,
    pub invitation_id: Option<InvitationId>,
    pub kind: ParticipantKind,
    pub completion_epoch: i64,
    pub state: ParticipantState,
    pub version: i64,
    pub current_conclusion_id: Option<String>,
    pub joined_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentWhoami {
    pub session: AgentSessionView,
    pub participant: AgentParticipantView,
    pub current_worktree_matches_context: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateWorkItem {
    pub id: String,
    pub parent_id: Option<String>,
    pub kind: String,
    pub title: String,
    pub description: String,
    pub required: bool,
    pub priority: i64,
    pub risk: String,
    pub required_capabilities: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub planned_resources: Vec<String>,
    pub version: i64,
    pub capability_match: bool,
    pub reason_codes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GapView {
    pub id: String,
    pub kind: String,
    pub severity: String,
    pub required: bool,
    pub status: String,
    pub title: String,
    pub description: String,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParticipantSummary {
    pub participant_id: ParticipantId,
    pub session_id: SessionId,
    pub label: String,
    pub kind: ParticipantKind,
    pub state: ParticipantState,
    pub completion_epoch: i64,
    pub stale: bool,
    pub last_seen_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvitationSeat {
    pub invitation_id: InvitationId,
    pub state: String,
    pub participant_kind: ParticipantKind,
    pub blocks_completion: bool,
    pub completion_epoch: i64,
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxRequest {
    pub id: String,
    pub kind: String,
    pub target_type: String,
    pub target_id: Option<String>,
    pub requested_by_session_id: SessionId,
    pub blocking: bool,
    pub status: String,
    pub payload: serde_json::Value,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncEvent {
    pub sequence: i64,
    pub event_type: String,
    pub actor_type: String,
    pub actor_id: Option<String>,
    pub entity_type: String,
    pub entity_id: Option<String>,
    pub payload: serde_json::Value,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSync {
    pub snapshot_id: String,
    pub base_cursor: i64,
    pub whoami: AgentWhoami,
    pub task: TaskDetails,
    pub health: TaskHealth,
    pub candidates: Vec<CandidateWorkItem>,
    pub deliverables: Vec<DeliverableView>,
    pub open_gaps: Vec<GapView>,
    pub participants: Vec<ParticipantSummary>,
    pub invitation_seats: Vec<InvitationSeat>,
    pub inbox: Vec<InboxRequest>,
    pub active_wait: Option<ActiveAwaitSummary>,
    pub owned_claims: Vec<OwnedClaimSummary>,
    pub events: Vec<SyncEvent>,
    pub previous_cursor: Option<i64>,
    pub next_cursor: i64,
    pub has_more_events: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwnedClaimSummary {
    pub claim_id: String,
    pub work_item_id: String,
    pub mode: String,
    pub generation: i64,
    pub lease_expires_at_micros: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SnapshotTokenBasis {
    session_id: SessionId,
    task_id: TaskId,
    protocol_version: i64,
    base_cursor: i64,
    next_cursor: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveAwaitSummary {
    pub wait_id: String,
    pub mode: String,
    pub created_at_micros: i64,
    pub targets: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwaitableDependency {
    pub work_item_id: String,
    pub title: String,
    pub blocking_work_item_ids: Vec<String>,
    pub owner_session_id: String,
    pub owner_label: String,
    pub ownership_source: String,
    pub await_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopedStateTokens {
    pub task_plan: String,
    pub completion: String,
    pub participant: String,
    pub work_items: BTreeMap<String, String>,
    pub claims: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedAction {
    pub action: String,
    pub priority: i64,
    pub reason: String,
    pub advisory: bool,
    pub conflict_free: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
    #[serde(default)]
    pub required_capabilities: Vec<String>,
    pub expand_ref: String,
    #[serde(default)]
    pub source_entities: Vec<String>,
    pub computed_at_cursor: i64,
    pub confidence: String,
    #[serde(default)]
    pub assumptions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForbiddenAction {
    pub action: String,
    pub reason: String,
    pub invariant: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockingFingerprintView {
    pub blocker_code: String,
    pub entity_id: Option<String>,
    pub dependency_version: i64,
    pub fingerprint: String,
    pub first_observed_cursor: i64,
    pub last_observed_cursor: i64,
    pub unchanged_since_micros: i64,
    pub category: String,
    pub stable_for_millis: i64,
    pub suggested_action: String,
    pub immediately_blockable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentNext {
    pub snapshot_id: String,
    pub base_cursor: i64,
    pub next_cursor: i64,
    pub planner_complete: bool,
    pub tokens: ScopedStateTokens,
    pub task_phase: String,
    pub participant_state: String,
    pub should_exit: bool,
    pub obligations: Vec<PlannedAction>,
    pub eligible_actions: Vec<PlannedAction>,
    pub forbidden_actions: Vec<ForbiddenAction>,
    pub completion_eligible: bool,
    pub completion_blockers: Vec<serde_json::Value>,
    pub blocking_fingerprint: Option<BlockingFingerprintView>,
    pub active_wait: Option<ActiveAwaitSummary>,
    pub awaitable_dependencies: Vec<AwaitableDependency>,
    pub changes: Vec<SyncEvent>,
    pub full_sync_ref: String,
    pub propose_action_ref: String,
    pub operation_contracts_ref: String,
    pub capabilities: AgentCapabilitySummary,
    pub tool_catalog: Vec<ToolCatalogEntry>,
    pub chat: ChatInboxView,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCapabilitySummary {
    pub current: Vec<String>,
    pub missing_for_known_work: Vec<String>,
    pub request_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCatalogEntry {
    pub command: String,
    pub purpose: String,
    pub advisory: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposeActionInput {
    pub summary: String,
    #[serde(default)]
    pub rationale: String,
    #[serde(default)]
    pub payload: serde_json::Value,
    #[serde(default)]
    pub dedupe_key: Option<String>,
    #[serde(default = "default_proposal_priority")]
    pub priority: String,
    #[serde(default)]
    pub safety_relevant: bool,
    #[serde(default)]
    pub source_entities: Vec<String>,
    #[serde(default)]
    pub provenance: serde_json::Value,
}

fn default_proposal_priority() -> String {
    "normal".into()
}

fn agent_tool_catalog() -> Vec<ToolCatalogEntry> {
    [
        ("agent sync --full", "expand complete shared state"),
        ("agent inspect <ENTITY>", "expand one referenced entity"),
        (
            "agent propose-action",
            "propose an unmodelled autonomous action",
        ),
        (
            "agent completion-challenge",
            "challenge structured completion assumptions",
        ),
        (
            "agent request-capability",
            "request a capability or external tool",
        ),
        ("work speculate start", "record isolated speculative work"),
    ]
    .into_iter()
    .map(|(command, purpose)| ToolCatalogEntry {
        command: format!("sessionbridge {command}"),
        purpose: purpose.into(),
        advisory: true,
    })
    .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionProposalResult {
    pub proposal_id: String,
    pub kind: String,
    pub status: String,
    pub event_sequence: i64,
    pub automatically_executed: bool,
    pub similar_proposal_ids: Vec<String>,
    pub soft_budget_exceeded: bool,
    pub consolidation_request_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposalResolutionResult {
    pub proposal_id: String,
    pub kind: String,
    pub status: String,
    pub reason: String,
    pub event_sequence: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidationResult {
    pub request_id: String,
    pub status: String,
    pub claimed_by_session_id: Option<String>,
    pub proposal_count: i64,
    pub event_sequence: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidationResolution {
    #[serde(default)]
    pub duplicate_groups: Vec<Vec<String>>,
    #[serde(default)]
    pub preserved_alternatives: Vec<String>,
    #[serde(default)]
    pub disagreements: Vec<String>,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionTokenBasis {
    pub task_id: TaskId,
    pub plan_version: i64,
    pub completion_epoch: i64,
    pub completion_revision: i64,
    pub event_cursor: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct AuthenticatedAgent {
    pub session: AgentSessionView,
    pub participant: AgentParticipantView,
}

pub struct AgentService<C> {
    clock: C,
}

impl<C: Clock + Clone> AgentService<C> {
    pub fn new(clock: C) -> Self {
        Self { clock }
    }

    pub fn whoami(&self, store: &mut SqliteStore, context: &SessionContext) -> Result<AgentWhoami> {
        let authenticated = self.authenticate_and_touch(store, context)?;
        Ok(whoami_view(authenticated))
    }

    pub fn sync(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        since: Option<i64>,
    ) -> Result<AgentSync> {
        reconcile_agent_runtime(
            store,
            context,
            self.clock.clone(),
            CompletionTrigger {
                actor_type: "system",
                actor_id: None,
            },
        )?;
        self.sync_internal(store, context, since, true)
    }

    fn sync_internal(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        since: Option<i64>,
        touch: bool,
    ) -> Result<AgentSync> {
        if since.is_some_and(|cursor| cursor < 0) {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "event cursor cannot be negative",
            ));
        }
        let authenticated = self.authenticate(store, context, touch)?;
        let binding = WorkspaceBinding {
            id: context.workspace_id,
            repository_identity: &context.repository_instance_id,
        };
        let task_service = TaskService::new(self.clock.clone());
        let task = task_service.show(store.connection(), binding, context.task_id)?;
        let health = task_service.health(store.connection(), binding, context.task_id)?;
        let capabilities = effective_capabilities(&authenticated.session);
        let candidates = load_candidates(store.connection(), context.task_id, &capabilities)?;
        let deliverables = DeliverableService::new(self.clock.clone()).list(
            store.connection(),
            binding,
            context.task_id,
        )?;
        let open_gaps = load_gaps(store.connection(), context.task_id)?;
        let participants = load_participants(
            store.connection(),
            context.task_id,
            task.task.completion_epoch,
            self.clock.now(),
        )?;
        let invitation_seats = load_invitations(
            store.connection(),
            context.task_id,
            task.task.completion_epoch,
        )?;
        let inbox = load_inbox(
            store.connection(),
            context.task_id,
            context.session_id,
            &capabilities,
        )?;
        let active_wait =
            load_active_wait(store.connection(), context.task_id, context.session_id)?;
        let owned_claims = load_owned_claim_summaries(store.connection(), context)?;
        let (events, next_cursor, has_more_events) =
            load_events(store.connection(), context.task_id, since)?;
        let base_cursor = since.unwrap_or(0);
        let snapshot_id = signed_token(
            context,
            "snapshot",
            &serde_json::to_vec(&serde_json::json!({
                "task": context.task_id,
                "task_id": context.task_id,
                "session_id": context.session_id,
                "protocol_version": authenticated.session.protocol_version,
                "plan": task.task.plan_version,
                "completion": task.task.completion_revision,
                "participant": authenticated.participant.version,
                "base_cursor": base_cursor,
                "next_cursor": next_cursor,
            }))?,
        );

        Ok(AgentSync {
            snapshot_id,
            base_cursor,
            whoami: whoami_view(authenticated),
            task,
            health,
            candidates,
            deliverables,
            open_gaps,
            participants,
            invitation_seats,
            inbox,
            active_wait,
            owned_claims,
            events,
            previous_cursor: since,
            next_cursor,
            has_more_events,
        })
    }

    pub fn next(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        limit: usize,
    ) -> Result<AgentNext> {
        self.next_internal(store, context, limit, true)
    }

    pub fn next_preview(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        limit: usize,
    ) -> Result<AgentNext> {
        self.next_internal(store, context, limit, false)
    }

    fn next_internal(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        limit: usize,
        touch: bool,
    ) -> Result<AgentNext> {
        if limit == 0 || limit > 100 {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "agent next limit must be between 1 and 100",
            ));
        }
        reconcile_agent_runtime(
            store,
            context,
            self.clock.clone(),
            CompletionTrigger {
                actor_type: "system",
                actor_id: None,
            },
        )?;
        let sync = self.sync_internal(store, context, None, touch)?;
        let connection = store.connection();
        let terminal = matches!(
            sync.task.task.phase,
            TaskPhase::Completed | TaskPhase::Cancelled | TaskPhase::Archived
        ) || matches!(
            sync.whoami.participant.state,
            ParticipantState::Concluded | ParticipantState::Retired | ParticipantState::Superseded
        );
        let mut obligations = if terminal {
            Vec::new()
        } else {
            load_owned_obligations(connection, context, self.clock.now())?
        };
        if !terminal {
            obligations.extend(load_owned_consolidations(connection, context)?);
        }
        let mut eligible_actions = Vec::new();

        for request in sync.inbox.iter().filter(|_| !terminal) {
            obligations.push(PlannedAction {
                action: "respond_coordination".into(),
                priority: if request.blocking { 100 } else { 70 },
                reason: if request.blocking {
                    "directed blocking coordination request requires a response"
                } else {
                    "directed coordination request is awaiting a response"
                }
                .into(),
                advisory: true,
                conflict_free: true,
                entity_type: Some("coordination_request".into()),
                entity_id: Some(request.id.clone()),
                required_capabilities: Vec::new(),
                expand_ref: format!("sessionbridge agent inspect {}", request.id),
                source_entities: vec![request.id.clone()],
                computed_at_cursor: sync.next_cursor,
                confidence: "high".into(),
                assumptions: Vec::new(),
            });
        }

        for candidate in sync
            .candidates
            .iter()
            .filter(|_| !terminal)
            .filter(|item| item.capability_match)
        {
            let recommended_session = soft_recommended_session(&candidate.id, &sync.participants);
            let recommended_here = recommended_session == Some(context.session_id);
            let mut reasons = candidate.reason_codes.clone();
            if recommended_here {
                reasons.push("soft_recommended_for_this_session".into());
            } else if let Some(session_id) = recommended_session {
                reasons.push(format!("soft_recommended_session:{session_id}"));
            }
            eligible_actions.push(PlannedAction {
                action: "start_work".into(),
                priority: candidate.priority
                    + if candidate.required { 20 } else { 0 }
                    + if recommended_here { 10 } else { 0 },
                reason: reasons.join(","),
                advisory: true,
                conflict_free: true,
                entity_type: Some("work_item".into()),
                entity_id: Some(candidate.id.clone()),
                required_capabilities: candidate.required_capabilities.clone(),
                expand_ref: format!("sessionbridge item inspect {}", candidate.id),
                source_entities: vec![candidate.id.clone()],
                computed_at_cursor: sync.next_cursor,
                confidence: "high".into(),
                assumptions: vec![
                    "soft recommendation is advisory and never reserves or locks the WorkItem"
                        .into(),
                ],
            });
        }
        if !terminal {
            eligible_actions.extend(load_stage_actions(connection, context)?);
            eligible_actions.extend(load_consolidation_actions(connection, context)?);
            eligible_actions.extend(load_speculative_actions(connection, context)?);
            eligible_actions.extend(load_open_proposal_actions(connection, context)?);
            eligible_actions.extend(load_runtime_health_actions(
                connection,
                context,
                unix_micros(self.clock.now()),
            )?);
            eligible_actions.extend(deliverable_actions(&sync.deliverables));
        }
        let awaitable_dependencies = if terminal {
            Vec::new()
        } else {
            load_awaitable_dependencies(connection, context, unix_micros(self.clock.now()))?
        };
        if obligations.is_empty()
            && eligible_actions.is_empty()
            && sync.active_wait.is_none()
            && !awaitable_dependencies.is_empty()
        {
            eligible_actions.push(PlannedAction {
                action: "await_dependencies".into(),
                priority: 50,
                reason: "all currently known blocked work has a live external ownership chain"
                    .into(),
                advisory: true,
                conflict_free: true,
                entity_type: Some("work_item".into()),
                entity_id: awaitable_dependencies
                    .first()
                    .map(|dependency| dependency.work_item_id.clone()),
                required_capabilities: Vec::new(),
                expand_ref: awaitable_dependencies
                    .first()
                    .map(|dependency| dependency.await_ref.clone())
                    .unwrap_or_default(),
                source_entities: awaitable_dependencies
                    .iter()
                    .map(|dependency| dependency.work_item_id.clone())
                    .collect(),
                computed_at_cursor: sync.next_cursor,
                confidence: "high".into(),
                assumptions: vec![
                    "await is optional; Agent may still propose independent or speculative work"
                        .into(),
                ],
            });
        }
        eligible_actions.sort_by(|left, right| {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| left.action.cmp(&right.action))
                .then_with(|| left.entity_id.cmp(&right.entity_id))
        });
        eligible_actions.truncate(limit);

        let completion_blockers = completion_blockers(&sync.health);
        let fingerprint_blockers = if obligations.is_empty()
            && eligible_actions.is_empty()
            && sync.active_wait.is_none()
        {
            completion_blockers.as_slice()
        } else {
            &[]
        };
        let blocking_fingerprint = persist_blocking_fingerprint(
            connection,
            context,
            &sync.whoami.participant.participant_id.to_string(),
            sync.task.task.completion_revision,
            sync.next_cursor,
            fingerprint_blockers,
            self.clock.now(),
        )?;
        if let Some(fingerprint) = &blocking_fingerprint {
            eligible_actions.push(PlannedAction {
                action: fingerprint.suggested_action.clone(),
                priority: if fingerprint.immediately_blockable {
                    92
                } else {
                    55
                },
                reason: format!(
                    "{} blocker fingerprint unchanged for {} ms",
                    fingerprint.category, fingerprint.stable_for_millis
                ),
                advisory: true,
                conflict_free: true,
                entity_type: Some("blocking_fingerprint".into()),
                entity_id: Some(fingerprint.fingerprint.clone()),
                required_capabilities: Vec::new(),
                expand_ref: "sessionbridge agent sync --full --format json".into(),
                source_entities: vec![fingerprint.fingerprint.clone()],
                computed_at_cursor: sync.next_cursor,
                confidence: "high".into(),
                assumptions: Vec::new(),
            });
        }
        let completion_eligible = !terminal
            && completion_blockers.is_empty()
            && obligations.is_empty()
            && eligible_actions.is_empty()
            && sync.active_wait.is_none();
        if completion_eligible && !terminal {
            eligible_actions.push(PlannedAction {
                action: "assess_completion".into(),
                priority: 10,
                reason: "mechanical completion conditions currently have no blockers".into(),
                advisory: true,
                conflict_free: true,
                entity_type: Some("task".into()),
                entity_id: Some(context.task_id.to_string()),
                required_capabilities: Vec::new(),
                expand_ref: "sessionbridge agent sync --full".into(),
                source_entities: vec![context.task_id.to_string()],
                computed_at_cursor: sync.next_cursor,
                confidence: "medium".into(),
                assumptions: vec!["semantic completion still requires Agent assessment".into()],
            });
        }

        for action in obligations.iter_mut().chain(eligible_actions.iter_mut()) {
            action.computed_at_cursor = sync.next_cursor;
            if action.source_entities.is_empty()
                && let Some(entity_id) = &action.entity_id
            {
                action.source_entities.push(entity_id.clone());
            }
        }

        let mut forbidden_actions = Vec::new();
        if !obligations.is_empty() || !eligible_actions.is_empty() {
            forbidden_actions.push(ForbiddenAction {
                action: "conclude_no_further_work".into(),
                reason: "known coordination or executable actions remain".into(),
                invariant: "completion_contract".into(),
            });
        }
        if sync.active_wait.is_some() {
            forbidden_actions.push(ForbiddenAction {
                action: "start_second_await".into(),
                reason: "participant already owns an active durable await".into(),
                invariant: "one_active_wait_per_participant".into(),
            });
        }
        if terminal {
            forbidden_actions.push(ForbiddenAction {
                action: "start_or_modify_work".into(),
                reason: "Task or Participant is terminal".into(),
                invariant: "terminal_state_is_immutable".into(),
            });
        }

        let tokens = scoped_tokens(context, &sync)?;
        let chat = ChatService::new(self.clock.clone()).inbox(connection, context, 20)?;
        let current_capabilities = effective_capabilities(&sync.whoami.session);
        let missing_for_known_work = sync
            .candidates
            .iter()
            .flat_map(|candidate| candidate.required_capabilities.iter())
            .filter(|capability| !current_capabilities.contains(capability.as_str()))
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let snapshot_payload = serde_json::to_vec(&serde_json::json!({
            "task": sync.task.task.id,
            "plan": sync.task.task.plan_version,
            "completion": sync.task.task.completion_revision,
            "participant": sync.whoami.participant.version,
            "cursor": sync.next_cursor,
        }))?;
        let snapshot_id = signed_token(context, "snapshot", &snapshot_payload);
        let mut changes = sync.events;
        if changes.len() > 20 {
            changes = changes.split_off(changes.len() - 20);
        }
        let base_cursor = changes
            .first()
            .map(|event| event.sequence.saturating_sub(1))
            .unwrap_or(sync.next_cursor);
        Ok(AgentNext {
            snapshot_id,
            base_cursor,
            next_cursor: sync.next_cursor,
            planner_complete: false,
            tokens,
            task_phase: enum_text(&sync.task.task.phase)?,
            participant_state: enum_text(&sync.whoami.participant.state)?,
            should_exit: terminal,
            obligations,
            eligible_actions,
            forbidden_actions,
            completion_eligible,
            completion_blockers,
            blocking_fingerprint,
            active_wait: sync.active_wait,
            awaitable_dependencies,
            changes,
            full_sync_ref: "sessionbridge agent sync --full --format json".into(),
            propose_action_ref: "sessionbridge agent propose-action --kind <KIND> --input <FILE>"
                .into(),
            operation_contracts_ref: "sessionbridge agent operations [OPERATION] --format json"
                .into(),
            capabilities: AgentCapabilitySummary {
                current: current_capabilities.into_iter().collect(),
                missing_for_known_work,
                request_ref: "sessionbridge agent request-capability --input <FILE>".into(),
            },
            tool_catalog: agent_tool_catalog(),
            chat,
        })
    }

    pub fn propose_action(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        kind: String,
        mut input: ProposeActionInput,
        request_id: String,
    ) -> Result<ActionProposalResult> {
        let kind = kind.trim().to_owned();
        input.summary = input.summary.trim().to_owned();
        input.rationale = input.rationale.trim().to_owned();
        input.dedupe_key = input
            .dedupe_key
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        if kind.is_empty() || kind.len() > 128 || input.summary.is_empty() {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "action proposal kind and summary are required",
            ));
        }
        if !input.payload.is_object() {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "action proposal payload must be a JSON object",
            ));
        }
        if !matches!(
            input.priority.as_str(),
            "low" | "normal" | "high" | "critical"
        ) {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "proposal priority must be low, normal, high, or critical",
            ));
        }
        if !input.provenance.is_object() {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "proposal provenance must be a JSON object",
            ));
        }
        let authenticated = self.authenticate_and_touch(store, context)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let observed_cursor =
            crate::app::events::latest_cursor(&transaction, Some(&context.task_id.to_string()))?;
        let normalized_summary = input
            .summary
            .to_ascii_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let semantic_fingerprint = format!(
            "sha256:{:x}",
            Sha256::digest(format!("{kind}\0{normalized_summary}").as_bytes())
        );
        let similar_proposal_ids = transaction.prepare(
            "SELECT id FROM agent_action_proposals WHERE task_id=?1 AND semantic_fingerprint=?2 AND status='proposed' ORDER BY created_at LIMIT 20"
        )?.query_map(params![context.task_id.to_string(),semantic_fingerprint],|row|row.get::<_,String>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
        let proposal_id = uuid::Uuid::now_v7().to_string();
        let inserted = transaction.execute(
            "INSERT OR IGNORE INTO agent_action_proposals (
                id, task_id, session_id, kind, summary, rationale, payload_json,
                status, dedupe_key, priority, safety_relevant, semantic_fingerprint,
                provenance_json, source_entities_json, observed_cursor, created_at, updated_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,'proposed',?8,?9,?10,?11,?12,?13,?14,?15,?15)",
            params![
                proposal_id,
                context.task_id.to_string(),
                context.session_id.to_string(),
                kind,
                input.summary,
                input.rationale,
                serde_json::to_string(&input.payload)?,
                input.dedupe_key,
                input.priority,
                input.safety_relevant,
                semantic_fingerprint,
                serde_json::to_string(&input.provenance)?,
                serde_json::to_string(&input.source_entities)?,
                observed_cursor,
                unix_micros(now),
            ],
        )?;
        let actual_id = if inserted == 1 {
            proposal_id
        } else {
            transaction.query_row(
                "SELECT id FROM agent_action_proposals
                 WHERE task_id=?1 AND session_id=?2 AND dedupe_key=?3",
                params![
                    context.task_id.to_string(),
                    context.session_id.to_string(),
                    input.dedupe_key,
                ],
                |row| row.get(0),
            )?
        };
        let open_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM agent_action_proposals WHERE task_id=?1 AND status='proposed'",
            [context.task_id.to_string()],
            |row| row.get(0),
        )?;
        let soft_budget_exceeded = open_count > PROPOSAL_SOFT_BUDGET;
        let consolidation_request_id = if soft_budget_exceeded {
            let existing: Option<String> = transaction.query_row(
                "SELECT id FROM proposal_consolidation_requests WHERE task_id=?1 AND status IN ('open','claimed')",
                [context.task_id.to_string()],|row|row.get(0)).optional()?;
            if let Some(id) = existing {
                Some(id)
            } else {
                let id = uuid::Uuid::now_v7().to_string();
                transaction.execute("INSERT INTO proposal_consolidation_requests(id,task_id,status,proposal_count,threshold,reason,created_at,updated_at) VALUES(?1,?2,'open',?3,?4,'proposal soft budget exceeded; preserve proposals and coordinate consolidation',?5,?5)",params![id,context.task_id.to_string(),open_count,PROPOSAL_SOFT_BUDGET,unix_micros(now)])?;
                Some(id)
            }
        } else {
            None
        };
        let event_sequence = crate::app::events::append(
            &transaction,
            context.workspace_id,
            Some(&context.task_id.to_string()),
            if inserted == 1 {
                "agent.action_proposed"
            } else {
                "agent.action_proposal_deduplicated"
            },
            crate::app::events::EventActor {
                kind: "session",
                id: Some(&authenticated.session.session_id.to_string()),
            },
            crate::app::events::EventEntity {
                kind: "agent_action_proposal",
                id: Some(&actual_id),
            },
            &serde_json::json!({
                "kind": kind,
                "summary": input.summary,
                "request_id": request_id,
                "automatically_executed": false,
                "similar_proposal_ids": similar_proposal_ids,
                "soft_budget_exceeded": soft_budget_exceeded,
                "consolidation_request_id": consolidation_request_id,
            }),
            now,
        )?;
        transaction.commit()?;
        Ok(ActionProposalResult {
            proposal_id: actual_id,
            kind,
            status: "proposed".into(),
            event_sequence,
            automatically_executed: false,
            similar_proposal_ids,
            soft_budget_exceeded,
            consolidation_request_id,
        })
    }

    pub fn resolve_action_proposal(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        proposal_id: &str,
        decision: &str,
        reason: String,
    ) -> Result<ProposalResolutionResult> {
        if !matches!(decision, "accepted" | "rejected" | "superseded") {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "proposal resolution must be accepted, rejected, or superseded",
            ));
        }
        let reason = reason.trim().to_owned();
        if reason.is_empty() {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "proposal resolution reason is required",
            ));
        }
        let authenticated = self.authenticate_and_touch(store, context)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let kind: String = transaction
            .query_row(
                "SELECT kind FROM agent_action_proposals
                 WHERE id=?1 AND task_id=?2 AND status='proposed'",
                params![proposal_id, context.task_id.to_string()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                SessionBridgeError::domain(
                    ErrorCode::ItemNotReady,
                    "action proposal is not open for resolution",
                )
            })?;
        transaction.execute(
            "UPDATE agent_action_proposals
             SET status=?3,version=version+1,updated_at=?4
             WHERE id=?1 AND task_id=?2 AND status='proposed'",
            params![
                proposal_id,
                context.task_id.to_string(),
                decision,
                unix_micros(now)
            ],
        )?;
        let event_sequence = crate::app::events::append(
            &transaction,
            context.workspace_id,
            Some(&context.task_id.to_string()),
            "agent.action_proposal_resolved",
            crate::app::events::EventActor {
                kind: "session",
                id: Some(&authenticated.session.session_id.to_string()),
            },
            crate::app::events::EventEntity {
                kind: "agent_action_proposal",
                id: Some(proposal_id),
            },
            &serde_json::json!({
                "kind": kind,
                "decision": decision,
                "reason": reason,
            }),
            now,
        )?;
        transaction.commit()?;
        Ok(ProposalResolutionResult {
            proposal_id: proposal_id.into(),
            kind,
            status: decision.into(),
            reason,
            event_sequence,
        })
    }

    pub fn claim_consolidation(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        id: &str,
    ) -> Result<ConsolidationResult> {
        let authenticated = self.authenticate_and_touch(store, context)?;
        let now = self.clock.now();
        let tx = store.begin_immediate()?;
        let changed=tx.execute("UPDATE proposal_consolidation_requests SET status='claimed',claimed_by_session_id=?3,updated_at=?4 WHERE id=?1 AND task_id=?2 AND status='open'",params![id,context.task_id.to_string(),context.session_id.to_string(),unix_micros(now)])?;
        if changed != 1 {
            return Err(SessionBridgeError::domain(
                ErrorCode::ItemNotReady,
                "consolidation request is not open",
            ));
        }
        let count: i64 = tx.query_row(
            "SELECT proposal_count FROM proposal_consolidation_requests WHERE id=?1",
            [id],
            |row| row.get(0),
        )?;
        let event_sequence = crate::app::events::append(
            &tx,
            context.workspace_id,
            Some(&context.task_id.to_string()),
            "proposal.consolidation_claimed",
            crate::app::events::EventActor {
                kind: "session",
                id: Some(&authenticated.session.session_id.to_string()),
            },
            crate::app::events::EventEntity {
                kind: "proposal_consolidation_request",
                id: Some(id),
            },
            &serde_json::json!({"proposal_count":count}),
            now,
        )?;
        tx.commit()?;
        Ok(ConsolidationResult {
            request_id: id.into(),
            status: "claimed".into(),
            claimed_by_session_id: Some(context.session_id.to_string()),
            proposal_count: count,
            event_sequence,
        })
    }

    pub fn resolve_consolidation(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        id: &str,
        resolution: ConsolidationResolution,
    ) -> Result<ConsolidationResult> {
        if resolution.summary.trim().is_empty() {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "consolidation resolution summary is required",
            ));
        }
        let authenticated = self.authenticate_and_touch(store, context)?;
        let now = self.clock.now();
        let tx = store.begin_immediate()?;
        let referenced = resolution
            .duplicate_groups
            .iter()
            .flatten()
            .chain(resolution.preserved_alternatives.iter())
            .collect::<std::collections::BTreeSet<_>>();
        for proposal_id in referenced {
            let valid: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM agent_action_proposals WHERE id=?1 AND task_id=?2)",
                params![proposal_id, context.task_id.to_string()],
                |row| row.get(0),
            )?;
            if !valid {
                return Err(SessionBridgeError::domain(
                    ErrorCode::NotFound,
                    format!("proposal {proposal_id} was not found"),
                ));
            }
        }
        let changed=tx.execute("UPDATE proposal_consolidation_requests SET status='resolved',resolution_json=?4,resolved_at=?5,updated_at=?5 WHERE id=?1 AND task_id=?2 AND status='claimed' AND claimed_by_session_id=?3",params![id,context.task_id.to_string(),context.session_id.to_string(),serde_json::to_string(&resolution)?,unix_micros(now)])?;
        if changed != 1 {
            return Err(SessionBridgeError::domain(
                ErrorCode::ApprovalRequired,
                "only the Session that claimed consolidation may resolve it",
            ));
        }
        let count: i64 = tx.query_row(
            "SELECT proposal_count FROM proposal_consolidation_requests WHERE id=?1",
            [id],
            |row| row.get(0),
        )?;
        let event_sequence = crate::app::events::append(
            &tx,
            context.workspace_id,
            Some(&context.task_id.to_string()),
            "proposal.consolidation_resolved",
            crate::app::events::EventActor {
                kind: "session",
                id: Some(&authenticated.session.session_id.to_string()),
            },
            crate::app::events::EventEntity {
                kind: "proposal_consolidation_request",
                id: Some(id),
            },
            &serde_json::to_value(&resolution)?,
            now,
        )?;
        tx.commit()?;
        Ok(ConsolidationResult {
            request_id: id.into(),
            status: "resolved".into(),
            claimed_by_session_id: Some(context.session_id.to_string()),
            proposal_count: count,
            event_sequence,
        })
    }

    pub fn verify_completion_token(
        &self,
        context: &SessionContext,
        token: &str,
    ) -> Result<CompletionTokenBasis> {
        let prefix = "sb_state_completion_";
        let value = token.strip_prefix(prefix).ok_or_else(|| {
            SessionBridgeError::domain(ErrorCode::InvalidInput, "invalid completion state token")
        })?;
        let (encoded, signature) = value.rsplit_once('.').ok_or_else(|| {
            SessionBridgeError::domain(ErrorCode::InvalidInput, "invalid completion state token")
        })?;
        let expected = signed_token(
            context,
            "completion",
            &URL_SAFE_NO_PAD.decode(encoded).map_err(|_| {
                SessionBridgeError::domain(
                    ErrorCode::InvalidInput,
                    "invalid completion state token payload",
                )
            })?,
        );
        let expected_signature = expected
            .rsplit_once('.')
            .map(|(_, value)| value)
            .unwrap_or("");
        if !constant_time_equal(expected_signature.as_bytes(), signature.as_bytes()) {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "completion state token signature is invalid",
            ));
        }
        let payload = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| {
            SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "invalid completion state token payload",
            )
        })?;
        let basis: CompletionTokenBasis = serde_json::from_slice(&payload)?;
        if basis.task_id != context.task_id {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "completion state token belongs to another Task",
            ));
        }
        Ok(basis)
    }

    pub fn verify_snapshot_token(
        &self,
        context: &SessionContext,
        token: &str,
        expected_cursor: i64,
    ) -> Result<()> {
        let value = token.strip_prefix("sb_state_snapshot_").ok_or_else(|| {
            SessionBridgeError::full_resync_required("snapshot token has an invalid scope")
        })?;
        let (encoded, signature) = value.rsplit_once('.').ok_or_else(|| {
            SessionBridgeError::full_resync_required("snapshot token is malformed")
        })?;
        let payload = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| {
            SessionBridgeError::full_resync_required("snapshot token payload is invalid")
        })?;
        let expected = signed_token(context, "snapshot", &payload);
        let expected_signature = expected
            .rsplit_once('.')
            .map(|(_, value)| value)
            .unwrap_or("");
        if !constant_time_equal(expected_signature.as_bytes(), signature.as_bytes()) {
            return Err(SessionBridgeError::full_resync_required(
                "snapshot token signature is invalid",
            ));
        }
        let basis: SnapshotTokenBasis = serde_json::from_slice(&payload).map_err(|_| {
            SessionBridgeError::full_resync_required("snapshot token payload is incompatible")
        })?;
        if basis.session_id != context.session_id
            || basis.task_id != context.task_id
            || basis.protocol_version != i64::from(PROTOCOL_VERSION)
            || basis.next_cursor != expected_cursor
        {
            return Err(SessionBridgeError::full_resync_required(
                "snapshot baseline does not match this Session, protocol, or cursor",
            ));
        }
        Ok(())
    }

    pub(crate) fn authenticate_and_touch(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
    ) -> Result<AuthenticatedAgent> {
        self.authenticate(store, context, true)
    }

    fn authenticate(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        touch: bool,
    ) -> Result<AuthenticatedAgent> {
        validate_context_token(&context.token)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let repository_identity: Option<String> = transaction
            .query_row(
                "SELECT repository_identity FROM workspaces WHERE id = ?1",
                [context.workspace_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        if repository_identity.as_deref() != Some(context.repository_instance_id.as_str()) {
            return Err(SessionBridgeError::domain(
                ErrorCode::WorkspaceNotFound,
                "session context is bound to a different repository instance",
            ));
        }

        let session = load_session(&transaction, context.session_id, now)?.ok_or_else(|| {
            SessionBridgeError::domain(ErrorCode::NotFound, "Agent Session was not found")
        })?;
        if session.task_id != context.task_id {
            return Err(SessionBridgeError::domain(
                ErrorCode::NotFound,
                "session context task does not match the stored Session",
            ));
        }
        let stored_hash: Vec<u8> = transaction.query_row(
            "SELECT token_hash FROM agent_sessions WHERE id = ?1",
            [context.session_id.to_string()],
            |row| row.get(0),
        )?;
        verify_session_token(&stored_hash, &context.token)?;

        let touched = if touch {
            transaction.execute(
                "UPDATE agent_sessions SET last_seen_at = ?2, updated_at = ?2
                 WHERE id = ?1 AND ended_at IS NULL",
                params![context.session_id.to_string(), unix_micros(now)],
            )?
        } else {
            0
        };
        let participant =
            load_current_participant(&transaction, context.task_id, context.session_id)?
                .ok_or_else(|| {
                    SessionBridgeError::domain(
                        ErrorCode::NotFound,
                        "TaskParticipant for this Session was not found",
                    )
                })?;
        let mut session = session;
        if touched == 1 {
            session.last_seen_at = format_timestamp(unix_micros(now))?;
        }
        transaction.commit()?;
        Ok(AuthenticatedAgent {
            session,
            participant,
        })
    }
}

fn load_active_wait(
    connection: &Connection,
    task_id: TaskId,
    session_id: SessionId,
) -> Result<Option<ActiveAwaitSummary>> {
    let row = connection
        .query_row(
            "SELECT id, mode, created_at FROM agent_waits
             WHERE task_id = ?1 AND session_id = ?2 AND status = 'active'",
            params![task_id.to_string(), session_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((wait_id, mode, created_at_micros)) = row else {
        return Ok(None);
    };
    let targets = connection
        .prepare("SELECT work_item_id FROM agent_wait_targets WHERE wait_id = ?1 ORDER BY ordinal")?
        .query_map([&wait_id], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(Some(ActiveAwaitSummary {
        wait_id,
        mode,
        created_at_micros,
        targets,
    }))
}

fn whoami_view(authenticated: AuthenticatedAgent) -> AgentWhoami {
    let current_worktree_matches_context = authenticated
        .session
        .worktree_path
        .as_deref()
        .and_then(|expected| {
            let current = std::env::current_dir().ok()?;
            let expected = std::fs::canonicalize(expected).ok()?;
            let current = std::fs::canonicalize(current).ok()?;
            Some(expected == current)
        })
        .unwrap_or(false);
    AgentWhoami {
        session: authenticated.session,
        participant: authenticated.participant,
        current_worktree_matches_context,
    }
}

fn load_session(
    connection: &Connection,
    session_id: SessionId,
    now: OffsetDateTime,
) -> Result<Option<AgentSessionView>> {
    #[allow(clippy::type_complexity)]
    let raw: Option<(
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        i64,
        i64,
        Option<i64>,
        Option<String>,
        String,
        String,
    )> = connection
        .query_row(
            "SELECT task_id, label, declared_capabilities_json, client_name, model,
                    worktree_path, branch, base_commit, head_commit, target_branch,
                    end_reason, protocol_version, last_seen_at, ended_at, id,
                    COALESCE(executor_identity,id), executor_identity_source
             FROM agent_sessions WHERE id = ?1",
            [session_id.to_string()],
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
                    row.get(15)?,
                    row.get(16)?,
                ))
            },
        )
        .optional()?;
    let Some((
        task_id,
        label,
        declared,
        client_name,
        model,
        worktree_path,
        branch,
        base_commit,
        head_commit,
        target_branch,
        end_reason,
        protocol_version,
        last_seen_at,
        ended_at,
        _,
        executor_identity,
        executor_identity_source,
    )) = raw
    else {
        return Ok(None);
    };
    let granted_capabilities = connection
        .prepare(
            "SELECT capability FROM capability_grants
             WHERE session_id = ?1 AND revoked_at IS NULL
               AND (expires_at IS NULL OR expires_at > ?2)
             ORDER BY capability",
        )?
        .query_map(params![session_id.to_string(), unix_micros(now)], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(Some(AgentSessionView {
        session_id,
        idempotency_actor_id: session_id.to_string(),
        task_id: parse_id(&task_id, "agent_sessions.task_id")?,
        executor_identity,
        executor_identity_source,
        label,
        client_name,
        model,
        declared_capabilities: parse_string_array(&declared, "declared_capabilities_json")?,
        granted_capabilities,
        worktree_path: worktree_path.map(PathBuf::from),
        branch,
        base_commit,
        head_commit,
        target_branch,
        protocol_version,
        last_seen_at: format_timestamp(last_seen_at)?,
        ended_at: ended_at.map(format_timestamp).transpose()?,
        end_reason,
    }))
}

fn load_current_participant(
    connection: &Connection,
    task_id: TaskId,
    session_id: SessionId,
) -> Result<Option<AgentParticipantView>> {
    #[allow(clippy::type_complexity)]
    let raw: Option<(
        String,
        Option<String>,
        String,
        i64,
        String,
        i64,
        Option<String>,
        i64,
    )> = connection
        .query_row(
            "SELECT id, invitation_id, kind, completion_epoch, state, version,
                    current_conclusion_id, joined_at
             FROM task_participants
             WHERE task_id = ?1 AND session_id = ?2
             ORDER BY completion_epoch DESC LIMIT 1",
            params![task_id.to_string(), session_id.to_string()],
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
                ))
            },
        )
        .optional()?;
    raw.map(
        |(id, invitation_id, kind, epoch, state, version, conclusion, joined_at)| {
            Ok(AgentParticipantView {
                participant_id: parse_id(&id, "task_participants.id")?,
                invitation_id: invitation_id
                    .map(|value| parse_id(&value, "task_participants.invitation_id"))
                    .transpose()?,
                kind: parse_enum(&kind, "task_participants.kind")?,
                completion_epoch: epoch,
                state: parse_enum(&state, "task_participants.state")?,
                version,
                current_conclusion_id: conclusion,
                joined_at: format_timestamp(joined_at)?,
            })
        },
    )
    .transpose()
}

fn load_candidates(
    connection: &Connection,
    task_id: TaskId,
    capabilities: &BTreeSet<String>,
) -> Result<Vec<CandidateWorkItem>> {
    #[allow(clippy::type_complexity)]
    let rows = connection
        .prepare(
            "SELECT w.id, w.parent_id, w.kind, w.title, w.description, w.required,
                    w.priority, w.risk, w.required_capabilities_json,
                    w.acceptance_criteria_json, w.planned_resources_json, w.version
             FROM work_items w
             WHERE w.task_id = ?1 AND w.plan_state = 'active' AND w.result_state = 'open'
               AND NOT EXISTS (
                   SELECT 1 FROM work_relations r JOIN work_items dependency ON dependency.id = r.to_id
                   WHERE r.task_id = w.task_id AND r.from_id = w.id
                     AND r.relation_type = 'depends_on' AND dependency.result_state <> 'accepted'
               )
               AND NOT EXISTS (
                   SELECT 1 FROM blockers b WHERE b.work_item_id = w.id AND b.status = 'open'
               )
               AND NOT EXISTS (
                   SELECT 1 FROM invalidations i
                   WHERE i.task_id = w.task_id AND i.target_type = 'work_item'
                     AND i.target_id = w.id AND i.status = 'open'
               )
               AND NOT EXISTS (
                   SELECT 1 FROM work_claims c
                   WHERE c.work_item_id = w.id AND c.mode = 'execute' AND c.closed_at IS NULL
               )
             ORDER BY w.priority DESC, w.required DESC, w.created_at, w.id",
        )?
        .query_map([task_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, String>(10)?,
                row.get::<_, i64>(11)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(
            |(
                id,
                parent_id,
                kind,
                title,
                description,
                required,
                priority,
                risk,
                caps,
                acceptance,
                resources,
                version,
            )| {
                let required_capabilities =
                    parse_string_array(&caps, "required_capabilities_json")?;
                let capability_match = required_capabilities
                    .iter()
                    .all(|capability| capabilities.contains(capability));
                let mut reason_codes = vec!["ready".to_owned()];
                if required != 0 {
                    reason_codes.push("required".to_owned());
                }
                if capability_match {
                    reason_codes.push("capability_match".to_owned());
                } else {
                    reason_codes.push("capability_mismatch".to_owned());
                }
                Ok(CandidateWorkItem {
                    id,
                    parent_id,
                    kind,
                    title,
                    description,
                    required: required != 0,
                    priority,
                    risk,
                    required_capabilities,
                    acceptance_criteria: parse_string_array(
                        &acceptance,
                        "acceptance_criteria_json",
                    )?,
                    planned_resources: parse_string_array(&resources, "planned_resources_json")?,
                    version,
                    capability_match,
                    reason_codes,
                })
            },
        )
        .collect()
}

fn load_gaps(connection: &Connection, task_id: TaskId) -> Result<Vec<GapView>> {
    let rows = connection
        .prepare(
            "SELECT id, kind, severity, required, status, title, description, version
             FROM gaps WHERE task_id = ?1 AND status = 'open'
             ORDER BY required DESC,
               CASE severity WHEN 'critical' THEN 4 WHEN 'high' THEN 3 WHEN 'medium' THEN 2 ELSE 1 END DESC,
               created_at, id",
        )?
        .query_map([task_id.to_string()], |row| {
            Ok(GapView {
                id: row.get(0)?,
                kind: row.get(1)?,
                severity: row.get(2)?,
                required: row.get::<_, i64>(3)? != 0,
                status: row.get(4)?,
                title: row.get(5)?,
                description: row.get(6)?,
                version: row.get(7)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn load_participants(
    connection: &Connection,
    task_id: TaskId,
    completion_epoch: i64,
    now: OffsetDateTime,
) -> Result<Vec<ParticipantSummary>> {
    let stale_before = unix_micros(now - crate::app::task::PARTICIPANT_STALE_AFTER);
    let raw = connection
        .prepare(
            "SELECT p.id, p.session_id, s.label, p.kind, p.state, p.completion_epoch,
                    s.last_seen_at
             FROM task_participants p JOIN agent_sessions s ON s.id = p.session_id
             WHERE p.task_id = ?1 AND p.completion_epoch = ?2
             ORDER BY p.kind, s.label, p.id",
        )?
        .query_map(params![task_id.to_string(), completion_epoch], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    raw.into_iter()
        .map(
            |(participant_id, session_id, label, kind, state, epoch, last_seen)| {
                let state: ParticipantState = parse_enum(&state, "task_participants.state")?;
                Ok(ParticipantSummary {
                    participant_id: parse_id(&participant_id, "task_participants.id")?,
                    session_id: parse_id(&session_id, "task_participants.session_id")?,
                    label,
                    kind: parse_enum(&kind, "task_participants.kind")?,
                    state,
                    completion_epoch: epoch,
                    stale: last_seen <= stale_before
                        && matches!(
                            state,
                            ParticipantState::Active
                                | ParticipantState::Waiting
                                | ParticipantState::Blocked
                        ),
                    last_seen_at: format_timestamp(last_seen)?,
                })
            },
        )
        .collect()
}

fn load_invitations(
    connection: &Connection,
    task_id: TaskId,
    completion_epoch: i64,
) -> Result<Vec<InvitationSeat>> {
    let raw = connection
        .prepare(
            "SELECT id, state, participant_kind, blocks_completion, completion_epoch, expires_at
             FROM agent_invitations
             WHERE task_id = ?1 AND completion_epoch = ?2
             ORDER BY created_at, id",
        )?
        .query_map(params![task_id.to_string(), completion_epoch], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    raw.into_iter()
        .map(|(id, state, kind, blocks, epoch, expires_at)| {
            Ok(InvitationSeat {
                invitation_id: parse_id(&id, "agent_invitations.id")?,
                state,
                participant_kind: parse_enum(&kind, "agent_invitations.participant_kind")?,
                blocks_completion: blocks != 0,
                completion_epoch: epoch,
                expires_at: format_timestamp(expires_at)?,
            })
        })
        .collect()
}

fn load_inbox(
    connection: &Connection,
    task_id: TaskId,
    session_id: SessionId,
    capabilities: &BTreeSet<String>,
) -> Result<Vec<InboxRequest>> {
    let raw = connection
        .prepare(
            "SELECT id, kind, target_type, target_id, requested_by_session_id,
                    blocking, status, payload_json, required_capability, created_at
             FROM coordination_requests
             WHERE task_id = ?1 AND status IN ('open', 'acknowledged')
               AND (assigned_session_id = ?2 OR assigned_session_id IS NULL)
             ORDER BY blocking DESC, created_at, id",
        )?
        .query_map(
            params![task_id.to_string(), session_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, i64>(9)?,
                ))
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    raw.into_iter()
        .filter(|row| {
            row.8
                .as_ref()
                .is_none_or(|capability| capabilities.contains(capability))
        })
        .map(
            |(
                id,
                kind,
                target_type,
                target_id,
                requested_by,
                blocking,
                status,
                payload,
                _,
                created_at,
            )| {
                Ok(InboxRequest {
                    id,
                    kind,
                    target_type,
                    target_id,
                    requested_by_session_id: parse_id(
                        &requested_by,
                        "coordination_requests.requested_by_session_id",
                    )?,
                    blocking: blocking != 0,
                    status,
                    payload: serde_json::from_str(&payload)?,
                    created_at: format_timestamp(created_at)?,
                })
            },
        )
        .collect()
}

fn load_events(
    connection: &Connection,
    task_id: TaskId,
    since: Option<i64>,
) -> Result<(Vec<SyncEvent>, i64, bool)> {
    let maximum: Option<i64> = connection.query_row(
        "SELECT MAX(sequence) FROM events WHERE task_id = ?1",
        [task_id.to_string()],
        |row| row.get(0),
    )?;
    let minimum: Option<i64> =
        connection.query_row("SELECT MIN(sequence) FROM events", [], |row| row.get(0))?;
    if let (Some(since), Some(minimum)) = (since, minimum)
        && since < minimum - 1
    {
        return Err(SessionBridgeError::domain(
            ErrorCode::FullResyncRequired,
            "requested event cursor is older than retained task history",
        )
        .with_details(serde_json::json!({
            "requested_cursor": since,
            "minimum_cursor": minimum,
            "invalid_scope": "snapshot",
            "recovery_options": [{
                "action": "full_resync",
                "command": "sessionbridge agent sync --full --format json",
                "safe": true
            }],
        })));
    }
    let basis = since.unwrap_or(0);
    let raw = connection
        .prepare(
            "SELECT sequence, event_type, actor_type, actor_id, entity_type,
                    entity_id, payload_json, created_at
             FROM events WHERE task_id = ?1 AND sequence > ?2
             ORDER BY sequence LIMIT ?3",
        )?
        .query_map(
            params![task_id.to_string(), basis, MAX_SYNC_EVENTS + 1],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                ))
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let has_more = raw.len() > usize::try_from(MAX_SYNC_EVENTS).unwrap_or(1_000);
    let mut events = raw;
    events.truncate(usize::try_from(MAX_SYNC_EVENTS).unwrap_or(1_000));
    let events = events
        .into_iter()
        .map(
            |(
                sequence,
                event_type,
                actor_type,
                actor_id,
                entity_type,
                entity_id,
                payload,
                created_at,
            )| {
                Ok(SyncEvent {
                    sequence,
                    event_type,
                    actor_type,
                    actor_id,
                    entity_type,
                    entity_id,
                    payload: serde_json::from_str(&payload)?,
                    created_at: format_timestamp(created_at)?,
                })
            },
        )
        .collect::<Result<Vec<_>>>()?;
    let next = events
        .last()
        .map(|event| event.sequence)
        .or(since)
        .or(maximum)
        .unwrap_or(0);
    Ok((events, next, has_more))
}

fn effective_capabilities(session: &AgentSessionView) -> BTreeSet<String> {
    session
        .declared_capabilities
        .iter()
        .chain(&session.granted_capabilities)
        .cloned()
        .collect()
}

fn soft_recommended_session(
    work_item_id: &str,
    participants: &[ParticipantSummary],
) -> Option<SessionId> {
    let mut eligible = participants
        .iter()
        .filter(|participant| {
            participant.kind == ParticipantKind::Worker
                && participant.state == ParticipantState::Active
                && !participant.stale
        })
        .map(|participant| participant.session_id)
        .collect::<Vec<_>>();
    eligible.sort_by_key(ToString::to_string);
    if eligible.is_empty() {
        return None;
    }
    let digest = Sha256::digest(work_item_id.as_bytes());
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    let index = usize::try_from(u64::from_be_bytes(bytes) % eligible.len() as u64).ok()?;
    eligible.get(index).copied()
}

fn parse_string_array(value: &str, field: &str) -> Result<Vec<String>> {
    serde_json::from_str::<Vec<String>>(value).map_err(|error| {
        SessionBridgeError::domain(
            ErrorCode::Database,
            format!("database field {field} is not a string array: {error}"),
        )
    })
}

fn parse_id<T: FromStr>(value: &str, field: &str) -> Result<T>
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

fn parse_enum<T: FromStr<Err = String>>(value: &str, field: &str) -> Result<T> {
    value.parse().map_err(|error| {
        SessionBridgeError::domain(
            ErrorCode::Database,
            format!("database field {field} is invalid: {error}"),
        )
    })
}

fn format_timestamp(micros: i64) -> Result<String> {
    let nanos = i128::from(micros)
        .checked_mul(1_000)
        .ok_or_else(|| SessionBridgeError::domain(ErrorCode::Database, "timestamp overflow"))?;
    OffsetDateTime::from_unix_timestamp_nanos(nanos)
        .map_err(|error| {
            SessionBridgeError::domain(
                ErrorCode::Database,
                format!("database timestamp is out of range: {error}"),
            )
        })?
        .format(&Rfc3339)
        .map_err(|error| {
            SessionBridgeError::domain(
                ErrorCode::Internal,
                format!("timestamp formatting failed: {error}"),
            )
        })
}

fn load_owned_obligations(
    connection: &Connection,
    context: &SessionContext,
    observed_at: OffsetDateTime,
) -> Result<Vec<PlannedAction>> {
    let now = unix_micros(observed_at);
    connection.execute(
        "UPDATE stage_handoffs SET status='expired',updated_at=?1
         WHERE status='reserved' AND expires_at<=?1",
        [now],
    )?;
    let rows = connection
        .prepare(
            "SELECT c.id, c.work_item_id, c.mode, c.lease_expires_at, w.title
             FROM work_claims c JOIN work_items w ON w.id = c.work_item_id
             WHERE c.task_id = ?1 AND c.session_id = ?2 AND c.closed_at IS NULL
             ORDER BY c.lease_expires_at, c.claimed_at",
        )?
        .query_map(
            params![context.task_id.to_string(), context.session_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut obligations: Vec<PlannedAction> = rows
        .into_iter()
        .map(|(claim_id, item_id, mode, lease, title)| PlannedAction {
            action: "continue_owned_claim".into(),
            priority: 110,
            reason: format!("active {mode} Claim for {title}; lease expires at {lease}"),
            advisory: true,
            conflict_free: true,
            entity_type: Some("claim".into()),
            entity_id: Some(claim_id),
            required_capabilities: Vec::new(),
            expand_ref: format!("sessionbridge item inspect {item_id}"),
            source_entities: Vec::new(),
            computed_at_cursor: 0,
            confidence: "high".into(),
            assumptions: Vec::new(),
        })
        .collect();
    let handoffs = connection
        .prepare(
            "SELECT h.id,h.work_item_id,h.to_stage,h.expires_at,w.title
             FROM stage_handoffs h JOIN work_items w ON w.id=h.work_item_id
             WHERE h.task_id=?1 AND h.coordinator_session_id=?2
               AND h.status='reserved' AND h.expires_at>?3
             ORDER BY h.expires_at,h.created_at",
        )?
        .query_map(
            params![
                context.task_id.to_string(),
                context.session_id.to_string(),
                now
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    obligations.extend(handoffs.into_iter().map(
        |(handoff_id, item_id, to_stage, expires_at, title)| PlannedAction {
            action: "coordinate_stage_handoff".into(),
            priority: 105,
            reason: format!(
                "{title} is awaiting {to_stage}; coordination reservation expires at {expires_at}"
            ),
            advisory: true,
            conflict_free: true,
            entity_type: Some("stage_handoff".into()),
            entity_id: Some(handoff_id),
            required_capabilities: Vec::new(),
            expand_ref: format!("sessionbridge item inspect {item_id}"),
            source_entities: Vec::new(),
            computed_at_cursor: 0,
            confidence: "high".into(),
            assumptions: Vec::new(),
        },
    ));
    Ok(obligations)
}

fn load_owned_claim_summaries(
    connection: &Connection,
    context: &SessionContext,
) -> Result<Vec<OwnedClaimSummary>> {
    connection
        .prepare(
            "SELECT id,work_item_id,mode,generation,lease_expires_at
             FROM work_claims
             WHERE task_id=?1 AND session_id=?2 AND closed_at IS NULL
             ORDER BY claimed_at,id",
        )?
        .query_map(
            params![context.task_id.to_string(), context.session_id.to_string()],
            |row| {
                Ok(OwnedClaimSummary {
                    claim_id: row.get(0)?,
                    work_item_id: row.get(1)?,
                    mode: row.get(2)?,
                    generation: row.get(3)?,
                    lease_expires_at_micros: row.get(4)?,
                })
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn load_stage_actions(
    connection: &Connection,
    context: &SessionContext,
) -> Result<Vec<PlannedAction>> {
    let rows = connection
        .prepare(
            "SELECT w.id, w.title,
                    EXISTS(SELECT 1 FROM review_results r WHERE r.work_item_id=w.id AND r.decision='approved'),
                    (SELECT c.id FROM change_sets c JOIN work_attempts a ON a.id=c.attempt_id
                     WHERE a.work_item_id=w.id AND c.integration_state='pending'
                     ORDER BY c.created_at LIMIT 1),
                    (SELECT c.changed_resources_json FROM change_sets c
                     JOIN work_attempts a ON a.id=c.attempt_id
                     WHERE a.work_item_id=w.id AND c.integration_state='pending'
                     ORDER BY c.created_at LIMIT 1)
             FROM work_items w
             WHERE w.task_id=?1 AND w.plan_state='active' AND w.result_state='submitted'
               AND NOT EXISTS(SELECT 1 FROM work_claims c WHERE c.work_item_id=w.id AND c.closed_at IS NULL)
             ORDER BY w.required DESC, w.priority DESC, w.updated_at, w.id",
        )?
        .query_map([context.task_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .map(|(id, title, approved, change_set_id, resources_json)| {
            let conflicts = match (&change_set_id, &resources_json) {
                (Some(change_set_id), Some(resources_json)) if approved => {
                    integration_resource_conflicts(
                        connection,
                        context,
                        change_set_id,
                        resources_json,
                    )
                    .unwrap_or_else(|error| vec![format!("preflight failed: {error}")])
                }
                _ => Vec::new(),
            };
            let (action, priority, reason, conflict_free) = if approved && change_set_id.is_some() {
                if conflicts.is_empty() {
                    (
                        "claim_integration",
                        95,
                        "approved ChangeSet is pending integration and has no owner".to_owned(),
                        true,
                    )
                } else {
                    (
                        "coordinate_integration_conflict",
                        96,
                        format!("integration resources conflict: {}", conflicts.join("; ")),
                        false,
                    )
                }
            } else {
                (
                    "claim_review",
                    100,
                    "submitted WorkItem requires review and has no owner".to_owned(),
                    true,
                )
            };
            PlannedAction {
                action: action.into(),
                priority,
                reason: format!("{reason}: {title}"),
                advisory: true,
                conflict_free,
                entity_type: Some("work_item".into()),
                entity_id: Some(id.clone()),
                required_capabilities: Vec::new(),
                expand_ref: format!("sessionbridge item inspect {id}"),
                source_entities: Vec::new(),
                computed_at_cursor: 0,
                confidence: "high".into(),
                assumptions: conflicts,
            }
        })
        .collect())
}

fn integration_resource_conflicts(
    connection: &Connection,
    context: &SessionContext,
    change_set_id: &str,
    resources_json: &str,
) -> Result<Vec<String>> {
    let requested: Vec<String> = serde_json::from_str(resources_json)?;
    let reservations = connection
        .prepare(
            "SELECT owner_type,owner_id,resource_key,mode FROM resource_reservations
             WHERE task_id=?1 AND released_at IS NULL AND policy='enforced'
               AND NOT(owner_type='change_set' AND owner_id=?2)",
        )?
        .query_map(params![context.task_id.to_string(), change_set_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut conflicts = Vec::new();
    for requested_key in requested {
        for (owner_type, owner_id, resource_key, mode) in &reservations {
            if resources_overlap(&requested_key, resource_key) {
                conflicts.push(format!(
                    "{requested_key} vs {owner_type}:{owner_id}:{resource_key}({mode})"
                ));
            }
        }
    }
    conflicts.sort();
    conflicts.dedup();
    Ok(conflicts)
}

fn load_awaitable_dependencies(
    connection: &Connection,
    context: &SessionContext,
    now: i64,
) -> Result<Vec<AwaitableDependency>> {
    connection.execute(
        "UPDATE stage_handoffs SET status='expired',updated_at=?1
         WHERE status='reserved' AND expires_at<=?1",
        [now],
    )?;
    let rows = connection
        .prepare(
            "SELECT dependency.id,dependency.title,source.id,
                    COALESCE(
                        (SELECT c.session_id FROM work_claims c
                         WHERE c.work_item_id=dependency.id AND c.closed_at IS NULL
                           AND c.lease_expires_at>?3
                         ORDER BY c.claimed_at DESC LIMIT 1),
                        (SELECT h.coordinator_session_id FROM stage_handoffs h
                         WHERE h.work_item_id=dependency.id AND h.status='reserved'
                           AND h.expires_at>?3
                         ORDER BY h.created_at DESC LIMIT 1)
                    ),
                    COALESCE(
                        (SELECT s.label FROM work_claims c
                         JOIN agent_sessions s ON s.id=c.session_id
                         WHERE c.work_item_id=dependency.id AND c.closed_at IS NULL
                           AND c.lease_expires_at>?3
                         ORDER BY c.claimed_at DESC LIMIT 1),
                        (SELECT s.label FROM stage_handoffs h
                         JOIN agent_sessions s ON s.id=h.coordinator_session_id
                         WHERE h.work_item_id=dependency.id AND h.status='reserved'
                           AND h.expires_at>?3
                         ORDER BY h.created_at DESC LIMIT 1)
                    ),
                    CASE WHEN EXISTS(
                        SELECT 1 FROM work_claims c WHERE c.work_item_id=dependency.id
                          AND c.closed_at IS NULL AND c.lease_expires_at>?3
                    ) THEN 'claim' ELSE 'stage_handoff' END
             FROM work_relations relation
             JOIN work_items source ON source.id=relation.from_id
             JOIN work_items dependency ON dependency.id=relation.to_id
             WHERE relation.task_id=?1 AND relation.relation_type='depends_on'
               AND source.plan_state='active' AND source.result_state='open'
               AND dependency.plan_state='active' AND dependency.result_state<>'accepted'
               AND COALESCE(
                    (SELECT c.session_id FROM work_claims c
                     WHERE c.work_item_id=dependency.id AND c.closed_at IS NULL
                       AND c.lease_expires_at>?3 ORDER BY c.claimed_at DESC LIMIT 1),
                    (SELECT h.coordinator_session_id FROM stage_handoffs h
                     WHERE h.work_item_id=dependency.id AND h.status='reserved'
                       AND h.expires_at>?3 ORDER BY h.created_at DESC LIMIT 1)
               ) IS NOT NULL
               AND COALESCE(
                    (SELECT c.session_id FROM work_claims c
                     WHERE c.work_item_id=dependency.id AND c.closed_at IS NULL
                       AND c.lease_expires_at>?3 ORDER BY c.claimed_at DESC LIMIT 1),
                    (SELECT h.coordinator_session_id FROM stage_handoffs h
                     WHERE h.work_item_id=dependency.id AND h.status='reserved'
                       AND h.expires_at>?3 ORDER BY h.created_at DESC LIMIT 1)
               )<>?2
             ORDER BY dependency.id,source.id",
        )?
        .query_map(
            params![
                context.task_id.to_string(),
                context.session_id.to_string(),
                now
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut grouped: BTreeMap<String, AwaitableDependency> = BTreeMap::new();
    for (item_id, title, blocked_item_id, owner_session_id, owner_label, source) in rows {
        let dependency = grouped
            .entry(item_id.clone())
            .or_insert_with(|| AwaitableDependency {
                work_item_id: item_id.clone(),
                title,
                blocking_work_item_ids: Vec::new(),
                owner_session_id,
                owner_label,
                ownership_source: source,
                await_ref: format!(
                    "sessionbridge agent await work-items --item {item_id} --format json"
                ),
            });
        dependency.blocking_work_item_ids.push(blocked_item_id);
    }
    Ok(grouped.into_values().collect())
}

fn load_consolidation_actions(
    connection: &Connection,
    context: &SessionContext,
) -> Result<Vec<PlannedAction>> {
    let rows = connection
        .prepare("SELECT id,proposal_count,threshold FROM proposal_consolidation_requests WHERE task_id=?1 AND status='open' ORDER BY created_at")?
        .query_map([context.task_id.to_string()], |row| Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?,row.get::<_,i64>(2)?)))?
        .collect::<std::result::Result<Vec<_>,_>>()?;
    Ok(rows.into_iter().map(|(id,count,threshold)| PlannedAction {
        action:"consolidate_proposals".into(),
        priority:75,
        reason:format!("{count} open proposals exceed the soft budget {threshold}; preserve alternatives while grouping duplicates and recording disagreements"),
        advisory:true,
        conflict_free:true,
        entity_type:Some("proposal_consolidation_request".into()),
        entity_id:Some(id),
        required_capabilities:Vec::new(),
        expand_ref:"sessionbridge agent sync --full --format json".into(),
        source_entities:Vec::new(),computed_at_cursor:0,confidence:"high".into(),assumptions:Vec::new(),
    }).collect())
}

fn load_owned_consolidations(
    connection: &Connection,
    context: &SessionContext,
) -> Result<Vec<PlannedAction>> {
    let rows=connection.prepare("SELECT id,proposal_count FROM proposal_consolidation_requests WHERE task_id=?1 AND status='claimed' AND claimed_by_session_id=?2 ORDER BY updated_at")?.query_map(params![context.task_id.to_string(),context.session_id.to_string()],|row|Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
    Ok(rows.into_iter().map(|(id,count)|PlannedAction{action:"resolve_proposal_consolidation".into(),priority:106,reason:format!("claimed consolidation covers {count} proposals and must preserve alternatives and disagreements"),advisory:true,conflict_free:true,entity_type:Some("proposal_consolidation_request".into()),entity_id:Some(id),required_capabilities:Vec::new(),expand_ref:"sessionbridge agent sync --full --format json".into(),source_entities:Vec::new(),computed_at_cursor:0,confidence:"high".into(),assumptions:Vec::new()}).collect())
}

fn load_speculative_actions(
    connection: &Connection,
    context: &SessionContext,
) -> Result<Vec<PlannedAction>> {
    let rows = connection
        .prepare(
            "SELECT id,title,status,session_id FROM speculative_work
         WHERE task_id=?1 AND status IN ('ready_for_adoption','revalidation_required')
         ORDER BY updated_at,id",
        )?
        .query_map([context.task_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .map(|(id, title, status, owner)| {
            let revalidation = status == "revalidation_required";
            PlannedAction {
                action: if revalidation {
                    "revalidate_speculation".into()
                } else {
                    "propose_speculative_adoption".into()
                },
                priority: if revalidation && owner == context.session_id.to_string() {
                    80
                } else {
                    65
                },
                reason: if revalidation {
                    format!("speculative result {title} was based on an older plan or merge point")
                } else {
                    format!("isolated result {title} is ready to be evaluated for formal adoption")
                },
                advisory: true,
                conflict_free: true,
                entity_type: Some("speculative_work".into()),
                entity_id: Some(id.clone()),
                required_capabilities: Vec::new(),
                expand_ref: format!("sessionbridge agent inspect {id}"),
                source_entities: Vec::new(),
                computed_at_cursor: 0,
                confidence: "medium".into(),
                assumptions: vec!["speculative result remains isolated until adoption".into()],
            }
        })
        .collect())
}

fn load_open_proposal_actions(
    connection: &Connection,
    context: &SessionContext,
) -> Result<Vec<PlannedAction>> {
    let rows = connection
        .prepare(
            "SELECT id,kind,summary,safety_relevant
             FROM agent_action_proposals
             WHERE task_id=?1 AND status='proposed'
               AND kind IN ('completion_challenge','capability_request')
             ORDER BY safety_relevant DESC,created_at,id LIMIT 20",
        )?
        .query_map([context.task_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, bool>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .map(|(id, kind, summary, safety_relevant)| PlannedAction {
            action: "assess_action_proposal".into(),
            priority: if kind == "completion_challenge" || safety_relevant {
                104
            } else {
                74
            },
            reason: format!("open {kind} requires an explicit audited resolution: {summary}"),
            advisory: true,
            conflict_free: true,
            entity_type: Some("agent_action_proposal".into()),
            entity_id: Some(id.clone()),
            required_capabilities: Vec::new(),
            expand_ref: format!("sessionbridge agent inspect {id}"),
            source_entities: vec![id],
            computed_at_cursor: 0,
            confidence: "high".into(),
            assumptions: Vec::new(),
        })
        .collect())
}

fn load_runtime_health_actions(
    connection: &Connection,
    context: &SessionContext,
    now: i64,
) -> Result<Vec<PlannedAction>> {
    let threshold = crate::app::task::cutoff_micros(now, crate::app::task::RUNTIME_STALL_AFTER);
    let mut actions = Vec::new();
    let unclaimed=connection.prepare("SELECT id,title,created_at FROM work_items w WHERE task_id=?1 AND plan_state='active' AND result_state='open' AND created_at<?2 AND NOT EXISTS(SELECT 1 FROM work_claims c WHERE c.work_item_id=w.id AND c.closed_at IS NULL) ORDER BY created_at LIMIT 20")?.query_map(params![context.task_id.to_string(),threshold],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
    actions.extend(unclaimed.into_iter().map(|(id, title, created)| {
        health_action(
            "inspect_unclaimed_work",
            70,
            id,
            format!(
                "{title} has remained unclaimed for {} seconds",
                (now - created).max(0) / 1_000_000
            ),
        )
    }));
    let claims=connection.prepare("SELECT c.id,w.title,COALESCE(c.renewed_at,c.claimed_at),c.generation FROM work_claims c JOIN work_items w ON w.id=c.work_item_id WHERE c.task_id=?1 AND c.closed_at IS NULL AND COALESCE(c.renewed_at,c.claimed_at)<?2 ORDER BY COALESCE(c.renewed_at,c.claimed_at) LIMIT 20")?.query_map(params![context.task_id.to_string(),threshold],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
    actions.extend(claims.into_iter().map(|(id, title, last_progress, generation)| {
        health_action(
            "inspect_stalled_claim",
            78,
            id,
            format!(
                "{title} Claim generation {generation} has had no recorded progress for {} seconds",
                (now - last_progress).max(0) / 1_000_000
            ),
        )
    }));
    let repeatedly_reclaimed=connection.prepare("SELECT active.id,w.title,COUNT(previous.id) FROM work_claims active JOIN work_items w ON w.id=active.work_item_id JOIN work_claims previous ON previous.work_item_id=active.work_item_id AND previous.mode=active.mode AND previous.close_reason='reclaimed' WHERE active.task_id=?1 AND active.closed_at IS NULL GROUP BY active.id,w.title HAVING COUNT(previous.id)>=2 ORDER BY COUNT(previous.id) DESC,active.claimed_at LIMIT 20")?.query_map([context.task_id.to_string()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
    actions.extend(repeatedly_reclaimed.into_iter().map(|(id, title, reclaim_count)| {
        health_action(
            "inspect_repeated_reclaim",
            84,
            id,
            format!("{title} has been reclaimed {reclaim_count} times; inspect ownership and worktree recovery causes"),
        )
    }));
    let integrations=connection.prepare("SELECT c.id,w.title,c.created_at FROM change_sets c JOIN work_attempts a ON a.id=c.attempt_id JOIN work_items w ON w.id=a.work_item_id WHERE c.task_id=?1 AND c.integration_state='pending' AND c.created_at<?2 ORDER BY c.created_at LIMIT 20")?.query_map(params![context.task_id.to_string(),threshold],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
    actions.extend(integrations.into_iter().map(|(id, title, created)| {
        health_action(
            "inspect_integration_starvation",
            82,
            id,
            format!(
                "{title} ChangeSet has awaited integration for {} seconds",
                (now - created).max(0) / 1_000_000
            ),
        )
    }));
    let session_id = context.session_id.to_string();
    let participants =
        load_participant_liveness(connection, context.task_id, Some(&session_id), now, 20)?;
    actions.extend(participants.into_iter().map(|participant| PlannedAction {
        action: if participant.retirement_candidate {
            "assess_participant_retirement".into()
        } else {
            "coordinate_stale_participant".into()
        },
        priority: if participant.retirement_candidate {
            88
        } else {
            72
        },
        reason: format!(
            "{} has not reported activity for {} seconds; {}",
            participant.label,
            (now - participant.last_seen_at).max(0) / 1_000_000,
            if participant.retirement_candidate {
                "Agent-managed handoff and retirement assessment is now eligible"
            } else {
                "coordinate recovery, but do not retire before the 30 minute threshold"
            }
        ),
        advisory: true,
        conflict_free: true,
        entity_type: Some("participant".into()),
        entity_id: Some(participant.participant_id.clone()),
        required_capabilities: Vec::new(),
        expand_ref: format!("sessionbridge agent inspect {}", participant.participant_id),
        source_entities: vec![participant.participant_id],
        computed_at_cursor: 0,
        confidence: "high".into(),
        assumptions: vec![
            "retirement remains an Agent decision and requires handoff evidence".into(),
        ],
    }));
    Ok(actions)
}

fn health_action(action: &str, priority: i64, id: String, reason: String) -> PlannedAction {
    PlannedAction {
        action: action.into(),
        priority,
        reason,
        advisory: true,
        conflict_free: true,
        entity_type: Some("health_signal".into()),
        entity_id: Some(id),
        required_capabilities: Vec::new(),
        expand_ref: "sessionbridge agent sync --full --format json".into(),
        source_entities: Vec::new(),
        computed_at_cursor: 0,
        confidence: "medium".into(),
        assumptions: Vec::new(),
    }
}

fn deliverable_actions(deliverables: &[DeliverableView]) -> Vec<PlannedAction> {
    deliverables
        .iter()
        .filter(|item| item.required && !matches!(item.status.as_str(), "verified" | "waived"))
        .map(|item| PlannedAction {
            action: if item.work_item_ids.is_empty() {
                "plan_deliverable".into()
            } else {
                "advance_deliverable".into()
            },
            priority: if item.work_item_ids.is_empty() {
                85
            } else {
                60
            },
            reason: if item.work_item_ids.is_empty() {
                "required Deliverable has no mapped WorkItem".into()
            } else {
                "required Deliverable is not yet verified".into()
            },
            advisory: true,
            conflict_free: item.work_item_ids.is_empty(),
            entity_type: Some("deliverable".into()),
            entity_id: Some(item.id.clone()),
            required_capabilities: Vec::new(),
            expand_ref: "sessionbridge deliverable list current --format json".into(),
            source_entities: Vec::new(),
            computed_at_cursor: 0,
            confidence: "high".into(),
            assumptions: Vec::new(),
        })
        .collect()
}

fn completion_blockers(health: &TaskHealth) -> Vec<serde_json::Value> {
    health
        .completion_signal_warnings
        .iter()
        .filter(|warning| warning.count > 0)
        .map(|warning| {
            serde_json::json!({
                "code": warning.code,
                "count": warning.count,
                "severity": warning.severity,
            })
        })
        .collect()
}

fn persist_blocking_fingerprint(
    connection: &Connection,
    context: &SessionContext,
    participant_id: &str,
    dependency_version: i64,
    cursor: i64,
    blockers: &[serde_json::Value],
    now: OffsetDateTime,
) -> Result<Option<BlockingFingerprintView>> {
    let now_micros = unix_micros(now);
    if blockers.is_empty() {
        connection.execute(
            "UPDATE blocking_fingerprints SET resolved_at=?2,updated_at=?2
             WHERE participant_id=?1 AND resolved_at IS NULL",
            params![participant_id, now_micros],
        )?;
        return Ok(None);
    }

    let blocker_code = blockers
        .first()
        .and_then(|blocker| blocker.get("code"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("COMPLETION_BLOCKED")
        .to_owned();
    let entity_id = blockers
        .first()
        .and_then(|blocker| blocker.get("entity_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let canonical = serde_json::to_vec(&serde_json::json!({
        "task_id": context.task_id,
        "participant_id": participant_id,
        "blockers": blockers,
    }))?;
    let fingerprint = format!("sha256:{:x}", Sha256::digest(canonical));
    let id = uuid::Uuid::now_v7().to_string();
    connection.execute(
        "INSERT INTO blocking_fingerprints (
            id,task_id,participant_id,blocker_code,entity_id,dependency_version,
            fingerprint,first_observed_cursor,last_observed_cursor,unchanged_since,
            resolved_at,created_at,updated_at
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?8,?9,NULL,?9,?9)
         ON CONFLICT(participant_id) DO UPDATE SET
            blocker_code=excluded.blocker_code,
            entity_id=excluded.entity_id,
            dependency_version=excluded.dependency_version,
            first_observed_cursor=CASE
                WHEN blocking_fingerprints.fingerprint=excluded.fingerprint
                THEN blocking_fingerprints.first_observed_cursor
                ELSE excluded.first_observed_cursor END,
            last_observed_cursor=excluded.last_observed_cursor,
            unchanged_since=CASE
                WHEN blocking_fingerprints.fingerprint=excluded.fingerprint
                THEN blocking_fingerprints.unchanged_since
                ELSE excluded.unchanged_since END,
            fingerprint=excluded.fingerprint,
            resolved_at=NULL,
            updated_at=excluded.updated_at",
        params![
            id,
            context.task_id.to_string(),
            participant_id,
            blocker_code,
            entity_id,
            dependency_version,
            fingerprint,
            cursor,
            now_micros,
        ],
    )?;
    let mut view = connection.query_row(
        "SELECT blocker_code,entity_id,dependency_version,fingerprint,
                first_observed_cursor,last_observed_cursor,unchanged_since
         FROM blocking_fingerprints WHERE participant_id=?1 AND resolved_at IS NULL",
        [participant_id],
        |row| {
            Ok(BlockingFingerprintView {
                blocker_code: row.get(0)?,
                entity_id: row.get(1)?,
                dependency_version: row.get(2)?,
                fingerprint: row.get(3)?,
                first_observed_cursor: row.get(4)?,
                last_observed_cursor: row.get(5)?,
                unchanged_since_micros: row.get(6)?,
                category: String::new(),
                stable_for_millis: 0,
                suggested_action: String::new(),
                immediately_blockable: false,
            })
        },
    )?;
    let codes = blockers
        .iter()
        .filter_map(|value| value.get("code").and_then(serde_json::Value::as_str))
        .collect::<Vec<_>>();
    let stable_micros = (now_micros - view.unchanged_since_micros).max(0);
    let (category, action, immediate) = classify_blockers(&codes, stable_micros);
    view.category = category.into();
    view.suggested_action = action.into();
    view.immediately_blockable = immediate;
    view.stable_for_millis = stable_micros / 1000;
    Ok(Some(view))
}

fn classify_blockers(codes: &[&str], stable_micros: i64) -> (&'static str, &'static str, bool) {
    if codes.iter().any(|code| {
        matches!(
            *code,
            "PENDING_APPROVALS" | "PENDING_BLOCKING_INVITATIONS" | "UNRESOLVED_DECISIONS"
        )
    }) {
        return ("external", "assess_external_wait", false);
    }
    if codes.iter().any(|code| {
        matches!(
            *code,
            "ACTIVE_CLAIMS"
                | "PENDING_REVIEWS"
                | "PENDING_INTEGRATION"
                | "OPEN_BLOCKING_COORDINATION_REQUESTS"
                | "WAITING_PARTICIPANTS"
        )
    }) {
        return ("agent_owned", "inspect_blocking_dependencies", false);
    }
    if codes
        .iter()
        .any(|code| code.starts_with("FATAL_") || code.starts_with("UNRECOVERABLE_"))
    {
        return ("irrecoverable", "assess_blocked", true);
    }
    if stable_micros >= 60_000_000 {
        ("uncertain", "assess_blocked", false)
    } else {
        ("uncertain", "observe_blocker_window", false)
    }
}

fn scoped_tokens(context: &SessionContext, sync: &AgentSync) -> Result<ScopedStateTokens> {
    let work_items = sync
        .candidates
        .iter()
        .map(|item| {
            Ok((
                item.id.clone(),
                signed_token(
                    context,
                    &format!("work_item:{}", item.id),
                    &serde_json::to_vec(&serde_json::json!({
                        "work_item_id": item.id,
                        "version": item.version,
                        "plan_version": sync.task.task.plan_version,
                    }))?,
                ),
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let claims = sync
        .owned_claims
        .iter()
        .map(|claim| {
            Ok((
                claim.claim_id.clone(),
                signed_token(
                    context,
                    &format!("claim:{}", claim.claim_id),
                    &serde_json::to_vec(&serde_json::json!({
                        "claim_id": claim.claim_id,
                        "work_item_id": claim.work_item_id,
                        "generation": claim.generation,
                        "lease_expires_at_micros": claim.lease_expires_at_micros,
                    }))?,
                ),
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    Ok(ScopedStateTokens {
        task_plan: signed_token(
            context,
            "task_plan",
            &serde_json::to_vec(&serde_json::json!({
                "task_id": context.task_id,
                "plan_version": sync.task.task.plan_version,
            }))?,
        ),
        completion: signed_token(
            context,
            "completion",
            &serde_json::to_vec(&serde_json::json!({
                "task_id": context.task_id,
                "plan_version": sync.task.task.plan_version,
                "completion_epoch": sync.task.task.completion_epoch,
                "completion_revision": sync.task.task.completion_revision,
                "event_cursor": sync.next_cursor,
            }))?,
        ),
        participant: signed_token(
            context,
            "participant",
            &serde_json::to_vec(&serde_json::json!({
                "participant_id": sync.whoami.participant.participant_id,
                "version": sync.whoami.participant.version,
            }))?,
        ),
        work_items,
        claims,
    })
}

fn signed_token(context: &SessionContext, scope: &str, payload: &[u8]) -> String {
    let encoded = URL_SAFE_NO_PAD.encode(payload);
    let mut digest = Sha256::new();
    digest.update(context.token.as_bytes());
    digest.update([0]);
    digest.update(scope.as_bytes());
    digest.update([0]);
    digest.update(encoded.as_bytes());
    format!(
        "sb_state_{scope}_{encoded}.{}",
        URL_SAFE_NO_PAD.encode(digest.finalize())
    )
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn enum_text<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_value(value)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| {
            SessionBridgeError::domain(ErrorCode::Internal, "enum did not serialize as text")
        })
}

fn validate_context_token(token: &str) -> Result<()> {
    if !token.starts_with(SESSION_TOKEN_PREFIX) || token.len() != SESSION_TOKEN_PREFIX.len() + 43 {
        return Err(SessionBridgeError::domain(
            ErrorCode::NotFound,
            "session context credentials are invalid",
        ));
    }
    Ok(())
}

const SESSION_TOKEN_PREFIX: &str = "sb_session_";

fn verify_session_token(stored_hash: &[u8], token: &str) -> Result<()> {
    if stored_hash.len() != 32 {
        return Err(SessionBridgeError::domain(
            ErrorCode::Database,
            "stored Session token hash has an invalid length",
        ));
    }
    let candidate = Sha256::digest(token.as_bytes());
    let difference = stored_hash
        .iter()
        .zip(candidate.iter())
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        });
    if difference != 0 {
        return Err(SessionBridgeError::domain(
            ErrorCode::NotFound,
            "session context credentials are invalid",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod blocker_classification_tests {
    use super::classify_blockers;

    #[test]
    fn blocker_categories_preserve_wait_and_blocked_distinctions() {
        assert_eq!(classify_blockers(&["PENDING_APPROVALS"], 0).0, "external");
        assert_eq!(
            classify_blockers(&["ACTIVE_CLAIMS"], 0).1,
            "inspect_blocking_dependencies"
        );
        assert_eq!(
            classify_blockers(&["UNRECOVERABLE_WORKTREE"], 0),
            ("irrecoverable", "assess_blocked", true)
        );
        assert_eq!(
            classify_blockers(&["OPEN_REQUIRED_GAPS"], 30_000_000).1,
            "observe_blocker_window"
        );
        assert_eq!(
            classify_blockers(&["OPEN_REQUIRED_GAPS"], 60_000_000).1,
            "assess_blocked"
        );
    }
}
