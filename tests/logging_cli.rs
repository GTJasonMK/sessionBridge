use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use serde_json::Value;
use tempfile::TempDir;

#[test]
fn commands_write_correlated_json_logs_without_polluting_machine_output_or_secrets() {
    let state = test_temp_dir();
    let project = test_temp_dir();
    let init = command(&state, project.path())
        .args([
            "init",
            "--path",
            project.path().to_str().unwrap(),
            "--name",
            "logging-test",
            "--format",
            "json",
            "--non-interactive",
            "--request-id",
            "logging-init-request",
        ])
        .output()
        .unwrap();
    let init_response = assert_success_json(&init);
    assert!(init.stderr.is_empty());
    let init_invocation_id = init_response["meta"]["invocation_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let failure = command(&state, project.path())
        .args([
            "task",
            "show",
            "not-a-task-id",
            "--format",
            "json",
            "--non-interactive",
        ])
        .output()
        .unwrap();
    assert!(!failure.status.success());
    let error: Value = serde_json::from_slice(&failure.stderr).unwrap();
    assert_eq!(error["ok"], false);
    assert!(error["meta"]["invocation_id"].is_string());

    let secret_marker = "SESSIONBRIDGE_TEST_SECRET_MUST_NOT_BE_LOGGED";
    let secret_failure = command(&state, project.path())
        .args([
            "agent",
            "join",
            "--invite-stdin",
            "--format",
            "json",
            "--non-interactive",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(format!("{{{secret_marker}").as_bytes())?;
            child.wait_with_output()
        })
        .unwrap();
    assert!(!secret_failure.status.success());
    let _: Value = serde_json::from_slice(&secret_failure.stderr).unwrap();

    let tail = command(&state, project.path())
        .args([
            "logs",
            "tail",
            "--lines",
            "100",
            "--format",
            "json",
            "--non-interactive",
        ])
        .output()
        .unwrap();
    assert!(tail.stderr.is_empty());
    let tail = assert_success_json(&tail);
    assert!(!tail["data"]["entries"].as_array().unwrap().is_empty());

    let log_dir = state.path().join("logs");
    assert_private_directory(&log_dir);
    let contents = read_all_logs(&log_dir);
    assert!(contents.contains("command_started"));
    assert!(contents.contains("command_completed"));
    assert!(contents.contains("command_failed"));
    assert!(contents.contains("logging-init-request"));
    assert!(contents.contains("invocation_id"));
    assert!(contents.contains(&init_invocation_id));
    assert!(contents.contains("task show"));
    assert!(contents.contains("not-a-task-id"));
    assert!(contents.contains("workspace_path"));
    assert!(contents.contains("session_id"));
    assert!(!contents.contains(secret_marker));
    for line in contents.lines().filter(|line| !line.trim().is_empty()) {
        let _: Value = serde_json::from_str(line).unwrap();
    }
}

fn command(state: &TempDir, current_dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_sessionbridge"));
    command
        .current_dir(current_dir)
        .env("SESSIONBRIDGE_STATE_HOME", state.path())
        .env("SESSIONBRIDGE_LOG", "sessionbridge=debug");
    command
}

fn assert_success_json(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["ok"], true);
    assert!(value["meta"]["invocation_id"].is_string());
    value
}

fn read_all_logs(directory: &Path) -> String {
    let mut paths = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("jsonl"))
        .collect::<Vec<PathBuf>>();
    paths.sort();
    assert!(!paths.is_empty());
    paths
        .into_iter()
        .map(|path| fs::read_to_string(path).unwrap())
        .collect()
}

#[cfg(unix)]
fn assert_private_directory(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

#[cfg(not(unix))]
fn assert_private_directory(path: &Path) {
    assert!(path.is_dir());
}

fn test_temp_dir() -> TempDir {
    TempDir::new().unwrap()
}
