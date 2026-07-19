use std::str::FromStr;

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::{
    Result,
    app::{
        chat::{ChatMessageView, ChatService},
        completion_service::{CompletionService, CompletionTrigger, TaskCompletionContext},
        deliverable::{DeliverableService, DeliverableView},
        task::{TaskDetails, TaskHealth, TaskService, WorkspaceBinding},
        task_reconciler::TaskReconciler,
    },
    clock::{Clock, SystemClock},
    domain::{TaskId, TaskPhase},
    error::{ErrorCode, SessionBridgeError},
    storage::SqliteStore,
    workspace::{LocatedWorkspace, RepositoryKind},
};

const DEFAULT_EVENT_LIMIT: usize = 200;
const MAX_EVENT_LIMIT: usize = 1_000;

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceView {
    pub project_id: String,
    pub name: String,
    pub root: String,
    pub repository_kind: RepositoryKind,
    pub repository_instance_id: String,
    pub database_path: String,
}

impl From<&LocatedWorkspace> for WorkspaceView {
    fn from(workspace: &LocatedWorkspace) -> Self {
        Self {
            project_id: workspace.config.project_id.to_string(),
            name: workspace.config.name.clone(),
            root: workspace.paths.root.display().to_string(),
            repository_kind: workspace.repository.kind,
            repository_instance_id: workspace.paths.repository_instance_id.clone(),
            database_path: workspace.paths.database_path.display().to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskSummary {
    pub id: String,
    pub title: String,
    pub phase: TaskPhase,
    pub autonomy_policy: String,
    pub target_branch: Option<String>,
    pub plan_version: i64,
    pub completion_epoch: i64,
    pub completion_revision: i64,
    pub created_at_micros: i64,
    pub updated_at_micros: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ParticipantView {
    pub id: String,
    pub session_id: String,
    pub label: String,
    pub kind: String,
    pub state: String,
    pub completion_epoch: i64,
    pub last_seen_at_micros: i64,
    pub worktree_path: Option<String>,
    pub branch: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InvitationView {
    pub id: String,
    pub batch_id: String,
    pub state: String,
    pub participant_kind: String,
    pub blocks_completion: bool,
    pub completion_epoch: i64,
    pub expires_at_micros: i64,
    pub consumed_by_session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkItemView {
    pub id: String,
    pub parent_id: Option<String>,
    pub kind: String,
    pub title: String,
    pub description: String,
    pub required: bool,
    pub priority: i64,
    pub risk: String,
    pub plan_state: String,
    pub result_state: String,
    pub version: i64,
    pub active_claim: Option<ClaimView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClaimView {
    pub id: String,
    pub session_id: String,
    pub mode: String,
    pub generation: i64,
    pub lease_expires_at_micros: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkRelationView {
    pub from_id: String,
    pub to_id: String,
    pub relation_type: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GapView {
    pub id: String,
    pub kind: String,
    pub severity: String,
    pub required: bool,
    pub status: String,
    pub title: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BlockerView {
    pub id: String,
    pub work_item_id: String,
    pub waits_for_type: String,
    pub waits_for_id: Option<String>,
    pub status: String,
    pub reason: String,
    pub resolution: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentAwaitView {
    pub id: String,
    pub participant_id: String,
    pub session_id: String,
    pub mode: String,
    pub status: String,
    pub created_at_micros: i64,
    pub completed_at_micros: Option<i64>,
    pub targets: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExternalAwaitView {
    pub id: String,
    pub participant_id: String,
    pub session_id: String,
    pub detector_type: String,
    pub detector: serde_json::Value,
    pub status: String,
    pub deadline_at_micros: Option<i64>,
    pub next_reassessment_at_micros: i64,
    pub last_observed_at_micros: Option<i64>,
    pub observation_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpeculativeWorkView {
    pub id: String,
    pub session_id: String,
    pub title: String,
    pub branch: String,
    pub base_commit: String,
    pub base_plan_version: i64,
    pub merge_ref: String,
    pub expected_merge_point: String,
    pub head_commit: Option<String>,
    pub status: String,
    pub summary: Option<String>,
    pub assumptions: serde_json::Value,
    pub updated_at_micros: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventView {
    pub sequence: i64,
    pub event_type: String,
    pub actor_type: String,
    pub actor_id: Option<String>,
    pub entity_type: String,
    pub entity_id: Option<String>,
    pub payload: serde_json::Value,
    pub created_at_micros: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct EventPage {
    pub events: Vec<EventView>,
    pub previous_cursor: i64,
    pub next_cursor: i64,
    pub has_more: bool,
}

struct RawEventRow {
    sequence: i64,
    event_type: String,
    actor_type: String,
    actor_id: Option<String>,
    entity_type: String,
    entity_id: Option<String>,
    payload_json: String,
    created_at_micros: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskDashboard {
    pub deliverables: Vec<DeliverableView>,
    pub details: TaskDetails,
    pub health: TaskHealth,
    pub participants: Vec<ParticipantView>,
    pub invitations: Vec<InvitationView>,
    pub work_items: Vec<WorkItemView>,
    pub relations: Vec<WorkRelationView>,
    pub gaps: Vec<GapView>,
    pub blockers: Vec<BlockerView>,
    pub waits: Vec<AgentAwaitView>,
    pub external_waits: Vec<ExternalAwaitView>,
    pub speculative_work: Vec<SpeculativeWorkView>,
    pub recent_events: EventPage,
    pub chat_messages: Vec<ChatMessageView>,
    pub pipeline: PipelineStatus,
}

#[derive(Debug, Clone, Serialize)]
pub struct PipelineStatus {
    pub required_deliverables_total: i64,
    pub required_deliverables_verified: i64,
    pub work_items_total: i64,
    pub work_items_accepted: i64,
    pub reviews_required: i64,
    pub reviews_approved: i64,
    pub integrations_required: i64,
    pub integrations_completed: i64,
    pub completion_blocker_count: i64,
}

pub struct OperatorService<C = SystemClock> {
    clock: C,
}

impl Default for OperatorService<SystemClock> {
    fn default() -> Self {
        Self::new(SystemClock)
    }
}

impl<C: Clock + Clone> OperatorService<C> {
    pub fn new(clock: C) -> Self {
        Self { clock }
    }

    pub fn open_store(&self, workspace: &LocatedWorkspace) -> Result<SqliteStore> {
        SqliteStore::open_with_lock(
            &workspace.paths.database_path,
            &workspace.paths.migration_lock_path,
        )
    }

    pub fn list_tasks(
        &self,
        connection: &Connection,
        workspace: &LocatedWorkspace,
    ) -> Result<Vec<TaskSummary>> {
        let mut statement = connection.prepare(
            "SELECT id, title, phase, autonomy_policy, target_branch, plan_version,
                    completion_epoch, completion_revision, created_at, updated_at
             FROM tasks WHERE workspace_id = ?1
             ORDER BY CASE phase WHEN 'active' THEN 0 WHEN 'paused' THEN 1
                         WHEN 'completed' THEN 2 WHEN 'cancelled' THEN 3 ELSE 4 END,
                      updated_at DESC, id",
        )?;
        let rows = statement.query_map([workspace.config.project_id.to_string()], |row| {
            let phase: String = row.get(2)?;
            let phase = TaskPhase::from_str(&phase).map_err(|_| rusqlite::Error::InvalidQuery)?;
            Ok(TaskSummary {
                id: row.get(0)?,
                title: row.get(1)?,
                phase,
                autonomy_policy: row.get(3)?,
                target_branch: row.get(4)?,
                plan_version: row.get(5)?,
                completion_epoch: row.get(6)?,
                completion_revision: row.get(7)?,
                created_at_micros: row.get(8)?,
                updated_at_micros: row.get(9)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn dashboard(
        &self,
        store: &mut SqliteStore,
        workspace: &LocatedWorkspace,
        task_id: TaskId,
    ) -> Result<TaskDashboard> {
        self.reconcile(store, workspace, task_id)?;
        let binding = WorkspaceBinding::from(workspace);
        let task_service = TaskService::new(self.clock.clone());
        let details = task_service.show(store.connection(), binding, task_id)?;
        let health = task_service.health(store.connection(), binding, task_id)?;
        let task_id_text = task_id.to_string();
        Ok(TaskDashboard {
            deliverables: DeliverableService::new(self.clock.clone()).list(
                store.connection(),
                binding,
                task_id,
            )?,
            participants: participants(store.connection(), &task_id_text)?,
            invitations: invitations(store.connection(), &task_id_text)?,
            work_items: work_items(store.connection(), &task_id_text)?,
            relations: relations(store.connection(), &task_id_text)?,
            gaps: gaps(store.connection(), &task_id_text)?,
            blockers: blockers(store.connection(), &task_id_text)?,
            waits: waits(store.connection(), &task_id_text)?,
            external_waits: external_waits(store.connection(), &task_id_text)?,
            speculative_work: speculative_work(store.connection(), &task_id_text)?,
            recent_events: events_since(
                store.connection(),
                &task_id_text,
                health
                    .event_cursor
                    .saturating_sub(DEFAULT_EVENT_LIMIT as i64),
                DEFAULT_EVENT_LIMIT,
            )?,
            chat_messages: ChatService::new(self.clock.clone()).recent(
                store.connection(),
                task_id,
                DEFAULT_EVENT_LIMIT,
            )?,
            pipeline: pipeline_status(store.connection(), &task_id_text, &health)?,
            details,
            health,
        })
    }

    pub fn events_since(
        &self,
        connection: &Connection,
        task_id: TaskId,
        cursor: i64,
        limit: usize,
    ) -> Result<EventPage> {
        if cursor < 0 {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "event cursor cannot be negative",
            ));
        }
        if limit == 0 || limit > MAX_EVENT_LIMIT {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                format!("event limit must be between 1 and {MAX_EVENT_LIMIT}"),
            ));
        }
        events_since(connection, &task_id.to_string(), cursor, limit)
    }

    pub fn reconcile(
        &self,
        store: &mut SqliteStore,
        workspace: &LocatedWorkspace,
        task_id: TaskId,
    ) -> Result<()> {
        TaskReconciler::new(self.clock.clone()).reconcile_workspace(store, workspace, task_id)?;
        CompletionService::new(self.clock.clone()).reconcile_task(
            store,
            TaskCompletionContext {
                workspace_id: workspace.config.project_id,
                repository_instance_id: &workspace.paths.repository_instance_id,
                task_id,
            },
            CompletionTrigger {
                actor_type: "system",
                actor_id: None,
            },
        )?;
        Ok(())
    }
}

fn speculative_work(connection: &Connection, task_id: &str) -> Result<Vec<SpeculativeWorkView>> {
    let mut statement=connection.prepare("SELECT id,session_id,title,branch,base_commit,base_plan_version,merge_ref,expected_merge_point,head_commit,status,summary,assumptions_json,updated_at FROM speculative_work WHERE task_id=?1 ORDER BY updated_at DESC,id")?;
    let rows = statement.query_map([task_id], |row| {
        let assumptions: String = row.get(11)?;
        Ok(SpeculativeWorkView {
            id: row.get(0)?,
            session_id: row.get(1)?,
            title: row.get(2)?,
            branch: row.get(3)?,
            base_commit: row.get(4)?,
            base_plan_version: row.get(5)?,
            merge_ref: row.get(6)?,
            expected_merge_point: row.get(7)?,
            head_commit: row.get(8)?,
            status: row.get(9)?,
            summary: row.get(10)?,
            assumptions: serde_json::from_str(&assumptions)
                .map_err(|_| rusqlite::Error::InvalidQuery)?,
            updated_at_micros: row.get(12)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn external_waits(connection: &Connection, task_id: &str) -> Result<Vec<ExternalAwaitView>> {
    let mut statement = connection.prepare(
        "SELECT id,participant_id,session_id,detector_type,detector_json,status,
                deadline_at,next_reassessment_at,last_observed_at,observation_count
         FROM external_waits WHERE task_id=?1 ORDER BY created_at DESC,id",
    )?;
    let rows = statement.query_map([task_id], |row| {
        let detector: String = row.get(4)?;
        Ok(ExternalAwaitView {
            id: row.get(0)?,
            participant_id: row.get(1)?,
            session_id: row.get(2)?,
            detector_type: row.get(3)?,
            detector: serde_json::from_str(&detector).map_err(|_| rusqlite::Error::InvalidQuery)?,
            status: row.get(5)?,
            deadline_at_micros: row.get(6)?,
            next_reassessment_at_micros: row.get(7)?,
            last_observed_at_micros: row.get(8)?,
            observation_count: row.get(9)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn pipeline_status(
    connection: &Connection,
    task_id: &str,
    health: &TaskHealth,
) -> Result<PipelineStatus> {
    let (
        required_deliverables_total,
        required_deliverables_verified,
        work_items_total,
        work_items_accepted,
        reviews_required,
        reviews_approved,
        integrations_required,
        integrations_completed,
    ): (i64, i64, i64, i64, i64, i64, i64, i64) = connection.query_row(
        "SELECT
            (SELECT COUNT(*) FROM deliverables WHERE task_id=?1 AND required=1),
            (SELECT COUNT(*) FROM deliverables WHERE task_id=?1 AND required=1 AND status IN ('verified','waived')),
            (SELECT COUNT(*) FROM work_items WHERE task_id=?1 AND plan_state='active'),
            (SELECT COUNT(*) FROM work_items WHERE task_id=?1 AND plan_state='active' AND result_state='accepted'),
            (SELECT COUNT(*) FROM work_items WHERE task_id=?1 AND plan_state='active' AND result_state IN ('submitted','accepted')),
            (SELECT COUNT(DISTINCT w.id) FROM work_items w JOIN review_results r ON r.work_item_id=w.id AND r.task_id=w.task_id AND r.decision='approved' WHERE w.task_id=?1 AND w.plan_state='active'),
            (SELECT COUNT(DISTINCT w.id) FROM work_items w JOIN work_attempts a ON a.work_item_id=w.id JOIN change_sets c ON c.attempt_id=a.id WHERE w.task_id=?1 AND w.plan_state='active'),
            (SELECT COUNT(DISTINCT w.id) FROM work_items w JOIN work_attempts a ON a.work_item_id=w.id JOIN change_sets c ON c.attempt_id=a.id AND c.integration_state='integrated' WHERE w.task_id=?1 AND w.plan_state='active')",
        [task_id],
        |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?)),
    )?;
    Ok(PipelineStatus {
        required_deliverables_total,
        required_deliverables_verified,
        work_items_total,
        work_items_accepted,
        reviews_required,
        reviews_approved,
        integrations_required,
        integrations_completed,
        completion_blocker_count: health
            .completion_signal_warnings
            .iter()
            .map(|warning| warning.count)
            .sum(),
    })
}

fn participants(connection: &Connection, task_id: &str) -> Result<Vec<ParticipantView>> {
    let mut statement = connection.prepare(
        "SELECT p.id, p.session_id, s.label, p.kind, p.state, p.completion_epoch,
                s.last_seen_at, s.worktree_path, s.branch, s.model
         FROM task_participants p
         JOIN agent_sessions s ON s.id = p.session_id AND s.task_id = p.task_id
         WHERE p.task_id = ?1 ORDER BY p.joined_at, p.id",
    )?;
    let rows = statement.query_map([task_id], |row| {
        Ok(ParticipantView {
            id: row.get(0)?,
            session_id: row.get(1)?,
            label: row.get(2)?,
            kind: row.get(3)?,
            state: row.get(4)?,
            completion_epoch: row.get(5)?,
            last_seen_at_micros: row.get(6)?,
            worktree_path: row.get(7)?,
            branch: row.get(8)?,
            model: row.get(9)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn invitations(connection: &Connection, task_id: &str) -> Result<Vec<InvitationView>> {
    let mut statement = connection.prepare(
        "SELECT id, batch_id, state, participant_kind, blocks_completion,
                completion_epoch, expires_at, consumed_by_session_id
         FROM agent_invitations WHERE task_id = ?1 ORDER BY created_at, id",
    )?;
    let rows = statement.query_map([task_id], |row| {
        Ok(InvitationView {
            id: row.get(0)?,
            batch_id: row.get(1)?,
            state: row.get(2)?,
            participant_kind: row.get(3)?,
            blocks_completion: row.get::<_, i64>(4)? != 0,
            completion_epoch: row.get(5)?,
            expires_at_micros: row.get(6)?,
            consumed_by_session_id: row.get(7)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn work_items(connection: &Connection, task_id: &str) -> Result<Vec<WorkItemView>> {
    let mut statement = connection.prepare(
        "SELECT w.id, w.parent_id, w.kind, w.title, w.description, w.required, w.priority,
                w.risk, w.plan_state, w.result_state, w.version,
                c.id, c.session_id, c.mode, c.generation, c.lease_expires_at
         FROM work_items w
         LEFT JOIN work_claims c ON c.work_item_id = w.id AND c.closed_at IS NULL
         WHERE w.task_id = ?1 ORDER BY w.priority DESC, w.created_at, w.id",
    )?;
    let rows = statement.query_map([task_id], |row| {
        let claim_id: Option<String> = row.get(11)?;
        let active_claim = match claim_id {
            Some(id) => Some(ClaimView {
                id,
                session_id: row.get(12)?,
                mode: row.get(13)?,
                generation: row.get(14)?,
                lease_expires_at_micros: row.get(15)?,
            }),
            None => None,
        };
        Ok(WorkItemView {
            id: row.get(0)?,
            parent_id: row.get(1)?,
            kind: row.get(2)?,
            title: row.get(3)?,
            description: row.get(4)?,
            required: row.get::<_, i64>(5)? != 0,
            priority: row.get(6)?,
            risk: row.get(7)?,
            plan_state: row.get(8)?,
            result_state: row.get(9)?,
            version: row.get(10)?,
            active_claim,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn relations(connection: &Connection, task_id: &str) -> Result<Vec<WorkRelationView>> {
    let mut statement = connection.prepare(
        "SELECT from_id, to_id, relation_type FROM work_relations
         WHERE task_id = ?1 ORDER BY created_at, id",
    )?;
    let rows = statement.query_map([task_id], |row| {
        Ok(WorkRelationView {
            from_id: row.get(0)?,
            to_id: row.get(1)?,
            relation_type: row.get(2)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn gaps(connection: &Connection, task_id: &str) -> Result<Vec<GapView>> {
    let mut statement = connection.prepare(
        "SELECT id, kind, severity, required, status, title, description
         FROM gaps WHERE task_id = ?1
         ORDER BY CASE severity WHEN 'critical' THEN 0 WHEN 'high' THEN 1
                   WHEN 'medium' THEN 2 ELSE 3 END, created_at, id",
    )?;
    let rows = statement.query_map([task_id], |row| {
        Ok(GapView {
            id: row.get(0)?,
            kind: row.get(1)?,
            severity: row.get(2)?,
            required: row.get::<_, i64>(3)? != 0,
            status: row.get(4)?,
            title: row.get(5)?,
            description: row.get(6)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn blockers(connection: &Connection, task_id: &str) -> Result<Vec<BlockerView>> {
    let mut statement = connection.prepare(
        "SELECT id, work_item_id, waits_for_type, waits_for_id, status, reason, resolution
         FROM blockers WHERE task_id = ?1 ORDER BY opened_at DESC, id",
    )?;
    let rows = statement.query_map([task_id], |row| {
        Ok(BlockerView {
            id: row.get(0)?,
            work_item_id: row.get(1)?,
            waits_for_type: row.get(2)?,
            waits_for_id: row.get(3)?,
            status: row.get(4)?,
            reason: row.get(5)?,
            resolution: row.get(6)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn waits(connection: &Connection, task_id: &str) -> Result<Vec<AgentAwaitView>> {
    let mut statement = connection.prepare(
        "SELECT id, participant_id, session_id, mode, status, created_at, completed_at
         FROM agent_waits WHERE task_id = ?1 ORDER BY created_at DESC, id",
    )?;
    let rows = statement.query_map([task_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, Option<i64>>(6)?,
        ))
    })?;
    let mut result = Vec::new();
    for row in rows {
        let (id, participant_id, session_id, mode, status, created_at_micros, completed_at_micros) =
            row?;
        let targets = connection
            .prepare(
                "SELECT work_item_id FROM agent_wait_targets WHERE wait_id = ?1 ORDER BY ordinal",
            )?
            .query_map([&id], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        result.push(AgentAwaitView {
            id,
            participant_id,
            session_id,
            mode,
            status,
            created_at_micros,
            completed_at_micros,
            targets,
        });
    }
    Ok(result)
}

fn events_since(
    connection: &Connection,
    task_id: &str,
    cursor: i64,
    limit: usize,
) -> Result<EventPage> {
    let mut statement = connection.prepare(
        "SELECT sequence, event_type, actor_type, actor_id, entity_type, entity_id,
                payload_json, created_at
         FROM events WHERE task_id = ?1 AND sequence > ?2
         ORDER BY sequence LIMIT ?3",
    )?;
    let rows = statement.query_map(params![task_id, cursor, (limit + 1) as i64], |row| {
        Ok(RawEventRow {
            sequence: row.get(0)?,
            event_type: row.get(1)?,
            actor_type: row.get(2)?,
            actor_id: row.get(3)?,
            entity_type: row.get(4)?,
            entity_id: row.get(5)?,
            payload_json: row.get(6)?,
            created_at_micros: row.get(7)?,
        })
    })?;
    let raw_events = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    let mut events = raw_events
        .into_iter()
        .map(|row| {
            Ok(EventView {
                sequence: row.sequence,
                event_type: row.event_type,
                actor_type: row.actor_type,
                actor_id: row.actor_id,
                entity_type: row.entity_type,
                entity_id: row.entity_id,
                payload: serde_json::from_str(&row.payload_json)?,
                created_at_micros: row.created_at_micros,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let has_more = events.len() > limit;
    events.truncate(limit);
    let next_cursor = events.last().map_or(cursor, |event| event.sequence);
    Ok(EventPage {
        events,
        previous_cursor: cursor,
        next_cursor,
        has_more,
    })
}
