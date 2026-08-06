use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const DEFAULT_BRANCH_PREFIX: &str = "copilot/";
pub const REASONING_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh"];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserConfig {
    #[serde(default)]
    pub yolo: bool,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,

    #[serde(default)]
    pub worktree: WorktreeConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeConfig {
    #[serde(default = "default_branch_prefix")]
    pub branch_prefix: String,

    #[serde(default = "default_worktree_root")]
    pub root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveWorktreeConfig {
    pub branch_prefix: String,
    pub root: PathBuf,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<ProjectWorktreeConfig>,

    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectWorktreeConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_prefix: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<PathBuf>,

    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone)]
pub struct ProjectSettings {
    pub repository_root: PathBuf,
    config: ProjectConfig,
    global: EffectiveWorktreeConfig,
}

impl Default for WorktreeConfig {
    fn default() -> Self {
        Self {
            branch_prefix: default_branch_prefix(),
            root: default_worktree_root(),
        }
    }
}

impl ProjectSettings {
    pub fn load(repository_root: &Path, global: &UserConfig) -> Result<Self> {
        let repository_root = canonicalize_or_absolute(repository_root);
        let mut config = load_project_config(&repository_root)?;
        if config
            .worktree
            .as_ref()
            .is_some_and(ProjectWorktreeConfig::is_empty)
        {
            config.worktree = None;
        }
        Ok(Self {
            repository_root,
            config,
            global: global_worktree(global),
        })
    }

    pub fn branch_prefix_override(&self) -> Option<&str> {
        self.config
            .worktree
            .as_ref()
            .and_then(|worktree| worktree.branch_prefix.as_deref())
    }

    pub fn root_override(&self) -> Option<&Path> {
        self.config
            .worktree
            .as_ref()
            .and_then(|worktree| worktree.root.as_deref())
    }

    pub fn effective_branch_prefix(&self) -> &str {
        self.branch_prefix_override()
            .unwrap_or(&self.global.branch_prefix)
    }

    pub fn effective_root(&self) -> PathBuf {
        match self.root_override() {
            Some(root) => resolve_path(root, &self.repository_root),
            None => self.global.root.clone(),
        }
    }

    pub fn set_branch_prefix_override(&mut self, value: Option<String>) {
        self.worktree_mut().branch_prefix = value;
        self.remove_empty_worktree();
    }

    pub fn set_root_override(&mut self, value: Option<PathBuf>) {
        self.worktree_mut().root = value;
        self.remove_empty_worktree();
    }

    pub fn save(&self) -> Result<()> {
        save_project_config(&self.repository_root, &self.config)
    }

    pub fn effective(&self) -> EffectiveWorktreeConfig {
        EffectiveWorktreeConfig {
            branch_prefix: self.effective_branch_prefix().to_string(),
            root: self.effective_root(),
        }
    }

    fn worktree_mut(&mut self) -> &mut ProjectWorktreeConfig {
        self.config
            .worktree
            .get_or_insert_with(ProjectWorktreeConfig::default)
    }

    fn remove_empty_worktree(&mut self) {
        let empty = self
            .config
            .worktree
            .as_ref()
            .is_some_and(ProjectWorktreeConfig::is_empty);
        if empty {
            self.config.worktree = None;
        }
    }
}

impl ProjectConfig {
    fn is_empty(&self) -> bool {
        self.worktree.is_none() && self.extra.is_empty()
    }
}

impl ProjectWorktreeConfig {
    fn is_empty(&self) -> bool {
        self.branch_prefix.is_none() && self.root.is_none() && self.extra.is_empty()
    }
}

fn default_branch_prefix() -> String {
    DEFAULT_BRANCH_PREFIX.to_string()
}

pub fn default_worktree_root() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local")
                .join("share")
        })
        .join("cst")
        .join("wt")
}

pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".config")
        })
        .join("copilot-session-tui")
        .join("config.json")
}

pub fn project_config_path(repository_root: &Path) -> PathBuf {
    repository_root.join(".cst.json")
}

pub fn global_worktree(config: &UserConfig) -> EffectiveWorktreeConfig {
    let base = config_path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    EffectiveWorktreeConfig {
        branch_prefix: config.worktree.branch_prefix.clone(),
        root: resolve_path(&config.worktree.root, &base),
    }
}

pub fn effective_worktree(
    config: &UserConfig,
    repository_root: &Path,
) -> Result<EffectiveWorktreeConfig> {
    Ok(ProjectSettings::load(repository_root, config)?.effective())
}

pub fn load() -> UserConfig {
    let path = config_path();
    match fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => UserConfig::default(),
    }
}

pub fn save(config: &UserConfig) -> Result<()> {
    write_json_atomic(&config_path(), config)
}

fn load_project_config(repository_root: &Path) -> Result<ProjectConfig> {
    let path = project_config_path(repository_root);
    if !path.exists() {
        return Ok(ProjectConfig::default());
    }
    let content = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read project settings: {}", path.display()))?;
    serde_json::from_str(&content).with_context(|| {
        format!(
            "Invalid project settings in {}. Fix the JSON before editing it in the TUI",
            path.display()
        )
    })
}

fn save_project_config(repository_root: &Path, config: &ProjectConfig) -> Result<()> {
    let path = project_config_path(repository_root);
    if config.is_empty() {
        if path.exists() {
            fs::remove_file(&path).with_context(|| {
                format!("Failed to remove project settings: {}", path.display())
            })?;
        }
        return Ok(());
    }
    write_json_atomic(&path, config)
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path
        .parent()
        .context("Configuration path has no parent directory")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("Failed to create config directory: {}", parent.display()))?;

    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("Failed to create temporary config in {}", parent.display()))?;
    serde_json::to_writer_pretty(temp.as_file_mut(), value)
        .context("Failed to serialize config")?;
    temp.as_file_mut().write_all(b"\n")?;
    temp.as_file_mut().sync_all()?;
    temp.persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("Failed to replace config atomically: {}", path.display()))?;
    Ok(())
}

fn resolve_path(path: &Path, base: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

fn canonicalize_or_absolute(path: &Path) -> PathBuf {
    let resolved = fs::canonicalize(path).unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(path)
        }
    });
    let text = resolved.to_string_lossy();
    if let Some(stripped) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{stripped}"));
    }
    match text.strip_prefix(r"\\?\") {
        Some(stripped) => PathBuf::from(stripped),
        None => resolved,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_global_config_gets_worktree_defaults() {
        let config: UserConfig =
            serde_json::from_str(r#"{"yolo":true,"model":"gpt-5","reasoning_effort":"high"}"#)
                .unwrap();

        assert!(config.yolo);
        assert_eq!(config.worktree.branch_prefix, DEFAULT_BRANCH_PREFIX);
        assert_eq!(config.worktree.root, default_worktree_root());
    }

    #[test]
    fn project_overrides_layer_over_global_values() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join(".cst.json"),
            r#"{"worktree":{"branch_prefix":"task/","root":"local-wt"}}"#,
        )
        .unwrap();
        let mut global = UserConfig::default();
        global.worktree.branch_prefix = "global/".to_string();
        global.worktree.root = temp.path().join("global-wt");

        let settings = ProjectSettings::load(temp.path(), &global).unwrap();
        assert_eq!(settings.effective_branch_prefix(), "task/");
        assert_eq!(
            settings.effective_root(),
            canonicalize_or_absolute(temp.path()).join("local-wt")
        );
    }

    #[test]
    fn inherited_transitions_remove_empty_project_file() {
        let temp = tempfile::tempdir().unwrap();
        let mut settings = ProjectSettings::load(temp.path(), &UserConfig::default()).unwrap();
        settings.set_branch_prefix_override(Some("feature/".to_string()));
        settings.save().unwrap();
        assert!(temp.path().join(".cst.json").exists());

        settings.set_branch_prefix_override(None);
        settings.save().unwrap();
        assert!(!temp.path().join(".cst.json").exists());
    }

    #[test]
    fn project_save_preserves_other_fields() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join(".cst.json"),
            r#"{"future_setting":true,"worktree":{"future_field":7}}"#,
        )
        .unwrap();
        let mut settings = ProjectSettings::load(temp.path(), &UserConfig::default()).unwrap();
        settings.set_branch_prefix_override(Some("feature/".to_string()));
        settings.save().unwrap();

        let saved: Value =
            serde_json::from_str(&fs::read_to_string(temp.path().join(".cst.json")).unwrap())
                .unwrap();
        assert_eq!(saved["future_setting"], true);
        assert_eq!(saved["worktree"]["future_field"], 7);
        assert_eq!(saved["worktree"]["branch_prefix"], "feature/");
    }

    #[test]
    fn malformed_project_config_is_actionable_and_unchanged() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(".cst.json");
        fs::write(&path, "{invalid").unwrap();

        let error = ProjectSettings::load(temp.path(), &UserConfig::default())
            .unwrap_err()
            .to_string();
        assert!(error.contains("Invalid project settings"));
        assert_eq!(fs::read_to_string(path).unwrap(), "{invalid");
    }
}
