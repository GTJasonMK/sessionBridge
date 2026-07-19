mod duration;
mod files;
mod output;
mod prompt;
mod secret;
mod token;

use std::path::PathBuf;

use thiserror::Error;

pub use duration::parse_duration;
pub use files::{
    BatchStaging, FinalizedBatch, PermissionGuarantee, atomic_write_private, ensure_private_dir,
    write_json_private,
};
pub use output::{
    PromptDestination, PromptOutputFormat, PromptOutputOptions, SecretDeliveryMode,
    ValidatedPromptOutput, validate_output_options,
};
pub use prompt::{
    PROMPT_TEMPLATE_VERSION, PromptDelivery, PromptRendererV1, PromptShell, PromptSpec,
    RenderedPrompt, sha256_hex,
};
pub use secret::{InviteSecret, InviteSecretInput};
pub use token::{InviteToken, InviteTokenHash};

pub type Result<T> = std::result::Result<T, InvitationError>;

#[derive(Debug, Error)]
pub enum InvitationError {
    #[error("random number generation failed: {0}")]
    Random(String),
    #[error("invalid invitation token: {0}")]
    InvalidToken(String),
    #[error("invalid duration: {0}")]
    InvalidDuration(String),
    #[error("invalid invitation output options: {0}")]
    InvalidOutput(String),
    #[error("path is not valid UTF-8: {0}")]
    NonUtf8Path(PathBuf),
    #[error("unsafe path component: {0}")]
    UnsafePathComponent(String),
    #[error("path already exists: {0}")]
    AlreadyExists(PathBuf),
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("timestamp formatting failed: {0}")]
    TimeFormat(String),
}

impl InvitationError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}
