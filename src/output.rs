use std::io::{self, Write};

use serde::Serialize;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{Result, SessionBridgeError};

pub const SCHEMA_VERSION: &str = "1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    Table,
    Text,
    Json,
    Jsonl,
}

#[derive(Debug, Serialize)]
pub struct Envelope<T> {
    pub schema_version: &'static str,
    pub ok: bool,
    pub data: T,
    pub meta: Meta,
}

#[derive(Debug, Serialize)]
pub struct Meta {
    pub timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invocation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<i64>,
}

#[derive(Debug, Serialize)]
struct ErrorEnvelope {
    schema_version: &'static str,
    ok: bool,
    error: ErrorBody,
    meta: Meta,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: crate::ErrorCode,
    message: String,
    retryable: bool,
    details: serde_json::Value,
}

pub fn write_json<T: Serialize>(value: T, cursor: Option<i64>) -> Result<()> {
    let envelope = Envelope {
        schema_version: SCHEMA_VERSION,
        ok: true,
        data: value,
        meta: Meta {
            timestamp: now_string(),
            invocation_id: crate::logging::invocation_id(),
            cursor,
        },
    };
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    serde_json::to_writer_pretty(&mut lock, &envelope)?;
    writeln!(lock).map_err(|source| SessionBridgeError::Io {
        path: "<stdout>".into(),
        source,
    })?;
    Ok(())
}

pub fn write_jsonl<T: Serialize>(value: &T) -> Result<()> {
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    serde_json::to_writer(&mut lock, value)?;
    writeln!(lock).map_err(|source| SessionBridgeError::Io {
        path: "<stdout>".into(),
        source,
    })?;
    Ok(())
}

pub fn write_text(text: &str) -> Result<()> {
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    lock.write_all(text.as_bytes())
        .and_then(|_| {
            if text.ends_with('\n') {
                Ok(())
            } else {
                writeln!(lock)
            }
        })
        .map_err(|source| SessionBridgeError::Io {
            path: "<stdout>".into(),
            source,
        })
}

pub fn write_value<T: Serialize>(
    format: OutputFormat,
    value: &T,
    cursor: Option<i64>,
    human: impl FnOnce() -> String,
) -> Result<()> {
    match format {
        OutputFormat::Json => write_json(value, cursor),
        OutputFormat::Jsonl => write_jsonl(value),
        OutputFormat::Table | OutputFormat::Text => write_text(&human()),
    }
}

pub fn write_error(error: &SessionBridgeError) {
    let body = ErrorEnvelope {
        schema_version: SCHEMA_VERSION,
        ok: false,
        error: ErrorBody {
            code: error.code(),
            message: error.to_string(),
            retryable: error.retryable_flag(),
            details: error.details(),
        },
        meta: Meta {
            timestamp: now_string(),
            invocation_id: crate::logging::invocation_id(),
            cursor: None,
        },
    };

    let stderr = io::stderr();
    let mut lock = stderr.lock();
    if serde_json::to_writer(&mut lock, &body).is_ok() {
        let _ = writeln!(lock);
    } else {
        let _ = writeln!(lock, "{}", error);
    }
}

fn now_string() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}
