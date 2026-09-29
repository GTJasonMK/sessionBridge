use super::{store::Store, *};
use crate::git::GitRepository;
use rusqlite::params;
use serde_json::json;
use std::{
    fs::OpenOptions,
    io::Read,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub fn prepare(store: &Store, member: &str) -> Result<PathBuf> {
    let m = store.member(member)?;
    let room = text(&m, "room_id")?;
    let repo = GitRepository::open(&store.root)?;
    let group = store.room(room)?;
    let integration = store
        .directory
        .join("worktrees")
        .join(room)
        .join("integration");
    let branch = format!("sessionbridge/{room}/integration");
    if group["integration_path"].is_null() {
        let target = if let Some(target) = group["target"].as_str() {
            target.to_owned()
        } else {
            repo.require_initial_commit()?;
            repo.require_clean()?;
            let target = repo.current_branch()?.ok_or_else(|| {
                error(
                    "DETACHED_HEAD",
                    "select a target branch before initializing",
                )
            })?;
            store.db.execute(
                "UPDATE rooms SET target=? WHERE id=?",
                params![target, room],
            )?;
            target
        };
        if !integration.exists() {
            repo.provision_worktree(&integration, &branch, Some(&format!("refs/heads/{target}")))?;
        }
        validate_worktree(&repo, &integration)?;
        if GitRepository::open(&integration)?
            .current_branch()?
            .as_deref()
            != Some(&branch)
        {
            return Err(error("WORKTREE_MISMATCH", "unexpected integration branch"));
        }
        store.db.execute(
            "UPDATE rooms SET integration_path=? WHERE id=?",
            params![
                integration
                    .to_str()
                    .ok_or_else(|| error("INVALID_INPUT", "worktree path must be UTF-8"))?,
                room
            ],
        )?;
    } else {
        if Path::new(text(&group, "integration_path")?) != integration {
            return Err(error("WORKTREE_MISMATCH", "integration path changed"));
        }
        validate_worktree(&repo, &integration)?;
        if GitRepository::open(&integration)?
            .current_branch()?
            .as_deref()
            != Some(&branch)
        {
            return Err(error("WORKTREE_MISMATCH", "integration branch changed"));
        }
    }
    let cwd = if m["role"] == "coordinator" {
        integration.clone()
    } else {
        store.directory.join("worktrees").join(room).join(member)
    };
    if let Some(saved) = m["cwd"].as_str() {
        if Path::new(saved) != cwd {
            return Err(error("WORKTREE_MISMATCH", "member worktree path changed"));
        }
    } else if !cwd.exists() {
        repo.provision_worktree(
            &cwd,
            &format!("sessionbridge/{room}/{member}"),
            Some(&GitRepository::open(&integration)?.head_commit()?),
        )?;
    }
    validate_worktree(&repo, &cwd)?;
    store.db.execute(
        "UPDATE members SET cwd=? WHERE id=?",
        params![
            cwd.to_str()
                .ok_or_else(|| error("INVALID_INPUT", "worktree path must be UTF-8"))?,
            member
        ],
    )?;
    Ok(cwd)
}
fn validate_worktree(repo: &GitRepository, path: &Path) -> Result<()> {
    if GitRepository::open(path)?.common_dir()? != repo.common_dir()? {
        return Err(error(
            "WORKTREE_MISMATCH",
            "worktree belongs to a different repository",
        ));
    }
    Ok(())
}

/// Execute argv without shell interpolation, with bounded runtime and bounded returned output.
pub fn command(cwd: &Path, argv: &[String], seconds: u64) -> Result<(i32, String)> {
    command_with_cancel(cwd, argv, seconds, || Ok(false))
}
fn command_with_cancel(
    cwd: &Path,
    argv: &[String],
    seconds: u64,
    mut cancelled: impl FnMut() -> Result<bool>,
) -> Result<(i32, String)> {
    if argv.is_empty()
        || argv.len() > 128
        || argv.iter().any(|s| s.contains('\0') || s.len() > 32768)
    {
        return Err(error("INVALID_INPUT", "invalid argv"));
    }
    let output = tempfile::tempfile()?;
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]);
    process::configure(&mut command);
    let mut child = command
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output.try_clone()?)
        .spawn()?;
    let started = Instant::now();
    let status = loop {
        match cancelled() {
            Ok(false) => {}
            Ok(true) => {
                process::terminate(&mut child)?;
                return Err(error("STOPPED", "verification cancelled"));
            }
            Err(e) => {
                process::terminate(&mut child)?;
                return Err(e);
            }
        }
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() > Duration::from_secs(seconds) {
            process::terminate(&mut child)?;
            return Err(error("TIMEOUT", format!("command exceeded {seconds}s")));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    use std::io::Seek;
    let mut output = output;
    output.rewind()?;
    let truncated = output.metadata()?.len() > 262144;
    let mut bytes = Vec::new();
    output.take(262144).read_to_end(&mut bytes)?;
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if truncated {
        text.push_str("\n[输出超过 256 KiB，已截断展示]");
    }
    Ok((status.code().unwrap_or(-1), text))
}
pub fn check(store: &Store, actor: &str, args: &Value, token: Option<&str>) -> Result<Value> {
    let room = text(args, "room")?;
    let credentials = if actor == "boss" {
        None
    } else {
        Some(store.db.query_row(
            "SELECT generation,token_hash FROM members WHERE id=?",
            [actor],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?)),
        )?)
    };
    if let Some(token) = token
        && credentials.as_ref().and_then(|(_, t)| t.as_deref()) != Some(hash(token).as_str())
    {
        return Err(error("UNAUTHORIZED", "verification credential was revoked"));
    }
    if actor != "boss" {
        let m = store.member(actor)?;
        if m["room_id"] != room || m["role"] != "coordinator" {
            return Err(error("FORBIDDEN", "coordinator required"));
        }
    }
    if store.room(room)?["state"] != "active" {
        return Err(error("ROOM_INACTIVE", "room not active"));
    }
    let r = store.room(room)?;
    let cwd = Path::new(text(&r, "integration_path")?);
    let repo = GitRepository::open(cwd)?;
    repo.require_clean()?;
    let head = repo.head_commit()?;
    let work_revision: i64 = store.db.query_row(
        "SELECT COALESCE(SUM(version),0) FROM work WHERE room_id=?",
        [room],
        |r| r.get(0),
    )?;
    let argv: Vec<String> = serde_json::from_value(args["argv"].clone())?;
    if argv.is_empty() {
        return Err(error("INVALID_INPUT", "verification argv is empty"));
    }
    // The MCP verifier must not provide a shell escape around the native CLI sandbox.
    let mut sandbox: Vec<String> = [
        "bwrap",
        "--unshare-all",
        "--die-with-parent",
        "--new-session",
        "--ro-bind",
        "/",
        "/",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
        "--bind",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    sandbox.extend([
        cwd.to_string_lossy().into_owned(),
        cwd.to_string_lossy().into_owned(),
        "--chdir".into(),
        cwd.to_string_lossy().into_owned(),
        "--".into(),
    ]);
    sandbox.extend(argv.clone());
    let (code, output) = command_with_cancel(cwd, &sandbox, 60, || {
        if store.room(room)?["state"] != "active" {
            return Ok(true);
        }
        if let Some(ref initial) = credentials {
            let current = store.db.query_row(
                "SELECT generation,token_hash FROM members WHERE id=?",
                [actor],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?)),
            )?;
            return Ok(initial != &current);
        }
        Ok(false)
    })?;
    if head != repo.head_commit()? {
        return Err(error(
            "CHECK_CHANGED_HEAD",
            "check modified the checked commit",
        ));
    }
    let check = id();
    store.transaction(|| {
        store.db.execute(
            "INSERT INTO checks VALUES(?,?,?,?,?,?,?)",
            params![
                check,
                room,
                head,
                serde_json::to_string(&argv)?,
                code,
                output,
                work_revision
            ],
        )?;
        store.system(
            room,
            &format!("验证 {}\n退出状态：{code}\n{output}", argv.join(" ")),
        )?;
        Ok(json!({"id":check,"head":head,"exit_code":code,"output":output}))
    })
}
pub fn integrate(store: &Store, actor: &str, args: &Value) -> Result<Value> {
    let room = text(args, "room")?;
    if actor != "boss" {
        let m = store.member(actor)?;
        if m["room_id"] != room || m["role"] != "coordinator" {
            return Err(error("FORBIDDEN", "coordinator required"));
        }
    }
    let r = store.room(room)?;
    if r["state"] == "completed" {
        return Ok(json!({"head":r["integrated_head"],"completed":true}));
    }
    if r["state"] != "active" {
        return Err(error("ROOM_INACTIVE", "room not active"));
    }
    let repo = GitRepository::open(&store.root)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(repo.common_dir()?.join("sessionbridge-integration.lock"))?;
    lock.lock()?;
    let target = text(&r, "target")?;
    if repo.current_branch()?.as_deref() != Some(target) {
        return Err(error(
            "TARGET_CHANGED",
            "project checkout is no longer on the target branch",
        ));
    }
    repo.require_clean()?;
    let cwd = Path::new(text(&r, "integration_path")?);
    let integration = GitRepository::open(cwd)?;
    integration.require_clean()?;
    let count: i64 = store.db.query_row(
        "SELECT count(*) FROM work WHERE room_id=? AND state='done'",
        [room],
        |r| r.get(0),
    )?;
    let open: i64 = store.db.query_row(
        "SELECT count(*) FROM work WHERE room_id=? AND state NOT IN ('done','cancelled')",
        [room],
        |r| r.get(0),
    )?;
    if count == 0 || open > 0 {
        return Err(error("WORK_PENDING", "all required work must be accepted"));
    }
    for work in store.rows(
        "SELECT commit_id FROM work WHERE room_id=? AND state='done'",
        &[&room],
    )? {
        integration.require_ancestor(text(&work, "commit_id")?, "HEAD")?;
    }
    let pending:i64=store.db.query_row("SELECT count(*) FROM deliveries d JOIN messages m ON m.id=d.message_id WHERE m.room_id=? AND m.kind='request' AND d.state IN ('queued','delivered')",[room],|r|r.get(0))?;
    if pending > 0 {
        return Err(error(
            "MESSAGES_PENDING",
            "process addressed requests before completing the room",
        ));
    }
    let head = integration.head_commit()?;
    let target_head = repo.head_commit()?;
    if !integration.is_ancestor(&target_head, &head)? {
        let (code, output) = command(
            cwd,
            &[
                "git".into(),
                "merge".into(),
                "--no-edit".into(),
                target_head,
            ],
            60,
        )?;
        store.system(
            room,
            &format!("目标分支有更新，重新整合（退出状态 {code}）\n{output}"),
        )?;
        return Err(error(
            "REVALIDATE",
            "target changed; resolve any integration conflicts and rerun checks",
        ));
    }
    let passed: i64 = store.db.query_row(
        "SELECT count(*) FROM checks WHERE room_id=?1 AND head=?2 AND exit_code=0 AND work_revision=(SELECT COALESCE(SUM(version),0) FROM work WHERE room_id=?1)",
        params![room, head],
        |r| r.get(0),
    )?;
    let failed:i64=store.db.query_row("SELECT count(*) FROM checks c WHERE room_id=?1 AND head=?2 AND work_revision=(SELECT COALESCE(SUM(version),0) FROM work WHERE room_id=?1) AND exit_code!=0 AND NOT EXISTS (SELECT 1 FROM checks newer WHERE newer.room_id=c.room_id AND newer.head=c.head AND newer.argv=c.argv AND newer.rowid>c.rowid)",params![room,head],|r|r.get(0))?;
    if passed == 0 || failed > 0 {
        return Err(error(
            "CHECKS_REQUIRED",
            "current integration HEAD needs passing checks with no unresolved failures",
        ));
    }
    let (code, output) = command(
        &store.root,
        &[
            "git".into(),
            "merge".into(),
            "--ff-only".into(),
            head.clone(),
        ],
        60,
    )?;
    if code != 0 {
        return Err(error("MERGE_FAILED", output));
    }
    if repo.current_branch()?.as_deref() != Some(target) || repo.head_commit()? != head {
        return Err(error(
            "TARGET_CHANGED",
            "target changed during integration; inspect before retrying",
        ));
    }
    repo.require_clean()?;
    store.transaction(|| {
        store.db.execute(
            "UPDATE rooms SET state='completed',integrated_head=? WHERE id=?",
            params![head, room],
        )?;
        store.system(room, &format!("已验证并合入 {target}\n{head}"))?;
        Ok(json!({"head":head,"completed":true}))
    })
}
