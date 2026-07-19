use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use time::Duration;
use uuid::Uuid;

use crate::{
    Result,
    app::{
        agent::AgentService,
        idempotency::{self, IdempotencyKey},
        runtime_reconciler::reconcile_after_agent_write,
        work::{
            AttemptInputPin, ChangeSetInput, ClaimResult, ClaimWorkItemRequest,
            CompleteReviewRequest, CompleteReviewResult, EvidenceInput, IntegrateChangeSetRequest,
            IntegrateChangeSetResult, IntegrationOutcome, ResourceRequest, ReviewDecision,
            SubmitWorkItemRequest, SubmitWorkItemResult, WorkService,
        },
    },
    clock::{Clock, unix_micros},
    domain::{ChangeSetId, ClaimId, ClaimMode, EvidenceId, SessionContext, WorkItemId},
    error::{ErrorCode, SessionBridgeError},
    git::GitRepository,
    storage::SqliteStore,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitBundle {
    #[serde(default)]
    pub evidence: Vec<EvidenceInput>,
    pub change_set: ChangeSetInput,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartWorkflowRequest {
    pub item_id: WorkItemId,
    #[serde(default)]
    pub resources: Vec<ResourceRequest>,
    #[serde(default)]
    pub inputs: Vec<AttemptInputPin>,
    pub lease_seconds: Option<i64>,
    #[serde(default)]
    pub adopt_existing: bool,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewWorkflowRequest {
    pub item_id: WorkItemId,
    pub decision: ReviewDecision,
    #[serde(default)]
    pub evidence_ids: Vec<EvidenceId>,
    pub lease_seconds: i64,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrationPreparation {
    pub preparation_id: String,
    pub work_item_id: WorkItemId,
    pub change_set_id: ChangeSetId,
    pub claim_id: ClaimId,
    pub claim_generation: i64,
    pub target_branch: String,
    pub target_head: String,
    pub change_set_head: String,
    pub expires_at_micros: i64,
    pub status: String,
    pub execute: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrationConfirmation {
    pub preparation_id: String,
    pub status: String,
    pub integration: IntegrateChangeSetResult,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewWorkflowResult {
    pub review: CompleteReviewResult,
    pub independent_review: bool,
    pub assurance_flags: Vec<String>,
}

pub struct WorkflowService<C> {
    clock: C,
}

impl<C: Clock + Clone> WorkflowService<C> {
    pub fn new(clock: C) -> Self {
        Self { clock }
    }

    pub fn start(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        request: StartWorkflowRequest,
    ) -> Result<ClaimResult> {
        let StartWorkflowRequest {
            item_id,
            resources,
            inputs,
            lease_seconds,
            adopt_existing,
            request_id,
        } = request;
        let (item_version, plan_version): (i64, i64) = store
            .connection()
            .query_row(
                "SELECT w.version, t.plan_version
                 FROM work_items w JOIN tasks t ON t.id=w.task_id
                 WHERE w.id=?1 AND w.task_id=?2",
                params![item_id.to_string(), context.task_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| {
                SessionBridgeError::domain(ErrorCode::NotFound, "WorkItem was not found")
            })?;
        WorkService::new(self.clock.clone()).claim_with_existing_work_policy(
            store,
            context,
            ClaimWorkItemRequest {
                item_id,
                mode: ClaimMode::Execute,
                expected_item_version: item_version,
                observed_plan_version: plan_version,
                resources,
                inputs,
                lease_seconds,
                request_id,
            },
            adopt_existing,
        )
    }

    pub fn submit(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        item_id: WorkItemId,
        bundle: SubmitBundle,
        request_id: String,
    ) -> Result<SubmitWorkItemResult> {
        let claim: Option<(String, i64)> = store
            .connection()
            .query_row(
                "SELECT id, generation FROM work_claims
                 WHERE task_id=?1 AND work_item_id=?2 AND session_id=?3
                   AND mode='execute' AND closed_at IS NULL
                 ORDER BY claimed_at DESC LIMIT 1",
                params![
                    context.task_id.to_string(),
                    item_id.to_string(),
                    context.session_id.to_string(),
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (claim_id, generation) = claim.ok_or_else(|| {
            SessionBridgeError::domain(
                ErrorCode::SessionHasActiveWork,
                "work submit requires the Session's active execute Claim for this WorkItem",
            )
            .with_details(serde_json::json!({
                "work_item_id": item_id,
                "recovery_options": [
                    {"action": "work_start", "command": format!("sessionbridge work start {item_id}")},
                    {"action": "agent_next", "command": "sessionbridge agent next --format json"}
                ]
            }))
        })?;
        WorkService::new(self.clock.clone()).submit(
            store,
            context,
            SubmitWorkItemRequest {
                item_id,
                claim_id: claim_id.parse::<ClaimId>().map_err(|error| {
                    SessionBridgeError::domain(
                        ErrorCode::Database,
                        format!("stored Claim ID is invalid: {error}"),
                    )
                })?,
                generation,
                evidence: bundle.evidence,
                change_set: bundle.change_set,
                request_id,
            },
        )
    }

    pub fn prepare_integration(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        item_id: WorkItemId,
        lease_seconds: i64,
        request_id: String,
    ) -> Result<IntegrationPreparation> {
        if lease_seconds <= 0 {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "integration preparation lease must be positive",
            ));
        }
        let authenticated =
            AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)?;
        if let Some(existing) =
            load_preparation_by_request(store.connection(), context, &request_id)?
        {
            return Ok(existing);
        }
        let repository_path = authenticated
            .session
            .worktree_path
            .as_ref()
            .ok_or_else(|| {
                SessionBridgeError::domain(
                    ErrorCode::WorktreeConflict,
                    "integration preparation requires a registered Git worktree",
                )
            })?;
        let repository = GitRepository::open(repository_path)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let (item_version, plan_version, configured_target): (i64, i64, Option<String>) =
            transaction
                .query_row(
                    "SELECT w.version, t.plan_version, COALESCE(t.target_branch, s.target_branch)
                     FROM work_items w JOIN tasks t ON t.id=w.task_id
                     JOIN agent_sessions s ON s.id=?3 AND s.task_id=t.id
                     WHERE w.id=?1 AND w.task_id=?2",
                    params![
                        item_id.to_string(),
                        context.task_id.to_string(),
                        context.session_id.to_string(),
                    ],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?
                .ok_or_else(|| {
                    SessionBridgeError::domain(ErrorCode::NotFound, "WorkItem was not found")
                })?;
        let (change_set_id, change_set_head): (String, String) = transaction
            .query_row(
                "SELECT c.id, c.head_commit FROM change_sets c
                 JOIN work_attempts a ON a.id=c.attempt_id
                 WHERE a.work_item_id=?1 AND c.task_id=?2 AND c.integration_state='pending'
                 ORDER BY c.updated_at DESC LIMIT 1",
                params![item_id.to_string(), context.task_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| {
                SessionBridgeError::domain(
                    ErrorCode::ItemNotReady,
                    "integration preparation requires a pending ChangeSet",
                )
            })?;
        let target_branch = match configured_target {
            Some(branch) => branch,
            None => repository.current_branch()?.ok_or_else(|| {
                SessionBridgeError::domain(
                    ErrorCode::TargetBranchNotIntegrated,
                    "Task has no target branch and the registered worktree is detached",
                )
            })?,
        };
        let target_head = repository.resolve_revision(&target_branch)?;
        let claim = WorkService::new(self.clock.clone()).claim_in_transaction(
            &transaction,
            context,
            &authenticated,
            &ClaimWorkItemRequest {
                item_id,
                mode: ClaimMode::Integrate,
                expected_item_version: item_version,
                observed_plan_version: plan_version,
                resources: Vec::new(),
                inputs: Vec::new(),
                lease_seconds: Some(lease_seconds),
                request_id: request_id.clone(),
            },
            now,
            crate::app::work::ClaimTransactionOptions {
                adopt_existing: false,
                bump_revision: true,
            },
        )?;
        let preparation_id = Uuid::now_v7().to_string();
        let expires_at = now + Duration::seconds(lease_seconds);
        transaction.execute(
            "INSERT INTO integration_preparations (
                id, task_id, session_id, work_item_id, change_set_id, claim_id,
                claim_generation, target_branch, target_head_at_prepare, change_set_head,
                status, request_id, expires_at, created_at, updated_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'prepared',?11,?12,?13,?13)",
            params![
                preparation_id,
                context.task_id.to_string(),
                context.session_id.to_string(),
                item_id.to_string(),
                change_set_id,
                claim.claim_id.to_string(),
                claim.generation,
                target_branch,
                target_head,
                change_set_head,
                request_id,
                unix_micros(expires_at),
                unix_micros(now),
            ],
        )?;
        transaction.commit()?;
        let result = IntegrationPreparation {
            preparation_id: preparation_id.clone(),
            work_item_id: item_id,
            change_set_id: change_set_id.parse().map_err(invalid_stored_id)?,
            claim_id: claim.claim_id,
            claim_generation: claim.generation,
            target_branch: target_branch.clone(),
            target_head,
            change_set_head: change_set_head.clone(),
            expires_at_micros: unix_micros(expires_at),
            status: "prepared".into(),
            execute: serde_json::json!({
                "responsibility": "Agent performs Git integration outside the SQLite transaction",
                "target_branch": target_branch,
                "change_set_head": change_set_head,
                "confirm_command": format!("sessionbridge work integrate confirm {preparation_id}"),
            }),
        };
        reconcile_after_agent_write(store, context, self.clock.clone())?;
        Ok(result)
    }

    pub fn review(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        request: ReviewWorkflowRequest,
    ) -> Result<ReviewWorkflowResult> {
        let ReviewWorkflowRequest {
            item_id,
            decision,
            evidence_ids,
            lease_seconds,
            request_id,
        } = request;
        if request_id.trim().is_empty() {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "request ID cannot be empty",
            ));
        }
        let authenticated =
            AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let actor_id = authenticated.session.idempotency_actor_id.clone();
        let facade_key = IdempotencyKey {
            actor_type: "session",
            actor_id: &actor_id,
            request_id: &request_id,
        };
        let facade_hash = idempotency::hash_payload(&serde_json::json!({
            "operation": "workflow.review",
            "item_id": item_id,
            "decision": decision,
            "evidence_ids": evidence_ids,
            "lease_seconds": lease_seconds,
        }))?;
        if let Some(stored) =
            idempotency::replay::<ReviewWorkflowResult>(&transaction, facade_key, &facade_hash)?
        {
            transaction.commit()?;
            return Ok(stored.value);
        }
        let reviewer_executor_identity = authenticated.session.executor_identity.clone();
        let (item_version, plan_version, risk, review_policy, implementer, implementer_executor, executors): (
            i64,
            i64,
            String,
            String,
            Option<String>,
            Option<String>,
            i64,
        ) = transaction
            .query_row(
                "SELECT w.version,t.plan_version,w.risk,w.review_policy,
                    (SELECT a.session_id FROM work_attempts a
                     WHERE a.work_item_id=w.id AND a.status='submitted'
                     ORDER BY a.ended_at DESC LIMIT 1),
                    (SELECT COALESCE(s.executor_identity,s.id) FROM work_attempts a
                     JOIN agent_sessions s ON s.id=a.session_id
                     WHERE a.work_item_id=w.id AND a.status='submitted'
                     ORDER BY a.ended_at DESC LIMIT 1),
                    (SELECT count(DISTINCT COALESCE(s.executor_identity,s.id)) FROM task_participants p
                     JOIN agent_sessions s ON s.id=p.session_id
                     WHERE p.task_id=w.task_id AND p.completion_epoch=t.completion_epoch
                       AND p.kind='worker' AND p.state NOT IN ('retired','superseded'))
             FROM work_items w JOIN tasks t ON t.id=w.task_id
             WHERE w.id=?1 AND w.task_id=?2",
                params![item_id.to_string(), context.task_id.to_string()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| {
                SessionBridgeError::domain(ErrorCode::NotFound, "WorkItem was not found")
            })?;
        for evidence_id in &evidence_ids {
            let valid: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM evidence WHERE id=?1 AND task_id=?2)",
                params![evidence_id.to_string(), context.task_id.to_string()],
                |row| row.get(0),
            )?;
            if !valid {
                return Err(SessionBridgeError::domain(
                    ErrorCode::NotFound,
                    format!("Evidence {evidence_id} was not found in this Task"),
                ));
            }
        }
        let independent = implementer_executor.as_deref() != Some(&reviewer_executor_identity);
        let independent_required = review_policy == "independent_review_required"
            || (review_policy == "risk_based_review"
                && matches!(risk.as_str(), "high" | "critical"));
        if independent_required && executors > 1 && !independent {
            return Err(SessionBridgeError::domain(
                ErrorCode::ApprovalRequired,
                "high-risk WorkItem requires an independent reviewer while another Worker is available",
            ).with_details(serde_json::json!({
                "work_item_id": item_id,
                "implementer_session_id": implementer,
                "implementer_executor_identity": implementer_executor,
                "reviewer_executor_identity": reviewer_executor_identity,
                "review_policy": review_policy,
                "recovery_options": [
                    {"action":"request_independent_review","allowed":true},
                    {"action":"request_policy_exception","allowed":true},
                    {"action":"self_review","allowed":false,"reason":"another Worker is available"}
                ]
            })));
        }
        if review_policy == "capability_review_required" {
            let has_review_capability: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM capability_grants
                 WHERE task_id=?1 AND session_id=?2 AND capability='review'
                   AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at>?3))",
                params![
                    context.task_id.to_string(),
                    context.session_id.to_string(),
                    unix_micros(now)
                ],
                |row| row.get(0),
            )?;
            if !has_review_capability {
                return Err(SessionBridgeError::domain(
                    ErrorCode::CapabilityNotGranted,
                    "WorkItem review policy requires the review capability",
                )
                .with_details(serde_json::json!({
                    "work_item_id": item_id,
                    "review_policy": review_policy,
                    "required_capability": "review",
                    "recovery_options": [
                        {"action":"request_capability","capability":"review","safe":true},
                        {"action":"request_policy_exception","safe":true}
                    ]
                })));
            }
        }
        let service = WorkService::new(self.clock.clone());
        let claim = service.claim_in_transaction(
            &transaction,
            context,
            &authenticated,
            &ClaimWorkItemRequest {
                item_id,
                mode: ClaimMode::Review,
                expected_item_version: item_version,
                observed_plan_version: plan_version,
                resources: Vec::new(),
                inputs: Vec::new(),
                lease_seconds: Some(lease_seconds),
                request_id: format!("{request_id}:claim"),
            },
            now,
            crate::app::work::ClaimTransactionOptions {
                adopt_existing: false,
                bump_revision: true,
            },
        )?;
        let mut review_request = CompleteReviewRequest {
            item_id,
            claim_id: claim.claim_id,
            generation: claim.generation,
            decision,
            evidence_ids,
            request_id: format!("{request_id}:complete"),
        };
        review_request.evidence_ids.sort_by_key(ToString::to_string);
        review_request.evidence_ids.dedup();
        let review_hash = idempotency::hash_payload(&serde_json::json!({
            "operation": "review.complete",
            "request": review_request,
        }))?;
        let review = service.complete_review_in_transaction(
            &transaction,
            context,
            &authenticated,
            review_request,
            &review_hash,
            now,
        )?;
        let result = ReviewWorkflowResult {
            review,
            independent_review: independent,
            assurance_flags: if independent {
                Vec::new()
            } else {
                let mut flags = vec!["self_review".into()];
                if executors <= 1 {
                    flags.push("single_agent_assurance".into());
                }
                if review_policy == "self_review_allowed" {
                    flags.push("self_review_allowed_by_policy".into());
                }
                flags
            },
        };
        idempotency::record(
            &transaction,
            facade_key,
            Some(&context.task_id.to_string()),
            &facade_hash,
            &result,
            Some(result.review.event_sequence),
            now,
        )?;
        transaction.commit()?;
        reconcile_after_agent_write(store, context, self.clock.clone())?;
        Ok(result)
    }

    pub fn confirm_integration(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        preparation_id: &str,
        details: serde_json::Value,
        _request_id: String,
    ) -> Result<IntegrationConfirmation> {
        let preparation = load_preparation(store.connection(), context, preparation_id)?;
        if preparation.status == "confirmed" {
            return Ok(IntegrationConfirmation {
                preparation_id: preparation_id.to_owned(),
                status: "confirmed".into(),
                integration: load_integrated_result(store.connection(), context, &preparation)?,
            });
        }
        if preparation.status != "prepared" {
            return Err(SessionBridgeError::domain(
                ErrorCode::ItemNotReady,
                format!("integration preparation is already {}", preparation.status),
            ));
        }
        let already_integrated: bool = store.connection().query_row(
            "SELECT integration_state='integrated' FROM change_sets WHERE id=?1 AND task_id=?2",
            params![
                preparation.change_set_id.to_string(),
                context.task_id.to_string()
            ],
            |row| row.get(0),
        )?;
        if preparation.expires_at_micros <= unix_micros(self.clock.now()) && !already_integrated {
            self.expire_integration_preparation(store, context, &preparation)?;
            return Err(SessionBridgeError::domain(
                ErrorCode::ClaimExpired,
                "integration preparation has expired",
            ));
        }
        let integration = if already_integrated {
            load_integrated_result(store.connection(), context, &preparation)?
        } else {
            WorkService::new(self.clock.clone()).integrate_change_set(
                store,
                context,
                IntegrateChangeSetRequest {
                    change_set_id: preparation.change_set_id,
                    claim_id: preparation.claim_id,
                    generation: preparation.claim_generation,
                    outcome: IntegrationOutcome::Integrated,
                    details,
                    request_id: format!("integration-confirm:{preparation_id}"),
                },
            )?
        };
        let now = self.clock.now();
        store.connection().execute(
            "UPDATE integration_preparations
             SET status='confirmed', confirmed_at=?2, updated_at=?2
             WHERE id=?1 AND status='prepared'",
            params![preparation_id, unix_micros(now)],
        )?;
        Ok(IntegrationConfirmation {
            preparation_id: preparation_id.to_owned(),
            status: "confirmed".into(),
            integration,
        })
    }

    fn expire_integration_preparation(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        preparation: &IntegrationPreparation,
    ) -> Result<()> {
        let claim_open: bool = store.connection().query_row(
            "SELECT closed_at IS NULL FROM work_claims WHERE id=?1 AND task_id=?2",
            params![
                preparation.claim_id.to_string(),
                context.task_id.to_string()
            ],
            |row| row.get(0),
        )?;
        if claim_open {
            WorkService::new(self.clock.clone()).close(
                store,
                context,
                crate::app::work::CloseClaimRequest {
                    claim_id: preparation.claim_id,
                    generation: preparation.claim_generation,
                    action: crate::app::work::ClaimCloseAction::Release,
                    reason: Some("integration preparation lease expired".into()),
                    handoff: None,
                    request_id: format!("integration-expire:{}", preparation.preparation_id),
                },
            )?;
        }
        let now = self.clock.now();
        store.connection().execute(
            "UPDATE integration_preparations SET status='expired',updated_at=?2
             WHERE id=?1 AND status='prepared'",
            params![preparation.preparation_id, unix_micros(now)],
        )?;
        Ok(())
    }
}

fn load_preparation_by_request(
    connection: &rusqlite::Connection,
    context: &SessionContext,
    request_id: &str,
) -> Result<Option<IntegrationPreparation>> {
    let id: Option<String> = connection
        .query_row(
            "SELECT id FROM integration_preparations WHERE session_id=?1 AND request_id=?2",
            params![context.session_id.to_string(), request_id],
            |row| row.get(0),
        )
        .optional()?;
    id.map(|id| load_preparation(connection, context, &id))
        .transpose()
}

fn load_integrated_result(
    connection: &rusqlite::Connection,
    context: &SessionContext,
    preparation: &IntegrationPreparation,
) -> Result<IntegrateChangeSetResult> {
    connection
        .query_row(
            "SELECT c.integration_state,w.id,w.version,w.result_state,
                    COALESCE((SELECT MAX(e.sequence) FROM events e
                              WHERE e.task_id=c.task_id
                                AND e.event_type='changeset.integration_completed'
                                AND e.entity_type='change_set' AND e.entity_id=c.id),0)
             FROM change_sets c
             JOIN work_attempts a ON a.id=c.attempt_id
             JOIN work_items w ON w.id=a.work_item_id
             WHERE c.id=?1 AND c.task_id=?2 AND c.integration_state='integrated'",
            params![
                preparation.change_set_id.to_string(),
                context.task_id.to_string()
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .optional()?
        .map(
            |(integration_state, item_id, item_version, result_state, event_sequence)| {
                Ok::<IntegrateChangeSetResult, SessionBridgeError>(IntegrateChangeSetResult {
                    change_set_id: preparation.change_set_id,
                    integration_state,
                    item_id: item_id.parse().map_err(invalid_stored_id)?,
                    item_version,
                    result_state,
                    event_sequence,
                })
            },
        )
        .transpose()?
        .ok_or_else(|| {
            SessionBridgeError::domain(
                ErrorCode::ItemNotReady,
                "integration preparation has no integrated ChangeSet result to replay",
            )
        })
}

fn load_preparation(
    connection: &rusqlite::Connection,
    context: &SessionContext,
    preparation_id: &str,
) -> Result<IntegrationPreparation> {
    connection
        .query_row(
            "SELECT work_item_id,change_set_id,claim_id,claim_generation,target_branch,
                    target_head_at_prepare,change_set_head,expires_at,status
             FROM integration_preparations
             WHERE id=?1 AND task_id=?2 AND session_id=?3",
            params![
                preparation_id,
                context.task_id.to_string(),
                context.session_id.to_string(),
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, String>(8)?,
                ))
            },
        )
        .optional()?
        .map(|(item, change_set, claim, generation, branch, target, head, expires, status)| {
            Ok::<IntegrationPreparation, SessionBridgeError>(IntegrationPreparation {
                preparation_id: preparation_id.to_owned(),
                work_item_id: item.parse().map_err(invalid_stored_id)?,
                change_set_id: change_set.parse().map_err(invalid_stored_id)?,
                claim_id: claim.parse().map_err(invalid_stored_id)?,
                claim_generation: generation,
                target_branch: branch.clone(),
                target_head: target,
                change_set_head: head.clone(),
                expires_at_micros: expires,
                status,
                execute: serde_json::json!({
                    "target_branch": branch,
                    "change_set_head": head,
                    "confirm_command": format!("sessionbridge work integrate confirm {preparation_id}"),
                }),
            })
        })
        .transpose()?
        .ok_or_else(|| {
            SessionBridgeError::domain(
                ErrorCode::NotFound,
                "active integration preparation was not found",
            )
        })
}

fn invalid_stored_id(error: uuid::Error) -> SessionBridgeError {
    SessionBridgeError::domain(
        ErrorCode::Database,
        format!("stored integration preparation ID is invalid: {error}"),
    )
}
