use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    Result,
    app::{events, task::WorkspaceBinding},
    clock::{Clock, unix_micros},
    domain::{TaskId, WorkItemId},
    error::{ErrorCode, SessionBridgeError},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliverableView {
    pub id: String,
    pub task_id: TaskId,
    pub key: String,
    pub title: String,
    pub description: String,
    pub kind: String,
    pub required: bool,
    pub source: String,
    pub status: String,
    pub version: i64,
    pub work_item_ids: Vec<String>,
    pub evidence_ids: Vec<String>,
}

pub struct AddDeliverableRequest {
    pub task_id: TaskId,
    pub key: String,
    pub title: String,
    pub description: String,
    pub kind: String,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeliverableEvaluation {
    pub status: &'static str,
    pub evidence_id: Option<String>,
}

struct DeliverableLink<'a> {
    task_id: TaskId,
    deliverable_id: &'a str,
    table: &'static str,
    column: &'static str,
    target_id: &'a str,
    status: &'static str,
    event_type: &'static str,
}

pub struct DeliverableService<C> {
    clock: C,
}
impl<C: Clock> DeliverableService<C> {
    pub fn new(clock: C) -> Self {
        Self { clock }
    }
    pub fn list(
        &self,
        c: &Connection,
        b: WorkspaceBinding<'_>,
        t: TaskId,
    ) -> Result<Vec<DeliverableView>> {
        require_task(c, b, t)?;
        let mut s=c.prepare("SELECT id,key,title,description,kind,required,source,status,version FROM deliverables WHERE task_id=?1 ORDER BY created_at,id")?;
        let rows = s.query_map([t.to_string()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get::<_, bool>(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, key, title, description, kind, required, source, status, version) = row?;
            out.push(DeliverableView {
                work_item_ids: related(c, "deliverable_coverage", "work_item_id", &id)?,
                evidence_ids: related(c, "deliverable_evidence", "evidence_id", &id)?,
                id,
                task_id: t,
                key,
                title,
                description,
                kind,
                required,
                source,
                status,
                version,
            });
        }
        Ok(out)
    }
    pub fn add(
        &self,
        c: &mut Connection,
        b: WorkspaceBinding<'_>,
        request: AddDeliverableRequest,
    ) -> Result<DeliverableView> {
        let AddDeliverableRequest {
            task_id: t,
            key,
            title,
            description,
            kind,
            required,
        } = request;
        for (f, v) in [("key", &key), ("title", &title), ("kind", &kind)] {
            if v.trim().is_empty() {
                return Err(SessionBridgeError::domain(
                    ErrorCode::InvalidInput,
                    format!("{f} cannot be blank"),
                ));
            }
        }
        let now = self.clock.now();
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_active(&tx, b, t)?;
        let id = Uuid::now_v7().to_string();
        tx.execute("INSERT INTO deliverables(id,task_id,key,title,description,kind,required,source,status,created_at,updated_at)VALUES(?1,?2,?3,?4,?5,?6,?7,'user','open',?8,?8)",params![id,t.to_string(),key.trim(),title.trim(),description.trim(),kind.trim(),required,unix_micros(now)])?;
        bump(&tx, t, now)?;
        event(
            &tx,
            b,
            t,
            "deliverable.added",
            &id,
            serde_json::json!({"key":key}),
            now,
        )?;
        tx.commit()?;
        Ok(self
            .list(c, b, t)?
            .into_iter()
            .find(|v| v.id == id)
            .unwrap())
    }
    pub fn map(
        &self,
        c: &mut Connection,
        b: WorkspaceBinding<'_>,
        t: TaskId,
        d: &str,
        item: WorkItemId,
    ) -> Result<DeliverableView> {
        self.link(
            c,
            b,
            DeliverableLink {
                task_id: t,
                deliverable_id: d,
                table: "deliverable_coverage",
                column: "work_item_id",
                target_id: &item.to_string(),
                status: "planned",
                event_type: "deliverable.mapped",
            },
        )
    }
    pub fn verify(
        &self,
        c: &mut Connection,
        b: WorkspaceBinding<'_>,
        t: TaskId,
        d: &str,
        e: &str,
    ) -> Result<DeliverableView> {
        self.link(
            c,
            b,
            DeliverableLink {
                task_id: t,
                deliverable_id: d,
                table: "deliverable_evidence",
                column: "evidence_id",
                target_id: e,
                status: "verified",
                event_type: "deliverable.verified",
            },
        )
    }
    fn link(
        &self,
        c: &mut Connection,
        b: WorkspaceBinding<'_>,
        link: DeliverableLink<'_>,
    ) -> Result<DeliverableView> {
        let DeliverableLink {
            task_id: t,
            deliverable_id: d,
            table,
            column,
            target_id: target,
            status,
            event_type,
        } = link;
        let now = self.clock.now();
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_active(&tx, b, t)?;
        let deliverable_kind: Option<String> = tx
            .query_row(
                "SELECT kind FROM deliverables WHERE id=?1 AND task_id=?2",
                params![d, t.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        let Some(deliverable_kind) = deliverable_kind else {
            return Err(not_found("Deliverable"));
        };
        let valid = if table == "deliverable_coverage" {
            tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM work_items WHERE id=?1 AND task_id=?2)",
                params![target, t.to_string()],
                |r| r.get(0),
            )?
        } else {
            evaluate_deliverable(&tx, t, d, &deliverable_kind, Some(target))?
                .evidence_id
                .as_deref()
                == Some(target)
        };
        if !valid {
            return Err(SessionBridgeError::domain(
                if table == "deliverable_coverage" {
                    ErrorCode::NotFound
                } else {
                    ErrorCode::ItemNotReady
                },
                if table == "deliverable_coverage" {
                    "WorkItem was not found"
                } else if requires_integrated_code(&deliverable_kind) {
                    "code Deliverable verification requires accepted mapped work, an integrated ChangeSet, and passed provenance-bound Evidence for its integrated commit"
                } else {
                    "non-code Deliverable verification requires passed Evidence explicitly bound to this Deliverable"
                },
            ));
        }
        tx.execute(
            &format!(
                "INSERT OR IGNORE INTO {table}(deliverable_id,{column},created_at)VALUES(?1,?2,?3)"
            ),
            params![d, target, unix_micros(now)],
        )?;
        tx.execute("UPDATE deliverables SET status=?3,version=version+1,updated_at=?4 WHERE id=?1 AND task_id=?2",params![d,t.to_string(),status,unix_micros(now)])?;
        bump(&tx, t, now)?;
        event(
            &tx,
            b,
            t,
            event_type,
            d,
            serde_json::json!({"target_id":target}),
            now,
        )?;
        tx.commit()?;
        Ok(self.list(c, b, t)?.into_iter().find(|v| v.id == d).unwrap())
    }
}

pub(crate) fn requires_integrated_code(kind: &str) -> bool {
    matches!(
        kind.trim().to_ascii_lowercase().as_str(),
        "code" | "implementation" | "cli" | "gui" | "frontend" | "backend" | "runtime"
    )
}

pub(crate) fn evaluate_deliverable(
    connection: &Connection,
    task_id: TaskId,
    deliverable_id: &str,
    kind: &str,
    required_evidence_id: Option<&str>,
) -> Result<DeliverableEvaluation> {
    let mapped: i64 = connection.query_row(
        "SELECT COUNT(*) FROM deliverable_coverage WHERE deliverable_id=?1",
        [deliverable_id],
        |row| row.get(0),
    )?;
    let evidence_id = if requires_integrated_code(kind) {
        let all_accepted: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM deliverable_coverage WHERE deliverable_id=?1)
               AND NOT EXISTS(
                   SELECT 1 FROM deliverable_coverage dc
                   JOIN work_items w ON w.id=dc.work_item_id
                   WHERE dc.deliverable_id=?1 AND w.result_state<>'accepted'
               )",
            [deliverable_id],
            |row| row.get(0),
        )?;
        if all_accepted {
            let missing = missing_integrated_change_sets(connection, task_id)?;
            connection
                .prepare(
                    "SELECT e.id,cs.id FROM evidence e
                     JOIN workspaces ws ON ws.id=(SELECT workspace_id FROM tasks WHERE id=?2)
                     JOIN change_sets cs ON cs.head_commit=e.commit_hash
                       AND cs.task_id=?2 AND cs.integration_state='integrated'
                     JOIN work_attempts a ON a.id=cs.attempt_id
                     JOIN deliverable_coverage dc ON dc.work_item_id=a.work_item_id
                     WHERE dc.deliverable_id=?1 AND e.task_id=?2 AND e.result='passed'
                       AND e.repository_identity=ws.repository_identity
                       AND (e.subject_type='work_item' AND e.subject_id=a.work_item_id
                            OR e.subject_type='deliverable' AND e.subject_id=?1)
                     ORDER BY e.created_at DESC,e.id DESC",
                )?
                .query_map(params![deliverable_id, task_id.to_string()], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
                .into_iter()
                .find_map(|(evidence_id, change_set_id)| {
                    (!missing.contains(&change_set_id)
                        && required_evidence_id.is_none_or(|required| required == evidence_id))
                    .then_some(evidence_id)
                })
        } else {
            None
        }
    } else {
        connection
            .prepare(
                "SELECT id FROM evidence WHERE task_id=?2 AND result='passed'
                   AND subject_type='deliverable' AND subject_id=?1
                 ORDER BY created_at DESC,id DESC",
            )?
            .query_map(params![deliverable_id, task_id.to_string()], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .find(|evidence_id| required_evidence_id.is_none_or(|required| required == evidence_id))
    };
    Ok(DeliverableEvaluation {
        status: if evidence_id.is_some() {
            "verified"
        } else if mapped > 0 {
            "planned"
        } else {
            "open"
        },
        evidence_id,
    })
}

fn missing_integrated_change_sets(
    connection: &Connection,
    task_id: TaskId,
) -> Result<BTreeSet<String>> {
    let serialized: Option<String> = connection
        .query_row(
            "SELECT missing_change_sets_json FROM task_integration_integrity WHERE task_id=?1",
            [task_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    let Some(serialized) = serialized else {
        return Ok(BTreeSet::new());
    };
    let values: Vec<serde_json::Value> = serde_json::from_str(&serialized)?;
    Ok(values
        .into_iter()
        .filter_map(|value| {
            value
                .get("change_set_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect())
}
fn require_task(c: &Connection, b: WorkspaceBinding<'_>, t: TaskId) -> Result<()> {
    c.query_row("SELECT 1 FROM tasks t JOIN workspaces w ON w.id=t.workspace_id WHERE t.id=?1 AND w.id=?2 AND w.repository_identity=?3",params![t.to_string(),b.id.to_string(),b.repository_identity],|_|Ok(())).optional()?.ok_or_else(||not_found("Task"))
}
fn require_active(c: &Connection, b: WorkspaceBinding<'_>, t: TaskId) -> Result<()> {
    require_task(c, b, t)?;
    let p: String = c.query_row(
        "SELECT phase FROM tasks WHERE id=?1",
        [t.to_string()],
        |r| r.get(0),
    )?;
    if p != "active" {
        return Err(SessionBridgeError::domain(
            ErrorCode::TaskNotActive,
            "Deliverables can only change on an active Task",
        ));
    }
    Ok(())
}
fn not_found(n: &str) -> SessionBridgeError {
    SessionBridgeError::domain(ErrorCode::NotFound, format!("{n} was not found"))
}
fn bump(c: &Connection, t: TaskId, n: time::OffsetDateTime) -> Result<()> {
    c.execute("UPDATE tasks SET completion_revision=completion_revision+1,version=version+1,updated_at=?2 WHERE id=?1",params![t.to_string(),unix_micros(n)])?;
    Ok(())
}
fn event(
    c: &Connection,
    b: WorkspaceBinding<'_>,
    t: TaskId,
    ty: &str,
    id: &str,
    p: serde_json::Value,
    n: time::OffsetDateTime,
) -> Result<i64> {
    events::append(
        c,
        b.id,
        Some(&t.to_string()),
        ty,
        events::EventActor {
            kind: "user",
            id: Some(&b.id.to_string()),
        },
        events::EventEntity {
            kind: "deliverable",
            id: Some(id),
        },
        &p,
        n,
    )
}
fn related(c: &Connection, table: &str, column: &str, id: &str) -> Result<Vec<String>> {
    let mut s = c.prepare(&format!(
        "SELECT {column} FROM {table} WHERE deliverable_id=?1 ORDER BY {column}"
    ))?;
    Ok(s.query_map([id], |r| r.get(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?)
}
