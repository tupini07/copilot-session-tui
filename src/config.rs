use anyhow::{Context, Result};
use fs4::{FileExt, TryLockError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const DEFAULT_BRANCH_PREFIX: &str = "copilot/";
pub const REASONING_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh"];
/// Plain control bytes like Ctrl-b travel reliably through every terminal we target,
/// unlike chords that depend on the keyboard-enhancement protocol.
pub const DEFAULT_MUX_PREFIX: &str = "C-b";
pub const DEFAULT_NTFY_SERVER: &str = "https://ntfy.sh";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptSnippet {
    pub name: String,
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_ntfy_server")]
    pub server: String,
    #[serde(default)]
    pub topic: String,
    #[serde(default = "default_true")]
    pub ready: bool,
    #[serde(default = "default_true")]
    pub error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserConfig {
    #[serde(default)]
    pub yolo: bool,

    /// Starred sessions, in the order the user arranged them.
    ///
    /// Deliberately a list rather than a set: the order is the feature, and it drives
    /// both the list grouping and the order favorite tabs open in. On disk this is a
    /// JSON array exactly as the previous set was, so older configs load unchanged.
    #[serde(
        default,
        deserialize_with = "deduplicated",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub favorites: Vec<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub snippets: Vec<PromptSnippet>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,

    /// Run sessions inside CST as multiplexed panes instead of launching and exiting.
    #[serde(default)]
    pub mux: bool,

    /// Key that escapes back to CST while a pane is focused.
    #[serde(default = "default_mux_prefix")]
    pub mux_prefix: String,

    #[serde(default)]
    pub worktree: WorktreeConfig,

    #[serde(default)]
    pub terminal: TerminalConfig,

    #[serde(default)]
    pub notifications: NotificationConfig,

    /// Kept at the root so older CST versions preserve it through `extra` when saving.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ntfy_access_token: String,

    /// Kept at the root so older CST versions preserve it through `extra` when saving.
    #[serde(default)]
    pub ntfy_verbose: bool,

    #[serde(flatten)]
    pub(crate) extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TerminalConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
}

fn default_mux_prefix() -> String {
    DEFAULT_MUX_PREFIX.to_string()
}

fn default_ntfy_server() -> String {
    DEFAULT_NTFY_SERVER.to_string()
}

const fn default_true() -> bool {
    true
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            server: default_ntfy_server(),
            topic: String::new(),
            ready: true,
            error: true,
        }
    }
}

/// Drop repeated favorites while preserving first-seen order.
///
/// The file is hand-editable and a duplicate would otherwise show the same session
/// twice in the list and open it twice as a tab.
fn deduplicated<'de, D>(deserializer: D) -> std::result::Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let mut seen = BTreeSet::new();
    Ok(Vec::<String>::deserialize(deserializer)?
        .into_iter()
        .filter(|id| seen.insert(id.clone()))
        .collect())
}

impl Default for UserConfig {
    fn default() -> Self {
        Self {
            yolo: false,
            favorites: Vec::new(),
            snippets: Vec::new(),
            model: None,
            reasoning_effort: None,
            mux: false,
            mux_prefix: default_mux_prefix(),
            worktree: WorktreeConfig::default(),
            terminal: TerminalConfig::default(),
            notifications: NotificationConfig::default(),
            ntfy_access_token: String::new(),
            ntfy_verbose: false,
            extra: BTreeMap::new(),
        }
    }
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

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub snippets: Vec<PromptSnippet>,

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

    pub fn snippets(&self) -> &[PromptSnippet] {
        &self.config.snippets
    }

    pub fn set_snippets(&mut self, snippets: Vec<PromptSnippet>) {
        self.config.snippets = snippets;
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
        self.worktree.is_none() && self.snippets.is_empty() && self.extra.is_empty()
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

fn global_snippets_path() -> PathBuf {
    config_path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("snippets.json")
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
    let config_path = config_path();
    let snippets_path = global_snippets_path();
    let mut config = load_base_config(&config_path).unwrap_or_default();
    // A damaged sidecar must not make unrelated settings/favorites fall back to
    // defaults and later overwrite a valid config.json.
    let _ = overlay_global_snippets(&mut config, &snippets_path);
    config
}

pub fn load_checked() -> Result<UserConfig> {
    load_checked_in(&config_path(), &global_snippets_path())
}

fn load_checked_in(config_path: &Path, snippets_path: &Path) -> Result<UserConfig> {
    let mut config = load_base_config(config_path)?;
    overlay_global_snippets(&mut config, snippets_path)?;
    Ok(config)
}

fn load_base_config(config_path: &Path) -> Result<UserConfig> {
    match fs::read_to_string(config_path) {
        Ok(content) => serde_json::from_str(&content)
            .with_context(|| format!("Invalid global settings in {}", config_path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(UserConfig::default()),
        Err(error) => Err(error)
            .with_context(|| format!("Failed to read global settings: {}", config_path.display())),
    }
}

fn overlay_global_snippets(config: &mut UserConfig, snippets_path: &Path) -> Result<()> {
    let lock_path = snippets_path.with_extension("lock");
    let _lock = SnippetLock::acquire(&lock_path)?;
    overlay_global_snippets_locked(config, snippets_path)
}

fn overlay_global_snippets_locked(config: &mut UserConfig, snippets_path: &Path) -> Result<()> {
    match fs::read_to_string(snippets_path) {
        Ok(content) => {
            config.snippets = serde_json::from_str(&content).with_context(|| {
                format!("Invalid global snippets in {}", snippets_path.display())
            })?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // One-time migration from the original config.json field. The sidecar is
            // authoritative afterward, so a long-running pre-snippet CST can rewrite
            // config.json without erasing prompts.
            if !config.snippets.is_empty() {
                write_json_atomic(snippets_path, &config.snippets)?;
            }
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "Failed to read global snippets: {}",
                    snippets_path.display()
                )
            });
        }
    }
    Ok(())
}

pub fn save(config: &UserConfig) -> Result<()> {
    write_json_atomic(&config_path(), config)
}

pub fn validate_ntfy_topic(topic: &str) -> Result<()> {
    if topic.is_empty() {
        anyhow::bail!("ntfy topic is required when notifications are enabled");
    }
    if topic.len() > 64
        || !topic
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        anyhow::bail!("ntfy topic must be 1-64 letters, numbers, dashes, or underscores");
    }
    Ok(())
}

pub fn validate_ntfy_access_token(token: &str) -> Result<()> {
    if token.len() > 512
        || token.chars().any(|character| {
            character.is_control() || character.is_whitespace() || !character.is_ascii()
        })
    {
        anyhow::bail!("ntfy access token must be printable ASCII without spaces");
    }
    Ok(())
}

pub fn normalize_ntfy_server(server: &str) -> Result<String> {
    let server = server.trim().trim_end_matches('/');
    let uri: ureq::http::Uri = server
        .parse()
        .map_err(|_| anyhow::anyhow!("ntfy server is not a valid URL"))?;
    if !matches!(uri.scheme_str(), Some("http" | "https")) {
        anyhow::bail!("ntfy server must start with http:// or https://");
    }
    if uri.host().is_none_or(str::is_empty)
        || uri
            .authority()
            .is_none_or(|authority| !valid_http_authority(authority.as_str()))
        || !matches!(uri.path(), "" | "/")
        || uri.query().is_some()
        || server.contains('#')
    {
        anyhow::bail!("ntfy server must contain a valid host and no path, query, or fragment");
    }
    Ok(server.to_string())
}

fn valid_http_authority(authority: &str) -> bool {
    if authority.is_empty() || authority.contains([' ', '@']) {
        return false;
    }
    if let Some(rest) = authority.strip_prefix('[') {
        let Some(end) = rest.find(']') else {
            return false;
        };
        if end == 0 {
            return false;
        }
        let suffix = &rest[end + 1..];
        return suffix.is_empty() || suffix.strip_prefix(':').is_some_and(valid_port);
    }
    if authority.matches(':').count() > 1 {
        return false;
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => !host.is_empty() && valid_port(port),
        None => true,
    }
}

fn valid_port(port: &str) -> bool {
    !port.is_empty() && port.parse::<u16>().is_ok_and(|port| port > 0)
}

pub fn validate_notification_config(config: &NotificationConfig) -> Result<()> {
    if !config.enabled {
        return Ok(());
    }
    let _ = normalize_ntfy_server(&config.server)?;
    validate_ntfy_topic(config.topic.trim())?;
    Ok(())
}

pub fn validate_user_notification_config(config: &UserConfig) -> Result<()> {
    validate_notification_config(&config.notifications)?;
    if config.notifications.enabled {
        validate_ntfy_access_token(config.ntfy_access_token.trim())?;
    }
    Ok(())
}

pub fn save_global_snippets_if_unchanged(
    original: &[PromptSnippet],
    snippets: &[PromptSnippet],
) -> Result<UserConfig> {
    let snippets_path = global_snippets_path();
    let lock_path = snippets_path.with_extension("lock");
    save_global_snippets_if_unchanged_in(
        &config_path(),
        &snippets_path,
        &lock_path,
        original,
        snippets,
    )
}

fn save_global_snippets_if_unchanged_in(
    config_path: &Path,
    snippets_path: &Path,
    lock_path: &Path,
    original: &[PromptSnippet],
    snippets: &[PromptSnippet],
) -> Result<UserConfig> {
    let _lock = SnippetLock::acquire(lock_path)?;
    let mut config = load_base_config(config_path)?;
    overlay_global_snippets_locked(&mut config, snippets_path)?;
    if config.snippets != original {
        anyhow::bail!("Global snippets changed on disk; close and reopen the snippet manager");
    }
    write_json_atomic(snippets_path, snippets)?;
    config.snippets = snippets.to_vec();
    Ok(config)
}

struct SnippetLock {
    file: fs::File,
}

impl SnippetLock {
    fn acquire(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| format!("Failed to open snippet lock {}", path.display()))?;
        match FileExt::try_lock(&file) {
            Ok(()) => Ok(Self { file }),
            Err(TryLockError::WouldBlock) => {
                anyhow::bail!("Another CST instance is saving global snippets")
            }
            Err(TryLockError::Error(error)) => {
                Err(error).with_context(|| format!("Failed to lock {}", path.display()))
            }
        }
    }
}

impl Drop for SnippetLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
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

fn write_json_atomic<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<()> {
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
        assert!(config.favorites.is_empty());
        assert_eq!(config.worktree.branch_prefix, DEFAULT_BRANCH_PREFIX);
        assert_eq!(config.worktree.root, default_worktree_root());
        assert!(config.terminal.shell.is_none());
    }

    #[test]
    fn favorites_round_trip_in_global_config() {
        let mut config = UserConfig::default();
        config.favorites.push("session-b".to_string());
        config.favorites.push("session-a".to_string());

        let json = serde_json::to_string(&config).unwrap();
        let loaded: UserConfig = serde_json::from_str(&json).unwrap();

        // Order is the point, so round-tripping must not sort them.
        assert_eq!(loaded.favorites, vec!["session-b", "session-a"]);
    }

    #[test]
    fn favorites_written_by_older_versions_still_load() {
        // Older releases stored a set, which serialised to the same JSON array.
        let loaded: UserConfig =
            serde_json::from_str(r#"{"favorites":["session-a","session-b"]}"#).unwrap();

        assert_eq!(loaded.favorites, vec!["session-a", "session-b"]);
    }

    #[test]
    fn hand_edited_duplicate_favorites_are_dropped() {
        let loaded: UserConfig =
            serde_json::from_str(r#"{"favorites":["a","b","a","b","c"]}"#).unwrap();

        assert_eq!(loaded.favorites, vec!["a", "b", "c"]);
    }

    #[test]
    fn terminal_shell_round_trips_in_global_config() {
        let mut config = UserConfig::default();
        config.terminal.shell = Some("pwsh".to_string());

        let json = serde_json::to_string(&config).unwrap();
        let loaded: UserConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.terminal.shell.as_deref(), Some("pwsh"));
    }

    #[test]
    fn global_config_round_trip_preserves_future_fields() {
        let json = r#"{"yolo":true,"future_setting":{"enabled":7}}"#;
        let loaded: UserConfig = serde_json::from_str(json).unwrap();
        let saved: Value = serde_json::to_value(loaded).unwrap();

        assert_eq!(saved["future_setting"]["enabled"], 7);
    }

    #[test]
    fn notification_defaults_are_disabled_and_private() {
        let config: UserConfig = serde_json::from_str("{}").unwrap();

        assert!(!config.notifications.enabled);
        assert_eq!(config.notifications.server, DEFAULT_NTFY_SERVER);
        assert!(config.notifications.topic.is_empty());
        assert!(config.ntfy_access_token.is_empty());
        assert!(!config.ntfy_verbose);
        assert!(config.notifications.ready);
        assert!(config.notifications.error);
    }

    #[test]
    fn older_root_flattening_preserves_new_ntfy_credentials() {
        #[derive(Serialize, Deserialize)]
        struct OlderConfig {
            #[serde(default)]
            notifications: NotificationConfig,
            #[serde(flatten)]
            extra: BTreeMap<String, Value>,
        }

        let current = UserConfig {
            ntfy_access_token: "tk_private_token".to_string(),
            ntfy_verbose: true,
            ..UserConfig::default()
        };
        let json = serde_json::to_string(&current).unwrap();
        let old: OlderConfig = serde_json::from_str(&json).unwrap();
        let rewritten = serde_json::to_string(&old).unwrap();
        let restored: UserConfig = serde_json::from_str(&rewritten).unwrap();

        assert_eq!(restored.ntfy_access_token, "tk_private_token");
        assert!(restored.ntfy_verbose);
    }

    #[test]
    fn ntfy_configuration_validates_server_and_topic() {
        let mut notifications = NotificationConfig {
            enabled: true,
            topic: "private_CST-topic_123".to_string(),
            server: "https://ntfy.example.test/".to_string(),
            ..NotificationConfig::default()
        };
        assert!(validate_notification_config(&notifications).is_ok());
        assert_eq!(
            normalize_ntfy_server(&notifications.server).unwrap(),
            "https://ntfy.example.test"
        );

        notifications.topic = "not/a/topic".to_string();
        assert!(validate_notification_config(&notifications).is_err());
        notifications.topic = "valid".to_string();
        notifications.server = "file:///tmp/ntfy".to_string();
        assert!(validate_notification_config(&notifications).is_err());
        for invalid in ["https://not a host", "https://:443", "https://host:bad"] {
            notifications.server = invalid.to_string();
            assert!(
                validate_notification_config(&notifications).is_err(),
                "accepted {invalid:?}"
            );
        }
        notifications.server = "https://ntfy.example.test/unintended-topic".to_string();
        assert!(
            validate_notification_config(&notifications).is_err(),
            "a server path would route JSON to the wrong ntfy topic"
        );

        notifications.enabled = false;
        notifications.server.clear();
        notifications.topic.clear();
        assert!(
            validate_notification_config(&notifications).is_ok(),
            "disabled integration must not block unrelated settings saves"
        );

        let mut config = UserConfig {
            notifications: NotificationConfig {
                enabled: true,
                server: "https://ntfy.example.test".to_string(),
                topic: "private_topic".to_string(),
                ..NotificationConfig::default()
            },
            ntfy_access_token: "token with spaces".to_string(),
            ..UserConfig::default()
        };
        assert!(validate_user_notification_config(&config).is_err());
        config.ntfy_access_token = "tk_valid-token_123".to_string();
        assert!(validate_user_notification_config(&config).is_ok());
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
    fn global_and_project_snippets_round_trip_without_losing_unknown_fields() {
        let global = UserConfig {
            snippets: vec![PromptSnippet {
                name: "Review".to_string(),
                prompt: "Review this carefully.".to_string(),
            }],
            ..UserConfig::default()
        };
        let json = serde_json::to_string(&global).unwrap();
        let loaded: UserConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.snippets, global.snippets);

        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join(".cst.json"), r#"{"future_setting":true}"#).unwrap();
        let mut settings = ProjectSettings::load(temp.path(), &global).unwrap();
        settings.set_snippets(vec![PromptSnippet {
            name: "Project plan".to_string(),
            prompt: "Use this repository's plan.".to_string(),
        }]);
        settings.save().unwrap();

        let reloaded = ProjectSettings::load(temp.path(), &global).unwrap();
        assert_eq!(reloaded.snippets()[0].name, "Project plan");
        let saved: Value =
            serde_json::from_str(&fs::read_to_string(temp.path().join(".cst.json")).unwrap())
                .unwrap();
        assert_eq!(saved["future_setting"], true);
    }

    #[test]
    fn legacy_global_snippets_migrate_to_an_authoritative_sidecar() {
        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("config.json");
        let snippets_path = temp.path().join("snippets.json");
        let snippet = PromptSnippet {
            name: "Quick check in".to_string(),
            prompt: "Anything else to work on?".to_string(),
        };
        fs::write(
            &config_path,
            serde_json::to_vec(&UserConfig {
                snippets: vec![snippet.clone()],
                ..UserConfig::default()
            })
            .unwrap(),
        )
        .unwrap();

        let migrated = load_checked_in(&config_path, &snippets_path).unwrap();
        assert_eq!(migrated.snippets, vec![snippet.clone()]);
        assert!(snippets_path.exists());

        // Simulate a long-running old CST rewriting config.json with no snippet field.
        fs::write(
            &config_path,
            serde_json::to_vec(&UserConfig::default()).unwrap(),
        )
        .unwrap();
        let protected = load_checked_in(&config_path, &snippets_path).unwrap();
        assert_eq!(protected.snippets, vec![snippet]);
    }

    #[test]
    fn sidecar_parse_errors_are_actionable_instead_of_silently_empty() {
        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("config.json");
        let snippets_path = temp.path().join("snippets.json");
        fs::write(&config_path, r#"{"yolo":true,"future_setting":7}"#).unwrap();
        fs::write(&snippets_path, "{invalid").unwrap();

        let error = load_checked_in(&config_path, &snippets_path)
            .unwrap_err()
            .to_string();

        assert!(error.contains("Invalid global snippets"));
        assert!(error.contains("snippets.json"));

        let preserved = load_base_config(&config_path).unwrap();
        assert!(preserved.yolo);
        assert_eq!(preserved.extra["future_setting"], 7);
    }

    #[test]
    fn sidecar_compare_and_write_rejects_a_stale_second_editor() {
        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("config.json");
        let snippets_path = temp.path().join("snippets.json");
        let lock_path = temp.path().join("snippets.lock");
        let original = PromptSnippet {
            name: "Original".to_string(),
            prompt: "old".to_string(),
        };
        let first_update = PromptSnippet {
            name: "Original".to_string(),
            prompt: "first".to_string(),
        };
        fs::write(
            &config_path,
            serde_json::to_vec(&UserConfig {
                yolo: true,
                snippets: vec![original.clone()],
                ..UserConfig::default()
            })
            .unwrap(),
        )
        .unwrap();
        write_json_atomic(&snippets_path, std::slice::from_ref(&original)).unwrap();

        let saved = save_global_snippets_if_unchanged_in(
            &config_path,
            &snippets_path,
            &lock_path,
            std::slice::from_ref(&original),
            std::slice::from_ref(&first_update),
        )
        .unwrap();
        assert!(saved.yolo, "fresh unrelated settings are retained");
        let compatibility: UserConfig =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(
            compatibility.snippets,
            vec![original.clone()],
            "sidecar edits never rewrite unrelated config.json snapshots"
        );

        let error = save_global_snippets_if_unchanged_in(
            &config_path,
            &snippets_path,
            &lock_path,
            std::slice::from_ref(&original),
            &[PromptSnippet {
                name: "Original".to_string(),
                prompt: "second".to_string(),
            }],
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("changed on disk"));
        let current: Vec<PromptSnippet> =
            serde_json::from_str(&fs::read_to_string(snippets_path).unwrap()).unwrap();
        assert_eq!(current, vec![first_update]);
    }

    #[test]
    fn migration_cannot_race_a_locked_snippet_writer() {
        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("config.json");
        let snippets_path = temp.path().join("snippets.json");
        let lock_path = temp.path().join("snippets.lock");
        let legacy = PromptSnippet {
            name: "Legacy".to_string(),
            prompt: "migrate me".to_string(),
        };
        fs::write(
            &config_path,
            serde_json::to_vec(&UserConfig {
                snippets: vec![legacy.clone()],
                ..UserConfig::default()
            })
            .unwrap(),
        )
        .unwrap();
        let held = SnippetLock::acquire(&lock_path).unwrap();

        let error = load_checked_in(&config_path, &snippets_path)
            .unwrap_err()
            .to_string();
        assert!(error.contains("saving global snippets"));
        assert!(
            !snippets_path.exists(),
            "an unlocked migrator must not write while the lock is held"
        );

        drop(held);
        assert_eq!(
            load_checked_in(&config_path, &snippets_path)
                .unwrap()
                .snippets,
            vec![legacy]
        );
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
