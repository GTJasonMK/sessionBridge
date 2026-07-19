use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::{
    Result,
    app::{collaboration_projection::load_participant_liveness, task_reconciler::load_integrity},
    clock::{Clock, unix_micros},
    domain::TaskId,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollaborationFinding {
    pub code: String,
    pub status: String,
    pub severity: String,
    pub message: String,
    pub entity_type: Option<String>,
    pub entity_id: Option<String>,
    pub suggested_actions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollaborationDiagnostics {
    pub task_id: TaskId,
    pub task_phase: String,
    pub generated_at_micros: i64,
    pub finding_count: usize,
    pub critical_count: usize,
    pub warning_count: usize,
    pub findings: Vec<CollaborationFinding>,
}

pub fn analyze<C: Clock>(
    connection: &Connection,
    task_id: TaskId,
    clock: &C,
) -> Result<CollaborationDiagnostics> {
    let now = unix_micros(clock.now());
    let phase: String = connection.query_row(
        "SELECT phase FROM tasks WHERE id=?1",
        [task_id.to_string()],
        |row| row.get(0),
    )?;
    let mut findings = Vec::new();
    if let Some(integrity) = load_integrity(connection, task_id)?
        && integrity.status != "valid"
    {
        let message = if integrity.status == "drift" {
            let missing = integrity
                .missing_change_sets
                .iter()
                .map(|change_set| {
                    format!(
                        "{}@{} (WorkItem {})",
                        change_set.change_set_id, change_set.head_commit, change_set.work_item_id
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "target branch {} at {} is missing integrated ChangeSets: {}",
                integrity.target_branch,
                integrity.target_head.as_deref().unwrap_or("unresolved"),
                missing
            )
        } else {
            format!(
                "target branch integration integrity is unobservable: {}",
                integrity.details
            )
        };
        findings.push(CollaborationFinding {
            code: if integrity.status == "drift" {
                "INTEGRATED_CHANGESET_DRIFT"
            } else {
                "INTEGRATION_INTEGRITY_UNOBSERVABLE"
            }
            .into(),
            status: "fail".into(),
            severity: "critical".into(),
            message,
            entity_type: Some("task".into()),
            entity_id: Some(task_id.to_string()),
            suggested_actions: vec![format!(
                "sessionbridge task integrity {task_id} --format json"
            )],
        });
    }

    let orphan_waits = connection
        .prepare(
            "SELECT DISTINCT aw.id,aw.session_id,awt.work_item_id
             FROM agent_waits aw JOIN agent_wait_targets awt ON awt.wait_id=aw.id
             WHERE aw.task_id=?1 AND aw.status='active' AND awt.status='pending'
               AND NOT EXISTS(
                 SELECT 1 FROM work_claims c WHERE c.work_item_id=awt.work_item_id
                   AND c.closed_at IS NULL AND c.lease_expires_at>?2
               )
               AND NOT EXISTS(
                 SELECT 1 FROM stage_handoffs h WHERE h.work_item_id=awt.work_item_id
                   AND h.status='reserved' AND h.expires_at>?2
               )",
        )?
        .query_map(params![task_id.to_string(), now], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (wait_id, session_id, work_item_id) in orphan_waits {
        findings.push(CollaborationFinding {
            code: "ORPHAN_AGENT_WAIT".into(),
            status: "fail".into(),
            severity: "critical".into(),
            message: format!(
                "Session {session_id} waits for WorkItem {work_item_id}, but no live Claim or stage handoff owns it"
            ),
            entity_type: Some("agent_wait".into()),
            entity_id: Some(wait_id.clone()),
            suggested_actions: vec![format!(
                "sessionbridge agent await show {wait_id} --format json"
            )],
        });
    }

    let waiting: i64 = scalar(
        connection,
        "SELECT count(*) FROM task_participants p JOIN tasks t ON t.id=p.task_id
         WHERE p.task_id=?1 AND p.completion_epoch=t.completion_epoch
           AND p.kind='worker' AND p.state='waiting'",
        &task_id,
    )?;
    let active: i64 = scalar(
        connection,
        "SELECT count(*) FROM task_participants p JOIN tasks t ON t.id=p.task_id
         WHERE p.task_id=?1 AND p.completion_epoch=t.completion_epoch
           AND p.kind='worker' AND p.state='active'",
        &task_id,
    )?;
    if waiting > 0 && active == 0 && !findings.iter().any(|item| item.code == "ORPHAN_AGENT_WAIT") {
        findings.push(CollaborationFinding {
            code: "ALL_WORKERS_WAITING".into(),
            status: "warning".into(),
            severity: "warning".into(),
            message: format!("all {waiting} non-terminal workers are waiting; ownership chains are currently valid"),
            entity_type: Some("task".into()),
            entity_id: Some(task_id.to_string()),
            suggested_actions: vec![format!("sessionbridge status {task_id} --format json")],
        });
    }

    let stage_rows = connection
        .prepare(
            "SELECT w.id,w.title,
                    CASE
                      WHEN w.result_state='submitted' AND NOT EXISTS(
                        SELECT 1 FROM review_results r WHERE r.work_item_id=w.id AND r.decision='approved'
                      ) THEN 'review'
                      WHEN w.result_state='submitted' AND EXISTS(
                        SELECT 1 FROM review_results r WHERE r.work_item_id=w.id AND r.decision='approved'
                      ) AND EXISTS(
                        SELECT 1 FROM change_sets c JOIN work_attempts a ON a.id=c.attempt_id
                        WHERE a.work_item_id=w.id AND c.integration_state='pending'
                      ) THEN 'integrate'
                    END
             FROM work_items w
             WHERE w.task_id=?1 AND w.plan_state='active' AND w.result_state='submitted'",
        )?
        .query_map([task_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (item_id, title, stage) in stage_rows {
        let Some(stage) = stage else { continue };
        let owned: bool = connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM work_claims WHERE work_item_id=?1 AND mode=?2
                  AND closed_at IS NULL AND lease_expires_at>?3
             ) OR EXISTS(
                SELECT 1 FROM stage_handoffs WHERE work_item_id=?1 AND to_stage=?2
                  AND status='reserved' AND expires_at>?3
             )",
            params![item_id, stage, now],
            |row| row.get(0),
        )?;
        if !owned {
            findings.push(CollaborationFinding {
                code: format!("UNOWNED_{}_STAGE", stage.to_ascii_uppercase()),
                status: "warning".into(),
                severity: "warning".into(),
                message: format!(
                    "{title} is ready for {stage}, but no Claim or handoff owns that stage"
                ),
                entity_type: Some("work_item".into()),
                entity_id: Some(item_id.clone()),
                suggested_actions: vec![format!(
                    "sessionbridge item inspect {item_id} --format json"
                )],
            });
        }
    }

    for participant in load_participant_liveness(connection, task_id, None, now, usize::MAX)? {
        findings.push(CollaborationFinding {
            code: if participant.retirement_candidate {
                "PARTICIPANT_RETIREMENT_CANDIDATE"
            } else {
                "PARTICIPANT_STALE_WARNING"
            }
            .into(),
            status: "warning".into(),
            severity: "warning".into(),
            message: format!(
                "{} has been inactive for {} seconds{}",
                participant.label,
                (now - participant.last_seen_at).max(0) / 1_000_000,
                if participant.retirement_candidate {
                    " and is eligible for Agent-managed handoff/retirement assessment"
                } else {
                    "; coordinate recovery before considering retirement"
                }
            ),
            entity_type: Some("participant".into()),
            entity_id: Some(participant.participant_id.clone()),
            suggested_actions: vec![format!(
                "sessionbridge agent inspect {} --format json",
                participant.participant_id
            )],
        });
    }

    if findings.is_empty() {
        findings.push(CollaborationFinding {
            code: "COLLABORATION_STATE_HEALTHY".into(),
            status: "pass".into(),
            severity: "info".into(),
            message: "no structural collaboration bottleneck was detected".into(),
            entity_type: Some("task".into()),
            entity_id: Some(task_id.to_string()),
            suggested_actions: Vec::new(),
        });
    }
    let critical_count = findings
        .iter()
        .filter(|item| item.severity == "critical")
        .count();
    let warning_count = findings
        .iter()
        .filter(|item| item.severity == "warning")
        .count();
    Ok(CollaborationDiagnostics {
        task_id,
        task_phase: phase,
        generated_at_micros: now,
        finding_count: findings.len(),
        critical_count,
        warning_count,
        findings,
    })
}

fn scalar(connection: &Connection, sql: &str, task_id: &TaskId) -> Result<i64> {
    connection
        .query_row(sql, [task_id.to_string()], |row| row.get(0))
        .map_err(Into::into)
}
