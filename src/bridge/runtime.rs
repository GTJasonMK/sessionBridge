use super::{
    ipc::{Hub, SharedStore, read_frame, write_frame},
    *,
};
use rusqlite::params;
use serde_json::json;
use std::{
    io::BufReader,
    process::{Command, Stdio},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

pub fn stop(hub: &Hub, shared: &SharedStore, member: &str) -> Result<Value> {
    {
        let store = shared
            .lock()
            .map_err(|_| error("INTERNAL", "store poisoned"))?;
        let m = store.member(member)?;
        if m["role"] == "boss" || m["state"] == "removed" {
            return Err(error("FORBIDDEN", "member cannot be stopped"));
        }
        let running = hub
            .running
            .lock()
            .map_err(|_| error("INTERNAL", "runtime registry poisoned"))?;
        store.transaction(|| {
            store
                .db
                .execute("UPDATE members SET token_hash=NULL WHERE id=?", [member])?;
            store.db.execute(
                "UPDATE deliveries SET state='queued' WHERE member_id=? AND state='delivered'",
                [member],
            )?;
            store.db.execute(
                "UPDATE approvals SET decision='deny' WHERE member_id=? AND decision IS NULL",
                [member],
            )?;
            if !running.contains_key(member) {
                store
                    .db
                    .execute("UPDATE members SET state='offline' WHERE id=?", [member])?;
            }
            store.system(
                text(&m, "room_id")?,
                &format!("正在停止 {}", text(&m, "name")?),
            )?;
            Ok(())
        })?;
        if let Some(sender) = running.get(member) {
            // A closed receiver means execution ended; still wait for its database cleanup.
            if sender.send(()).is_err() {
                eprintln!("member {member} is already finishing");
            }
        }
    }
    hub.notify();
    for _ in 0..60 {
        if !hub
            .running
            .lock()
            .map_err(|_| error("INTERNAL", "runtime registry poisoned"))?
            .contains_key(member)
        {
            return Ok(json!({"stopped":true}));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(error(
        "STOP_PENDING",
        "process cleanup is not complete; wait before reassigning work",
    ))
}

pub fn initialize(
    hub: Arc<Hub>,
    shared: SharedStore,
    member: &str,
    request: &Value,
    actor: &str,
) -> Result<Value> {
    if actor != "boss" {
        return Err(error("FORBIDDEN", "boss required"));
    }
    let _lifecycle = hub
        .lifecycle
        .lock()
        .map_err(|_| error("INTERNAL", "lifecycle lock poisoned"))?;
    if hub.is_stopping() {
        return Err(error("SHUTTING_DOWN", "service is shutting down"));
    }
    let store = shared
        .lock()
        .map_err(|_| error("INTERNAL", "store poisoned"))?;
    let m = store.member(member)?;
    let mut config = serde_json::Map::new();
    config.insert("cli".into(), json!(m["cli"]));
    config.insert("provider".into(), json!(m["provider"]));
    config.insert("model".into(), json!(m["model"]));
    let fingerprint = hash(&format!(
        "member.initialize:{}:{}",
        member,
        serde_json::to_string(&config)?
    ));
    let request_id = text(request, "request_id")?;
    let result = store.apply_idempotent(actor, request_id, fingerprint, || {
        if hub.is_stopping() {
            return Err(error("SHUTTING_DOWN", "service is shutting down"));
        }
        let m = store.member(member)?;
        if m["role"] == "boss"
            || !matches!(m["state"].as_str(), Some("configured" | "offline"))
        {
            return Err(error(
                "MEMBER_STATE",
                "member is already initialized or removed",
            ));
        }
        let room = text(&m, "room_id")?.to_owned();
        if store.room(&room)?["state"] != "active" {
            return Err(error(
                "ROOM_INACTIVE",
                "resume the room before initializing",
            ));
        }
        if hub
            .running
            .lock()
            .map_err(|_| error("INTERNAL", "registry poisoned"))?
            .contains_key(member)
        {
            return Err(error("MEMBER_RUNNING", "wait for old process to exit"));
        }
        let cwd = git::prepare(&store, member)?;
        let generation = m["generation"]
            .as_i64()
            .and_then(|n| n.checked_add(1))
            .ok_or_else(|| error("DATABASE", "invalid connection generation"))?;
        let token = secret()?;
        let challenge = secret()?;
        let directory = store.directory.join("members").join(member).join("runtime");
        let context = Context {
            workspace: store.root.to_string_lossy().into_owned(),
            member: member.into(),
            generation,
            token: token.clone(),
        };
        let context_path = directory.join("context.json");
        let context_bytes = serde_json::to_vec(&context)?;
        let mut command = provider::prepare(
            text(&m, "cli")?,
            text(&m, "provider")?,
            text(&m, "model")?,
            &directory,
            &context_path,
        )?;
        command.current_dir(&cwd);
        let prompt = format!(
            "JOIN protocol=2 generation={generation} challenge={challenge}\nYou are {} ({}) in workgroup {}. Read the sessionbridge skill using skill.read, call inspect, then ready with this challenge, generation, protocol=2 and a UUID request_id. After recovery, inspect unfinished work and do not replay side effects blindly. Do not create hidden subagents. End this turn after joining.",
            text(&m, "name")?,
            text(&m, "role")?,
            text(&m, "room_id")?
        );
        // Restore interrupted deliveries, then process them only after the new handshake.
        store.db.execute(
            "UPDATE deliveries SET state='queued' WHERE member_id=? AND state='delivered'",
            [member],
        )?;
        store.db.execute(
            "UPDATE members SET state='starting',generation=?,token_hash=?,challenge=?,inspected=0,skill_read=0,error=NULL WHERE id=?",
            params![generation, hash(&token), challenge, member],
        )?;
        private_write(&context_path, &context_bytes)?;
        store.system(&room, &format!("正在初始化 {}", text(&m, "name")?))?;
        spawn(hub.clone(),shared.clone(),member.to_owned(),command,prompt,true)
            .map(|_|json!({"member":member,"generation":generation,"state":"starting"}))
    })?;
    hub.notify();
    Ok(result)
}

pub fn schedule(hub: &Arc<Hub>) -> Result<()> {
    let _lifecycle = hub
        .lifecycle
        .lock()
        .map_err(|_| error("INTERNAL", "lifecycle lock poisoned"))?;
    let stores: Vec<_> = hub
        .stores
        .lock()
        .map_err(|_| error("INTERNAL", "registry poisoned"))?
        .values()
        .cloned()
        .collect();
    for shared in stores {
        let members = {
            let store = shared
                .lock()
                .map_err(|_| error("INTERNAL", "store poisoned"))?;
            store.rows("SELECT m.id FROM members m JOIN rooms r ON r.id=m.room_id
            WHERE m.role!='boss' AND m.state='ready' AND m.token_hash IS NOT NULL AND r.state='active'
            AND EXISTS(SELECT 1 FROM members c WHERE c.room_id=r.id AND c.role='coordinator' AND c.state IN ('ready','busy') AND c.token_hash IS NOT NULL)
            AND EXISTS(SELECT 1 FROM members e WHERE e.room_id=r.id AND e.role='executor' AND e.state IN ('ready','busy') AND e.token_hash IS NOT NULL)
            AND EXISTS(SELECT 1 FROM deliveries d WHERE d.member_id=m.id AND d.state='queued')", &[])?
        };
        for row in members {
            if hub.is_stopping() {
                return Ok(());
            }
            let member = text(&row, "id")?;
            let (m, messages, directory) = {
                let store = shared
                    .lock()
                    .map_err(|_| error("INTERNAL", "store poisoned"))?;
                let m = store.member(member)?;
                let messages = store.rows(
                    "SELECT m.* FROM messages m JOIN deliveries d ON m.id=d.message_id WHERE d.member_id=? AND d.state='queued' ORDER BY m.seq LIMIT 20",
                    &[&member],
                )?;
                let directory = store.directory.join("members").join(member).join("runtime");
                (m, messages, directory)
            };
            if messages.is_empty() {
                continue;
            }
            let launch = provider::launch(text(&m, "cli")?, text(&m, "model")?, &directory);
            let mut command = match launch {
                Ok(c) => c,
                Err(e) => {
                    let store = shared
                        .lock()
                        .map_err(|_| error("INTERNAL", "store poisoned"))?;
                    store.transaction(|| {
                        store.db.execute(
                            "UPDATE members SET state='offline',token_hash=NULL,error=? WHERE id=?",
                            params![e.to_string(), member],
                        )?;
                        store
                            .system(text(&m, "room_id")?, &format!("{}：{e}", text(&m, "name")?))?;
                        Ok(())
                    })?;
                    hub.notify();
                    continue;
                }
            };
            command.current_dir(text(&m, "cwd")?);
            let prompt = format!(
                "Process these addressed group messages in order. Inspect current work before acting. Use message.send for addressed replies and message.ack for processed IDs. Do not reply to acknowledgements or poll; end your turn when idle.\n{}",
                serde_json::to_string(&messages)?
            );
            let scheduled = {
                let store = shared
                    .lock()
                    .map_err(|_| error("INTERNAL", "store poisoned"))?;
                store.transaction(|| {
                if hub
                    .running
                    .lock()
                    .map_err(|_| error("INTERNAL", "runtime registry poisoned"))?
                    .contains_key(member)
                {
                    return Err(error(
                        "MEMBER_RUNNING",
                        "member is already processing a turn",
                    ));
                }
                if hub.is_stopping()
                    || store.room(text(&m, "room_id")?)?["state"] != "active"
                    || store.member(member)?["state"] != "ready"
                {
                    return Err(error(
                        "ROOM_INACTIVE",
                        "member or room stopped before scheduling",
                    ));
                }
                for message in &messages {
                    if store.db.execute(
                        "UPDATE deliveries SET state='delivered' WHERE message_id=? AND member_id=? AND state='queued'",
                        params![text(message, "id")?, member],
                    )? != 1
                    {
                        return Err(error("DELIVERY", "queued message changed before scheduling"));
                    }
                }
                store
                    .db
                    .execute("UPDATE members SET state='busy' WHERE id=? AND state='ready'", [member])?;
                Ok(())
                })
            };
            match scheduled {
                Ok(()) => {}
                Err(e) if e.code == "MEMBER_RUNNING" => continue,
                Err(e) => return Err(e),
            }
            if let Err(e) = spawn(
                hub.clone(),
                shared.clone(),
                member.into(),
                command,
                prompt,
                false,
            ) {
                let store = shared
                    .lock()
                    .map_err(|_| error("INTERNAL", "store poisoned"))?;
                store.transaction(|| {
                    store.db.execute(
                        "UPDATE deliveries SET state='queued' WHERE member_id=? AND state='delivered'",
                        [member],
                    )?;
                    store.db.execute(
                        "UPDATE members SET state='ready',error=? WHERE id=? AND state='busy'",
                        params![e.to_string(), member],
                    )?;
                    Ok(())
                })?;
                return Err(e);
            }
            hub.notify();
        }
    }
    Ok(())
}

fn spawn(
    hub: Arc<Hub>,
    shared: SharedStore,
    member: String,
    command: Command,
    prompt: String,
    joining: bool,
) -> Result<()> {
    let (stop_tx, stop_rx) = mpsc::channel();
    {
        let mut running = hub
            .running
            .lock()
            .map_err(|_| error("INTERNAL", "registry poisoned"))?;
        if running.contains_key(&member) {
            return Err(error("MEMBER_RUNNING", "member already has a running turn"));
        }
        running.insert(member.clone(), stop_tx);
    }
    let registry = hub.clone();
    let cleanup_hub = hub.clone();
    let key = member.clone();
    let launched=std::thread::Builder::new().name(format!("member-{member}")).spawn(move || {
        let result=run(&hub,&shared,&member,command,&prompt,joining,&stop_rx);
        let cleanup=(|| ->Result<()> {
            let store=shared.lock().map_err(|_|error("INTERNAL","store poisoned"))?;
            let m=store.member(&member)?;
            let revoked_before_cleanup:bool=store.db.query_row("SELECT token_hash IS NULL FROM members WHERE id=?",[&member],|r|r.get(0))?;
            let failure=match result {
                Ok(()) if m["state"]=="starting"=>Some(error("HANDSHAKE","CLI exited without validated READY")),
                Ok(())=>None,
                Err(e) if e.code=="STOPPED"&&revoked_before_cleanup=>None,
                Err(e)=>Some(e),
            };
            store.transaction(|| {
                let revoked:bool=store.db.query_row("SELECT token_hash IS NULL FROM members WHERE id=?",[&member],|r|r.get(0))?;
                if let Some(e)=failure {
                    store.db.execute("UPDATE members SET state='offline',token_hash=NULL,error=? WHERE id=?",params![e.to_string(),member])?;
                    store.system(text(&m,"room_id")?,&format!("{}：{e}",text(&m,"name")?))?;
                } else {
                    store.db.execute("UPDATE members SET state=? WHERE id=?",params![if revoked{"offline"}else{"ready"},member])?;
                }
                store.db.execute("UPDATE approvals SET decision='deny' WHERE member_id=? AND decision IS NULL",[&member])?;
                Ok(())
            })?;
            Ok(())
        })();
        if let Err(e)=cleanup {eprintln!("runtime cleanup failed for {member}: {e}");}
        if let Ok(mut running)=cleanup_hub.running.lock(){running.remove(&member);}
        hub.notify();
    });
    if let Err(e) = launched {
        registry
            .running
            .lock()
            .map_err(|_| error("INTERNAL", "registry poisoned"))?
            .remove(&key);
        return Err(e.into());
    }
    Ok(())
}

fn run(
    hub: &Hub,
    shared: &SharedStore,
    member: &str,
    mut command: Command,
    prompt: &str,
    joining: bool,
    stop: &mpsc::Receiver<()>,
) -> Result<()> {
    let store = shared
        .lock()
        .map_err(|_| error("INTERNAL", "store poisoned"))?;
    let m = store.member(member)?;
    if stop.try_recv().is_ok() || !matches!(m["state"].as_str(), Some("starting" | "busy")) {
        return Err(error("STOPPED", "turn was cancelled before launch"));
    }
    let cli = text(&m, "cli")?;
    let room = text(&m, "room_id")?;
    let generation = m["generation"]
        .as_i64()
        .ok_or_else(|| error("DATABASE", "invalid connection generation"))?;
    let mut session = m["session"].as_str().map(str::to_owned);
    if cli == "claude"
        && let Some(session) = m["session"].as_str()
    {
        command.args(["--resume", session]);
    }
    drop(store);
    // Each process owns its children, so cancellation can terminate its whole process group.
    process::configure(&mut command);
    let stderr = tempfile::tempfile()?;
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(stderr.try_clone()?)
        .spawn()?;
    let pid = child.id();
    let registration = (|| -> Result<()> {
        shared
            .lock()
            .map_err(|_| error("INTERNAL", "store poisoned"))?
            .db
            .execute(
                "INSERT INTO processes(member_id,pid,fingerprint) VALUES(?,?,?)",
                params![member, pid, process::fingerprint(pid)?],
            )?;
        Ok(())
    })();
    if let Err(e) = registration {
        process::terminate(&mut child)?;
        return Err(e);
    }
    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| error("PIPE", "stdin unavailable"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| error("PIPE", "stdout unavailable"))?;
    let (tx, rx) = mpsc::sync_channel(128);
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let frame = read_frame(&mut reader);
            let end = !matches!(frame, Ok(Some(_)));
            if tx.send(frame).is_err() || end {
                break;
            }
        }
    });
    let mut pending_approvals: Vec<(String, Value)> = Vec::new();
    let mut output_id: Option<String> = None;
    let started = Instant::now();
    let mut completed = false;
    let mut streamed = false;
    let mut resumed_from_stale_thread = false;
    let result = (|| -> Result<()> {
        if cli == "codex" {
            write_frame(
                &mut input,
                &json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"sessionbridge","version":"2"},"capabilities":{}}}),
            )?;
        } else {
            write_frame(
                &mut input,
                &json!({"type":"control_request","request_id":"host-initialize","request":{"subtype":"initialize","hooks":{}}}),
            )?;
            write_frame(
                &mut input,
                &json!({"type":"user","message":{"role":"user","content":prompt}}),
            )?;
        }
        loop {
            if stop.try_recv().is_ok() {
                return Err(error("STOPPED", "已停止；恢复前请核实未完成操作"));
            }
            if joining && started.elapsed() > Duration::from_secs(120) {
                return Err(error("HANDSHAKE_TIMEOUT", "初始化超过 120 秒"));
            }
            for (approval, event) in pending_approvals.clone() {
                let decision: Option<String> = shared
                    .lock()
                    .map_err(|_| error("INTERNAL", "store poisoned"))?
                    .db
                    .query_row(
                        "SELECT decision FROM approvals WHERE id=?",
                        [&approval],
                        |r| r.get(0),
                    )?;
                if let Some(decision) = decision {
                    let allow = decision == "allow";
                    let response = if cli == "claude" {
                        json!({"type":"control_response","response":{"subtype":"success","request_id":event["request_id"],"response":if allow {json!({"behavior":"allow","updatedInput":event["request"]["input"]})} else {json!({"behavior":"deny","message":"老板拒绝了此操作"})}}})
                    } else if event["method"] == "item/permissions/requestApproval" {
                        json!({"id":event["id"],"result":{"scope":"turn","permissions":if allow {event["params"]["permissions"].clone()} else {json!({})}}})
                    } else {
                        json!({"id":event["id"],"result":{"decision":if allow {"accept"} else {"decline"}}})
                    };
                    write_frame(&mut input, &response)?;
                    pending_approvals.retain(|(id, _)| id != &approval);
                }
            }
            let event = match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Ok(Some(v))) => v,
                Ok(Ok(None)) => break,
                Ok(Err(e)) => return Err(e),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            if cli == "codex" {
                if !event["error"].is_null() {
                    if event["id"].as_i64() == Some(2)
                        && session.is_some()
                        && !resumed_from_stale_thread
                    {
                        resumed_from_stale_thread = true;
                        session = None;
                        shared
                            .lock()
                            .map_err(|_| error("INTERNAL", "store poisoned"))?
                            .db
                            .execute("UPDATE members SET session=NULL WHERE id=?", [member])?;
                        write_frame(
                            &mut input,
                            &json!({
                                "id": 2,
                                "method": "thread/start",
                                "params": codex_thread_params(&command, &m)?
                            }),
                        )?;
                        continue;
                    }
                    return Err(error("CLI_PROTOCOL", event["error"].to_string()));
                }
                match event["id"].as_i64() {
                    Some(1) => {
                        write_frame(&mut input, &json!({"method":"initialized"}))?;
                        let mut params = codex_thread_params(&command, &m)?;
                        let method = if let Some(session) = session.as_deref() {
                            params["threadId"] = json!(session);
                            "thread/resume"
                        } else {
                            "thread/start"
                        };
                        write_frame(&mut input, &json!({"id":2,"method":method,"params":params}))?;
                    }
                    Some(2) => {
                        let session = text(&event["result"]["thread"], "id")?;
                        shared
                            .lock()
                            .map_err(|_| error("INTERNAL", "store poisoned"))?
                            .db
                            .execute(
                                "UPDATE members SET session=? WHERE id=?",
                                params![session, member],
                            )?;
                        write_frame(
                            &mut input,
                            &json!({"id":3,"method":"turn/start","params":{"threadId":session,"input":[{"type":"text","text":prompt}]}}),
                        )?;
                    }
                    _ => {}
                }
                let method = event["method"].as_str().unwrap_or("");
                if method == "item/agentMessage/delta" {
                    append(
                        hub,
                        shared,
                        member,
                        room,
                        &mut output_id,
                        event["params"]["delta"]
                            .as_str()
                            .ok_or_else(|| error("CLI_PROTOCOL", "missing Codex text delta"))?,
                    )?;
                    streamed = true;
                }
                if method == "item/completed" {
                    let item = &event["params"]["item"];
                    if item["type"] == "agentMessage" {
                        if !streamed {
                            append(
                                hub,
                                shared,
                                member,
                                room,
                                &mut output_id,
                                item["text"].as_str().ok_or_else(|| {
                                    error("CLI_PROTOCOL", "missing Codex message text")
                                })?,
                            )?;
                        }
                        output_id = None;
                        streamed = false;
                    } else if item["type"] != "reasoning" && item["type"] != "userMessage" {
                        tool_event(hub, shared, member, room, item)?;
                    }
                }
                if method.ends_with("requestApproval") {
                    pending_approvals.push((
                        approval(hub, shared, member, room, generation, &event["params"])?,
                        event.clone(),
                    ));
                } else if event.get("id").is_some() && !method.is_empty() {
                    write_frame(
                        &mut input,
                        &json!({"id":event["id"],"error":{"code":-32601,"message":"Unsupported host request; use group message to ask the boss"}}),
                    )?;
                }
                if method == "turn/completed" {
                    if event["params"]["turn"]["status"] != "completed" {
                        return Err(error("TURN_FAILED", event["params"]["turn"].to_string()));
                    }
                    completed = true;
                    break;
                }
            } else {
                match event["type"].as_str().unwrap_or("") {
                    "system" if event["subtype"] == "init" => {
                        let session = text(&event, "session_id")?;
                        shared
                            .lock()
                            .map_err(|_| error("INTERNAL", "store poisoned"))?
                            .db
                            .execute(
                                "UPDATE members SET session=? WHERE id=?",
                                params![session, member],
                            )?;
                    }
                    "stream_event" if event["event"]["delta"]["type"] == "text_delta" => {
                        append(
                            hub,
                            shared,
                            member,
                            room,
                            &mut output_id,
                            event["event"]["delta"]["text"].as_str().ok_or_else(|| {
                                error("CLI_PROTOCOL", "missing Claude text delta")
                            })?,
                        )?;
                        streamed = true;
                    }
                    "assistant" => {
                        if let Some(blocks) = event["message"]["content"].as_array() {
                            for block in blocks {
                                if block["type"] == "text" && !streamed {
                                    append(
                                        hub,
                                        shared,
                                        member,
                                        room,
                                        &mut output_id,
                                        block["text"].as_str().ok_or_else(|| {
                                            error("CLI_PROTOCOL", "missing Claude message text")
                                        })?,
                                    )?;
                                } else if block["type"] == "tool_use" {
                                    tool_event(hub, shared, member, room, block)?;
                                }
                            }
                        }
                        output_id = None;
                        streamed = false;
                    }
                    "user" => {
                        if let Some(blocks) = event["message"]["content"].as_array() {
                            for block in blocks {
                                if block["type"] == "tool_result" {
                                    tool_event(hub, shared, member, room, block)?;
                                }
                            }
                        }
                    }
                    "control_request" => {
                        if event["request"]["subtype"] == "can_use_tool" {
                            pending_approvals.push((
                                approval(hub, shared, member, room, generation, &event["request"])?,
                                event,
                            ));
                        } else {
                            write_frame(
                                &mut input,
                                &json!({"type":"control_response","response":{"subtype":"error","request_id":event["request_id"],"error":"Unsupported control request"}}),
                            )?;
                        }
                    }
                    "result" => {
                        if event["is_error"] == true {
                            return Err(error(
                                "TURN_FAILED",
                                event
                                    .get("result")
                                    .or(event.get("errors"))
                                    .unwrap_or(&Value::Null)
                                    .to_string(),
                            ));
                        }
                        completed = true;
                        break;
                    }
                    _ => {}
                }
            }
        }
        if !completed {
            return Err(error("CLI_EXIT", "CLI exited before completing its turn"));
        }
        Ok(())
    })();
    process::terminate(&mut child)?;
    shared
        .lock()
        .map_err(|_| error("INTERNAL", "store poisoned"))?
        .db
        .execute(
            "DELETE FROM processes WHERE member_id=? AND pid=?",
            params![member, pid],
        )?;
    if result.is_err() {
        use std::io::{Read, Seek};
        let mut stderr = stderr;
        stderr.rewind()?;
        let mut bytes = Vec::new();
        stderr.take(16384).read_to_end(&mut bytes)?;
        // Native stderr can contain credentials; retain it only in the member's private runtime directory.
        let store = shared
            .lock()
            .map_err(|_| error("INTERNAL", "store poisoned"))?;
        private_write(
            &store
                .directory
                .join("members")
                .join(member)
                .join(generation.to_string())
                .join("stderr.log"),
            &bytes,
        )?;
    }
    result
}

fn codex_thread_params(command: &Command, member: &Value) -> Result<Value> {
    let configuration = command
        .get_envs()
        .find(|(key, _)| *key == "CODEX_HOME")
        .and_then(|(_, value)| value)
        .ok_or_else(|| error("CONFIG", "missing private Codex configuration"))?;
    let snapshot: toml::Value = toml::from_str(&std::fs::read_to_string(
        Path::new(configuration).join("config.toml"),
    )?)
    .map_err(|e| error("CONFIG", e.to_string()))?;
    let mut params = json!({
        "cwd": member["cwd"],
        "model": member["model"],
        "approvalPolicy": "on-request",
        "sandbox": "workspace-write",
        "developerInstructions": "You are a SessionBridge group member. Read the sessionbridge-collaboration skill and use the sessionbridge MCP tools. Do not spawn hidden subagents. Follow the current JOIN challenge before other work."
    });
    let mut pinned = serde_json::Map::new();
    for key in ["model_provider", "model_providers", "mcp_servers"] {
        if let Some(value) = snapshot.get(key) {
            pinned.insert(key.into(), serde_json::to_value(value)?);
        }
    }
    params["config"] = Value::Object(pinned);
    Ok(params)
}

fn append(
    hub: &Hub,
    shared: &SharedStore,
    member: &str,
    room: &str,
    current: &mut Option<String>,
    delta: &str,
) -> Result<()> {
    if delta.is_empty() {
        return Ok(());
    }
    let store = shared
        .lock()
        .map_err(|_| error("INTERNAL", "store poisoned"))?;
    if let Some(mid) = current {
        store.db.execute(
            "UPDATE messages SET body=body||? WHERE id=?",
            params![delta, mid.as_str()],
        )?;
    } else {
        *current = Some(
            text(
                &store.insert_message(room, member, "output", delta, &[], None, 0)?,
                "id",
            )?
            .into(),
        );
    }
    hub.notify();
    Ok(())
}
fn tool_event(
    hub: &Hub,
    shared: &SharedStore,
    member: &str,
    room: &str,
    event: &Value,
) -> Result<()> {
    let store = shared
        .lock()
        .map_err(|_| error("INTERNAL", "store poisoned"))?;
    store.insert_message(
        room,
        member,
        "tool",
        &serde_json::to_string(event)?,
        &[],
        None,
        0,
    )?;
    hub.notify();
    Ok(())
}
fn approval(
    hub: &Hub,
    shared: &SharedStore,
    member: &str,
    room: &str,
    generation: i64,
    details: &Value,
) -> Result<String> {
    let store = shared
        .lock()
        .map_err(|_| error("INTERNAL", "store poisoned"))?;
    let aid = id();
    store.transaction(|| {
        store.db.execute(
            "INSERT INTO approvals(id,member_id,generation,details) VALUES(?,?,?,?)",
            params![aid, member, generation, serde_json::to_string(details)?],
        )?;
        store.insert_message(room, member, "approval", "需要你的授权", &[], None, 0)?;
        Ok(())
    })?;
    hub.notify();
    Ok(aid)
}
