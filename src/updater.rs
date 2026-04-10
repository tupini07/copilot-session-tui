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

fn check_latest_version() -> Result<String> {
    // Check cache first
    if let Some(cached) = should_check() {
        return Ok(cached);
    }

    // Query GitHub API for latest release
    let url = format!(
        "https://api.github.com/repos/{}/{}/releases/latest",
        REPO_OWNER, REPO_NAME
    );

    let response: serde_json::Value = ureq::get(&url)
        .set("User-Agent", "copilot-session-tui")
        .timeout(Duration::from_secs(5))
        .call()
        .context("Failed to check for updates")?
        .into_json()
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

/// Spawn a background thread that checks for updates.
/// Returns a receiver that will get Some(UpdateInfo) if a newer version is available.
pub fn check_for_updates_async() -> mpsc::Receiver<Option<UpdateInfo>> {
    let (tx, rx) = mpsc::channel();
    let current = env!("CARGO_PKG_VERSION").to_string();

    thread::spawn(move || {
        let result = check_latest_version()
            .ok()
            .and_then(|latest| {
                let current_ver = semver::Version::parse(&current).ok()?;
                let latest_ver = semver::Version::parse(&latest).ok()?;
                if latest_ver > current_ver {
                    Some(UpdateInfo {
                        latest_version: latest,
                        current_version: current,
                    })
                } else {
                    None
                }
            });
        let _ = tx.send(result);
    });

    rx
}

/// Perform the actual self-update. Call this AFTER terminal is restored.
pub fn perform_update() -> Result<()> {
    let status = self_update::backends::github::Update::configure()
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .bin_name("copilot-session-tui")
        .show_download_progress(true)
        .current_version(self_update::cargo_crate_version!())
        .build()?
        .update()?;

    println!("Updated to version {}!", status.version());
    println!("Please restart the application.");
    Ok(())
}
