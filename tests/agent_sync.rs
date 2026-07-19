use rusqlite::params;
use sessionbridge::{
    ErrorCode,
    app::{
        agent::{AgentService, ConsolidationResolution, ProposeActionInput},
        invite::{CreateInvitationBatchRequest, InvitationService, PublicSecretDelivery},
        join::{JoinRequest, JoinService},
        task::{PublishTaskRequest, TaskService, WorkspaceBinding},
        workflow::{StartWorkflowRequest, WorkflowService},
        workspace_service::WorkspaceService,
    },
    clock::{FakeClock, unix_micros},
    domain::{AutonomyPolicy, CompletionPolicy, ParticipantKind, SessionContext},
    invitation::PromptShell,
    storage::SqliteStore,
    workspace::{LocatedWorkspace, read_json_private},
};
use tempfile::TempDir;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

#[test]
fn publish_invite_join_and_sync_share_one_authoritative_view() {
    let project = test_temp_dir();
    let state = test_temp_dir();
    let now = OffsetDateTime::UNIX_EPOCH;
    let clock = FakeClock::new(now);
    WorkspaceService::new(clock.clone())
        .initialize(project.path(), "agent-sync", Some(state.path()))
        .unwrap();
    let workspace =
        LocatedWorkspace::discover_with_state_root(project.path(), state.path()).unwrap();
    let mut store = SqliteStore::open_with_lock(
        &workspace.paths.database_path,
        &workspace.paths.migration_lock_path,
    )
    .unwrap();

    let published = TaskService::new(clock.clone())
        .publish(
            store.connection_mut(),
            WorkspaceBinding::from(&workspace),
            PublishTaskRequest {
                title: "Build autonomous coordination".into(),
                objective: "Agents decompose and complete the implementation together".into(),
                constraints: vec!["Keep credentials out of logs".into()],
                non_goals: Vec::new(),
                done_conditions: vec!["All integration tests pass".into()],
                autonomy_policy: AutonomyPolicy::Bounded,
                completion_policy: CompletionPolicy::AllWorkersConcluded,
                target_branch: None,
                request_id: Some(Uuid::now_v7().to_string()),
            },
        )
        .unwrap();

    let artifact_root = workspace.paths.state_root.join("invitation-batches");
    let manifest = {
        let mut invitations = InvitationService::new(
            &mut store,
            &clock,
            workspace.config.project_id,
            &artifact_root,
        )
        .unwrap();
        invitations
            .create_batch(CreateInvitationBatchRequest {
                task_id: published.task.id,
                count: 1,
                participant_kind: ParticipantKind::Worker,
                expires_in: Duration::hours(1),
                capability_limits: Vec::new(),
                request_id: Uuid::now_v7().to_string(),
                shell: PromptShell::Posix,
                provision_worktree: false,
            })
            .unwrap()
    };
    let invite_path = match &manifest.prompts[0].secret_delivery {
        PublicSecretDelivery::File { invite_path } => invite_path.clone(),
    };
    let joined = JoinService::new(clock.clone())
        .join(
            &mut store,
            &workspace,
            JoinRequest {
                invite_file: invite_path,
                provision_worktree: false,
                worktree: None,
                declared_capabilities: vec!["rust".into(), "testing".into()],
                label: Some("worker-one".into()),
                client_name: Some("integration-test".into()),
                model: Some("test-model".into()),
                request_id: None,
            },
        )
        .unwrap();
    let context: SessionContext = read_json_private(&joined.context_path).unwrap();

    let work_item_id = Uuid::now_v7();
    store
        .connection()
        .execute(
            "INSERT INTO work_items (
                id, task_id, kind, title, description, required, priority, risk,
                required_capabilities_json, acceptance_criteria_json,
                expected_artifacts_json, planned_resources_json, plan_state,
                result_state, created_by_session_id, version, created_at, updated_at
             ) VALUES (?1, ?2, 'implementation', 'Implement sync', 'Build the shared view',
                       1, 10, 'medium', '[\"rust\"]', '[\"tests pass\"]', '[]',
                       '[\"path:src/app\"]', 'active', 'open', ?3, 0, ?4, ?4)",
            params![
                work_item_id.to_string(),
                published.task.id.to_string(),
                joined.session_id.to_string(),
                unix_micros(now),
            ],
        )
        .unwrap();
    store
        .connection()
        .execute(
            "INSERT INTO gaps (
                id, task_id, kind, severity, required, status, title, description,
                reported_by_session_id, version, created_at, updated_at
             ) VALUES (?1, ?2, 'missing_test', 'high', 1, 'open',
                       'Missing concurrency test', 'Add a race test', ?3, 0, ?4, ?4)",
            params![
                Uuid::now_v7().to_string(),
                published.task.id.to_string(),
                joined.session_id.to_string(),
                unix_micros(now),
            ],
        )
        .unwrap();

    let sync = AgentService::new(clock.clone())
        .sync(&mut store, &context, Some(0))
        .unwrap();

    assert_eq!(sync.task.task.id, published.task.id);
    assert_eq!(sync.whoami.session.label, "worker-one");
    assert_eq!(
        sync.whoami.participant.participant_id,
        joined.participant_id
    );
    assert_eq!(sync.candidates.len(), 1);
    assert!(sync.candidates[0].capability_match);
    assert_eq!(sync.open_gaps.len(), 1);
    assert_eq!(sync.participants.len(), 1);
    assert!(
        sync.invitation_seats
            .iter()
            .any(|seat| seat.state == "consumed")
    );
    assert!(
        sync.events
            .iter()
            .any(|event| event.event_type == "task.published")
    );
    assert!(
        sync.events
            .iter()
            .any(|event| event.event_type == "agent.joined")
    );
    assert!(sync.next_cursor >= joined.event_cursor);
    assert!(sync.snapshot_id.starts_with("sb_state_snapshot_"));
    assert_eq!(sync.base_cursor, 0);
    assert!(sync.owned_claims.is_empty());
    AgentService::new(clock.clone())
        .verify_snapshot_token(&context, &sync.snapshot_id, sync.next_cursor)
        .unwrap();
    let snapshot_error = AgentService::new(clock.clone())
        .verify_snapshot_token(&context, &sync.snapshot_id, sync.next_cursor + 1)
        .unwrap_err();
    assert_eq!(snapshot_error.code(), ErrorCode::FullResyncRequired);

    let next = AgentService::new(clock.clone())
        .next(&mut store, &context, 20)
        .unwrap();
    let last_seen_before_preview: i64 = store
        .connection()
        .query_row(
            "SELECT last_seen_at FROM agent_sessions WHERE id=?1",
            [context.session_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    clock.advance(Duration::minutes(1));
    AgentService::new(clock.clone())
        .next_preview(&mut store, &context, 20)
        .unwrap();
    let last_seen_after_preview: i64 = store
        .connection()
        .query_row(
            "SELECT last_seen_at FROM agent_sessions WHERE id=?1",
            [context.session_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(last_seen_after_preview, last_seen_before_preview);
    AgentService::new(clock.clone())
        .next(&mut store, &context, 20)
        .unwrap();
    let last_seen_after_next: i64 = store
        .connection()
        .query_row(
            "SELECT last_seen_at FROM agent_sessions WHERE id=?1",
            [context.session_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(last_seen_after_next > last_seen_after_preview);
    assert!(!next.planner_complete);
    assert!(
        next.eligible_actions
            .iter()
            .any(|action| action.action == "start_work")
    );
    let start_action = next
        .eligible_actions
        .iter()
        .find(|action| action.action == "start_work")
        .unwrap();
    assert_eq!(start_action.computed_at_cursor, next.next_cursor);
    assert_eq!(start_action.confidence, "high");
    assert!(!start_action.source_entities.is_empty());
    assert!(!start_action.expand_ref.is_empty());
    assert!(next.tokens.task_plan.starts_with("sb_state_task_plan_"));
    assert!(
        next.tokens
            .work_items
            .get(&work_item_id.to_string())
            .is_some_and(|token| token.starts_with("sb_state_work_item:"))
    );
    assert!(next.tokens.claims.is_empty());
    assert!(!next.tokens.task_plan.contains(&context.token));
    let completion_basis = AgentService::new(clock.clone())
        .verify_completion_token(&context, &next.tokens.completion)
        .unwrap();
    assert_eq!(completion_basis.task_id, published.task.id);
    assert_eq!(completion_basis.plan_version, published.task.plan_version);
    let mut tampered = next.tokens.completion.clone();
    tampered.push('x');
    assert_eq!(
        AgentService::new(clock.clone())
            .verify_completion_token(&context, &tampered)
            .unwrap_err()
            .code(),
        ErrorCode::InvalidInput
    );
    assert_eq!(
        next.propose_action_ref,
        "sessionbridge agent propose-action --kind <KIND> --input <FILE>"
    );
    assert!(
        next.blocking_fingerprint.is_none(),
        "known executable work must not be misclassified as a persistent blocker"
    );

    let proposal = AgentService::new(clock.clone())
        .propose_action(
            &mut store,
            &context,
            "alternative_validation".into(),
            ProposeActionInput {
                summary: "Run an independent concurrency model check".into(),
                rationale: "Planner candidates do not cover this validation approach".into(),
                payload: serde_json::json!({"tool": "loom"}),
                dedupe_key: Some("loom-validation".into()),
                priority: "high".into(),
                safety_relevant: false,
                source_entities: vec![work_item_id.to_string()],
                provenance: serde_json::json!({"basis": "agent analysis"}),
            },
            Uuid::now_v7().to_string(),
        )
        .unwrap();
    assert_eq!(proposal.status, "proposed");
    assert!(!proposal.automatically_executed);
    let stored: (String, String) = store
        .connection()
        .query_row(
            "SELECT status, payload_json FROM agent_action_proposals WHERE id=?1",
            [&proposal.proposal_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(stored.0, "proposed");
    assert!(stored.1.contains("loom"));
    let related = AgentService::new(clock.clone())
        .propose_action(
            &mut store,
            &context,
            "alternative_validation".into(),
            ProposeActionInput {
                summary: "Run an independent concurrency model check".into(),
                rationale: "Confirm the same hypothesis with a different setup".into(),
                payload: serde_json::json!({"tool": "loom", "variant": "release"}),
                dedupe_key: None,
                priority: "normal".into(),
                safety_relevant: false,
                source_entities: vec![work_item_id.to_string()],
                provenance: serde_json::json!({"basis": "independent follow-up"}),
            },
            Uuid::now_v7().to_string(),
        )
        .unwrap();
    assert!(related.similar_proposal_ids.contains(&proposal.proposal_id));
    let provenance: (String, String, i64) = store.connection().query_row(
        "SELECT priority,provenance_json,observed_cursor FROM agent_action_proposals WHERE id=?1",
        [&related.proposal_id],
        |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
    ).unwrap();
    assert_eq!(provenance.0, "normal");
    assert!(provenance.1.contains("independent follow-up"));
    assert!(provenance.2 >= joined.event_cursor);
    let challenge = AgentService::new(clock.clone())
        .propose_action(
            &mut store,
            &context,
            "completion_challenge".into(),
            ProposeActionInput {
                summary: "Required runtime behavior is still missing".into(),
                rationale: "The current completion model omitted a user-visible requirement".into(),
                payload: serde_json::json!({"missing_requirement":"runtime smoke"}),
                dedupe_key: None,
                priority: "critical".into(),
                safety_relevant: true,
                source_entities: vec![published.task.id.to_string()],
                provenance: serde_json::json!({"basis":"completion scan"}),
            },
            Uuid::now_v7().to_string(),
        )
        .unwrap();
    let challenged = AgentService::new(clock.clone())
        .next(&mut store, &context, 100)
        .unwrap();
    assert!(challenged.completion_blockers.iter().any(|blocker| {
        blocker["code"] == "OPEN_COMPLETION_CHALLENGES" && blocker["count"] == 1
    }));
    assert!(challenged.eligible_actions.iter().any(|action| {
        action.action == "assess_action_proposal"
            && action.entity_id.as_deref() == Some(&challenge.proposal_id)
    }));
    let resolved = AgentService::new(clock.clone())
        .resolve_action_proposal(
            &mut store,
            &context,
            &challenge.proposal_id,
            "accepted",
            "Create follow-up work before completion".into(),
        )
        .unwrap();
    assert_eq!(resolved.status, "accepted");
    let after_resolution = AgentService::new(clock.clone())
        .next(&mut store, &context, 100)
        .unwrap();
    assert!(
        !after_resolution
            .completion_blockers
            .iter()
            .any(|blocker| blocker["code"] == "OPEN_COMPLETION_CHALLENGES")
    );
    let consolidation_id = Uuid::now_v7().to_string();
    store.connection().execute("INSERT INTO proposal_consolidation_requests(id,task_id,status,proposal_count,threshold,reason,created_at,updated_at) VALUES(?1,?2,'open',51,50,'test consolidation',0,0)",params![consolidation_id,published.task.id.to_string()]).unwrap();
    let claimed = AgentService::new(clock.clone())
        .claim_consolidation(&mut store, &context, &consolidation_id)
        .unwrap();
    assert_eq!(claimed.status, "claimed");
    let next_with_consolidation = AgentService::new(clock.clone())
        .next(&mut store, &context, 20)
        .unwrap();
    assert!(
        next_with_consolidation
            .obligations
            .iter()
            .any(|action| action.action == "resolve_proposal_consolidation")
    );
    let resolved = AgentService::new(clock.clone())
        .resolve_consolidation(
            &mut store,
            &context,
            &consolidation_id,
            ConsolidationResolution {
                duplicate_groups: vec![vec![
                    proposal.proposal_id.clone(),
                    related.proposal_id.clone(),
                ]],
                preserved_alternatives: vec![related.proposal_id.clone()],
                disagreements: vec!["validation environment differs".into()],
                summary: "Grouped related validations while preserving the release variant".into(),
            },
        )
        .unwrap();
    assert_eq!(resolved.status, "resolved");

    let started = WorkflowService::new(clock.clone())
        .start(
            &mut store,
            &context,
            StartWorkflowRequest {
                item_id: work_item_id.to_string().parse().unwrap(),
                resources: Vec::new(),
                inputs: Vec::new(),
                lease_seconds: Some(300),
                adopt_existing: false,
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap();
    assert_eq!(started.item_id.to_string(), work_item_id.to_string());
    assert_eq!(started.generation, 1);
    let next_with_claim = AgentService::new(clock.clone())
        .next(&mut store, &context, 20)
        .unwrap();
    assert!(
        next_with_claim
            .obligations
            .iter()
            .any(|action| action.action == "continue_owned_claim")
    );
}

#[test]
fn sync_rejects_tampered_session_token() {
    let project = test_temp_dir();
    let state = test_temp_dir();
    let now = OffsetDateTime::UNIX_EPOCH;
    let clock = FakeClock::new(now);
    WorkspaceService::new(clock.clone())
        .initialize(project.path(), "tamper", Some(state.path()))
        .unwrap();
    let workspace =
        LocatedWorkspace::discover_with_state_root(project.path(), state.path()).unwrap();
    let mut store = SqliteStore::open_with_lock(
        &workspace.paths.database_path,
        &workspace.paths.migration_lock_path,
    )
    .unwrap();

    let context = SessionContext {
        schema_version: 1,
        protocol_version: 1,
        workspace_id: workspace.config.project_id,
        repository_instance_id: workspace.paths.repository_instance_id.clone(),
        task_id: sessionbridge::domain::TaskId::new(),
        session_id: sessionbridge::domain::SessionId::new(),
        token: "not-a-session-token".into(),
        database_path: workspace.paths.database_path.display().to_string(),
    };

    let error = AgentService::new(clock)
        .whoami(&mut store, &context)
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::NotFound);
}

fn test_temp_dir() -> TempDir {
    TempDir::new().unwrap()
}
