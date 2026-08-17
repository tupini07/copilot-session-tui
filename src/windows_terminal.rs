use crate::config::UserConfig;
use crate::session::Session;
#[cfg(windows)]
use anyhow::Context;
use anyhow::Result;
#[cfg(any(windows, test))]
use std::collections::HashSet;
#[cfg(any(windows, test))]
use std::ffi::OsString;
use std::path::Path;
#[cfg(any(windows, test))]
use std::path::PathBuf;

#[cfg(any(windows, test))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct FavoriteTab {
    session_id: String,
    title: String,
    cwd: Option<PathBuf>,
}

#[cfg(any(windows, test))]
#[derive(Debug, Default, PartialEq, Eq)]
struct FavoriteLaunchPlan {
    tabs: Vec<FavoriteTab>,
    active: Vec<String>,
    stale: Vec<String>,
}

pub fn open_favorites(
    sessions: &[Session],
    config: &UserConfig,
    copilot_home: &Path,
    mux_override: Option<bool>,
) -> Result<()> {
    #[cfg(not(windows))]
    {
        let _ = (sessions, config, copilot_home, mux_override);
        anyhow::bail!("Opening favorite tabs is currently supported only on Windows");
    }

    #[cfg(windows)]
    {
        open_favorites_windows(sessions, config, copilot_home, mux_override)
    }
}

#[cfg(windows)]
fn open_favorites_windows(
    sessions: &[Session],
    config: &UserConfig,
    copilot_home: &Path,
    mux_override: Option<bool>,
) -> Result<()> {
    use std::io::ErrorKind;
    use std::process::Command;

    let plan = build_launch_plan(sessions, config);
    report_skipped(&plan);

    if config.favorites.is_empty() {
        println!("No favorite sessions configured.");
        return Ok(());
    }
    if plan.tabs.is_empty() {
        println!("No inactive favorite sessions to open.");
        return Ok(());
    }

    let current_exe =
        std::env::current_exe().context("Could not resolve the current CST executable")?;
    let mut failed = Vec::new();
    let requested = plan.tabs.len();

    for tab in plan.tabs {
        let args = windows_terminal_args(&tab, &current_exe, copilot_home, mux_override);
        match Command::new("wt.exe").args(&args).status() {
            Ok(status) if status.success() => {}
            Ok(status) => failed.push(format!(
                "'{}' (Windows Terminal exited with {})",
                tab.title, status
            )),
            Err(error) if error.kind() == ErrorKind::NotFound => {
                anyhow::bail!(
                    "Windows Terminal (wt.exe) was not found. Install Windows Terminal or enable its app execution alias."
                );
            }
            Err(error) => failed.push(format!("'{}' ({error})", tab.title)),
        }
    }

    let launched = requested - failed.len();
    println!("Opened {launched} favorite session tab(s).");
    if failed.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(
            "Failed to open {} tab(s): {}",
            failed.len(),
            failed.join(", ")
        )
    }
}

#[cfg(any(windows, test))]
fn build_launch_plan(sessions: &[Session], config: &UserConfig) -> FavoriteLaunchPlan {
    let mut plan = FavoriteLaunchPlan::default();
    let mut resolved = HashSet::new();

    for session in sessions {
        if !config.favorites.contains(&session.id) {
            continue;
        }
        resolved.insert(session.id.as_str());
        if session.is_active {
            plan.active.push(session.display_name().to_string());
        } else {
            let cwd = PathBuf::from(&session.cwd);
            plan.tabs.push(FavoriteTab {
                session_id: session.id.clone(),
                title: session.display_name().to_string(),
                cwd: cwd.is_dir().then_some(cwd),
            });
        }
    }

    plan.stale.extend(
        config
            .favorites
            .iter()
            .filter(|id| !resolved.contains(id.as_str()))
            .cloned(),
    );
    plan
}

#[cfg(windows)]
fn report_skipped(plan: &FavoriteLaunchPlan) {
    if !plan.active.is_empty() {
        eprintln!(
            "Skipped {} active favorite(s): {}",
            plan.active.len(),
            plan.active.join(", ")
        );
    }
    if !plan.stale.is_empty() {
        eprintln!(
            "Skipped {} missing favorite ID(s): {}",
            plan.stale.len(),
            plan.stale.join(", ")
        );
    }
}

#[cfg(any(windows, test))]
fn windows_terminal_args(
    tab: &FavoriteTab,
    current_exe: &Path,
    copilot_home: &Path,
    mux_override: Option<bool>,
) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("-w"),
        OsString::from("0"),
        OsString::from("new-tab"),
        OsString::from("--title"),
        OsString::from(&tab.title),
        OsString::from("--suppressApplicationTitle"),
    ];
    if let Some(cwd) = &tab.cwd {
        args.push(OsString::from("--startingDirectory"));
        args.push(cwd.as_os_str().to_owned());
    }
    args.push(current_exe.as_os_str().to_owned());
    args.push(OsString::from("--copilot-home"));
    args.push(copilot_home.as_os_str().to_owned());
    args.push(OsString::from("--session"));
    args.push(OsString::from(&tab.session_id));
    match mux_override {
        Some(true) => args.push(OsString::from("--mux")),
        Some(false) => args.push(OsString::from("--no-mux")),
        None => {}
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn session(id: &str, name: &str, updated: i64, active: bool, cwd: &Path) -> Session {
        Session {
            id: id.to_string(),
            cwd: cwd.to_string_lossy().to_string(),
            project_root: cwd.to_string_lossy().to_string(),
            summary: Some(name.to_string()),
            created_at: None,
            updated_at: Some(Utc.timestamp_opt(updated, 0).unwrap()),
            is_active: active,
            dir_path: PathBuf::new(),
            edited_files: Vec::new(),
            last_user_message: None,
            turn_count: 0,
            tool_call_count: 0,
        }
    }

    #[test]
    fn launch_plan_preserves_loader_order_and_partitions_skipped_favorites() {
        let cwd = tempfile::tempdir().unwrap();
        let sessions = vec![
            session("newest", "Newest", 3, false, cwd.path()),
            session("active", "Already open", 2, true, cwd.path()),
            session("oldest", "Oldest", 1, false, cwd.path()),
            session("ordinary", "Not favorite", 0, false, cwd.path()),
        ];
        let mut config = UserConfig::default();
        for id in ["newest", "active", "oldest", "missing"] {
            config.favorites.insert(id.to_string());
        }

        let plan = build_launch_plan(&sessions, &config);

        assert_eq!(
            plan.tabs
                .iter()
                .map(|tab| tab.session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["newest", "oldest"]
        );
        assert_eq!(plan.active, vec!["Already open"]);
        assert_eq!(plan.stale, vec!["missing"]);
    }

    #[test]
    fn missing_working_directory_is_not_sent_to_windows_terminal() {
        let sessions = vec![session(
            "favorite",
            "Favorite",
            1,
            false,
            Path::new(r"Z:\missing\directory"),
        )];
        let mut config = UserConfig::default();
        config.favorites.insert("favorite".to_string());

        let plan = build_launch_plan(&sessions, &config);

        assert_eq!(plan.tabs.len(), 1);
        assert_eq!(plan.tabs[0].cwd, None);
    }

    #[test]
    fn terminal_arguments_keep_paths_titles_and_overrides_separate() {
        let tab = FavoriteTab {
            session_id: "session;42".to_string(),
            title: "Map parser; review".to_string(),
            cwd: Some(PathBuf::from(r"C:\work trees\map parser")),
        };

        let args = windows_terminal_args(
            &tab,
            Path::new(r"C:\Program Files\CST\copilot-session-tui.exe"),
            Path::new(r"C:\Users\Test User\.copilot"),
            Some(false),
        );

        assert_eq!(
            args,
            vec![
                "-w",
                "0",
                "new-tab",
                "--title",
                "Map parser; review",
                "--suppressApplicationTitle",
                "--startingDirectory",
                r"C:\work trees\map parser",
                r"C:\Program Files\CST\copilot-session-tui.exe",
                "--copilot-home",
                r"C:\Users\Test User\.copilot",
                "--session",
                "session;42",
                "--no-mux",
            ]
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>()
        );
    }
}
