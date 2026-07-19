mod commands;
mod error;
mod preferences;

use std::sync::Mutex;

use preferences::GuiPreferences;

pub struct AppState {
    preferences: Mutex<GuiPreferences>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let invocation_id = uuid::Uuid::now_v7().to_string();
    sessionbridge::logging::set_invocation_id(invocation_id);
    let logging =
        sessionbridge::logging::init().expect("failed to initialize SessionBridge logging");
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(AppState {
            preferences: Mutex::new(
                GuiPreferences::load().expect("failed to load SessionBridge GUI preferences"),
            ),
        })
        .invoke_handler(tauri::generate_handler![
            commands::workspace_restore,
            commands::workspace_open,
            commands::workspace_initialize,
            commands::task_list,
            commands::task_snapshot,
            commands::chat_send,
            commands::launch_preflight,
            commands::launch_execute,
            commands::prompt_list,
            commands::prompt_show,
            commands::task_transition,
            commands::doctor_run,
            commands::collaboration_diagnostics,
            commands::logs_tail,
        ])
        .run(tauri::generate_context!())
        .expect("error while running SessionBridge desktop application");
    drop(logging);
}
