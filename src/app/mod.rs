pub mod agent;
pub mod await_service;
pub mod chat;
pub mod collaboration_projection;
pub mod collaboration_service;
pub mod completion_service;
pub mod deliverable;
pub mod diagnostics;
pub mod events;
pub mod external_wait;
pub mod handlers;
pub mod idempotency;
pub mod invite;
pub mod join;
pub mod launch;
pub mod maintenance;
pub mod mcp;
pub mod operation_contract;
pub mod operator;
pub mod planning;
pub mod runtime;
pub mod runtime_reconciler;
pub mod settings;
pub mod speculative;
pub mod task;
pub mod task_reconciler;
pub mod work;
pub mod workflow;
pub mod workspace_service;

use crate::{Result, cli::Cli};

pub fn dispatch(cli: Cli) -> Result<()> {
    handlers::dispatch(cli)
}
