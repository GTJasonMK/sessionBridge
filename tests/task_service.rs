use std::sync::{Arc, Barrier};

use rusqlite::{Connection, params};
use sessionbridge::{
    ErrorCode,
    app::task::{
        AddCriterionRequest, PublishTaskRequest, TaskHealthOptions, TaskMutationRequest,
        TaskService, WorkspaceBinding,
    },
    clock::FakeClock,
    domain::{
        AutonomyPolicy, ClaimId, CompletionPolicy, CriterionKind, CriterionSource, EvaluationMode,
        ParticipantId, SessionId, TaskPhase, WorkItemId, WorkspaceId,
    },
    storage::SqliteStore,
};
use tempfile::tempdir;
use time::{Duration, OffsetDateTime};

const NOW_SECONDS: i64 = 1_735_689_600;
const NOW_MICROS: i64 = NOW_SECONDS * 1_000_000;

struct Harness {
    _directory: tempfile::TempDir,
    database_path: std::path::PathBuf,
    store: SqliteStore,
    service: TaskService<FakeClock>,
    clock: FakeClock,
    workspace_id: WorkspaceId,
    repository_identity: String,
}

impl Harness {
    fn new() -> Self {
        let directory = tempdir().unwrap();
        let database_path = directory.path().join("state.db");
        SqliteStore::migrate(&database_path).unwrap();
        let store = SqliteStore::open(&database_path).unwrap();
        let workspace_id = WorkspaceId::new();
        let repository_identity = "repository-instance".to_owned();
        store
            .connection()
            .execute(
                "INSERT INTO workspaces (id, name, repository_identity, created_at)
                 VALUES (?1, 'Workspace', ?2, ?3)",
                params![workspace_id.to_string(), repository_identity, NOW_MICROS],
            )
            .unwrap();
        let clock = FakeClock::new(OffsetDateTime::from_unix_timestamp(NOW_SECONDS).unwrap());
        let service = TaskService::new(clock.clone());
        Self {
            _directory: directory,
            database_path,
            store,
            service,
            clock,
            workspace_id,
            repository_identity,
        }
    }

    fn binding(&self) -> WorkspaceBinding<'_> {
        WorkspaceBinding {
            id: self.workspace_id,
            repository_identity: &self.repository_identity,
        }
    }

    fn publish(&mut self, request_id: Option<&str>) -> sessionbridge::app::task::PublishTaskResult {
        let workspace_id = self.workspace_id;
        let repository_identity = self.repository_identity.clone();
        self.service
            .publish(
                self.store.connection_mut(),
                WorkspaceBinding {
                    id: workspace_id,
                    repository_identity: &repository_identity,
                },
                publish_request(
                    "Implement coordination",
                    "Build the complete task flow",
                    request_id,
                ),
            )
            .unwrap()
    }
}

#[test]
fn concurrent_publish_retries_share_one_task_and_one_event() {
    let harness = Harness::new();
    let barrier = Arc::new(Barrier::new(3));
    let mut workers = Vec::new();
    for _ in 0..2 {
        let database_path = harness.database_path.clone();
        let workspace_id = harness.workspace_id;
        let repository_identity = harness.repository_identity.clone();
        let barrier = Arc::clone(&barrier);
        workers.push(std::thread::spawn(move || {
            let mut store = SqliteStore::open(&database_path).unwrap();
            let service = TaskService::new(FakeClock::new(
                OffsetDateTime::from_unix_timestamp(NOW_SECONDS).unwrap(),
            ));
            barrier.wait();
            service
                .publish(
                    store.connection_mut(),
                    WorkspaceBinding {
                        id: workspace_id,
                        repository_identity: &repository_identity,
                    },
                    publish_request(
                        "Concurrent task",
                        "Create exactly one task",
                        Some("concurrent-request"),
                    ),
                )
                .unwrap()
        }));
    }
    barrier.wait();
    let first = workers.remove(0).join().unwrap();
    let second = workers.remove(0).join().unwrap();

    assert_eq!(first.task.id, second.task.id);
    assert_eq!(first.event_sequence, second.event_sequence);
    assert_eq!(table_count(harness.store.connection(), "tasks"), 1);
    assert_eq!(table_count(harness.store.connection(), "events"), 1);
    assert_eq!(
        table_count(harness.store.connection(), "idempotency_records"),
        1
    );
}

#[test]
fn criterion_change_opens_a_new_completion_epoch_and_migrates_active_workers() {
    let mut harness = Harness::new();
    let published = harness.publish(Some("criterion-task"));
    let session_id = SessionId::new();
    let participant_id = ParticipantId::new();
    let invitation_id = uuid::Uuid::now_v7().to_string();
    harness
        .store
        .connection()
        .execute(
            "INSERT INTO agent_sessions (
                id, task_id, token_hash, label, declared_capabilities_json, capacity_json,
                protocol_version, last_seen_at, created_at, updated_at
             ) VALUES (?1, ?2, ?3, 'worker', '[]', '{}', 1, ?4, ?4, ?4)",
            params![
                session_id.to_string(),
                published.task.id.to_string(),
                vec![7_u8; 32],
                NOW_MICROS,
            ],
        )
        .unwrap();
    harness
        .store
        .connection()
        .execute(
            "INSERT INTO task_participants (
                id, task_id, session_id, kind, completion_epoch, state, version,
                joined_at, created_at, updated_at
             ) VALUES (?1, ?2, ?3, 'worker', 1, 'active', 0, ?4, ?4, ?4)",
            params![
                participant_id.to_string(),
                published.task.id.to_string(),
                session_id.to_string(),
                NOW_MICROS,
            ],
        )
        .unwrap();
    harness
        .store
        .connection()
        .execute(
            "INSERT INTO agent_invitations (
                id, task_id, completion_epoch, participant_kind, batch_id, token_hash,
                state, blocks_completion, capability_limits_json, prompt_template_version,
                protocol_version, expires_at, created_at, updated_at
             ) VALUES (?1, ?2, 1, 'worker', 'batch', ?3, 'pending', 1, '[]', 1, 1,
                       ?4, ?5, ?5)",
            params![
                invitation_id,
                published.task.id.to_string(),
                vec![8_u8; 32],
                NOW_MICROS + Duration::hours(1).whole_microseconds() as i64,
                NOW_MICROS,
            ],
        )
        .unwrap();
    let workspace_id = harness.workspace_id;
    let repository_identity = harness.repository_identity.clone();

    let result = harness
        .service
        .add_criterion(
            harness.store.connection_mut(),
            WorkspaceBinding {
                id: workspace_id,
                repository_identity: &repository_identity,
            },
            AddCriterionRequest {
                task_id: published.task.id,
                kind: CriterionKind::Constraint,
                content: "No credentials may be committed".into(),
                required: true,
                evaluation_mode: EvaluationMode::ContinuousGuard,
                request_id: Some("criterion-add".into()),
            },
        )
        .unwrap();

    assert_eq!(result.previous_completion_epoch, 1);
    assert_eq!(result.task.completion_epoch, 2);
    assert_eq!(result.task.plan_version, 1);
    assert_eq!(result.participants_superseded, 1);
    assert_eq!(result.participants_migrated, 1);
    assert_eq!(result.invitations_rebound, 1);
    let states: (i64, i64, i64) = harness
        .store
        .connection()
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM task_participants WHERE task_id = ?1 AND state = 'superseded'),
                (SELECT COUNT(*) FROM task_participants WHERE task_id = ?1 AND completion_epoch = 2 AND state = 'active'),
                (SELECT completion_epoch FROM agent_invitations WHERE id = ?2)",
            [published.task.id.to_string(), invitation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(states, (1, 1, 2));
}

fn publish_request(title: &str, objective: &str, request_id: Option<&str>) -> PublishTaskRequest {
    PublishTaskRequest {
        title: title.to_owned(),
        objective: objective.to_owned(),
        constraints: Vec::new(),
        non_goals: Vec::new(),
        done_conditions: Vec::new(),
        autonomy_policy: AutonomyPolicy::Bounded,
        completion_policy: CompletionPolicy::AllWorkersConcluded,
        target_branch: Some("main".to_owned()),
        request_id: request_id.map(str::to_owned),
    }
}

#[test]
fn publish_creates_task_objective_default_checklist_and_event_atomically() {
    let mut harness = Harness::new();
    let result = harness.publish(None);

    assert_eq!(result.task.phase, TaskPhase::Active);
    assert_eq!(result.task.workspace_id, harness.workspace_id);
    assert_eq!(result.task.created_at.unix_timestamp(), NOW_SECONDS);
    assert_eq!(result.task.updated_at, result.task.created_at);
    assert_eq!(result.task.completion_epoch, 1);
    assert_eq!(result.task.completion_revision, 0);
    assert_eq!(result.criteria.len(), 6);
    assert_eq!(result.criteria[0].content, "Build the complete task flow");
    assert_eq!(result.criteria[0].source, CriterionSource::User);
    assert_eq!(
        result
            .criteria
            .iter()
            .filter(|criterion| criterion.source == CriterionSource::System)
            .count(),
        5
    );

    let details = harness
        .service
        .show(
            harness.store.connection(),
            harness.binding(),
            result.task.id,
        )
        .unwrap();
    assert_eq!(details.task.id, result.task.id);
    assert_eq!(details.criteria.len(), 6);
    assert_eq!(details.event_cursor, result.event_sequence);

    let event: (String, String, String) = harness
        .store
        .connection()
        .query_row(
            "SELECT event_type, actor_type, entity_type FROM events WHERE sequence = ?1",
            [result.event_sequence],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        event,
        ("task.published".into(), "user".into(), "task".into())
    );
}

#[test]
fn explicit_done_conditions_replace_the_system_checklist() {
    let mut harness = Harness::new();
    let mut request = publish_request("Task", "Objective", None);
    request.constraints = vec!["  Keep compatibility  ".into()];
    request.non_goals = vec!["Do not add a daemon".into()];
    request.done_conditions = vec!["Integration tests pass".into()];
    let workspace_id = harness.workspace_id;
    let repository_identity = harness.repository_identity.clone();

    let result = harness
        .service
        .publish(
            harness.store.connection_mut(),
            WorkspaceBinding {
                id: workspace_id,
                repository_identity: &repository_identity,
            },
            request,
        )
        .unwrap();

    assert_eq!(result.criteria.len(), 4);
    assert!(
        result
            .criteria
            .iter()
            .all(|criterion| criterion.source == CriterionSource::User)
    );
    assert_eq!(result.criteria[1].content, "Keep compatibility");
}

#[test]
fn workspace_binding_mismatch_is_rejected_before_any_task_write() {
    let mut harness = Harness::new();
    let workspace_id = harness.workspace_id;
    let error = harness
        .service
        .publish(
            harness.store.connection_mut(),
            WorkspaceBinding {
                id: workspace_id,
                repository_identity: "another-repository-instance",
            },
            publish_request("Task", "Objective", None),
        )
        .unwrap_err();

    assert_eq!(error.code(), ErrorCode::WorkspaceNotFound);
    assert_eq!(table_count(harness.store.connection(), "tasks"), 0);
    assert_eq!(table_count(harness.store.connection(), "events"), 0);
}

#[test]
fn publish_request_id_replays_same_result_and_rejects_payload_reuse() {
    let mut harness = Harness::new();
    let first = harness.publish(Some("publish-request"));
    let replay = harness.publish(Some("publish-request"));

    assert_eq!(replay.task.id, first.task.id);
    assert_eq!(replay.event_sequence, first.event_sequence);
    assert_eq!(table_count(harness.store.connection(), "tasks"), 1);
    assert_eq!(table_count(harness.store.connection(), "events"), 1);
    assert_eq!(
        table_count(harness.store.connection(), "idempotency_records"),
        1
    );

    let workspace_id = harness.workspace_id;
    let repository_identity = harness.repository_identity.clone();
    let error = harness
        .service
        .publish(
            harness.store.connection_mut(),
            WorkspaceBinding {
                id: workspace_id,
                repository_identity: &repository_identity,
            },
            publish_request(
                "Different title",
                "Build the complete task flow",
                Some("publish-request"),
            ),
        )
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::IdempotencyKeyReused);
    assert_eq!(table_count(harness.store.connection(), "tasks"), 1);
}

#[test]
fn criterion_failure_rolls_back_task_event_and_idempotency_record() {
    let mut harness = Harness::new();
    harness
        .store
        .connection()
        .execute_batch(
            "CREATE TRIGGER reject_task_criterion
             BEFORE INSERT ON task_criteria
             BEGIN
                 SELECT RAISE(ABORT, 'criterion rejected by test');
             END;",
        )
        .unwrap();
    let workspace_id = harness.workspace_id;
    let repository_identity = harness.repository_identity.clone();

    let error = harness
        .service
        .publish(
            harness.store.connection_mut(),
            WorkspaceBinding {
                id: workspace_id,
                repository_identity: &repository_identity,
            },
            publish_request("Task", "Objective", Some("rollback-request")),
        )
        .unwrap_err();

    assert_eq!(error.code(), ErrorCode::Database);
    assert_eq!(table_count(harness.store.connection(), "tasks"), 0);
    assert_eq!(table_count(harness.store.connection(), "task_criteria"), 0);
    assert_eq!(table_count(harness.store.connection(), "events"), 0);
    assert_eq!(
        table_count(harness.store.connection(), "idempotency_records"),
        0
    );
}

#[test]
fn pause_and_resume_freeze_open_claim_lease_countdown() {
    let mut harness = Harness::new();
    let task = harness.publish(None).task;
    let session_id = insert_session(harness.store.connection(), task.id.to_string(), 1);
    let work_item_id =
        insert_work_item(harness.store.connection(), task.id.to_string(), session_id);
    let original_lease = NOW_MICROS + 60_000_000;
    insert_claim(
        harness.store.connection(),
        task.id.to_string(),
        work_item_id,
        session_id,
        original_lease,
    );

    let workspace_id = harness.workspace_id;
    let repository_identity = harness.repository_identity.clone();
    let paused = harness
        .service
        .pause(
            harness.store.connection_mut(),
            WorkspaceBinding {
                id: workspace_id,
                repository_identity: &repository_identity,
            },
            TaskMutationRequest::new(task.id),
        )
        .unwrap();
    assert_eq!(paused.task.phase, TaskPhase::Paused);
    assert_eq!(paused.effects.claims_suspended, 1);

    harness.clock.advance(Duration::seconds(30));
    let resumed = harness
        .service
        .resume(
            harness.store.connection_mut(),
            WorkspaceBinding {
                id: workspace_id,
                repository_identity: &repository_identity,
            },
            TaskMutationRequest::new(task.id),
        )
        .unwrap();
    assert_eq!(resumed.task.phase, TaskPhase::Active);
    assert_eq!(resumed.task.version, 2);
    assert_eq!(resumed.task.completion_revision, 2);
    assert_eq!(resumed.effects.claim_leases_extended, 1);
    let lease: i64 = harness
        .store
        .connection()
        .query_row("SELECT lease_expires_at FROM work_claims", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(lease, original_lease + 30_000_000);
    let event_types = event_types(harness.store.connection());
    assert_eq!(
        event_types,
        vec!["task.published", "task.paused", "task.resumed"]
    );
}

#[test]
fn lifecycle_request_id_replays_before_revalidating_the_new_phase() {
    let mut harness = Harness::new();
    let task = harness.publish(None).task;
    let workspace_id = harness.workspace_id;
    let repository_identity = harness.repository_identity.clone();
    let request = TaskMutationRequest {
        task_id: task.id,
        reason: Some("maintenance".into()),
        request_id: Some("pause-request".into()),
    };

    let first = harness
        .service
        .pause(
            harness.store.connection_mut(),
            WorkspaceBinding {
                id: workspace_id,
                repository_identity: &repository_identity,
            },
            request.clone(),
        )
        .unwrap();
    let replay = harness
        .service
        .pause(
            harness.store.connection_mut(),
            WorkspaceBinding {
                id: workspace_id,
                repository_identity: &repository_identity,
            },
            request,
        )
        .unwrap();

    assert_eq!(replay.event_sequence, first.event_sequence);
    assert_eq!(event_types(harness.store.connection()).len(), 2);

    let error = harness
        .service
        .pause(
            harness.store.connection_mut(),
            WorkspaceBinding {
                id: workspace_id,
                repository_identity: &repository_identity,
            },
            TaskMutationRequest {
                task_id: task.id,
                reason: Some("different reason".into()),
                request_id: Some("pause-request".into()),
            },
        )
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::IdempotencyKeyReused);
}

#[test]
fn invalid_lifecycle_transition_does_not_append_an_event() {
    let mut harness = Harness::new();
    let task = harness.publish(None).task;
    let workspace_id = harness.workspace_id;
    let repository_identity = harness.repository_identity.clone();

    let error = harness
        .service
        .resume(
            harness.store.connection_mut(),
            WorkspaceBinding {
                id: workspace_id,
                repository_identity: &repository_identity,
            },
            TaskMutationRequest::new(task.id),
        )
        .unwrap_err();

    assert_eq!(error.code(), ErrorCode::TaskNotActive);
    assert_eq!(
        event_types(harness.store.connection()),
        vec!["task.published"]
    );
}

#[test]
fn cancel_rolls_back_all_side_effects_when_event_append_fails_then_retries_cleanly() {
    let mut harness = Harness::new();
    let task = harness.publish(None).task;
    let session_id = insert_session(harness.store.connection(), task.id.to_string(), 2);
    let work_item_id =
        insert_work_item(harness.store.connection(), task.id.to_string(), session_id);
    let claim_id = insert_claim(
        harness.store.connection(),
        task.id.to_string(),
        work_item_id,
        session_id,
        NOW_MICROS + 60_000_000,
    );
    insert_attempt(
        harness.store.connection(),
        task.id.to_string(),
        work_item_id,
        claim_id,
        session_id,
    );
    insert_plan_change(harness.store.connection(), task.id.to_string(), session_id);
    insert_invitation(
        harness.store.connection(),
        task.id.to_string(),
        task.completion_epoch,
        7,
    );
    insert_cancelled_coordination_state(
        harness.store.connection(),
        task.id.to_string(),
        work_item_id,
        claim_id,
        session_id,
    );
    harness
        .store
        .connection()
        .execute_batch(
            "CREATE TRIGGER reject_cancel_event
             BEFORE INSERT ON events
             WHEN NEW.event_type = 'task.cancelled'
             BEGIN
                 SELECT RAISE(ABORT, 'cancel event rejected by test');
             END;",
        )
        .unwrap();
    let workspace_id = harness.workspace_id;
    let repository_identity = harness.repository_identity.clone();
    let request = TaskMutationRequest {
        task_id: task.id,
        reason: Some("stop work".into()),
        request_id: Some("cancel-request".into()),
    };

    let error = harness
        .service
        .cancel(
            harness.store.connection_mut(),
            WorkspaceBinding {
                id: workspace_id,
                repository_identity: &repository_identity,
            },
            request.clone(),
        )
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::Database);
    assert_eq!(
        task_phase(harness.store.connection(), task.id.to_string()),
        "active"
    );
    assert_eq!(
        scalar_text(
            harness.store.connection(),
            "SELECT status FROM work_attempts"
        ),
        "active"
    );
    assert_eq!(
        scalar_text(
            harness.store.connection(),
            "SELECT state FROM agent_invitations"
        ),
        "pending"
    );
    assert_eq!(
        table_count(harness.store.connection(), "idempotency_records"),
        0
    );

    harness
        .store
        .connection()
        .execute_batch("DROP TRIGGER reject_cancel_event;")
        .unwrap();
    let result = harness
        .service
        .cancel(
            harness.store.connection_mut(),
            WorkspaceBinding {
                id: workspace_id,
                repository_identity: &repository_identity,
            },
            request,
        )
        .unwrap();

    assert_eq!(result.task.phase, TaskPhase::Cancelled);
    assert_eq!(result.effects.claims_revoked, 1);
    assert_eq!(result.effects.attempts_abandoned, 1);
    assert_eq!(result.effects.plan_changes_superseded, 1);
    assert_eq!(result.effects.invitations_revoked, 1);
    assert_eq!(result.effects.reservations_released, 1);
    assert_eq!(result.effects.intents_cleared, 1);
    assert_eq!(result.effects.coordination_requests_cancelled, 1);
    assert_eq!(result.effects.blockers_cancelled, 1);
    assert_eq!(
        scalar_text(
            harness.store.connection(),
            "SELECT close_reason FROM work_claims"
        ),
        "revoked"
    );
    assert_eq!(
        scalar_text(
            harness.store.connection(),
            "SELECT status FROM work_attempts"
        ),
        "abandoned"
    );
    assert_eq!(
        scalar_text(
            harness.store.connection(),
            "SELECT state FROM agent_invitations"
        ),
        "revoked"
    );
}

#[test]
fn reopen_starts_new_completion_epoch_and_never_reuses_old_invitation() {
    let mut harness = Harness::new();
    let task = harness.publish(None).task;
    let session_id = insert_session(harness.store.connection(), task.id.to_string(), 3);
    let participant_id = ParticipantId::new();
    harness
        .store
        .connection()
        .execute(
            "INSERT INTO task_participants (
                id, task_id, session_id, kind, completion_epoch, state,
                joined_at, created_at, updated_at
             ) VALUES (?1, ?2, ?3, 'worker', 1, 'active', ?4, ?4, ?4)",
            params![
                participant_id.to_string(),
                task.id.to_string(),
                session_id.to_string(),
                NOW_MICROS,
            ],
        )
        .unwrap();
    insert_invitation(harness.store.connection(), task.id.to_string(), 1, 8);
    harness
        .store
        .connection()
        .execute(
            "UPDATE tasks
             SET phase = 'completed', completed_at = ?2,
                 completion_revision = 5, version = 1
             WHERE id = ?1",
            params![task.id.to_string(), NOW_MICROS],
        )
        .unwrap();
    let workspace_id = harness.workspace_id;
    let repository_identity = harness.repository_identity.clone();

    let result = harness
        .service
        .reopen(
            harness.store.connection_mut(),
            WorkspaceBinding {
                id: workspace_id,
                repository_identity: &repository_identity,
            },
            TaskMutationRequest {
                task_id: task.id,
                reason: Some("new user requirement".into()),
                request_id: Some("reopen-request".into()),
            },
        )
        .unwrap();

    assert_eq!(result.task.phase, TaskPhase::Active);
    assert_eq!(result.task.completion_epoch, 2);
    assert_eq!(result.task.completion_revision, 6);
    assert_eq!(result.effects.participants_superseded, 1);
    assert_eq!(result.effects.participants_migrated, 1);
    assert_eq!(result.effects.invitations_revoked, 1);
    let details = harness
        .service
        .show(harness.store.connection(), harness.binding(), task.id)
        .unwrap();
    assert!(details.completed_at.is_none());
    let participant_states: Vec<(i64, String)> = harness
        .store
        .connection()
        .prepare(
            "SELECT completion_epoch, state FROM task_participants
             WHERE task_id = ?1 ORDER BY completion_epoch",
        )
        .unwrap()
        .query_map([task.id.to_string()], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        participant_states,
        vec![(1, "superseded".into()), (2, "active".into())]
    );
    assert_eq!(
        scalar_text(
            harness.store.connection(),
            "SELECT state FROM agent_invitations"
        ),
        "revoked"
    );
    assert_eq!(
        event_types(harness.store.connection()).last().copied(),
        Some("task.completion_epoch_changed")
    );
}

#[test]
fn cancelled_task_can_be_archived_but_not_resumed() {
    let mut harness = Harness::new();
    let task = harness.publish(None).task;
    let workspace_id = harness.workspace_id;
    let repository_identity = harness.repository_identity.clone();
    harness
        .service
        .cancel(
            harness.store.connection_mut(),
            WorkspaceBinding {
                id: workspace_id,
                repository_identity: &repository_identity,
            },
            TaskMutationRequest::new(task.id),
        )
        .unwrap();

    let resume_error = harness
        .service
        .resume(
            harness.store.connection_mut(),
            WorkspaceBinding {
                id: workspace_id,
                repository_identity: &repository_identity,
            },
            TaskMutationRequest::new(task.id),
        )
        .unwrap_err();
    assert_eq!(resume_error.code(), ErrorCode::TaskNotActive);

    let archived = harness
        .service
        .archive(
            harness.store.connection_mut(),
            WorkspaceBinding {
                id: workspace_id,
                repository_identity: &repository_identity,
            },
            TaskMutationRequest::new(task.id),
        )
        .unwrap();
    assert_eq!(archived.task.phase, TaskPhase::Archived);
    let details = harness
        .service
        .show(harness.store.connection(), harness.binding(), task.id)
        .unwrap();
    assert!(details.cancelled_at.is_some());
    assert!(details.archived_at.is_some());
}

#[test]
fn health_exposes_planning_coverage_work_and_completion_cohort_signals() {
    let mut harness = Harness::new();
    let published = harness.publish(None);
    let health = harness
        .service
        .health_with_options(
            harness.store.connection(),
            harness.binding(),
            published.task.id,
            TaskHealthOptions {
                participant_stale_after: sessionbridge::app::task::PARTICIPANT_STALE_AFTER,
            },
        )
        .unwrap();
    assert!(health.planning_required);
    assert!(health.completion_cohort_open);
    assert!(health.no_progress_possible);
    assert_eq!(health.signals.uncovered_user_criteria, 1);
    assert_eq!(health.signals.worker_participants, 0);
    assert!(
        health
            .completion_signal_warnings
            .iter()
            .any(|warning| warning.code == "PLANNING_REQUIRED")
    );
    assert!(
        health
            .completion_signal_warnings
            .iter()
            .any(|warning| warning.code == "MISSING_TASK_INTEGRATION_EVIDENCE")
    );

    let requirement_id = uuid::Uuid::now_v7().to_string();
    let work_item_id = WorkItemId::new();
    harness
        .store
        .connection()
        .execute(
            "INSERT INTO requirements (
                id, task_id, kind, title, required, status, source, created_at, updated_at
             ) VALUES (?1, ?2, 'behavior', 'Required behavior', 1, 'active', 'agent', ?3, ?3)",
            params![requirement_id, published.task.id.to_string(), NOW_MICROS],
        )
        .unwrap();
    harness
        .store
        .connection()
        .execute(
            "INSERT INTO criterion_coverage (
                id, task_id, criterion_id, covered_by_type, covered_by_id,
                status, created_at, updated_at
             ) VALUES (?1, ?2, ?3, 'requirement', ?4, 'covered', ?5, ?5)",
            params![
                uuid::Uuid::now_v7().to_string(),
                published.task.id.to_string(),
                published.criteria[0].id.to_string(),
                requirement_id,
                NOW_MICROS,
            ],
        )
        .unwrap();
    harness
        .store
        .connection()
        .execute(
            "INSERT INTO work_items (
                id, task_id, kind, title, required, priority, risk,
                plan_state, result_state, created_at, updated_at
             ) VALUES (?1, ?2, 'implementation', 'Implement behavior', 1, 10, 'medium',
                       'active', 'open', ?3, ?3)",
            params![
                work_item_id.to_string(),
                published.task.id.to_string(),
                NOW_MICROS
            ],
        )
        .unwrap();
    harness
        .store
        .connection()
        .execute(
            "INSERT INTO requirement_coverage (
                id, task_id, requirement_id, covered_by_type, covered_by_id,
                coverage_state, created_at, updated_at
             ) VALUES (?1, ?2, ?3, 'work_item', ?4, 'verified', ?5, ?5)",
            params![
                uuid::Uuid::now_v7().to_string(),
                published.task.id.to_string(),
                requirement_id,
                work_item_id.to_string(),
                NOW_MICROS,
            ],
        )
        .unwrap();
    harness
        .store
        .connection()
        .execute(
            "INSERT INTO gaps (
                id, task_id, kind, severity, required, status, title,
                created_at, updated_at
             ) VALUES (?1, ?2, 'coverage', 'high', 1, 'open', 'Open gap', ?3, ?3)",
            params![
                uuid::Uuid::now_v7().to_string(),
                published.task.id.to_string(),
                NOW_MICROS,
            ],
        )
        .unwrap();
    harness
        .store
        .connection()
        .execute(
            "INSERT INTO evidence (id, task_id, kind, result, created_at)
             VALUES (?1, ?2, 'integration', 'passed', ?3)",
            params![
                uuid::Uuid::now_v7().to_string(),
                published.task.id.to_string(),
                NOW_MICROS,
            ],
        )
        .unwrap();

    let health = harness
        .service
        .health(
            harness.store.connection(),
            harness.binding(),
            published.task.id,
        )
        .unwrap();
    assert!(!health.planning_required);
    assert_eq!(health.signals.uncovered_user_criteria, 0);
    assert_eq!(health.signals.uncovered_required_requirements, 0);
    assert_eq!(health.signals.required_leaf_items_not_accepted, 1);
    assert_eq!(health.signals.ready_unclaimed_items, 1);
    assert_eq!(health.signals.open_required_gaps, 1);
    assert_eq!(health.signals.passed_task_integration_evidence, 1);
    assert!(
        !health
            .completion_signal_warnings
            .iter()
            .any(|warning| warning.code == "MISSING_TASK_INTEGRATION_EVIDENCE")
    );
}

#[test]
fn task_reads_reject_unknown_database_enum_values_instead_of_coercing_them() {
    let mut harness = Harness::new();
    let task = harness.publish(None).task;
    harness
        .store
        .connection()
        .execute_batch("PRAGMA ignore_check_constraints = ON;")
        .unwrap();
    harness
        .store
        .connection()
        .execute(
            "UPDATE tasks SET phase = 'unexpected_phase' WHERE id = ?1",
            [task.id.to_string()],
        )
        .unwrap();

    let error = harness
        .service
        .show(harness.store.connection(), harness.binding(), task.id)
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::Database);
}

fn insert_session(connection: &Connection, task_id: String, token_byte: u8) -> SessionId {
    let session_id = SessionId::new();
    connection
        .execute(
            "INSERT INTO agent_sessions (
                id, task_id, token_hash, label, protocol_version,
                last_seen_at, created_at, updated_at
             ) VALUES (?1, ?2, ?3, 'worker', 1, ?4, ?4, ?4)",
            params![
                session_id.to_string(),
                task_id,
                vec![token_byte; 32],
                NOW_MICROS,
            ],
        )
        .unwrap();
    session_id
}

fn insert_work_item(connection: &Connection, task_id: String, session_id: SessionId) -> WorkItemId {
    let work_item_id = WorkItemId::new();
    connection
        .execute(
            "INSERT INTO work_items (
                id, task_id, kind, title, required, priority, risk,
                plan_state, result_state, created_by_session_id, created_at, updated_at
             ) VALUES (?1, ?2, 'implementation', 'Implementation', 1, 10, 'medium',
                       'active', 'open', ?3, ?4, ?4)",
            params![
                work_item_id.to_string(),
                task_id,
                session_id.to_string(),
                NOW_MICROS,
            ],
        )
        .unwrap();
    work_item_id
}

fn insert_claim(
    connection: &Connection,
    task_id: String,
    work_item_id: WorkItemId,
    session_id: SessionId,
    lease_expires_at: i64,
) -> ClaimId {
    let claim_id = ClaimId::new();
    connection
        .execute(
            "INSERT INTO work_claims (
                id, task_id, work_item_id, session_id, mode, generation,
                claimed_at, lease_expires_at
             ) VALUES (?1, ?2, ?3, ?4, 'execute', 1, ?5, ?6)",
            params![
                claim_id.to_string(),
                task_id,
                work_item_id.to_string(),
                session_id.to_string(),
                NOW_MICROS,
                lease_expires_at,
            ],
        )
        .unwrap();
    claim_id
}

fn insert_attempt(
    connection: &Connection,
    task_id: String,
    work_item_id: WorkItemId,
    claim_id: ClaimId,
    session_id: SessionId,
) {
    connection
        .execute(
            "INSERT INTO work_attempts (
                id, task_id, claim_id, work_item_id, session_id,
                base_plan_version, status, started_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, 0, 'active', ?6)",
            params![
                uuid::Uuid::now_v7().to_string(),
                task_id,
                claim_id.to_string(),
                work_item_id.to_string(),
                session_id.to_string(),
                NOW_MICROS,
            ],
        )
        .unwrap();
}

fn insert_plan_change(connection: &Connection, task_id: String, session_id: SessionId) {
    connection
        .execute(
            "INSERT INTO plan_changes (
                id, task_id, proposer_session_id, base_plan_version, operations_json,
                risk_class, status, reason, created_at, updated_at
             ) VALUES (?1, ?2, ?3, 0, '[]', 'safe', 'proposed', 'Need change', ?4, ?4)",
            params![
                uuid::Uuid::now_v7().to_string(),
                task_id,
                session_id.to_string(),
                NOW_MICROS,
            ],
        )
        .unwrap();
}

fn insert_invitation(connection: &Connection, task_id: String, epoch: i64, token_byte: u8) {
    connection
        .execute(
            "INSERT INTO agent_invitations (
                id, task_id, completion_epoch, participant_kind, batch_id, token_hash,
                state, blocks_completion, prompt_template_version, protocol_version,
                expires_at, created_at, updated_at
             ) VALUES (?1, ?2, ?3, 'worker', ?4, ?5, 'pending', 1, 1, 1, ?6, ?7, ?7)",
            params![
                uuid::Uuid::now_v7().to_string(),
                task_id,
                epoch,
                uuid::Uuid::now_v7().to_string(),
                vec![token_byte; 32],
                NOW_MICROS + 3_600_000_000_i64,
                NOW_MICROS,
            ],
        )
        .unwrap();
}

fn insert_cancelled_coordination_state(
    connection: &Connection,
    task_id: String,
    work_item_id: WorkItemId,
    claim_id: ClaimId,
    session_id: SessionId,
) {
    connection
        .execute(
            "INSERT INTO resource_reservations (
                id, task_id, owner_type, owner_id, resource_key, mode, policy, created_at
             ) VALUES (?1, ?2, 'claim', ?3, 'src/lib.rs', 'write', 'enforced', ?4)",
            params![
                uuid::Uuid::now_v7().to_string(),
                task_id,
                claim_id.to_string(),
                NOW_MICROS,
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO agent_intents (
                id, task_id, session_id, kind, target_type, summary,
                expires_at, created_at, updated_at
             ) VALUES (?1, ?2, ?3, 'implement', 'work_item', 'Implementing', ?4, ?5, ?5)",
            params![
                uuid::Uuid::now_v7().to_string(),
                task_id,
                session_id.to_string(),
                NOW_MICROS + 60_000_000,
                NOW_MICROS,
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO coordination_requests (
                id, task_id, kind, target_type, requested_by_session_id,
                blocking, status, created_at, updated_at
             ) VALUES (?1, ?2, 'review', 'work_item', ?3, 1, 'open', ?4, ?4)",
            params![
                uuid::Uuid::now_v7().to_string(),
                task_id,
                session_id.to_string(),
                NOW_MICROS,
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO blockers (
                id, task_id, work_item_id, waits_for_type, status, reason,
                opened_by_session_id, opened_at
             ) VALUES (?1, ?2, ?3, 'decision', 'open', 'Needs decision', ?4, ?5)",
            params![
                uuid::Uuid::now_v7().to_string(),
                task_id,
                work_item_id.to_string(),
                session_id.to_string(),
                NOW_MICROS,
            ],
        )
        .unwrap();
}

fn table_count(connection: &Connection, table: &str) -> i64 {
    connection
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn event_types(connection: &Connection) -> Vec<&'static str> {
    let mut statement = connection
        .prepare("SELECT event_type FROM events ORDER BY sequence")
        .unwrap();
    statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(|value| match value.unwrap().as_str() {
            "task.published" => "task.published",
            "task.paused" => "task.paused",
            "task.resumed" => "task.resumed",
            "task.cancelled" => "task.cancelled",
            "task.completion_epoch_changed" => "task.completion_epoch_changed",
            "task.archived" => "task.archived",
            other => panic!("unexpected event type {other}"),
        })
        .collect()
}

fn task_phase(connection: &Connection, task_id: String) -> String {
    connection
        .query_row("SELECT phase FROM tasks WHERE id = ?1", [task_id], |row| {
            row.get(0)
        })
        .unwrap()
}

fn scalar_text(connection: &Connection, sql: &str) -> String {
    connection.query_row(sql, [], |row| row.get(0)).unwrap()
}
