use std::{
    collections::VecDeque,
    env,
    fs::File,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    sync::OnceLock,
};

use anyhow::anyhow;
use serde::Serialize;
use tracing_appender::{
    non_blocking::{NonBlockingBuilder, WorkerGuard},
    rolling::{RollingFileAppender, Rotation},
};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

use crate::{
    Result,
    error::{ErrorCode, SessionBridgeError},
    workspace::{default_state_root, ensure_private_dir},
};

pub const LOG_DIR_ENV: &str = "SESSIONBRIDGE_LOG_DIR";
pub const LOG_FILTER_ENV: &str = "SESSIONBRIDGE_LOG";
pub const LOG_FILE_PREFIX: &str = "sessionbridge";
pub const LOG_FILE_SUFFIX: &str = "jsonl";
pub const MAX_LOG_FILES: usize = 15;
pub const MAX_TAIL_LINES: usize = 10_000;

static INVOCATION_ID: OnceLock<String> = OnceLock::new();

pub struct LoggingGuard {
    _worker_guard: WorkerGuard,
    pub directory: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogLocationResult {
    pub log_dir: PathBuf,
    pub active_files: Vec<PathBuf>,
    pub rotation: &'static str,
    pub max_log_files: usize,
    pub filter_environment: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    pub file: PathBuf,
    pub record: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogTailResult {
    pub log_dir: PathBuf,
    pub files_scanned: Vec<PathBuf>,
    pub entries: Vec<LogEntry>,
}

pub fn set_invocation_id(invocation_id: impl Into<String>) {
    let _ = INVOCATION_ID.set(invocation_id.into());
}

pub fn invocation_id() -> Option<String> {
    INVOCATION_ID.get().cloned()
}

pub fn init() -> Result<LoggingGuard> {
    let directory = log_directory()?;
    ensure_private_dir(&directory)?;
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(LOG_FILE_PREFIX)
        .filename_suffix(LOG_FILE_SUFFIX)
        .max_log_files(MAX_LOG_FILES)
        .build(&directory)
        .map_err(|error| {
            SessionBridgeError::Internal(anyhow!(
                "failed to initialize rolling log file in {}: {error}",
                directory.display()
            ))
        })?;
    let (writer, worker_guard) = NonBlockingBuilder::default()
        .lossy(false)
        .thread_name("sessionbridge-log-writer")
        .finish(appender);
    let filter = log_filter()?;
    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .json()
                .flatten_event(true)
                .with_current_span(true)
                .with_span_list(false)
                .with_target(true)
                .with_ansi(false)
                .with_writer(writer),
        )
        .try_init()
        .map_err(|error| {
            SessionBridgeError::Internal(anyhow!("failed to install tracing subscriber: {error}"))
        })?;
    Ok(LoggingGuard {
        _worker_guard: worker_guard,
        directory,
    })
}

pub fn log_directory() -> Result<PathBuf> {
    match env::var_os(LOG_DIR_ENV) {
        Some(value) if value.is_empty() => Err(SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            format!("{LOG_DIR_ENV} cannot be empty"),
        )),
        Some(value) => absolutize(PathBuf::from(value)),
        None => Ok(default_state_root()?.join("logs")),
    }
}

pub fn location() -> Result<LogLocationResult> {
    let log_dir = log_directory()?;
    ensure_private_dir(&log_dir)?;
    let active_files = list_log_files(&log_dir)?;
    Ok(LogLocationResult {
        log_dir,
        active_files,
        rotation: "daily_utc",
        max_log_files: MAX_LOG_FILES,
        filter_environment: LOG_FILTER_ENV,
    })
}

pub fn tail(lines: usize) -> Result<LogTailResult> {
    if lines == 0 || lines > MAX_TAIL_LINES {
        return Err(SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            format!("log tail lines must be between 1 and {MAX_TAIL_LINES}"),
        ));
    }
    let log_dir = log_directory()?;
    ensure_private_dir(&log_dir)?;
    let files_scanned = list_log_files(&log_dir)?;
    let mut recent = VecDeque::with_capacity(lines);
    for file in &files_scanned {
        let reader = BufReader::new(File::open(file).map_err(|source| SessionBridgeError::Io {
            path: file.clone(),
            source,
        })?);
        for line in reader.lines() {
            let line = line.map_err(|source| SessionBridgeError::Io {
                path: file.clone(),
                source,
            })?;
            if line.trim().is_empty() {
                continue;
            }
            if recent.len() == lines {
                recent.pop_front();
            }
            recent.push_back((file.clone(), line));
        }
    }
    let entries = recent
        .into_iter()
        .map(|(file, line)| LogEntry {
            file,
            record: serde_json::from_str(&line)
                .unwrap_or_else(|_| serde_json::json!({ "unparsed": line })),
        })
        .collect();
    Ok(LogTailResult {
        log_dir,
        files_scanned,
        entries,
    })
}

fn log_filter() -> Result<EnvFilter> {
    let value = env::var(LOG_FILTER_ENV)
        .ok()
        .or_else(|| env::var("RUST_LOG").ok())
        .unwrap_or_else(|| "sessionbridge=info".to_owned());
    if value.trim().is_empty() {
        return Err(SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            format!("{LOG_FILTER_ENV} cannot be empty"),
        ));
    }
    EnvFilter::try_new(&value).map_err(|error| {
        SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            format!("invalid {LOG_FILTER_ENV} filter '{value}': {error}"),
        )
    })
}

fn list_log_files(directory: &Path) -> Result<Vec<PathBuf>> {
    let entries = std::fs::read_dir(directory)
        .map_err(|source| SessionBridgeError::Io {
            path: directory.to_path_buf(),
            source,
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|source| SessionBridgeError::Io {
            path: directory.to_path_buf(),
            source,
        })?;
    let mut files = entries
        .into_iter()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && is_log_file(path))
        .collect::<Vec<_>>();
    files.sort();
    Ok(files)
}

fn is_log_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name.starts_with(&format!("{LOG_FILE_PREFIX}."))
                && name.ends_with(&format!(".{LOG_FILE_SUFFIX}"))
        })
}

fn absolutize(path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        std::env::current_dir()
            .map(|current| current.join(&path))
            .map_err(|source| SessionBridgeError::Io { path, source })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_only_sessionbridge_jsonl_log_files() {
        assert!(is_log_file(Path::new("sessionbridge.2026-07-10.jsonl")));
        assert!(!is_log_file(Path::new("other.2026-07-10.jsonl")));
        assert!(!is_log_file(Path::new("sessionbridge.2026-07-10.log")));
    }
}
