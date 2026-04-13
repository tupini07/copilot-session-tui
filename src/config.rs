use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserConfig {
    #[serde(default)]
    pub yolo: bool,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

impl Default for UserConfig {
    fn default() -> Self {
        Self {
            yolo: false,
            model: None,
            reasoning_effort: None,
        }
    }
}

pub const REASONING_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh"];

fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".config")
        })
        .join("copilot-session-tui")
        .join("config.json")
}

pub fn load() -> UserConfig {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => UserConfig::default(),
    }
}

pub fn save(config: &UserConfig) -> Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create config directory: {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(config)
        .context("Failed to serialize config")?;
    std::fs::write(&path, json)
        .with_context(|| format!("Failed to write config: {}", path.display()))?;
    Ok(())
}
