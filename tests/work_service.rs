use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::{Arc, Barrier},
};

use rusqlite::params;
use sessionbridge::{
    ErrorCode,
    app::{
        invite::{CreateInvitationBatchRequest, InvitationService, PublicSecretDelivery},
        join::{JoinRequest, JoinService},
        planning::{PlanningService, ProposeWorkItemRequest},
        speculative::{CheckpointSpeculativeRequest, SpeculativeService, StartSpeculativeRequest},
        task::{PublishTaskRequest, TaskService, WorkspaceBinding},
        work::{
            ArtifactInput, ChangeSetInput, CheckpointRequest, ClaimWorkItemRequest, EvidenceInput,
            ReclaimClaimRequest, RenewClaimRequest, ReviewDecision, ScopeReconcileRequest,
            SubmitWorkItemRequest, WorkService,
        },
        workflow::{ReviewWorkflowRequest, StartWorkflowRequest, WorkflowService},
        workspace_service::WorkspaceService,
    },
    clock::{Clock, FakeClock},
    domain::{
        AutonomyPolicy, ClaimMode, CompletionPolicy, ParticipantKind, SessionContext, WorkItemId,
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
    context_path: PathBuf,
    workspace: LocatedWorkspace,
    invitation_root: PathBuf,
    plan_version: i64,
}

#[test]
fn speculative_work_is_isolated_and_requires_revalidation_after_plan_change() {
    let mut harness = Harness::new_git();
    let head = git(
        harness.workspace.paths.root.as_path(),
        &["rev-parse", "HEAD"],
    );
    let service = SpeculativeService::new(harness.clock.clone());
    let started = service
        .start(
            &mut harness.store,
            &harness.context,
            StartSpeculativeRequest {
                title: "Explore an alternative runtime".into(),
                branch: "main".into(),
                merge_ref: "main".into(),
                assumptions: vec!["current protocol remains compatible".into()],
                provenance: serde_json::json!({"source":"agent exploration"}),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap();
    assert_eq!(started.status, "active");
    let shared_claims: i64 = harness
        .store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM work_claims WHERE task_id=?1",
            [harness.context.task_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let reservations: i64 = harness
        .store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM resource_reservations WHERE task_id=?1",
            [harness.context.task_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!((shared_claims, reservations), (0, 0));
    let ready = service
        .checkpoint(
            &mut harness.store,
            &harness.context,
            &started.id,
            CheckpointSpeculativeRequest {
                head_commit: head.clone(),
                summary: "Alternative validated in isolation".into(),
                provenance: serde_json::json!({"tests":["cargo test"]}),
            },
        )
        .unwrap();
    assert_eq!(ready.status, "ready_for_adoption");
    let adoption = service
        .propose_adoption(
            &mut harness.store,
            &harness.context,
            &started.id,
            Uuid::now_v7().to_string(),
        )
        .unwrap();
    assert_eq!(adoption.kind, "speculative_adoption");
    assert!(!adoption.automatically_executed);
    harness
        .store
        .connection()
        .execute(
            "UPDATE tasks SET plan_version=plan_version+1 WHERE id=?1",
            [harness.context.task_id.to_string()],
        )
        .unwrap();
    let stale = service
        .checkpoint(
            &mut harness.store,
            &harness.context,
            &started.id,
            CheckpointSpeculativeRequest {
                head_commit: head,
                summary: "Recheck after shared plan changed".into(),
                provenance: serde_json::json!({"reason":"plan changed"}),
            },
        )
        .unwrap();
    assert_eq!(stale.status, "revalidation_required");
    assert!(
        stale
            .recovery_options
            .iter()
            .any(|option| option["action"] == "rebase")
    );
    let abandoned = service
        .abandon(
            &mut harness.store,
            &harness.context,
            &started.id,
            "Alternative retained for history after plan changed",
        )
        .unwrap();
    assert_eq!(abandoned.status, "abandoned");
}

impl Harness {
    fn new() -> Self {
        Self::build(false)
    }

    fn new_git() -> Self {
        Self::build(true)
    }

    fn build(git_backed: bool) -> Self {
        let project = test_temp_dir();
        let state = test_temp_dir();
        if git_backed {
            git(project.path(), &["init", "-b", "main"]);
            git(project.path(), &["config", "user.name", "SessionBridge"]);
            git(
                project.path(),
                &["config", "user.email", "sessionbridge@example.invalid"],
            );
            fs::write(project.path().join("README.md"), "work service\n").unwrap();
            git(project.path(), &["add", "README.md"]);
            git(project.path(), &["commit", "-m", "initial"]);
        }
        let clock = FakeClock::new(OffsetDateTime::UNIX_EPOCH + Duration::hours(2));
        WorkspaceService::new(clock.clone())
            .initialize(project.path(), "work", Some(state.path()))
            .unwrap();
        if git_backed {
            git(project.path(), &["add", ".sessionbridge/config.toml"]);
            git(
                project.path(),
                &["commit", "-m", "add SessionBridge config"],
            );
        }
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
                    title: "Work task".into(),
                    objective: "Coordinate concurrent implementation".into(),
                    constraints: Vec::new(),
                    non_goals: Vec::new(),
                    done_conditions: vec!["Implementation is reviewed and integrated".into()],
                    autonomy_policy: AutonomyPolicy::Bounded,
                    completion_policy: CompletionPolicy::AllWorkersConcluded,
                    target_branch: None,
                    request_id: Some(Uuid::now_v7().to_string()),
                },
            )
            .unwrap();
        let invitation_root = workspace.paths.state_root.join("work-invites");
        let invitation = InvitationService::new(
            &mut store,
            &clock,
            workspace.config.project_id,
            &invitation_root,
        )
        .unwrap()
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
                    declared_capabilities: vec!["rust".into()],
                    label: None,
                    client_name: Some("work-test".into()),
                    model: None,
                    request_id: None,
                },
            )
            .unwrap();
        let context_path = joined.context_path.clone();
        let context = read_json_private(&context_path).unwrap();
        if !git_backed {
            store
                .connection()
                .execute(
                    "UPDATE agent_sessions SET worktree_path = ?2 WHERE id = ?1",
                    params![
                        joined.session_id.to_string(),
                        workspace
                            .paths
                            .root
                            .join(joined.session_id.to_string())
                            .display()
                            .to_string(),
                    ],
                )
                .unwrap();
        }
        Self {
            _project: project,
            _state: state,
            store,
            clock,
            context,
            context_path,
            workspace,
            invitation_root,
            plan_version: 0,
        }
    }

    fn item(&mut self, title: &str, resource: &str) -> WorkItemId {
        let result = PlanningService::new(self.clock.clone())
            .propose_work_item(
                &mut self.store,
                &self.context,
                ProposeWorkItemRequest {
                    kind: "implementation".into(),
                    title: title.into(),
                    description: String::new(),
                    required: true,
                    priority: 1,
                    risk: "medium".into(),
                    review_policy: "risk_based_review".into(),
                    required_capabilities: vec!["rust".into()],
                    acceptance_criteria: vec!["tests pass".into()],
                    expected_artifacts: vec!["source".into()],
                    planned_resources: vec![resource.into()],
                    parent_id: None,
                    dedupe_key: Some(title.to_ascii_lowercase().replace(' ', "-")),
                    requirement_ids: Vec::new(),
                    depends_on: Vec::new(),
                    base_plan_version: self.plan_version,
                    reason: "Create executable work".into(),
                    request_id: Uuid::now_v7().to_string(),
                },
            )
            .unwrap();
        self.plan_version = result.plan_version;
        result.entity.id
    }

    fn claim(
        &mut self,
        item_id: WorkItemId,
        lease_seconds: i64,
    ) -> sessionbridge::app::work::ClaimResult {
        WorkService::new(self.clock.clone())
            .claim(
                &mut self.store,
                &self.context,
                ClaimWorkItemRequest {
                    item_id,
                    mode: ClaimMode::Execute,
                    expected_item_version: 0,
                    observed_plan_version: self.plan_version,
                    resources: Vec::new(),
                    inputs: Vec::new(),
                    lease_seconds: Some(lease_seconds),
                    request_id: Uuid::now_v7().to_string(),
                },
            )
            .unwrap()
    }

    fn second_context(&mut self) -> SessionContext {
        let worktree =
            if self.workspace.repository.kind == sessionbridge::workspace::RepositoryKind::Git {
                let path = self
                    ._state
                    .path()
                    .join(format!("reviewer-{}", Uuid::now_v7()));
                git(
                    &self.workspace.paths.root,
                    &[
                        "worktree",
                        "add",
                        "--detach",
                        path.to_str().unwrap(),
                        "HEAD",
                    ],
                );
                Some(path)
            } else {
                None
            };
        let invitation = InvitationService::new(
            &mut self.store,
            &self.clock,
            self.workspace.config.project_id,
            &self.invitation_root,
        )
        .unwrap()
        .create_batch(CreateInvitationBatchRequest {
            task_id: self.context.task_id,
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
        let joined = JoinService::new(self.clock.clone())
            .join(
                &mut self.store,
                &self.workspace,
                JoinRequest {
                    invite_file,
                    provision_worktree: false,
                    worktree,
                    declared_capabilities: vec!["rust".into()],
                    label: None,
                    client_name: Some("work-test-2".into()),
                    model: None,
                    request_id: None,
                },
            )
            .unwrap();
        read_json_private(&joined.context_path).unwrap()
    }
}

#[test]
fn work_start_dry_run_uses_real_validation_without_mutating_the_database() {
    let mut harness = Harness::new();
    let item = harness.item("Dry-run work", "path:src/dry-run.rs");
    let output = Command::new(env!("CARGO_BIN_EXE_sessionbridge"))
        .args([
            "--context",
            harness.context_path.to_str().unwrap(),
            "--format",
            "json",
            "work",
            "start",
            &item.to_string(),
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["data"]["dry_run"], true);
    assert_eq!(response["data"]["database_mutated"], false);
    let counts: (i64, i64) = harness
        .store
        .connection()
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM work_claims WHERE work_item_id=?1),
                (SELECT COUNT(*) FROM work_attempts WHERE work_item_id=?1)",
            [item.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(counts, (0, 0));
}

#[test]
fn transient_resources_are_advisory_and_do_not_serialize_business_work() {
    let mut harness = Harness::new();
    let first = harness.item("First cached build", "path:src/first-cache.rs");
    let second = harness.item("Second cached build", "path:src/second-cache.rs");
    let service = WorkService::new(harness.clock.clone());
    for item_id in [first, second] {
        service
            .claim(
                &mut harness.store,
                &harness.context,
                ClaimWorkItemRequest {
                    item_id,
                    mode: ClaimMode::Execute,
                    expected_item_version: 0,
                    observed_plan_version: harness.plan_version,
                    resources: vec![sessionbridge::app::work::ResourceRequest {
                        resource_key: "transient:cargo-target".into(),
                        mode: "exclusive".into(),
                        policy: "enforced".into(),
                        pinned_revision: None,
                    }],
                    inputs: Vec::new(),
                    lease_seconds: Some(300),
                    request_id: Uuid::now_v7().to_string(),
                },
            )
            .unwrap();
    }
    let policies: Vec<String> = harness
        .store
        .connection()
        .prepare(
            "SELECT policy FROM resource_reservations
             WHERE task_id=?1 AND resource_key='transient:cargo-target' ORDER BY created_at",
        )
        .unwrap()
        .query_map([harness.context.task_id.to_string()], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(policies, vec!["advisory", "advisory"]);
}

#[test]
fn enforced_path_reservations_conflict_across_work_items() {
    let mut harness = Harness::new();
    let first = harness.item("Implement parent", "path:src/app");
    let second = harness.item("Implement child", "path:src/app/worker.rs");
    let _claim = harness.claim(first, 300);

    let error = WorkService::new(harness.clock.clone())
        .claim(
            &mut harness.store,
            &harness.context,
            ClaimWorkItemRequest {
                item_id: second,
                mode: ClaimMode::Execute,
                expected_item_version: 0,
                observed_plan_version: harness.plan_version,
                resources: Vec::new(),
                inputs: Vec::new(),
                lease_seconds: Some(300),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap_err();

    assert_eq!(error.code(), ErrorCode::ResourceConflict);
    assert_eq!(error.details()["conflicting_resource_key"], "path:src/app");
}

#[test]
fn execute_claim_rejects_a_dirty_registered_git_worktree() {
    let mut harness = Harness::new_git();
    let item = harness.item("Dirty worktree", "path:src/dirty.rs");
    fs::write(
        harness.workspace.paths.root.join("untracked.txt"),
        "dirty\n",
    )
    .unwrap();

    let error = WorkService::new(harness.clock.clone())
        .claim(
            &mut harness.store,
            &harness.context,
            ClaimWorkItemRequest {
                item_id: item,
                mode: ClaimMode::Execute,
                expected_item_version: 0,
                observed_plan_version: harness.plan_version,
                resources: Vec::new(),
                inputs: Vec::new(),
                lease_seconds: Some(300),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::WorktreeDirty);
    let claims: i64 = harness
        .store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM work_claims WHERE work_item_id = ?1",
            [item.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(claims, 0);
}

#[test]
fn existing_git_commits_require_explicit_adoption_and_use_target_head_as_attempt_base() {
    let mut harness = Harness::new_git();
    let item = harness.item("Adopt existing commits", "path:src/adopted.rs");
    harness
        .store
        .connection()
        .execute(
            "UPDATE tasks SET target_branch='main' WHERE id=?1",
            [harness.context.task_id.to_string()],
        )
        .unwrap();
    git(
        &harness.workspace.paths.root,
        &["checkout", "-b", "agent-work"],
    );
    harness
        .store
        .connection()
        .execute(
            "UPDATE agent_sessions SET branch='agent-work' WHERE id=?1",
            [harness.context.session_id.to_string()],
        )
        .unwrap();
    fs::create_dir_all(harness.workspace.paths.root.join("src")).unwrap();
    fs::write(
        harness.workspace.paths.root.join("src/adopted.rs"),
        "pub fn adopted() {}\n",
    )
    .unwrap();
    git(&harness.workspace.paths.root, &["add", "src/adopted.rs"]);
    git(
        &harness.workspace.paths.root,
        &["commit", "-m", "existing work"],
    );
    let target_head = git(&harness.workspace.paths.root, &["rev-parse", "main"]);

    let service = WorkflowService::new(harness.clock.clone());
    let request = || StartWorkflowRequest {
        item_id: item,
        resources: Vec::new(),
        inputs: Vec::new(),
        lease_seconds: Some(300),
        adopt_existing: false,
        request_id: Uuid::now_v7().to_string(),
    };
    let error = service
        .start(&mut harness.store, &harness.context, request())
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::WorktreeConflict);
    assert_eq!(
        error.details()["recovery_options"][0]["action"],
        "adopt_existing_work"
    );

    let adopted = service
        .start(
            &mut harness.store,
            &harness.context,
            StartWorkflowRequest {
                adopt_existing: true,
                ..request()
            },
        )
        .unwrap();
    let base_commit: String = harness
        .store
        .connection()
        .query_row(
            "SELECT base_commit FROM work_attempts WHERE id=?1",
            [adopted.attempt_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(base_commit, target_head);
}

#[test]
fn git_scope_reconciliation_batches_dirty_paths_and_is_idempotent() {
    let mut harness = Harness::new_git();
    let item = harness.item("Batch scope", "path:src/original.rs");
    let claim = harness.claim(item, 300);
    fs::create_dir_all(harness.workspace.paths.root.join("src")).unwrap();
    fs::create_dir_all(harness.workspace.paths.root.join("tests")).unwrap();
    fs::write(
        harness.workspace.paths.root.join("src/added.rs"),
        "// added\n",
    )
    .unwrap();
    fs::write(
        harness.workspace.paths.root.join("tests/added.rs"),
        "// test\n",
    )
    .unwrap();
    let service = WorkService::new(harness.clock.clone());
    let request_id = Uuid::now_v7().to_string();
    let reconciled = service
        .reconcile_scope_from_git(
            &mut harness.store,
            &harness.context,
            ScopeReconcileRequest {
                claim_id: claim.claim_id,
                generation: claim.generation,
                request_id: request_id.clone(),
            },
        )
        .unwrap();
    let keys = reconciled
        .active_reservations
        .iter()
        .map(|resource| resource.resource_key.as_str())
        .collect::<Vec<_>>();
    assert!(keys.contains(&"path:src/added.rs"));
    assert!(keys.contains(&"path:tests/added.rs"));
    let replayed = service
        .reconcile_scope_from_git(
            &mut harness.store,
            &harness.context,
            ScopeReconcileRequest {
                claim_id: claim.claim_id,
                generation: claim.generation,
                request_id,
            },
        )
        .unwrap();
    assert_eq!(replayed.event_sequence, reconciled.event_sequence);
    let reservation_count: i64 = harness
        .store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM resource_reservations
             WHERE owner_type='claim' AND owner_id=?1 AND released_at IS NULL",
            [claim.claim_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        reservation_count,
        i64::try_from(reconciled.active_reservations.len()).unwrap()
    );
}

#[test]
fn sequential_execute_claim_uses_the_current_git_head_as_attempt_baseline() {
    let mut harness = Harness::new_git();
    let initial_head = git(&harness.workspace.paths.root, &["rev-parse", "HEAD"]);
    let first_item = harness.item("First Git work", "path:src/first.rs");
    let first_claim = harness.claim(first_item, 300);

    fs::create_dir_all(harness.workspace.paths.root.join("src")).unwrap();
    fs::write(
        harness.workspace.paths.root.join("src/first.rs"),
        "pub fn first() {}\n",
    )
    .unwrap();
    git(&harness.workspace.paths.root, &["add", "src/first.rs"]);
    git(
        &harness.workspace.paths.root,
        &["commit", "-m", "implement first work item"],
    );
    let current_head = git(&harness.workspace.paths.root, &["rev-parse", "HEAD"]);
    assert_ne!(current_head, initial_head);

    WorkService::new(harness.clock.clone())
        .submit(
            &mut harness.store,
            &harness.context,
            SubmitWorkItemRequest {
                item_id: first_item,
                claim_id: first_claim.claim_id,
                generation: first_claim.generation,
                evidence: vec![EvidenceInput {
                    attempt_id: None,
                    artifact_id: None,
                    kind: "unit_test".into(),
                    result: "passed".into(),
                    subject_type: None,
                    subject_id: None,
                    commit: None,
                    verifier: None,
                    environment: serde_json::json!({}),
                    details: serde_json::json!({ "command": "cargo test" }),
                }],
                change_set: ChangeSetInput {
                    base_commit: initial_head,
                    head_commit: current_head.clone(),
                    changed_resources: Vec::new(),
                    tests: Vec::new(),
                },
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap();

    let second_item = harness.item("Second Git work", "path:src/second.rs");
    let second_claim = harness.claim(second_item, 300);
    let (attempt_base, session_head): (String, String) = harness
        .store
        .connection()
        .query_row(
            "SELECT a.base_commit, s.head_commit
             FROM work_attempts a JOIN agent_sessions s ON s.id = a.session_id
             WHERE a.id = ?1",
            [second_claim.attempt_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(attempt_base, current_head);
    assert_eq!(session_head, current_head);
}

#[test]
fn atomic_propose_and_claim_is_idempotent_and_bumps_revision_once() {
    let mut harness = Harness::new();
    let planning = PlanningService::new(harness.clock.clone());
    let before: (i64, i64) = harness
        .store
        .connection()
        .query_row(
            "SELECT plan_version, completion_revision FROM tasks WHERE id = ?1",
            [harness.context.task_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let request = propose_request("Atomic work", "atomic-work", "path:src/atomic.rs", before.0);

    let first = planning
        .propose_and_claim(&mut harness.store, &harness.context, request.clone())
        .unwrap();
    assert_eq!(first.plan.entity.plan_state, "active");
    assert_eq!(first.claim.item_id, first.plan.entity.id);
    assert_eq!(first.claim.mode, ClaimMode::Execute);
    assert_eq!(first.claim.base_plan_version, first.plan.plan_version);
    assert_eq!(first.claim.reservations.len(), 1);
    assert_eq!(
        first.claim.reservations[0].resource_key,
        "path:src/atomic.rs"
    );

    let after: (i64, i64) = harness
        .store
        .connection()
        .query_row(
            "SELECT plan_version, completion_revision FROM tasks WHERE id = ?1",
            [harness.context.task_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(after.0, before.0 + 1);
    assert_eq!(after.1, before.1 + 1);
    assert_eq!(first.plan.completion_revision, after.1);

    let persisted: (i64, i64, i64, i64) = harness
        .store
        .connection()
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM work_items WHERE id = ?1 AND plan_state = 'active'),
                (SELECT COUNT(*) FROM work_claims WHERE id = ?2 AND closed_at IS NULL),
                (SELECT COUNT(*) FROM work_attempts WHERE id = ?3 AND status = 'active'),
                (SELECT COUNT(*) FROM resource_reservations
                 WHERE owner_type = 'claim' AND owner_id = ?2 AND released_at IS NULL)",
            params![
                first.plan.entity.id.to_string(),
                first.claim.claim_id.to_string(),
                first.claim.attempt_id.to_string(),
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(persisted, (1, 1, 1, 1));

    let replay = planning
        .propose_and_claim(&mut harness.store, &harness.context, request)
        .unwrap();
    assert_eq!(replay.plan.entity.id, first.plan.entity.id);
    assert_eq!(replay.plan.plan_change_id, first.plan.plan_change_id);
    assert_eq!(replay.claim.claim_id, first.claim.claim_id);
    assert_eq!(replay.claim.attempt_id, first.claim.attempt_id);
    assert_eq!(replay.claim.event_sequence, first.claim.event_sequence);
    let after_replay: (i64, i64) = harness
        .store
        .connection()
        .query_row(
            "SELECT plan_version, completion_revision FROM tasks WHERE id = ?1",
            [harness.context.task_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(after_replay, after);
}

#[test]
fn atomic_propose_and_claim_rolls_back_plan_on_resource_conflict() {
    let mut harness = Harness::new();
    let existing_item = harness.item("Existing owner", "path:src/shared");
    let _existing_claim = harness.claim(existing_item, 300);
    let before: (i64, i64, i64, i64, i64) = harness
        .store
        .connection()
        .query_row(
            "SELECT
                (SELECT plan_version FROM tasks WHERE id = ?1),
                (SELECT completion_revision FROM tasks WHERE id = ?1),
                (SELECT COUNT(*) FROM work_items WHERE task_id = ?1),
                (SELECT COUNT(*) FROM plan_changes WHERE task_id = ?1),
                (SELECT COUNT(*) FROM work_claims WHERE task_id = ?1)",
            [harness.context.task_id.to_string()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    let request = propose_request(
        "Conflicting atomic work",
        "conflicting-atomic-work",
        "path:src/shared/child.rs",
        before.0,
    );
    let request_id = request.request_id.clone();

    let error = PlanningService::new(harness.clock.clone())
        .propose_and_claim(&mut harness.store, &harness.context, request)
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::ResourceConflict);

    let after: (i64, i64, i64, i64, i64) = harness
        .store
        .connection()
        .query_row(
            "SELECT
                (SELECT plan_version FROM tasks WHERE id = ?1),
                (SELECT completion_revision FROM tasks WHERE id = ?1),
                (SELECT COUNT(*) FROM work_items WHERE task_id = ?1),
                (SELECT COUNT(*) FROM plan_changes WHERE task_id = ?1),
                (SELECT COUNT(*) FROM work_claims WHERE task_id = ?1)",
            [harness.context.task_id.to_string()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(after, before);
    let rolled_back: (i64, i64) = harness
        .store
        .connection()
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM work_items
                 WHERE task_id = ?1 AND dedupe_key = 'conflicting-atomic-work'),
                (SELECT COUNT(*) FROM idempotency_records WHERE request_id = ?2)",
            params![harness.context.task_id.to_string(), request_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(rolled_back, (0, 0));
}

#[test]
fn stale_fence_is_rejected_and_expired_claim_can_be_reclaimed() {
    let mut harness = Harness::new();
    let item = harness.item("Lease work", "path:src/lease.rs");
    let claim = harness.claim(item, 5);

    let stale = WorkService::new(harness.clock.clone())
        .renew(
            &mut harness.store,
            &harness.context,
            RenewClaimRequest {
                claim_id: claim.claim_id,
                generation: claim.generation + 1,
                lease_seconds: Some(30),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap_err();
    assert_eq!(stale.code(), ErrorCode::StaleClaim);

    harness.clock.advance(Duration::seconds(6));
    let expired = WorkService::new(harness.clock.clone())
        .renew(
            &mut harness.store,
            &harness.context,
            RenewClaimRequest {
                claim_id: claim.claim_id,
                generation: claim.generation,
                lease_seconds: Some(30),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap_err();
    assert_eq!(expired.code(), ErrorCode::ClaimExpired);

    let reclaimed = WorkService::new(harness.clock.clone())
        .reclaim(
            &mut harness.store,
            &harness.context,
            ReclaimClaimRequest {
                claim_id: claim.claim_id,
                resources: Vec::new(),
                handoff: Some(serde_json::json!({ "reviewed_old_attempt": true })),
                lease_seconds: Some(30),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap();
    assert_eq!(reclaimed.generation, claim.generation + 1);
    assert_ne!(reclaimed.claim_id, claim.claim_id);
    let old_reason: String = harness
        .store
        .connection()
        .query_row(
            "SELECT close_reason FROM work_claims WHERE id = ?1",
            [claim.claim_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(old_reason, "reclaimed");
}

#[test]
fn checkpoint_and_submit_atomically_transfer_the_conflict_footprint() {
    let mut harness = Harness::new_git();
    let base_commit = git(
        harness.workspace.paths.root.as_path(),
        &["rev-parse", "HEAD"],
    );
    let item = harness.item("Submit work", "path:src/submit");
    let claim = harness.claim(item, 300);
    fs::create_dir_all(harness.workspace.paths.root.join("src/submit")).unwrap();
    fs::write(
        harness.workspace.paths.root.join("src/submit/lib.rs"),
        "pub fn submitted() -> bool { true }\n",
    )
    .unwrap();
    git(
        harness.workspace.paths.root.as_path(),
        &["add", "src/submit/lib.rs"],
    );
    git(
        harness.workspace.paths.root.as_path(),
        &["commit", "-m", "submit work"],
    );
    let head_commit = git(
        harness.workspace.paths.root.as_path(),
        &["rev-parse", "HEAD"],
    );
    let service = WorkService::new(harness.clock.clone());

    let checkpoint = service
        .checkpoint(
            &mut harness.store,
            &harness.context,
            CheckpointRequest {
                attempt_id: claim.attempt_id,
                generation: claim.generation,
                summary: Some("Implementation and tests are ready".into()),
                changed_resources: vec!["path:src/submit/lib.rs".into()],
                artifacts: vec![ArtifactInput {
                    attempt_id: None,
                    work_item_id: None,
                    kind: "source".into(),
                    value: "src/submit/lib.rs".into(),
                    digest: Some("sha256:test".into()),
                    revision: None,
                    metadata: serde_json::json!({}),
                }],
                evidence: vec![EvidenceInput {
                    attempt_id: None,
                    artifact_id: None,
                    kind: "unit_test".into(),
                    result: "passed".into(),
                    subject_type: None,
                    subject_id: None,
                    commit: None,
                    verifier: None,
                    environment: serde_json::json!({}),
                    details: serde_json::json!({ "command": "cargo test" }),
                }],
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap();
    assert_eq!(checkpoint.artifact_ids.len(), 1);
    assert_eq!(checkpoint.evidence_ids.len(), 1);
    let renewed_lease: i64 = harness
        .store
        .connection()
        .query_row(
            "SELECT lease_expires_at FROM work_claims WHERE id=?1",
            [claim.claim_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        renewed_lease
            >= sessionbridge::clock::unix_micros(harness.clock.now() + time::Duration::minutes(15))
    );

    let submitted = service
        .submit(
            &mut harness.store,
            &harness.context,
            SubmitWorkItemRequest {
                item_id: item,
                claim_id: claim.claim_id,
                generation: claim.generation,
                evidence: vec![EvidenceInput {
                    attempt_id: None,
                    artifact_id: None,
                    kind: "integration_test".into(),
                    result: "passed".into(),
                    subject_type: None,
                    subject_id: None,
                    commit: None,
                    verifier: None,
                    environment: serde_json::json!({}),
                    details: serde_json::json!({ "command": "cargo test --all-targets" }),
                }],
                change_set: ChangeSetInput {
                    base_commit,
                    head_commit,
                    changed_resources: vec!["path:src/submit/lib.rs".into()],
                    tests: vec![serde_json::json!({ "name": "all-targets", "passed": true })],
                },
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap();

    assert_eq!(submitted.result_state, "submitted");
    let review_handoff: (String, String) = harness
        .store
        .connection()
        .query_row(
            "SELECT to_stage,status FROM stage_handoffs WHERE work_item_id=?1 ORDER BY created_at DESC LIMIT 1",
            [item.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(review_handoff, ("review".into(), "reserved".into()));
    harness
        .store
        .connection()
        .execute(
            "UPDATE work_items SET review_policy='independent_review_required' WHERE id=?1",
            [item.to_string()],
        )
        .unwrap();
    let reviewer_context = harness.second_context();
    let workflow = WorkflowService::new(harness.clock.clone());
    let missing_evidence = Uuid::now_v7().to_string().parse().unwrap();
    let error = workflow
        .review(
            &mut harness.store,
            &harness.context,
            ReviewWorkflowRequest {
                item_id: item,
                decision: ReviewDecision::Approved,
                evidence_ids: vec![missing_evidence],
                lease_seconds: 300,
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::NotFound);
    let partial_review_state: (i64, i64, i64) = harness
        .store
        .connection()
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM work_claims WHERE work_item_id=?1 AND mode='review'),
                (SELECT COUNT(*) FROM work_attempts a JOIN work_claims c ON c.id=a.claim_id
                 WHERE a.work_item_id=?1 AND c.mode='review'),
                (SELECT COUNT(*) FROM review_results WHERE work_item_id=?1)",
            [item.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(partial_review_state, (0, 0, 0));

    let self_review_error = workflow
        .review(
            &mut harness.store,
            &harness.context,
            ReviewWorkflowRequest {
                item_id: item,
                decision: ReviewDecision::Approved,
                evidence_ids: submitted.evidence_ids.clone(),
                lease_seconds: 300,
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap_err();
    assert_eq!(self_review_error.code(), ErrorCode::ApprovalRequired);

    let review_request_id = Uuid::now_v7().to_string();
    let review = workflow
        .review(
            &mut harness.store,
            &reviewer_context,
            ReviewWorkflowRequest {
                item_id: item,
                decision: ReviewDecision::Approved,
                evidence_ids: submitted.evidence_ids.clone(),
                lease_seconds: 300,
                request_id: review_request_id.clone(),
            },
        )
        .unwrap();
    assert_eq!(review.review.result_state, "submitted");
    assert!(review.independent_review);
    assert!(review.assurance_flags.is_empty());
    let handoff_states: Vec<(String, String)> = harness
        .store
        .connection()
        .prepare(
            "SELECT to_stage,status FROM stage_handoffs WHERE work_item_id=?1 ORDER BY created_at",
        )
        .unwrap()
        .query_map([item.to_string()], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        handoff_states,
        vec![
            ("review".into(), "consumed".into()),
            ("integrate".into(), "reserved".into())
        ]
    );
    let replayed_review = workflow
        .review(
            &mut harness.store,
            &reviewer_context,
            ReviewWorkflowRequest {
                item_id: item,
                decision: ReviewDecision::Approved,
                evidence_ids: submitted.evidence_ids.clone(),
                lease_seconds: 300,
                request_id: review_request_id,
            },
        )
        .unwrap();
    assert_eq!(
        replayed_review.review.review_result_id,
        review.review.review_result_id
    );

    let preparation = workflow
        .prepare_integration(
            &mut harness.store,
            &harness.context,
            item,
            300,
            Uuid::now_v7().to_string(),
        )
        .unwrap();
    assert_eq!(preparation.change_set_id, submitted.change_set_id);
    assert_eq!(preparation.target_branch, "main");
    let integration_handoff_status: String = harness
        .store
        .connection()
        .query_row(
            "SELECT status FROM stage_handoffs WHERE work_item_id=?1 AND to_stage='integrate'",
            [item.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(integration_handoff_status, "consumed");
    let confirmed = workflow
        .confirm_integration(
            &mut harness.store,
            &harness.context,
            &preparation.preparation_id,
            serde_json::json!({ "target": "main" }),
            Uuid::now_v7().to_string(),
        )
        .unwrap();
    assert_eq!(confirmed.integration.result_state, "accepted");
    harness
        .store
        .connection()
        .execute(
            "UPDATE integration_preparations SET status='prepared',confirmed_at=NULL WHERE id=?1",
            [&preparation.preparation_id],
        )
        .unwrap();
    let replayed_confirmation = workflow
        .confirm_integration(
            &mut harness.store,
            &harness.context,
            &preparation.preparation_id,
            serde_json::json!({ "target": "main" }),
            Uuid::now_v7().to_string(),
        )
        .unwrap();
    assert_eq!(
        replayed_confirmation.integration.event_sequence,
        confirmed.integration.event_sequence
    );
    let preparation_status: String = harness
        .store
        .connection()
        .query_row(
            "SELECT status FROM integration_preparations WHERE id=?1",
            [&preparation.preparation_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(preparation_status, "confirmed");
    let (item_state, attempt_state, close_reason): (String, String, String) = harness
        .store
        .connection()
        .query_row(
            "SELECT i.result_state, a.status, c.close_reason
             FROM work_items i
             JOIN work_attempts a ON a.work_item_id = i.id
             JOIN work_claims c ON c.id = a.claim_id
             WHERE i.id = ?1 AND c.id = ?2",
            [item.to_string(), claim.claim_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(item_state, "accepted");
    assert_eq!(attempt_state, "submitted");
    assert_eq!(close_reason, "submitted");
    let active_reservations: i64 = harness
        .store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM resource_reservations
             WHERE owner_id = ?1 AND released_at IS NULL",
            [submitted.change_set_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(active_reservations, 0);
}

#[test]
fn concurrent_execute_claims_have_exactly_one_winner() {
    let mut harness = Harness::new();
    let item = harness.item("Contended work", "path:src/contended.rs");
    let second_context = harness.second_context();
    let contexts = [harness.context.clone(), second_context];
    let database_path = harness.workspace.paths.database_path.clone();
    let clock = harness.clock.clone();
    let barrier = Arc::new(Barrier::new(3));
    let mut workers = Vec::new();
    for context in contexts {
        let database_path = database_path.clone();
        let clock = clock.clone();
        let barrier = Arc::clone(&barrier);
        workers.push(std::thread::spawn(move || {
            let mut store = SqliteStore::open(&database_path).unwrap();
            barrier.wait();
            WorkService::new(clock).claim(
                &mut store,
                &context,
                ClaimWorkItemRequest {
                    item_id: item,
                    mode: ClaimMode::Execute,
                    expected_item_version: 0,
                    observed_plan_version: 1,
                    resources: Vec::new(),
                    inputs: Vec::new(),
                    lease_seconds: Some(300),
                    request_id: Uuid::now_v7().to_string(),
                },
            )
        }));
    }
    barrier.wait();
    let outcomes = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
    let loser = outcomes
        .into_iter()
        .find_map(Result::err)
        .expect("one Claim must lose the race");
    assert_eq!(loser.code(), ErrorCode::ClaimConflict);
    let active_claims: i64 = harness
        .store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM work_claims WHERE work_item_id = ?1 AND closed_at IS NULL",
            [item.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(active_claims, 1);
}

fn test_temp_dir() -> TempDir {
    TempDir::new().unwrap()
}

fn propose_request(
    title: &str,
    dedupe_key: &str,
    resource: &str,
    base_plan_version: i64,
) -> ProposeWorkItemRequest {
    ProposeWorkItemRequest {
        kind: "implementation".into(),
        title: title.into(),
        description: String::new(),
        required: true,
        priority: 1,
        risk: "low".into(),
        review_policy: "risk_based_review".into(),
        required_capabilities: vec!["rust".into()],
        acceptance_criteria: vec!["tests pass".into()],
        expected_artifacts: vec!["source".into()],
        planned_resources: vec![resource.into()],
        parent_id: None,
        dedupe_key: Some(dedupe_key.into()),
        requirement_ids: Vec::new(),
        depends_on: Vec::new(),
        base_plan_version,
        reason: "Create and claim executable work atomically".into(),
        request_id: Uuid::now_v7().to_string(),
    }
}

fn git(root: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}
