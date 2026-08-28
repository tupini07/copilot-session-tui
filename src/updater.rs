#![cfg_attr(test, allow(dead_code))]

use anyhow::{Context, Result};
use fs4::{FileExt, TryLockError};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const REPO_OWNER: &str = "tupini07";
const REPO_NAME: &str = "copilot-session-tui";
const CHECK_INTERVAL_HOURS: i64 = 12;

#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub latest_version: String,
    pub current_version: String,
}

pub type UpdateCheckResult = std::result::Result<Option<UpdateInfo>, String>;
pub type UpdateInstallResult = std::result::Result<UpdateInstallOutcome, String>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpdateInstallOutcome {
    Installed(String),
    AlreadyInstalled(String),
}

#[derive(Debug, Serialize, Deserialize)]
struct UpdateCache {
    last_checked: String,
    latest_version: String,
}

fn cache_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".copilot")
        .join("session-tui-update-cache.json")
}

fn read_cache() -> Option<UpdateCache> {
    let path = cache_path();
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

fn write_cache(cache: &UpdateCache) {
    let path = cache_path();
    if let Ok(json) = serde_json::to_string(cache) {
        let _ = std::fs::write(&path, json);
    }
}

fn should_check() -> Option<String> {
    let cache = read_cache()?;
    let last_checked = chrono::DateTime::parse_from_rfc3339(&cache.last_checked).ok()?;
    let elapsed = chrono::Utc::now().signed_duration_since(last_checked);
    if elapsed.num_hours() < CHECK_INTERVAL_HOURS {
        // Return cached version without hitting the network
        Some(cache.latest_version)
    } else {
        None
    }
}

fn check_latest_version(force: bool) -> Result<String> {
    if !force {
        if let Some(cached) = should_check() {
            return Ok(cached);
        }
    }

    // Query GitHub even when the normal startup cache is still fresh if the user
    // explicitly asks to update.
    let url = format!(
        "https://api.github.com/repos/{}/{}/releases/latest",
        REPO_OWNER, REPO_NAME
    );

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .build()
        .into();

    let response: serde_json::Value = agent
        .get(&url)
        .header("User-Agent", "copilot-session-tui")
        .call()
        .context("Failed to check for updates")?
        .into_body()
        .read_json()
        .context("Failed to parse update response")?;

    let tag = response["tag_name"]
        .as_str()
        .context("No tag_name in response")?;

    let version = tag.strip_prefix('v').unwrap_or(tag).to_string();

    // Update cache
    write_cache(&UpdateCache {
        last_checked: chrono::Utc::now().to_rfc3339(),
        latest_version: version.clone(),
    });

    Ok(version)
}

fn compare_versions(current: String, latest: String) -> Result<Option<UpdateInfo>> {
    let current_ver = semver::Version::parse(&current)
        .with_context(|| format!("Invalid current version: {current}"))?;
    let latest_ver = semver::Version::parse(&latest)
        .with_context(|| format!("Invalid latest version: {latest}"))?;
    Ok((latest_ver > current_ver).then_some(UpdateInfo {
        latest_version: latest,
        current_version: current,
    }))
}

fn spawn_update_check(force: bool) -> mpsc::Receiver<UpdateCheckResult> {
    let (tx, rx) = mpsc::channel();
    let current = env!("CARGO_PKG_VERSION").to_string();

    thread::spawn(move || {
        let result = check_latest_version(force)
            .and_then(|latest| compare_versions(current, latest))
            .map_err(|error| error.to_string());
        let _ = tx.send(result);
    });

    rx
}

/// Check using the normal startup cache to avoid an API request on every launch.
pub fn check_for_updates_async() -> mpsc::Receiver<UpdateCheckResult> {
    spawn_update_check(false)
}

/// Bypass the startup cache after an explicit user request.
pub fn force_check_for_updates_async() -> mpsc::Receiver<UpdateCheckResult> {
    spawn_update_check(true)
}

/// Install an update without stopping the current CST process or any panes it owns.
pub fn install_update_async(latest_version: String) -> mpsc::Receiver<UpdateInstallResult> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = run_update_helper(&latest_version).map_err(|error| error.to_string());
        let _ = sender.send(result);
    });
    receiver
}

/// Entry point used by the short-lived updater helper process.
pub fn install_update_helper(latest_version: &str) -> Result<()> {
    let outcome = install_update(latest_version)?;
    println!("{}", serde_json::to_string(&outcome)?);
    Ok(())
}

fn run_update_helper(latest_version: &str) -> Result<UpdateInstallOutcome> {
    let executable = invocation_executable()?;
    let output = std::process::Command::new(&executable)
        .arg("--install-update-helper")
        .arg(latest_version)
        .output()
        .with_context(|| format!("Failed to start update helper {}", executable.display()))?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "Update helper failed{}",
            if error.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", error.trim())
            }
        );
    }
    serde_json::from_slice(&output.stdout).context("Update helper returned an invalid result")
}

fn install_update(latest_version: &str) -> Result<UpdateInstallOutcome> {
    let _lock = UpdateLock::acquire()?;
    let installed = semver::Version::parse(env!("CARGO_PKG_VERSION"))?;
    let latest = semver::Version::parse(latest_version)?;
    if installed >= latest {
        return Ok(UpdateInstallOutcome::AlreadyInstalled(
            latest_version.to_string(),
        ));
    }
    let version = perform_update_with_progress(false, latest_version)?;
    if semver::Version::parse(&version)? < latest {
        anyhow::bail!("Updater installed v{version}, older than requested v{latest_version}");
    }
    Ok(UpdateInstallOutcome::Installed(version))
}

pub(crate) fn invocation_executable() -> Result<PathBuf> {
    let argument = std::env::args_os()
        .next()
        .context("The CST executable path is unavailable")?;
    let path = PathBuf::from(argument);
    if path.is_absolute() {
        Ok(path)
    } else if path.components().count() > 1 {
        Ok(std::env::current_dir()?.join(path))
    } else {
        find_on_path(&path)
            .or_else(|| std::env::current_exe().ok())
            .context("Could not resolve the CST executable")
    }
}

fn find_on_path(name: &std::path::Path) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        if candidate.extension().is_none() {
            let executable = candidate.with_extension("exe");
            if executable.is_file() {
                return Some(executable);
            }
        }
    }
    None
}

/// Best-effort cleanup for Windows executables relocated by a prior live update.
///
/// Removal fails harmlessly while any old CST process still maps the file; a later
/// startup retries after those sessions have naturally ended.
pub fn cleanup_old_update_files() {
    #[cfg(windows)]
    if let Ok(executable) = invocation_executable() {
        if let (Some(directory), Some(stem)) = (
            executable.parent(),
            executable.file_stem().and_then(|stem| stem.to_str()),
        ) {
            cleanup_old_update_files_in(directory, stem);
        }
    }
}

#[cfg(any(windows, test))]
fn cleanup_old_update_files_in(directory: &std::path::Path, stem: &str) {
    let prefix = format!(".{stem}.");
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(random) = name
            .strip_prefix(&prefix)
            .and_then(|name| name.strip_suffix(".__relocated__.exe"))
        else {
            continue;
        };
        if random.len() == 32 && random.bytes().all(|byte| byte.is_ascii_lowercase()) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[derive(Debug)]
struct UpdateLock {
    file: std::fs::File,
}

impl UpdateLock {
    fn acquire() -> Result<Self> {
        let path = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".copilot")
            .join("session-tui-update.lock");
        Self::acquire_in(&path)
    }

    fn acquire_in(path: &std::path::Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| format!("Failed to open update lock {}", path.display()))?;
        match FileExt::try_lock(&file) {
            Ok(()) => Ok(Self { file }),
            Err(TryLockError::WouldBlock) => {
                anyhow::bail!("Another CST instance is already installing the update")
            }
            Err(TryLockError::Error(error)) => {
                Err(error).with_context(|| format!("Failed to lock {}", path.display()))
            }
        }
    }
}

impl Drop for UpdateLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

fn perform_update_with_progress(show_progress: bool, target_version: &str) -> Result<String> {
    let target_tag = format!("v{target_version}");
    let status = self_update::backends::github::Update::configure()
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .bin_name("copilot-session-tui")
        .show_download_progress(show_progress)
        .show_output(show_progress)
        .no_confirm(true) // user already confirmed via TUI
        .current_version(self_update::cargo_crate_version!())
        .target_version_tag(&target_tag)
        .build()?
        .update()?;

    if show_progress {
        println!("Updated to version {}!", status.version());
    }
    Ok(status.version().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_minor_versions_semantically() {
        let update = compare_versions("0.9.0".to_string(), "0.10.0".to_string())
            .unwrap()
            .unwrap();

        assert_eq!(update.current_version, "0.9.0");
        assert_eq!(update.latest_version, "0.10.0");
        assert!(compare_versions("0.10.0".to_string(), "0.10.0".to_string())
            .unwrap()
            .is_none());
    }

    #[test]
    fn install_outcome_has_a_stable_helper_protocol() {
        let outcome = UpdateInstallOutcome::Installed("0.19.0".to_string());
        let encoded = serde_json::to_vec(&outcome).unwrap();
        assert_eq!(
            serde_json::from_slice::<UpdateInstallOutcome>(&encoded).unwrap(),
            outcome
        );
    }

    #[test]
    fn update_lock_prevents_concurrent_installers() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("update.lock");
        let first = UpdateLock::acquire_in(&path).unwrap();

        let error = UpdateLock::acquire_in(&path).unwrap_err().to_string();

        assert!(error.contains("already installing"), "got {error}");
        drop(first);
        UpdateLock::acquire_in(&path).unwrap();
    }

    #[test]
    fn startup_cleanup_only_removes_self_replace_relocated_files() {
        let temp = tempfile::tempdir().unwrap();
        let relocated = temp.path().join(format!(
            ".copilot-session-tui.{}.__relocated__.exe",
            "a".repeat(32)
        ));
        let unrelated = temp.path().join(".other.aaaaaaaa.__relocated__.exe");
        let malformed = temp
            .path()
            .join(".copilot-session-tui.short.__relocated__.exe");
        std::fs::write(&relocated, "old").unwrap();
        std::fs::write(&unrelated, "keep").unwrap();
        std::fs::write(&malformed, "keep").unwrap();

        cleanup_old_update_files_in(temp.path(), "copilot-session-tui");

        assert!(!relocated.exists());
        assert!(unrelated.exists());
        assert!(malformed.exists());
    }
}
