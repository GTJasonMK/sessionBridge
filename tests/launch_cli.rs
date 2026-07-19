use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};

use serde_json::Value;
use tempfile::TempDir;

struct Harness {
    _root: TempDir,
    repo: PathBuf,
    state: PathBuf,
    database: PathBuf,
}

impl Harness {
    fn new() -> Self {
        let root = test_temp_dir();
        let repo = root.path().join("repo");
        let state = root.path().join("state");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&state).unwrap();
        git(&repo, &["init", "--initial-branch=main"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "user.name", "SessionBridge Test"]);
        fs::write(repo.join("README.md"), "baseline\n").unwrap();
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-m", "initial"]);

        let init = command_for(&state, &repo)
            .args(["init", "--name", "launch-test", "--format", "json"])
            .output()
            .unwrap();
        let init = success_json(&init);
        let database = PathBuf::from(init["data"]["database_path"].as_str().unwrap());
        git(&repo, &["add", ".sessionbridge/config.toml"]);
        git(&repo, &["commit", "-m", "workspace"]);

        Self {
            _root: root,
            repo,
            state,
            database,
        }
    }

    fn command(&self) -> Command {
        command_for(&self.state, &self.repo)
    }

    fn counts(&self) -> (i64, i64) {
        let connection = rusqlite::Connection::open(&self.database).unwrap();
        let tasks = connection
            .query_row("SELECT count(*) FROM tasks", [], |row| row.get(0))
            .unwrap();
        let invitations = connection
            .query_row("SELECT count(*) FROM agent_invitations", [], |row| {
                row.get(0)
            })
            .unwrap();
        (tasks, invitations)
    }
}

#[test]
fn launch_dry_run_publish_replay_and_prompt_work_end_to_end() {
    let harness = Harness::new();
    let dry = harness
        .command()
        .args([
            "launch",
            "Implement the demo feature",
            "--dry-run",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    let dry = success_json(&dry);
    assert_eq!(dry["data"]["agents"], 4);
    assert_eq!(dry["data"]["expires_in_seconds"], 7200);
    assert_eq!(dry["data"]["target_branch"], "main");
    assert_eq!(harness.counts(), (0, 0));

    let request_id = "019f4d40-1111-7111-8111-111111111111";
    let first = harness
        .command()
        .args([
            "launch",
            "Implement the demo feature",
            "--request-id",
            request_id,
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    let first = success_json(&first);
    assert_eq!(first["data"]["launch_id"], request_id);
    assert_eq!(
        first["data"]["invitations"]["prompts"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert!(
        first["data"]["invitations"]["prompts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|prompt| prompt.get("prompt").is_none())
    );
    assert_eq!(harness.counts(), (1, 4));

    let replay = harness
        .command()
        .args([
            "launch",
            "Implement the demo feature",
            "--request-id",
            request_id,
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    let replay = success_json(&replay);
    assert_eq!(
        replay["data"]["task"]["task"]["id"],
        first["data"]["task"]["task"]["id"]
    );
    assert_eq!(harness.counts(), (1, 4));

    let prompt = harness
        .command()
        .args([
            "prompt", "show", "current", "--index", "1", "--format", "text",
        ])
        .output()
        .unwrap();
    assert!(prompt.status.success());
    let prompt = String::from_utf8(prompt.stdout).unwrap();
    assert!(prompt.contains("SessionBridge 自主协同 Agent"));
    assert!(!prompt.contains("sb_invite_"));

    let status = harness
        .command()
        .args(["status", "current", "--format", "json"])
        .output()
        .unwrap();
    let status = success_json(&status);
    assert_eq!(
        status["data"]["health"]["signals"]["pending_blocking_invitations"],
        4
    );

    let watch = harness
        .command()
        .args(["watch", "current", "--once", "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(success_json(&watch)["data"]["task"]["phase"], "active");
}

#[test]
fn launch_recovers_after_task_publish_without_creating_duplicates() {
    let harness = Harness::new();
    let blocking_file = harness._root.path().join("prompt-parent");
    fs::write(&blocking_file, "not a directory").unwrap();
    let prompt_root = blocking_file.join("prompts");
    let failed = harness
        .command()
        .args([
            "launch",
            "Recoverable launch",
            "--prompt-dir",
            prompt_root.to_str().unwrap(),
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(!failed.status.success());
    let error: Value = serde_json::from_slice(&failed.stderr).unwrap();
    assert_eq!(error["error"]["code"], "LAUNCH_RECOVERY_REQUIRED");
    let launch_id = error["error"]["details"]["launch_id"].as_str().unwrap();
    assert_eq!(harness.counts(), (1, 0));

    fs::remove_file(&blocking_file).unwrap();
    fs::create_dir(&blocking_file).unwrap();
    fs::write(harness.repo.join("BASELINE.md"), "new baseline\n").unwrap();
    git(&harness.repo, &["add", "BASELINE.md"]);
    git(&harness.repo, &["commit", "-m", "advance baseline"]);

    let changed = harness
        .command()
        .args(["launch", "resume", launch_id, "--format", "json"])
        .output()
        .unwrap();
    assert!(!changed.status.success());
    let changed: Value = serde_json::from_slice(&changed.stderr).unwrap();
    assert_eq!(changed["error"]["code"], "LAUNCH_BASELINE_CHANGED");

    let recovered = harness
        .command()
        .args([
            "launch",
            "resume",
            launch_id,
            "--accept-baseline-change",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    let recovered = success_json(&recovered);
    assert_eq!(recovered["data"]["launch_id"], launch_id);
    assert_eq!(harness.counts(), (1, 4));

    let replay = harness
        .command()
        .args(["launch", "resume", launch_id, "--format", "json"])
        .output()
        .unwrap();
    success_json(&replay);
    assert_eq!(harness.counts(), (1, 4));
}

#[test]
fn dirty_and_noninteractive_preflight_fail_without_side_effects() {
    let harness = Harness::new();
    let missing = harness
        .command()
        .args(["launch", "--format", "json", "--non-interactive"])
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert_eq!(harness.counts(), (0, 0));

    fs::write(harness.repo.join("UNCOMMITTED.md"), "not in HEAD\n").unwrap();
    let dirty = harness
        .command()
        .args(["launch", "Dirty launch", "--dry-run", "--format", "json"])
        .output()
        .unwrap();
    assert!(!dirty.status.success());
    let dirty: Value = serde_json::from_slice(&dirty.stderr).unwrap();
    assert_eq!(dirty["error"]["code"], "WORKTREE_DIRTY");
    assert_eq!(harness.counts(), (0, 0));

    let allowed = harness
        .command()
        .args([
            "launch",
            "Dirty launch",
            "--dry-run",
            "--allow-dirty-baseline",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    let allowed = success_json(&allowed);
    assert_eq!(allowed["data"]["dirty_paths"][0], "UNCOMMITTED.md");
    assert!(!allowed["data"]["warnings"].as_array().unwrap().is_empty());
    assert_eq!(harness.counts(), (0, 0));
}

#[test]
fn concurrent_same_launch_id_converges_to_one_task_and_batch() {
    let harness = Harness::new();
    let repo = harness.repo.clone();
    let state = harness.state.clone();
    let request_id = "019f4d40-2222-7222-8222-222222222222";
    let first = std::thread::spawn({
        let repo = repo.clone();
        let state = state.clone();
        move || {
            command_for(&state, &repo)
                .args([
                    "launch",
                    "Concurrent launch",
                    "--request-id",
                    request_id,
                    "--format",
                    "json",
                ])
                .output()
                .unwrap()
        }
    });
    let second = std::thread::spawn(move || {
        command_for(&state, &repo)
            .args([
                "launch",
                "Concurrent launch",
                "--request-id",
                request_id,
                "--format",
                "json",
            ])
            .output()
            .unwrap()
    });
    let first = success_json(&first.join().unwrap());
    let second = success_json(&second.join().unwrap());
    assert_eq!(
        first["data"]["task"]["task"]["id"],
        second["data"]["task"]["task"]["id"]
    );
    assert_eq!(harness.counts(), (1, 4));
}

#[test]
fn current_is_ambiguous_and_project_profiles_override_launch_defaults() {
    let harness = Harness::new();
    let initialized = harness
        .command()
        .args(["config", "init", "--format", "json"])
        .output()
        .unwrap();
    success_json(&initialized);
    let settings = harness.repo.join(".sessionbridge/settings.toml");
    fs::write(
        &settings,
        r#"schema_version = 1
default_profile = "normal"

[profiles.normal]
agents = 4
expires_in = "2h"
autonomy = "bounded"
shell = "posix"

[profiles.large]
agents = 6
expires_in = "4h"
autonomy = "bounded"
shell = "posix"
target_branch = "main"
"#,
    )
    .unwrap();
    git(&harness.repo, &["add", ".sessionbridge/settings.toml"]);
    git(&harness.repo, &["commit", "-m", "settings"]);

    let large = harness
        .command()
        .args([
            "launch",
            "Large launch",
            "--profile",
            "large",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    let large = success_json(&large);
    assert_eq!(
        large["data"]["invitations"]["prompts"]
            .as_array()
            .unwrap()
            .len(),
        6
    );

    let second = harness
        .command()
        .args(["launch", "Second task", "--agents", "1", "--format", "json"])
        .output()
        .unwrap();
    success_json(&second);
    let ambiguous = harness
        .command()
        .args(["status", "current", "--format", "json"])
        .output()
        .unwrap();
    assert!(!ambiguous.status.success());
    let ambiguous: Value = serde_json::from_slice(&ambiguous.stderr).unwrap();
    assert_eq!(ambiguous["error"]["code"], "CURRENT_TASK_AMBIGUOUS");
    assert_eq!(
        ambiguous["error"]["details"]["tasks"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

fn command_for(state: &std::path::Path, current_dir: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_sessionbridge"));
    command
        .current_dir(current_dir)
        .env("SESSIONBRIDGE_STATE_HOME", state)
        .env("SESSIONBRIDGE_LOG", "sessionbridge=info");
    command
}

fn success_json(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["ok"], true);
    value
}

fn git(path: &std::path::Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn test_temp_dir() -> TempDir {
    TempDir::new().unwrap()
}
