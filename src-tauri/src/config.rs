use anyhow::{Result, Context};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default = "default_api_url")]
    pub api_url: String,

    #[serde(default)]
    pub api_key: String,

    #[serde(default = "default_service_label")]
    pub service_label: String,

    #[serde(default = "default_plist_path")]
    pub plist_path: String,

    #[serde(default = "default_dashboard_url")]
    pub dashboard_url: String,

    #[serde(default)]
    pub default_model: String,
}

fn default_api_url() -> String { "http://127.0.0.1:8000/v1".into() }
fn default_service_label() -> String { "ai.omlx.server".into() }
fn default_dashboard_url() -> String { "http://127.0.0.1:8000/admin".into() }

fn default_plist_path() -> String {
    let home = dirs::home_dir().unwrap_or_default();
    home.join("Library/LaunchAgents/ai.omlx.server.plist")
        .to_string_lossy()
        .into()
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            api_url: default_api_url(),
            api_key: String::new(),
            service_label: default_service_label(),
            plist_path: default_plist_path(),
            dashboard_url: default_dashboard_url(),
            default_model: String::new(),
        }
    }
}

impl AppConfig {
    pub fn config_path() -> PathBuf {
        let home = dirs::home_dir().unwrap_or_default();
        home.join(".config/omlx-menubar/config.json")
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_path();
        if path.exists() {
            let contents = std::fs::read_to_string(&path)
                .context("Failed to read config file")?;
            let config: Self = serde_json::from_str(&contents)
                .context("Failed to parse config file")?;
            Ok(config)
        } else {
            let config = Self::default();
            config.save()?;
            Ok(config)
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .context("Failed to create config directory")?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json)
            .context("Failed to write config file")?;
        #[cfg(unix)]
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .context("Failed to set config file permissions")?;
        Ok(())
    }
}
