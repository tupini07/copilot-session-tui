//! Small machine-managed state that is not a user setting.
//!
//! Kept out of `config.json` on purpose: that file is hand-editable, and a
//! `ConfigWatcher` polls it every second, so writing a marker there at startup would
//! trip a spurious config reload. Kept out of the update cache too — that lives under
//! Copilot's `~/.copilot`, is semantically a deletable network cache, and its fields
//! carry no serde defaults, so a marker stored there would be silently lost the first
//! time an older cache failed to parse.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppState {
    /// Highest CST version that has opened a workspace on this machine.
    ///
    /// Empty on a fresh install, which means "show nothing" rather than "show every
    /// release ever".
    #[serde(default)]
    pub last_ran_version: String,

    /// Anything a newer CST wrote that this one does not know about, so downgrading
    /// does not silently drop it. Same approach as `UserConfig::extra`.
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

/// Read the marker, treating every failure as a fresh install.
///
/// Deliberately infallible: a corrupt marker must never stop CST from starting. The
/// worst outcome is one missed What's New screen.
pub(crate) fn load_in(root: &Path) -> AppState {
    fs::read_to_string(state_path(root))
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default()
}

/// Record `version` as the newest that has run, if it is.
///
/// Monotonic on purpose. Two CST versions can legitimately run side by side — a
/// long-lived old instance with panes open, and a freshly updated one in another
/// terminal. Without this the older instance would rewrite the marker backwards on its
/// next launch and the newer one would show its notes again. The cost is that
/// downgrading and re-upgrading never re-shows them, which is the calmer trade.
pub(crate) fn advance_last_ran_version_in(root: &Path, version: &str) -> Result<()> {
    let mut state = load_in(root);
    if !is_newer(version, &state.last_ran_version) {
        return Ok(());
    }
    state.last_ran_version = version.to_string();
    write_in(root, &state)
}

fn is_newer(candidate: &str, current: &str) -> bool {
    match (
        semver::Version::parse(candidate),
        semver::Version::parse(current),
    ) {
        (Ok(candidate), Ok(current)) => candidate > current,
        // No usable marker yet (fresh install, or something hand-edited into it).
        (Ok(_), Err(_)) => true,
        _ => false,
    }
}

pub(crate) fn write_in(root: &Path, state: &AppState) -> Result<()> {
    let path = state_path(root);
    fs::create_dir_all(root)
        .with_context(|| format!("Failed to create state directory: {}", root.display()))?;
    let content = serde_json::to_vec_pretty(state)?;
    let mut temp = tempfile::NamedTempFile::new_in(root)?;
    temp.as_file_mut().write_all(&content)?;
    temp.as_file_mut().sync_all()?;
    temp.persist(&path)
        .map_err(|error| error.error)
        .with_context(|| format!("Failed to replace app state: {}", path.display()))?;
    Ok(())
}

/// Sibling of `scratchpads/` rather than inside it: this is not per-session.
///
/// `copilot-session-tui/` rather than the other `cst/` root, which holds things the
/// user navigates into (worktrees). This is internal bookkeeping.
pub(crate) fn state_root() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local")
                .join("share")
        })
        .join("copilot-session-tui")
}

fn state_path(root: &Path) -> PathBuf {
    root.join("app-state.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_machine_has_no_version_recorded_rather_than_a_wrong_one() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(load_in(temp.path()), AppState::default());
        assert!(load_in(temp.path()).last_ran_version.is_empty());
    }

    #[test]
    fn the_marker_only_ever_moves_forward_so_two_cst_versions_can_run_side_by_side() {
        let temp = tempfile::tempdir().unwrap();
        advance_last_ran_version_in(temp.path(), "0.29.0").unwrap();
        assert_eq!(load_in(temp.path()).last_ran_version, "0.29.0");

        // An older instance launching afterwards must not drag it back, or the newer
        // one would replay its notes on every start.
        advance_last_ran_version_in(temp.path(), "0.28.0").unwrap();
        assert_eq!(load_in(temp.path()).last_ran_version, "0.29.0");

        advance_last_ran_version_in(temp.path(), "0.30.0").unwrap();
        assert_eq!(load_in(temp.path()).last_ran_version, "0.30.0");
    }

    #[test]
    fn a_corrupt_marker_is_treated_as_a_fresh_install_rather_than_failing_startup() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(state_path(temp.path()), "{not json").unwrap();

        assert_eq!(load_in(temp.path()), AppState::default());
        // And it is repairable by simply writing over it.
        advance_last_ran_version_in(temp.path(), "0.29.0").unwrap();
        assert_eq!(load_in(temp.path()).last_ran_version, "0.29.0");
    }

    #[test]
    fn a_field_written_by_a_newer_cst_survives_a_save_by_an_older_one() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path()).unwrap();
        fs::write(
            state_path(temp.path()),
            r#"{"last_ran_version":"0.29.0","something_newer":{"kept":true}}"#,
        )
        .unwrap();

        advance_last_ran_version_in(temp.path(), "0.30.0").unwrap();

        let written = fs::read_to_string(state_path(temp.path())).unwrap();
        assert!(written.contains("something_newer"), "got: {written}");
        assert!(written.contains("0.30.0"), "got: {written}");
    }
}
