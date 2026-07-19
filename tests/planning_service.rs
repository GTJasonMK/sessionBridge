use sessionbridge::{
    ErrorCode,
    app::{
        invite::{CreateInvitationBatchRequest, InvitationService, PublicSecretDelivery},
        join::{JoinRequest, JoinService},
        planning::{
            AddRequirementRequest, PlanningService, ProposeWorkItemRequest, RelateWorkItemsRequest,
            ReportGapRequest, ResolveGapRequest, SplitChildInput, SplitWorkItemRequest,
            SupersedeRequirementRequest,
        },
        task::{PublishTaskRequest, TaskService, WorkspaceBinding},
        workspace_service::WorkspaceService,
    },
    clock::FakeClock,
    domain::{
        AutonomyPolicy, CompletionPolicy, CriterionId, ParticipantKind, SessionContext, TaskId,
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
    objective_id: CriterionId,
}

impl Harness {
    fn new(kind: ParticipantKind, autonomy: AutonomyPolicy) -> Self {
        let project = test_temp_dir();
        let state = test_temp_dir();
        let clock = FakeClock::new(OffsetDateTime::UNIX_EPOCH);
        WorkspaceService::new(clock.clone())
            .initialize(project.path(), "planning", Some(state.path()))
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
                    title: "Planning task".into(),
                    objective: "Create a shared plan".into(),
                    constraints: Vec::new(),
                    non_goals: Vec::new(),
                    done_conditions: vec!["Plan is implemented".into()],
                    autonomy_policy: autonomy,
                    completion_policy: CompletionPolicy::AllWorkersConcluded,
                    target_branch: None,
                    request_id: Some(Uuid::now_v7().to_string()),
                },
            )
            .unwrap();
        let objective_id = published
            .criteria
            .iter()
            .find(|criterion| criterion.kind.as_str() == "objective")
            .unwrap()
            .id;
        let artifact_root = workspace.paths.state_root.join("planning-invites");
        let invitation = {
            let mut service = InvitationService::new(
                &mut store,
                &clock,
                workspace.config.project_id,
                artifact_root,
            )
            .unwrap();
            service
                .create_batch(CreateInvitationBatchRequest {
                    task_id: published.task.id,
                    count: 1,
                    participant_kind: kind,
                    expires_in: Duration::hours(1),
                    capability_limits: Vec::new(),
                    request_id: Uuid::now_v7().to_string(),
                    shell: PromptShell::Posix,
                    provision_worktree: false,
                })
                .unwrap()
        };
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
                    declared_capabilities: vec!["rust".into()],
                    label: None,
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
            task_id: published.task.id,
            objective_id,
        }
    }

    fn task_versions(&self) -> (i64, i64) {
        self.store
            .connection()
            .query_row(
                "SELECT plan_version, completion_revision FROM tasks WHERE id = ?1",
                [self.task_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
    }
}

#[test]
fn bounded_agent_builds_requirement_and_work_item_with_coverage() {
    let mut harness = Harness::new(ParticipantKind::Worker, AutonomyPolicy::Bounded);
    let planning = PlanningService::new(harness.clock.clone());
    let requirement = planning
        .add_requirement(
            &mut harness.store,
            &harness.context,
            AddRequirementRequest {
                kind: "functional".into(),
                title: "Expose a synchronized task view".into(),
                description: "Agents must see the same current state".into(),
                required: true,
                criterion_ids: vec![harness.objective_id],
                base_plan_version: 0,
                reason: "Derive an implementable requirement from the objective".into(),
                request_id: "requirement-one".into(),
            },
        )
        .unwrap();
    assert_eq!(requirement.entity.status, "active");
    assert_eq!(requirement.plan_version, 1);

    let work = planning
        .propose_work_item(
            &mut harness.store,
            &harness.context,
            ProposeWorkItemRequest {
                kind: "implementation".into(),
                title: "Implement synchronized view".into(),
                description: String::new(),
                required: true,
                priority: 10,
                risk: "medium".into(),
                review_policy: "risk_based_review".into(),
                required_capabilities: vec!["rust".into()],
                acceptance_criteria: vec!["integration test passes".into()],
                expected_artifacts: vec!["source".into()],
                planned_resources: vec!["path:src/app".into()],
                parent_id: None,
                dedupe_key: Some("sync-view".into()),
                requirement_ids: vec![requirement.entity.id],
                depends_on: Vec::new(),
                base_plan_version: 1,
                reason: "Create executable work".into(),
                request_id: "work-one".into(),
            },
        )
        .unwrap();

    assert_eq!(work.entity.plan_state, "active");
    assert_eq!(work.plan_version, 2);
    let coverage: String = harness
        .store
        .connection()
        .query_row(
            "SELECT coverage_state FROM requirement_coverage
             WHERE requirement_id = ?1 AND covered_by_id = ?2",
            [
                requirement.entity.id.to_string(),
                work.entity.id.to_string(),
            ],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(coverage, "planned");
}

#[test]
fn plan_version_cas_and_dedupe_are_enforced() {
    let mut harness = Harness::new(ParticipantKind::Worker, AutonomyPolicy::Bounded);
    let planning = PlanningService::new(harness.clock.clone());
    let request = ProposeWorkItemRequest {
        kind: "verification".into(),
        title: "Test coordination".into(),
        description: String::new(),
        required: true,
        priority: 1,
        risk: "low".into(),
        review_policy: "risk_based_review".into(),
        required_capabilities: Vec::new(),
        acceptance_criteria: vec!["test passes".into()],
        expected_artifacts: Vec::new(),
        planned_resources: Vec::new(),
        parent_id: None,
        dedupe_key: Some("coord-test".into()),
        requirement_ids: Vec::new(),
        depends_on: Vec::new(),
        base_plan_version: 0,
        reason: "Cover verification".into(),
        request_id: "work-a".into(),
    };
    planning
        .propose_work_item(&mut harness.store, &harness.context, request.clone())
        .unwrap();

    let stale = planning
        .propose_work_item(
            &mut harness.store,
            &harness.context,
            ProposeWorkItemRequest {
                request_id: "work-b".into(),
                title: "Another test".into(),
                dedupe_key: Some("another".into()),
                ..request.clone()
            },
        )
        .unwrap_err();
    assert_eq!(stale.code(), ErrorCode::PlanChanged);

    let duplicate = planning
        .propose_work_item(
            &mut harness.store,
            &harness.context,
            ProposeWorkItemRequest {
                request_id: "work-c".into(),
                base_plan_version: 1,
                title: "Duplicate test".into(),
                ..request
            },
        )
        .unwrap_err();
    assert_eq!(duplicate.code(), ErrorCode::VersionConflict);
}

#[test]
fn dependency_relations_reject_cycles() {
    let mut harness = Harness::new(ParticipantKind::Worker, AutonomyPolicy::Bounded);
    let planning = PlanningService::new(harness.clock.clone());
    let first = planning
        .propose_work_item(
            &mut harness.store,
            &harness.context,
            work_request("First", 0),
        )
        .unwrap();
    let second = planning
        .propose_work_item(
            &mut harness.store,
            &harness.context,
            ProposeWorkItemRequest {
                base_plan_version: 1,
                title: "Second".into(),
                request_id: Uuid::now_v7().to_string(),
                ..work_request("Second template", 1)
            },
        )
        .unwrap();
    planning
        .relate_work_items(
            &mut harness.store,
            &harness.context,
            RelateWorkItemsRequest {
                from_id: first.entity.id,
                to_id: second.entity.id,
                relation_type: "depends_on".into(),
                base_plan_version: 2,
                reason: "First depends on second".into(),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap();
    let error = planning
        .relate_work_items(
            &mut harness.store,
            &harness.context,
            RelateWorkItemsRequest {
                from_id: second.entity.id,
                to_id: first.entity.id,
                relation_type: "depends_on".into(),
                base_plan_version: 3,
                reason: "Would create a cycle".into(),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidInput);
}

#[test]
fn split_supersedes_parent_and_creates_active_children() {
    let mut harness = Harness::new(ParticipantKind::Worker, AutonomyPolicy::Bounded);
    let planning = PlanningService::new(harness.clock.clone());
    let parent = planning
        .propose_work_item(
            &mut harness.store,
            &harness.context,
            work_request("Parent", 0),
        )
        .unwrap();
    let result = planning
        .split_work_item(
            &mut harness.store,
            &harness.context,
            SplitWorkItemRequest {
                item_id: parent.entity.id,
                expected_item_version: 0,
                base_plan_version: 1,
                children: vec![split_child("Child A"), split_child("Child B")],
                reason: "Separate independently executable resources".into(),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap();
    assert_eq!(result.parent_plan_state, "superseded");
    assert_eq!(result.child_ids.len(), 2);
    let active_children: i64 = harness
        .store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM work_items WHERE parent_id = ?1 AND plan_state = 'active'",
            [parent.entity.id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(active_children, 2);
}

#[test]
fn required_requirement_needs_replacement_and_gap_resolution_is_audited() {
    let mut harness = Harness::new(ParticipantKind::Worker, AutonomyPolicy::Bounded);
    let planning = PlanningService::new(harness.clock.clone());
    let requirement = planning
        .add_requirement(
            &mut harness.store,
            &harness.context,
            AddRequirementRequest {
                kind: "functional".into(),
                title: "Required behavior".into(),
                description: String::new(),
                required: true,
                criterion_ids: vec![harness.objective_id],
                base_plan_version: 0,
                reason: "Capture required behavior".into(),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap();
    let error = planning
        .supersede_requirement(
            &mut harness.store,
            &harness.context,
            SupersedeRequirementRequest {
                requirement_id: requirement.entity.id,
                replacement_id: None,
                base_plan_version: 1,
                reason: "Remove requirement".into(),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::ApprovalRequired);

    let gap = planning
        .report_gap(
            &mut harness.store,
            &harness.context,
            ReportGapRequest {
                kind: "verification".into(),
                severity: "medium".into(),
                required: true,
                title: "Missing test".into(),
                description: String::new(),
                dedupe_key: Some("missing-test".into()),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap();
    let resolved = planning
        .resolve_gap(
            &mut harness.store,
            &harness.context,
            ResolveGapRequest {
                gap_id: gap.gap_id,
                reason: "The test was added".into(),
                evidence_ids: Vec::new(),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap();
    assert_eq!(resolved.status, "resolved");
    assert!(resolved.completion_revision > gap.completion_revision);
}

fn work_request(title: &str, base_plan_version: i64) -> ProposeWorkItemRequest {
    ProposeWorkItemRequest {
        kind: "implementation".into(),
        title: title.into(),
        description: String::new(),
        required: true,
        priority: 1,
        risk: "low".into(),
        review_policy: "risk_based_review".into(),
        required_capabilities: Vec::new(),
        acceptance_criteria: vec!["done".into()],
        expected_artifacts: Vec::new(),
        planned_resources: Vec::new(),
        parent_id: None,
        dedupe_key: Some(title.to_ascii_lowercase().replace(' ', "-")),
        requirement_ids: Vec::new(),
        depends_on: Vec::new(),
        base_plan_version,
        reason: "Create work".into(),
        request_id: Uuid::now_v7().to_string(),
    }
}

fn split_child(title: &str) -> SplitChildInput {
    SplitChildInput {
        kind: "implementation".into(),
        title: title.into(),
        description: String::new(),
        required: true,
        priority: 1,
        risk: "low".into(),
        review_policy: "risk_based_review".into(),
        required_capabilities: Vec::new(),
        acceptance_criteria: vec!["done".into()],
        expected_artifacts: Vec::new(),
        planned_resources: Vec::new(),
        dedupe_key: Some(title.to_ascii_lowercase().replace(' ', "-")),
        depends_on: Vec::new(),
    }
}

#[test]
fn proposal_policy_records_pending_plan_without_advancing_plan_version() {
    let mut harness = Harness::new(ParticipantKind::Worker, AutonomyPolicy::Proposal);
    let result = PlanningService::new(harness.clock.clone())
        .add_requirement(
            &mut harness.store,
            &harness.context,
            AddRequirementRequest {
                kind: "functional".into(),
                title: "Proposed requirement".into(),
                description: String::new(),
                required: true,
                criterion_ids: vec![harness.objective_id],
                base_plan_version: 0,
                reason: "Needs approval under proposal policy".into(),
                request_id: "proposal".into(),
            },
        )
        .unwrap();

    assert_eq!(result.entity.status, "proposed");
    assert_eq!(result.plan_change_status, "pending_approval");
    assert_eq!(result.plan_version, 0);
    assert_eq!(harness.task_versions().0, 0);
}

#[test]
fn proposal_policy_rejects_atomic_propose_and_claim_without_partial_plan() {
    let mut harness = Harness::new(ParticipantKind::Worker, AutonomyPolicy::Proposal);
    let before = harness.task_versions();

    let error = PlanningService::new(harness.clock.clone())
        .propose_and_claim(
            &mut harness.store,
            &harness.context,
            work_request("Needs approval", before.0),
        )
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::ProposalReviewRequired);

    let after: (i64, i64, i64, i64) = harness
        .store
        .connection()
        .query_row(
            "SELECT
                (SELECT plan_version FROM tasks WHERE id = ?1),
                (SELECT completion_revision FROM tasks WHERE id = ?1),
                (SELECT COUNT(*) FROM work_items WHERE task_id = ?1),
                (SELECT COUNT(*) FROM plan_changes WHERE task_id = ?1)",
            [harness.context.task_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(after, (before.0, before.1, 0, 0));
}

#[test]
fn observer_cannot_change_plan_or_report_completion_relevant_gap() {
    let mut harness = Harness::new(ParticipantKind::Observer, AutonomyPolicy::Bounded);
    let error = PlanningService::new(harness.clock.clone())
        .report_gap(
            &mut harness.store,
            &harness.context,
            ReportGapRequest {
                kind: "missing_test".into(),
                severity: "high".into(),
                required: true,
                title: "Missing test".into(),
                description: String::new(),
                dedupe_key: Some("missing-test".into()),
                request_id: "observer-gap".into(),
            },
        )
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::CapabilityNotGranted);
}

#[test]
fn gap_is_completion_relevant_but_does_not_change_plan_version() {
    let mut harness = Harness::new(ParticipantKind::Worker, AutonomyPolicy::Bounded);
    let before = harness.task_versions();
    let result = PlanningService::new(harness.clock.clone())
        .report_gap(
            &mut harness.store,
            &harness.context,
            ReportGapRequest {
                kind: "missing_test".into(),
                severity: "high".into(),
                required: true,
                title: "Missing concurrency test".into(),
                description: String::new(),
                dedupe_key: Some("concurrency-test".into()),
                request_id: "gap-one".into(),
            },
        )
        .unwrap();
    let after = harness.task_versions();

    assert_eq!(result.status, "open");
    assert_eq!(after.0, before.0);
    assert_eq!(after.1, before.1 + 1);
}

fn test_temp_dir() -> TempDir {
    TempDir::new().unwrap()
}
