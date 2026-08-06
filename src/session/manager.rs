use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::{EffectiveWorktreeConfig, UserConfig};

use super::worktree::{self, ManagedWorktree};

fn apply_config_args(cmd: &mut Command, config: &UserConfig) {
    if config.yolo {
        cmd.arg("--yolo");
    }
    if let Some(ref model) = config.model {
        cmd.arg(format!("--model={}", model));
    }
    if let Some(ref effort) = config.reasoning_effort {
        cmd.arg(format!("--reasoning-effort={}", effort));
    }
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

fn find_copilot() -> Result<String> {
    // Check common locations
    let candidates = ["copilot", "copilot.exe"];

    for candidate in &candidates {
        if Command::new(candidate).arg("--version").output().is_ok() {
            return Ok(candidate.to_string());
        }
    }

    // Check npm global
    if let Ok(output) = Command::new("npm").args(["root", "-g"]).output() {
        let npm_root = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let copilot_path = format!("{}/@github/copilot/bin/copilot", npm_root);
        if Path::new(&copilot_path).exists() {
            return Ok(copilot_path);
        }
    }

    anyhow::bail!("Could not find copilot CLI. Make sure it's installed and in PATH.")
}

#[cfg(test)]
mod tests {
    use super::*;

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
