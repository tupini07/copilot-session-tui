use anyhow::{Context, Result};
use chrono::DateTime;
use std::fs;
use std::path::{Path, PathBuf};

use super::{Session, WorkspaceYaml};
use crate::events::parser;

/// Discover the copilot config directory
pub fn copilot_home() -> PathBuf {
    if let Ok(home) = std::env::var("COPILOT_HOME") {
        return PathBuf::from(home);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".copilot")
}

/// Load all sessions from the session-state directory
pub fn load_sessions(copilot_home: &Path) -> Result<Vec<Session>> {
    let session_dir = copilot_home.join("session-state");
    if !session_dir.exists() {
        anyhow::bail!(
            "Session directory not found: {}",
            session_dir.display()
        );
    }

    let mut sessions = Vec::new();
    let entries = fs::read_dir(&session_dir)
        .with_context(|| format!("Failed to read {}", session_dir.display()))?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        match load_single_session(&path) {
            Ok(session) => sessions.push(session),
            Err(_) => continue, // skip malformed sessions
        }
    }

    // Sort by updated_at descending (most recent first)
    sessions.sort_by(|a, b| {
        let a_time = a.updated_at.or(a.created_at);
        let b_time = b.updated_at.or(b.created_at);
        b_time.cmp(&a_time)
    });

    Ok(sessions)
}

fn load_single_session(dir: &Path) -> Result<Session> {
    let workspace_path = dir.join("workspace.yaml");
    let yaml_str = fs::read_to_string(&workspace_path)
        .with_context(|| format!("Failed to read {}", workspace_path.display()))?;

    let ws: WorkspaceYaml = serde_yaml::from_str(&yaml_str)
        .with_context(|| format!("Failed to parse {}", workspace_path.display()))?;

    let created_at = ws
        .created_at
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));

    let updated_at = ws
        .updated_at
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));

    let is_active = detect_active(dir);

    Ok(Session {
        id: ws.id,
        cwd: ws.cwd.unwrap_or_default(),
        summary: ws.summary,
        created_at,
        updated_at,
        is_active,
        dir_path: dir.to_path_buf(),
        edited_files: Vec::new(),
        last_user_message: None,
        turn_count: 0,
        tool_call_count: 0,
    })
}

/// Load detail data (edited files, messages) for a single session — lazy/on-demand
pub fn load_session_details(session: &mut Session) -> Result<()> {
    let events_path = session.dir_path.join("events.jsonl");
    if events_path.exists() {
        let details = parser::parse_events(&events_path)?;
        session.edited_files = details.edited_files;
        session.last_user_message = details.last_user_message;
        session.turn_count = details.turn_count;
        session.tool_call_count = details.tool_call_count;
    }
    Ok(())
}

/// Detect if a session is currently active by checking lock files
fn detect_active(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with("inuse.") && name_str.ends_with(".lock") {
            // Extract PID from filename
            let pid_str = name_str
                .strip_prefix("inuse.")
                .and_then(|s| s.strip_suffix(".lock"));
            if let Some(pid_str) = pid_str {
                if let Ok(pid) = pid_str.parse::<u32>() {
                    if is_process_running(pid) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

#[cfg(windows)]
fn is_process_running(pid: u32) -> bool {
    use std::process::Command;
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {}", pid), "/NH"])
        .output()
        .map(|o| {
            let out = String::from_utf8_lossy(&o.stdout);
            out.contains(&pid.to_string())
        })
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn is_process_running(pid: u32) -> bool {
    use std::path::Path as StdPath;
    StdPath::new(&format!("/proc/{}", pid)).exists()
}
