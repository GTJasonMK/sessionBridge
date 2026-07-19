use crate::{
    Result,
    app::{
        completion_service::{CompletionService, CompletionTrigger},
        task_reconciler::TaskReconciler,
    },
    clock::Clock,
    domain::SessionContext,
    storage::SqliteStore,
};

pub fn reconcile_agent_runtime<C: Clock + Clone>(
    store: &mut SqliteStore,
    context: &SessionContext,
    clock: C,
    trigger: CompletionTrigger<'_>,
) -> Result<()> {
    TaskReconciler::new(clock.clone()).reconcile_agent(store, context)?;
    CompletionService::new(clock).reconcile(store, context, trigger)?;
    Ok(())
}

pub fn reconcile_after_agent_write<C: Clock + Clone>(
    store: &mut SqliteStore,
    context: &SessionContext,
    clock: C,
) -> Result<()> {
    let actor_id = context.session_id.to_string();
    reconcile_agent_runtime(
        store,
        context,
        clock,
        CompletionTrigger {
            actor_type: "session",
            actor_id: Some(&actor_id),
        },
    )
}
