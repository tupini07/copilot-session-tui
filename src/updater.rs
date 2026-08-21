use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
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

/// Perform the actual self-update. Call this AFTER terminal is restored.
/// Download and install the newest release, returning its version.
pub fn perform_update() -> Result<String> {
    let status = self_update::backends::github::Update::configure()
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .bin_name("copilot-session-tui")
        .show_download_progress(true)
        .no_confirm(true) // user already confirmed via TUI
        .current_version(self_update::cargo_crate_version!())
        .build()?
        .update()?;

    println!("Updated to version {}!", status.version());
    Ok(status.version().to_string())
}

/// Start the freshly installed binary in place of this one.
///
/// Restarting by hand is awkward when CST is the terminal's root process: quitting
/// takes the window with it, so there is nothing left to type the command into.
/// Handing the terminal straight to the new build avoids that entirely.
///
/// `executable` must be captured *before* updating. The installer replaces the running
/// binary by renaming it out of the way, and Windows reports the moved file's new path,
/// so asking afterwards can point back at the old build.
///
/// On Unix the process image is replaced, so this only returns on failure. Windows has
/// no equivalent, so the old process stays on as a thin parent and forwards the exit
/// status; something has to remain attached to the console or the window would close.
pub fn relaunch(executable: &std::path::Path) -> Result<std::process::ExitStatus> {
    // The same arguments, so a restart lands wherever the original launch did.
    let arguments: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let mut command = std::process::Command::new(executable);
    command.args(&arguments);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(anyhow::Error::new(command.exec())
            .context("Could not start the updated copilot-session-tui"))
    }

    #[cfg(not(unix))]
    {
        command
            .status()
            .context("Could not start the updated copilot-session-tui")
    }
}

/// Mirrors the `#[cfg(unix)]` arm of [`relaunch`] so it is type-checked on Windows too.
#[cfg(test)]
fn relaunch_unix_shape(error: std::io::Error) -> Result<std::process::ExitStatus> {
    Err(anyhow::Error::new(error).context("Could not start the updated copilot-session-tui"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_relaunch_explains_itself() {
        // `CommandExt::exec` hands back a bare io::Error, and this is the shape the
        // Unix arm wraps it in. Compiling it here keeps that arm honest on Windows.
        let error = relaunch_unix_shape(std::io::Error::other("no such file"))
            .expect_err("relaunch cannot succeed here");

        assert!(format!("{error:#}").contains("Could not start the updated copilot-session-tui"));
        assert!(format!("{error:#}").contains("no such file"));
    }

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
}
