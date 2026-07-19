use std::{
    collections::{BTreeSet, HashMap, HashSet},
    str::FromStr,
};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    Result,
    app::{
        agent::AgentService,
        events::{self, EventActor, EventEntity},
        idempotency::{self, IdempotencyKey},
    },
    clock::{Clock, unix_micros},
    domain::{ParticipantState, SessionContext, TaskPhase, WorkItemId},
    error::{ErrorCode, SessionBridgeError},
    storage::SqliteStore,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum AwaitMode {
    All,
    Any,
}

impl AwaitMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Any => "any",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterAwaitRequest {
    pub work_item_ids: Vec<WorkItemId>,
    pub mode: AwaitMode,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwaitTargetView {
    pub ordinal: i64,
    pub work_item_id: WorkItemId,
    pub status: String,
    pub terminal_state: Option<String>,
    pub triggering_event_sequence: Option<i64>,
    pub result: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwaitResult {
    pub wait_id: String,
    pub outcome: String,
    pub mode: AwaitMode,
    pub participant_state: ParticipantState,
    pub trigger_event_cursor: i64,
    pub completed_targets: Vec<AwaitTargetView>,
    pub pending_targets: Vec<AwaitTargetView>,
    pub next_action: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwaitView {
    pub wait_id: String,
    pub task_id: String,
    pub participant_id: String,
    pub session_id: String,
    pub mode: AwaitMode,
    pub status: String,
    pub created_event_cursor: i64,
    pub completion_event_cursor: Option<i64>,
    pub result: Option<AwaitResult>,
    pub created_at_micros: i64,
    pub completed_at_micros: Option<i64>,
    pub targets: Vec<AwaitTargetView>,
}

pub struct AwaitService<C> {
    clock: C,
}

impl<C: Clock + Clone> AwaitService<C> {
    pub fn new(clock: C) -> Self {
        Self { clock }
    }

    pub fn register(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut request: RegisterAwaitRequest,
    ) -> Result<AwaitView> {
        validate_request_id(&request.request_id)?;
        request.work_item_ids.sort_by_key(ToString::to_string);
        request.work_item_ids.dedup();
        if request.work_item_ids.is_empty() {
            return Err(invalid(
                ErrorCode::WaitTargetInvalid,
                "await requires at least one WorkItem",
            ));
        }
        let authenticated =
            AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)?;
        let payload_hash = idempotency::hash_payload(&request)?;
        let actor_id = authenticated.session.idempotency_actor_id.clone();
        let now = self.clock.now();
        let now_micros = unix_micros(now);
        let transaction = store.begin_immediate()?;
        let key = IdempotencyKey {
            actor_type: "session",
            actor_id: &actor_id,
            request_id: &request.request_id,
        };
        if let Some(stored) = idempotency::replay::<AwaitView>(&transaction, key, &payload_hash)? {
            let current = load_wait(&transaction, &stored.value.wait_id)?;
            transaction.commit()?;
            return Ok(current);
        }
        if authenticated.participant.state != ParticipantState::Active {
            return Err(invalid(
                ErrorCode::WaitNotAllowed,
                "only an active worker can start await",
            ));
        }
        require_clear_work(&transaction, context)?;
        let current_wait: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_waits WHERE participant_id = ?1 AND status = 'active')",
            [authenticated.participant.participant_id.to_string()], |row| row.get(0))?;
        if current_wait {
            return Err(invalid(
                ErrorCode::WaitAlreadyActive,
                "participant already has an active await",
            ));
        }

        let mut target_rows = Vec::new();
        let mut owners = BTreeSet::new();
        for item_id in &request.work_item_ids {
            let row = load_target(&transaction, context, *item_id, now_micros)?;
            if row.plan_state != "active" && row.result_state != "accepted" {
                target_rows.push((row, "impossible"));
                continue;
            }
            if row.result_state == "accepted" {
                target_rows.push((row, "satisfied"));
                continue;
            }
            let owner = row.owner_session.as_deref().ok_or_else(|| {
                invalid(
                    ErrorCode::WaitTargetUnowned,
                    format!("WorkItem {item_id} has no active owner and must remain available for coordination work"),
                )
                .with_details(serde_json::json!({
                    "work_item_id": item_id,
                    "recovery_options": [
                        {"action": "inspect", "command": format!("sessionbridge item inspect {item_id}")},
                        {"action": "claim_available_stage", "allowed": true},
                        {"action": "propose_alternative_work", "allowed": true},
                        {"action": "await", "allowed": false, "reason": "target has no active owner"}
                    ]
                }))
            })?;
            if owner == context.session_id.to_string() {
                return Err(invalid(
                    ErrorCode::WaitTargetSelfOwned,
                    format!("cannot await self-owned WorkItem {item_id}"),
                ));
            }
            owners.insert(owner.to_owned());
            target_rows.push((row, "pending"));
        }
        detect_cycle(&transaction, &context.session_id.to_string(), &owners)?;

        let wait_id = Uuid::now_v7().to_string();
        let created_cursor =
            events::latest_cursor(&transaction, Some(&context.task_id.to_string()))?;
        transaction.execute(
            "INSERT INTO agent_waits (id, task_id, participant_id, session_id, mode, status,
                created_event_cursor, created_at, request_id)
             VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?6, ?7, ?8)",
            params![
                wait_id,
                context.task_id.to_string(),
                authenticated.participant.participant_id.to_string(),
                context.session_id.to_string(),
                request.mode.as_str(),
                created_cursor,
                now_micros,
                request.request_id
            ],
        )?;
        for (ordinal, (target, status)) in target_rows.iter().enumerate() {
            transaction.execute(
                "INSERT INTO agent_wait_targets (wait_id, ordinal, work_item_id, status, terminal_state, result_json, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![wait_id, ordinal as i64, target.id.to_string(), status,
                    if *status == "pending" { None } else { Some(target.terminal_state()) },
                    if *status == "satisfied" { Some(serde_json::to_string(&target_callback(&transaction, target.id, None)?)?) } else { None },
                    now_micros],
            )?;
        }
        transaction.execute(
            "UPDATE task_participants SET state = 'waiting', version = version + 1, updated_at = ?2
             WHERE id = ?1 AND state = 'active' AND version = ?3",
            params![
                authenticated.participant.participant_id.to_string(),
                now_micros,
                authenticated.participant.version
            ],
        )?;
        let event_sequence = events::append(
            &transaction,
            context.workspace_id,
            Some(&context.task_id.to_string()),
            "agent.await_registered",
            EventActor {
                kind: "session",
                id: Some(&context.session_id.to_string()),
            },
            EventEntity {
                kind: "agent_wait",
                id: Some(&wait_id),
            },
            &serde_json::json!({"mode": request.mode, "work_item_ids": request.work_item_ids}),
            now,
        )?;
        reconcile_wait(
            &transaction,
            context.workspace_id,
            &wait_id,
            event_sequence,
            now,
        )?;
        let result = load_wait(&transaction, &wait_id)?;
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
        Ok(result)
    }

    pub fn show(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        wait_id: &str,
    ) -> Result<AwaitView> {
        AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)?;
        let view = load_wait(store.connection(), wait_id)?;
        if view.session_id != context.session_id.to_string() {
            return Err(invalid(
                ErrorCode::WaitSessionMismatch,
                "await belongs to another Session",
            ));
        }
        Ok(view)
    }

    pub fn list(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
    ) -> Result<Vec<AwaitView>> {
        AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)?;
        let mut statement = store.connection().prepare(
            "SELECT id FROM agent_waits WHERE session_id = ?1 AND task_id = ?2 ORDER BY created_at DESC")?;
        let ids = statement
            .query_map(
                params![context.session_id.to_string(), context.task_id.to_string()],
                |row| row.get::<_, String>(0),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ids.iter()
            .map(|id| load_wait(store.connection(), id))
            .collect()
    }

    pub fn poll(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        wait_id: &str,
    ) -> Result<AwaitView> {
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let owner: Option<String> = transaction
            .query_row(
                "SELECT session_id FROM agent_waits WHERE id = ?1",
                [wait_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(owner) = owner else {
            return Err(invalid(ErrorCode::WaitNotFound, "await was not found"));
        };
        if owner != context.session_id.to_string() {
            return Err(invalid(
                ErrorCode::WaitSessionMismatch,
                "await belongs to another Session",
            ));
        }
        let cursor = events::latest_cursor(&transaction, Some(&context.task_id.to_string()))?;
        refresh_target_states(&transaction, wait_id, cursor, now)?;
        reconcile_wait(&transaction, context.workspace_id, wait_id, cursor, now)?;
        let view = load_wait(&transaction, wait_id)?;
        transaction.commit()?;
        Ok(view)
    }

    pub fn touch(&self, store: &mut SqliteStore, context: &SessionContext) -> Result<()> {
        AgentService::new(self.clock.clone()).whoami(store, context)?;
        Ok(())
    }
}

pub fn resolve_accepted_work_item(
    transaction: &Transaction<'_>,
    context: &SessionContext,
    item_id: WorkItemId,
    trigger_event: i64,
    completed_by_session_id: &str,
    now: time::OffsetDateTime,
) -> Result<()> {
    let callback = target_callback(transaction, item_id, Some(completed_by_session_id))?;
    let wait_ids = transaction.prepare(
        "SELECT DISTINCT wait_id FROM agent_wait_targets WHERE work_item_id = ?1 AND status = 'pending'")?
        .query_map([item_id.to_string()], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for wait_id in wait_ids {
        transaction.execute(
            "UPDATE agent_wait_targets SET status = 'satisfied', terminal_state = 'accepted',
                triggering_event_sequence = ?2, result_json = ?3, updated_at = ?4
             WHERE wait_id = ?1 AND work_item_id = ?5 AND status = 'pending'",
            params![
                wait_id,
                trigger_event,
                serde_json::to_string(&callback)?,
                unix_micros(now),
                item_id.to_string()
            ],
        )?;
        events::append(
            transaction,
            context.workspace_id,
            Some(&context.task_id.to_string()),
            "agent.await_target_satisfied",
            EventActor {
                kind: "session",
                id: Some(completed_by_session_id),
            },
            EventEntity {
                kind: "agent_wait",
                id: Some(&wait_id),
            },
            &serde_json::json!({"work_item_id": item_id, "trigger_event_sequence": trigger_event}),
            now,
        )?;
        reconcile_wait(
            transaction,
            context.workspace_id,
            &wait_id,
            trigger_event,
            now,
        )?;
    }
    Ok(())
}

pub fn resolve_task_terminal(
    connection: &Connection,
    workspace_id: crate::domain::WorkspaceId,
    task_id: crate::domain::TaskId,
    phase: TaskPhase,
    trigger_event: i64,
    now: time::OffsetDateTime,
) -> Result<()> {
    let wait_ids = connection
        .prepare("SELECT id FROM agent_waits WHERE task_id = ?1 AND status = 'active'")?
        .query_map([task_id.to_string()], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for wait_id in wait_ids {
        finish_wait(
            connection,
            workspace_id,
            &wait_id,
            "task_terminal",
            trigger_event,
            serde_json::json!({"task_id": task_id, "phase": phase}),
            now,
        )?;
    }
    Ok(())
}

pub fn resolve_participant_retired(
    connection: &Connection,
    context: &SessionContext,
    participant_id: &str,
    trigger_event: i64,
    now: time::OffsetDateTime,
) -> Result<()> {
    let wait_id: Option<String> = connection
        .query_row(
            "SELECT id FROM agent_waits WHERE participant_id = ?1 AND status = 'active'",
            [participant_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(wait_id) = wait_id else {
        return Ok(());
    };
    let view = load_wait(connection, &wait_id)?;
    let result = AwaitResult {
        wait_id: wait_id.clone(),
        outcome: "participant_retired".into(),
        mode: view.mode,
        participant_state: ParticipantState::Retired,
        trigger_event_cursor: trigger_event,
        completed_targets: view
            .targets
            .iter()
            .filter(|target| target.status == "satisfied")
            .cloned()
            .collect(),
        pending_targets: view
            .targets
            .iter()
            .filter(|target| target.status == "pending")
            .cloned()
            .collect(),
        next_action: serde_json::json!({"action": "exit"}),
    };
    let event_sequence = events::append(
        connection,
        context.workspace_id,
        Some(&context.task_id.to_string()),
        "agent.await_task_terminal",
        EventActor {
            kind: "system",
            id: None,
        },
        EventEntity {
            kind: "agent_wait",
            id: Some(&wait_id),
        },
        &serde_json::json!({"outcome": "participant_retired", "participant_id": participant_id}),
        now,
    )?;
    connection.execute(
        "UPDATE agent_waits SET status = 'participant_retired', result_json = ?2,
            completion_event_cursor = ?3, completed_at = ?4 WHERE id = ?1 AND status = 'active'",
        params![
            wait_id,
            serde_json::to_string(&result)?,
            event_sequence,
            unix_micros(now)
        ],
    )?;
    Ok(())
}

#[derive(Debug)]
struct TargetRow {
    id: WorkItemId,
    plan_state: String,
    result_state: String,
    owner_session: Option<String>,
}
impl TargetRow {
    fn terminal_state(&self) -> &str {
        if self.result_state == "accepted" {
            "accepted"
        } else {
            &self.plan_state
        }
    }
}

fn load_target(
    connection: &Connection,
    context: &SessionContext,
    item_id: WorkItemId,
    now_micros: i64,
) -> Result<TargetRow> {
    connection.execute(
        "UPDATE stage_handoffs SET status='expired',updated_at=?1
         WHERE status='reserved' AND expires_at<=?1",
        [now_micros],
    )?;
    connection
        .query_row(
            "SELECT w.id, w.plan_state, w.result_state,
                COALESCE((SELECT c.session_id FROM work_claims c
                 WHERE c.work_item_id = w.id AND c.closed_at IS NULL AND c.lease_expires_at > ?3
                 ORDER BY c.claimed_at DESC LIMIT 1),
                (SELECT h.coordinator_session_id FROM stage_handoffs h
                 WHERE h.work_item_id=w.id AND h.status='reserved' AND h.expires_at>?3
                 ORDER BY h.created_at DESC LIMIT 1))
         FROM work_items w WHERE w.id = ?1 AND w.task_id = ?2",
            params![item_id.to_string(), context.task_id.to_string(), now_micros],
            |row| {
                Ok(TargetRow {
                    id: WorkItemId::from_str(&row.get::<_, String>(0)?)
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    plan_state: row.get(1)?,
                    result_state: row.get(2)?,
                    owner_session: row.get(3)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| {
            invalid(
                ErrorCode::WaitTargetInvalid,
                format!("WorkItem {item_id} was not found in this Task"),
            )
        })
}

fn require_clear_work(connection: &Connection, context: &SessionContext) -> Result<()> {
    let active: i64 = connection.query_row(
        "SELECT
          (SELECT count(*) FROM work_claims WHERE task_id = ?1 AND session_id = ?2 AND closed_at IS NULL) +
          (SELECT count(*) FROM work_attempts WHERE task_id = ?1 AND session_id = ?2 AND status = 'active') +
          (SELECT count(*) FROM agent_intents WHERE task_id = ?1 AND session_id = ?2 AND cleared_at IS NULL AND kind IN ('plan','implement'))",
        params![context.task_id.to_string(), context.session_id.to_string()], |row| row.get(0))?;
    if active != 0 {
        return Err(invalid(
            ErrorCode::SessionHasActiveWork,
            "cannot await while Session-owned work remains",
        ));
    }
    Ok(())
}

fn detect_cycle(connection: &Connection, waiter: &str, owners: &BTreeSet<String>) -> Result<()> {
    let mut graph: HashMap<String, Vec<String>> = HashMap::new();
    let mut statement = connection.prepare(
        "SELECT w.session_id,
                COALESCE((SELECT c.session_id FROM work_claims c
                          WHERE c.work_item_id=t.work_item_id AND c.closed_at IS NULL
                          ORDER BY c.claimed_at DESC LIMIT 1),
                         (SELECT h.coordinator_session_id FROM stage_handoffs h
                          WHERE h.work_item_id=t.work_item_id AND h.status='reserved'
                          ORDER BY h.created_at DESC LIMIT 1))
         FROM agent_waits w
         JOIN agent_wait_targets t ON t.wait_id = w.id AND t.status = 'pending'
         WHERE w.status = 'active'
           AND (EXISTS(SELECT 1 FROM work_claims c WHERE c.work_item_id=t.work_item_id AND c.closed_at IS NULL)
                OR EXISTS(SELECT 1 FROM stage_handoffs h WHERE h.work_item_id=t.work_item_id AND h.status='reserved'))",
    )?;
    for row in statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })? {
        let (from, to) = row?;
        graph.entry(from).or_default().push(to);
    }
    graph.insert(waiter.to_owned(), owners.iter().cloned().collect());
    fn reaches(
        graph: &HashMap<String, Vec<String>>,
        current: &str,
        target: &str,
        seen: &mut HashSet<String>,
    ) -> bool {
        if current == target {
            return true;
        }
        if !seen.insert(current.to_owned()) {
            return false;
        }
        graph
            .get(current)
            .is_some_and(|next| next.iter().any(|node| reaches(graph, node, target, seen)))
    }
    for owner in owners {
        if reaches(&graph, owner, waiter, &mut HashSet::new()) {
            return Err(invalid(
                ErrorCode::WaitCycleDetected,
                "await would create a participant dependency cycle",
            )
            .with_details(
                serde_json::json!({"waiter_session_id": waiter, "owner_session_id": owner}),
            ));
        }
    }
    Ok(())
}

fn refresh_target_states(
    connection: &Connection,
    wait_id: &str,
    cursor: i64,
    now: time::OffsetDateTime,
) -> Result<()> {
    connection.execute(
        "UPDATE stage_handoffs SET status='expired',updated_at=?1
         WHERE status='reserved' AND expires_at<=?1",
        [unix_micros(now)],
    )?;
    let targets = connection
        .prepare(
            "SELECT t.work_item_id, w.plan_state, w.result_state,
                    (EXISTS(SELECT 1 FROM work_claims c
                           WHERE c.work_item_id = w.id AND c.closed_at IS NULL
                             AND c.lease_expires_at > ?2)
                     OR EXISTS(SELECT 1 FROM stage_handoffs h
                           WHERE h.work_item_id=w.id AND h.status='reserved'
                             AND h.expires_at>?2))
             FROM agent_wait_targets t
             JOIN work_items w ON w.id = t.work_item_id
             WHERE t.wait_id = ?1 AND t.status = 'pending'",
        )?
        .query_map(params![wait_id, unix_micros(now)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, bool>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (item, plan, result, has_owner) in targets {
        let (status, terminal) = if result == "accepted" {
            ("satisfied", "accepted")
        } else if matches!(plan.as_str(), "cancelled" | "superseded") {
            ("impossible", plan.as_str())
        } else if !has_owner {
            ("impossible", "unowned")
        } else {
            continue;
        };
        let callback = if status == "satisfied" {
            Some(serde_json::to_string(&target_callback(
                connection,
                WorkItemId::from_str(&item).map_err(|_| rusqlite::Error::InvalidQuery)?,
                None,
            )?)?)
        } else {
            None
        };
        connection.execute(
            "UPDATE agent_wait_targets SET status = ?3, terminal_state = ?4, triggering_event_sequence = ?5,
                result_json = ?6, updated_at = ?7 WHERE wait_id = ?1 AND work_item_id = ?2 AND status = 'pending'",
            params![wait_id, item, status, terminal, cursor, callback, unix_micros(now)])?;
    }
    Ok(())
}

fn reconcile_wait(
    connection: &Connection,
    workspace_id: crate::domain::WorkspaceId,
    wait_id: &str,
    trigger: i64,
    now: time::OffsetDateTime,
) -> Result<()> {
    let row: Option<(String, String)> = connection
        .query_row(
            "SELECT mode, status FROM agent_waits WHERE id = ?1",
            [wait_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((mode, status)) = row else {
        return Err(invalid(ErrorCode::WaitNotFound, "await was not found"));
    };
    if status != "active" {
        return Ok(());
    }
    let (pending, satisfied, impossible): (i64, i64, i64) = connection.query_row(
        "SELECT sum(status = 'pending'), sum(status = 'satisfied'), sum(status = 'impossible')
         FROM agent_wait_targets WHERE wait_id = ?1",
        [wait_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if (mode == "all" && impossible > 0)
        || (mode == "any" && pending == 0 && satisfied == 0 && impossible > 0)
    {
        return finish_wait(
            connection,
            workspace_id,
            wait_id,
            "dependency_failed",
            trigger,
            serde_json::json!({"reason": "dependency_unavailable"}),
            now,
        );
    }
    let complete =
        (mode == "any" && satisfied > 0) || (mode == "all" && pending == 0 && impossible == 0);
    if complete {
        if mode == "any" {
            connection.execute("UPDATE agent_wait_targets SET status = 'not_required', updated_at = ?2 WHERE wait_id = ?1 AND status = 'pending'", params![wait_id, unix_micros(now)])?;
        }
        finish_wait(
            connection,
            workspace_id,
            wait_id,
            "satisfied",
            trigger,
            serde_json::json!({}),
            now,
        )?;
    }
    Ok(())
}

fn finish_wait(
    connection: &Connection,
    workspace_id: crate::domain::WorkspaceId,
    wait_id: &str,
    outcome: &str,
    trigger: i64,
    extra: serde_json::Value,
    now: time::OffsetDateTime,
) -> Result<()> {
    let row: Option<(String, String, String, String)> = connection.query_row(
        "SELECT task_id, participant_id, session_id, mode FROM agent_waits WHERE id = ?1 AND status = 'active'",
        [wait_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).optional()?;
    let Some((task_id, participant_id, session_id, mode_text)) = row else {
        return Ok(());
    };
    let mode = if mode_text == "all" {
        AwaitMode::All
    } else {
        AwaitMode::Any
    };
    connection.execute("UPDATE task_participants SET state = 'active', version = version + 1, updated_at = ?2 WHERE id = ?1 AND state = 'waiting'", params![participant_id, unix_micros(now)])?;
    let targets = load_targets(connection, wait_id)?;
    let result = AwaitResult {
        wait_id: wait_id.to_owned(),
        outcome: outcome.to_owned(),
        mode,
        participant_state: ParticipantState::Active,
        trigger_event_cursor: trigger,
        completed_targets: targets
            .iter()
            .filter(|target| target.status == "satisfied")
            .cloned()
            .collect(),
        pending_targets: targets
            .iter()
            .filter(|target| target.status == "pending")
            .cloned()
            .collect(),
        next_action: serde_json::json!({
            "command": "sessionbridge agent next --format json",
            "trigger_event_cursor": trigger,
        }),
    };
    let event_sequence = events::append(
        connection,
        workspace_id,
        Some(&task_id),
        match outcome {
            "satisfied" => "agent.await_satisfied",
            "dependency_failed" => "agent.await_dependency_failed",
            _ => "agent.await_task_terminal",
        },
        EventActor {
            kind: "system",
            id: None,
        },
        EventEntity {
            kind: "agent_wait",
            id: Some(wait_id),
        },
        &serde_json::json!({"outcome": outcome, "session_id": session_id, "details": extra}),
        now,
    )?;
    let result_json = serde_json::to_string(&result)?;
    connection.execute(
        "UPDATE agent_waits SET status = ?2, result_json = ?3, completion_event_cursor = ?4,
            completed_at = ?5 WHERE id = ?1 AND status = 'active'",
        params![
            wait_id,
            outcome,
            result_json,
            event_sequence,
            unix_micros(now)
        ],
    )?;
    connection.execute(
        "INSERT OR IGNORE INTO await_delivery_outbox (
            id, wait_id, task_id, session_id, payload_json, status, created_at
         ) VALUES (?1,?2,?3,?4,?5,'pending',?6)",
        params![
            uuid::Uuid::now_v7().to_string(),
            wait_id,
            task_id,
            session_id,
            result_json,
            unix_micros(now),
        ],
    )?;
    Ok(())
}

fn target_callback(
    connection: &Connection,
    item_id: WorkItemId,
    completed_by: Option<&str>,
) -> Result<serde_json::Value> {
    let (version, result_state): (i64, String) = connection.query_row(
        "SELECT version, result_state FROM work_items WHERE id = ?1",
        [item_id.to_string()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let review_id: Option<String> = connection.query_row("SELECT id FROM review_results WHERE work_item_id = ?1 AND decision = 'approved' ORDER BY created_at DESC LIMIT 1", [item_id.to_string()], |row| row.get(0)).optional()?;
    let change_set_id: Option<String> = connection.query_row("SELECT c.id FROM change_sets c JOIN work_attempts a ON a.id = c.attempt_id WHERE a.work_item_id = ?1 AND c.integration_state = 'integrated' ORDER BY c.updated_at DESC LIMIT 1", [item_id.to_string()], |row| row.get(0)).optional()?;
    let evidence_ids = connection.prepare("SELECT e.id FROM evidence e JOIN work_attempts a ON a.id = e.attempt_id WHERE a.work_item_id = ?1 ORDER BY e.created_at")?
        .query_map([item_id.to_string()], |row| row.get::<_, String>(0))?.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(
        serde_json::json!({"work_item_id": item_id, "terminal_state": result_state,
        "completed_by_session_id": completed_by, "work_item_version": version,
        "review_id": review_id, "change_set_id": change_set_id, "evidence_ids": evidence_ids}),
    )
}

fn load_wait(connection: &Connection, wait_id: &str) -> Result<AwaitView> {
    let row = connection
        .query_row(
            "SELECT task_id, participant_id, session_id, mode, status, created_event_cursor,
                completion_event_cursor, result_json, created_at, completed_at
         FROM agent_waits WHERE id = ?1",
            [wait_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, Option<i64>>(9)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| invalid(ErrorCode::WaitNotFound, "await was not found"))?;
    Ok(AwaitView {
        wait_id: wait_id.to_owned(),
        task_id: row.0,
        participant_id: row.1,
        session_id: row.2,
        mode: if row.3 == "all" {
            AwaitMode::All
        } else {
            AwaitMode::Any
        },
        status: row.4,
        created_event_cursor: row.5,
        completion_event_cursor: row.6,
        result: row
            .7
            .map(|value| serde_json::from_str(&value))
            .transpose()?,
        created_at_micros: row.8,
        completed_at_micros: row.9,
        targets: load_targets(connection, wait_id)?,
    })
}

fn load_targets(connection: &Connection, wait_id: &str) -> Result<Vec<AwaitTargetView>> {
    let mut statement = connection.prepare("SELECT ordinal, work_item_id, status, terminal_state, triggering_event_sequence, result_json FROM agent_wait_targets WHERE wait_id = ?1 ORDER BY ordinal")?;
    statement
        .query_map([wait_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?
        .map(|row| {
            let row = row?;
            Ok(AwaitTargetView {
                ordinal: row.0,
                work_item_id: WorkItemId::from_str(&row.1)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                status: row.2,
                terminal_state: row.3,
                triggering_event_sequence: row.4,
                result: row
                    .5
                    .map(|value| serde_json::from_str(&value))
                    .transpose()?,
            })
        })
        .collect()
}

fn validate_request_id(value: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > 200 {
        return Err(invalid(
            ErrorCode::InvalidInput,
            "request ID must be 1-200 characters",
        ));
    }
    Ok(())
}
fn invalid(code: ErrorCode, message: impl Into<String>) -> SessionBridgeError {
    SessionBridgeError::domain(code, message)
}

#[cfg(test)]
mod tests {
    use rusqlite::{Connection, params};
    use time::OffsetDateTime;

    use crate::{
        app::events,
        domain::{ParticipantId, SessionId, TaskId, WorkspaceId},
        storage::migrations::MIGRATIONS,
    };

    use super::*;

    #[test]
    fn accepted_work_item_satisfies_wait_and_reactivates_participant_atomically() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
        MIGRATIONS.to_latest(&mut connection).unwrap();
        let workspace_id = WorkspaceId::new();
        let task_id = TaskId::new();
        let waiter_session = SessionId::new();
        let worker_session = SessionId::new();
        let participant_id = ParticipantId::new();
        let item_id = WorkItemId::new();
        let wait_id = Uuid::now_v7().to_string();
        connection.execute(
            "INSERT INTO workspaces (id, name, repository_identity, created_at) VALUES (?1, 'test', 'repo', 1)",
            [workspace_id.to_string()],
        ).unwrap();
        connection.execute(
            "INSERT INTO tasks (id, workspace_id, title, phase, autonomy_policy, completion_policy, created_at, updated_at)
             VALUES (?1, ?2, 'task', 'active', 'bounded', 'all_workers_concluded', 1, 1)",
            params![task_id.to_string(), workspace_id.to_string()],
        ).unwrap();
        for (session, token) in [
            (waiter_session, vec![1_u8; 32]),
            (worker_session, vec![2_u8; 32]),
        ] {
            connection.execute(
                "INSERT INTO agent_sessions (id, task_id, token_hash, label, protocol_version, last_seen_at, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 'agent', 1, 1, 1, 1)",
                params![session.to_string(), task_id.to_string(), token],
            ).unwrap();
        }
        connection.execute(
            "INSERT INTO task_participants (id, task_id, session_id, kind, completion_epoch, state, version, joined_at, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'worker', 1, 'waiting', 1, 1, 1, 1)",
            params![participant_id.to_string(), task_id.to_string(), waiter_session.to_string()],
        ).unwrap();
        connection.execute(
            "INSERT INTO work_items (id, task_id, kind, title, risk, plan_state, result_state, version, created_at, updated_at, accepted_at)
             VALUES (?1, ?2, 'implementation', 'upstream', 'low', 'active', 'accepted', 2, 1, 2, 2)",
            params![item_id.to_string(), task_id.to_string()],
        ).unwrap();
        connection.execute(
            "INSERT INTO agent_waits (id, task_id, participant_id, session_id, mode, status, created_event_cursor, created_at, request_id)
             VALUES (?1, ?2, ?3, ?4, 'all', 'active', 0, 1, 'request')",
            params![wait_id, task_id.to_string(), participant_id.to_string(), waiter_session.to_string()],
        ).unwrap();
        connection.execute(
            "INSERT INTO agent_wait_targets (wait_id, ordinal, work_item_id, status, updated_at)
             VALUES (?1, 0, ?2, 'pending', 1)",
            params![wait_id, item_id.to_string()],
        ).unwrap();

        let context = SessionContext {
            schema_version: 1,
            protocol_version: 1,
            workspace_id,
            repository_instance_id: "repo".into(),
            task_id,
            session_id: worker_session,
            token: "unused".into(),
            database_path: "/tmp/unused.db".into(),
        };
        let transaction = connection.transaction().unwrap();
        let trigger = events::append(
            &transaction,
            workspace_id,
            Some(&task_id.to_string()),
            "changeset.integration_completed",
            EventActor {
                kind: "session",
                id: Some(&worker_session.to_string()),
            },
            EventEntity {
                kind: "work_item",
                id: Some(&item_id.to_string()),
            },
            &serde_json::json!({"result_state": "accepted"}),
            OffsetDateTime::UNIX_EPOCH,
        )
        .unwrap();
        resolve_accepted_work_item(
            &transaction,
            &context,
            item_id,
            trigger,
            &worker_session.to_string(),
            OffsetDateTime::UNIX_EPOCH,
        )
        .unwrap();
        transaction.commit().unwrap();

        let status: String = connection
            .query_row(
                "SELECT status FROM agent_waits WHERE id = ?1",
                [&wait_id],
                |row| row.get(0),
            )
            .unwrap();
        let participant_state: String = connection
            .query_row(
                "SELECT state FROM task_participants WHERE id = ?1",
                [participant_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "satisfied");
        assert_eq!(participant_state, "active");
        let result: String = connection
            .query_row(
                "SELECT result_json FROM agent_waits WHERE id = ?1",
                [&wait_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<AwaitResult>(&result)
                .unwrap()
                .outcome,
            "satisfied"
        );
        let outbox: (String, String) = connection
            .query_row(
                "SELECT status,payload_json FROM await_delivery_outbox WHERE wait_id=?1",
                [&wait_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(outbox.0, "pending");
        assert_eq!(
            serde_json::from_str::<AwaitResult>(&outbox.1)
                .unwrap()
                .outcome,
            "satisfied"
        );

        let first_any = WorkItemId::new();
        let second_any = WorkItemId::new();
        for item in [first_any, second_any] {
            connection.execute(
                "INSERT INTO work_items (id, task_id, kind, title, risk, plan_state, result_state, version, created_at, updated_at, accepted_at)
                 VALUES (?1, ?2, 'implementation', 'any target', 'low', 'active', 'accepted', 1, 1, 2, 2)",
                params![item.to_string(), task_id.to_string()],
            ).unwrap();
        }
        let any_wait = Uuid::now_v7().to_string();
        connection
            .execute(
                "UPDATE task_participants SET state = 'waiting' WHERE id = ?1",
                [participant_id.to_string()],
            )
            .unwrap();
        connection.execute(
            "INSERT INTO agent_waits (id, task_id, participant_id, session_id, mode, status, created_event_cursor, created_at, request_id)
             VALUES (?1, ?2, ?3, ?4, 'any', 'active', 0, 1, 'any-request')",
            params![any_wait, task_id.to_string(), participant_id.to_string(), waiter_session.to_string()],
        ).unwrap();
        for (ordinal, item) in [first_any, second_any].into_iter().enumerate() {
            connection.execute(
                "INSERT INTO agent_wait_targets (wait_id, ordinal, work_item_id, status, updated_at) VALUES (?1, ?2, ?3, 'pending', 1)",
                params![any_wait, ordinal as i64, item.to_string()],
            ).unwrap();
        }
        let transaction = connection.transaction().unwrap();
        let trigger = events::append(
            &transaction,
            workspace_id,
            Some(&task_id.to_string()),
            "changeset.integration_completed",
            EventActor {
                kind: "session",
                id: Some(&worker_session.to_string()),
            },
            EventEntity {
                kind: "work_item",
                id: Some(&first_any.to_string()),
            },
            &serde_json::json!({"result_state": "accepted"}),
            OffsetDateTime::UNIX_EPOCH,
        )
        .unwrap();
        resolve_accepted_work_item(
            &transaction,
            &context,
            first_any,
            trigger,
            &worker_session.to_string(),
            OffsetDateTime::UNIX_EPOCH,
        )
        .unwrap();
        transaction.commit().unwrap();
        let statuses = connection
            .prepare("SELECT status FROM agent_wait_targets WHERE wait_id = ?1 ORDER BY ordinal")
            .unwrap()
            .query_map([&any_wait], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(statuses, vec!["satisfied", "not_required"]);
        let status: String = connection
            .query_row(
                "SELECT status FROM agent_waits WHERE id = ?1",
                [&any_wait],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "satisfied");
    }
}
