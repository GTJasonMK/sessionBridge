use std::{fs, process::Command};

use sessionbridge::{
    app::{
        invite::{CreateInvitationBatchRequest, InvitationService, PublicSecretDelivery},
        join::{JoinRequest, JoinService},
        task::{PublishTaskRequest, TaskService, WorkspaceBinding},
        workspace_service::WorkspaceService,
    },
    clock::FakeClock,
    domain::{AutonomyPolicy, CompletionPolicy, ParticipantKind},
    git::GitRepository,
    invitation::PromptShell,
    storage::SqliteStore,
    workspace::link,
};
use tempfile::TempDir;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

#[test]
fn links_git_worktree_and_registers_it_during_join() {
    let repository_dir = test_temp_dir();
    let state_dir = test_temp_dir();
    let linked_dir = test_temp_dir();
    git(repository_dir.path(), &["init", "-b", "main"]);
    git(
        repository_dir.path(),
        &["config", "user.email", "test@example.com"],
    );
    git(
        repository_dir.path(),
        &["config", "user.name", "SessionBridge Test"],
    );
    fs::write(repository_dir.path().join("README.md"), "workspace link\n").unwrap();
    git(repository_dir.path(), &["add", "README.md"]);
    git(repository_dir.path(), &["commit", "-m", "initial"]);

    let linked_path = linked_dir.path().join("linked-agent");
    GitRepository::open(repository_dir.path())
        .unwrap()
        .provision_worktree(&linked_path, "agent-linked", Some("main"))
        .unwrap();
    let clock = FakeClock::new(OffsetDateTime::UNIX_EPOCH + Duration::hours(4));
    WorkspaceService::new(clock.clone())
        .initialize(
            repository_dir.path(),
            "linked-workspace",
            Some(state_dir.path()),
        )
        .unwrap();
    let source = sessionbridge::workspace::LocatedWorkspace::discover_with_state_root(
        repository_dir.path(),
        state_dir.path(),
    )
    .unwrap();
    let linked = link(&source, &linked_path).unwrap();
    assert_eq!(linked.config.project_id, source.config.project_id);
    assert_eq!(
        linked.paths.repository_instance_id,
        source.paths.repository_instance_id
    );
    assert_eq!(linked.paths.database_path, source.paths.database_path);

    let mut store = SqliteStore::open_with_lock(
        &source.paths.database_path,
        &source.paths.migration_lock_path,
    )
    .unwrap();
    let task = TaskService::new(clock.clone())
        .publish(
            store.connection_mut(),
            WorkspaceBinding::from(&source),
            PublishTaskRequest {
                title: "Linked worktree task".into(),
                objective: "Register an existing worktree".into(),
                constraints: Vec::new(),
                non_goals: Vec::new(),
                done_conditions: vec!["join succeeds".into()],
                autonomy_policy: AutonomyPolicy::Bounded,
                completion_policy: CompletionPolicy::AllWorkersConcluded,
                target_branch: Some("main".into()),
                request_id: Some(Uuid::now_v7().to_string()),
            },
        )
        .unwrap();
    let invitation = InvitationService::new(
        &mut store,
        &clock,
        source.config.project_id,
        source.paths.state_root.join("link-invites"),
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
    let joined = JoinService::new(clock)
        .join(
            &mut store,
            &source,
            JoinRequest {
                invite_file,
                provision_worktree: false,
                worktree: Some(linked_path.clone()),
                declared_capabilities: vec!["rust".into()],
                label: Some("linked-agent".into()),
                client_name: None,
                model: None,
                request_id: None,
            },
        )
        .unwrap();
    assert_eq!(joined.worktree_path, fs::canonicalize(linked_path).unwrap());
}

fn git(path: &std::path::Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .status()
            .unwrap()
            .success()
    );
}

fn test_temp_dir() -> TempDir {
    TempDir::new().unwrap()
}
