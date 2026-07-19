use std::fmt;

use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::{InvitationError, InviteToken, Result};

pub const INVITE_SECRET_SCHEMA_VERSION: &str = "1";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteSecret {
    pub schema_version: String,
    pub invite_id: String,
    pub task_id: String,
    pub completion_epoch: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,
    pub token: InviteToken,
    pub protocol_version: String,
    pub prompt_template_version: String,
    pub expires_at: String,
}

pub struct InviteSecretInput {
    pub invite_id: String,
    pub task_id: String,
    pub completion_epoch: u64,
    pub batch_id: Option<String>,
    pub token: InviteToken,
    pub protocol_version: String,
    pub prompt_template_version: String,
    pub expires_at: OffsetDateTime,
}

impl InviteSecret {
    pub fn new(input: InviteSecretInput) -> Result<Self> {
        let expires_at = input
            .expires_at
            .format(&Rfc3339)
            .map_err(|error| InvitationError::TimeFormat(error.to_string()))?;

        Ok(Self {
            schema_version: INVITE_SECRET_SCHEMA_VERSION.to_owned(),
            invite_id: input.invite_id,
            task_id: input.task_id,
            completion_epoch: input.completion_epoch,
            batch_id: input.batch_id,
            token: input.token,
            protocol_version: input.protocol_version,
            prompt_template_version: input.prompt_template_version,
            expires_at,
        })
    }

    pub fn to_json_bytes(&self) -> Result<Vec<u8>> {
        let mut bytes = serde_json::to_vec(self)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    pub fn from_json_slice(bytes: &[u8]) -> Result<Self> {
        let secret: Self = serde_json::from_slice(bytes)?;
        if secret.schema_version != INVITE_SECRET_SCHEMA_VERSION {
            return Err(InvitationError::InvalidToken(format!(
                "unsupported invite secret schema version {}",
                secret.schema_version
            )));
        }
        if secret.invite_id.is_empty()
            || secret.task_id.is_empty()
            || secret.protocol_version.is_empty()
            || secret.prompt_template_version.is_empty()
        {
            return Err(InvitationError::InvalidToken(
                "invite secret contains an empty required field".to_owned(),
            ));
        }
        OffsetDateTime::parse(&secret.expires_at, &Rfc3339)
            .map_err(|error| InvitationError::TimeFormat(error.to_string()))?;
        Ok(secret)
    }
}

impl fmt::Debug for InviteSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InviteSecret")
            .field("schema_version", &self.schema_version)
            .field("invite_id", &self.invite_id)
            .field("task_id", &self.task_id)
            .field("completion_epoch", &self.completion_epoch)
            .field("batch_id", &self.batch_id)
            .field("token", &"[REDACTED]")
            .field("protocol_version", &self.protocol_version)
            .field("prompt_template_version", &self.prompt_template_version)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}
