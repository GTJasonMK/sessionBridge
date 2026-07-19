use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

use crate::{
    Result,
    clock::unix_micros,
    error::{ErrorCode, SessionBridgeError},
};

#[derive(Debug, Clone, Copy)]
pub struct IdempotencyKey<'a> {
    pub actor_type: &'a str,
    pub actor_id: &'a str,
    pub request_id: &'a str,
}

#[derive(Debug, Clone)]
pub struct StoredResult<T> {
    pub value: T,
    pub event_sequence: Option<i64>,
}

pub fn hash_payload<T: Serialize>(payload: &T) -> Result<[u8; 32]> {
    let encoded = serde_json::to_vec(payload)?;
    Ok(Sha256::digest(encoded).into())
}

pub fn replay<T: DeserializeOwned>(
    connection: &Connection,
    key: IdempotencyKey<'_>,
    payload_hash: &[u8; 32],
) -> Result<Option<StoredResult<T>>> {
    let row = connection
        .query_row(
            "SELECT payload_hash, result_json, event_sequence
             FROM idempotency_records
             WHERE actor_type = ?1 AND actor_id = ?2 AND request_id = ?3",
            params![key.actor_type, key.actor_id, key.request_id],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                ))
            },
        )
        .optional()?;

    let Some((stored_hash, result_json, event_sequence)) = row else {
        return Ok(None);
    };
    if stored_hash.as_slice() != payload_hash {
        return Err(SessionBridgeError::domain(
            ErrorCode::IdempotencyKeyReused,
            "request ID was already used with a different payload",
        )
        .with_details(serde_json::json!({
            "actor_type": key.actor_type,
            "actor_id": key.actor_id,
            "request_id": key.request_id,
        })));
    }

    Ok(Some(StoredResult {
        value: serde_json::from_str(&result_json)?,
        event_sequence,
    }))
}

#[allow(clippy::too_many_arguments)]
pub fn record<T: Serialize>(
    connection: &Connection,
    key: IdempotencyKey<'_>,
    task_id: Option<&str>,
    payload_hash: &[u8; 32],
    result: &T,
    event_sequence: Option<i64>,
    created_at: time::OffsetDateTime,
) -> Result<()> {
    connection.execute(
        "INSERT INTO idempotency_records (
            actor_type, actor_id, request_id, task_id, payload_hash,
            result_json, event_sequence, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            key.actor_type,
            key.actor_id,
            key.request_id,
            task_id,
            payload_hash.as_slice(),
            serde_json::to_string(result)?,
            event_sequence,
            unix_micros(created_at),
        ],
    )?;
    Ok(())
}
