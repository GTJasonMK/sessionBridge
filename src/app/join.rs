use std::{
    fs,
    path::{Path, PathBuf},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::{
    Result,
    app::{
        events::{self, EventActor, EventEntity},
        idempotency::{self, IdempotencyKey},
    },
    clock::{Clock, unix_micros},
    domain::{ParticipantId, ParticipantKind, SessionContext, SessionId, TaskId, TaskPhase},
    error::{ErrorCode, SessionBridgeError},
    git::{GitRepository, WorktreeMetadata},
    invitation::{InviteSecret, InviteTokenHash, write_json_private},
    storage::SqliteStore,
    workspace::{LocatedWorkspace, PROTOCOL_VERSION, atomic_write_json_private, read_json_private},
};

const JOIN_RESERVATION_LEASE: Duration = Duration::minutes(2);
const SESSION_TOKEN_PREFIX: &str = "sb_session_";

#[derive(Debug, Clone)]
pub struct JoinRequest {
    pub invite_file: PathBuf,
    pub provision_worktree: bool,
    pub worktree: Option<PathBuf>,
    pub declared_capabilities: Vec<String>,
    pub label: Option<String>,
    pub client_name: Option<String>,
    pub model: Option<String>,
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NextAction {
    pub cwd: PathBuf,
    pub argv: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JoinResult {
    pub session_id: SessionId,
    pub participant_id: ParticipantId,
    pub participant_kind: ParticipantKind,
    pub completion_epoch: i64,
    pub context_path: PathBuf,
    pub worktree_path: PathBuf,
    pub task_phase: TaskPhase,
    pub protocol_version: String,
    pub prompt_template_version: String,
    pub event_cursor: i64,
    pub next_actions: Vec<NextAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JoinAttemptMarker {
    schema_version: u32,
    request_id: String,
    invite_id: String,
    task_id: String,
    repository_instance_id: String,
    session_id: SessionId,
    participant_id: ParticipantId,
    provision_worktree: bool,
    worktree_created: bool,
    worktree_path: PathBuf,
    branch: Option<String>,
    temporary_context_path: PathBuf,
    context_path: PathBuf,
    created_at_micros: i64,
}

#[derive(Debug, Clone, Serialize)]
struct JoinPayload<'a> {
    invite_id: &'a str,
    provision_worktree: bool,
    worktree: &'a Option<PathBuf>,
    declared_capabilities: &'a [String],
    label: &'a Option<String>,
    client_name: &'a Option<String>,
    model: &'a Option<String>,
}

#[derive(Debug, Clone)]
struct ReservedInvitation {
    task_id: TaskId,
    completion_epoch: i64,
    participant_kind: ParticipantKind,
    target_branch: Option<String>,
    prompt_template_version: i64,
}

enum ReservationOutcome {
    Reserved(ReservedInvitation),
    Expired,
}

#[derive(Debug, Clone)]
struct SessionSecret {
    value: String,
    hash: [u8; 32],
}

pub struct JoinService<C> {
    clock: C,
}

impl<C: Clock> JoinService<C> {
    pub fn new(clock: C) -> Self {
        Self { clock }
    }

    pub fn join(
        &self,
        store: &mut SqliteStore,
        workspace: &LocatedWorkspace,
        mut request: JoinRequest,
    ) -> Result<JoinResult> {
        normalize_capabilities(&mut request.declared_capabilities)?;
        normalize_optional_label("nickname", &mut request.label, 32)?;
        normalize_optional_label("client name", &mut request.client_name, 128)?;
        normalize_optional_label("model", &mut request.model, 128)?;
        if request.provision_worktree && request.worktree.is_some() {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "automatic provisioning and explicit worktree registration are mutually exclusive",
            ));
        }
        request.worktree = request
            .worktree
            .map(|path| {
                fs::canonicalize(&path).map_err(|source| SessionBridgeError::Io { path, source })
            })
            .transpose()?;

        let invite_file = canonical_invite_file(&request.invite_file)?;
        let secret = read_invite_secret(&invite_file)?;
        validate_secret_versions(&secret)?;
        let invite_id = parse_uuid(&secret.invite_id, "invitation ID")?;
        let task_id = parse_task_id(&secret.task_id)?;
        let request_id = normalized_request_id(request.request_id.as_deref())?;
        let now = self.clock.now();
        let marker_path = workspace
            .paths
            .join_attempts_dir
            .join(format!("{request_id}.json"));
        let mut marker = load_or_create_marker(
            &marker_path,
            &request_id,
            &secret,
            workspace,
            request.provision_worktree,
            request.worktree.as_deref(),
            now,
        )?;

        let payload = JoinPayload {
            invite_id: &secret.invite_id,
            provision_worktree: request.provision_worktree,
            worktree: &request.worktree,
            declared_capabilities: &request.declared_capabilities,
            label: &request.label,
            client_name: &request.client_name,
            model: &request.model,
        };
        let payload_hash = idempotency::hash_payload(&payload)?;
        let idempotency_key = IdempotencyKey {
            actor_type: "invitation",
            actor_id: &secret.invite_id,
            request_id: &request_id,
        };

        let reserved = {
            let transaction = store.begin_immediate()?;
            if let Some(stored) =
                idempotency::replay::<JoinResult>(&transaction, idempotency_key, &payload_hash)?
            {
                transaction.commit()?;
                return finish_replayed_join(stored.value, &marker, &marker_path, &invite_file);
            }
            let outcome = reserve_invitation(
                &transaction,
                workspace,
                &secret,
                invite_id,
                task_id,
                &request_id,
                now,
            );
            match outcome {
                Ok(ReservationOutcome::Reserved(reserved)) => {
                    transaction.commit()?;
                    reserved
                }
                Ok(ReservationOutcome::Expired) => {
                    transaction.commit()?;
                    let _ = fs::remove_file(&marker_path);
                    return Err(SessionBridgeError::domain(
                        ErrorCode::InviteExpired,
                        "invitation has expired",
                    ));
                }
                Err(error) => {
                    drop(transaction);
                    let _ = fs::remove_file(&marker_path);
                    return Err(error);
                }
            }
        };

        let worktree = match prepare_worktree(workspace, &reserved, &mut marker) {
            Ok(worktree) => {
                if let Err(error) = atomic_write_json_private(&marker_path, &marker) {
                    let _ = release_reservation(store, &secret.invite_id, &request_id, now);
                    let _ = cleanup_attempt(workspace, &marker, &marker_path);
                    return Err(error);
                }
                worktree
            }
            Err(error) => {
                let _ = release_reservation(store, &secret.invite_id, &request_id, now);
                let _ = cleanup_attempt(workspace, &marker, &marker_path);
                return Err(error);
            }
        };

        let (context, session_secret) = match prepare_context(workspace, &marker, reserved.task_id)
        {
            Ok(value) => value,
            Err(error) => {
                let _ = release_reservation(store, &secret.invite_id, &request_id, now);
                let _ = cleanup_attempt(workspace, &marker, &marker_path);
                return Err(error);
            }
        };

        let result = match finalize_join(
            store,
            workspace,
            &secret,
            &request,
            &request_id,
            &payload_hash,
            &marker,
            &reserved,
            &worktree,
            &session_secret,
            now,
        ) {
            Ok(result) => result,
            Err(error) => {
                return Err(join_recovery_required(
                    error,
                    &marker_path,
                    &invite_file,
                    &request_id,
                ));
            }
        };

        finalize_context_file(&marker, &context).map_err(|error| {
            join_recovery_required(error, &marker_path, &invite_file, &request_id)
        })?;
        let mut result = result;
        if let Some(warning) = cleanup_warning(
            &invite_file,
            "invitation was consumed but secret cleanup failed",
        ) {
            result.warnings.push(warning);
        }
        if let Some(warning) = cleanup_warning(
            &marker_path,
            "join succeeded but attempt marker cleanup failed",
        ) {
            result.warnings.push(warning);
        }
        Ok(result)
    }
}

#[allow(clippy::too_many_arguments)]
fn reserve_invitation(
    transaction: &Transaction<'_>,
    workspace: &LocatedWorkspace,
    secret: &InviteSecret,
    invite_id: Uuid,
    task_id: TaskId,
    request_id: &str,
    now: OffsetDateTime,
) -> Result<ReservationOutcome> {
    #[allow(clippy::type_complexity)]
    let row: Option<(
        String,
        i64,
        String,
        String,
        i64,
        Vec<u8>,
        i64,
        i64,
        i64,
        Option<String>,
        Option<i64>,
        String,
        i64,
        Option<String>,
        String,
    )> = transaction
        .query_row(
            "SELECT i.task_id, i.completion_epoch, i.participant_kind, i.state,
                    i.blocks_completion, i.token_hash, i.expires_at,
                    i.protocol_version, i.prompt_template_version,
                    i.reserved_by_request_id, i.reservation_expires_at,
                    t.phase, t.completion_epoch, t.target_branch, t.workspace_id
             FROM agent_invitations i
             JOIN tasks t ON t.id = i.task_id
             WHERE i.id = ?1",
            [invite_id.to_string()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                    row.get(12)?,
                    row.get(13)?,
                    row.get(14)?,
                ))
            },
        )
        .optional()?;
    let Some((
        stored_task_id,
        invite_epoch,
        participant_kind,
        state,
        blocks_completion,
        stored_hash,
        expires_at,
        protocol_version,
        prompt_template_version,
        reserved_by,
        reservation_expires_at,
        phase,
        task_epoch,
        target_branch,
        workspace_id,
    )) = row
    else {
        return Err(SessionBridgeError::domain(
            ErrorCode::InviteNotFound,
            format!("invitation {} was not found", secret.invite_id),
        ));
    };

    if stored_task_id != task_id.to_string()
        || workspace_id != workspace.config.project_id.to_string()
    {
        return Err(SessionBridgeError::domain(
            ErrorCode::InviteNotFound,
            "invitation does not belong to this workspace and task",
        ));
    }
    verify_invite_hash(&stored_hash, &secret.token)?;
    if protocol_version != i64::from(PROTOCOL_VERSION)
        || protocol_version.to_string() != secret.protocol_version
    {
        return Err(SessionBridgeError::domain(
            ErrorCode::ProtocolVersionMismatch,
            "invitation protocol version does not match this CLI",
        ));
    }
    if prompt_template_version.to_string() != secret.prompt_template_version {
        return Err(SessionBridgeError::domain(
            ErrorCode::PromptTemplateUnsupported,
            "invitation prompt template version does not match its secret",
        ));
    }
    if invite_epoch != task_epoch
        || u64::try_from(invite_epoch).ok() != Some(secret.completion_epoch)
    {
        return Err(SessionBridgeError::domain(
            ErrorCode::CompletionEpochChanged,
            "invitation belongs to a different task completion epoch",
        ));
    }

    let phase = parse_task_phase(&phase)?;
    match phase {
        TaskPhase::Active => {}
        TaskPhase::Paused => {
            return Err(SessionBridgeError::retryable(
                ErrorCode::TaskPaused,
                "task is paused; resume it before joining",
            ));
        }
        _ => {
            return Err(SessionBridgeError::domain(
                ErrorCode::TaskNotActive,
                format!("cannot join task while it is {phase}"),
            ));
        }
    }

    let now_micros = unix_micros(now);
    if expires_at <= now_micros || state == "expired" {
        if matches!(state.as_str(), "pending" | "reserved") {
            transaction.execute(
                "UPDATE agent_invitations
                 SET state = 'expired', reserved_by_request_id = NULL,
                     reservation_expires_at = NULL, updated_at = ?2
                 WHERE id = ?1 AND state IN ('pending', 'reserved')",
                params![secret.invite_id, now_micros],
            )?;
            transaction.execute(
                "UPDATE tasks SET completion_revision = completion_revision + 1,
                    updated_at = ?2 WHERE id = ?1",
                params![stored_task_id, now_micros],
            )?;
            events::append(
                transaction,
                workspace.config.project_id,
                Some(&stored_task_id),
                "invitation.expired",
                EventActor {
                    kind: "system",
                    id: None,
                },
                EventEntity {
                    kind: "invitation",
                    id: Some(&secret.invite_id),
                },
                &serde_json::json!({ "invite_id": secret.invite_id }),
                now,
            )?;
        }
        return Ok(ReservationOutcome::Expired);
    }

    match state.as_str() {
        "pending" => {
            let changed = transaction.execute(
                "UPDATE agent_invitations
                 SET state = 'reserved', reserved_by_request_id = ?2,
                     reservation_expires_at = ?3, updated_at = ?4
                 WHERE id = ?1 AND state = 'pending'",
                params![
                    secret.invite_id,
                    request_id,
                    unix_micros(now + JOIN_RESERVATION_LEASE),
                    now_micros,
                ],
            )?;
            if changed != 1 {
                return Err(SessionBridgeError::retryable(
                    ErrorCode::InviteReserved,
                    "invitation was concurrently reserved",
                ));
            }
            events::append(
                transaction,
                workspace.config.project_id,
                Some(&stored_task_id),
                "invitation.reserved",
                EventActor {
                    kind: "invitation",
                    id: Some(&secret.invite_id),
                },
                EventEntity {
                    kind: "invitation",
                    id: Some(&secret.invite_id),
                },
                &serde_json::json!({ "request_id": request_id }),
                now,
            )?;
        }
        "reserved" if reserved_by.as_deref() == Some(request_id) => {
            transaction.execute(
                "UPDATE agent_invitations SET reservation_expires_at = ?2, updated_at = ?3
                 WHERE id = ?1 AND state = 'reserved' AND reserved_by_request_id = ?4",
                params![
                    secret.invite_id,
                    unix_micros(now + JOIN_RESERVATION_LEASE),
                    now_micros,
                    request_id,
                ],
            )?;
        }
        "reserved" => {
            let expired_reservation =
                reservation_expires_at.is_some_and(|value| value <= now_micros);
            let message = if expired_reservation {
                "invitation has an expired reservation that requires explicit recovery"
            } else {
                "invitation is reserved by another join attempt"
            };
            return Err(
                SessionBridgeError::retryable(ErrorCode::InviteReserved, message).with_details(
                    serde_json::json!({
                        "invite_id": secret.invite_id,
                        "reservation_recovery_required": expired_reservation,
                    }),
                ),
            );
        }
        "prepared" => {
            return Err(SessionBridgeError::domain(
                ErrorCode::InviteNotSendable,
                "invitation batch has not been finalized",
            ));
        }
        "consumed" => {
            return Err(SessionBridgeError::domain(
                ErrorCode::InviteConsumed,
                "invitation has already been consumed",
            ));
        }
        "revoked" | "abandoned" => {
            return Err(SessionBridgeError::domain(
                ErrorCode::InviteRevoked,
                format!("invitation is {state}"),
            ));
        }
        other => {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                format!("invitation has unknown state '{other}'"),
            ));
        }
    }

    let participant_kind = parse_participant_kind(&participant_kind)?;
    if (participant_kind == ParticipantKind::Worker) != (blocks_completion == 1) {
        return Err(SessionBridgeError::domain(
            ErrorCode::Database,
            "invitation completion policy is internally inconsistent",
        ));
    }
    Ok(ReservationOutcome::Reserved(ReservedInvitation {
        task_id,
        completion_epoch: invite_epoch,
        participant_kind,
        target_branch,
        prompt_template_version,
    }))
}

fn prepare_worktree(
    workspace: &LocatedWorkspace,
    reserved: &ReservedInvitation,
    marker: &mut JoinAttemptMarker,
) -> Result<WorktreeMetadata> {
    if marker.provision_worktree {
        if workspace.repository.git_worktree_root.is_none() {
            return Err(SessionBridgeError::domain(
                ErrorCode::WorktreeConflict,
                "automatic worktree provisioning requires a Git repository",
            ));
        }
        let repository = GitRepository::open(&workspace.paths.root)?;
        if marker.worktree_created {
            let child = GitRepository::open(&marker.worktree_path)?;
            let branch = child.current_branch()?.ok_or_else(|| {
                SessionBridgeError::domain(
                    ErrorCode::JoinRecoveryRequired,
                    "provisioned join worktree is detached",
                )
            })?;
            if marker.branch.as_deref() != Some(branch.as_str()) {
                return Err(SessionBridgeError::domain(
                    ErrorCode::JoinRecoveryRequired,
                    "provisioned join worktree branch does not match its marker",
                ));
            }
            let head = child.head_commit()?;
            return Ok(WorktreeMetadata {
                path: marker.worktree_path.clone(),
                branch,
                base_commit: head.clone(),
                head_commit: head,
            });
        }

        let task_short = short_id(reserved.task_id.0);
        let session_short = short_id(marker.session_id.0);
        let branch = format!("sessionbridge/{task_short}/{session_short}");
        let metadata = repository.provision_worktree(
            &marker.worktree_path,
            &branch,
            reserved.target_branch.as_deref(),
        )?;
        marker.branch = Some(branch);
        marker.worktree_created = true;
        Ok(metadata)
    } else {
        let explicit_worktree = marker.worktree_path != workspace.paths.root;
        let repository = match workspace.repository.kind {
            crate::workspace::RepositoryKind::Git => {
                Some(GitRepository::open(&marker.worktree_path)?)
            }
            crate::workspace::RepositoryKind::Directory => None,
        };
        if explicit_worktree {
            let repository = repository.as_ref().ok_or_else(|| {
                SessionBridgeError::domain(
                    ErrorCode::WorktreeConflict,
                    "explicit worktree must be a Git worktree of this Workspace repository",
                )
            })?;
            let expected_common_dir =
                workspace
                    .repository
                    .git_common_dir
                    .as_ref()
                    .ok_or_else(|| {
                        SessionBridgeError::domain(
                            ErrorCode::WorktreeConflict,
                            "explicit worktree registration requires a Git Workspace",
                        )
                    })?;
            if repository.common_dir()? != *expected_common_dir {
                return Err(SessionBridgeError::domain(
                    ErrorCode::WorktreeConflict,
                    "explicit worktree belongs to a different Git repository",
                ));
            }
            repository.require_clean()?;
        }
        let head = repository
            .as_ref()
            .map(GitRepository::head_commit)
            .transpose()?
            .unwrap_or_default();
        let branch = repository
            .as_ref()
            .map(GitRepository::current_branch)
            .transpose()?
            .flatten()
            .unwrap_or_else(|| "(non-git-or-detached)".to_owned());
        Ok(WorktreeMetadata {
            path: marker.worktree_path.clone(),
            branch,
            base_commit: head.clone(),
            head_commit: head,
        })
    }
}

fn prepare_context(
    workspace: &LocatedWorkspace,
    marker: &JoinAttemptMarker,
    task_id: TaskId,
) -> Result<(SessionContext, SessionSecret)> {
    if marker.temporary_context_path.exists() {
        let context: SessionContext = read_json_private(&marker.temporary_context_path)?;
        validate_recovery_context(&context, workspace, marker, task_id)?;
        let hash = Sha256::digest(context.token.as_bytes()).into();
        return Ok((
            context.clone(),
            SessionSecret {
                value: context.token,
                hash,
            },
        ));
    }

    let secret = generate_session_secret()?;
    let context = SessionContext {
        schema_version: 1,
        protocol_version: PROTOCOL_VERSION,
        workspace_id: workspace.config.project_id,
        repository_instance_id: workspace.paths.repository_instance_id.clone(),
        task_id,
        session_id: marker.session_id,
        token: secret.value.clone(),
        database_path: workspace.paths.database_path.display().to_string(),
    };
    write_json_private(&marker.temporary_context_path, &context)?;
    Ok((context, secret))
}

#[allow(clippy::too_many_arguments)]
fn finalize_join(
    store: &mut SqliteStore,
    workspace: &LocatedWorkspace,
    secret: &InviteSecret,
    request: &JoinRequest,
    request_id: &str,
    payload_hash: &[u8; 32],
    marker: &JoinAttemptMarker,
    reserved: &ReservedInvitation,
    worktree: &WorktreeMetadata,
    session_secret: &SessionSecret,
    now: OffsetDateTime,
) -> Result<JoinResult> {
    let transaction = store.begin_immediate()?;
    let key = IdempotencyKey {
        actor_type: "invitation",
        actor_id: &secret.invite_id,
        request_id,
    };
    if let Some(stored) = idempotency::replay::<JoinResult>(&transaction, key, payload_hash)? {
        transaction.commit()?;
        return Ok(stored.value);
    }

    let state: Option<(String, Option<String>, i64, String)> = transaction
        .query_row(
            "SELECT i.state, i.reserved_by_request_id, t.completion_epoch, t.phase
             FROM agent_invitations i JOIN tasks t ON t.id = i.task_id
             WHERE i.id = ?1 AND i.task_id = ?2",
            params![secret.invite_id, reserved.task_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((state, reserved_by, task_epoch, phase)) = state else {
        return Err(SessionBridgeError::domain(
            ErrorCode::InviteNotFound,
            "reserved invitation disappeared before join finalize",
        ));
    };
    if state != "reserved" || reserved_by.as_deref() != Some(request_id) {
        return Err(SessionBridgeError::domain(
            ErrorCode::JoinRecoveryRequired,
            "join attempt no longer owns the invitation reservation",
        ));
    }
    if task_epoch != reserved.completion_epoch {
        return Err(SessionBridgeError::domain(
            ErrorCode::CompletionEpochChanged,
            "task completion epoch changed during join",
        ));
    }
    if parse_task_phase(&phase)? != TaskPhase::Active {
        return Err(SessionBridgeError::domain(
            ErrorCode::TaskNotActive,
            format!("task became {phase} during join"),
        ));
    }

    let worktree_path = worktree.path.display().to_string();
    let conflict: Option<String> = transaction
        .query_row(
            "SELECT id FROM agent_sessions
             WHERE worktree_path = ?1 AND ended_at IS NULL AND id <> ?2 LIMIT 1",
            params![worktree_path, marker.session_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(session_id) = conflict {
        return Err(SessionBridgeError::domain(
            ErrorCode::WorktreeConflict,
            "another active Session is already registered for this worktree",
        )
        .with_details(serde_json::json!({
            "worktree_path": worktree.path,
            "conflicting_session_id": session_id,
        })));
    }

    let now_micros = unix_micros(now);
    let label = request
        .label
        .clone()
        .unwrap_or_else(|| format!("agent-{}", short_id(marker.session_id.0)));
    transaction.execute(
        "INSERT INTO agent_sessions (
            id, task_id, token_hash, label, client_name, model,
            declared_capabilities_json, capacity_json, worktree_path,
            branch, base_commit, head_commit, target_branch, protocol_version,
            last_seen_at, created_at, updated_at, executor_identity,
            executor_identity_source
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, '{}', ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14, ?14, ?15, 'invitation')",
        params![
            marker.session_id.to_string(),
            reserved.task_id.to_string(),
            session_secret.hash.as_slice(),
            &label,
            request.client_name,
            request.model,
            serde_json::to_string(&request.declared_capabilities)?,
            worktree_path,
            worktree.branch,
            worktree.base_commit,
            worktree.head_commit,
            reserved.target_branch,
            i64::from(PROTOCOL_VERSION),
            now_micros,
            secret.invite_id,
        ],
    )?;
    transaction.execute(
        "INSERT INTO task_participants (
            id, task_id, session_id, invitation_id, kind, completion_epoch,
            state, version, joined_at, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'active', 1, ?7, ?7, ?7)",
        params![
            marker.participant_id.to_string(),
            reserved.task_id.to_string(),
            marker.session_id.to_string(),
            secret.invite_id,
            reserved.participant_kind.as_str(),
            reserved.completion_epoch,
            now_micros,
        ],
    )?;
    let consumed = transaction.execute(
        "UPDATE agent_invitations
         SET state = 'consumed', consumed_at = ?3, consumed_by_session_id = ?4,
             reserved_by_request_id = NULL, reservation_expires_at = NULL, updated_at = ?3
         WHERE id = ?1 AND state = 'reserved' AND reserved_by_request_id = ?2",
        params![
            secret.invite_id,
            request_id,
            now_micros,
            marker.session_id.to_string(),
        ],
    )?;
    if consumed != 1 {
        return Err(SessionBridgeError::domain(
            ErrorCode::InviteReserved,
            "invitation reservation changed during join finalize",
        ));
    }
    transaction.execute(
        "UPDATE tasks SET completion_revision = completion_revision + 1,
            updated_at = ?2 WHERE id = ?1",
        params![reserved.task_id.to_string(), now_micros],
    )?;
    events::append(
        &transaction,
        workspace.config.project_id,
        Some(&reserved.task_id.to_string()),
        "invitation.consumed",
        EventActor {
            kind: "invitation",
            id: Some(&secret.invite_id),
        },
        EventEntity {
            kind: "invitation",
            id: Some(&secret.invite_id),
        },
        &serde_json::json!({ "session_id": marker.session_id }),
        now,
    )?;
    let event_cursor = events::append(
        &transaction,
        workspace.config.project_id,
        Some(&reserved.task_id.to_string()),
        "agent.joined",
        EventActor {
            kind: "session",
            id: Some(&marker.session_id.to_string()),
        },
        EventEntity {
            kind: "participant",
            id: Some(&marker.participant_id.to_string()),
        },
        &serde_json::json!({
            "session_id": marker.session_id,
            "participant_id": marker.participant_id,
            "participant_kind": reserved.participant_kind,
            "nickname": label,
        }),
        now,
    )?;

    let result = JoinResult {
        session_id: marker.session_id,
        participant_id: marker.participant_id,
        participant_kind: reserved.participant_kind,
        completion_epoch: reserved.completion_epoch,
        context_path: marker.context_path.clone(),
        worktree_path: marker.worktree_path.clone(),
        task_phase: TaskPhase::Active,
        protocol_version: PROTOCOL_VERSION.to_string(),
        prompt_template_version: reserved.prompt_template_version.to_string(),
        event_cursor,
        next_actions: next_actions(&marker.context_path, &marker.worktree_path),
        warnings: Vec::new(),
    };
    idempotency::record(
        &transaction,
        key,
        Some(&reserved.task_id.to_string()),
        payload_hash,
        &result,
        Some(event_cursor),
        now,
    )?;
    transaction.commit()?;
    Ok(result)
}

fn load_or_create_marker(
    marker_path: &Path,
    request_id: &str,
    secret: &InviteSecret,
    workspace: &LocatedWorkspace,
    provision_worktree: bool,
    requested_worktree: Option<&Path>,
    now: OffsetDateTime,
) -> Result<JoinAttemptMarker> {
    if marker_path.exists() {
        let marker: JoinAttemptMarker = read_json_private(marker_path)?;
        if marker.schema_version != 1
            || marker.request_id != request_id
            || marker.invite_id != secret.invite_id
            || marker.task_id != secret.task_id
            || marker.repository_instance_id != workspace.paths.repository_instance_id
            || marker.provision_worktree != provision_worktree
            || requested_worktree.is_some_and(|path| marker.worktree_path != path)
            || (!provision_worktree
                && requested_worktree.is_none()
                && marker.worktree_path != workspace.paths.root)
        {
            return Err(SessionBridgeError::domain(
                ErrorCode::JoinRecoveryRequired,
                "join attempt marker does not match this request",
            )
            .with_details(serde_json::json!({ "join_attempt_marker": marker_path })));
        }
        return Ok(marker);
    }

    let session_id = SessionId::new();
    let participant_id = ParticipantId::new();
    let worktree_path = if provision_worktree {
        workspace.paths.worktrees_dir.join(session_id.to_string())
    } else {
        requested_worktree
            .map(Path::to_path_buf)
            .unwrap_or_else(|| workspace.paths.root.clone())
    };
    let context_path = workspace
        .paths
        .contexts_dir
        .join(format!("{session_id}.json"));
    let temporary_context_path = workspace
        .paths
        .contexts_dir
        .join(format!(".{session_id}.{request_id}.context.tmp"));
    let marker = JoinAttemptMarker {
        schema_version: 1,
        request_id: request_id.to_owned(),
        invite_id: secret.invite_id.clone(),
        task_id: secret.task_id.clone(),
        repository_instance_id: workspace.paths.repository_instance_id.clone(),
        session_id,
        participant_id,
        provision_worktree,
        worktree_created: false,
        worktree_path,
        branch: None,
        temporary_context_path,
        context_path,
        created_at_micros: unix_micros(now),
    };
    write_json_private(marker_path, &marker)?;
    Ok(marker)
}

fn finish_replayed_join(
    result: JoinResult,
    marker: &JoinAttemptMarker,
    marker_path: &Path,
    invite_file: &Path,
) -> Result<JoinResult> {
    if result.context_path.exists() {
        let _ = fs::remove_file(&marker.temporary_context_path);
    } else {
        let parent = result.context_path.parent().ok_or_else(|| {
            SessionBridgeError::domain(
                ErrorCode::JoinRecoveryRequired,
                "stored join context path has no parent",
            )
        })?;
        let temporary = parent.join(format!(
            ".{}.{}.context.tmp",
            result.session_id, marker.request_id
        ));
        if temporary.exists() {
            let context: SessionContext = read_json_private(&temporary)?;
            let recovery_marker = JoinAttemptMarker {
                session_id: result.session_id,
                participant_id: result.participant_id,
                temporary_context_path: temporary,
                context_path: result.context_path.clone(),
                worktree_path: result.worktree_path.clone(),
                ..marker.clone()
            };
            finalize_context_file(&recovery_marker, &context)?;
        } else {
            return Err(SessionBridgeError::domain(
                ErrorCode::JoinRecoveryRequired,
                "join database commit exists but neither temporary nor final context is available",
            )
            .with_details(serde_json::json!({ "join_attempt_marker": marker_path })));
        }
    }
    if !result.context_path.exists() {
        return Err(SessionBridgeError::domain(
            ErrorCode::JoinRecoveryRequired,
            "join context recovery did not produce the final context file",
        )
        .with_details(serde_json::json!({ "join_attempt_marker": marker_path })));
    }
    let _ = fs::remove_file(invite_file);
    let _ = fs::remove_file(marker_path);
    Ok(result)
}

fn finalize_context_file(marker: &JoinAttemptMarker, context: &SessionContext) -> Result<()> {
    if marker.context_path.exists() {
        let existing: SessionContext = read_json_private(&marker.context_path)?;
        if existing.session_id != context.session_id || existing.token != context.token {
            return Err(SessionBridgeError::domain(
                ErrorCode::JoinRecoveryRequired,
                "existing final context does not match the join attempt",
            ));
        }
        if marker.temporary_context_path.exists() {
            fs::remove_file(&marker.temporary_context_path).map_err(|source| {
                SessionBridgeError::Io {
                    path: marker.temporary_context_path.clone(),
                    source,
                }
            })?;
        }
        return Ok(());
    }
    fs::rename(&marker.temporary_context_path, &marker.context_path).map_err(|source| {
        SessionBridgeError::Io {
            path: marker.context_path.clone(),
            source,
        }
    })?;
    sync_parent_directory(&marker.context_path)?;
    Ok(())
}

fn release_reservation(
    store: &mut SqliteStore,
    invite_id: &str,
    request_id: &str,
    now: OffsetDateTime,
) -> Result<()> {
    let transaction = store.begin_immediate()?;
    transaction.execute(
        "UPDATE agent_invitations
         SET state = CASE WHEN expires_at <= ?3 THEN 'expired' ELSE 'pending' END,
             reserved_by_request_id = NULL, reservation_expires_at = NULL, updated_at = ?3
         WHERE id = ?1 AND state = 'reserved' AND reserved_by_request_id = ?2",
        params![invite_id, request_id, unix_micros(now)],
    )?;
    transaction.commit()?;
    Ok(())
}

fn cleanup_attempt(
    workspace: &LocatedWorkspace,
    marker: &JoinAttemptMarker,
    marker_path: &Path,
) -> Result<()> {
    if marker.worktree_created {
        let repository = GitRepository::open(&workspace.paths.root)?;
        repository.remove_worktree(&marker.worktree_path)?;
        if let Some(branch) = marker.branch.as_deref() {
            repository.delete_branch(branch)?;
        }
    }
    for path in [&marker.temporary_context_path, marker_path] {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(SessionBridgeError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        }
    }
    Ok(())
}

fn cleanup_warning(path: &Path, message: &str) -> Option<String> {
    match fs::remove_file(path) {
        Ok(()) => None,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => Some(format!("{message} at {}: {error}", path.display())),
    }
}

fn validate_recovery_context(
    context: &SessionContext,
    workspace: &LocatedWorkspace,
    marker: &JoinAttemptMarker,
    task_id: TaskId,
) -> Result<()> {
    if context.schema_version != 1
        || context.protocol_version != PROTOCOL_VERSION
        || context.workspace_id != workspace.config.project_id
        || context.repository_instance_id != workspace.paths.repository_instance_id
        || context.task_id != task_id
        || context.session_id != marker.session_id
        || context.token.is_empty()
    {
        return Err(SessionBridgeError::domain(
            ErrorCode::JoinRecoveryRequired,
            "temporary session context does not match the join attempt",
        ));
    }
    Ok(())
}

fn next_actions(context_path: &Path, worktree_path: &Path) -> Vec<NextAction> {
    ["whoami", "sync"]
        .into_iter()
        .map(|command| NextAction {
            cwd: worktree_path.to_path_buf(),
            argv: vec![
                "sessionbridge".to_owned(),
                "agent".to_owned(),
                command.to_owned(),
                "--context".to_owned(),
                context_path.display().to_string(),
                "--format".to_owned(),
                "json".to_owned(),
                "--non-interactive".to_owned(),
            ],
        })
        .collect()
}

fn canonical_invite_file(path: &Path) -> Result<PathBuf> {
    match fs::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            Err(SessionBridgeError::domain(
                ErrorCode::InviteSecretMissing,
                format!("invitation secret file is missing: {}", path.display()),
            ))
        }
        Err(source) => Err(SessionBridgeError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn read_invite_secret(path: &Path) -> Result<InviteSecret> {
    let bytes = fs::read(path).map_err(|source| SessionBridgeError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    InviteSecret::from_json_slice(&bytes).map_err(Into::into)
}

fn validate_secret_versions(secret: &InviteSecret) -> Result<()> {
    if secret.protocol_version != PROTOCOL_VERSION.to_string() {
        return Err(SessionBridgeError::domain(
            ErrorCode::ProtocolVersionMismatch,
            format!(
                "invitation protocol {} does not match CLI protocol {}",
                secret.protocol_version, PROTOCOL_VERSION
            ),
        ));
    }
    Ok(())
}

fn verify_invite_hash(stored: &[u8], token: &crate::invitation::InviteToken) -> Result<()> {
    let bytes: [u8; 32] = stored.try_into().map_err(|_| {
        SessionBridgeError::domain(
            ErrorCode::Database,
            "stored invitation token hash has an invalid length",
        )
    })?;
    if !InviteTokenHash::from_bytes(bytes).verify(token) {
        return Err(SessionBridgeError::domain(
            ErrorCode::InviteNotFound,
            "invitation secret does not match the stored invitation",
        ));
    }
    Ok(())
}

fn generate_session_secret() -> Result<SessionSecret> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| {
        SessionBridgeError::domain(
            ErrorCode::Internal,
            format!("secure random generation failed: {error}"),
        )
    })?;
    let value = format!("{SESSION_TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes));
    let hash = Sha256::digest(value.as_bytes()).into();
    Ok(SessionSecret { value, hash })
}

fn normalized_request_id(value: Option<&str>) -> Result<String> {
    match value {
        Some(value) => Uuid::parse_str(value)
            .map(|value| value.to_string())
            .map_err(|error| {
                SessionBridgeError::domain(
                    ErrorCode::InvalidInput,
                    format!("join request ID must be a UUID: {error}"),
                )
            }),
        None => Ok(Uuid::now_v7().to_string()),
    }
}

fn normalize_capabilities(values: &mut Vec<String>) -> Result<()> {
    for value in values.iter_mut() {
        *value = value.trim().to_owned();
        if value.is_empty() || value.chars().any(char::is_control) {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "declared capabilities cannot be empty or contain control characters",
            ));
        }
    }
    values.sort();
    values.dedup();
    Ok(())
}

fn normalize_optional_label(
    name: &str,
    value: &mut Option<String>,
    max_characters: usize,
) -> Result<()> {
    if let Some(current) = value {
        *current = current.trim().to_owned();
        if current.is_empty()
            || current.chars().count() > max_characters
            || current.chars().any(char::is_control)
        {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                format!("{name} must contain between 1 and {max_characters} visible characters"),
            ));
        }
    }
    Ok(())
}

fn parse_uuid(value: &str, name: &str) -> Result<Uuid> {
    Uuid::parse_str(value).map_err(|error| {
        SessionBridgeError::domain(ErrorCode::InvalidInput, format!("invalid {name}: {error}"))
    })
}

fn parse_task_id(value: &str) -> Result<TaskId> {
    value.parse().map_err(|error| {
        SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            format!("invalid task ID in invitation: {error}"),
        )
    })
}

fn parse_task_phase(value: &str) -> Result<TaskPhase> {
    value
        .parse()
        .map_err(|error: String| SessionBridgeError::domain(ErrorCode::Database, error))
}

fn parse_participant_kind(value: &str) -> Result<ParticipantKind> {
    value
        .parse()
        .map_err(|error: String| SessionBridgeError::domain(ErrorCode::Database, error))
}

fn short_id(id: Uuid) -> String {
    let value = id.simple().to_string();
    value[value.len() - 12..].to_owned()
}

fn join_recovery_required(
    source: SessionBridgeError,
    marker_path: &Path,
    invite_file: &Path,
    request_id: &str,
) -> SessionBridgeError {
    SessionBridgeError::domain(
        ErrorCode::JoinRecoveryRequired,
        format!("join requires recovery after: {source}"),
    )
    .with_details(serde_json::json!({
        "join_attempt_marker": marker_path,
        "request_id": request_id,
        "retry_argv": [
            "sessionbridge", "agent", "join", "--invite-file",
            invite_file.display().to_string(), "--request-id", request_id
        ],
    }))
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> Result<()> {
    use std::fs::File;

    let parent = path.parent().ok_or_else(|| {
        SessionBridgeError::domain(ErrorCode::InvalidInput, "path has no parent directory")
    })?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| SessionBridgeError::Io {
            path: parent.to_path_buf(),
            source,
        })
}

#[cfg(not(unix))]
fn sync_parent_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod short_id_tests {
    use super::short_id;
    use uuid::Uuid;

    #[test]
    fn short_ids_use_random_tail_instead_of_uuid_v7_timestamp_prefix() {
        let first = Uuid::parse_str("019f4c11-2030-7000-8000-000000000001").unwrap();
        let second = Uuid::parse_str("019f4c11-2030-7000-8000-000000000002").unwrap();

        assert_eq!(short_id(first), "000000000001");
        assert_eq!(short_id(second), "000000000002");
    }
}
