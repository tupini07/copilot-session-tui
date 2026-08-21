use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use crate::config::{EffectiveWorktreeConfig, UserConfig};

use super::worktree::{self, ManagedWorktree};

fn apply_config_args(cmd: &mut Command, config: &UserConfig) {
    for arg in config_args(config) {
        cmd.arg(arg);
    }
}

/// Copilot CLI arguments implied by the user's config.
///
/// Shared by the launch-and-exit path (`std::process::Command`) and the multiplexer
/// path (`portable_pty::CommandBuilder`) so both modes launch identical sessions.
pub fn config_args(config: &UserConfig) -> Vec<String> {
    let mut args = Vec::new();
    if config.yolo {
        args.push("--yolo".to_string());
    }
    if let Some(ref model) = config.model {
        args.push(format!("--model={}", model));
    }
    if let Some(ref effort) = config.reasoning_effort {
        args.push(format!("--reasoning-effort={}", effort));
    }
    args
}

/// Program plus arguments for resuming an existing session inside a pane.
pub fn resume_command(session_id: &str, config: &UserConfig) -> Result<(String, Vec<String>)> {
    let copilot = find_copilot()?;
    let mut args = vec![format!("--resume={}", session_id)];
    args.extend(config_args(config));
    Ok((copilot, args))
}

/// Program plus arguments for starting a fresh session inside a pane, and the id that
/// session will have.
///
/// The id is ours to choose: `--session-id` names a new session rather than resuming one.
/// Deciding it up front is what lets a pane bind its scratchpad and terminal to the real
/// session from the moment it spawns, instead of waiting for Copilot to invent an id and
/// having nothing stable to key on in the meantime.
pub fn new_session_command(config: &UserConfig) -> Result<(String, Vec<String>, String)> {
    let copilot = find_copilot()?;
    let (args, session_id) = new_session_args(config);
    Ok((copilot, args, session_id))
}

/// The argument half of [`new_session_command`], split out so it can be tested without
/// a Copilot binary on PATH.
fn new_session_args(config: &UserConfig) -> (Vec<String>, String) {
    let session_id = uuid::Uuid::new_v4().to_string();
    let mut args = vec![format!("--session-id={session_id}")];
    args.extend(config_args(config));
    (args, session_id)
}

/// Rename a session using the current `name` field while preserving legacy metadata.
pub fn rename_session(session_dir: &Path, new_name: &str) -> Result<()> {
    let workspace_path = session_dir.join("workspace.yaml");
    let content = fs::read_to_string(&workspace_path)
        .with_context(|| format!("Failed to read {}", workspace_path.display()))?;

    let mut new_lines = Vec::new();
    let has_name = content.lines().any(|line| line.starts_with("name:"));
    let mut found_title = false;

    for line in content.lines() {
        if line.starts_with("name:") {
            new_lines.push(format!("name: {}", new_name));
            found_title = true;
        } else if !has_name && line.starts_with("summary:") && !line.starts_with("summary_count:") {
            new_lines.push(format!("summary: {}", new_name));
            found_title = true;
        } else {
            new_lines.push(line.to_string());
        }
    }

    if !found_title {
        // New Copilot CLI versions use `name`; older `summary` files remain
        // supported by the replacement path above.
        let mut inserted = Vec::new();
        for line in &new_lines {
            inserted.push(line.clone());
            if line.starts_with("id:") {
                inserted.push(format!("name: {}", new_name));
            }
        }
        new_lines = inserted;
    }

    let new_content = new_lines.join("\n") + "\n";
    fs::write(&workspace_path, new_content)
        .with_context(|| format!("Failed to write {}", workspace_path.display()))?;

    Ok(())
}

/// Delete a session by removing its directory
pub fn delete_session(session_dir: &Path) -> Result<()> {
    fs::remove_dir_all(session_dir)
        .with_context(|| format!("Failed to delete {}", session_dir.display()))?;
    Ok(())
}

pub fn delete_managed_session(
    session_dir: &Path,
    entry: &ManagedWorktree,
    force: bool,
) -> Result<String> {
    let outcome = worktree::remove_managed_worktree(entry, force)?;

    fs::remove_dir_all(session_dir)
        .with_context(|| format!("Failed to delete {}", session_dir.display()))?;

    let registry_warning = worktree::unregister(entry).err().map(|error| {
        format!("Registry cleanup will be pruned automatically on next load: {error}")
    });

    let mut messages = vec!["Session and worktree deleted".to_string()];
    if let Some(notice) = outcome.branch_notice {
        messages.push(notice);
    } else if outcome.branch_removed {
        messages.push(format!("Branch '{}' deleted", entry.branch));
    }
    if let Some(warning) = registry_warning {
        messages.push(warning);
    }
    Ok(messages.join(". "))
}

/// Resume a session by launching `copilot --resume=<id>` in the session's working directory
pub fn resume_session(session_id: &str, cwd: &str, config: &UserConfig) -> Result<()> {
    let copilot = find_copilot()?;

    let mut cmd = Command::new(copilot);
    cmd.arg(format!("--resume={}", session_id));
    apply_config_args(&mut cmd, config);

    // Set the working directory to the session's original cwd
    if !cwd.is_empty() {
        let cwd_path = Path::new(cwd);
        if cwd_path.exists() {
            cmd.current_dir(cwd_path);
        }
    }

    cmd.status().context("Failed to launch copilot")?;

    Ok(())
}

/// Start a new session by launching `copilot` in the given working directory
pub fn start_new_session(cwd: &str, config: &UserConfig) -> Result<()> {
    let copilot = find_copilot()?;

    let mut cmd = Command::new(copilot);
    apply_config_args(&mut cmd, config);
    let cwd_path = Path::new(cwd);
    if cwd_path.exists() {
        cmd.current_dir(cwd_path);
    }

    cmd.status().context("Failed to launch copilot")?;

    Ok(())
}

pub fn start_worktree_session(
    project: &str,
    branch: &str,
    worktree_config: &EffectiveWorktreeConfig,
    config: &UserConfig,
) -> Result<PathBuf> {
    let copilot = find_copilot()?;
    let created = worktree::create_managed_worktree(Path::new(project), branch, worktree_config)?;

    if let Some(ref notice) = created.notice {
        eprintln!("Notice: {notice}");
    }
    eprintln!(
        "Starting isolated session on '{}' in {}...",
        branch,
        created.entry.path.display()
    );

    let mut cmd = Command::new(copilot);
    apply_config_args(&mut cmd, config);
    cmd.current_dir(&created.entry.path);

    if let Err(error) = cmd.status() {
        let rollback = worktree::rollback_created_worktree(&created.entry);
        return Err(match rollback {
            Ok(()) => anyhow::Error::new(error)
                .context("Failed to launch copilot; worktree creation was rolled back"),
            Err(rollback_error) => anyhow::Error::new(error).context(format!(
                "Failed to launch copilot, and worktree rollback also failed: {rollback_error}"
            )),
        });
    }

    Ok(created.entry.path)
}

/// Locate the Copilot binary, caching the result.
///
/// The probe spawns `copilot --version`, which boots the whole Node CLI and costs
/// ~400ms. That ran on the UI thread for every session create and resume, freezing the
/// TUI before the pane could even show its startup spinner. The location cannot
/// meaningfully change while CST is running, so resolve it once.
fn find_copilot() -> Result<String> {
    static RESOLVED: OnceLock<Option<String>> = OnceLock::new();

    RESOLVED.get_or_init(locate_copilot).clone().ok_or_else(|| {
        anyhow::anyhow!("Could not find copilot CLI. Make sure it's installed and in PATH.")
    })
}

/// Populate the Copilot lookup cache off the UI thread at startup.
pub fn warm_copilot_lookup() {
    std::thread::spawn(|| {
        let _ = find_copilot();
    });
}

fn locate_copilot() -> Option<String> {
    // Check common locations
    let candidates = ["copilot", "copilot.exe"];

    for candidate in &candidates {
        if Command::new(candidate).arg("--version").output().is_ok() {
            return Some(candidate.to_string());
        }
    }

    // Check npm global
    if let Ok(output) = Command::new("npm").args(["root", "-g"]).output() {
        let npm_root = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let copilot_path = format!("{}/@github/copilot/bin/copilot", npm_root);
        if Path::new(&copilot_path).exists() {
            return Some(copilot_path);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_session_is_told_the_id_it_will_have() {
        let config = UserConfig::default();

        let (args, session_id) = new_session_args(&config);

        assert!(
            uuid::Uuid::parse_str(&session_id).is_ok(),
            "Copilot expects a UUID, got {session_id}"
        );
        assert_eq!(
            args.first().map(String::as_str),
            Some(format!("--session-id={session_id}").as_str()),
            "the pane and the child must agree on the id"
        );
    }

    #[test]
    fn every_new_session_gets_its_own_id() {
        let config = UserConfig::default();

        let (_, first) = new_session_args(&config);
        let (_, second) = new_session_args(&config);

        // Two new sessions sharing an id would share a scratchpad.
        assert_ne!(first, second);
    }

    #[test]
    fn naming_a_new_session_does_not_drop_the_configured_arguments() {
        let config = UserConfig {
            yolo: true,
            model: Some("claude-opus-5".to_string()),
            ..UserConfig::default()
        };

        let (args, _) = new_session_args(&config);

        assert!(args.iter().any(|arg| arg == "--yolo"), "got {args:?}");
        assert!(
            args.iter().any(|arg| arg == "--model=claude-opus-5"),
            "got {args:?}"
        );
    }

    /// Proves against the real Copilot binary that the id CST picks is the id the
    /// session actually gets. Ignored by default: it needs Copilot installed and
    /// authenticated, and it spends a few AI credits.
    ///
    /// ```text
    /// cargo test -- --ignored a_new_session_really_is_created_under_the_id_we_chose
    /// ```
    #[test]
    #[ignore = "requires a real, authenticated Copilot CLI and spends AI credits"]
    fn a_new_session_really_is_created_under_the_id_we_chose() {
        let (program, args, session_id) = new_session_command(&UserConfig::default())
            .expect("Copilot CLI must be installed for this probe");

        let workdir = tempfile::tempdir().unwrap();
        let status = Command::new(&program)
            .args(&args)
            .args(["-p", "reply with exactly: ok"])
            .current_dir(workdir.path())
            .status()
            .expect("failed to launch copilot");
        assert!(status.success(), "copilot rejected {args:?}");

        let session_dir = dirs::home_dir()
            .expect("home directory")
            .join(".copilot")
            .join("session-state")
            .join(&session_id);
        let found = session_dir.is_dir();
        if found {
            let _ = fs::remove_dir_all(&session_dir);
        }
        assert!(
            found,
            "expected the session at {}, so per-session state keyed on {session_id} would find it",
            session_dir.display()
        );
    }

    #[test]
    fn rename_updates_current_name_field() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace.yaml");
        fs::write(
            &workspace,
            "id: test\nname: Generated title\nsummary_count: 0\n",
        )
        .unwrap();

        rename_session(temp.path(), "My title").unwrap();

        let content = fs::read_to_string(workspace).unwrap();
        assert!(content.contains("name: My title\n"));
        assert!(!content.contains("summary: My title\n"));
    }

    #[test]
    fn rename_preserves_legacy_summary_field() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace.yaml");
        fs::write(
            &workspace,
            "id: test\nsummary: Old title\nsummary_count: 1\n",
        )
        .unwrap();

        rename_session(temp.path(), "My title").unwrap();

        let content = fs::read_to_string(workspace).unwrap();
        assert!(content.contains("summary: My title\n"));
        assert!(content.contains("summary_count: 1\n"));
    }
}
