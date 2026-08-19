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
        anyhow::bail!("Session directory not found: {}", session_dir.display());
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
            Ok(session) => {
                // Skip empty sessions (no title and no events)
                if session.summary.is_none() {
                    let events_path = path.join("events.jsonl");
                    let has_events = events_path.exists()
                        && fs::metadata(&events_path)
                            .map(|m| m.len() > 100) // trivial events file = just session.start
                            .unwrap_or(false);
                    if !has_events {
                        continue;
                    }
                }
                sessions.push(session);
            }
            Err(_) => continue,
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
    let cwd = ws.cwd.unwrap_or_default();
    let project_root = resolve_project_root(&cwd);

    Ok(Session {
        id: ws.id,
        cwd,
        project_root,
        // `name` is the current Copilot CLI field. Prefer `summary` when
        // present so titles written by older TUI versions remain effective.
        summary: ws.summary.or(ws.name),
        created_at,
        updated_at,
        is_active,
        dir_path: dir.to_path_buf(),
        edited_files: Vec::new(),
        last_user_message: None,
        turn_count: 0,
        tool_call_count: 0,
        details_parsed_len: 0,
    })
}

/// Load detail data (edited files, messages) for a single session — lazy/on-demand.
///
/// Resumes from whatever was already summarized, so reselecting a session costs a
/// stat, and a session that is still running only pays for its newly appended events.
pub fn load_session_details(session: &mut Session) -> Result<()> {
    let events_path = session.dir_path.join("events.jsonl");
    if !events_path.exists() {
        return Ok(());
    }

    let mut details = parser::SessionDetails {
        edited_files: std::mem::take(&mut session.edited_files),
        last_user_message: session.last_user_message.take(),
        turn_count: session.turn_count,
        tool_call_count: session.tool_call_count,
        parsed_len: session.details_parsed_len,
    };

    let result = parser::parse_events_into(&events_path, &mut details);

    session.edited_files = details.edited_files;
    session.last_user_message = details.last_user_message;
    session.turn_count = details.turn_count;
    session.tool_call_count = details.tool_call_count;
    session.details_parsed_len = details.parsed_len;

    result
}

/// Bytes of a session's event log that have not been summarized yet.
///
/// Used to decide whether details are cheap enough to load immediately or should
/// wait for the selection to settle.
pub fn pending_detail_bytes(session: &Session) -> u64 {
    fs::metadata(session.dir_path.join("events.jsonl"))
        .map(|meta| meta.len().saturating_sub(session.details_parsed_len))
        .unwrap_or(0)
}

/// Canonicalize a path to get consistent casing/representation, falling back to lossy string
fn canonicalize_or_lossy(p: &Path) -> String {
    fs::canonicalize(p)
        .map(|c| {
            let s = c.to_string_lossy().to_string();
            // Strip Windows \\?\ extended path prefix
            s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
        })
        .unwrap_or_else(|_| p.to_string_lossy().to_string())
}

/// Resolve the project root of `cwd`, returning `None` when `cwd` is not inside
/// a Git repository. Used to detect whether the current directory is a project.
pub fn detect_project_root(cwd: &str) -> Option<String> {
    find_project_root(cwd)
}

/// Resolve the project root from a working directory.
/// Walks up from `cwd` looking for `.git`. If `.git` is a file (git worktree),
/// follows the `gitdir:` pointer back to the main repository root.
fn resolve_project_root(cwd: &str) -> String {
    find_project_root(cwd).unwrap_or_else(|| cwd.to_string())
}

fn find_project_root(cwd: &str) -> Option<String> {
    if cwd.is_empty() {
        return None;
    }

    let mut current = PathBuf::from(cwd);
    loop {
        let git_path = current.join(".git");
        if git_path.is_dir() {
            // Normal git repo — this directory is the project root
            return Some(canonicalize_or_lossy(&current));
        }
        if git_path.is_file() {
            // Git worktree — .git is a file like "gitdir: /path/to/main/.git/worktrees/<name>"
            if let Ok(content) = fs::read_to_string(&git_path) {
                if let Some(gitdir) = content.trim().strip_prefix("gitdir:") {
                    let gitdir = gitdir.trim();
                    let gitdir_path = if Path::new(gitdir).is_absolute() {
                        PathBuf::from(gitdir)
                    } else {
                        current.join(gitdir)
                    };
                    // Follow: .git/worktrees/<name> -> go up 2 levels to get .git, then parent
                    if let Some(dot_git) = gitdir_path.parent().and_then(|p| p.parent()) {
                        if let Some(repo_root) = dot_git.parent() {
                            if dot_git.ends_with(".git") {
                                return Some(canonicalize_or_lossy(repo_root));
                            }
                        }
                    }
                }
            }
            // Couldn't resolve — use the worktree dir itself
            return Some(canonicalize_or_lossy(&current));
        }
        if !current.pop() {
            break;
        }
    }
    // No .git found
    None
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

pub fn session_is_active(dir: &Path) -> bool {
    detect_active(dir)
}

/// Whether a process is still alive.
///
/// `OpenProcess` succeeds for a live process and fails with `ERROR_INVALID_PARAMETER`
/// once the PID is gone. A PID belonging to a process we may not query (access denied)
/// still exists, so that case counts as running.
#[cfg(windows)]
fn is_process_running(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_INVALID_PARAMETER, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    // SAFETY: `OpenProcess` takes no pointers; the handle it returns is closed below and
    // never escapes this function.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return GetLastError() != ERROR_INVALID_PARAMETER;
        }
        CloseHandle(handle);
        true
    }
}

#[cfg(not(windows))]
fn is_process_running(pid: u32) -> bool {
    use std::path::Path as StdPath;
    StdPath::new(&format!("/proc/{}", pid)).exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_workspace(dir: &Path, title_fields: &str) {
        fs::write(
            dir.join("workspace.yaml"),
            format!(
                "id: test-session\ncwd: {}\n{title_fields}created_at: 2026-08-06T12:00:00Z\nupdated_at: 2026-08-06T12:00:00Z\n",
                dir.display()
            ),
        )
        .unwrap();
    }

    #[test]
    fn loads_current_name_field() {
        let temp = tempfile::tempdir().unwrap();
        write_workspace(temp.path(), "name: Generated title\n");

        let session = load_single_session(temp.path()).unwrap();

        assert_eq!(session.display_name(), "Generated title");
    }

    #[test]
    fn legacy_summary_takes_precedence() {
        let temp = tempfile::tempdir().unwrap();
        write_workspace(
            temp.path(),
            "name: Generated title\nsummary: Legacy TUI rename\n",
        );

        let session = load_single_session(temp.path()).unwrap();

        assert_eq!(session.display_name(), "Legacy TUI rename");
    }

    #[test]
    fn detect_project_root_finds_repo_root_from_subdirectory() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();
        let nested = temp.path().join("src").join("deep");
        fs::create_dir_all(&nested).unwrap();

        let detected = detect_project_root(&nested.to_string_lossy()).unwrap();

        assert_eq!(detected, canonicalize_or_lossy(temp.path()));
    }

    #[test]
    fn detect_project_root_returns_none_outside_a_repo() {
        let temp = tempfile::tempdir().unwrap();

        assert!(detect_project_root(&temp.path().to_string_lossy()).is_none());
    }
}
