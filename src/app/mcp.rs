use std::io::{BufRead, Write};
use std::time::Duration as StdDuration;
use std::{path::PathBuf, str::FromStr};

use rusqlite::params;
use serde_json::{Value, json};

use crate::{
    Result,
    app::{
        agent::AgentService,
        await_service::{AwaitMode, AwaitService, RegisterAwaitRequest},
        chat::{ChatService, SendChatMessageRequest},
        completion_service::{CompletionService, ConcludeRequest},
        external_wait::{ExternalDetector, ExternalWaitService, RegisterExternalWaitRequest},
    },
    clock::{Clock, SystemClock, unix_micros},
    domain::{
        CompletionAssessment, ConclusionOutcome, OpenSignalDisposition, SessionContext, WorkItemId,
    },
    error::{ErrorCode, SessionBridgeError},
    storage::SqliteStore,
};

pub fn serve(store: &mut SqliteStore, context: &SessionContext) -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.map_err(|source| SessionBridgeError::Io {
            path: "<stdin>".into(),
            source,
        })?;
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = serde_json::from_str(&line)?;
        let id = request.get("id").cloned();
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let (response, delivery_wait_id) = match method {
            "initialize" => Some(success(
                id,
                json!({
                    "protocolVersion": "2025-03-26",
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "sessionbridge", "version": env!("CARGO_PKG_VERSION")}
                }),
            ))
            .map(|response| (response, None)),
            "notifications/initialized" => None,
            "ping" => Some((success(id, json!({})), None)),
            "tools/list" => Some(success(
                id,
                json!({"tools": [
                            await_tool_definition(),
                            external_await_tool_definition(),
                            agent_next_tool_definition(),
                            agent_conclude_tool_definition(),
                            chat_send_tool_definition(),
                chat_history_tool_definition(),
                    chat_inbox_tool_definition(),
                    chat_ack_tool_definition(),
                    chat_await_tool_definition()
                        ]}),
            ))
            .map(|response| (response, None)),
            "tools/call" => Some(handle_tool_call(store, context, id, &request)),
            _ => id.map(|id| (failure(Some(id), -32601, "method not found", None), None)),
        }
        .unzip();
        if let Some(response) = response {
            serde_json::to_writer(&mut stdout, &response)?;
            stdout
                .write_all(b"\n")
                .map_err(|source| SessionBridgeError::Io {
                    path: "<stdout>".into(),
                    source,
                })?;
            stdout.flush().map_err(|source| SessionBridgeError::Io {
                path: "<stdout>".into(),
                source,
            })?;
            if let Some(delivery) = delivery_wait_id.flatten() {
                acknowledge_delivery(store, &delivery)?;
            }
        }
    }
    Ok(())
}

fn handle_tool_call(
    store: &mut SqliteStore,
    context: &SessionContext,
    id: Option<Value>,
    request: &Value,
) -> (Value, Option<Delivery>) {
    match call_tool(store, context, request) {
        Ok((delivery, result)) => (
            success(
                id,
                json!({
                    "content": [{"type": "text", "text": result.to_string()}],
                    "structuredContent": result,
                    "isError": false
                }),
            ),
            Some(delivery),
        ),
        Err(error) => (
            success(
                id,
                json!({
                    "content": [{"type": "text", "text": error.to_string()}],
                    "structuredContent": {
                        "code": error.code(),
                        "retryable": error.retryable_flag(),
                        "details": error.details()
                    },
                    "isError": true
                }),
            ),
            None,
        ),
    }
}

#[derive(Debug)]
enum Delivery {
    WorkItem(String),
    External(String),
    Immediate,
}

fn call_tool(
    store: &mut SqliteStore,
    context: &SessionContext,
    request: &Value,
) -> Result<(Delivery, Value)> {
    match request.pointer("/params/name").and_then(Value::as_str) {
        Some("sessionbridge_await_work_items") => {
            call_await(store, context, request).map(|(id, value)| (Delivery::WorkItem(id), value))
        }
        Some("sessionbridge_await_external") => call_external_await(store, context, request)
            .map(|(id, value)| (Delivery::External(id), value)),
        Some("sessionbridge_agent_next") => {
            call_agent_next(store, context, request).map(|value| (Delivery::Immediate, value))
        }
        Some("sessionbridge_agent_conclude") => {
            call_agent_conclude(store, context, request).map(|value| (Delivery::Immediate, value))
        }
        Some("sessionbridge_chat_send") => {
            call_chat_send(store, context, request).map(|value| (Delivery::Immediate, value))
        }
        Some("sessionbridge_chat_history") => {
            call_chat_history(store, context, request).map(|value| (Delivery::Immediate, value))
        }
        Some("sessionbridge_chat_inbox") => {
            call_chat_inbox(store, context, request).map(|value| (Delivery::Immediate, value))
        }
        Some("sessionbridge_chat_ack") => {
            call_chat_ack(store, context, request).map(|value| (Delivery::Immediate, value))
        }
        Some("sessionbridge_chat_await") => {
            call_chat_await(store, context, request).map(|value| (Delivery::Immediate, value))
        }
        _ => Err(invalid("unknown SessionBridge MCP tool")),
    }
}

fn call_agent_next(
    store: &mut SqliteStore,
    context: &SessionContext,
    request: &Value,
) -> Result<Value> {
    let arguments = request
        .pointer("/params/arguments")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("tool arguments must be an object"))?;
    let limit = arguments.get("limit").and_then(Value::as_u64).unwrap_or(20);
    let limit = usize::try_from(limit).map_err(|_| invalid("limit is too large"))?;
    let preview = arguments
        .get("preview")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let service = AgentService::new(SystemClock);
    let result = if preview {
        service.next_preview(store, context, limit)?
    } else {
        service.next(store, context, limit)?
    };
    Ok(serde_json::to_value(result)?)
}

fn call_agent_conclude(
    store: &mut SqliteStore,
    context: &SessionContext,
    request: &Value,
) -> Result<Value> {
    let arguments = request
        .pointer("/params/arguments")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("tool arguments must be an object"))?;
    let outcome: ConclusionOutcome = arguments
        .get("outcome")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("outcome is required"))?
        .parse()
        .map_err(|error| invalid(format!("invalid conclude outcome: {error}")))?;
    let summary = required_string(arguments, "summary")?;
    let checked = required_string_array(arguments, "checked")?;
    if checked.is_empty() {
        return Err(invalid("checked must contain at least one area"));
    }
    let remaining_risks = optional_string_array(arguments, "remaining_risks")?;
    let mut open_signals = Vec::new();
    for (field, code) in [
        ("blocker_ids", "BLOCKER"),
        ("gap_ids", "GAP"),
        ("coordination_request_ids", "COORDINATION_REQUEST"),
    ] {
        for entity_id in optional_string_array(arguments, field)? {
            open_signals.push(OpenSignalDisposition {
                code: code.into(),
                entity_id: Some(entity_id),
                disposition: "requires_intervention".into(),
                rationale: format!("referenced by MCP conclude field {field}"),
            });
        }
    }
    let request_id = required_string(arguments, "request_id")?;
    let agent = AgentService::new(SystemClock);
    let next = agent.next(store, context, 20)?;
    let basis = agent.verify_completion_token(context, &next.tokens.completion)?;
    let result = CompletionService::new(SystemClock).conclude(
        store,
        context,
        ConcludeRequest {
            outcome,
            basis_completion_revision: basis.completion_revision,
            basis_event_cursor: basis.event_cursor,
            basis_plan_version: basis.plan_version,
            assessment: CompletionAssessment {
                summary,
                checked,
                open_signals,
                remaining_risks,
                expected_future_work: outcome == ConclusionOutcome::Blocked,
            },
            request_id,
        },
    )?;
    Ok(serde_json::to_value(result)?)
}

fn required_string(arguments: &serde_json::Map<String, Value>, field: &str) -> Result<String> {
    arguments
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| invalid(format!("{field} must be a non-empty string")))
}

fn required_string_array(
    arguments: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<Vec<String>> {
    arguments
        .get(field)
        .ok_or_else(|| invalid(format!("{field} is required")))?;
    optional_string_array(arguments, field)
}

fn optional_string_array(
    arguments: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<Vec<String>> {
    let Some(value) = arguments.get(field) else {
        return Ok(Vec::new());
    };
    value
        .as_array()
        .ok_or_else(|| invalid(format!("{field} must be an array")))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| invalid(format!("{field} must contain non-empty strings")))
        })
        .collect()
}

fn call_await(
    store: &mut SqliteStore,
    context: &SessionContext,
    request: &Value,
) -> Result<(String, Value)> {
    let params = request
        .pointer("/params")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("tools/call params must be an object"))?;
    let arguments = params
        .get("arguments")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("tool arguments must be an object"))?;
    let items = arguments
        .get("work_item_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("work_item_ids must be an array"))?
        .iter()
        .map(|value| {
            WorkItemId::from_str(
                value
                    .as_str()
                    .ok_or_else(|| invalid("work_item_ids must contain strings"))?,
            )
            .map_err(|_| invalid("work_item_ids contains an invalid ID"))
        })
        .collect::<Result<Vec<_>>>()?;
    let mode = match arguments
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("all")
    {
        "all" => AwaitMode::All,
        "any" => AwaitMode::Any,
        _ => return Err(invalid("await mode must be all or any")),
    };
    let request_id = arguments
        .get("request_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| invalid("request_id must be a non-empty string"))?;
    let service = AwaitService::new(SystemClock);
    let mut view = service.register(
        store,
        context,
        RegisterAwaitRequest {
            work_item_ids: items,
            mode,
            request_id,
        },
    )?;
    while view.status == "active" {
        std::thread::sleep(StdDuration::from_millis(250));
        view = service.poll(store, context, &view.wait_id)?;
    }
    let wait_id = view.wait_id.clone();
    let result = serde_json::to_value(view)?;
    Ok((wait_id, result))
}

fn call_external_await(
    store: &mut SqliteStore,
    context: &SessionContext,
    request: &Value,
) -> Result<(String, Value)> {
    let arguments = request
        .pointer("/params/arguments")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("tool arguments must be an object"))?;
    let detector_type = arguments
        .get("detector_type")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("detector_type is required"))?;
    let path = arguments
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("path is required"))?;
    let request_id = arguments
        .get("request_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| invalid("request_id must be a non-empty string"))?;
    let reassess_interval_seconds = arguments
        .get("reassess_interval_seconds")
        .and_then(Value::as_i64)
        .unwrap_or(30);
    let deadline_seconds = arguments.get("deadline_seconds").and_then(Value::as_i64);
    let service = ExternalWaitService::new(SystemClock);
    let mut view = service.register(
        store,
        context,
        RegisterExternalWaitRequest {
            detector: ExternalDetector {
                detector_type: detector_type.into(),
                path: PathBuf::from(path),
            },
            deadline_seconds,
            reassess_interval_seconds,
            request_id: request_id.into(),
        },
    )?;
    while view.status == "active" {
        std::thread::sleep(StdDuration::from_millis(250));
        view = service.poll(store, context, &view.wait_id)?;
    }
    let id = view.wait_id.clone();
    Ok((id, serde_json::to_value(view)?))
}

fn acknowledge_delivery(store: &mut SqliteStore, delivery: &Delivery) -> Result<()> {
    let delivered_at = unix_micros(SystemClock.now());
    let (table, wait_id) = match delivery {
        Delivery::WorkItem(id) => ("await_delivery_outbox", id),
        Delivery::External(id) => ("external_wait_delivery_outbox", id),
        Delivery::Immediate => return Ok(()),
    };
    store.connection().execute(
        &format!(
            "UPDATE {table}
         SET status='delivered',delivery_attempts=delivery_attempts+1,delivered_at=?2
         WHERE wait_id=?1 AND status='pending'"
        ),
        params![wait_id, delivered_at],
    )?;
    Ok(())
}

fn call_chat_send(
    store: &mut SqliteStore,
    context: &SessionContext,
    request: &Value,
) -> Result<Value> {
    let arguments = request
        .pointer("/params/arguments")
        .cloned()
        .ok_or_else(|| invalid("tool arguments must be an object"))?;
    let message: SendChatMessageRequest = serde_json::from_value(arguments)?;
    serde_json::to_value(ChatService::new(SystemClock).send_agent(store, context, message)?)
        .map_err(Into::into)
}

fn call_chat_history(
    store: &mut SqliteStore,
    context: &SessionContext,
    request: &Value,
) -> Result<Value> {
    let arguments = request
        .pointer("/params/arguments")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("tool arguments must be an object"))?;
    let since = arguments.get("since").and_then(Value::as_i64).unwrap_or(0);
    let limit = arguments.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
    AgentService::new(SystemClock).whoami(store, context)?;
    serde_json::to_value(ChatService::new(SystemClock).history(
        store.connection(),
        context.task_id,
        since,
        limit,
    )?)
    .map_err(Into::into)
}

fn call_chat_inbox(
    store: &mut SqliteStore,
    context: &SessionContext,
    request: &Value,
) -> Result<Value> {
    let limit = request
        .pointer("/params/arguments/limit")
        .and_then(Value::as_u64)
        .unwrap_or(20) as usize;
    serde_json::to_value(ChatService::new(SystemClock).inbox(store.connection(), context, limit)?)
        .map_err(Into::into)
}

fn call_chat_ack(
    store: &mut SqliteStore,
    context: &SessionContext,
    request: &Value,
) -> Result<Value> {
    let ids = request
        .pointer("/params/arguments/message_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("message_ids must be an array"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid("message_ids must contain strings"))
        })
        .collect::<Result<Vec<_>>>()?;
    serde_json::to_value(ChatService::new(SystemClock).ack_agent(store, context, ids)?)
        .map_err(Into::into)
}

fn call_chat_await(
    store: &mut SqliteStore,
    context: &SessionContext,
    request: &Value,
) -> Result<Value> {
    let limit = request
        .pointer("/params/arguments/limit")
        .and_then(Value::as_u64)
        .unwrap_or(20) as usize;
    let directed_only = request
        .pointer("/params/arguments/directed_only")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    serde_json::to_value(ChatService::new(SystemClock).await_agent(
        store,
        context,
        limit,
        directed_only,
    )?)
    .map_err(Into::into)
}

fn chat_send_tool_definition() -> Value {
    json!({
        "name":"sessionbridge_chat_send",
        "description":"Post a concise, auditable message to the Task workgroup. Use for intent, progress, questions, decisions, warnings, and handoffs; chat does not replace Claims or approvals.",
        "inputSchema":{"type":"object","properties":{
            "kind":{"type":"string","enum":["progress","intent","question","decision","warning","handoff","note"],"default":"note"},
            "body":{"type":"string","minLength":1,"maxLength":8000},
            "reply_to_message_id":{"type":"string"},
            "mentions":{"type":"array","items":{"type":"string"},"default":[]},
            "related_entities":{"type":"array","items":{"type":"object","properties":{"entity_type":{"type":"string"},"entity_id":{"type":"string"}},"required":["entity_type","entity_id"],"additionalProperties":false},"default":[]},
            "request_id":{"type":"string","minLength":1}
        },"required":["body","request_id"],"additionalProperties":false}
    })
}

fn chat_history_tool_definition() -> Value {
    json!({
        "name":"sessionbridge_chat_history",
        "description":"Read Task workgroup messages after an Event cursor without polling the model.",
        "inputSchema":{"type":"object","properties":{
            "since":{"type":"integer","minimum":0,"default":0},
            "limit":{"type":"integer","minimum":1,"maximum":200,"default":50}
        },"additionalProperties":false}
    })
}

fn chat_inbox_tool_definition() -> Value {
    json!({"name":"sessionbridge_chat_inbox","description":"Read directed-first unread Task chat messages. Reading does not acknowledge them.","inputSchema":{"type":"object","properties":{"limit":{"type":"integer","minimum":1,"maximum":200,"default":20}},"additionalProperties":false}})
}

fn chat_ack_tool_definition() -> Value {
    json!({"name":"sessionbridge_chat_ack","description":"Acknowledge exactly the chat messages that this Session has processed.","inputSchema":{"type":"object","properties":{"message_ids":{"type":"array","minItems":1,"maxItems":200,"items":{"type":"string"}}},"required":["message_ids"],"additionalProperties":false}})
}

fn chat_await_tool_definition() -> Value {
    json!({"name":"sessionbridge_chat_await","description":"Block without model polling until unread directed chat arrives (or any unread chat when directed_only is false).","inputSchema":{"type":"object","properties":{"limit":{"type":"integer","minimum":1,"maximum":200,"default":20},"directed_only":{"type":"boolean","default":true}},"additionalProperties":false}})
}

fn agent_next_tool_definition() -> Value {
    json!({
        "name":"sessionbridge_agent_next",
        "description":"Read compact coordination state and executable operation references. Preview mode does not update heartbeat.",
        "inputSchema":{
            "type":"object",
            "properties":{
                "limit":{"type":"integer","minimum":1,"maximum":100,"default":20},
                "preview":{"type":"boolean","default":false}
            },
            "additionalProperties":false
        }
    })
}

fn agent_conclude_tool_definition() -> Value {
    json!({
        "name":"sessionbridge_agent_conclude",
        "description":"Submit a typed semantic completion assessment; SessionBridge acquires and validates the current completion scope internally.",
        "inputSchema":{
            "type":"object",
            "properties":{
                "outcome":{"type":"string","enum":["no_further_work","blocked"]},
                "summary":{"type":"string","minLength":1},
                "checked":{"type":"array","minItems":1,"items":{"type":"string","minLength":1}},
                "remaining_risks":{"type":"array","items":{"type":"string","minLength":1}},
                "blocker_ids":{"type":"array","items":{"type":"string","minLength":1}},
                "gap_ids":{"type":"array","items":{"type":"string","minLength":1}},
                "coordination_request_ids":{"type":"array","items":{"type":"string","minLength":1}},
                "request_id":{"type":"string","minLength":1}
            },
            "required":["outcome","summary","checked","request_id"],
            "additionalProperties":false
        }
    })
}

fn external_await_tool_definition() -> Value {
    json!({
        "name":"sessionbridge_await_external",
        "description":"Suspend until a persisted external detector is satisfied, reaches its deadline, or loses observability. Reuse request_id after disconnect.",
        "inputSchema":{"type":"object","properties":{
            "detector_type":{"type":"string","enum":["file_exists","file_absent"]},
            "path":{"type":"string","minLength":1},
            "deadline_seconds":{"type":"integer","minimum":1},
            "reassess_interval_seconds":{"type":"integer","minimum":1,"default":30},
            "request_id":{"type":"string","minLength":1}
        },"required":["detector_type","path","request_id"],"additionalProperties":false}
    })
}

fn await_tool_definition() -> Value {
    json!({
        "name": "sessionbridge_await_work_items",
        "description": "Suspend this tool call until the selected SessionBridge WorkItems are accepted, become impossible, or the Task terminates. The request remains open and returns one durable callback; do not poll while it is active.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "work_item_ids": {"type": "array", "items": {"type": "string"}, "minItems": 1},
                "mode": {"type": "string", "enum": ["all", "any"], "default": "all"},
                "request_id": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Stable caller-generated idempotency key. Reuse it after disconnect or server restart to resume the same durable wait."
                }
            },
            "required": ["work_item_ids", "request_id"],
            "additionalProperties": false
        }
    })
}

fn success(id: Option<Value>, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "result": result})
}

fn failure(id: Option<Value>, code: i64, message: &str, data: Option<Value>) -> Value {
    json!({"jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "error": {"code": code, "message": message, "data": data}})
}

fn invalid(message: impl Into<String>) -> SessionBridgeError {
    SessionBridgeError::domain(ErrorCode::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deferred_tool_requires_a_stable_request_id_for_reconnect() {
        let definition = await_tool_definition();
        assert_eq!(
            definition.pointer("/inputSchema/required"),
            Some(&json!(["work_item_ids", "request_id"]))
        );
        assert_eq!(
            definition.pointer("/inputSchema/properties/request_id/minLength"),
            Some(&json!(1))
        );
        let external = external_await_tool_definition();
        assert_eq!(external["name"], "sessionbridge_await_external");
        assert!(
            external["inputSchema"]["required"]
                .as_array()
                .unwrap()
                .contains(&json!("request_id"))
        );
        assert_eq!(
            chat_send_tool_definition()["name"],
            "sessionbridge_chat_send"
        );
        assert_eq!(
            chat_history_tool_definition()["name"],
            "sessionbridge_chat_history"
        );
        assert_eq!(
            chat_inbox_tool_definition()["name"],
            "sessionbridge_chat_inbox"
        );
        assert_eq!(chat_ack_tool_definition()["name"], "sessionbridge_chat_ack");
        assert_eq!(
            chat_await_tool_definition()["name"],
            "sessionbridge_chat_await"
        );
    }

    #[test]
    fn protocol_errors_preserve_json_rpc_request_id() {
        let response = failure(Some(json!(17)), -32601, "method not found", None);
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 17);
        assert_eq!(response["error"]["code"], -32601);
    }
}
