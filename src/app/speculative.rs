use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{
    Result,
    app::{
        agent::{ActionProposalResult, AgentService, ProposeActionInput},
        events::{self, EventActor, EventEntity},
    },
    clock::{Clock, unix_micros},
    domain::SessionContext,
    error::{ErrorCode, SessionBridgeError},
    git::GitRepository,
    storage::SqliteStore,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartSpeculativeRequest {
    pub title: String,
    pub branch: String,
    pub merge_ref: String,
    #[serde(default)]
    pub assumptions: Vec<String>,
    #[serde(default)]
    pub provenance: serde_json::Value,
    #[serde(default)]
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointSpeculativeRequest {
    pub head_commit: String,
    pub summary: String,
    #[serde(default)]
    pub provenance: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeculativeView {
    pub id: String,
    pub title: String,
    pub branch: String,
    pub base_commit: String,
    pub base_plan_version: i64,
    pub merge_ref: String,
    pub expected_merge_point: String,
    pub head_commit: Option<String>,
    pub status: String,
    pub summary: Option<String>,
    pub assumptions: Vec<String>,
    pub provenance: serde_json::Value,
    pub recovery_options: Vec<serde_json::Value>,
}

pub struct SpeculativeService<C> {
    clock: C,
}
impl<C: Clock + Clone> SpeculativeService<C> {
    pub fn new(clock: C) -> Self {
        Self { clock }
    }
    pub fn start(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut request: StartSpeculativeRequest,
    ) -> Result<SpeculativeView> {
        request.title = request.title.trim().into();
        request.branch = request.branch.trim().into();
        request.merge_ref = request.merge_ref.trim().into();
        if request.title.is_empty()
            || request.branch.is_empty()
            || request.merge_ref.is_empty()
            || request.request_id.trim().is_empty()
            || !request.provenance.is_object()
        {
            return Err(invalid(
                "speculative work requires title, branch, merge_ref, request ID, and object provenance",
            ));
        }
        let auth = AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)?;
        if let Some(id) = store
            .connection()
            .query_row(
                "SELECT id FROM speculative_work WHERE session_id=?1 AND request_id=?2",
                params![context.session_id.to_string(), request.request_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            return load(store.connection(), &id);
        }
        let path = auth
            .session
            .worktree_path
            .as_ref()
            .ok_or_else(|| invalid("speculative work requires a registered Git worktree"))?;
        let repo = GitRepository::open(path)?;
        let base_commit = repo.resolve_revision(&request.branch)?;
        let expected_merge_point = repo.resolve_revision(&request.merge_ref)?;
        let now = self.clock.now();
        let tx = store.begin_immediate()?;
        let plan_version: i64 = tx.query_row(
            "SELECT plan_version FROM tasks WHERE id=?1 AND phase='active'",
            [context.task_id.to_string()],
            |row| row.get(0),
        )?;
        let id = uuid::Uuid::now_v7().to_string();
        tx.execute("INSERT INTO speculative_work(id,task_id,session_id,title,branch,base_commit,base_plan_version,expected_merge_point,merge_ref,assumptions_json,status,provenance_json,request_id,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'active',?11,?12,?13,?13)",params![id,context.task_id.to_string(),context.session_id.to_string(),request.title,request.branch,base_commit,plan_version,expected_merge_point,request.merge_ref,serde_json::to_string(&request.assumptions)?,serde_json::to_string(&request.provenance)?,request.request_id,unix_micros(now)])?;
        events::append(
            &tx,
            context.workspace_id,
            Some(&context.task_id.to_string()),
            "speculative.started",
            EventActor {
                kind: "session",
                id: Some(&context.session_id.to_string()),
            },
            EventEntity {
                kind: "speculative_work",
                id: Some(&id),
            },
            &serde_json::json!({"branch":request.branch,"base_commit":base_commit,"merge_ref":request.merge_ref,"shared_reservation":false}),
            now,
        )?;
        tx.commit()?;
        load(store.connection(), &id)
    }
    pub fn checkpoint(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        id: &str,
        request: CheckpointSpeculativeRequest,
    ) -> Result<SpeculativeView> {
        if request.summary.trim().is_empty() || !request.provenance.is_object() {
            return Err(invalid(
                "speculative checkpoint requires summary and object provenance",
            ));
        }
        let auth = AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)?;
        let current = load(store.connection(), id)?;
        let path = auth
            .session
            .worktree_path
            .as_ref()
            .ok_or_else(|| invalid("speculative work requires a registered Git worktree"))?;
        let repo = GitRepository::open(path)?;
        let head = repo.resolve_revision(&request.head_commit)?;
        repo.require_ancestor(&current.base_commit, &head)?;
        let merge_point = repo.resolve_revision(&current.merge_ref)?;
        let now = self.clock.now();
        let tx = store.begin_immediate()?;
        let plan: i64 = tx.query_row(
            "SELECT plan_version FROM tasks WHERE id=?1",
            [context.task_id.to_string()],
            |row| row.get(0),
        )?;
        let status =
            if plan == current.base_plan_version && merge_point == current.expected_merge_point {
                "ready_for_adoption"
            } else {
                "revalidation_required"
            };
        tx.execute("UPDATE speculative_work SET head_commit=?2,status=?3,summary=?4,provenance_json=?5,updated_at=?6 WHERE id=?1 AND task_id=?7 AND session_id=?8",params![id,head,status,request.summary.trim(),serde_json::to_string(&request.provenance)?,unix_micros(now),context.task_id.to_string(),context.session_id.to_string()])?;
        events::append(
            &tx,
            context.workspace_id,
            Some(&context.task_id.to_string()),
            "speculative.checkpointed",
            EventActor {
                kind: "session",
                id: Some(&context.session_id.to_string()),
            },
            EventEntity {
                kind: "speculative_work",
                id: Some(id),
            },
            &serde_json::json!({"head_commit":head,"status":status,"current_plan_version":plan,"current_merge_point":merge_point}),
            now,
        )?;
        tx.commit()?;
        load(store.connection(), id)
    }
    pub fn propose_adoption(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        id: &str,
        request_id: String,
    ) -> Result<ActionProposalResult> {
        let view = load(store.connection(), id)?;
        if view.status != "ready_for_adoption" {
            return Err(SessionBridgeError::domain(
                ErrorCode::ItemNotReady,
                "speculative work must be ready_for_adoption before proposing adoption",
            ));
        }
        let result=AgentService::new(self.clock.clone()).propose_action(store,context,"speculative_adoption".into(),ProposeActionInput{summary:format!("Adopt speculative result: {}",view.title),rationale:view.summary.clone().unwrap_or_else(||"isolated result is ready for formal evaluation".into()),payload:serde_json::json!({"speculative_work_id":id,"branch":view.branch,"base_commit":view.base_commit,"head_commit":view.head_commit,"merge_ref":view.merge_ref,"assumptions":view.assumptions}),dedupe_key:Some(format!("speculative-adoption:{id}")),priority:"high".into(),safety_relevant:false,source_entities:vec![id.into()],provenance:view.provenance},request_id)?;
        store.connection().execute(
            "UPDATE speculative_work SET adoption_proposal_id=?2,updated_at=?3 WHERE id=?1",
            params![id, &result.proposal_id, unix_micros(self.clock.now())],
        )?;
        Ok(result)
    }
    pub fn abandon(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        id: &str,
        reason: &str,
    ) -> Result<SpeculativeView> {
        if reason.trim().is_empty() {
            return Err(invalid("speculative abandon reason is required"));
        }
        let auth = AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)?;
        let now = self.clock.now();
        let tx = store.begin_immediate()?;
        let changed=tx.execute("UPDATE speculative_work SET status='abandoned',summary=?4,updated_at=?5 WHERE id=?1 AND task_id=?2 AND session_id=?3 AND status IN ('active','ready_for_adoption','revalidation_required')",params![id,context.task_id.to_string(),context.session_id.to_string(),reason.trim(),unix_micros(now)])?;
        if changed != 1 {
            return Err(SessionBridgeError::domain(
                ErrorCode::ItemNotReady,
                "speculative work is not active or is owned by another Session",
            ));
        }
        events::append(
            &tx,
            context.workspace_id,
            Some(&context.task_id.to_string()),
            "speculative.abandoned",
            EventActor {
                kind: "session",
                id: Some(&auth.session.session_id.to_string()),
            },
            EventEntity {
                kind: "speculative_work",
                id: Some(id),
            },
            &serde_json::json!({"reason":reason}),
            now,
        )?;
        tx.commit()?;
        load(store.connection(), id)
    }
}

fn load(c: &rusqlite::Connection, id: &str) -> Result<SpeculativeView> {
    let row = c
        .query_row(
            "SELECT title,branch,base_commit,base_plan_version,merge_ref,expected_merge_point,
                head_commit,status,summary,assumptions_json,provenance_json
         FROM speculative_work WHERE id=?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, String>(7)?,
                    r.get::<_, Option<String>>(8)?,
                    r.get::<_, String>(9)?,
                    r.get::<_, String>(10)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| {
            SessionBridgeError::domain(ErrorCode::NotFound, "speculative work was not found")
        })?;
    let recovery_options = if row.7 == "revalidation_required" {
        vec![
            serde_json::json!({"action":"rebase"}),
            serde_json::json!({"action":"revalidate_assumptions"}),
            serde_json::json!({"action":"propose_alternative"}),
        ]
    } else {
        vec![
            serde_json::json!({"action":"propose_adoption"}),
            serde_json::json!({"action":"continue_isolated_validation"}),
        ]
    };
    Ok(SpeculativeView {
        id: id.into(),
        title: row.0,
        branch: row.1,
        base_commit: row.2,
        base_plan_version: row.3,
        merge_ref: row.4,
        expected_merge_point: row.5,
        head_commit: row.6,
        status: row.7,
        summary: row.8,
        assumptions: serde_json::from_str(&row.9)?,
        provenance: serde_json::from_str(&row.10)?,
        recovery_options,
    })
}
fn invalid(message: &str) -> SessionBridgeError {
    SessionBridgeError::domain(ErrorCode::InvalidInput, message)
}
