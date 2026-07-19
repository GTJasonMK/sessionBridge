use std::{process::ExitCode, time::Instant};

use clap::Parser;
use sessionbridge::cli::{Cli, run};
use uuid::Uuid;

fn main() -> ExitCode {
    let invocation_id = Uuid::now_v7();
    sessionbridge::logging::set_invocation_id(invocation_id.to_string());
    let logging = match sessionbridge::logging::init() {
        Ok(logging) => logging,
        Err(error) => {
            sessionbridge::output::write_error(&error);
            return ExitCode::from(error.exit_code());
        }
    };
    let cli = Cli::parse();
    let command = cli.command.log_name();
    let task_reference = cli.command.task_reference().unwrap_or("").to_owned();
    let workspace_path = cli
        .workspace
        .as_deref()
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    let context = cli.context.as_deref().and_then(|path| {
        sessionbridge::workspace::read_json_private::<sessionbridge::domain::SessionContext>(path)
            .ok()
    });
    let workspace_id = context
        .as_ref()
        .map(|context| context.workspace_id.to_string())
        .unwrap_or_default();
    let task_id = context
        .as_ref()
        .map(|context| context.task_id.to_string())
        .unwrap_or_default();
    let session_id = context
        .as_ref()
        .map(|context| context.session_id.to_string())
        .unwrap_or_default();
    let started = Instant::now();
    let span = tracing::info_span!(
        "cli_invocation",
        invocation_id = %invocation_id,
        command,
        request_id = cli.request_id.as_deref().unwrap_or(""),
        workspace_path = %workspace_path,
        workspace_id = %workspace_id,
        task_id = %task_id,
        task_reference = %task_reference,
        session_id = %session_id,
        pid = std::process::id(),
        version = env!("CARGO_PKG_VERSION"),
    );
    let _entered = span.enter();
    tracing::info!(
        event = "command_started",
        log_dir = %logging.directory.display(),
        "SessionBridge command started"
    );

    match run(cli) {
        Ok(()) => {
            tracing::info!(
                event = "command_completed",
                elapsed_ms = started.elapsed().as_millis(),
                "SessionBridge command completed"
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!(
                event = "command_failed",
                error_code = ?error.code(),
                retryable = error.retryable_flag(),
                elapsed_ms = started.elapsed().as_millis(),
                error = %error,
                "SessionBridge command failed"
            );
            sessionbridge::output::write_error(&error);
            ExitCode::from(error.exit_code())
        }
    }
}
