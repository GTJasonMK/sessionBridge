use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

use crate::{
    Result,
    app::{
        completion_service::{CompletionTrigger, TaskCompletionContext, reconcile_in_transaction},
        events::{self, EventActor, EventEntity},
        idempotency::{self, IdempotencyKey},
    },
    clock::{Clock, unix_micros},
    domain::{InvitationId, InvitationState, ParticipantKind, TaskId, TaskPhase, WorkspaceId},
    error::{ErrorCode, SessionBridgeError},
    invitation::{
        BatchStaging, InviteSecret, InviteSecretInput, InviteToken, InviteTokenHash,
        PROMPT_TEMPLATE_VERSION, PromptDelivery, PromptRendererV1, PromptShell, PromptSpec,
        ensure_private_dir, sha256_hex,
    },
    storage::SqliteStore,
    workspace::{PROTOCOL_VERSION, atomic_write_json_private, atomic_write_private},
};

const MARKER_SCHEMA_VERSION: u32 = 1;
const MARKER_FILE_NAME: &str = ".sessionbridge-request.json";
const PREPARE_LEASE: Duration = Duration::minutes(5);
const MAX_BATCH_SIZE: usize = 256;
const IDEMPOTENCY_ACTOR_TYPE: &str = "user";
const PROMPT_TEMPLATE_VERSION_NUMBER: i64 = 1;

#[derive(Debug, Clone)]
pub struct CreateInvitationBatchRequest {
    pub task_id: TaskId,
    pub count: usize,
    pub participant_kind: ParticipantKind,
    pub expires_in: Duration,
    pub capability_limits: Vec<String>,
    pub request_id: String,
    pub shell: PromptShell,
    pub provision_worktree: bool,
}

#[derive(Debug, Clone)]
pub struct ShowInvitationRequest {
    pub invitation_id: InvitationId,
    pub include_prompt: bool,
    pub audit: bool,
}

#[derive(Debug, Clone)]
pub struct RevokeInvitationRequest {
    pub invitation_id: InvitationId,
    pub reason: String,
    pub request_id: String,
}

#[derive(Debug, Clone)]
pub struct RotateInvitationRequest {
    pub invitation_id: InvitationId,
    pub expires_in: Duration,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InvitationBatchManifest {
    pub batch_id: String,
    pub prompts: Vec<InvitationManifest>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InvitationManifest {
    pub invite_id: InvitationId,
    pub task_id: TaskId,
    pub batch_id: String,
    pub batch_index: usize,
    pub batch_count: usize,
    pub status: InvitationState,
    pub participant_kind: ParticipantKind,
    pub blocks_completion: bool,
    pub completion_epoch: i64,
    pub expires_at: String,
    pub prompt_template_version: i64,
    pub protocol_version: i64,
    pub prompt_path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_sha256: Option<String>,
    pub secret_delivery: PublicSecretDelivery,
    pub secret_exposed: bool,
    pub sendable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotates_from_id: Option<InvitationId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PublicSecretDelivery {
    File { invite_path: PathBuf },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RevokeInvitationResult {
    pub invitation: InvitationManifest,
}

pub struct InvitationService<'a, C: Clock> {
    store: &'a mut SqliteStore,
    clock: &'a C,
    workspace_id: WorkspaceId,
    artifact_root: PathBuf,
}

impl<'a, C: Clock> InvitationService<'a, C> {
    pub fn new(
        store: &'a mut SqliteStore,
        clock: &'a C,
        workspace_id: WorkspaceId,
        artifact_root: impl AsRef<Path>,
    ) -> Result<Self> {
        let artifact_root = absolute_path(artifact_root.as_ref())?;
        ensure_private_dir(&artifact_root)?;
        let artifact_root =
            fs::canonicalize(&artifact_root).map_err(|source| SessionBridgeError::Io {
                path: artifact_root.clone(),
                source,
            })?;
        let artifact_root = artifact_root.join(workspace_id.to_string());
        ensure_private_dir(&artifact_root)?;

        Ok(Self {
            store,
            clock,
            workspace_id,
            artifact_root,
        })
    }

    pub fn create_batch(
        &mut self,
        mut request: CreateInvitationBatchRequest,
    ) -> Result<InvitationBatchManifest> {
        validate_count(request.count)?;
        validate_request_id(&request.request_id)?;
        validate_expiry_duration(request.expires_in)?;
        request.capability_limits = normalize_capabilities(&request.capability_limits)?;

        let payload = CreatePayload::new(&request, &self.artifact_root);
        let payload_hash = idempotency::hash_payload(&payload)?;
        if let Some(result) = replay_batch(
            self.store.connection(),
            self.workspace_id,
            &request.request_id,
            &payload_hash,
        )? {
            return Ok(result);
        }

        if let Some(rows) = load_rows_for_prepare_request(
            self.store.connection(),
            self.workspace_id,
            &request.request_id,
        )? {
            return self.resume_prepared(rows, &request.request_id, &payload_hash);
        }

        for _ in 0..3 {
            let now = normalize_timestamp(self.clock.now())?;
            let task = load_task(self.store.connection(), self.workspace_id, request.task_id)?;
            ensure_task_active(&task)?;
            let expires_at = checked_expiry(now, request.expires_in)?;
            let prepare_expires_at = checked_expiry(now, PREPARE_LEASE)?;

            let batch = self.build_batch(BuildBatchSpec {
                task: &task,
                count: request.count,
                participant_kind: request.participant_kind,
                capability_limits: &request.capability_limits,
                request_id: &request.request_id,
                payload_hash: &payload_hash,
                expires_at,
                prepare_expires_at,
                shell: request.shell,
                provision_worktree: request.provision_worktree,
                operation: MarkerOperation::Create,
                rotates_from_id: None,
            })?;

            match insert_prepared_batch(
                self.store,
                self.workspace_id,
                &request.request_id,
                &payload_hash,
                &batch,
                now,
            )? {
                PrepareInsert::Inserted => {
                    let (final_path, marker) = batch.finalize()?;
                    return finalize_prepared_batch(
                        self.store,
                        FinalizeBatchContext {
                            now: self.clock.now(),
                            workspace_id: self.workspace_id,
                            artifact_root: &self.artifact_root,
                            final_path,
                            marker,
                            request_id: &request.request_id,
                            payload_hash: &payload_hash,
                        },
                    );
                }
                PrepareInsert::Existing(rows) => {
                    drop(batch);
                    return self.resume_prepared(rows, &request.request_id, &payload_hash);
                }
                PrepareInsert::Replayed(result) => {
                    drop(batch);
                    return Ok(result);
                }
                PrepareInsert::EpochChanged => {
                    drop(batch);
                }
            }
        }

        Err(SessionBridgeError::retryable(
            ErrorCode::CompletionEpochChanged,
            "task completion epoch changed repeatedly while preparing invitations",
        ))
    }

    pub fn list(&self, task_id: TaskId) -> Result<Vec<InvitationManifest>> {
        let task = load_task(self.store.connection(), self.workspace_id, task_id)?;
        let rows = load_rows_for_task(self.store.connection(), self.workspace_id, task_id)?;
        let now = normalize_timestamp(self.clock.now())?;
        rows.iter()
            .map(|row| manifest_for_row(row, &task, &self.artifact_root, now, false, true))
            .collect()
    }

    pub fn show(&self, request: ShowInvitationRequest) -> Result<InvitationManifest> {
        let row = load_invitation(
            self.store.connection(),
            self.workspace_id,
            request.invitation_id,
        )?;
        let task = TaskSnapshot {
            id: row.task_id,
            phase: row.task_phase,
            completion_epoch: row.task_completion_epoch,
        };
        let now = normalize_timestamp(self.clock.now())?;

        if request.include_prompt && !request.audit {
            ensure_sendable(&row, &task, &self.artifact_root, now)?;
        }

        manifest_for_row(
            &row,
            &task,
            &self.artifact_root,
            now,
            request.include_prompt,
            request.audit,
        )
    }

    pub fn revoke(&mut self, request: RevokeInvitationRequest) -> Result<RevokeInvitationResult> {
        validate_request_id(&request.request_id)?;
        if request.reason.trim().is_empty() {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "invitation revoke reason cannot be empty",
            ));
        }

        let payload = RevokePayload {
            operation: "revoke",
            invitation_id: request.invitation_id,
            reason: request.reason.trim(),
        };
        let payload_hash = idempotency::hash_payload(&payload)?;
        let actor_id = self.workspace_id.to_string();
        let key = IdempotencyKey {
            actor_type: IDEMPOTENCY_ACTOR_TYPE,
            actor_id: &actor_id,
            request_id: &request.request_id,
        };
        if let Some(stored) = idempotency::replay::<RevokeInvitationResult>(
            self.store.connection(),
            key,
            &payload_hash,
        )? {
            return Ok(stored.value);
        }

        let now = normalize_timestamp(self.clock.now())?;
        let clock = self.clock;
        let artifact_root = self.artifact_root.clone();
        let workspace_id = self.workspace_id;
        let transaction = self.store.begin_immediate()?;
        if let Some(stored) =
            idempotency::replay::<RevokeInvitationResult>(&transaction, key, &payload_hash)?
        {
            transaction.commit()?;
            return Ok(stored.value);
        }

        let mut row = load_invitation(&transaction, workspace_id, request.invitation_id)?;
        let task = TaskSnapshot {
            id: row.task_id,
            phase: row.task_phase,
            completion_epoch: row.task_completion_epoch,
        };
        let next_state = match row.state {
            InvitationState::Prepared => InvitationState::Abandoned,
            InvitationState::Pending | InvitationState::Reserved => InvitationState::Revoked,
            InvitationState::Revoked => InvitationState::Revoked,
            InvitationState::Consumed => return Err(invitation_state_error(&row)),
            InvitationState::Expired | InvitationState::Abandoned => {
                return Err(invitation_state_error(&row));
            }
        };

        let state_changed = row.state != next_state;
        if state_changed {
            let changed = transaction.execute(
                "UPDATE agent_invitations
                 SET state = ?1, revoked_at = CASE WHEN ?1 = 'revoked' THEN ?2 ELSE revoked_at END,
                     reserved_by_request_id = NULL, reservation_expires_at = NULL, updated_at = ?2
                 WHERE id = ?3 AND state = ?4",
                params![
                    next_state.as_str(),
                    unix_micros(now),
                    row.id.to_string(),
                    row.state.as_str(),
                ],
            )?;
            if changed != 1 {
                return Err(version_conflict(
                    "invitation changed while it was being revoked",
                ));
            }
            row.state = next_state;
        }
        if state_changed {
            bump_completion_revision(&transaction, row.task_id, now)?;
        }

        let event_sequence = events::append(
            &transaction,
            workspace_id,
            Some(&row.task_id.to_string()),
            "invitation.revoked",
            EventActor {
                kind: IDEMPOTENCY_ACTOR_TYPE,
                id: Some(&actor_id),
            },
            EventEntity {
                kind: "invitation",
                id: Some(&row.id.to_string()),
            },
            &serde_json::json!({
                "invite_id": row.id,
                "batch_id": row.batch_id,
                "state": row.state,
                "reason": request.reason.trim(),
            }),
            now,
        )?;
        let repository_instance_id =
            load_workspace_repository_identity(&transaction, workspace_id)?;
        let reconciliation = reconcile_in_transaction(
            &transaction,
            TaskCompletionContext {
                workspace_id,
                repository_instance_id: &repository_instance_id,
                task_id: row.task_id,
            },
            clock,
            CompletionTrigger {
                actor_type: IDEMPOTENCY_ACTOR_TYPE,
                actor_id: Some(&actor_id),
            },
        )?;

        let invitation = manifest_for_row(&row, &task, &artifact_root, now, false, true)?;
        let result = RevokeInvitationResult { invitation };
        idempotency::record(
            &transaction,
            key,
            Some(&row.task_id.to_string()),
            &payload_hash,
            &result,
            reconciliation.event_sequence.or(Some(event_sequence)),
            now,
        )?;
        transaction.commit()?;

        remove_secret_best_effort(&artifact_root, &row);
        Ok(result)
    }

    pub fn rotate(&mut self, request: RotateInvitationRequest) -> Result<InvitationBatchManifest> {
        validate_request_id(&request.request_id)?;
        validate_expiry_duration(request.expires_in)?;

        let old = load_invitation(
            self.store.connection(),
            self.workspace_id,
            request.invitation_id,
        )?;
        let task = TaskSnapshot {
            id: old.task_id,
            phase: old.task_phase,
            completion_epoch: old.task_completion_epoch,
        };

        let old_marker = load_marker(&self.artifact_root, &old.batch_id)?;
        let shell = parse_shell(&old_marker.prompt_shell)?;
        let provision_worktree = old_marker.provision_worktree;
        let payload = RotatePayload {
            operation: "rotate",
            invitation_id: request.invitation_id,
            expires_in_nanoseconds: request.expires_in.whole_nanoseconds().to_string(),
            artifact_root: path_text(&self.artifact_root)?.to_owned(),
        };
        let payload_hash = idempotency::hash_payload(&payload)?;
        if let Some(result) = replay_batch(
            self.store.connection(),
            self.workspace_id,
            &request.request_id,
            &payload_hash,
        )? {
            return Ok(result);
        }
        if let Some(rows) = load_rows_for_prepare_request(
            self.store.connection(),
            self.workspace_id,
            &request.request_id,
        )? {
            return self.resume_prepared(rows, &request.request_id, &payload_hash);
        }
        ensure_task_active(&task)?;
        ensure_rotatable(&old, normalize_timestamp(self.clock.now())?)?;

        let capabilities: Vec<String> = serde_json::from_str(&old.capability_limits_json)?;
        for _ in 0..3 {
            let now = normalize_timestamp(self.clock.now())?;
            let current_old = load_invitation(
                self.store.connection(),
                self.workspace_id,
                request.invitation_id,
            )?;
            let current_task = TaskSnapshot {
                id: current_old.task_id,
                phase: current_old.task_phase,
                completion_epoch: current_old.task_completion_epoch,
            };
            ensure_task_active(&current_task)?;
            ensure_rotatable(&current_old, now)?;
            let expires_at = checked_expiry(now, request.expires_in)?;
            let prepare_expires_at = checked_expiry(now, PREPARE_LEASE)?;
            let batch = self.build_batch(BuildBatchSpec {
                task: &current_task,
                count: 1,
                participant_kind: current_old.participant_kind,
                capability_limits: &capabilities,
                request_id: &request.request_id,
                payload_hash: &payload_hash,
                expires_at,
                prepare_expires_at,
                shell,
                provision_worktree,
                operation: MarkerOperation::Rotate {
                    old_invite_id: current_old.id.to_string(),
                },
                rotates_from_id: Some(current_old.id),
            })?;

            match insert_prepared_batch(
                self.store,
                self.workspace_id,
                &request.request_id,
                &payload_hash,
                &batch,
                now,
            )? {
                PrepareInsert::Inserted => {
                    let (final_path, marker) = batch.finalize()?;
                    let result = finalize_prepared_batch(
                        self.store,
                        FinalizeBatchContext {
                            now: self.clock.now(),
                            workspace_id: self.workspace_id,
                            artifact_root: &self.artifact_root,
                            final_path,
                            marker,
                            request_id: &request.request_id,
                            payload_hash: &payload_hash,
                        },
                    )?;
                    remove_secret_best_effort(&self.artifact_root, &current_old);
                    return Ok(result);
                }
                PrepareInsert::Existing(rows) => {
                    drop(batch);
                    return self.resume_prepared(rows, &request.request_id, &payload_hash);
                }
                PrepareInsert::Replayed(result) => {
                    drop(batch);
                    return Ok(result);
                }
                PrepareInsert::EpochChanged => drop(batch),
            }
        }

        Err(SessionBridgeError::retryable(
            ErrorCode::CompletionEpochChanged,
            "task completion epoch changed repeatedly while rotating invitation",
        ))
    }

    fn resume_prepared(
        &mut self,
        rows: Vec<InvitationRow>,
        request_id: &str,
        payload_hash: &[u8; 32],
    ) -> Result<InvitationBatchManifest> {
        let batch_id = one_batch_id(&rows)?;
        let final_path = self.artifact_root.join(&batch_id);
        let marker = if final_path.is_dir() {
            load_marker(&self.artifact_root, &batch_id)?
        } else {
            let staging_path = find_staging_batch(
                &self.artifact_root,
                request_id,
                payload_hash,
                Some(&batch_id),
            )?
            .ok_or_else(|| {
                SessionBridgeError::domain(
                    ErrorCode::InviteSecretMissing,
                    "prepared invitation batch has no recoverable staging directory",
                )
                .with_details(serde_json::json!({
                    "batch_id": batch_id,
                    "request_id": request_id,
                }))
            })?;
            let marker = read_marker_at(&staging_path.join(MARKER_FILE_NAME))?;
            finalize_recovered_staging(&staging_path, &final_path)?;
            marker
        };
        validate_marker_request(&marker, request_id, payload_hash)?;
        validate_marker_workspace(&marker, self.workspace_id)?;
        let rotated_from = match &marker.operation {
            MarkerOperation::Create => None,
            MarkerOperation::Rotate { old_invite_id } => Some(parse_invitation_id(old_invite_id)?),
        };
        let result = finalize_prepared_batch(
            self.store,
            FinalizeBatchContext {
                now: self.clock.now(),
                workspace_id: self.workspace_id,
                artifact_root: &self.artifact_root,
                final_path,
                marker,
                request_id,
                payload_hash,
            },
        )?;
        if let Some(old_id) = rotated_from
            && let Ok(old) = load_invitation(self.store.connection(), self.workspace_id, old_id)
        {
            remove_secret_best_effort(&self.artifact_root, &old);
        }
        Ok(result)
    }

    fn build_batch(&self, spec: BuildBatchSpec<'_>) -> Result<PreparedBatch> {
        let batch_id = Uuid::now_v7().to_string();
        let staging = BatchStaging::new(&self.artifact_root, &batch_id)?;
        let final_path = staging.final_path().to_path_buf();
        let created_at = normalize_timestamp(self.clock.now())?;
        let expires_at_text = format_timestamp(spec.expires_at)?;
        let mut invitations = Vec::with_capacity(spec.count);
        let mut artifacts = Vec::with_capacity(spec.count);

        for index in 0..spec.count {
            let invitation_id = InvitationId::new();
            let token = InviteToken::generate()?;
            let secret_name = secret_file_name(invitation_id);
            let prompt_name = prompt_file_name(invitation_id);
            let invite_path = final_path.join(&secret_name);
            let secret = InviteSecret::new(InviteSecretInput {
                invite_id: invitation_id.to_string(),
                task_id: spec.task.id.to_string(),
                completion_epoch: u64::try_from(spec.task.completion_epoch).map_err(|_| {
                    SessionBridgeError::domain(
                        ErrorCode::Database,
                        "task completion epoch cannot be represented in an invite secret",
                    )
                })?,
                batch_id: Some(batch_id.clone()),
                token: token.clone(),
                protocol_version: PROTOCOL_VERSION.to_string(),
                prompt_template_version: PROMPT_TEMPLATE_VERSION.into(),
                expires_at: spec.expires_at,
            })?;
            staging.stage_bytes(&secret_name, &secret.to_json_bytes()?)?;

            let rendered = PromptRendererV1.render(&PromptSpec {
                task_id: &spec.task.id.to_string(),
                invite_id: &invitation_id.to_string(),
                expires_at: &expires_at_text,
                protocol_version: &PROTOCOL_VERSION.to_string(),
                shell: spec.shell,
                delivery: PromptDelivery::File {
                    invite_path: &invite_path,
                },
                provision_worktree: spec.provision_worktree,
            })?;
            staging.stage_bytes(&prompt_name, rendered.text.as_bytes())?;

            artifacts.push(MarkerArtifact {
                invite_id: invitation_id.to_string(),
                batch_index: index + 1,
                secret_file: secret_name,
                prompt_file: prompt_name,
                prompt_sha256: rendered.sha256.clone(),
            });
            invitations.push(PreparedInvitation {
                id: invitation_id,
                token,
                rotates_from_id: spec.rotates_from_id,
            });
        }

        let marker = RequestMarker {
            schema_version: MARKER_SCHEMA_VERSION,
            operation: spec.operation,
            workspace_id: self.workspace_id.to_string(),
            task_id: spec.task.id.to_string(),
            batch_id,
            request_id: spec.request_id.to_owned(),
            payload_sha256: hex_hash(spec.payload_hash),
            completion_epoch: spec.task.completion_epoch,
            prompt_shell: shell_name(spec.shell).to_owned(),
            provision_worktree: spec.provision_worktree,
            capability_limits: spec.capability_limits.to_vec(),
            created_at: format_timestamp(created_at)?,
            prepare_expires_at: format_timestamp(spec.prepare_expires_at)?,
            expires_at: expires_at_text,
            artifacts,
        };
        staging.stage_json(MARKER_FILE_NAME, &marker)?;

        Ok(PreparedBatch {
            task: spec.task.clone(),
            participant_kind: spec.participant_kind,
            capability_limits_json: serde_json::to_string(spec.capability_limits)?,
            expires_at: spec.expires_at,
            prepare_expires_at: spec.prepare_expires_at,
            marker,
            invitations,
            artifacts: PreparedArtifacts::Fresh(staging),
        })
    }
}

#[derive(Debug, Clone, Serialize)]
struct CreatePayload<'a> {
    operation: &'static str,
    task_id: TaskId,
    count: usize,
    participant_kind: ParticipantKind,
    expires_in_nanoseconds: String,
    capability_limits: &'a [String],
    prompt_shell: &'static str,
    provision_worktree: bool,
    artifact_root: String,
}

impl<'a> CreatePayload<'a> {
    fn new(request: &'a CreateInvitationBatchRequest, artifact_root: &Path) -> Self {
        Self {
            operation: "create",
            task_id: request.task_id,
            count: request.count,
            participant_kind: request.participant_kind,
            expires_in_nanoseconds: request.expires_in.whole_nanoseconds().to_string(),
            capability_limits: &request.capability_limits,
            prompt_shell: shell_name(request.shell),
            provision_worktree: request.provision_worktree,
            artifact_root: artifact_root.to_string_lossy().into_owned(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct RotatePayload {
    operation: &'static str,
    invitation_id: InvitationId,
    expires_in_nanoseconds: String,
    artifact_root: String,
}

#[derive(Debug, Clone, Serialize)]
struct RevokePayload<'a> {
    operation: &'static str,
    invitation_id: InvitationId,
    reason: &'a str,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RequestMarker {
    schema_version: u32,
    operation: MarkerOperation,
    workspace_id: String,
    task_id: String,
    batch_id: String,
    request_id: String,
    payload_sha256: String,
    completion_epoch: i64,
    prompt_shell: String,
    provision_worktree: bool,
    capability_limits: Vec<String>,
    created_at: String,
    prepare_expires_at: String,
    expires_at: String,
    artifacts: Vec<MarkerArtifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum MarkerOperation {
    Create,
    Rotate { old_invite_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MarkerArtifact {
    invite_id: String,
    batch_index: usize,
    secret_file: String,
    prompt_file: String,
    prompt_sha256: String,
}

#[derive(Debug, Clone)]
struct BuildBatchSpec<'a> {
    task: &'a TaskSnapshot,
    count: usize,
    participant_kind: ParticipantKind,
    capability_limits: &'a [String],
    request_id: &'a str,
    payload_hash: &'a [u8; 32],
    expires_at: OffsetDateTime,
    prepare_expires_at: OffsetDateTime,
    shell: PromptShell,
    provision_worktree: bool,
    operation: MarkerOperation,
    rotates_from_id: Option<InvitationId>,
}

#[derive(Debug)]
struct PreparedBatch {
    task: TaskSnapshot,
    participant_kind: ParticipantKind,
    capability_limits_json: String,
    expires_at: OffsetDateTime,
    prepare_expires_at: OffsetDateTime,
    marker: RequestMarker,
    invitations: Vec<PreparedInvitation>,
    artifacts: PreparedArtifacts,
}

#[derive(Debug)]
struct PreparedInvitation {
    id: InvitationId,
    token: InviteToken,
    rotates_from_id: Option<InvitationId>,
}

#[derive(Debug)]
enum PreparedArtifacts {
    Fresh(BatchStaging),
}

impl PreparedArtifacts {
    fn finalize(self) -> Result<PathBuf> {
        match self {
            Self::Fresh(staging) => finalize_fresh_staging(staging),
        }
    }
}

impl PreparedBatch {
    fn finalize(self) -> Result<(PathBuf, RequestMarker)> {
        let Self {
            marker, artifacts, ..
        } = self;
        Ok((artifacts.finalize()?, marker))
    }
}

#[derive(Debug, Clone)]
struct TaskSnapshot {
    id: TaskId,
    phase: TaskPhase,
    completion_epoch: i64,
}

#[derive(Debug, Clone)]
struct InvitationRow {
    id: InvitationId,
    task_id: TaskId,
    task_phase: TaskPhase,
    task_completion_epoch: i64,
    completion_epoch: i64,
    participant_kind: ParticipantKind,
    batch_id: String,
    token_hash: InviteTokenHash,
    state: InvitationState,
    blocks_completion: bool,
    capability_limits_json: String,
    prompt_template_version: i64,
    protocol_version: i64,
    expires_at: OffsetDateTime,
    rotates_from_id: Option<InvitationId>,
}

enum PrepareInsert {
    Inserted,
    Existing(Vec<InvitationRow>),
    Replayed(InvitationBatchManifest),
    EpochChanged,
}

fn insert_prepared_batch(
    store: &mut SqliteStore,
    workspace_id: WorkspaceId,
    request_id: &str,
    payload_hash: &[u8; 32],
    batch: &PreparedBatch,
    now: OffsetDateTime,
) -> Result<PrepareInsert> {
    let actor_id = workspace_id.to_string();
    let key = IdempotencyKey {
        actor_type: IDEMPOTENCY_ACTOR_TYPE,
        actor_id: &actor_id,
        request_id,
    };
    let transaction = store.begin_immediate()?;
    if let Some(stored) =
        idempotency::replay::<InvitationBatchManifest>(&transaction, key, payload_hash)?
    {
        transaction.commit()?;
        return Ok(PrepareInsert::Replayed(stored.value));
    }
    if let Some(rows) = load_rows_for_prepare_request(&transaction, workspace_id, request_id)? {
        transaction.commit()?;
        return Ok(PrepareInsert::Existing(rows));
    }

    let task = load_task(&transaction, workspace_id, batch.task.id)?;
    ensure_task_active(&task)?;
    if task.completion_epoch != batch.task.completion_epoch {
        transaction.commit()?;
        return Ok(PrepareInsert::EpochChanged);
    }
    if now >= batch.expires_at {
        return Err(SessionBridgeError::domain(
            ErrorCode::InviteExpired,
            "invitation expiry elapsed before the batch could be prepared",
        ));
    }

    if let MarkerOperation::Rotate { old_invite_id } = &batch.marker.operation {
        let old = load_invitation(
            &transaction,
            workspace_id,
            parse_invitation_id(old_invite_id)?,
        )?;
        if old.task_id != task.id {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "rotation source belongs to a different task",
            ));
        }
        ensure_rotatable(&old, now)?;
    }

    for invitation in &batch.invitations {
        transaction.execute(
            "INSERT INTO agent_invitations (
                id, task_id, completion_epoch, participant_kind, batch_id, token_hash,
                state, blocks_completion, capability_limits_json, prepare_request_id,
                prepare_expires_at, prompt_template_version, protocol_version, expires_at,
                rotates_from_id, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'prepared', ?7, ?8, ?9, ?10, ?11,
                       ?12, ?13, ?14, ?15, ?15)",
            params![
                invitation.id.to_string(),
                batch.task.id.to_string(),
                batch.task.completion_epoch,
                batch.participant_kind.as_str(),
                batch.marker.batch_id,
                invitation.token.hash().as_bytes().as_slice(),
                bool_i64(blocks_completion(batch.participant_kind)),
                batch.capability_limits_json,
                request_id,
                unix_micros(batch.prepare_expires_at),
                PROMPT_TEMPLATE_VERSION_NUMBER,
                i64::from(PROTOCOL_VERSION),
                unix_micros(batch.expires_at),
                invitation.rotates_from_id.map(|id| id.to_string()),
                unix_micros(now),
            ],
        )?;
    }
    bump_completion_revision(&transaction, batch.task.id, now)?;
    transaction.commit()?;
    Ok(PrepareInsert::Inserted)
}

struct FinalizeBatchContext<'a> {
    now: OffsetDateTime,
    workspace_id: WorkspaceId,
    artifact_root: &'a Path,
    final_path: PathBuf,
    marker: RequestMarker,
    request_id: &'a str,
    payload_hash: &'a [u8; 32],
}

fn finalize_prepared_batch(
    store: &mut SqliteStore,
    context: FinalizeBatchContext<'_>,
) -> Result<InvitationBatchManifest> {
    let FinalizeBatchContext {
        now,
        workspace_id,
        artifact_root,
        final_path,
        mut marker,
        request_id,
        payload_hash,
    } = context;
    validate_marker_request(&marker, request_id, payload_hash)?;
    validate_marker_workspace(&marker, workspace_id)?;
    validate_final_artifacts(&final_path, &marker)?;
    let now = normalize_timestamp(now)?;
    let actor_id = workspace_id.to_string();
    let key = IdempotencyKey {
        actor_type: IDEMPOTENCY_ACTOR_TYPE,
        actor_id: &actor_id,
        request_id,
    };
    let transaction = store.begin_immediate()?;
    if let Some(stored) =
        idempotency::replay::<InvitationBatchManifest>(&transaction, key, payload_hash)?
    {
        transaction.commit()?;
        return Ok(stored.value);
    }

    let mut rows = load_rows_for_prepare_request(&transaction, workspace_id, request_id)?
        .ok_or_else(|| {
            SessionBridgeError::domain(
                ErrorCode::InviteNotFound,
                "prepared invitation rows disappeared before batch finalization",
            )
        })?;
    validate_prepared_rows(&rows, &marker)?;
    let task = load_task(&transaction, workspace_id, rows[0].task_id)?;
    match task.phase {
        TaskPhase::Active => {}
        TaskPhase::Paused => {
            return Err(SessionBridgeError::retryable(
                ErrorCode::TaskPaused,
                "task is paused; retry the same invitation request after resume",
            ));
        }
        TaskPhase::Completed | TaskPhase::Cancelled | TaskPhase::Archived => {
            abandon_prepared_rows(&transaction, &rows, now)?;
            transaction.commit()?;
            return Err(task_not_active(&task));
        }
    }
    if rows.iter().any(|row| now >= row.expires_at) {
        abandon_prepared_rows(&transaction, &rows, now)?;
        transaction.commit()?;
        return Err(SessionBridgeError::domain(
            ErrorCode::InviteExpired,
            "invitation expired before batch finalization",
        ));
    }

    if marker.completion_epoch != task.completion_epoch
        || rows
            .iter()
            .any(|row| row.completion_epoch != task.completion_epoch)
    {
        rewrite_secret_epochs(&final_path, &mut marker, &rows, task.completion_epoch)?;
        let changed = transaction.execute(
            "UPDATE agent_invitations
             SET completion_epoch = ?1, updated_at = ?2
             WHERE batch_id = ?3 AND prepare_request_id = ?4 AND state = 'prepared'",
            params![
                task.completion_epoch,
                unix_micros(now),
                marker.batch_id,
                request_id,
            ],
        )?;
        if changed != rows.len() {
            return Err(version_conflict(
                "prepared invitation cohort changed while its epoch was refreshed",
            ));
        }
        for row in &mut rows {
            row.completion_epoch = task.completion_epoch;
        }
    }

    let old_rotation = match &marker.operation {
        MarkerOperation::Create => None,
        MarkerOperation::Rotate { old_invite_id } => {
            let old_id = parse_invitation_id(old_invite_id)?;
            let old = load_invitation(&transaction, workspace_id, old_id)?;
            if old.task_id != task.id {
                abandon_prepared_rows(&transaction, &rows, now)?;
                transaction.commit()?;
                return Err(SessionBridgeError::domain(
                    ErrorCode::InvalidInput,
                    "rotation source belongs to a different task",
                ));
            }
            if let Err(error) = ensure_rotatable(&old, now) {
                abandon_prepared_rows(&transaction, &rows, now)?;
                transaction.commit()?;
                return Err(error);
            }
            Some(old)
        }
    };

    let changed = transaction.execute(
        "UPDATE agent_invitations
         SET state = 'pending', updated_at = ?1
         WHERE batch_id = ?2 AND prepare_request_id = ?3 AND state = 'prepared'",
        params![unix_micros(now), marker.batch_id, request_id],
    )?;
    if changed != rows.len() {
        return Err(version_conflict(
            "prepared invitation batch changed before it could become pending",
        ));
    }
    for row in &mut rows {
        row.state = InvitationState::Pending;
    }

    let mut event_sequence = None;
    if let Some(old) = &old_rotation {
        let revoked = transaction.execute(
            "UPDATE agent_invitations
             SET state = 'revoked', revoked_at = ?1, reserved_by_request_id = NULL,
                 reservation_expires_at = NULL, updated_at = ?1
             WHERE id = ?2 AND state IN ('pending', 'reserved')",
            params![unix_micros(now), old.id.to_string()],
        )?;
        if revoked != 1 {
            return Err(version_conflict(
                "rotation source changed before the new invitation was published",
            ));
        }
        bump_completion_revision(&transaction, task.id, now)?;
        event_sequence = Some(events::append(
            &transaction,
            workspace_id,
            Some(&task.id.to_string()),
            "invitation.rotated",
            EventActor {
                kind: IDEMPOTENCY_ACTOR_TYPE,
                id: Some(&actor_id),
            },
            EventEntity {
                kind: "invitation",
                id: Some(&rows[0].id.to_string()),
            },
            &serde_json::json!({
                "old_invite_id": old.id,
                "new_invite_id": rows[0].id,
                "batch_id": marker.batch_id,
                "completion_epoch": task.completion_epoch,
            }),
            now,
        )?);
    } else {
        for row in &rows {
            event_sequence = Some(events::append(
                &transaction,
                workspace_id,
                Some(&task.id.to_string()),
                "invitation.created",
                EventActor {
                    kind: IDEMPOTENCY_ACTOR_TYPE,
                    id: Some(&actor_id),
                },
                EventEntity {
                    kind: "invitation",
                    id: Some(&row.id.to_string()),
                },
                &InvitationEvent {
                    invite_id: row.id,
                    batch_id: &row.batch_id,
                    participant_kind: row.participant_kind,
                    blocks_completion: row.blocks_completion,
                    completion_epoch: row.completion_epoch,
                    expires_at: format_timestamp(row.expires_at)?,
                    prompt_template_version: row.prompt_template_version,
                    protocol_version: row.protocol_version,
                },
                now,
            )?);
        }
    }

    let manifest = batch_manifest_from_rows(&rows, &task, artifact_root, now, true, true)?;
    idempotency::record(
        &transaction,
        key,
        Some(&task.id.to_string()),
        payload_hash,
        &manifest,
        event_sequence,
        now,
    )?;
    transaction.commit()?;
    Ok(manifest)
}

#[derive(Serialize)]
struct InvitationEvent<'a> {
    invite_id: InvitationId,
    batch_id: &'a str,
    participant_kind: ParticipantKind,
    blocks_completion: bool,
    completion_epoch: i64,
    expires_at: String,
    prompt_template_version: i64,
    protocol_version: i64,
}

fn batch_manifest_from_rows(
    rows: &[InvitationRow],
    task: &TaskSnapshot,
    artifact_root: &Path,
    now: OffsetDateTime,
    include_prompt: bool,
    audit: bool,
) -> Result<InvitationBatchManifest> {
    let batch_id = one_batch_id(rows)?;
    let mut prompts = Vec::with_capacity(rows.len());
    for row in rows {
        prompts.push(manifest_for_row(
            row,
            task,
            artifact_root,
            now,
            include_prompt,
            audit,
        )?);
    }
    prompts.sort_by_key(|manifest| manifest.batch_index);
    Ok(InvitationBatchManifest { batch_id, prompts })
}

fn manifest_for_row(
    row: &InvitationRow,
    task: &TaskSnapshot,
    artifact_root: &Path,
    now: OffsetDateTime,
    include_prompt: bool,
    audit: bool,
) -> Result<InvitationManifest> {
    let marker = load_marker(artifact_root, &row.batch_id)?;
    let marker_artifact = marker
        .artifacts
        .iter()
        .find(|artifact| artifact.invite_id == row.id.to_string())
        .ok_or_else(|| {
            SessionBridgeError::domain(
                ErrorCode::InviteNotSendable,
                "invitation marker does not contain its database invitation",
            )
            .with_details(serde_json::json!({
                "invite_id": row.id,
                "batch_id": row.batch_id,
            }))
        })?;
    let batch_count = marker.artifacts.len();
    let batch_index = marker_artifact.batch_index;
    let invite_path = secret_path(artifact_root, row);
    let prompt_path = prompt_path(artifact_root, row);
    let sendable = is_sendable(row, task, artifact_root, now);

    let mut prompt = None;
    let mut prompt_sha256 = Some(marker_artifact.prompt_sha256.clone());
    if include_prompt {
        if !audit {
            ensure_sendable(row, task, artifact_root, now)?;
        }
        let shell = parse_shell(&marker.prompt_shell)?;
        let provision_worktree = marker.provision_worktree;
        ensure_supported_versions(row)?;
        let expires_at = format_timestamp(row.expires_at)?;
        let rendered = PromptRendererV1.render(&PromptSpec {
            task_id: &row.task_id.to_string(),
            invite_id: &row.id.to_string(),
            expires_at: &expires_at,
            protocol_version: &row.protocol_version.to_string(),
            shell,
            delivery: PromptDelivery::File {
                invite_path: &invite_path,
            },
            provision_worktree,
        })?;
        prompt_sha256 = Some(rendered.sha256);
        prompt = Some(rendered.text);
    }

    Ok(InvitationManifest {
        invite_id: row.id,
        task_id: row.task_id,
        batch_id: row.batch_id.clone(),
        batch_index,
        batch_count,
        status: row.state,
        participant_kind: row.participant_kind,
        blocks_completion: row.blocks_completion,
        completion_epoch: row.completion_epoch,
        expires_at: format_timestamp(row.expires_at)?,
        prompt_template_version: row.prompt_template_version,
        protocol_version: row.protocol_version,
        prompt_path,
        prompt_sha256,
        secret_delivery: PublicSecretDelivery::File { invite_path },
        secret_exposed: false,
        sendable,
        prompt,
        rotates_from_id: row.rotates_from_id,
    })
}

fn ensure_sendable(
    row: &InvitationRow,
    task: &TaskSnapshot,
    artifact_root: &Path,
    now: OffsetDateTime,
) -> Result<()> {
    match row.state {
        InvitationState::Pending => {}
        _ => return Err(invitation_state_error(row)),
    }
    if now >= row.expires_at {
        return Err(SessionBridgeError::domain(
            ErrorCode::InviteExpired,
            "invitation has expired",
        ));
    }
    if row.completion_epoch != task.completion_epoch {
        return Err(SessionBridgeError::domain(
            ErrorCode::CompletionEpochChanged,
            "invitation belongs to an older task completion epoch",
        ));
    }
    match task.phase {
        TaskPhase::Active => {}
        TaskPhase::Paused => {
            return Err(SessionBridgeError::retryable(
                ErrorCode::TaskPaused,
                "task is paused; the invitation can be rendered after resume",
            ));
        }
        TaskPhase::Completed | TaskPhase::Cancelled | TaskPhase::Archived => {
            return Err(task_not_active(task));
        }
    }
    ensure_supported_versions(row)?;
    validate_secret_file(artifact_root, row)
}

fn is_sendable(
    row: &InvitationRow,
    task: &TaskSnapshot,
    artifact_root: &Path,
    now: OffsetDateTime,
) -> bool {
    row.state == InvitationState::Pending
        && now < row.expires_at
        && row.completion_epoch == task.completion_epoch
        && task.phase == TaskPhase::Active
        && row.prompt_template_version == PROMPT_TEMPLATE_VERSION_NUMBER
        && row.protocol_version == i64::from(PROTOCOL_VERSION)
        && validate_secret_file(artifact_root, row).is_ok()
}

fn validate_secret_file(artifact_root: &Path, row: &InvitationRow) -> Result<()> {
    let path = secret_path(artifact_root, row);
    let bytes = fs::read(&path).map_err(|_| {
        SessionBridgeError::domain(
            ErrorCode::InviteSecretMissing,
            "invitation secret file is missing or unreadable",
        )
        .with_details(serde_json::json!({
            "invite_id": row.id,
            "invite_path": path,
        }))
    })?;
    let secret = InviteSecret::from_json_slice(&bytes).map_err(|_| {
        SessionBridgeError::domain(
            ErrorCode::InviteSecretMissing,
            "invitation secret file is invalid",
        )
        .with_details(serde_json::json!({ "invite_id": row.id }))
    })?;
    let secret_expires_at = OffsetDateTime::parse(&secret.expires_at, &Rfc3339)
        .map_err(|_| {
            SessionBridgeError::domain(
                ErrorCode::InviteSecretMissing,
                "invitation secret expiry is invalid",
            )
            .with_details(serde_json::json!({ "invite_id": row.id }))
        })
        .and_then(|value| {
            normalize_timestamp(value).map_err(|_| {
                SessionBridgeError::domain(
                    ErrorCode::InviteSecretMissing,
                    "invitation secret expiry is outside the supported range",
                )
                .with_details(serde_json::json!({ "invite_id": row.id }))
            })
        })?;
    if secret.invite_id != row.id.to_string()
        || secret.task_id != row.task_id.to_string()
        || secret.batch_id.as_deref() != Some(row.batch_id.as_str())
        || secret.completion_epoch != u64::try_from(row.completion_epoch).unwrap_or(u64::MAX)
        || secret.protocol_version != row.protocol_version.to_string()
        || secret.prompt_template_version != row.prompt_template_version.to_string()
        || secret_expires_at != row.expires_at
        || !row.token_hash.verify(&secret.token)
    {
        return Err(SessionBridgeError::domain(
            ErrorCode::InviteSecretMissing,
            "invitation secret does not match its database record",
        )
        .with_details(serde_json::json!({ "invite_id": row.id })));
    }
    Ok(())
}

fn ensure_supported_versions(row: &InvitationRow) -> Result<()> {
    if row.protocol_version != i64::from(PROTOCOL_VERSION) {
        return Err(SessionBridgeError::domain(
            ErrorCode::ProtocolVersionMismatch,
            format!(
                "invitation protocol {} does not match CLI protocol {}",
                row.protocol_version, PROTOCOL_VERSION
            ),
        ));
    }
    if row.prompt_template_version != PROMPT_TEMPLATE_VERSION_NUMBER {
        return Err(SessionBridgeError::domain(
            ErrorCode::PromptTemplateUnsupported,
            format!(
                "prompt template {} is not supported by this CLI",
                row.prompt_template_version
            ),
        ));
    }
    Ok(())
}

fn ensure_rotatable(row: &InvitationRow, now: OffsetDateTime) -> Result<()> {
    if now >= row.expires_at {
        return Err(SessionBridgeError::domain(
            ErrorCode::InviteExpired,
            "expired invitation cannot be rotated",
        ));
    }
    match row.state {
        InvitationState::Pending | InvitationState::Reserved => Ok(()),
        _ => Err(invitation_state_error(row)),
    }
}

fn invitation_state_error(row: &InvitationRow) -> SessionBridgeError {
    let (code, message) = match row.state {
        InvitationState::Prepared => (
            ErrorCode::InviteNotSendable,
            "invitation files have not been finalized",
        ),
        InvitationState::Pending => (
            ErrorCode::InviteNotSendable,
            "invitation is not currently sendable",
        ),
        InvitationState::Reserved => (
            ErrorCode::InviteReserved,
            "invitation is reserved by a join attempt",
        ),
        InvitationState::Consumed => (
            ErrorCode::InviteConsumed,
            "invitation has already been consumed",
        ),
        InvitationState::Revoked => (ErrorCode::InviteRevoked, "invitation has been revoked"),
        InvitationState::Expired => (ErrorCode::InviteExpired, "invitation has expired"),
        InvitationState::Abandoned => (
            ErrorCode::InviteNotSendable,
            "invitation preparation was abandoned",
        ),
    };
    SessionBridgeError::domain(code, message).with_details(serde_json::json!({
        "invite_id": row.id,
        "state": row.state,
    }))
}

fn load_workspace_repository_identity(
    connection: &Connection,
    workspace_id: WorkspaceId,
) -> Result<String> {
    connection
        .query_row(
            "SELECT repository_identity FROM workspaces WHERE id = ?1",
            [workspace_id.to_string()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| {
            SessionBridgeError::domain(ErrorCode::WorkspaceNotFound, "Workspace was not found")
        })
}

fn load_task(
    connection: &Connection,
    workspace_id: WorkspaceId,
    task_id: TaskId,
) -> Result<TaskSnapshot> {
    let row = connection
        .query_row(
            "SELECT id, phase, completion_epoch
             FROM tasks
             WHERE id = ?1 AND workspace_id = ?2",
            params![task_id.to_string(), workspace_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((id, phase, completion_epoch)) = row else {
        return Err(SessionBridgeError::domain(
            ErrorCode::NotFound,
            format!("task {task_id} was not found in this workspace"),
        ));
    };
    Ok(TaskSnapshot {
        id: parse_task_id(&id)?,
        phase: parse_phase(&phase)?,
        completion_epoch,
    })
}

fn load_invitation(
    connection: &Connection,
    workspace_id: WorkspaceId,
    invitation_id: InvitationId,
) -> Result<InvitationRow> {
    let mut rows = load_rows(
        connection,
        "WHERE i.id = ?1 AND t.workspace_id = ?2",
        params![invitation_id.to_string(), workspace_id.to_string()],
    )?;
    rows.pop().ok_or_else(|| {
        SessionBridgeError::domain(
            ErrorCode::InviteNotFound,
            format!("invitation {invitation_id} was not found in this workspace"),
        )
    })
}

fn load_rows_for_task(
    connection: &Connection,
    workspace_id: WorkspaceId,
    task_id: TaskId,
) -> Result<Vec<InvitationRow>> {
    load_rows(
        connection,
        "WHERE i.task_id = ?1 AND t.workspace_id = ?2 ORDER BY i.created_at, i.id",
        params![task_id.to_string(), workspace_id.to_string()],
    )
}

fn load_rows_for_prepare_request(
    connection: &Connection,
    workspace_id: WorkspaceId,
    request_id: &str,
) -> Result<Option<Vec<InvitationRow>>> {
    let rows = load_rows(
        connection,
        "WHERE i.prepare_request_id = ?1 AND t.workspace_id = ?2 ORDER BY i.created_at, i.id",
        params![request_id, workspace_id.to_string()],
    )?;
    Ok((!rows.is_empty()).then_some(rows))
}

fn load_rows<P>(
    connection: &Connection,
    predicate: &str,
    parameters: P,
) -> Result<Vec<InvitationRow>>
where
    P: rusqlite::Params,
{
    let sql = format!(
        "SELECT i.id, i.task_id, t.phase, t.completion_epoch, i.completion_epoch,
                i.participant_kind, i.batch_id, i.token_hash, i.state, i.blocks_completion,
                i.capability_limits_json, i.prompt_template_version, i.protocol_version,
                i.expires_at, i.rotates_from_id
         FROM agent_invitations i
         JOIN tasks t ON t.id = i.task_id
         {predicate}"
    );
    let mut statement = connection.prepare(&sql)?;
    let raw = statement
        .query_map(parameters, |row| {
            Ok(RawInvitationRow {
                id: row.get(0)?,
                task_id: row.get(1)?,
                task_phase: row.get(2)?,
                task_completion_epoch: row.get(3)?,
                completion_epoch: row.get(4)?,
                participant_kind: row.get(5)?,
                batch_id: row.get(6)?,
                token_hash: row.get(7)?,
                state: row.get(8)?,
                blocks_completion: row.get(9)?,
                capability_limits_json: row.get(10)?,
                prompt_template_version: row.get(11)?,
                protocol_version: row.get(12)?,
                expires_at: row.get(13)?,
                rotates_from_id: row.get(14)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    raw.into_iter().map(InvitationRow::try_from).collect()
}

struct RawInvitationRow {
    id: String,
    task_id: String,
    task_phase: String,
    task_completion_epoch: i64,
    completion_epoch: i64,
    participant_kind: String,
    batch_id: String,
    token_hash: Vec<u8>,
    state: String,
    blocks_completion: i64,
    capability_limits_json: String,
    prompt_template_version: i64,
    protocol_version: i64,
    expires_at: i64,
    rotates_from_id: Option<String>,
}

impl TryFrom<RawInvitationRow> for InvitationRow {
    type Error = SessionBridgeError;

    fn try_from(raw: RawInvitationRow) -> Result<Self> {
        let token_hash: [u8; 32] = raw.token_hash.try_into().map_err(|_| {
            SessionBridgeError::domain(
                ErrorCode::Database,
                "invitation token hash has an invalid length",
            )
        })?;
        Ok(Self {
            id: parse_invitation_id(&raw.id)?,
            task_id: parse_task_id(&raw.task_id)?,
            task_phase: parse_phase(&raw.task_phase)?,
            task_completion_epoch: raw.task_completion_epoch,
            completion_epoch: raw.completion_epoch,
            participant_kind: ParticipantKind::from_str(&raw.participant_kind)
                .map_err(database_value_error)?,
            batch_id: raw.batch_id,
            token_hash: InviteTokenHash::from_bytes(token_hash),
            state: InvitationState::from_str(&raw.state).map_err(database_value_error)?,
            blocks_completion: raw.blocks_completion != 0,
            capability_limits_json: raw.capability_limits_json,
            prompt_template_version: raw.prompt_template_version,
            protocol_version: raw.protocol_version,
            expires_at: timestamp_from_micros(raw.expires_at)?,
            rotates_from_id: raw
                .rotates_from_id
                .as_deref()
                .map(parse_invitation_id)
                .transpose()?,
        })
    }
}

fn load_marker(artifact_root: &Path, batch_id: &str) -> Result<RequestMarker> {
    read_marker_at(&artifact_root.join(batch_id).join(MARKER_FILE_NAME))
}

fn read_marker_at(path: &Path) -> Result<RequestMarker> {
    let bytes = fs::read(path).map_err(|source| SessionBridgeError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let marker: RequestMarker = serde_json::from_slice(&bytes)?;
    if marker.schema_version != MARKER_SCHEMA_VERSION {
        return Err(SessionBridgeError::domain(
            ErrorCode::SchemaUpgradeRequired,
            format!(
                "invitation request marker schema {} is unsupported",
                marker.schema_version
            ),
        ));
    }
    Ok(marker)
}

fn validate_marker_request(
    marker: &RequestMarker,
    request_id: &str,
    payload_hash: &[u8; 32],
) -> Result<()> {
    if marker.request_id != request_id || marker.payload_sha256 != hex_hash(payload_hash) {
        return Err(SessionBridgeError::domain(
            ErrorCode::IdempotencyKeyReused,
            "request ID is associated with different invitation preparation artifacts",
        ));
    }
    Ok(())
}

fn validate_marker_workspace(marker: &RequestMarker, workspace_id: WorkspaceId) -> Result<()> {
    if marker.workspace_id != workspace_id.to_string() {
        return Err(SessionBridgeError::domain(
            ErrorCode::InviteNotSendable,
            "invitation request marker belongs to a different workspace",
        ));
    }
    Ok(())
}

fn validate_prepared_rows(rows: &[InvitationRow], marker: &RequestMarker) -> Result<()> {
    if rows.is_empty()
        || rows.iter().any(|row| {
            row.batch_id != marker.batch_id
                || row.task_id.to_string() != marker.task_id
                || row.state != InvitationState::Prepared
        })
    {
        return Err(SessionBridgeError::domain(
            ErrorCode::InviteNotSendable,
            "invitation preparation rows are incomplete or no longer recoverable",
        ));
    }
    let marker_ids: BTreeSet<_> = marker
        .artifacts
        .iter()
        .map(|artifact| artifact.invite_id.as_str())
        .collect();
    let row_ids: BTreeSet<_> = rows.iter().map(|row| row.id.to_string()).collect();
    if marker_ids.len() != rows.len() || !row_ids.iter().all(|id| marker_ids.contains(id.as_str()))
    {
        return Err(SessionBridgeError::domain(
            ErrorCode::InviteNotSendable,
            "invitation request marker does not match the prepared database batch",
        ));
    }
    Ok(())
}

fn validate_final_artifacts(final_path: &Path, marker: &RequestMarker) -> Result<()> {
    if !final_path.is_dir()
        || final_path.file_name().and_then(|name| name.to_str()) != Some(&marker.batch_id)
    {
        return Err(SessionBridgeError::domain(
            ErrorCode::InviteSecretMissing,
            "invitation batch final directory is missing or mismatched",
        ));
    }
    for artifact in &marker.artifacts {
        let invitation_id = parse_invitation_id(&artifact.invite_id)?;
        if artifact.secret_file != secret_file_name(invitation_id)
            || artifact.prompt_file != prompt_file_name(invitation_id)
        {
            return Err(SessionBridgeError::domain(
                ErrorCode::InviteSecretMissing,
                "invitation artifact marker contains an unsafe file name",
            ));
        }
        let prompt = fs::read(final_path.join(&artifact.prompt_file)).map_err(|source| {
            SessionBridgeError::Io {
                path: final_path.join(&artifact.prompt_file),
                source,
            }
        })?;
        if sha256_hex(&prompt) != artifact.prompt_sha256 {
            return Err(SessionBridgeError::domain(
                ErrorCode::InviteNotSendable,
                "invitation prompt does not match its request marker",
            ));
        }
        fs::metadata(final_path.join(&artifact.secret_file)).map_err(|source| {
            SessionBridgeError::Io {
                path: final_path.join(&artifact.secret_file),
                source,
            }
        })?;
    }
    Ok(())
}

fn rewrite_secret_epochs(
    final_path: &Path,
    marker: &mut RequestMarker,
    rows: &[InvitationRow],
    completion_epoch: i64,
) -> Result<()> {
    let completion_epoch_u64 = u64::try_from(completion_epoch).map_err(|_| {
        SessionBridgeError::domain(
            ErrorCode::Database,
            "task completion epoch cannot be represented in an invite secret",
        )
    })?;
    for row in rows {
        let path = final_path.join(secret_file_name(row.id));
        let bytes = fs::read(&path).map_err(|source| SessionBridgeError::Io {
            path: path.clone(),
            source,
        })?;
        let old = InviteSecret::from_json_slice(&bytes)?;
        if !row.token_hash.verify(&old.token) {
            return Err(SessionBridgeError::domain(
                ErrorCode::InviteSecretMissing,
                "cannot update an invitation secret whose token does not match the database",
            ));
        }
        let expires_at = OffsetDateTime::parse(&old.expires_at, &Rfc3339).map_err(|error| {
            SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                format!("invalid invitation expiry in secret file: {error}"),
            )
        })?;
        let updated = InviteSecret::new(InviteSecretInput {
            invite_id: old.invite_id,
            task_id: old.task_id,
            completion_epoch: completion_epoch_u64,
            batch_id: old.batch_id,
            token: old.token,
            protocol_version: old.protocol_version,
            prompt_template_version: old.prompt_template_version,
            expires_at,
        })?;
        atomic_write_private(&path, &updated.to_json_bytes()?)?;
    }
    marker.completion_epoch = completion_epoch;
    atomic_write_json_private(&final_path.join(MARKER_FILE_NAME), marker)?;
    Ok(())
}

fn abandon_prepared_rows(
    transaction: &Transaction<'_>,
    rows: &[InvitationRow],
    now: OffsetDateTime,
) -> Result<()> {
    let batch_id = one_batch_id(rows)?;
    let changed = transaction.execute(
        "UPDATE agent_invitations
         SET state = 'abandoned', updated_at = ?1
         WHERE batch_id = ?2 AND state = 'prepared'",
        params![unix_micros(now), batch_id],
    )?;
    if changed != rows.len() {
        return Err(version_conflict(
            "prepared invitation batch changed while it was being abandoned",
        ));
    }
    bump_completion_revision(transaction, rows[0].task_id, now)?;
    Ok(())
}

fn bump_completion_revision(
    transaction: &Transaction<'_>,
    task_id: TaskId,
    now: OffsetDateTime,
) -> Result<()> {
    let changed = transaction.execute(
        "UPDATE tasks
         SET completion_revision = completion_revision + 1, updated_at = ?2
         WHERE id = ?1",
        params![task_id.to_string(), unix_micros(now)],
    )?;
    if changed != 1 {
        return Err(SessionBridgeError::domain(
            ErrorCode::NotFound,
            format!("task {task_id} disappeared during invitation cohort mutation"),
        ));
    }
    Ok(())
}

fn finalize_fresh_staging(staging: BatchStaging) -> Result<PathBuf> {
    let staging_path = staging.staging_path().to_path_buf();
    let final_path = staging.final_path().to_path_buf();
    let parent = final_path.parent().ok_or_else(|| {
        SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            "invitation batch final path has no parent",
        )
    })?;
    match fs::rename(&staging_path, &final_path) {
        Ok(()) => {
            drop(staging);
            sync_directory(parent)?;
            Ok(final_path)
        }
        Err(source) => {
            // BatchStaging normally cleans on Drop. Once prepared rows exist the staging
            // directory is recovery state, so intentionally retain it on rename failure.
            std::mem::forget(staging);
            Err(SessionBridgeError::Io {
                path: final_path,
                source,
            })
        }
    }
}

fn finalize_recovered_staging(staging_path: &Path, final_path: &Path) -> Result<()> {
    if final_path.exists() {
        return Ok(());
    }
    fs::rename(staging_path, final_path).map_err(|source| SessionBridgeError::Io {
        path: final_path.to_path_buf(),
        source,
    })?;
    sync_directory(final_path.parent().ok_or_else(|| {
        SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            "invitation batch final path has no parent",
        )
    })?)
}

fn find_staging_batch(
    artifact_root: &Path,
    request_id: &str,
    payload_hash: &[u8; 32],
    batch_id: Option<&str>,
) -> Result<Option<PathBuf>> {
    let mut found = Vec::new();
    for entry in fs::read_dir(artifact_root).map_err(|source| SessionBridgeError::Io {
        path: artifact_root.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| SessionBridgeError::Io {
            path: artifact_root.to_path_buf(),
            source,
        })?;
        let file_type = entry.file_type().map_err(|source| SessionBridgeError::Io {
            path: entry.path(),
            source,
        })?;
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with('.') || !name.ends_with(".staging") {
            continue;
        }
        let marker_path = entry.path().join(MARKER_FILE_NAME);
        let Ok(marker) = read_marker_at(&marker_path) else {
            continue;
        };
        if marker.request_id == request_id
            && marker.payload_sha256 == hex_hash(payload_hash)
            && batch_id.is_none_or(|batch_id| marker.batch_id == batch_id)
        {
            found.push(entry.path());
        }
    }
    match found.len() {
        0 => Ok(None),
        1 => Ok(found.pop()),
        _ => Err(SessionBridgeError::domain(
            ErrorCode::InviteNotSendable,
            "multiple invitation staging directories claim the same request ID",
        )
        .with_details(serde_json::json!({ "request_id": request_id }))),
    }
}

fn replay_batch(
    connection: &Connection,
    workspace_id: WorkspaceId,
    request_id: &str,
    payload_hash: &[u8; 32],
) -> Result<Option<InvitationBatchManifest>> {
    let actor_id = workspace_id.to_string();
    let key = IdempotencyKey {
        actor_type: IDEMPOTENCY_ACTOR_TYPE,
        actor_id: &actor_id,
        request_id,
    };
    Ok(
        idempotency::replay::<InvitationBatchManifest>(connection, key, payload_hash)?
            .map(|stored| stored.value),
    )
}

fn normalize_capabilities(values: &[String]) -> Result<Vec<String>> {
    let mut normalized = BTreeSet::new();
    for value in values {
        let value = value.trim();
        if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
            return Err(SessionBridgeError::domain(
                ErrorCode::InvalidInput,
                "capability limits must be non-empty printable strings of at most 128 characters",
            ));
        }
        normalized.insert(value.to_owned());
    }
    Ok(normalized.into_iter().collect())
}

fn validate_count(count: usize) -> Result<()> {
    if !(1..=MAX_BATCH_SIZE).contains(&count) {
        return Err(SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            format!("invitation count must be between 1 and {MAX_BATCH_SIZE}"),
        ));
    }
    Ok(())
}

fn validate_request_id(request_id: &str) -> Result<()> {
    if request_id.is_empty() || request_id.len() > 256 || request_id.chars().any(char::is_control) {
        return Err(SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            "request ID must be a non-empty printable string of at most 256 characters",
        ));
    }
    Ok(())
}

fn validate_expiry_duration(duration: Duration) -> Result<()> {
    if duration <= Duration::ZERO {
        return Err(SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            "invitation expiry duration must be positive",
        ));
    }
    Ok(())
}

fn ensure_task_active(task: &TaskSnapshot) -> Result<()> {
    match task.phase {
        TaskPhase::Active => Ok(()),
        TaskPhase::Paused => Err(SessionBridgeError::retryable(
            ErrorCode::TaskPaused,
            "task is paused; resume it before creating or rotating invitations",
        )),
        TaskPhase::Completed | TaskPhase::Cancelled | TaskPhase::Archived => {
            Err(task_not_active(task))
        }
    }
}

fn task_not_active(task: &TaskSnapshot) -> SessionBridgeError {
    SessionBridgeError::domain(
        ErrorCode::TaskNotActive,
        format!(
            "task {} is {} and cannot accept new invitations",
            task.id, task.phase
        ),
    )
    .with_details(serde_json::json!({
        "task_id": task.id,
        "phase": task.phase,
    }))
}

fn blocks_completion(kind: ParticipantKind) -> bool {
    matches!(kind, ParticipantKind::Worker)
}

fn checked_expiry(now: OffsetDateTime, duration: Duration) -> Result<OffsetDateTime> {
    let value = now.checked_add(duration).ok_or_else(|| {
        SessionBridgeError::domain(ErrorCode::InvalidInput, "invitation expiry is out of range")
    })?;
    normalize_timestamp(value)
}

fn normalize_timestamp(value: OffsetDateTime) -> Result<OffsetDateTime> {
    timestamp_from_micros(unix_micros(value))
}

fn timestamp_from_micros(value: i64) -> Result<OffsetDateTime> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(value) * 1_000).map_err(|error| {
        SessionBridgeError::domain(
            ErrorCode::Database,
            format!("timestamp is outside the supported range: {error}"),
        )
    })
}

fn format_timestamp(value: OffsetDateTime) -> Result<String> {
    value.format(&Rfc3339).map_err(|error| {
        SessionBridgeError::domain(
            ErrorCode::Internal,
            format!("failed to format invitation timestamp: {error}"),
        )
    })
}

fn parse_phase(value: &str) -> Result<TaskPhase> {
    TaskPhase::from_str(value).map_err(database_value_error)
}

fn parse_task_id(value: &str) -> Result<TaskId> {
    TaskId::from_str(value).map_err(|error| {
        SessionBridgeError::domain(
            ErrorCode::Database,
            format!("database contains an invalid task ID: {error}"),
        )
    })
}

fn parse_invitation_id(value: &str) -> Result<InvitationId> {
    InvitationId::from_str(value).map_err(|error| {
        SessionBridgeError::domain(
            ErrorCode::Database,
            format!("database or marker contains an invalid invitation ID: {error}"),
        )
    })
}

fn database_value_error(error: String) -> SessionBridgeError {
    SessionBridgeError::domain(ErrorCode::Database, error)
}

fn version_conflict(message: &str) -> SessionBridgeError {
    SessionBridgeError::retryable(ErrorCode::VersionConflict, message)
}

fn bool_i64(value: bool) -> i64 {
    if value { 1 } else { 0 }
}

fn one_batch_id(rows: &[InvitationRow]) -> Result<String> {
    let batches: BTreeSet<_> = rows.iter().map(|row| row.batch_id.as_str()).collect();
    if batches.len() != 1 {
        return Err(SessionBridgeError::domain(
            ErrorCode::InviteNotSendable,
            "invitation request spans multiple database batches",
        ));
    }
    Ok((*batches.first().expect("one batch checked")).to_owned())
}

fn secret_file_name(invitation_id: InvitationId) -> String {
    format!("invite-{invitation_id}.json")
}

fn prompt_file_name(invitation_id: InvitationId) -> String {
    format!("prompt-{invitation_id}.txt")
}

fn secret_path(artifact_root: &Path, row: &InvitationRow) -> PathBuf {
    artifact_root
        .join(&row.batch_id)
        .join(secret_file_name(row.id))
}

fn prompt_path(artifact_root: &Path, row: &InvitationRow) -> PathBuf {
    artifact_root
        .join(&row.batch_id)
        .join(prompt_file_name(row.id))
}

fn remove_secret_best_effort(artifact_root: &Path, row: &InvitationRow) {
    let path = secret_path(artifact_root, row);
    if let Err(error) = fs::remove_file(&path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(
            invite_id = %row.id,
            path = %path.display(),
            error = %error,
            "failed to remove revoked invitation secret"
        );
    }
}

fn shell_name(shell: PromptShell) -> &'static str {
    match shell {
        PromptShell::Posix => "posix",
        PromptShell::PowerShell => "powershell",
    }
}

fn parse_shell(value: &str) -> Result<PromptShell> {
    match value {
        "posix" => Ok(PromptShell::Posix),
        "powershell" => Ok(PromptShell::PowerShell),
        _ => Err(SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            format!("unknown invitation prompt shell: {value}"),
        )),
    }
}

fn path_text(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| {
        SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            format!("path is not valid UTF-8: {}", path.display()),
        )
    })
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let current = std::env::current_dir().map_err(|source| SessionBridgeError::Io {
        path: PathBuf::from("."),
        source,
    })?;
    Ok(current.join(path))
}

fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::fs::File;
        let directory = File::open(path).map_err(|source| SessionBridgeError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        directory
            .sync_all()
            .map_err(|source| SessionBridgeError::Io {
                path: path.to_path_buf(),
                source,
            })?;
    }
    Ok(())
}

fn hex_hash(hash: &[u8; 32]) -> String {
    let mut output = String::with_capacity(64);
    for byte in hash {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}
