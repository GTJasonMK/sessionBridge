use std::{fs, path::PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use sessionbridge::workspace::{atomic_write_private, ensure_private_dir};

use crate::error::{GuiError, GuiResult};

const SCHEMA_VERSION: u32 = 1;
const MAX_RECENT: usize = 10;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuiPreferences {
    pub schema_version: u32,
    pub last_workspace: Option<PathBuf>,
    pub recent_workspaces: Vec<PathBuf>,
}

impl Default for GuiPreferences {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            last_workspace: None,
            recent_workspaces: Vec::new(),
        }
    }
}

impl GuiPreferences {
    pub fn load() -> GuiResult<Self> {
        let path = path()?;
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(GuiError::from(error)),
        };
        let preferences: Self =
            toml::from_str(&content).map_err(sessionbridge::SessionBridgeError::from)?;
        if preferences.schema_version != SCHEMA_VERSION {
            return Ok(Self::default());
        }
        Ok(preferences)
    }

    pub fn remember(&mut self, root: PathBuf) -> GuiResult<()> {
        self.recent_workspaces
            .retain(|candidate| candidate != &root);
        self.recent_workspaces.insert(0, root.clone());
        self.recent_workspaces.truncate(MAX_RECENT);
        self.last_workspace = Some(root);
        self.save()
    }

    pub fn forget(&mut self, root: &PathBuf) -> GuiResult<()> {
        self.recent_workspaces.retain(|candidate| candidate != root);
        if self.last_workspace.as_ref() == Some(root) {
            self.last_workspace = self.recent_workspaces.first().cloned();
        }
        self.save()
    }

    fn save(&self) -> GuiResult<()> {
        let path = path()?;
        let encoded =
            toml::to_string_pretty(self).map_err(sessionbridge::SessionBridgeError::from)?;
        atomic_write_private(&path, encoded.as_bytes()).map_err(Into::into)
    }
}

fn path() -> GuiResult<PathBuf> {
    let dirs = ProjectDirs::from("dev", "SessionBridge", "SessionBridge").ok_or_else(|| {
        sessionbridge::SessionBridgeError::domain(
            sessionbridge::ErrorCode::WorkspaceNotFound,
            "cannot determine the GUI configuration directory",
        )
    })?;
    let directory = dirs.config_dir().join("gui");
    ensure_private_dir(&directory)?;
    Ok(directory.join("preferences.toml"))
}
