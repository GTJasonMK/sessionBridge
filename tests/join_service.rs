use std::{fs, path::PathBuf};

use rusqlite::params;
use sessionbridge::{
    ErrorCode,
    app::join::{JoinRequest, JoinService},
    clock::{FakeClock, unix_micros},
    domain::{InvitationId, TaskId},
    invitation::{
        InviteSecret, InviteSecretInput, InviteToken, PROMPT_TEMPLATE_VERSION, write_json_private,
    },
    storage::SqliteStore,
    workspace::{LocatedWorkspace, PROTOCOL_VERSION, initialize},
};
use tempfile::TempDir;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

struct Fixture {
    _project: TempDir,
    _state: TempDir,
    workspace: LocatedWorkspace,
    store: SqliteStore,
    clock: FakeClock,
    task_id: TaskId,
    invite_id: InvitationId,
    invite_path: PathBuf,
    secret_bytes: Vec<u8>,
}

impl Fixture {
    fn new(expires_at: OffsetDateTime) -> Self {
        let project = test_temp_dir();
        let state = test_temp_dir();
        let now = OffsetDateTime::UNIX_EPOCH + Duration::days(20_000);
        let clock = FakeClock::new(now);
        let workspace = initialize(project.path(), "join-test", Some(state.path())).unwrap();
        SqliteStore::migrate_with_lock(
            &workspace.paths.database_path,
            &workspace.paths.migration_lock_path,
        )
        .unwrap();
        let store = SqliteStore::open_with_lock(
            &workspace.paths.database_path,
            &workspace.paths.migration_lock_path,
        )
        .unwrap();
        let task_id = TaskId::new();
        let invite_id = InvitationId::new();
        let token = InviteToken::generate().unwrap();
        let batch_id = Uuid::now_v7().to_string();
        let now_micros = unix_micros(now);
        store
            .connection()
            .execute(
                "INSERT INTO workspaces (id, name, repository_identity, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    workspace.config.project_id.to_string(),
                    workspace.config.name,
                    workspace.paths.repository_instance_id,
                    now_micros,
                ],
            )
            .unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO tasks (
                    id, workspace_id, title, phase, autonomy_policy, completion_policy,
                    plan_version, completion_epoch, completion_revision, version,
                    created_at, updated_at
                 ) VALUES (?1, ?2, 'join task', 'active', 'bounded',
                           'all_workers_concluded', 0, 1, 0, 0, ?3, ?3)",
                params![
                    task_id.to_string(),
                    workspace.config.project_id.to_string(),
                    now_micros,
                ],
            )
            .unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO agent_invitations (
                    id, task_id, completion_epoch, participant_kind, batch_id,
                    token_hash, state, blocks_completion, capability_limits_json,
                    prompt_template_version, protocol_version, expires_at,
                    created_at, updated_at
                 ) VALUES (?1, ?2, 1, 'worker', ?3, ?4, 'pending', 1, '[]',
                           ?5, ?6, ?7, ?8, ?8)",
                params![
                    invite_id.to_string(),
                    task_id.to_string(),
                    batch_id,
                    token.hash().as_bytes().as_slice(),
                    PROMPT_TEMPLATE_VERSION.parse::<i64>().unwrap(),
                    i64::from(PROTOCOL_VERSION),
                    unix_micros(expires_at),
                    now_micros,
                ],
            )
            .unwrap();
        let secret = InviteSecret::new(InviteSecretInput {
            invite_id: invite_id.to_string(),
            task_id: task_id.to_string(),
            completion_epoch: 1,
            batch_id: Some(batch_id),
            token,
            protocol_version: PROTOCOL_VERSION.to_string(),
            prompt_template_version: PROMPT_TEMPLATE_VERSION.into(),
            expires_at,
        })
        .unwrap();
        let secret_bytes = secret.to_json_bytes().unwrap();
        let invite_path = workspace
            .paths
            .invites_dir
            .join(format!("{invite_id}.json"));
        write_json_private(&invite_path, &secret).unwrap();

        Self {
            _project: project,
            _state: state,
            workspace,
            store,
            clock,
            task_id,
            invite_id,
            invite_path,
            secret_bytes,
        }
    }

    fn request(&self, request_id: Option<String>) -> JoinRequest {
        JoinRequest {
            invite_file: self.invite_path.clone(),
            provision_worktree: false,
            worktree: None,
            declared_capabilities: vec!["rust".into(), "testing".into(), "rust".into()],
            label: None,
            client_name: Some("integration-test".into()),
            model: None,
            request_id,
        }
    }

    fn restore_secret(&self) {
        fs::write(&self.invite_path, &self.secret_bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.invite_path, fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
}

fn test_temp_dir() -> TempDir {
    TempDir::new().unwrap()
}

#[test]
fn join_consumes_invitation_and_creates_private_context() {
    let now = OffsetDateTime::UNIX_EPOCH + Duration::days(20_000);
    let mut fixture = Fixture::new(now + Duration::hours(1));
    let mut request = fixture.request(None);
    request.label = Some("  Build Reviewer  ".into());
    let workspace = fixture.workspace.clone();

    let result = JoinService::new(fixture.clock.clone())
        .join(&mut fixture.store, &workspace, request)
        .unwrap();

    assert_eq!(result.completion_epoch, 1);
    assert_eq!(result.worktree_path, fixture.workspace.paths.root);
    assert!(result.context_path.is_file());
    assert!(!fixture.invite_path.exists());
    let invitation_state: String = fixture
        .store
        .connection()
        .query_row(
            "SELECT state FROM agent_invitations WHERE id = ?1",
            [fixture.invite_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(invitation_state, "consumed");
    let participant_count: i64 = fixture
        .store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM task_participants WHERE task_id = ?1 AND state = 'active'",
            [fixture.task_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(participant_count, 1);
    let nickname: String = fixture
        .store
        .connection()
        .query_row(
            "SELECT label FROM agent_sessions WHERE id = ?1",
            [result.session_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(nickname, "Build Reviewer");
    let executor_identity: (String, String) = fixture
        .store
        .connection()
        .query_row(
            "SELECT executor_identity, executor_identity_source
             FROM agent_sessions WHERE id = ?1",
            [result.session_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        executor_identity,
        (fixture.invite_id.to_string(), "invitation".into())
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(result.context_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn same_request_id_replays_but_new_request_cannot_adopt_session() {
    let now = OffsetDateTime::UNIX_EPOCH + Duration::days(20_000);
    let mut fixture = Fixture::new(now + Duration::hours(1));
    let request_id = Uuid::now_v7().to_string();
    let request = fixture.request(Some(request_id.clone()));
    let workspace = fixture.workspace.clone();
    let first = JoinService::new(fixture.clock.clone())
        .join(&mut fixture.store, &workspace, request)
        .unwrap();

    fixture.restore_secret();
    let request = fixture.request(Some(request_id));
    let replay = JoinService::new(fixture.clock.clone())
        .join(&mut fixture.store, &workspace, request)
        .unwrap();
    assert_eq!(replay.session_id, first.session_id);
    assert_eq!(replay.context_path, first.context_path);

    fixture.restore_secret();
    let request = fixture.request(None);
    let error = JoinService::new(fixture.clock.clone())
        .join(&mut fixture.store, &workspace, request)
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InviteConsumed);
}

#[test]
fn expired_invitation_is_reconciled_in_database() {
    let now = OffsetDateTime::UNIX_EPOCH + Duration::days(20_000);
    let mut fixture = Fixture::new(now - Duration::seconds(1));
    let request = fixture.request(None);
    let workspace = fixture.workspace.clone();

    let error = JoinService::new(fixture.clock.clone())
        .join(&mut fixture.store, &workspace, request)
        .unwrap_err();

    assert_eq!(error.code(), ErrorCode::InviteExpired);
    let state: String = fixture
        .store
        .connection()
        .query_row(
            "SELECT state FROM agent_invitations WHERE id = ?1",
            [fixture.invite_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state, "expired");
}
