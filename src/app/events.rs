use rusqlite::{Connection, params};
use serde::Serialize;

use crate::{Result, clock::unix_micros, domain::WorkspaceId};

#[derive(Debug, Clone, Copy)]
pub struct EventActor<'a> {
    pub kind: &'a str,
    pub id: Option<&'a str>,
}

#[derive(Debug, Clone, Copy)]
pub struct EventEntity<'a> {
    pub kind: &'a str,
    pub id: Option<&'a str>,
}

#[allow(clippy::too_many_arguments)]
pub fn append<T: Serialize>(
    connection: &Connection,
    workspace_id: WorkspaceId,
    task_id: Option<&str>,
    event_type: &str,
    actor: EventActor<'_>,
    entity: EventEntity<'_>,
    payload: &T,
    occurred_at: time::OffsetDateTime,
) -> Result<i64> {
    let payload = serde_json::to_string(payload)?;
    connection.execute(
        "INSERT INTO events (
            workspace_id, task_id, event_type, actor_type, actor_id,
            entity_type, entity_id, payload_json, schema_version, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1, ?9)",
        params![
            workspace_id.to_string(),
            task_id,
            event_type,
            actor.kind,
            actor.id,
            entity.kind,
            entity.id,
            payload,
            unix_micros(occurred_at),
        ],
    )?;
    let sequence = connection.last_insert_rowid();
    tracing::debug!(
        event = "audit_event_appended",
        event_sequence = sequence,
        event_type,
        workspace_id = %workspace_id,
        task_id = task_id.unwrap_or(""),
        actor_type = actor.kind,
        actor_id = actor.id.unwrap_or(""),
        entity_type = entity.kind,
        entity_id = entity.id.unwrap_or(""),
        "immutable audit event appended"
    );
    Ok(sequence)
}

pub fn latest_cursor(connection: &Connection, task_id: Option<&str>) -> Result<i64> {
    let cursor = match task_id {
        Some(task_id) => connection.query_row(
            "SELECT COALESCE(MAX(sequence), 0) FROM events WHERE task_id = ?1",
            [task_id],
            |row| row.get(0),
        )?,
        None => {
            connection.query_row("SELECT COALESCE(MAX(sequence), 0) FROM events", [], |row| {
                row.get(0)
            })?
        }
    };
    Ok(cursor)
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use time::OffsetDateTime;

    use super::*;

    #[test]
    fn latest_cursor_is_zero_for_empty_event_table() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE events (sequence INTEGER PRIMARY KEY, task_id TEXT);")
            .unwrap();

        assert_eq!(latest_cursor(&connection, None).unwrap(), 0);
        assert_eq!(latest_cursor(&connection, Some("task")).unwrap(), 0);
    }

    #[test]
    fn actor_and_entity_are_copyable_transaction_inputs() {
        let actor = EventActor {
            kind: "user",
            id: Some("id"),
        };
        let entity = EventEntity {
            kind: "task",
            id: Some("task"),
        };
        let _ = (actor, actor, entity, entity, OffsetDateTime::UNIX_EPOCH);
    }
}
