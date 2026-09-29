use serde_json::Value;
use sessionbridge::bridge::{self, Request};
use std::path::PathBuf;

fn binary() -> bridge::Result<PathBuf> {
    if let Some(path) = std::env::var_os("SESSIONBRIDGE_CLI") {
        return Ok(PathBuf::from(path));
    }
    let current = std::env::current_exe()?;
    let sibling = current.with_file_name("sessionbridge");
    if sibling.is_file() {
        return Ok(sibling);
    }
    for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let path = directory.join("sessionbridge");
        if path.is_file() {
            return Ok(path);
        }
    }
    Err(bridge::error(
        "CLI_NOT_FOUND",
        "请安装 SessionBridge CLI，或设置 SESSIONBRIDGE_CLI",
    ))
}

#[tauri::command]
pub async fn bridge_call(
    workspace: String,
    operation: String,
    args: Value,
) -> bridge::Result<Value> {
    tauri::async_runtime::spawn_blocking(move || {
        if !["watch", "room.get", "room.list", "history"].contains(&operation.as_str()) {
            bridge::ipc::start(&binary()?)?;
        }
        bridge::ipc::call(&Request {
            workspace,
            operation,
            args,
            token: bridge::ipc::operator_token()?,
        })
    })
    .await
    .map_err(|e| bridge::error("IPC", e.to_string()))?
}
