use std::path::PathBuf;

use serde::Serialize;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, SessionBridgeError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    InvalidInput,
    NotFound,
    WorkspaceNotFound,
    CurrentTaskAmbiguous,
    LaunchNotFound,
    LaunchBaselineChanged,
    LaunchRecoveryRequired,
    TaskNotActive,
    TaskPaused,
    PlanChanged,
    VersionConflict,
    ItemNotReady,
    CapabilityMismatch,
    CapabilityNotGranted,
    SessionCapacityReached,
    ClaimConflict,
    ClaimExpired,
    StaleClaim,
    ResourceConflict,
    WorktreeConflict,
    WorktreeDirty,
    TargetBranchNotIntegrated,
    ScopeDrift,
    OpenBlocker,
    InputInvalidated,
    ContractNotAccepted,
    ProposalReviewRequired,
    ApprovalRequired,
    DecisionRequired,
    ConclusionStale,
    CompletionEpochChanged,
    ParticipantVersionChanged,
    SessionHasActiveWork,
    SessionAlreadyConcluded,
    CompletionCohortOpen,
    ParticipantBlocked,
    ParticipantStale,
    ParticipantDeparted,
    ParticipantRetired,
    RetirementNotAllowed,
    WaitNotAllowed,
    WaitAlreadyActive,
    WaitTargetInvalid,
    WaitTargetSelfOwned,
    WaitTargetUnowned,
    WaitCycleDetected,
    WaitNotFound,
    WaitSessionMismatch,
    WaitAlreadyTerminal,
    InviteNotFound,
    InviteConsumed,
    InviteRevoked,
    InviteExpired,
    InviteReserved,
    InviteNotSendable,
    InviteSecretMissing,
    InviteOutputRequired,
    JoinRecoveryRequired,
    SecretOutputNotAllowed,
    ProtocolVersionMismatch,
    PromptTemplateUnsupported,
    IdempotencyKeyReused,
    DatabaseBusy,
    MigrationFailed,
    SchemaUpgradeRequired,
    SchemaTooNew,
    FullResyncRequired,
    Io,
    Database,
    Internal,
}

#[derive(Debug, Error)]
pub enum SessionBridgeError {
    #[error("{message}")]
    Domain {
        code: ErrorCode,
        message: String,
        retryable: bool,
        details: serde_json::Value,
    },
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("migration error: {0}")]
    Migration(#[from] rusqlite_migration::Error),
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("configuration error: {0}")]
    TomlSerialize(#[from] toml::ser::Error),
    #[error("configuration error: {0}")]
    TomlDeserialize(#[from] toml::de::Error),
    #[error("{0}")]
    Internal(#[from] anyhow::Error),
}

impl SessionBridgeError {
    pub fn domain(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::Domain {
            code,
            message: message.into(),
            retryable: false,
            details: serde_json::Value::Null,
        }
    }

    pub fn full_resync_required(message: impl Into<String>) -> Self {
        Self::domain(ErrorCode::FullResyncRequired, message).with_details(serde_json::json!({
            "invalid_scope": "snapshot",
            "recovery_options": [{
                "action": "full_resync",
                "command": "sessionbridge agent sync --full --format json",
                "safe": true
            }]
        }))
    }

    pub fn retryable(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::Domain {
            code,
            message: message.into(),
            retryable: true,
            details: serde_json::Value::Null,
        }
    }

    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        if let Self::Domain {
            details: current, ..
        } = &mut self
        {
            *current = details;
        }
        self
    }

    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Domain { code, .. } => *code,
            Self::Io { .. } => ErrorCode::Io,
            Self::Database(error)
                if matches!(
                    error.sqlite_error_code(),
                    Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
                ) =>
            {
                ErrorCode::DatabaseBusy
            }
            Self::Database(_) => ErrorCode::Database,
            Self::Migration(_) => ErrorCode::MigrationFailed,
            Self::Json(_) | Self::TomlSerialize(_) | Self::TomlDeserialize(_) => {
                ErrorCode::InvalidInput
            }
            Self::Internal(_) => ErrorCode::Internal,
        }
    }

    pub fn retryable_flag(&self) -> bool {
        match self {
            Self::Domain { retryable, .. } => *retryable,
            Self::Database(error) => matches!(
                error.sqlite_error_code(),
                Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
            ),
            _ => false,
        }
    }

    pub fn details(&self) -> serde_json::Value {
        match self {
            Self::Domain { details, .. } => details.clone(),
            Self::Io { path, .. } => serde_json::json!({ "path": path }),
            _ => serde_json::Value::Null,
        }
    }

    pub fn exit_code(&self) -> u8 {
        match self.code() {
            ErrorCode::InvalidInput => 2,
            ErrorCode::NotFound
            | ErrorCode::WorkspaceNotFound
            | ErrorCode::InviteNotFound
            | ErrorCode::LaunchNotFound => 3,
            ErrorCode::DatabaseBusy => 75,
            _ => 1,
        }
    }
}

impl From<crate::invitation::InvitationError> for SessionBridgeError {
    fn from(error: crate::invitation::InvitationError) -> Self {
        use crate::invitation::InvitationError;

        match error {
            InvitationError::Io { path, source } => Self::Io { path, source },
            InvitationError::Json(error) => Self::Json(error),
            InvitationError::InvalidOutput(message) => {
                Self::domain(ErrorCode::InviteOutputRequired, message)
            }
            InvitationError::InvalidToken(message)
            | InvitationError::InvalidDuration(message)
            | InvitationError::TimeFormat(message)
            | InvitationError::UnsafePathComponent(message) => {
                Self::domain(ErrorCode::InvalidInput, message)
            }
            InvitationError::NonUtf8Path(path) => Self::domain(
                ErrorCode::InvalidInput,
                format!("path is not valid UTF-8: {}", path.display()),
            ),
            InvitationError::AlreadyExists(path) => Self::domain(
                ErrorCode::InvalidInput,
                format!("path already exists: {}", path.display()),
            ),
            InvitationError::Random(message) => Self::domain(
                ErrorCode::Internal,
                format!("secure random failure: {message}"),
            ),
        }
    }
}
