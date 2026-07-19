use std::fmt;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use sha2::{Digest, Sha256};

use super::{InvitationError, Result};

const TOKEN_PREFIX: &str = "sb_inv_";
const TOKEN_BYTES: usize = 32;
const TOKEN_ENCODED_LEN: usize = 43;

#[derive(Clone, PartialEq, Eq)]
pub struct InviteToken(String);

impl InviteToken {
    pub fn generate() -> Result<Self> {
        let mut bytes = [0_u8; TOKEN_BYTES];
        getrandom::fill(&mut bytes).map_err(|error| InvitationError::Random(error.to_string()))?;
        Ok(Self(format!(
            "{TOKEN_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(bytes)
        )))
    }

    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let encoded = value
            .strip_prefix(TOKEN_PREFIX)
            .ok_or_else(|| InvitationError::InvalidToken("missing sb_inv_ prefix".to_owned()))?;

        if encoded.len() != TOKEN_ENCODED_LEN {
            return Err(InvitationError::InvalidToken(
                "token has an unexpected encoded length".to_owned(),
            ));
        }

        let decoded = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| {
            InvitationError::InvalidToken("token is not valid base64url".to_owned())
        })?;
        if decoded.len() != TOKEN_BYTES {
            return Err(InvitationError::InvalidToken(
                "token has an unexpected decoded length".to_owned(),
            ));
        }

        Ok(Self(value))
    }

    pub fn expose_secret(&self) -> &str {
        &self.0
    }

    pub fn hash(&self) -> InviteTokenHash {
        InviteTokenHash::from_token(self)
    }
}

impl fmt::Debug for InviteToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("InviteToken([REDACTED])")
    }
}

impl Serialize for InviteToken {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.expose_secret())
    }
}

impl<'de> Deserialize<'de> for InviteToken {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(D::Error::custom)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct InviteTokenHash([u8; 32]);

impl InviteTokenHash {
    pub fn from_token(token: &InviteToken) -> Self {
        let digest = Sha256::digest(token.expose_secret().as_bytes());
        Self(digest.into())
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(self) -> String {
        let mut output = String::with_capacity(64);
        for byte in self.0 {
            use std::fmt::Write as _;
            let _ = write!(output, "{byte:02x}");
        }
        output
    }

    pub fn verify(&self, candidate: &InviteToken) -> bool {
        let candidate = Self::from_token(candidate);
        self.0
            .iter()
            .zip(candidate.0.iter())
            .fold(0_u8, |difference, (left, right)| {
                difference | (left ^ right)
            })
            == 0
    }
}

impl fmt::Debug for InviteTokenHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("InviteTokenHash")
            .field(&self.to_hex())
            .finish()
    }
}
