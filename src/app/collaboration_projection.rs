use rusqlite::{Connection, params};

use crate::{Result, app::task, domain::TaskId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParticipantLiveness {
    pub participant_id: String,
    pub label: String,
    pub last_seen_at: i64,
    pub retirement_candidate: bool,
}

pub fn load_participant_liveness(
    connection: &Connection,
    task_id: TaskId,
    exclude_session_id: Option<&str>,
    now_micros: i64,
    limit: usize,
) -> Result<Vec<ParticipantLiveness>> {
    let stale_before = task::cutoff_micros(now_micros, task::PARTICIPANT_STALE_AFTER);
    let retirement_before = task::cutoff_micros(now_micros, task::PARTICIPANT_RETIREMENT_AFTER);
    let rows = connection
        .prepare(
            "SELECT p.id,s.label,s.last_seen_at
             FROM task_participants p JOIN agent_sessions s ON s.id=p.session_id
             JOIN tasks t ON t.id=p.task_id
             WHERE p.task_id=?1 AND p.completion_epoch=t.completion_epoch
               AND p.kind='worker' AND p.state IN ('active','waiting','blocked','departed')
               AND (?2 IS NULL OR s.id<>?2) AND s.last_seen_at<=?3
             ORDER BY s.last_seen_at,p.id LIMIT ?4",
        )?
        .query_map(
            params![
                task_id.to_string(),
                exclude_session_id,
                stale_before,
                i64::try_from(limit).unwrap_or(i64::MAX),
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .map(
            |(participant_id, label, last_seen_at)| ParticipantLiveness {
                participant_id,
                label,
                last_seen_at,
                retirement_candidate: last_seen_at <= retirement_before,
            },
        )
        .collect())
}
