use std::{collections::BTreeMap, fs, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    Result,
    domain::AutonomyPolicy,
    error::{ErrorCode, SessionBridgeError},
    invitation::parse_duration,
    workspace::{LocatedWorkspace, atomic_create_private},
};

pub const SETTINGS_SCHEMA_VERSION: u32 = 1;
pub const SETTINGS_FILE_NAME: &str = "settings.toml";
pub const DEFAULT_PROFILE_NAME: &str = "normal";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingsShell {
    Posix,
    Powershell,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchProfile {
    pub agents: u16,
    pub expires_in: String,
    pub autonomy: AutonomyPolicy,
    pub shell: SettingsShell,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSettings {
    pub schema_version: u32,
    pub default_profile: String,
    pub profiles: BTreeMap<String, LaunchProfile>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoadedSettings {
    pub path: PathBuf,
    pub exists: bool,
    pub settings: ProjectSettings,
}

impl Default for ProjectSettings {
    fn default() -> Self {
        let mut profiles = BTreeMap::new();
        profiles.insert(
            DEFAULT_PROFILE_NAME.into(),
            LaunchProfile {
                agents: 4,
                expires_in: "2h".into(),
                autonomy: AutonomyPolicy::Bounded,
                shell: SettingsShell::Posix,
                target_branch: None,
            },
        );
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            default_profile: DEFAULT_PROFILE_NAME.into(),
            profiles,
        }
    }
}

impl ProjectSettings {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version > SETTINGS_SCHEMA_VERSION {
            return Err(SessionBridgeError::domain(
                ErrorCode::SchemaTooNew,
                format!(
                    "settings schema {} is newer than supported schema {}",
                    self.schema_version, SETTINGS_SCHEMA_VERSION
                ),
            ));
        }
        if self.schema_version < SETTINGS_SCHEMA_VERSION {
            return Err(SessionBridgeError::domain(
                ErrorCode::SchemaUpgradeRequired,
                format!(
                    "settings schema {} must be upgraded to {}",
                    self.schema_version, SETTINGS_SCHEMA_VERSION
                ),
            ));
        }
        validate_profile_name(&self.default_profile)?;
        if self.profiles.is_empty() {
            return Err(invalid("settings must define at least one profile"));
        }
        for (name, profile) in &self.profiles {
            validate_profile_name(name)?;
            if profile.agents == 0 || usize::from(profile.agents) > 256 {
                return Err(invalid(format!(
                    "profile '{name}' agents must be between 1 and 256"
                )));
            }
            parse_duration(&profile.expires_in)?;
            if profile
                .target_branch
                .as_deref()
                .is_some_and(|value| value.trim().is_empty())
            {
                return Err(invalid(format!(
                    "profile '{name}' target_branch cannot be empty"
                )));
            }
        }
        if !self.profiles.contains_key(&self.default_profile) {
            return Err(invalid(format!(
                "default profile '{}' is not defined",
                self.default_profile
            )));
        }
        Ok(())
    }

    pub fn profile(&self, requested: Option<&str>) -> Result<(&str, &LaunchProfile)> {
        let name = requested.unwrap_or(&self.default_profile);
        validate_profile_name(name)?;
        self.profiles
            .get_key_value(name)
            .map(|(name, profile)| (name.as_str(), profile))
            .ok_or_else(|| {
                SessionBridgeError::domain(
                    ErrorCode::InvalidInput,
                    format!("launch profile '{name}' is not defined"),
                )
                .with_details(serde_json::json!({
                    "profile": name,
                    "available_profiles": self.profiles.keys().collect::<Vec<_>>(),
                }))
            })
    }
}

pub fn settings_path(workspace: &LocatedWorkspace) -> PathBuf {
    workspace
        .paths
        .config_path
        .parent()
        .expect("workspace config always has a parent")
        .join(SETTINGS_FILE_NAME)
}

pub fn load(workspace: &LocatedWorkspace) -> Result<LoadedSettings> {
    let path = settings_path(workspace);
    let (exists, settings) = match fs::read_to_string(&path) {
        Ok(value) => (true, toml::from_str(&value)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            (false, ProjectSettings::default())
        }
        Err(source) => {
            return Err(SessionBridgeError::Io {
                path: path.clone(),
                source,
            });
        }
    };
    settings.validate()?;
    Ok(LoadedSettings {
        path,
        exists,
        settings,
    })
}

pub fn initialize(workspace: &LocatedWorkspace) -> Result<LoadedSettings> {
    let path = settings_path(workspace);
    if path.exists() {
        return Err(SessionBridgeError::domain(
            ErrorCode::InvalidInput,
            format!("settings file already exists: {}", path.display()),
        ));
    }
    let settings = ProjectSettings::default();
    let encoded = toml::to_string_pretty(&settings)?;
    atomic_create_private(&path, encoded.as_bytes())?;
    Ok(LoadedSettings {
        path,
        exists: true,
        settings,
    })
}

fn validate_profile_name(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(invalid(format!(
            "invalid profile name '{value}'; use 1-64 ASCII letters, digits, '-' or '_'"
        )));
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> SessionBridgeError {
    SessionBridgeError::domain(ErrorCode::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_settings_are_valid() {
        let settings = ProjectSettings::default();
        settings.validate().unwrap();
        let (_, profile) = settings.profile(None).unwrap();
        assert_eq!(profile.agents, 4);
        assert_eq!(profile.expires_in, "2h");
    }

    #[test]
    fn rejects_missing_default_profile() {
        let settings = ProjectSettings {
            default_profile: "missing".into(),
            ..ProjectSettings::default()
        };
        assert_eq!(
            settings.validate().unwrap_err().code(),
            ErrorCode::InvalidInput
        );
    }
}
