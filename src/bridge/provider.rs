use super::*;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde_json::json;
use std::process::Command;

fn database() -> Result<Connection> {
    let path = if let Some(path) = std::env::var_os("SESSIONBRIDGE_CCSWITCH_DB") {
        PathBuf::from(path)
    } else {
        directories::BaseDirs::new()
            .ok_or_else(|| error("HOME", "home unavailable"))?
            .home_dir()
            .join(".cc-switch/cc-switch.db")
    };
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|cause| {
        error(
            "CCSWITCH_UNAVAILABLE",
            format!("无法读取 CC Switch 配置：{cause}"),
        )
    })
}
pub fn list() -> Result<Value> {
    let db = database()?;
    let mut stmt = db.prepare(
        "SELECT id,name,app_type,settings_config FROM providers WHERE app_type IN ('claude','codex') ORDER BY app_type,name",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let providers = rows
        .into_iter()
        .map(|(id, name, cli, raw)| {
            let settings: Value = serde_json::from_str(&raw)?;
            Ok(json!({
                "id": id,
                "name": name,
                "cli": cli,
                "model": configured_model(&cli, &settings)?
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(json!(providers))
}

fn configured_model(cli: &str, settings: &Value) -> Result<String> {
    let model = match cli {
        "claude" => settings
            .get("model")
            .and_then(Value::as_str)
            .filter(|model| !model.trim().is_empty())
            .or_else(|| settings.pointer("/env/ANTHROPIC_MODEL").and_then(Value::as_str))
            .unwrap_or_default()
            .to_owned(),
        "codex" => {
            let Some(raw) = settings.get("config").and_then(Value::as_str) else {
                return Ok(String::new());
            };
            let config: toml::Value = toml::from_str(raw)
                .map_err(|cause| error("PROVIDER_FORMAT", cause.to_string()))?;
            config
                .get("model")
                .and_then(toml::Value::as_str)
                .unwrap_or_default()
                .to_owned()
        }
        _ => String::new(),
    };
    Ok(model)
}
pub fn config(cli: &str, provider: &str) -> Result<Value> {
    let db = database()?;
    let raw: Option<String> = db
        .query_row(
            "SELECT settings_config FROM providers WHERE app_type=? AND id=?",
            params![cli, provider],
            |r| r.get(0),
        )
        .optional()?;
    serde_json::from_str(&raw.ok_or_else(|| {
        error(
            "PROVIDER_NOT_FOUND",
            "selected CC Switch provider no longer exists",
        )
    })?)
    .map_err(Into::into)
}

pub fn prepare(
    cli: &str,
    provider: &str,
    model: &str,
    directory: &Path,
    context: &Path,
) -> Result<Command> {
    let config = config(cli, provider)?;
    crate::private_files::ensure_private_dir(directory)?;
    let binary = std::env::current_exe()?;
    let mcp = json!({"command":binary,"args":["mcp","--context",context],
        "env":{"SESSIONBRIDGE_V2_STATE":state_dir()?}});
    let skill = include_bytes!("../../skills/sessionbridge-collaboration/SKILL.md");
    if !["claude", "codex"].contains(&cli) {
        return Err(error("INVALID_INPUT", "unsupported CLI"));
    }
    if cli == "claude" {
        let env = config
            .get("env")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                error(
                    "PROVIDER_FORMAT",
                    "Claude provider must contain env configuration",
                )
            })?;
        let settings = directory.join("settings.json");
        private_write(&settings, &serde_json::to_vec(&json!({"env":env}))?)?;
        private_write(
            &directory.join("mcp.json"),
            &serde_json::to_vec(&json!({"mcpServers":{"sessionbridge":mcp}}))?,
        )?;
    } else {
        let raw = config["config"]
            .as_str()
            .ok_or_else(|| error("PROVIDER_FORMAT", "Codex provider must contain TOML config"))?;
        let mut doc: toml::Value =
            toml::from_str(raw).map_err(|e| error("PROVIDER_FORMAT", e.to_string()))?;
        doc["model"] = toml::Value::String(model.into());
        let table = doc
            .as_table_mut()
            .ok_or_else(|| error("PROVIDER_FORMAT", "invalid Codex config"))?;
        table.insert(
            "approval_policy".into(),
            toml::Value::String("on-request".into()),
        );
        table.insert(
            "sandbox_mode".into(),
            toml::Value::String("workspace-write".into()),
        );
        let mut servers = toml::map::Map::new();
        let mut server = toml::Value::try_from(&mcp).map_err(|e| error("CONFIG", e.to_string()))?;
        server
            .as_table_mut()
            .ok_or_else(|| error("CONFIG", "invalid MCP configuration"))?
            .insert(
                "default_tools_approval_mode".into(),
                toml::Value::String("approve".into()),
            );
        servers.insert("sessionbridge".into(), server);
        table.insert("mcp_servers".into(), toml::Value::Table(servers));
        private_write(
            &directory.join("config.toml"),
            toml::to_string(&doc)
                .map_err(|e| error("CONFIG", e.to_string()))?
                .as_bytes(),
        )?;
        if !config["auth"].is_object() {
            return Err(error(
                "PROVIDER_FORMAT",
                "Codex provider auth is required; global login is not used implicitly",
            ));
        }
        private_write(
            &directory.join("auth.json"),
            &serde_json::to_vec(&config["auth"])?,
        )?;
    }
    private_write(
        &directory.join("skills/sessionbridge-collaboration/SKILL.md"),
        skill,
    )?;
    launch(cli, model, directory)
}

pub fn launch(cli: &str, model: &str, directory: &Path) -> Result<Command> {
    if !["claude", "codex"].contains(&cli) {
        return Err(error("INVALID_INPUT", "unsupported CLI"));
    }
    let mut command = Command::new(cli);
    if cli == "claude" {
        let settings = directory.join("settings.json");
        let config: Value = serde_json::from_slice(&std::fs::read(&settings)?)?;
        let env = config["env"]
            .as_object()
            .ok_or_else(|| error("PROVIDER_FORMAT", "invalid runtime environment"))?;
        for key in [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_MODEL",
        ] {
            command.env_remove(key);
        }
        for (key, value) in env {
            command.env(
                key,
                value
                    .as_str()
                    .ok_or_else(|| error("PROVIDER_FORMAT", "env values must be strings"))?,
            );
        }
        command
            .env("CLAUDE_CONFIG_DIR", directory)
            .args([
                "-p",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
                "--include-partial-messages",
                "--permission-mode",
                "acceptEdits",
                "--permission-prompt-tool",
                "stdio",
                "--settings",
            ])
            .arg(settings)
            .args([
                "--setting-sources",
                "project,local",
                "--strict-mcp-config",
                "--mcp-config",
            ])
            .arg(directory.join("mcp.json"))
            .args([
                "--model",
                model,
                "--allowedTools",
                "mcp__sessionbridge__*",
                "--disallowedTools",
                "Agent,Task",
            ]);
    } else {
        command
            .env_remove("OPENAI_API_KEY")
            .env("CODEX_HOME", directory)
            .args(["app-server", "--stdio"]);
    }
    Ok(command)
}
