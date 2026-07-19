use serde::Serialize;
use sessionbridge::SessionBridgeError;

#[derive(Debug, Clone, Serialize)]
pub struct GuiError {
    pub code: sessionbridge::ErrorCode,
    pub message: String,
    pub retryable: bool,
    pub details: serde_json::Value,
    pub operation_id: String,
}

impl From<SessionBridgeError> for GuiError {
    fn from(error: SessionBridgeError) -> Self {
        Self {
            code: error.code(),
            message: error.to_string(),
            retryable: error.retryable_flag(),
            details: error.details(),
            operation_id: uuid::Uuid::now_v7().to_string(),
        }
    }
}

impl From<std::io::Error> for GuiError {
    fn from(error: std::io::Error) -> Self {
        SessionBridgeError::Io {
            path: "<gui-preferences>".into(),
            source: error,
        }
        .into()
    }
}

pub type GuiResult<T> = std::result::Result<T, GuiError>;
