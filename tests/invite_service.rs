use std::{collections::BTreeSet, fs, path::PathBuf};

use rusqlite::params;
use sessionbridge::{
    ErrorCode,
    app::invite::{
        CreateInvitationBatchRequest, InvitationService, PublicSecretDelivery,
        RevokeInvitationRequest, RotateInvitationRequest, ShowInvitationRequest,
    },
    clock::{Clock, FakeClock, unix_micros},
    domain::{InvitationState, ParticipantKind, TaskId, WorkspaceId},
    invitation::{InviteSecret, PromptShell},
    storage::SqliteStore,
};
use tempfile::{Builder, TempDir};
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};

struct Harness {
    store: SqliteStore,
    clock: FakeClock,
    workspace_id: WorkspaceId,
    task_id: TaskId,
    artifacts: PathBuf,
    _directory: TempDir,
}

impl Harness {
    fn new() -> Self {
        let fixture_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp");
        fs::create_dir_all(&fixture_root).unwrap();
        let directory = Builder::new()
            .prefix("invite-service-")
            .tempdir_in(fixture_root)
            .unwrap();
        let database_path = directory.path().join("state.db");
        SqliteStore::migrate(&database_path).unwrap();
        let store = SqliteStore::open(&database_path).unwrap();
        let clock =
            FakeClock::new(OffsetDateTime::parse("2026-07-10T12:00:00Z", &Rfc3339).unwrap());
        let workspace_id = WorkspaceId::new();
        let task_id = TaskId::new();
        let now = unix_micros(clock.now());
        store
            .connection()
            .execute(
                "INSERT INTO workspaces (id, name, repository_identity, created_at)
                 VALUES (?1, 'Invite tests', ?2, ?3)",
                params![
                    workspace_id.to_string(),
                    format!("repo:{workspace_id}"),
                    now,
                ],
            )
            .unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO tasks (
                    id, workspace_id, title, phase, autonomy_policy, completion_policy,
                    completion_epoch, created_at, updated_at
                 ) VALUES (?1, ?2, 'Coordinate agents', 'active', 'bounded',
                           'all_workers_concluded', 1, ?3, ?3)",
                params![task_id.to_string(), workspace_id.to_string(), now],
            )
            .unwrap();
        let artifacts = directory.path().join("artifacts");

        Self {
            store,
            clock,
            workspace_id,
            task_id,
            artifacts,
            _directory: directory,
        }
    }

    fn service(&mut self) -> InvitationService<'_, FakeClock> {
        InvitationService::new(
            &mut self.store,
            &self.clock,
            self.workspace_id,
            &self.artifacts,
        )
        .unwrap()
    }

    fn create_request(
        &self,
        request_id: &str,
        count: usize,
        participant_kind: ParticipantKind,
    ) -> CreateInvitationBatchRequest {
        CreateInvitationBatchRequest {
            task_id: self.task_id,
            count,
            participant_kind,
            expires_in: Duration::hours(1),
            capability_limits: vec!["git.write".to_owned(), "task.plan".to_owned()],
            request_id: request_id.to_owned(),
            shell: PromptShell::Posix,
            provision_worktree: true,
        }
    }
}

#[test]
fn creates_single_private_sendable_prompt_without_exposing_token() {
    let mut harness = Harness::new();
    let request = harness.create_request("create-one", 1, ParticipantKind::Worker);
    let result = harness.service().create_batch(request).unwrap();

    assert_eq!(result.prompts.len(), 1);
    let manifest = &result.prompts[0];
    assert_eq!(manifest.status, InvitationState::Pending);
    assert_eq!(manifest.participant_kind, ParticipantKind::Worker);
    assert!(manifest.blocks_completion);
    assert!(manifest.sendable);
    assert!(!manifest.secret_exposed);
    assert_eq!(manifest.batch_index, 1);
    assert_eq!(manifest.batch_count, 1);

    let prompt = manifest.prompt.as_deref().unwrap();
    assert!(prompt.contains("sessionbridge agent join --invite-file"));
    assert!(!prompt.contains("--request-id"));
    assert_eq!(fs::read_to_string(&manifest.prompt_path).unwrap(), prompt);

    let invite_path = invite_path(manifest);
    let secret = InviteSecret::from_json_slice(&fs::read(&invite_path).unwrap()).unwrap();
    assert!(!prompt.contains(secret.token.expose_secret()));
    assert_eq!(secret.invite_id, manifest.invite_id.to_string());
    assert_eq!(secret.task_id, harness.task_id.to_string());

    let token_hash: Vec<u8> = harness
        .store
        .connection()
        .query_row(
            "SELECT token_hash FROM agent_invitations WHERE id = ?1",
            [manifest.invite_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(token_hash.as_slice(), secret.token.hash().as_bytes());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        assert_eq!(
            fs::metadata(invite_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&manifest.prompt_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(manifest.prompt_path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    assert_eq!(count(&harness.store, "agent_invitations"), 1);
    assert_eq!(count(&harness.store, "events"), 1);
    assert_eq!(count(&harness.store, "idempotency_records"), 1);
    assert_eq!(completion_revision(&harness), 1);
    let prepared: i64 = harness
        .store
        .connection()
        .query_row(
            "SELECT count(*) FROM agent_invitations WHERE state = 'prepared'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(prepared, 0);
}

#[test]
fn creates_observer_batch_as_distinct_nonblocking_invitations() {
    let mut harness = Harness::new();
    let request = harness.create_request("create-batch", 3, ParticipantKind::Observer);
    let result = harness.service().create_batch(request).unwrap();

    assert_eq!(result.prompts.len(), 3);
    let invite_ids: BTreeSet<_> = result
        .prompts
        .iter()
        .map(|manifest| manifest.invite_id.to_string())
        .collect();
    let prompt_paths: BTreeSet<_> = result
        .prompts
        .iter()
        .map(|manifest| manifest.prompt_path.clone())
        .collect();
    let secret_paths: BTreeSet<_> = result.prompts.iter().map(invite_path).collect();
    assert_eq!(invite_ids.len(), 3);
    assert_eq!(prompt_paths.len(), 3);
    assert_eq!(secret_paths.len(), 3);

    for (index, manifest) in result.prompts.iter().enumerate() {
        assert_eq!(manifest.batch_id, result.batch_id);
        assert_eq!(manifest.batch_index, index + 1);
        assert_eq!(manifest.batch_count, 3);
        assert_eq!(manifest.status, InvitationState::Pending);
        assert_eq!(manifest.participant_kind, ParticipantKind::Observer);
        assert!(!manifest.blocks_completion);
        assert!(manifest.sendable);
        let secret =
            InviteSecret::from_json_slice(&fs::read(invite_path(manifest)).unwrap()).unwrap();
        assert!(
            !manifest
                .prompt
                .as_deref()
                .unwrap()
                .contains(secret.token.expose_secret())
        );
    }

    let task_id = harness.task_id;
    let listed = harness.service().list(task_id).unwrap();
    assert_eq!(listed.len(), 3);
    assert!(listed.iter().all(|manifest| manifest.prompt.is_none()));
    assert_eq!(count(&harness.store, "agent_invitations"), 3);
    assert_eq!(count(&harness.store, "events"), 3);
    assert_eq!(completion_revision(&harness), 1);
}

#[test]
fn corrupt_batch_marker_is_reported_instead_of_guessing_prompt_metadata() {
    let mut harness = Harness::new();
    let request = harness.create_request("corrupt-marker", 1, ParticipantKind::Worker);
    let created = harness.service().create_batch(request).unwrap();
    let invitation_id = created.prompts[0].invite_id;
    let marker_path = created.prompts[0]
        .prompt_path
        .parent()
        .unwrap()
        .join(".sessionbridge-request.json");
    fs::write(marker_path, b"{not-json").unwrap();

    let error = harness
        .service()
        .show(ShowInvitationRequest {
            invitation_id,
            include_prompt: true,
            audit: true,
        })
        .unwrap_err();

    assert_eq!(error.code(), ErrorCode::InvalidInput);
    assert!(error.to_string().contains("serialization error"));
}

#[test]
fn database_failure_rolls_back_entire_batch_and_cleans_staging() {
    let mut harness = Harness::new();
    harness
        .store
        .connection()
        .execute_batch(
            "CREATE TRIGGER test_reject_second_invitation
             BEFORE INSERT ON agent_invitations
             WHEN (SELECT count(*) FROM agent_invitations) >= 1
             BEGIN
                 SELECT RAISE(ABORT, 'reject second invitation');
             END;",
        )
        .unwrap();
    let request = harness.create_request("create-fails", 2, ParticipantKind::Worker);

    let error = harness.service().create_batch(request).unwrap_err();

    assert_eq!(error.code(), ErrorCode::Database);
    assert_eq!(count(&harness.store, "agent_invitations"), 0);
    assert_eq!(count(&harness.store, "events"), 0);
    assert_eq!(count(&harness.store, "idempotency_records"), 0);
    assert_eq!(completion_revision(&harness), 0);
    let workspace_artifacts = harness.artifacts.join(harness.workspace_id.to_string());
    assert!(workspace_artifacts.is_dir());
    assert_eq!(fs::read_dir(workspace_artifacts).unwrap().count(), 0);
}

#[test]
fn retry_recovers_finalized_files_and_prepared_rows_without_new_tokens() {
    let mut harness = Harness::new();
    harness
        .store
        .connection()
        .execute_batch(
            "CREATE TRIGGER test_reject_batch_publish
             BEFORE UPDATE OF state ON agent_invitations
             WHEN OLD.state = 'prepared' AND NEW.state = 'pending'
             BEGIN
                 SELECT RAISE(ABORT, 'interrupt publish');
             END;",
        )
        .unwrap();
    let request = harness.create_request("recover-create", 2, ParticipantKind::Worker);

    let error = harness.service().create_batch(request.clone()).unwrap_err();

    assert_eq!(error.code(), ErrorCode::Database);
    let prepared: i64 = harness
        .store
        .connection()
        .query_row(
            "SELECT count(*) FROM agent_invitations WHERE state = 'prepared'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(prepared, 2);
    assert_eq!(count(&harness.store, "events"), 0);
    assert_eq!(count(&harness.store, "idempotency_records"), 0);
    assert_eq!(completion_revision(&harness), 1);
    let before: Vec<(String, Vec<u8>)> = {
        let mut statement = harness
            .store
            .connection()
            .prepare("SELECT id, token_hash FROM agent_invitations ORDER BY id")
            .unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    harness
        .store
        .connection()
        .execute_batch("DROP TRIGGER test_reject_batch_publish;")
        .unwrap();

    let recovered = harness.service().create_batch(request).unwrap();

    let after: Vec<(String, Vec<u8>)> = {
        let mut statement = harness
            .store
            .connection()
            .prepare("SELECT id, token_hash FROM agent_invitations ORDER BY id")
            .unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    assert_eq!(after, before);
    assert_eq!(recovered.prompts.len(), 2);
    assert!(
        recovered
            .prompts
            .iter()
            .all(|manifest| manifest.status == InvitationState::Pending && manifest.sendable)
    );
    assert_eq!(count(&harness.store, "agent_invitations"), 2);
    assert_eq!(count(&harness.store, "events"), 2);
    assert_eq!(count(&harness.store, "idempotency_records"), 1);
    assert_eq!(completion_revision(&harness), 1);
}

#[test]
fn create_is_idempotent_and_rejects_request_id_payload_reuse() {
    let mut harness = Harness::new();
    let request = harness.create_request("stable-create", 1, ParticipantKind::Worker);

    let first = harness.service().create_batch(request.clone()).unwrap();
    let replay = harness.service().create_batch(request).unwrap();

    assert_eq!(replay, first);
    assert_eq!(count(&harness.store, "agent_invitations"), 1);
    assert_eq!(count(&harness.store, "events"), 1);
    assert_eq!(count(&harness.store, "idempotency_records"), 1);
    assert_eq!(completion_revision(&harness), 1);

    let changed = harness.create_request("stable-create", 2, ParticipantKind::Worker);
    let error = harness.service().create_batch(changed).unwrap_err();
    assert_eq!(error.code(), ErrorCode::IdempotencyKeyReused);
    assert_eq!(count(&harness.store, "agent_invitations"), 1);
}

#[test]
fn revoke_is_authoritative_idempotent_and_keeps_only_audit_prompt() {
    let mut harness = Harness::new();
    let create = harness.create_request("create-revoke", 1, ParticipantKind::Worker);
    let created = harness.service().create_batch(create).unwrap();
    let invitation_id = created.prompts[0].invite_id;
    let secret_path = invite_path(&created.prompts[0]);

    let request = RevokeInvitationRequest {
        invitation_id,
        reason: "sent to the wrong session".to_owned(),
        request_id: "revoke-one".to_owned(),
    };
    let revoked = harness.service().revoke(request.clone()).unwrap();
    let replay = harness.service().revoke(request).unwrap();

    assert_eq!(revoked, replay);
    assert_eq!(revoked.invitation.status, InvitationState::Revoked);
    assert!(!revoked.invitation.sendable);
    assert!(!secret_path.exists());
    let state: String = harness
        .store
        .connection()
        .query_row(
            "SELECT state FROM agent_invitations WHERE id = ?1",
            [invitation_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state, "revoked");

    let error = harness
        .service()
        .show(ShowInvitationRequest {
            invitation_id,
            include_prompt: true,
            audit: false,
        })
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InviteRevoked);
    let audit = harness
        .service()
        .show(ShowInvitationRequest {
            invitation_id,
            include_prompt: true,
            audit: true,
        })
        .unwrap();
    assert!(!audit.sendable);
    assert!(audit.prompt.as_deref().unwrap().contains("agent join"));
    assert_eq!(completion_revision(&harness), 2);
}

#[test]
fn rotate_publishes_new_secret_before_atomically_revoking_old_invitation() {
    let mut harness = Harness::new();
    let create = harness.create_request("create-rotate", 1, ParticipantKind::Worker);
    let created = harness.service().create_batch(create).unwrap();
    let old = &created.prompts[0];
    let old_id = old.invite_id;
    let old_secret_path = invite_path(old);

    let request = RotateInvitationRequest {
        invitation_id: old_id,
        expires_in: Duration::hours(2),
        request_id: "rotate-one".to_owned(),
    };
    let rotated = harness.service().rotate(request.clone()).unwrap();
    let replay = harness.service().rotate(request).unwrap();

    assert_eq!(rotated, replay);
    assert_eq!(rotated.prompts.len(), 1);
    let new = &rotated.prompts[0];
    assert_ne!(new.invite_id, old_id);
    assert_eq!(new.status, InvitationState::Pending);
    assert_eq!(new.rotates_from_id, Some(old_id));
    assert!(new.sendable);
    assert!(invite_path(new).is_file());
    assert!(!old_secret_path.exists());

    let states: Vec<(String, String)> = {
        let mut statement = harness
            .store
            .connection()
            .prepare(
                "SELECT id, state FROM agent_invitations
                 WHERE id IN (?1, ?2) ORDER BY id",
            )
            .unwrap();
        statement
            .query_map([old_id.to_string(), new.invite_id.to_string()], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    assert!(states.contains(&(old_id.to_string(), "revoked".to_owned())));
    assert!(states.contains(&(new.invite_id.to_string(), "pending".to_owned())));
    assert_eq!(count(&harness.store, "agent_invitations"), 2);
    assert_eq!(count(&harness.store, "idempotency_records"), 2);
    assert_eq!(completion_revision(&harness), 3);
}

#[test]
fn interrupted_rotate_keeps_old_pending_until_new_publish_and_revoke_commit_together() {
    let mut harness = Harness::new();
    let create = harness.create_request("create-rotate-recovery", 1, ParticipantKind::Worker);
    let created = harness.service().create_batch(create).unwrap();
    let old = &created.prompts[0];
    let old_id = old.invite_id;
    let old_secret_path = invite_path(old);
    harness
        .store
        .connection()
        .execute_batch(&format!(
            "CREATE TRIGGER test_interrupt_old_revoke
             BEFORE UPDATE OF state ON agent_invitations
             WHEN OLD.id = '{old_id}' AND NEW.state = 'revoked'
             BEGIN
                 SELECT RAISE(ABORT, 'interrupt rotation');
             END;"
        ))
        .unwrap();
    let request = RotateInvitationRequest {
        invitation_id: old_id,
        expires_in: Duration::hours(1),
        request_id: "recover-rotate".to_owned(),
    };

    let error = harness.service().rotate(request.clone()).unwrap_err();

    assert_eq!(error.code(), ErrorCode::Database);
    let interim: Vec<(String, String)> = {
        let mut statement = harness
            .store
            .connection()
            .prepare("SELECT id, state FROM agent_invitations ORDER BY created_at, id")
            .unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    assert_eq!(interim.len(), 2);
    assert!(interim.contains(&(old_id.to_string(), "pending".to_owned())));
    assert!(
        interim
            .iter()
            .any(|(id, state)| id != &old_id.to_string() && state == "prepared")
    );
    assert!(old_secret_path.is_file());
    assert_eq!(count(&harness.store, "events"), 1);
    assert_eq!(count(&harness.store, "idempotency_records"), 1);
    assert_eq!(completion_revision(&harness), 2);
    harness
        .store
        .connection()
        .execute_batch("DROP TRIGGER test_interrupt_old_revoke;")
        .unwrap();

    let recovered = harness.service().rotate(request).unwrap();

    assert_eq!(recovered.prompts.len(), 1);
    let new = &recovered.prompts[0];
    assert_eq!(new.status, InvitationState::Pending);
    assert_eq!(new.rotates_from_id, Some(old_id));
    assert!(!old_secret_path.exists());
    assert!(invite_path(new).is_file());
    let old_state: String = harness
        .store
        .connection()
        .query_row(
            "SELECT state FROM agent_invitations WHERE id = ?1",
            [old_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(old_state, "revoked");
    assert_eq!(count(&harness.store, "events"), 2);
    assert_eq!(count(&harness.store, "idempotency_records"), 2);
    assert_eq!(completion_revision(&harness), 3);
}

#[test]
fn paused_task_rejects_create_without_database_or_file_side_effects() {
    let mut harness = Harness::new();
    harness
        .store
        .connection()
        .execute(
            "UPDATE tasks SET phase = 'paused' WHERE id = ?1",
            [harness.task_id.to_string()],
        )
        .unwrap();
    let request = harness.create_request("paused-create", 1, ParticipantKind::Worker);

    let error = harness.service().create_batch(request).unwrap_err();

    assert_eq!(error.code(), ErrorCode::TaskPaused);
    assert!(error.retryable_flag());
    assert_eq!(count(&harness.store, "agent_invitations"), 0);
    let workspace_artifacts = harness.artifacts.join(harness.workspace_id.to_string());
    assert!(workspace_artifacts.is_dir());
    assert_eq!(fs::read_dir(workspace_artifacts).unwrap().count(), 0);
    assert_eq!(completion_revision(&harness), 0);
}

fn invite_path(manifest: &sessionbridge::app::invite::InvitationManifest) -> PathBuf {
    match &manifest.secret_delivery {
        PublicSecretDelivery::File { invite_path } => invite_path.clone(),
    }
}

fn count(store: &SqliteStore, table: &str) -> i64 {
    assert!(matches!(
        table,
        "agent_invitations" | "events" | "idempotency_records"
    ));
    store
        .connection()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn completion_revision(harness: &Harness) -> i64 {
    harness
        .store
        .connection()
        .query_row(
            "SELECT completion_revision FROM tasks WHERE id = ?1",
            [harness.task_id.to_string()],
            |row| row.get(0),
        )
        .unwrap()
}
