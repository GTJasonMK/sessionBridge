use rusqlite::params;
use sessionbridge::{
    ErrorCode,
    app::{
        collaboration_service::{
            AcceptContractRequest, ApprovalDecision, AttestCapabilityRequest, ClearIntentRequest,
            CollaborationService, CoordinationAction, CoordinationRequestInput,
            CoordinationResponseInput, DecisionResolution, OpenBlockerRequest,
            ProposeDecisionRequest, PublishContractRequest, ResolveBlockerRequest,
            ResolveDecisionRequest, ResolveInvalidationRequest, ResolvePlanChangeRequest,
            RetireParticipantRequest, ReviseContractRequest, SetIntentRequest,
        },
        invite::{CreateInvitationBatchRequest, InvitationService, PublicSecretDelivery},
        join::{JoinRequest, JoinService},
        planning::{PlanningService, ProposeWorkItemRequest},
        task::{PublishTaskRequest, TaskService, WorkspaceBinding},
        workspace_service::WorkspaceService,
    },
    clock::{Clock, FakeClock, unix_micros},
    domain::{
        AutonomyPolicy, CompletionPolicy, ParticipantId, ParticipantKind, SessionContext,
        SessionId, TaskId,
    },
    invitation::PromptShell,
    storage::SqliteStore,
    workspace::{LocatedWorkspace, read_json_private},
};
use tempfile::TempDir;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

struct Harness {
    _project: TempDir,
    _state: TempDir,
    store: SqliteStore,
    clock: FakeClock,
    context: SessionContext,
    task_id: TaskId,
}

impl Harness {
    fn new() -> Self {
        let project = test_temp_dir();
        let state = test_temp_dir();
        let clock = FakeClock::new(OffsetDateTime::UNIX_EPOCH + Duration::hours(3));
        WorkspaceService::new(clock.clone())
            .initialize(project.path(), "collaboration", Some(state.path()))
            .unwrap();
        let workspace =
            LocatedWorkspace::discover_with_state_root(project.path(), state.path()).unwrap();
        let mut store = SqliteStore::open_with_lock(
            &workspace.paths.database_path,
            &workspace.paths.migration_lock_path,
        )
        .unwrap();
        let task = TaskService::new(clock.clone())
            .publish(
                store.connection_mut(),
                WorkspaceBinding::from(&workspace),
                PublishTaskRequest {
                    title: "Collaboration task".into(),
                    objective: "Coordinate agents through shared protocol objects".into(),
                    constraints: Vec::new(),
                    non_goals: Vec::new(),
                    done_conditions: vec!["All protocol objects are auditable".into()],
                    autonomy_policy: AutonomyPolicy::Open,
                    completion_policy: CompletionPolicy::AllWorkersConcluded,
                    target_branch: None,
                    request_id: Some(Uuid::now_v7().to_string()),
                },
            )
            .unwrap();
        let invitation = InvitationService::new(
            &mut store,
            &clock,
            workspace.config.project_id,
            workspace.paths.state_root.join("collaboration-invites"),
        )
        .unwrap()
        .create_batch(CreateInvitationBatchRequest {
            task_id: task.task.id,
            count: 1,
            participant_kind: ParticipantKind::Worker,
            expires_in: Duration::hours(1),
            capability_limits: Vec::new(),
            request_id: Uuid::now_v7().to_string(),
            shell: PromptShell::Posix,
            provision_worktree: false,
        })
        .unwrap();
        let invite_file = match &invitation.prompts[0].secret_delivery {
            PublicSecretDelivery::File { invite_path } => invite_path.clone(),
        };
        let joined = JoinService::new(clock.clone())
            .join(
                &mut store,
                &workspace,
                JoinRequest {
                    invite_file,
                    provision_worktree: false,
                    worktree: None,
                    declared_capabilities: vec!["rust".into(), "review".into()],
                    label: Some("primary".into()),
                    client_name: None,
                    model: None,
                    request_id: None,
                },
            )
            .unwrap();
        let context = read_json_private(&joined.context_path).unwrap();
        Self {
            _project: project,
            _state: state,
            store,
            clock,
            context,
            task_id: task.task.id,
        }
    }

    fn add_stale_participant(&mut self) -> ParticipantId {
        let session_id = SessionId::new();
        let participant_id = ParticipantId::new();
        let stale_at = unix_micros(self.clock.now() - Duration::minutes(31));
        self.store
            .connection()
            .execute(
                "INSERT INTO agent_sessions (
                    id, task_id, token_hash, label, declared_capabilities_json, capacity_json,
                    protocol_version, last_seen_at, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, 'stale-worker', '[]', '{}', 1, ?4, ?4, ?4)",
                params![
                    session_id.to_string(),
                    self.task_id.to_string(),
                    vec![11_u8; 32],
                    stale_at,
                ],
            )
            .unwrap();
        self.store
            .connection()
            .execute(
                "INSERT INTO task_participants (
                    id, task_id, session_id, kind, completion_epoch, state, version,
                    joined_at, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, 'worker', 1, 'active', 0, ?4, ?4, ?4)",
                params![
                    participant_id.to_string(),
                    self.task_id.to_string(),
                    session_id.to_string(),
                    stale_at,
                ],
            )
            .unwrap();
        participant_id
    }
}

#[test]
fn intent_coordination_contract_decision_and_invalidation_round_trip() {
    let mut harness = Harness::new();
    let service = CollaborationService::new(harness.clock.clone());
    let intent = service
        .set_intent(
            &mut harness.store,
            &harness.context,
            SetIntentRequest {
                kind: "plan".into(),
                target_type: "task".into(),
                target_id: Some(harness.task_id.to_string()),
                summary: "Inspect missing coordination work".into(),
                expected_outputs: vec!["work_items".into()],
                ttl_seconds: Some(300),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap();
    assert_eq!(intent.state, "active");
    service
        .clear_intent(
            &mut harness.store,
            &harness.context,
            ClearIntentRequest {
                intent_id: Some(intent.intent_ids[0].clone()),
                reason: Some("Planning finished".into()),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap();

    let coordination = service
        .request_coordination(
            &mut harness.store,
            &harness.context,
            CoordinationRequestInput {
                kind: "review".into(),
                target_type: "task".into(),
                target_id: Some(harness.task_id.to_string()),
                assigned_session_id: Some(harness.context.session_id.to_string()),
                required_capability: Some("review".into()),
                blocking: true,
                correlation_id: Some("review-self-test".into()),
                payload: serde_json::json!({ "scope": "protocol" }),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap();
    let acknowledged = service
        .respond_coordination(
            &mut harness.store,
            &harness.context,
            CoordinationResponseInput {
                coordination_request_id: coordination.coordination_request_id.clone(),
                action: CoordinationAction::Acknowledge,
                response: serde_json::json!({ "accepted": true }),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap();
    assert_eq!(acknowledged.status, "acknowledged");
    service
        .respond_coordination(
            &mut harness.store,
            &harness.context,
            CoordinationResponseInput {
                coordination_request_id: coordination.coordination_request_id,
                action: CoordinationAction::Resolve,
                response: serde_json::json!({ "result": "reviewed" }),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap();

    let contract = service
        .publish_contract(
            &mut harness.store,
            &harness.context,
            PublishContractRequest {
                name: "agent-sync-v1".into(),
                kind: "api".into(),
                owner_type: "task".into(),
                owner_id: harness.task_id.to_string(),
                compatibility: "compatible".into(),
                content: serde_json::json!({ "fields": ["cursor", "candidates"] }),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap();
    let accepted = service
        .accept_contract(
            &mut harness.store,
            &harness.context,
            AcceptContractRequest {
                contract_id: contract.contract_id.clone(),
                revision: 1,
                evidence_ids: Vec::new(),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap();
    assert_eq!(accepted.state, "accepted");
    let revised = service
        .revise_contract(
            &mut harness.store,
            &harness.context,
            ReviseContractRequest {
                contract_id: contract.contract_id,
                compatibility: "breaking".into(),
                content: serde_json::json!({ "fields": ["cursor", "candidates", "inbox"] }),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap();
    assert_eq!(revised.revision, 2);

    let decision = service
        .propose_decision(
            &mut harness.store,
            &harness.context,
            ProposeDecisionRequest {
                topic: "Use cursor-based sync".into(),
                proposal: serde_json::json!({ "choice": "cursor" }),
                supersedes_id: None,
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap();
    let decision = service
        .resolve_decision(
            &mut harness.store,
            &harness.context,
            ResolveDecisionRequest {
                decision_id: decision.decision_id,
                resolution: DecisionResolution::Approve,
                reason: Some("Single-worker fallback".into()),
                evidence_ids: Vec::new(),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap();
    assert_eq!(decision.status, "accepted");

    let invalidation_id = Uuid::now_v7().to_string();
    harness
        .store
        .connection()
        .execute(
            "INSERT INTO invalidations (
                id, task_id, source_type, source_id, target_type, target_id,
                new_revision, status, reason, created_at
             ) VALUES (?1, ?2, 'contract', ?3, 'work_item', 'target', '2', 'open',
                       'Contract changed', ?4)",
            params![
                invalidation_id,
                harness.task_id.to_string(),
                revised.contract_id,
                unix_micros(harness.clock.now()),
            ],
        )
        .unwrap();
    let resolved = service
        .resolve_invalidation(
            &mut harness.store,
            &harness.context,
            ResolveInvalidationRequest {
                invalidation_id,
                reason: "Dependent work was revalidated".into(),
                evidence_ids: Vec::new(),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap();
    assert_eq!(resolved.status, "resolved");
}

#[test]
fn stale_clean_participant_can_be_retired_with_handoff() {
    let mut harness = Harness::new();
    let target = harness.add_stale_participant();
    let target_session: String = harness
        .store
        .connection()
        .query_row(
            "SELECT session_id FROM task_participants WHERE id=?1",
            [target.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let external_wait = Uuid::now_v7().to_string();
    harness
        .store
        .connection()
        .execute(
            "UPDATE task_participants SET state='waiting' WHERE id=?1",
            [target.to_string()],
        )
        .unwrap();
    harness.store.connection().execute("INSERT INTO external_waits(id,task_id,participant_id,session_id,detector_type,detector_json,status,reassess_interval_seconds,next_reassessment_at,request_id,request_hash,created_at,updated_at) VALUES(?1,?2,?3,?4,'file_exists',?5,'active',30,30,'retire-wait','hash',0,0)",rusqlite::params![external_wait,harness.task_id.to_string(),target.to_string(),target_session,r#"{"type":"file_exists","path":"/tmp/retire-wait"}"#]).unwrap();

    let result = CollaborationService::new(harness.clock.clone())
        .retire_participant(
            &mut harness.store,
            &harness.context,
            RetireParticipantRequest {
                participant_id: target.to_string(),
                reason: "Session stopped heartbeating and owns no work".into(),
                handoff: serde_json::json!({ "repository_checked": true }),
                evidence_ids: Vec::new(),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap();

    assert_eq!(result.status, "applied");
    let state: String = harness
        .store
        .connection()
        .query_row(
            "SELECT state FROM task_participants WHERE id = ?1",
            [target.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state, "retired");
    let delivery:(String,String)=harness.store.connection().query_row("SELECT w.status,o.status FROM external_waits w JOIN external_wait_delivery_outbox o ON o.wait_id=w.id WHERE w.id=?1",[external_wait],|row|Ok((row.get(0)?,row.get(1)?))).unwrap();
    assert_eq!(delivery, ("cancelled".into(), "pending".into()));
}

#[test]
fn system_reconciled_invalidation_cannot_be_manually_resolved() {
    let mut harness = Harness::new();
    let invalidation_id = Uuid::now_v7().to_string();
    harness
        .store
        .connection()
        .execute(
            "INSERT INTO invalidations(
                id,task_id,source_type,source_id,target_type,target_id,new_revision,
                status,reason,created_at,management_mode
             ) VALUES(?1,?2,'target_branch_integrity','main','change_set','change-set',
                      'rewritten','open','drift',0,'system_reconciled')",
            params![invalidation_id, harness.task_id.to_string()],
        )
        .unwrap();
    let error = CollaborationService::new(harness.clock.clone())
        .resolve_invalidation(
            &mut harness.store,
            &harness.context,
            ResolveInvalidationRequest {
                invalidation_id,
                reason: "ignore drift".into(),
                evidence_ids: Vec::new(),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidInput);
    assert_eq!(
        error.details()["recovery_options"][0]["action"],
        "restore_external_state_and_reconcile"
    );
}

#[test]
fn capability_attestation_and_plan_approval_apply_pending_work() {
    let mut harness = Harness::new();
    harness
        .store
        .connection()
        .execute(
            "UPDATE tasks SET autonomy_policy = 'proposal' WHERE id = ?1",
            [harness.task_id.to_string()],
        )
        .unwrap();
    let proposed = PlanningService::new(harness.clock.clone())
        .propose_work_item(
            &mut harness.store,
            &harness.context,
            ProposeWorkItemRequest {
                kind: "implementation".into(),
                title: "Pending work".into(),
                description: String::new(),
                required: true,
                priority: 1,
                risk: "low".into(),
                review_policy: "risk_based_review".into(),
                required_capabilities: vec!["database".into()],
                acceptance_criteria: vec!["done".into()],
                expected_artifacts: Vec::new(),
                planned_resources: vec!["path:src/db".into()],
                parent_id: None,
                dedupe_key: Some("pending-work".into()),
                requirement_ids: Vec::new(),
                depends_on: Vec::new(),
                base_plan_version: 0,
                reason: "Needs peer review under proposal policy".into(),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap();
    assert_eq!(proposed.entity.plan_state, "proposed");

    let service = CollaborationService::new(harness.clock.clone());
    let capability = service
        .attest_capability(
            &mut harness.store,
            &harness.context,
            AttestCapabilityRequest {
                capability: "database".into(),
                scope: vec!["task".into()],
                expires_in_seconds: Some(600),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap();
    assert_eq!(capability.capability, "database");
    let approved = service
        .resolve_plan_change(
            &mut harness.store,
            &harness.context,
            ResolvePlanChangeRequest {
                plan_change_id: proposed.plan_change_id,
                decision: ApprovalDecision::Approve,
                reason: Some("Single-worker fallback after review".into()),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap();
    assert_eq!(approved.target_status.as_deref(), Some("applied"));
    let state: String = harness
        .store
        .connection()
        .query_row(
            "SELECT plan_state FROM work_items WHERE id = ?1",
            [proposed.entity.id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state, "active");
    let blocker = service
        .open_blocker(
            &mut harness.store,
            &harness.context,
            OpenBlockerRequest {
                work_item_id: proposed.entity.id.to_string(),
                waits_for_type: "external_service".into(),
                waits_for_id: Some("service-a".into()),
                reason: "External service is unavailable".into(),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap();
    assert_eq!(blocker.status, "open");
    let resolved = service
        .resolve_blocker(
            &mut harness.store,
            &harness.context,
            ResolveBlockerRequest {
                blocker_id: blocker.blocker_id,
                resolution: "Service recovered".into(),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap();
    assert_eq!(resolved.status, "resolved");
}

fn test_temp_dir() -> TempDir {
    TempDir::new().unwrap()
}
