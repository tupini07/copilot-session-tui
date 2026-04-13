use anyhow::{Context, Result};
use std::fs;
use std::path::Path;
use std::process::Command;

/// Rename a session by updating the summary field in workspace.yaml
pub fn rename_session(session_dir: &Path, new_name: &str) -> Result<()> {
    let workspace_path = session_dir.join("workspace.yaml");
    let content = fs::read_to_string(&workspace_path)
        .with_context(|| format!("Failed to read {}", workspace_path.display()))?;

    let mut new_lines = Vec::new();
    let mut found_summary = false;

    for line in content.lines() {
        if line.starts_with("summary:") && !line.starts_with("summary_count:") {
            new_lines.push(format!("summary: {}", new_name));
            found_summary = true;
        } else {
            new_lines.push(line.to_string());
        }
    }

    if !found_summary {
        // Insert summary after id line
        let mut inserted = Vec::new();
        for line in &new_lines {
            inserted.push(line.clone());
            if line.starts_with("id:") {
                inserted.push(format!("summary: {}", new_name));
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

/// Resume a session by launching `copilot --resume=<id>` in the session's working directory
pub fn resume_session(session_id: &str, cwd: &str) -> Result<()> {
    let copilot = find_copilot()?;

    let mut cmd = Command::new(copilot);
    cmd.arg(format!("--resume={}", session_id));

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
pub fn start_new_session(cwd: &str) -> Result<()> {
    let copilot = find_copilot()?;

    let mut cmd = Command::new(copilot);
    let cwd_path = Path::new(cwd);
    if cwd_path.exists() {
        cmd.current_dir(cwd_path);
    }

    cmd.status().context("Failed to launch copilot")?;

    Ok(())
}

fn find_copilot() -> Result<String> {
    // Check common locations
    let candidates = [
        "copilot",
        "copilot.exe",
    ];

    for candidate in &candidates {
        if Command::new(candidate)
            .arg("--version")
            .output()
            .is_ok()
        {
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
