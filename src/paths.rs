//! The two roots everything CST writes for itself lives under.
//!
//! `CST_HOME` moves both, so a CST started for a test cannot touch the user's own
//! settings or state. Nothing else could: `dirs` reads these from the system on
//! Windows and ignores environment variables, and every running CST reloads the
//! config file within a second of it changing, so a test that saved anything
//! changed the user's live setup.
//!
//! Copilot's own files are not under these and keep their own override,
//! `COPILOT_HOME`.

use std::ffi::OsString;
use std::path::PathBuf;

pub const HOME_VAR: &str = "CST_HOME";

/// Where the settings file and its neighbours live.
pub fn config_dir() -> PathBuf {
    config_dir_with(std::env::var_os(HOME_VAR))
}

/// Where CST keeps its state: recovery records, scratchpads, the worktree registry,
/// and by default the worktrees themselves.
pub fn data_dir() -> PathBuf {
    data_dir_with(std::env::var_os(HOME_VAR))
}

fn override_root(value: Option<OsString>) -> Option<PathBuf> {
    // Set but empty is how a shell spells "unset" more often than not.
    value.filter(|value| !value.is_empty()).map(PathBuf::from)
}

fn config_dir_with(home: Option<OsString>) -> PathBuf {
    if let Some(root) = override_root(home) {
        return root.join("config");
    }
    dirs::config_dir().unwrap_or_else(|| user_home().join(".config"))
}

fn data_dir_with(home: Option<OsString>) -> PathBuf {
    if let Some(root) = override_root(home) {
        return root.join("data");
    }
    dirs::data_local_dir().unwrap_or_else(|| user_home().join(".local").join("share"))
}

fn user_home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cst_home_moves_config_and_state_into_separate_folders_under_it() {
        let home = Some(OsString::from("/tmp/run"));

        assert_eq!(
            config_dir_with(home.clone()),
            PathBuf::from("/tmp/run/config")
        );
        assert_eq!(data_dir_with(home), PathBuf::from("/tmp/run/data"));
    }

    #[test]
    fn nothing_else_asks_dirs_for_these_folders_so_cst_home_cannot_be_bypassed() {
        fn visit(dir: &std::path::Path, offenders: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir)
                .expect("readable source tree")
                .flatten()
            {
                let path = entry.path();
                if path.is_dir() {
                    visit(&path, offenders);
                } else if path.extension().is_some_and(|ext| ext == "rs")
                    && !path.ends_with("paths.rs")
                {
                    let text = std::fs::read_to_string(&path).unwrap_or_default();
                    if text.contains("dirs::config_dir") || text.contains("dirs::data_local_dir") {
                        offenders.push(path.display().to_string());
                    }
                }
            }
        }
        let mut offenders = Vec::new();
        visit(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut offenders,
        );

        assert!(
            offenders.is_empty(),
            "use crate::paths instead, or CST_HOME stops isolating test runs: {offenders:?}"
        );
    }

    #[test]
    fn an_empty_cst_home_counts_as_unset_rather_than_the_current_directory() {
        assert_eq!(
            config_dir_with(Some(OsString::new())),
            config_dir_with(None)
        );
        assert_eq!(data_dir_with(Some(OsString::new())), data_dir_with(None));
    }
}
