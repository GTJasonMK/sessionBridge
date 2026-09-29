use serde_json::{Value, json};
use sessionbridge::bridge::{self, store::Store};
use std::{path::Path, process::Command};

fn call(store: &Store, actor: &str, op: &str, mut args: Value) -> bridge::Result<Value> {
    args["request_id"] = json!(bridge::id());
    store.execute(actor, op, &args)
}
fn create(store: &Store) -> (String, String, String) {
    let room = call(store, "boss", "room.create", json!({"title":"协议验证"})).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let members = store
        .rows("SELECT id,role FROM members WHERE room_id=?", &[&room])
        .unwrap();
    let member = |role| {
        members.iter().find(|m| m["role"] == role).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    (room, member("coordinator"), member("executor"))
}
#[test]
fn durable_addressing_permissions_and_handshake() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("project");
    std::fs::create_dir(&repo).unwrap();
    let state = temp.path().join("state");
    let store = Store::open_in(&repo, &state).unwrap();
    let (room, coordinator, executor) = create(&store);
    let (other, other_coordinator, _) = create(&store);
    let request = json!({"room":room,"kind":"request","body":"实现功能","recipients":[coordinator],"request_id":bridge::id()});
    let message = store.execute("boss", "message.send", &request).unwrap();
    assert_eq!(
        store.execute("boss", "message.send", &request).unwrap(),
        message
    );
    let mut conflict = request.clone();
    conflict["body"] = json!("不同内容");
    assert_eq!(
        store
            .execute("boss", "message.send", &conflict)
            .unwrap_err()
            .code,
        "REQUEST_CONFLICT"
    );
    assert_eq!(
        store
            .rows(
                "SELECT * FROM deliveries WHERE message_id=?",
                &[&message["id"].as_str().unwrap()]
            )
            .unwrap()
            .len(),
        1
    );
    assert!(call(&store, &executor, "history", json!({"room":other})).is_err());
    assert!(
        call(
            &store,
            "boss",
            "message.send",
            json!({"room":room,"kind":"request","body":"不能跨群","recipients":[other_coordinator]})
        )
        .is_err()
    );
    let extra = call(
        &store,
        "boss",
        "member.add",
        json!({"room":room,"name":"执行者 2"}),
    )
    .unwrap();
    assert!(
        store
            .rows(
                "SELECT * FROM deliveries WHERE member_id=? AND message_id=?",
                &[
                    &extra["id"].as_str().unwrap(),
                    &message["id"].as_str().unwrap()
                ]
            )
            .unwrap()
            .is_empty()
    );
    assert!(
        call(
            &store,
            &executor,
            "member.add",
            json!({"room":room,"name":"隐藏成员"})
        )
        .is_err()
    );
    let broadcast = call(
        &store,
        "boss",
        "message.send",
        json!({"room":room,"kind":"notice","body":"全体通知","broadcast":true}),
    )
    .unwrap();
    assert_eq!(
        store
            .rows(
                "SELECT * FROM deliveries WHERE message_id=?",
                &[&broadcast["id"].as_str().unwrap()]
            )
            .unwrap()
            .len(),
        3
    );
    assert!(
        call(
            &store,
            &coordinator,
            "message.ack",
            json!({"messages":[message["id"]]})
        )
        .is_err()
    );
    store
        .db
        .execute(
            "UPDATE deliveries SET state='delivered' WHERE message_id=?",
            [message["id"].as_str().unwrap()],
        )
        .unwrap();
    call(
        &store,
        &coordinator,
        "message.ack",
        json!({"messages":[message["id"]]}),
    )
    .unwrap();
    assert!(
        call(
            &store,
            &executor,
            "message.ack",
            json!({"messages":[message["id"]]})
        )
        .is_err()
    );
    store.db.execute("UPDATE members SET state='starting',generation=2,token_hash=?,challenge='challenge' WHERE id=?",[bridge::hash("credential"),executor.clone()]).unwrap();
    assert_eq!(store.authenticate("credential").unwrap(), executor);
    let ready = json!({"protocol":"2","generation":2,"challenge":"challenge"});
    assert!(call(&store, &executor, "ready", ready.clone()).is_err());
    call(&store, &executor, "skill.read", json!({})).unwrap();
    call(&store, &executor, "inspect", json!({})).unwrap();
    assert!(
        call(
            &store,
            &executor,
            "ready",
            json!({"protocol":"1","generation":2,"challenge":"challenge"})
        )
        .is_err()
    );
    assert_eq!(store.member(&executor).unwrap()["state"], "starting");
    assert!(
        call(
            &store,
            &executor,
            "ready",
            json!({"protocol":"2","generation":1,"challenge":"challenge"})
        )
        .is_err()
    );
    call(&store, &executor, "ready", ready).unwrap();
    assert_eq!(store.member(&executor).unwrap()["state"], "ready");
    assert!(call(&store, "boss", "member.remove", json!({"member":executor})).is_err());
    store
        .db
        .execute(
            "UPDATE members SET state='offline',token_hash=NULL WHERE id=?",
            [&executor],
        )
        .unwrap();
    call(&store, "boss", "member.remove", json!({"member":executor})).unwrap();
    assert!(store.authenticate("credential").is_err());
    assert!(
        call(
            &store,
            "boss",
            "message.send",
            json!({"room":room,"kind":"request","body":"迟到","recipients":[executor]})
        )
        .is_err()
    );
    drop(store);
    let recovered = Store::open_in(&repo, &state).unwrap();
    let history = call(&recovered, "boss", "history", json!({"room":room})).unwrap();
    assert!(
        history["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"] == message["id"])
    );
    assert!(!repo.join(".sessionbridge").exists());
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}
#[test]
fn isolated_work_requires_commit_evidence_and_current_checks() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project");
    std::fs::create_dir(&root).unwrap();
    git(&root, &["init", "-b", "main"]);
    git(&root, &["config", "user.email", "test@example.invalid"]);
    git(&root, &["config", "user.name", "Test"]);
    std::fs::write(root.join("README.md"), "baseline\n").unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-m", "baseline"]);
    let store = Store::open_in(&root, &temp.path().join("state")).unwrap();
    let (room, coordinator, executor) = create(&store);
    let integration = bridge::git::prepare(&store, &coordinator).unwrap();
    let worker = bridge::git::prepare(&store, &executor).unwrap();
    assert_ne!(worker, root);
    assert_ne!(worker, integration);
    let work = call(
        &store,
        &coordinator,
        "work.create",
        json!({"room":room,"title":"新增文件","owner":executor}),
    )
    .unwrap();
    let wid = work["id"].as_str().unwrap();
    call(
        &store,
        &executor,
        "work.update",
        json!({"room":room,"work":wid,"version":1,"state":"running"}),
    )
    .unwrap();
    assert!(
        call(
            &store,
            &executor,
            "work.update",
            json!({"room":room,"work":wid,"version":1,"state":"blocked","evidence":"stale"})
        )
        .is_err()
    );
    assert!(
        call(
            &store,
            &executor,
            "work.update",
            json!({"room":room,"work":wid,"version":2,"state":"review","evidence":"伪造"})
        )
        .is_err()
    );
    std::fs::write(worker.join("result.txt"), "verified\n").unwrap();
    git(&worker, &["add", "."]);
    git(&worker, &["commit", "-m", "result"]);
    let commit = git(&worker, &["rev-parse", "HEAD"]);
    call(&store,&executor,"work.update",json!({"room":room,"work":wid,"version":2,"state":"review","evidence":"result written","commit":commit})).unwrap();
    assert!(
        call(
            &store,
            &executor,
            "work.update",
            json!({"room":room,"work":wid,"version":3,"state":"done","evidence":"self-approved"})
        )
        .is_err()
    );
    assert!(
        call(
            &store,
            &coordinator,
            "work.update",
            json!({"room":room,"work":wid,"version":3,"state":"done","evidence":"not merged yet"})
        )
        .is_err()
    );
    git(&integration, &["merge", "--ff-only", &commit]);
    assert_eq!(call(&store,&coordinator,"work.update",json!({"room":room,"work":wid,"version":3,"state":"done","evidence":"reviewed","commit":"f".repeat(40)})).unwrap_err().code,"INVALID_INPUT");
    call(
        &store,
        &coordinator,
        "work.update",
        json!({"room":room,"work":wid,"version":3,"state":"done","evidence":"reviewed"}),
    )
    .unwrap();
    assert!(bridge::git::integrate(&store, &coordinator, &json!({"room":room})).is_err());
    let check = bridge::git::check(
        &store,
        &coordinator,
        &json!({"room":room,"argv":["test","-f","result.txt"]}),
        None,
    )
    .unwrap();
    assert_eq!(check["exit_code"], 0);
    assert_eq!(
        bridge::git::integrate(&store, &coordinator, &json!({"room":room}))
            .unwrap_err()
            .code,
        "MESSAGES_PENDING"
    );
    store
        .db
        .execute("UPDATE deliveries SET state='processed'", [])
        .unwrap();
    std::fs::write(root.join("dirty.txt"), "user work").unwrap();
    assert!(bridge::git::integrate(&store, &coordinator, &json!({"room":room})).is_err());
    git(&root, &["add", "dirty.txt"]);
    git(&root, &["commit", "-m", "user change"]);
    assert_eq!(
        bridge::git::integrate(&store, &coordinator, &json!({"room":room}))
            .unwrap_err()
            .code,
        "REVALIDATE"
    );
    assert_eq!(
        bridge::git::integrate(&store, &coordinator, &json!({"room":room}))
            .unwrap_err()
            .code,
        "CHECKS_REQUIRED"
    );
    bridge::git::check(
        &store,
        &coordinator,
        &json!({"room":room,"argv":["test","-f","result.txt"]}),
        None,
    )
    .unwrap();
    let result = bridge::git::integrate(&store, &coordinator, &json!({"room":room})).unwrap();
    assert_eq!(result["completed"], true);
    assert_eq!(
        std::fs::read_to_string(root.join("result.txt")).unwrap(),
        "verified\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("dirty.txt")).unwrap(),
        "user work"
    );
    assert_eq!(
        bridge::git::integrate(&store, &coordinator, &json!({"room":room})).unwrap(),
        result
    );
}

#[test]
fn change_cursor_captures_stream_updates_and_dependency_invalidation() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open_in(temp.path(), &temp.path().join("state")).unwrap();
    let (room, coordinator, executor) = create(&store);
    let first = call(
        &store,
        &coordinator,
        "work.create",
        json!({"room":room,"title":"接口","owner":executor}),
    )
    .unwrap();
    let second = call(
        &store,
        &coordinator,
        "work.create",
        json!({"room":room,"title":"调用方","owner":executor,"dependencies":[first["id"]]}),
    )
    .unwrap();
    store
        .db
        .execute("UPDATE work SET state='done',commit_id='stale'", [])
        .unwrap();
    let output = store
        .insert_message(&room, &coordinator, "output", "a", &[], None, 0)
        .unwrap();
    let before = call(&store, "boss", "history", json!({"room":room,"tail":true})).unwrap();
    store
        .db
        .execute(
            "UPDATE messages SET body=body||'b' WHERE id=?",
            [output["id"].as_str().unwrap()],
        )
        .unwrap();
    let delta = call(
        &store,
        "boss",
        "history",
        json!({"room":room,"change_after":before["change_cursor"]}),
    )
    .unwrap();
    assert_eq!(delta["messages"].as_array().unwrap().len(), 1);
    assert_eq!(delta["messages"][0]["body"], "ab");
    call(
        &store,
        &coordinator,
        "work.update",
        json!({"room":room,"work":first["id"],"version":1,"state":"todo"}),
    )
    .unwrap();
    let downstream = store
        .rows(
            "SELECT * FROM work WHERE id=?",
            &[&second["id"].as_str().unwrap()],
        )
        .unwrap()
        .remove(0);
    assert_eq!(downstream["state"], "todo");
    assert_eq!(downstream["version"], 2);
    assert!(downstream["commit_id"].is_null());
    assert!(
        call(
            &store,
            &executor,
            "work.update",
            json!({"room":room,"work":second["id"],"version":2,"state":"running"})
        )
        .is_err()
    );
}

#[test]
fn verification_cannot_write_outside_its_worktree() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project");
    std::fs::create_dir(&root).unwrap();
    git(&root, &["init", "-b", "main"]);
    git(&root, &["config", "user.email", "test@example.invalid"]);
    git(&root, &["config", "user.name", "Test"]);
    git(&root, &["commit", "--allow-empty", "-m", "baseline"]);
    let store = Store::open_in(&root, &temp.path().join("state")).unwrap();
    let (room, coordinator, _) = create(&store);
    bridge::git::prepare(&store, &coordinator).unwrap();
    let outside = root.join("must-not-exist");
    let result = bridge::git::check(
        &store,
        &coordinator,
        &json!({"room":room,"argv":["touch",outside]}),
        None,
    )
    .unwrap();
    assert_ne!(result["exit_code"], 0);
    assert!(!outside.exists());
}

#[test]
fn schema_cleanup_preserves_existing_history_and_rolls_back_failed_writes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project");
    std::fs::create_dir(&root).unwrap();
    let base = temp.path().join("state");
    let directory = base
        .join("workspaces")
        .join(bridge::hash(root.to_str().unwrap()));
    std::fs::create_dir_all(&directory).unwrap();
    let old = rusqlite::Connection::open(directory.join("state.db")).unwrap();
    old.execute_batch(include_str!("fixtures/workgroup_v1.sql"))
        .unwrap();
    old.execute(
        "INSERT INTO rooms(id,title,created) VALUES('room','history',0)",
        [],
    )
    .unwrap();
    old.execute(
        "INSERT INTO members(id,room_id,name,role) VALUES('member','room','executor','executor')",
        [],
    )
    .unwrap();
    old.execute("INSERT INTO messages(id,room_id,sender,kind,body,recipients,request_id,fingerprint,created) VALUES('message','room','system','notice','preserve me','[\"member\"]','request','hash',0)",[]).unwrap();
    old.execute(
        "INSERT INTO deliveries VALUES('message','member','delivered')",
        [],
    )
    .unwrap();
    drop(old);
    let store = Store::open_in(&root, &base).unwrap();
    assert_eq!(
        store
            .db
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        store
            .rows("SELECT body FROM messages WHERE id='message'", &[])
            .unwrap()[0]["body"],
        "preserve me"
    );
    assert!(store.db.prepare("SELECT work_id FROM messages").is_err());
    assert!(
        !store
            .db
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
    assert_eq!(
        store
            .db
            .query_row(
                "SELECT count(*) FROM message_changes WHERE message_id='message'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
    assert_eq!(
        store.rows("SELECT state FROM deliveries", &[]).unwrap()[0]["state"],
        "delivered"
    );
    let (room, coordinator, _) = create(&store);
    store.db.execute_batch("CREATE TRIGGER reject_delivery BEFORE INSERT ON deliveries BEGIN SELECT RAISE(ABORT,'disk test'); END;").unwrap();
    let args =
        json!({"room":room,"kind":"request","body":"must roll back","recipients":[coordinator]});
    assert!(call(&store, "boss", "message.send", args.clone()).is_err());
    assert!(store.db.is_autocommit());
    assert!(
        store
            .rows("SELECT id FROM messages WHERE body='must roll back'", &[])
            .unwrap()
            .is_empty()
    );
    store
        .db
        .execute_batch("DROP TRIGGER reject_delivery")
        .unwrap();
    call(&store, "boss", "message.send", args).unwrap();
    for bad in [
        json!({"room":room,"limit":0}),
        json!({"room":room,"after":"wrong"}),
        json!({"room":room,"before":1,"tail":true}),
    ] {
        assert_eq!(
            call(&store, "boss", "history", bad).unwrap_err().code,
            "INVALID_INPUT"
        );
    }
}

#[test]
fn stop_kills_children_even_when_the_group_leader_exits_first() {
    use std::process::Stdio;
    let temp = tempfile::tempdir().unwrap();
    let marker = temp.path().join("child");
    let mut cmd = Command::new("python3");
    cmd.args(["-c","import os,signal,time,sys; pid=os.fork(); signal.signal(signal.SIGTERM, signal.SIG_IGN) if pid==0 else None; open(sys.argv[1],'w').write(str(os.getpid())) if pid==0 else None; time.sleep(20)"]).arg(&marker).stdout(Stdio::null()).stderr(Stdio::null());
    bridge::process::configure(&mut cmd);
    let mut child = cmd.spawn().unwrap();
    for _ in 0..100 {
        if marker.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let pid = loop {
        match std::fs::read_to_string(&marker)
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok())
        {
            Some(pid) => break pid,
            None => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    };
    bridge::process::terminate(&mut child).unwrap();
    let mut running = true;
    for _ in 0..100 {
        running = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(s) => !s.rsplit_once(") ").unwrap().1.starts_with('Z'),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => panic!("{e}"),
        };
        if !running {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!running, "child survived cancellation");
}

#[test]
fn large_history_pages_fit_transport_and_keep_all_cursors() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open_in(temp.path(), &temp.path().join("state")).unwrap();
    let (room, _, _) = create(&store);
    store
        .transaction(|| {
            for _ in 0..100 {
                store.insert_message(
                    &room,
                    "system",
                    "notice",
                    &"x".repeat(32768),
                    &[],
                    None,
                    0,
                )?;
            }
            Ok(())
        })
        .unwrap();
    let mut cursor = 0;
    let mut count = 0;
    loop {
        let page = call(
            &store,
            "boss",
            "history",
            json!({"room":room,"after":cursor,"limit":200}),
        )
        .unwrap();
        let mut wire = Vec::new();
        bridge::ipc::write_frame(&mut wire, &json!({"ok":true,"data":page})).unwrap();
        assert!(wire.len() <= bridge::ipc::MAX_FRAME_BYTES);
        count += page["messages"].as_array().unwrap().len();
        cursor = page["cursor"].as_i64().unwrap();
        if page["has_more"] == false {
            break;
        }
    }
    assert_eq!(count, 101);
    let tail = call(
        &store,
        "boss",
        "history",
        json!({"room":room,"tail":true,"limit":200}),
    )
    .unwrap();
    assert_eq!(
        tail["messages"].as_array().unwrap().last().unwrap()["seq"],
        cursor
    );
    let mut change = tail["change_cursor"].as_i64().unwrap();
    store
        .db
        .execute(
            "UPDATE messages SET body=body||'updated' WHERE room_id=?",
            [&room],
        )
        .unwrap();
    let mut changed = std::collections::HashSet::new();
    loop {
        let page = call(
            &store,
            "boss",
            "history",
            json!({"room":room,"change_after":change,"limit":200}),
        )
        .unwrap();
        for message in page["messages"].as_array().unwrap() {
            changed.insert(message["id"].as_str().unwrap().to_owned());
        }
        change = page["change_cursor"].as_i64().unwrap();
        if page["has_more"] == false {
            break;
        }
    }
    assert_eq!(changed.len(), 101);
}
