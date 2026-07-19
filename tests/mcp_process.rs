use std::{
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
    thread,
    time::Duration,
};

use rusqlite::{OptionalExtension, params};
use serde_json::json;
use sessionbridge::{
    domain::{ParticipantId, SessionContext, SessionId, TaskId, WorkspaceId},
    storage::SqliteStore,
};
use sha2::{Digest, Sha256};
use tempfile::tempdir;

#[test]
fn external_deferred_call_survives_server_crash_and_replays_once() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("state.db");
    SqliteStore::migrate(&db).unwrap();
    let store = SqliteStore::open(&db).unwrap();
    let workspace = WorkspaceId::new();
    let task = TaskId::new();
    let session = SessionId::new();
    let participant = ParticipantId::new();
    let token = format!("sb_session_{}", "A".repeat(43));
    store.connection().execute("INSERT INTO workspaces(id,name,repository_identity,created_at) VALUES(?1,'mcp','repo',0)",[workspace.to_string()]).unwrap();
    store.connection().execute("INSERT INTO tasks(id,workspace_id,title,phase,autonomy_policy,completion_policy,created_at,updated_at) VALUES(?1,?2,'mcp','active','open','all_workers_concluded',0,0)",params![task.to_string(),workspace.to_string()]).unwrap();
    store.connection().execute("INSERT INTO agent_sessions(id,task_id,token_hash,label,protocol_version,last_seen_at,created_at,updated_at) VALUES(?1,?2,?3,'agent',1,0,0,0)",params![session.to_string(),task.to_string(),Sha256::digest(token.as_bytes()).to_vec()]).unwrap();
    store.connection().execute("INSERT INTO task_participants(id,task_id,session_id,kind,completion_epoch,state,joined_at,created_at,updated_at) VALUES(?1,?2,?3,'worker',1,'active',0,0,0)",params![participant.to_string(),task.to_string(),session.to_string()]).unwrap();
    let context_path = dir.path().join("context.json");
    let context = SessionContext {
        schema_version: 1,
        protocol_version: 1,
        workspace_id: workspace,
        repository_instance_id: "repo".into(),
        task_id: task,
        session_id: session,
        token,
        database_path: db.display().to_string(),
    };
    std::fs::write(&context_path, serde_json::to_vec(&context).unwrap()).unwrap();
    let target = dir.path().join("ready");
    let request=json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"sessionbridge_await_external","arguments":{"detector_type":"file_exists","path":target,"reassess_interval_seconds":1,"request_id":"crash-replay"}}}).to_string()+"\n";
    let binary = env!("CARGO_BIN_EXE_sessionbridge");
    let mut first = Command::new(binary)
        .args(["--context", context_path.to_str().unwrap(), "mcp", "serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    first
        .stdin
        .as_mut()
        .unwrap()
        .write_all(request.as_bytes())
        .unwrap();
    first.stdin.as_mut().unwrap().flush().unwrap();
    let wait_id = loop {
        if let Some(id) = store
            .connection()
            .query_row(
                "SELECT id FROM external_waits WHERE request_id='crash-replay'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .unwrap()
        {
            break id;
        }
        thread::sleep(Duration::from_millis(20));
    };
    first.kill().unwrap();
    first.wait().unwrap();
    let state: String = store
        .connection()
        .query_row(
            "SELECT status FROM external_waits WHERE id=?1",
            [&wait_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state, "active");
    std::fs::write(&target, b"ready").unwrap();
    thread::sleep(Duration::from_millis(1100));
    let mut second = Command::new(binary)
        .args(["--context", context_path.to_str().unwrap(), "mcp", "serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    second
        .stdin
        .as_mut()
        .unwrap()
        .write_all(request.as_bytes())
        .unwrap();
    second.stdin.as_mut().unwrap().flush().unwrap();
    let mut line = String::new();
    BufReader::new(second.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    for _ in 0..100 {
        let delivered: bool = store
            .connection()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM external_wait_delivery_outbox WHERE wait_id=?1 AND status='delivered')",
                [&wait_id],
                |row| row.get(0),
            )
            .unwrap();
        if delivered {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    second.kill().unwrap();
    second.wait().unwrap();
    let response: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(response["id"], 7);
    assert_eq!(response["result"]["structuredContent"]["wait_id"], wait_id);
    assert_eq!(
        response["result"]["structuredContent"]["status"],
        "satisfied"
    );
    let delivery: (String, i64) = store
        .connection()
        .query_row(
            "SELECT status,delivery_attempts FROM external_wait_delivery_outbox WHERE wait_id=?1",
            [wait_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(delivery, ("delivered".into(), 1));
    let completions: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE event_type='agent.external_wait_completed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(completions, 1);
}
