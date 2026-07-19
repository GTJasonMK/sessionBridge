use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::time::{Duration as StdDuration, Instant};

use crate::{
    Result,
    app::{
        agent::AgentService,
        events::{self, EventActor, EventEntity},
        idempotency::{self, IdempotencyKey},
    },
    clock::{Clock, unix_micros},
    domain::{SessionContext, TaskId, TaskPhase},
    error::{ErrorCode, SessionBridgeError},
    storage::SqliteStore,
};

const MAX_BODY_BYTES: usize = 8_000;
const MAX_PAGE_SIZE: usize = 200;
const MESSAGE_KINDS: &[&str] = &[
    "progress", "intent", "question", "decision", "warning", "handoff", "note",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendChatMessageRequest {
    #[serde(default = "default_message_kind")]
    pub kind: String,
    pub body: String,
    #[serde(default)]
    pub reply_to_message_id: Option<String>,
    #[serde(default)]
    pub mentions: Vec<String>,
    #[serde(default)]
    pub related_entities: Vec<ChatEntityRef>,
    #[serde(default)]
    pub request_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatEntityRef {
    pub entity_type: String,
    pub entity_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessageView {
    pub id: String,
    pub task_id: TaskId,
    pub sender_type: String,
    pub sender_id: Option<String>,
    pub sender_label: String,
    pub kind: String,
    pub body: String,
    pub reply_to_message_id: Option<String>,
    pub mentions: Vec<String>,
    pub related_entities: Vec<ChatEntityRef>,
    pub event_sequence: i64,
    pub created_at_micros: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessagePage {
    pub messages: Vec<ChatMessageView>,
    pub previous_cursor: i64,
    pub next_cursor: i64,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatInboxMessage {
    #[serde(flatten)]
    pub message: ChatMessageView,
    pub directed: bool,
    pub direction_reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatInboxView {
    pub messages: Vec<ChatInboxMessage>,
    pub unread_count: i64,
    pub directed_unread_count: i64,
    pub has_more: bool,
    pub send_ref: String,
    pub await_ref: String,
    pub ack_ref: String,
    pub history_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatAckResult {
    pub acknowledged_message_ids: Vec<String>,
    pub newly_acknowledged: usize,
    pub unread_count: i64,
    pub directed_unread_count: i64,
}

pub struct ChatService<C> {
    clock: C,
}

struct ChatSender {
    sender_type: &'static str,
    id: Option<String>,
    label: String,
}

impl<C: Clock + Clone> ChatService<C> {
    pub fn new(clock: C) -> Self {
        Self { clock }
    }

    pub fn send_agent(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        request: SendChatMessageRequest,
    ) -> Result<ChatMessageView> {
        let authenticated =
            AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)?;
        self.send(
            store,
            context.workspace_id,
            context.task_id,
            ChatSender {
                sender_type: "agent",
                id: Some(context.session_id.to_string()),
                label: authenticated.session.label,
            },
            request,
        )
    }

    pub fn send_operator(
        &self,
        store: &mut SqliteStore,
        workspace_id: crate::domain::WorkspaceId,
        task_id: TaskId,
        request: SendChatMessageRequest,
    ) -> Result<ChatMessageView> {
        self.send(
            store,
            workspace_id,
            task_id,
            ChatSender {
                sender_type: "operator",
                id: Some("local-operator".into()),
                label: "你".into(),
            },
            request,
        )
    }

    fn send(
        &self,
        store: &mut SqliteStore,
        workspace_id: crate::domain::WorkspaceId,
        task_id: TaskId,
        sender: ChatSender,
        mut request: SendChatMessageRequest,
    ) -> Result<ChatMessageView> {
        normalize_request(&mut request)?;
        let actor_id = sender.id.as_deref().unwrap_or(sender.sender_type);
        let payload_hash = idempotency::hash_payload(&serde_json::json!({
            "operation":"chat.send",
            "task_id":task_id,
            "sender_type":sender.sender_type,
            "request":request,
        }))?;
        let now = self.clock.now();
        let transaction = store.begin_immediate()?;
        let key = IdempotencyKey {
            actor_type: sender.sender_type,
            actor_id,
            request_id: &request.request_id,
        };
        if let Some(stored) = idempotency::replay(&transaction, key, &payload_hash)? {
            transaction.commit()?;
            return Ok(stored.value);
        }
        let phase: String = transaction
            .query_row(
                "SELECT phase FROM tasks WHERE id=?1 AND workspace_id=?2",
                params![task_id.to_string(), workspace_id.to_string()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| SessionBridgeError::domain(ErrorCode::NotFound, "Task was not found"))?;
        let phase: TaskPhase = phase.parse().map_err(|_| {
            SessionBridgeError::domain(ErrorCode::Database, "Task has an invalid phase")
        })?;
        if matches!(phase, TaskPhase::Cancelled | TaskPhase::Archived) {
            return Err(SessionBridgeError::domain(
                ErrorCode::TaskNotActive,
                "cancelled or archived Tasks do not accept new chat messages",
            ));
        }
        validate_reply(
            &transaction,
            task_id,
            request.reply_to_message_id.as_deref(),
        )?;
        validate_mentions(&transaction, task_id, &request.mentions)?;
        let id = uuid::Uuid::now_v7().to_string();
        let event_sequence = events::append(
            &transaction,
            workspace_id,
            Some(&task_id.to_string()),
            "chat.message_sent",
            EventActor {
                kind: sender.sender_type,
                id: sender.id.as_deref(),
            },
            EventEntity {
                kind: "chat_message",
                id: Some(&id),
            },
            &serde_json::json!({
                "kind":request.kind,
                "reply_to_message_id":request.reply_to_message_id,
                "mentions":request.mentions,
                "related_entities":request.related_entities,
            }),
            now,
        )?;
        transaction.execute(
            "INSERT INTO chat_messages(
                id,task_id,sender_type,sender_id,sender_label,kind,body,
                reply_to_message_id,mentions_json,related_entities_json,event_sequence,created_at
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                id,
                task_id.to_string(),
                sender.sender_type,
                sender.id,
                sender.label,
                request.kind,
                request.body,
                request.reply_to_message_id,
                serde_json::to_string(&request.mentions)?,
                serde_json::to_string(&request.related_entities)?,
                event_sequence,
                unix_micros(now),
            ],
        )?;
        let result = load_message(&transaction, &id)?.ok_or_else(|| {
            SessionBridgeError::domain(ErrorCode::Internal, "chat message insert was not visible")
        })?;
        idempotency::record(
            &transaction,
            key,
            Some(&task_id.to_string()),
            &payload_hash,
            &result,
            Some(event_sequence),
            now,
        )?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn history(
        &self,
        connection: &Connection,
        task_id: TaskId,
        since: i64,
        limit: usize,
    ) -> Result<ChatMessagePage> {
        if since < 0 || limit == 0 || limit > MAX_PAGE_SIZE {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                format!("chat cursor must be non-negative and limit must be 1..={MAX_PAGE_SIZE}"),
            ));
        }
        let mut statement = connection.prepare(
            "SELECT id,task_id,sender_type,sender_id,sender_label,kind,body,
                    reply_to_message_id,mentions_json,related_entities_json,event_sequence,created_at
             FROM chat_messages WHERE task_id=?1 AND event_sequence>?2
             ORDER BY event_sequence LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![task_id.to_string(), since, (limit + 1) as i64],
            row_to_message,
        )?;
        let mut messages = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        let has_more = messages.len() > limit;
        messages.truncate(limit);
        let next_cursor = messages
            .last()
            .map_or(since, |message| message.event_sequence);
        Ok(ChatMessagePage {
            messages,
            previous_cursor: since,
            next_cursor,
            has_more,
        })
    }

    pub fn recent(
        &self,
        connection: &Connection,
        task_id: TaskId,
        limit: usize,
    ) -> Result<Vec<ChatMessageView>> {
        if limit == 0 || limit > MAX_PAGE_SIZE {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                format!("chat recent limit must be 1..={MAX_PAGE_SIZE}"),
            ));
        }
        let mut statement = connection.prepare(
            "SELECT id,task_id,sender_type,sender_id,sender_label,kind,body,
                    reply_to_message_id,mentions_json,related_entities_json,event_sequence,created_at
             FROM chat_messages WHERE task_id=?1 ORDER BY event_sequence DESC LIMIT ?2",
        )?;
        let mut messages = statement
            .query_map(params![task_id.to_string(), limit as i64], row_to_message)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        messages.reverse();
        Ok(messages)
    }

    pub fn inbox(
        &self,
        connection: &Connection,
        context: &SessionContext,
        limit: usize,
    ) -> Result<ChatInboxView> {
        if limit == 0 || limit > MAX_PAGE_SIZE {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                format!("chat inbox limit must be 1..={MAX_PAGE_SIZE}"),
            ));
        }
        let task_id = context.task_id.to_string();
        let session_id = context.session_id.to_string();
        let directed_sql = "(EXISTS(SELECT 1 FROM json_each(m.mentions_json) WHERE value=?2)
            OR reply.sender_id=?2)";
        let counts: (i64, i64) = connection.query_row(
            &format!(
                "SELECT COUNT(*), COALESCE(SUM(CASE WHEN {directed_sql} THEN 1 ELSE 0 END),0)
                 FROM chat_messages m
                 LEFT JOIN chat_messages reply ON reply.id=m.reply_to_message_id
                 JOIN chat_inbox_states inbox ON inbox.session_id=?2 AND inbox.task_id=m.task_id
                 LEFT JOIN chat_message_receipts receipt
                   ON receipt.message_id=m.id AND receipt.session_id=?2
                 WHERE m.task_id=?1 AND m.event_sequence>inbox.starts_after_sequence
                   AND m.sender_id IS NOT ?2 AND receipt.message_id IS NULL"
            ),
            params![task_id, session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let mut statement = connection.prepare(&format!(
            "SELECT m.id,m.task_id,m.sender_type,m.sender_id,m.sender_label,m.kind,m.body,
                    m.reply_to_message_id,m.mentions_json,m.related_entities_json,m.event_sequence,m.created_at,
                    CASE WHEN {directed_sql} THEN 1 ELSE 0 END,
                    CASE WHEN EXISTS(SELECT 1 FROM json_each(m.mentions_json) WHERE value=?2)
                         THEN 'mention' WHEN reply.sender_id=?2 THEN 'reply' ELSE 'broadcast' END
             FROM chat_messages m
             LEFT JOIN chat_messages reply ON reply.id=m.reply_to_message_id
             JOIN chat_inbox_states inbox ON inbox.session_id=?2 AND inbox.task_id=m.task_id
             LEFT JOIN chat_message_receipts receipt
               ON receipt.message_id=m.id AND receipt.session_id=?2
             WHERE m.task_id=?1 AND m.event_sequence>inbox.starts_after_sequence
               AND m.sender_id IS NOT ?2 AND receipt.message_id IS NULL
             ORDER BY 13 DESC,m.event_sequence LIMIT ?3"
        ))?;
        let mut messages = statement
            .query_map(params![task_id, session_id, (limit + 1) as i64], |row| {
                Ok(ChatInboxMessage {
                    message: row_to_message(row)?,
                    directed: row.get(12)?,
                    direction_reason: row.get(13)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let has_more = messages.len() > limit;
        messages.truncate(limit);
        messages.sort_by_key(|item| item.message.event_sequence);
        Ok(ChatInboxView {
            messages,
            unread_count: counts.0,
            directed_unread_count: counts.1,
            has_more,
            send_ref: "sessionbridge agent chat send --input <JSON_FILE>".into(),
            await_ref: "sessionbridge agent chat await [--include-broadcasts]".into(),
            ack_ref:
                "sessionbridge agent chat ack --message <MESSAGE_ID> [--message <MESSAGE_ID> ...]"
                    .into(),
            history_ref: "sessionbridge agent chat history --since <EVENT_CURSOR> --format json"
                .into(),
        })
    }

    pub fn ack_agent(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        mut message_ids: Vec<String>,
    ) -> Result<ChatAckResult> {
        AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)?;
        message_ids.sort();
        message_ids.dedup();
        if message_ids.is_empty() || message_ids.len() > MAX_PAGE_SIZE {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                format!("chat ack requires 1..={MAX_PAGE_SIZE} message IDs"),
            ));
        }
        let now = unix_micros(self.clock.now());
        let transaction = store.begin_immediate()?;
        let mut newly_acknowledged = 0;
        for message_id in &message_ids {
            let belongs_to_task: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM chat_messages WHERE id=?1 AND task_id=?2)",
                params![message_id, context.task_id.to_string()],
                |row| row.get(0),
            )?;
            if !belongs_to_task {
                return Err(SessionBridgeError::domain(
                    ErrorCode::NotFound,
                    format!("chat message {message_id} is not part of this Task"),
                ));
            }
            newly_acknowledged += transaction.execute(
                "INSERT OR IGNORE INTO chat_message_receipts(session_id,message_id,acknowledged_at)
                 VALUES(?1,?2,?3)",
                params![context.session_id.to_string(), message_id, now],
            )?;
        }
        transaction.commit()?;
        let inbox = self.inbox(store.connection(), context, 1)?;
        Ok(ChatAckResult {
            acknowledged_message_ids: message_ids,
            newly_acknowledged,
            unread_count: inbox.unread_count,
            directed_unread_count: inbox.directed_unread_count,
        })
    }

    pub fn await_agent(
        &self,
        store: &mut SqliteStore,
        context: &SessionContext,
        limit: usize,
        directed_only: bool,
    ) -> Result<ChatInboxView> {
        AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)?;
        let mut last_heartbeat = Instant::now();
        loop {
            let mut inbox = self.inbox(store.connection(), context, limit)?;
            let ready = if directed_only {
                inbox.directed_unread_count > 0
            } else {
                inbox.unread_count > 0
            };
            if ready {
                if directed_only {
                    inbox.messages.retain(|message| message.directed);
                    inbox.unread_count = inbox.directed_unread_count;
                    inbox.has_more = inbox.directed_unread_count > inbox.messages.len() as i64;
                }
                return Ok(inbox);
            }
            let terminal: bool = store.connection().query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM tasks t
                    JOIN task_participants p ON p.task_id=t.id AND p.session_id=?2
                    WHERE t.id=?1 AND (
                        t.phase IN ('completed','cancelled','archived')
                        OR p.state IN ('concluded','departed','retired','superseded')
                    )
                )",
                params![context.task_id.to_string(), context.session_id.to_string()],
                |row| row.get(0),
            )?;
            if terminal {
                return Err(SessionBridgeError::domain(
                    ErrorCode::TaskNotActive,
                    "Task or Participant became terminal while awaiting chat",
                ));
            }
            if last_heartbeat.elapsed() >= StdDuration::from_secs(30) {
                AgentService::new(self.clock.clone()).authenticate_and_touch(store, context)?;
                last_heartbeat = Instant::now();
            }
            std::thread::sleep(StdDuration::from_millis(250));
        }
    }
}

fn normalize_request(request: &mut SendChatMessageRequest) -> Result<()> {
    request.kind = request.kind.trim().to_ascii_lowercase();
    request.body = request.body.trim().to_owned();
    request.request_id = request.request_id.trim().to_owned();
    if !MESSAGE_KINDS.contains(&request.kind.as_str()) {
        return Err(SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            "chat kind must be progress, intent, question, decision, warning, handoff, or note",
        ));
    }
    if request.body.is_empty() || request.body.len() > MAX_BODY_BYTES {
        return Err(SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            format!("chat body must contain 1..={MAX_BODY_BYTES} bytes"),
        ));
    }
    if request.request_id.is_empty() {
        return Err(SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            "chat request_id is required",
        ));
    }
    request.mentions.sort();
    request.mentions.dedup();
    request.related_entities.sort_by(|left, right| {
        (&left.entity_type, &left.entity_id).cmp(&(&right.entity_type, &right.entity_id))
    });
    request.related_entities.dedup();
    if request
        .related_entities
        .iter()
        .any(|entity| entity.entity_type.trim().is_empty() || entity.entity_id.trim().is_empty())
    {
        return Err(SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            "chat related entity type and ID cannot be blank",
        ));
    }
    Ok(())
}

fn validate_reply(connection: &Connection, task_id: TaskId, reply_to: Option<&str>) -> Result<()> {
    let Some(reply_to) = reply_to else {
        return Ok(());
    };
    let valid: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM chat_messages WHERE id=?1 AND task_id=?2)",
        params![reply_to, task_id.to_string()],
        |row| row.get(0),
    )?;
    if !valid {
        return Err(SessionBridgeError::domain(
            ErrorCode::NotFound,
            "reply target is not a chat message in this Task",
        ));
    }
    Ok(())
}

fn validate_mentions(connection: &Connection, task_id: TaskId, mentions: &[String]) -> Result<()> {
    for mention in mentions {
        let valid: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_sessions WHERE id=?1 AND task_id=?2)",
            params![mention, task_id.to_string()],
            |row| row.get(0),
        )?;
        if !valid {
            return Err(SessionBridgeError::domain(
                ErrorCode::NotFound,
                format!("mentioned Session {mention} is not part of this Task"),
            ));
        }
    }
    Ok(())
}

fn load_message(connection: &Connection, id: &str) -> Result<Option<ChatMessageView>> {
    connection
        .query_row(
            "SELECT id,task_id,sender_type,sender_id,sender_label,kind,body,
                    reply_to_message_id,mentions_json,related_entities_json,event_sequence,created_at
             FROM chat_messages WHERE id=?1",
            [id],
            row_to_message,
        )
        .optional()
        .map_err(Into::into)
}

fn row_to_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChatMessageView> {
    let task_id: String = row.get(1)?;
    let mentions: String = row.get(8)?;
    let related_entities: String = row.get(9)?;
    Ok(ChatMessageView {
        id: row.get(0)?,
        task_id: task_id.parse().map_err(|_| rusqlite::Error::InvalidQuery)?,
        sender_type: row.get(2)?,
        sender_id: row.get(3)?,
        sender_label: row.get(4)?,
        kind: row.get(5)?,
        body: row.get(6)?,
        reply_to_message_id: row.get(7)?,
        mentions: serde_json::from_str(&mentions).map_err(|_| rusqlite::Error::InvalidQuery)?,
        related_entities: serde_json::from_str(&related_entities)
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        event_sequence: row.get(10)?,
        created_at_micros: row.get(11)?,
    })
}

fn default_message_kind() -> String {
    "note".into()
}
