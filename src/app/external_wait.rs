use std::path::PathBuf;

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::Duration;
use uuid::Uuid;

use crate::{
    Result,
    app::{
        agent::AgentService,
        events::{self, EventActor, EventEntity},
    },
    clock::{Clock, unix_micros},
    domain::{ParticipantState, SessionContext},
    error::{ErrorCode, SessionBridgeError},
    storage::SqliteStore,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalDetector {
    #[serde(rename = "type")]
    pub detector_type: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterExternalWaitRequest {
    pub detector: ExternalDetector,
    pub deadline_seconds: Option<i64>,
    pub reassess_interval_seconds: i64,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalWaitView {
    pub wait_id: String,
    pub status: String,
    pub detector: ExternalDetector,
    pub deadline_at_micros: Option<i64>,
    pub next_reassessment_at_micros: i64,
    pub last_observed_at_micros: Option<i64>,
    pub observation_count: i64,
    pub result: Option<serde_json::Value>,
}

pub struct ExternalWaitService<C> {
    clock: C,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalScanResult {
    pub scanned: usize,
    pub completed: usize,
    pub next_due_at_micros: Option<i64>,
}

impl<C: Clock + Clone> ExternalWaitService<C> {
    pub fn new(clock: C) -> Self {
        Self { clock }
    }

    pub fn register(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        request: RegisterExternalWaitRequest,
    ) -> Result<ExternalWaitView> {
        validate(&request)?;
        let authenticated =
            AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let request_hash = format!("sha256:{:x}", Sha256::digest(serde_json::to_vec(&request)?));
        if let Some((id, stored_hash)) = transaction
            .query_row(
                "SELECT id,request_hash FROM external_waits WHERE session_id=?1 AND request_id=?2",
                params![context.session_id.to_string(), request.request_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        {
            let view = load(&transaction, &id)?;
            if stored_hash != request_hash {
                return Err(domain(
                    ErrorCode::IdempotencyKeyReused,
                    "external wait request ID was reused with different parameters",
                ));
            }
            transaction.commit()?;
            return Ok(view);
        }
        if authenticated.participant.state != ParticipantState::Active {
            return Err(domain(
                ErrorCode::WaitNotAllowed,
                "only an active worker can start external wait",
            ));
        }
        let owned_work: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM work_claims WHERE session_id=?1 AND task_id=?2 AND closed_at IS NULL)
                 OR EXISTS(SELECT 1 FROM agent_waits WHERE session_id=?1 AND task_id=?2 AND status='active')
                 OR EXISTS(SELECT 1 FROM external_waits WHERE session_id=?1 AND task_id=?2 AND status='active')",
            params![context.session_id.to_string(), context.task_id.to_string()],
            |row| row.get(0),
        )?;
        if owned_work {
            return Err(domain(
                ErrorCode::SessionHasActiveWork,
                "cannot start external wait while Session-owned Claim remains",
            ));
        }
        let id = Uuid::now_v7().to_string();
        let deadline = request
            .deadline_seconds
            .map(|seconds| now + Duration::seconds(seconds));
        let next = now + Duration::seconds(request.reassess_interval_seconds);
        transaction.execute(
            "INSERT INTO external_waits (id,task_id,participant_id,session_id,detector_type,
                detector_json,status,deadline_at,reassess_interval_seconds,next_reassessment_at,
                request_id,request_hash,created_at,updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,'active',?7,?8,?9,?10,?11,?12,?12)",
            params![
                id,
                context.task_id.to_string(),
                authenticated.participant.participant_id.to_string(),
                context.session_id.to_string(),
                request.detector.detector_type,
                serde_json::to_string(&request.detector)?,
                deadline.map(unix_micros),
                request.reassess_interval_seconds,
                unix_micros(next),
                request.request_id,
                request_hash,
                unix_micros(now),
            ],
        )?;
        transaction.execute(
            "UPDATE task_participants SET state='waiting',version=version+1,updated_at=?2
             WHERE id=?1 AND state='active'",
            params![
                authenticated.participant.participant_id.to_string(),
                unix_micros(now)
            ],
        )?;
        events::append(
            &transaction,
            context.workspace_id,
            Some(&context.task_id.to_string()),
            "agent.external_wait_registered",
            EventActor {
                kind: "session",
                id: Some(&context.session_id.to_string()),
            },
            EventEntity {
                kind: "external_wait",
                id: Some(&id),
            },
            &serde_json::json!({"detector":request.detector,"deadline_at_micros":deadline.map(unix_micros)}),
            now,
        )?;
        let view = load(&transaction, &id)?;
        transaction.commit()?;
        Ok(view)
    }

    pub fn poll(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        id: &str,
    ) -> Result<ExternalWaitView> {
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let mut view = load(&transaction, id)?;
        let owner: String = transaction.query_row(
            "SELECT session_id FROM external_waits WHERE id=?1",
            [id],
            |row| row.get(0),
        )?;
        if owner != context.session_id.to_string() {
            return Err(domain(
                ErrorCode::WaitSessionMismatch,
                "external wait belongs to another Session",
            ));
        }
        if view.status != "active" || unix_micros(now) < view.next_reassessment_at_micros {
            transaction.commit()?;
            return Ok(view);
        }
        let observation = match std::fs::metadata(&view.detector.path) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.to_string()),
        };
        let satisfied = match (view.detector.detector_type.as_str(), &observation) {
            ("file_exists", Ok(exists)) => *exists,
            ("file_absent", Ok(exists)) => !*exists,
            _ => {
                if !matches!(
                    view.detector.detector_type.as_str(),
                    "file_exists" | "file_absent"
                ) {
                    return Err(domain(
                        ErrorCode::InvalidInput,
                        "stored external detector type is invalid",
                    ));
                }
                false
            }
        };
        let deadline_reached = view
            .deadline_at_micros
            .is_some_and(|deadline| unix_micros(now) >= deadline);
        let status = if observation.is_err() {
            Some("detector_failed")
        } else if satisfied {
            Some("satisfied")
        } else if deadline_reached {
            Some("deadline_reached")
        } else {
            None
        };
        let result = serde_json::json!({"satisfied":satisfied,"path":view.detector.path,"observation_error":observation.err(),"observed_at_micros":unix_micros(now)});
        transaction.execute(
            "UPDATE external_waits SET status=COALESCE(?2,status),last_observed_at=?3,
                observation_count=observation_count+1,result_json=?4,
                next_reassessment_at=?5,completed_at=CASE WHEN ?2 IS NULL THEN NULL ELSE ?3 END,updated_at=?3
             WHERE id=?1 AND status='active'",
            params![id,status,unix_micros(now),serde_json::to_string(&result)?,unix_micros(now + Duration::seconds(reassessment_interval(&transaction,id)?))],
        )?;
        if let Some(status) = status {
            transaction.execute(
                "UPDATE task_participants SET state='active',version=version+1,updated_at=?2
                 WHERE id=(SELECT participant_id FROM external_waits WHERE id=?1) AND state='waiting'",
                params![id,unix_micros(now)],
            )?;
            events::append(
                &transaction,
                context.workspace_id,
                Some(&context.task_id.to_string()),
                "agent.external_wait_completed",
                EventActor {
                    kind: "system",
                    id: None,
                },
                EventEntity {
                    kind: "external_wait",
                    id: Some(id),
                },
                &serde_json::json!({"status":status,"result":result}),
                now,
            )?;
        }
        view = load(&transaction, id)?;
        if view.status != "active" {
            transaction.execute(
                "INSERT OR IGNORE INTO external_wait_delivery_outbox
                    (id,wait_id,task_id,session_id,payload_json,status,created_at)
                 VALUES (?1,?2,?3,?4,?5,'pending',?6)",
                params![
                    Uuid::now_v7().to_string(),
                    id,
                    context.task_id.to_string(),
                    context.session_id.to_string(),
                    serde_json::to_string(&view)?,
                    unix_micros(now),
                ],
            )?;
        }
        transaction.commit()?;
        Ok(view)
    }

    pub fn scan_due(&self, store: &mut SqliteStore, limit: usize) -> Result<ExternalScanResult> {
        if limit == 0 || limit > 10_000 {
            return Err(domain(
                ErrorCode::InvalidInput,
                "external scan limit must be between 1 and 10000",
            ));
        }
        let now_micros = unix_micros(self.clock.now());
        let rows=store.connection().prepare("SELECT ew.id,t.workspace_id,ew.task_id,ew.session_id FROM external_waits ew JOIN tasks t ON t.id=ew.task_id WHERE ew.status='active' AND ew.next_reassessment_at<=?1 ORDER BY ew.next_reassessment_at,ew.id LIMIT ?2")?.query_map(params![now_micros,limit as i64],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
        let mut completed = 0;
        for (id, workspace, task, session) in &rows {
            let context = SessionContext {
                schema_version: 1,
                protocol_version: 1,
                workspace_id: workspace
                    .parse()
                    .map_err(|_| domain(ErrorCode::Database, "stored workspace ID is invalid"))?,
                repository_instance_id: String::new(),
                task_id: task
                    .parse()
                    .map_err(|_| domain(ErrorCode::Database, "stored task ID is invalid"))?,
                session_id: session
                    .parse()
                    .map_err(|_| domain(ErrorCode::Database, "stored session ID is invalid"))?,
                token: String::new(),
                database_path: String::new(),
            };
            if self.poll(store, &context, id)?.status != "active" {
                completed += 1;
            }
        }
        let next_due_at_micros = store.connection().query_row(
            "SELECT MIN(next_reassessment_at) FROM external_waits WHERE status='active'",
            [],
            |row| row.get(0),
        )?;
        Ok(ExternalScanResult {
            scanned: rows.len(),
            completed,
            next_due_at_micros,
        })
    }
}

pub fn resolve_task_terminal(
    connection: &rusqlite::Connection,
    workspace_id: crate::domain::WorkspaceId,
    task_id: crate::domain::TaskId,
    phase: crate::domain::TaskPhase,
    now: time::OffsetDateTime,
) -> Result<()> {
    let waits = connection
        .prepare("SELECT id FROM external_waits WHERE task_id=?1 AND status='active'")?
        .query_map([task_id.to_string()], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for id in waits {
        let result = serde_json::json!({
            "reason": "task_terminal",
            "task_phase": phase,
            "completed_at_micros": unix_micros(now)
        });
        connection.execute(
            "UPDATE external_waits SET status='cancelled',result_json=?2,
                completed_at=?3,updated_at=?3 WHERE id=?1 AND status='active'",
            params![id, serde_json::to_string(&result)?, unix_micros(now)],
        )?;
        let owner: String = connection.query_row(
            "SELECT session_id FROM external_waits WHERE id=?1",
            [&id],
            |row| row.get(0),
        )?;
        connection.execute(
            "INSERT OR IGNORE INTO external_wait_delivery_outbox
                (id,wait_id,task_id,session_id,payload_json,status,created_at)
             VALUES (?1,?2,?3,?4,?5,'pending',?6)",
            params![
                Uuid::now_v7().to_string(),
                id,
                task_id.to_string(),
                owner,
                serde_json::to_string(&result)?,
                unix_micros(now),
            ],
        )?;
        events::append(
            connection,
            workspace_id,
            Some(&task_id.to_string()),
            "agent.external_wait_completed",
            EventActor {
                kind: "system",
                id: None,
            },
            EventEntity {
                kind: "external_wait",
                id: Some(&id),
            },
            &result,
            now,
        )?;
    }
    Ok(())
}

pub fn resolve_participant_retired(
    connection: &rusqlite::Connection,
    context: &SessionContext,
    participant_id: &str,
    now: time::OffsetDateTime,
) -> Result<()> {
    let row: Option<(String, String)> = connection
        .query_row(
            "SELECT id,session_id FROM external_waits WHERE participant_id=?1 AND status='active'",
            [participant_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((id, session_id)) = row else {
        return Ok(());
    };
    let result = serde_json::json!({"reason":"participant_retired","participant_id":participant_id,"completed_at_micros":unix_micros(now)});
    connection.execute("UPDATE external_waits SET status='cancelled',result_json=?2,completed_at=?3,updated_at=?3 WHERE id=?1 AND status='active'",params![id,serde_json::to_string(&result)?,unix_micros(now)])?;
    events::append(
        connection,
        context.workspace_id,
        Some(&context.task_id.to_string()),
        "agent.external_wait_completed",
        EventActor {
            kind: "system",
            id: None,
        },
        EventEntity {
            kind: "external_wait",
            id: Some(&id),
        },
        &result,
        now,
    )?;
    connection.execute("INSERT OR IGNORE INTO external_wait_delivery_outbox(id,wait_id,task_id,session_id,payload_json,status,created_at) VALUES(?1,?2,?3,?4,?5,'pending',?6)",params![Uuid::now_v7().to_string(),id,context.task_id.to_string(),session_id,serde_json::to_string(&result)?,unix_micros(now)])?;
    Ok(())
}

fn validate(request: &RegisterExternalWaitRequest) -> Result<()> {
    if !matches!(
        request.detector.detector_type.as_str(),
        "file_exists" | "file_absent"
    ) {
        return Err(domain(
            ErrorCode::InvalidInput,
            "external detector type must be file_exists or file_absent",
        ));
    }
    if !request.detector.path.is_absolute() {
        return Err(domain(
            ErrorCode::InvalidInput,
            "external detector path must be absolute",
        ));
    }
    if request.reassess_interval_seconds <= 0
        || request.deadline_seconds.is_some_and(|value| value <= 0)
        || request.request_id.trim().is_empty()
    {
        return Err(domain(
            ErrorCode::InvalidInput,
            "external wait requires positive reassessment/deadline values and request ID",
        ));
    }
    Ok(())
}

fn reassessment_interval(connection: &rusqlite::Connection, id: &str) -> Result<i64> {
    Ok(connection.query_row(
        "SELECT reassess_interval_seconds FROM external_waits WHERE id=?1",
        [id],
        |row| row.get(0),
    )?)
}

fn load(connection: &rusqlite::Connection, id: &str) -> Result<ExternalWaitView> {
    let row = connection
        .query_row(
            "SELECT status,detector_json,deadline_at,next_reassessment_at,
                    last_observed_at,observation_count,result_json
             FROM external_waits WHERE id=?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| domain(ErrorCode::WaitNotFound, "external wait was not found"))?;
    Ok(ExternalWaitView {
        wait_id: id.into(),
        status: row.0,
        detector: serde_json::from_str(&row.1)?,
        deadline_at_micros: row.2,
        next_reassessment_at_micros: row.3,
        last_observed_at_micros: row.4,
        observation_count: row.5,
        result: row
            .6
            .map(|value| serde_json::from_str(&value))
            .transpose()?,
    })
}

fn domain(code: ErrorCode, message: &str) -> SessionBridgeError {
    SessionBridgeError::domain(code, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        clock::FakeClock,
        domain::{ParticipantId, SessionId, TaskId, WorkspaceId},
    };
    use rusqlite::params;
    use sha2::{Digest, Sha256};
    use tempfile::tempdir;
    use time::OffsetDateTime;

    #[test]
    fn file_detector_satisfies_and_reactivates_participant() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("state.db");
        SqliteStore::migrate(&database).unwrap();
        let mut store = SqliteStore::open(&database).unwrap();
        let workspace_id = WorkspaceId::new();
        let task_id = TaskId::new();
        let session_id = SessionId::new();
        let participant_id = ParticipantId::new();
        let token = format!("sb_session_{}", "A".repeat(43));
        let token_hash = Sha256::digest(token.as_bytes()).to_vec();
        store.connection().execute("INSERT INTO workspaces(id,name,repository_identity,created_at) VALUES(?1,'test','repo',0)", [workspace_id.to_string()]).unwrap();
        store.connection().execute("INSERT INTO tasks(id,workspace_id,title,phase,autonomy_policy,completion_policy,created_at,updated_at) VALUES(?1,?2,'test','active','open','all_workers_concluded',0,0)",params![task_id.to_string(),workspace_id.to_string()]).unwrap();
        store.connection().execute("INSERT INTO agent_sessions(id,task_id,token_hash,label,protocol_version,last_seen_at,created_at,updated_at) VALUES(?1,?2,?3,'agent',1,0,0,0)",params![session_id.to_string(),task_id.to_string(),token_hash]).unwrap();
        store.connection().execute("INSERT INTO task_participants(id,task_id,session_id,kind,completion_epoch,state,joined_at,created_at,updated_at) VALUES(?1,?2,?3,'worker',1,'active',0,0,0)",params![participant_id.to_string(),task_id.to_string(),session_id.to_string()]).unwrap();
        let context = SessionContext {
            schema_version: 1,
            protocol_version: 1,
            workspace_id,
            repository_instance_id: "repo".into(),
            task_id,
            session_id,
            token,
            database_path: database.display().to_string(),
        };
        let clock = FakeClock::new(OffsetDateTime::UNIX_EPOCH);
        let target = directory.path().join("external-ready");
        let service = ExternalWaitService::new(clock.clone());
        let registered = service
            .register(
                &mut store,
                &context,
                RegisterExternalWaitRequest {
                    detector: ExternalDetector {
                        detector_type: "file_exists".into(),
                        path: target.clone(),
                    },
                    deadline_seconds: None,
                    reassess_interval_seconds: 5,
                    request_id: "external-1".into(),
                },
            )
            .unwrap();
        assert_eq!(registered.status, "active");
        let state: String = store
            .connection()
            .query_row(
                "SELECT state FROM task_participants WHERE id=?1",
                [participant_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, "waiting");
        std::fs::write(&target, b"ready").unwrap();
        clock.advance(Duration::seconds(5));
        let scan = service.scan_due(&mut store, 100).unwrap();
        assert_eq!((scan.scanned, scan.completed), (1, 1));
        let completed = service
            .poll(&mut store, &context, &registered.wait_id)
            .unwrap();
        assert_eq!(completed.status, "satisfied");
        assert_eq!(completed.observation_count, 1);
        assert_eq!(completed.result.as_ref().unwrap()["satisfied"], true);
        let outbox: (String, String) = store
            .connection()
            .query_row(
                "SELECT status,payload_json FROM external_wait_delivery_outbox WHERE wait_id=?1",
                [&registered.wait_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(outbox.0, "pending");
        assert!(outbox.1.contains("satisfied"));
        let state: String = store
            .connection()
            .query_row(
                "SELECT state FROM task_participants WHERE id=?1",
                [participant_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, "active");
    }
}
