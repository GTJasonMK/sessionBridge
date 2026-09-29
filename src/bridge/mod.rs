//! Workspace collaboration protocol v2. The legacy state directory is never opened.
pub mod cli;
pub mod git;
pub mod ipc;
pub mod mcp;
pub mod process;
pub mod provider;
pub mod runtime;
pub mod store;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub use crate::error::{Error, Result, error};
pub use crate::private_files::atomic_write_private as private_write;
pub fn id() -> String {
    uuid::Uuid::now_v7().to_string()
}
pub fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}
pub fn hash(value: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
pub fn secret() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| error("RANDOM", e.to_string()))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
pub fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty() && s.len() <= 32768)
        .ok_or_else(|| error("INVALID_INPUT", format!("missing or invalid {key}")))
}
pub fn optional_integer(v: &Value, key: &str, default: i64) -> Result<i64> {
    match v.get(key) {
        None => Ok(default),
        Some(n) => n.as_i64().filter(|n| *n >= 0).ok_or_else(|| {
            error(
                "INVALID_INPUT",
                format!("{key} must be a nonnegative integer"),
            )
        }),
    }
}
pub fn optional_text<'a>(v: &'a Value, key: &str) -> Result<Option<&'a str>> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.len() <= 32768 => Ok(Some(s)),
        _ => Err(error("INVALID_INPUT", format!("{key} must be text"))),
    }
}
pub fn state_dir() -> Result<PathBuf> {
    let base = if let Some(path) = std::env::var_os("SESSIONBRIDGE_V2_STATE") {
        PathBuf::from(path)
    } else {
        directories::BaseDirs::new()
            .ok_or_else(|| error("HOME", "home directory unavailable"))?
            .state_dir()
            .ok_or_else(|| error("STATE", "state directory unavailable"))?
            .join("sessionbridge-v2")
    };
    crate::private_files::ensure_private_dir(&base)?;
    Ok(base.canonicalize()?)
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub workspace: String,
    pub operation: String,
    #[serde(default)]
    pub args: Value,
    pub token: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Context {
    pub workspace: String,
    pub member: String,
    pub generation: i64,
    pub token: String,
}
