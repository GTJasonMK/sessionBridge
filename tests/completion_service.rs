use std::path::PathBuf;

use rusqlite::params;
use sessionbridge::{
    app::{
        completion_service::{AgentMutationRequest, CompletionService, ConcludeRequest},
        invite::{
            CreateInvitationBatchRequest, InvitationService, PublicSecretDelivery,
            RevokeInvitationRequest,
        },
        join::{JoinRequest, JoinService},
        task::{PublishTaskRequest, TaskService, WorkspaceBinding},
        workspace_service::WorkspaceService,
    },
    clock::FakeClock,
    domain::{
        AutonomyPolicy, CompletionAssessment, CompletionPolicy, ConclusionOutcome,
        OpenSignalDisposition, ParticipantKind, ParticipantState, SessionContext, TaskId,
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
    workspace: LocatedWorkspace,
    store: SqliteStore,
    clock: FakeClock,
    task_id: TaskId,
    invitation_root: PathBuf,
}

impl Harness {
    fn new() -> Self {
        let project = test_temp_dir();
        let state = test_temp_dir();
        let clock = FakeClock::new(OffsetDateTime::UNIX_EPOCH + Duration::hours(1));
        WorkspaceService::new(clock.clone())
            .initialize(project.path(), "completion", Some(state.path()))
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
                    title: "Completion task".into(),
                    objective: "Agents decide when no further work remains".into(),
                    constraints: Vec::new(),
                    non_goals: Vec::new(),
                    done_conditions: vec!["Agents complete a global scan".into()],
                    autonomy_policy: AutonomyPolicy::Bounded,
                    completion_policy: CompletionPolicy::AllWorkersConcluded,
                    target_branch: None,
                    request_id: Some(Uuid::now_v7().to_string()),
                },
            )
            .unwrap();
        let invitation_root = workspace.paths.state_root.join("completion-invites");
        Self {
            _project: project,
            _state: state,
            workspace,
            store,
            clock,
            task_id: task.task.id,
            invitation_root,
        }
    }

    fn create_invitation(&mut self) -> PathBuf {
        let manifest = InvitationService::new(
            &mut self.store,
            &self.clock,
            self.workspace.config.project_id,
            &self.invitation_root,
        )
        .unwrap()
        .create_batch(CreateInvitationBatchRequest {
            task_id: self.task_id,
            count: 1,
            participant_kind: ParticipantKind::Worker,
            expires_in: Duration::hours(1),
            capability_limits: Vec::new(),
            request_id: Uuid::now_v7().to_string(),
            shell: PromptShell::Posix,
            provision_worktree: false,
        })
        .unwrap();
        match &manifest.prompts[0].secret_delivery {
            PublicSecretDelivery::File { invite_path } => invite_path.clone(),
        }
    }

    fn join(&mut self) -> SessionContext {
        let invite_file = self.create_invitation();
        let joined = JoinService::new(self.clock.clone())
            .join(
                &mut self.store,
                &self.workspace,
                JoinRequest {
                    invite_file,
                    provision_worktree: false,
                    worktree: None,
                    declared_capabilities: Vec::new(),
                    label: None,
                    client_name: None,
                    model: None,
                    request_id: None,
                },
            )
            .unwrap();
        self.store
            .connection()
            .execute(
                "UPDATE agent_sessions SET worktree_path = ?2 WHERE id = ?1",
                params![
                    joined.session_id.to_string(),
                    self.workspace
                        .paths
                        .root
                        .join(joined.session_id.to_string())
                        .display()
                        .to_string(),
                ],
            )
            .unwrap();
        read_json_private(&joined.context_path).unwrap()
    }

    fn head(&self) -> (String, i64, i64) {
        self.store
            .connection()
            .query_row(
                "SELECT phase, plan_version, completion_revision FROM tasks WHERE id = ?1",
                [self.task_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap()
    }

    fn conclude(
        &mut self,
        context: &SessionContext,
        outcome: ConclusionOutcome,
    ) -> sessionbridge::app::completion_service::ConcludeResult {
        let (_, plan_version, revision) = self.head();
        let open_signals = if outcome == ConclusionOutcome::Blocked {
            vec![OpenSignalDisposition {
                code: "EXTERNAL_BLOCKER".into(),
                entity_id: Some(Uuid::now_v7().to_string()),
                disposition: "requires_intervention".into(),
                rationale: "External dependency is unavailable".into(),
            }]
        } else {
            Vec::new()
        };
        let basis_event_cursor = latest_cursor(&self.store);
        CompletionService::new(self.clock.clone())
            .conclude(
                &mut self.store,
                context,
                ConcludeRequest {
                    outcome,
                    basis_completion_revision: revision,
                    basis_event_cursor,
                    basis_plan_version: plan_version,
                    assessment: CompletionAssessment {
                        summary: "Scanned criteria, plan, repository, tests, reviews, and integration state"
                            .into(),
                        checked: vec![
                            "task_criteria".into(),
                            "requirements_and_gaps".into(),
                            "repository_and_tests".into(),
                        ],
                        open_signals,
                        remaining_risks: vec!["Test fixture has no real repository changes".into()],
                        expected_future_work: outcome == ConclusionOutcome::Blocked,
                    },
                    request_id: Uuid::now_v7().to_string(),
                },
            )
            .unwrap()
    }
}

#[test]
fn final_worker_conclusion_creates_one_immutable_completion() {
    let mut harness = Harness::new();
    let first = harness.join();
    let second = harness.join();

    let first_result = harness.conclude(&first, ConclusionOutcome::NoFurtherWork);
    assert!(!first_result.completion_created);
    assert!(first_result.should_exit);
    let second_result = harness.conclude(&second, ConclusionOutcome::NoFurtherWork);

    assert!(second_result.completion_created);
    assert_eq!(second_result.task_phase.as_str(), "completed");
    assert!(second_result.completion_id.is_some());
    assert_eq!(harness.head().0, "completed");
    let completion_count: i64 = harness
        .store
        .connection()
        .query_row("SELECT COUNT(*) FROM task_completions", [], |row| {
            row.get(0)
        })
        .unwrap();
    let conclusion_count: i64 = harness
        .store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM task_completion_conclusions",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(completion_count, 1);
    assert_eq!(conclusion_count, 2);
}

#[test]
fn repeated_conclude_returns_the_existing_terminal_conclusion() {
    let mut harness = Harness::new();
    let context = harness.join();

    let first = harness.conclude(&context, ConclusionOutcome::NoFurtherWork);
    let repeated = harness.conclude(&context, ConclusionOutcome::NoFurtherWork);

    assert_eq!(repeated.conclusion_id, first.conclusion_id);
    assert_eq!(repeated.participation_state, ParticipantState::Concluded);
    assert!(repeated.should_exit);
    let conclusion_count: i64 = harness
        .store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM agent_conclusions WHERE task_id=?1",
            [harness.task_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(conclusion_count, 1);
}

#[test]
fn blocking_invitation_prevents_completion_until_it_is_resolved() {
    let mut harness = Harness::new();
    let context = harness.join();
    let _pending_invite = harness.create_invitation();
    let result = harness.conclude(&context, ConclusionOutcome::NoFurtherWork);
    assert!(!result.completion_created);
    assert!(
        result
            .completion_pending_reasons
            .iter()
            .any(|reason| reason["reason"] == "blocking_worker_invitation")
    );

    let invite_id: String = harness
        .store
        .connection()
        .query_row(
            "SELECT id FROM agent_invitations WHERE state = 'pending' ORDER BY created_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    InvitationService::new(
        &mut harness.store,
        &harness.clock,
        harness.workspace.config.project_id,
        &harness.invitation_root,
    )
    .unwrap()
    .revoke(RevokeInvitationRequest {
        invitation_id: invite_id.parse().unwrap(),
        reason: "The additional worker is no longer needed".into(),
        request_id: Uuid::now_v7().to_string(),
    })
    .unwrap();
    assert_eq!(harness.head().0, "completed");
    let completion_count: i64 = harness
        .store
        .connection()
        .query_row("SELECT COUNT(*) FROM task_completions", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(completion_count, 1);
}

#[test]
fn blocked_participant_never_completes_task() {
    let mut harness = Harness::new();
    let context = harness.join();

    let result = harness.conclude(&context, ConclusionOutcome::Blocked);

    assert_eq!(result.participation_state, ParticipantState::Blocked);
    assert!(result.requires_intervention);
    assert!(!result.completion_created);
    assert_eq!(harness.head().0, "active");
}

#[test]
fn resume_withdraws_current_conclusion_without_mutating_history() {
    let mut harness = Harness::new();
    let context = harness.join();
    let _pending = harness.create_invitation();
    let concluded = harness.conclude(&context, ConclusionOutcome::NoFurtherWork);
    assert_eq!(concluded.participation_state, ParticipantState::Concluded);

    let resumed = CompletionService::new(harness.clock.clone())
        .resume(
            &mut harness.store,
            &context,
            AgentMutationRequest {
                reason: Some("New directed work arrived".into()),
                request_id: Uuid::now_v7().to_string(),
            },
        )
        .unwrap();

    assert_eq!(resumed.participation_state, ParticipantState::Active);
    let (state, pointer): (String, Option<String>) = harness
        .store
        .connection()
        .query_row(
            "SELECT state, current_conclusion_id FROM task_participants WHERE session_id = ?1",
            [context.session_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, "active");
    assert!(pointer.is_none());
    let history: i64 = harness
        .store
        .connection()
        .query_row("SELECT COUNT(*) FROM agent_conclusions", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(history, 1);
}

fn latest_cursor(store: &SqliteStore) -> i64 {
    store
        .connection()
        .query_row("SELECT COALESCE(MAX(sequence), 0) FROM events", [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn test_temp_dir() -> TempDir {
    TempDir::new().unwrap()
}
