use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorPosition {
    pub row: usize,
    pub col: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionWorkspaceState {
    #[serde(default)]
    pub scratchpad_open: bool,
    #[serde(default)]
    pub terminal_open: bool,
    #[serde(default)]
    pub cursor: CursorPosition,
}

pub(crate) fn set_scratchpad_open_in(root: &Path, session_id: &str, open: bool) -> Result<()> {
    update_in(root, session_id, |state| {
        state.scratchpad_open = open;
    })
}

pub(crate) fn set_terminal_open_in(root: &Path, session_id: &str, open: bool) -> Result<()> {
    update_in(root, session_id, |state| {
        state.terminal_open = open;
    })
}

pub fn delete(session_id: &str) -> Result<bool> {
    delete_in(&workspace_root(), session_id)
}

fn delete_in(root: &Path, session_id: &str) -> Result<bool> {
    let path = state_path(root, session_id);
    match fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error)
            .with_context(|| format!("Failed to delete workspace state: {}", path.display())),
    }
}

pub(crate) fn load_in(root: &Path, session_id: &str) -> Result<SessionWorkspaceState> {
    read_file(&state_path(root, session_id))
}

pub(crate) fn set_cursor_in(root: &Path, session_id: &str, cursor: CursorPosition) -> Result<()> {
    update_in(root, session_id, |state| state.cursor = cursor)
}

fn update_in(
    root: &Path,
    session_id: &str,
    update: impl FnOnce(&mut SessionWorkspaceState),
) -> Result<()> {
    let path = state_path(root, session_id);
    let mut state = read_file(&path)?;
    update(&mut state);
    write_file(&path, &state)
}

fn read_file(path: &Path) -> Result<SessionWorkspaceState> {
    match fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content)
            .with_context(|| format!("Invalid workspace state: {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(SessionWorkspaceState::default())
        }
        Err(error) => Err(error)
            .with_context(|| format!("Failed to read workspace state: {}", path.display())),
    }
}

fn write_file(path: &Path, state: &SessionWorkspaceState) -> Result<()> {
    let parent = path
        .parent()
        .context("Workspace state path has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "Failed to create workspace state directory: {}",
            parent.display()
        )
    })?;
    let content = serde_json::to_vec_pretty(state)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.as_file_mut().write_all(&content)?;
    temp.as_file_mut().sync_all()?;
    temp.persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("Failed to replace workspace state: {}", path.display()))?;
    Ok(())
}

pub(crate) fn workspace_root() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local")
                .join("share")
        })
        .join("copilot-session-tui")
        .join("scratchpads")
}

fn state_path(root: &Path, session_id: &str) -> PathBuf {
    root.join(format!("{}.state.json", session_key(session_id)))
}

fn session_key(session_id: &str) -> String {
    format!("{:x}", Sha256::digest(session_id.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_per_session_without_exposing_ids() {
        let temp = tempfile::tempdir().unwrap();
        set_cursor_in(
            temp.path(),
            "private-session-id",
            CursorPosition { row: 4, col: 7 },
        )
        .unwrap();
        update_in(temp.path(), "private-session-id", |state| {
            state.scratchpad_open = true;
            state.terminal_open = true;
        })
        .unwrap();

        let state = load_in(temp.path(), "private-session-id").unwrap();
        assert_eq!(state.cursor, CursorPosition { row: 4, col: 7 });
        assert!(state.scratchpad_open);
        assert!(state.terminal_open);

        let raw = fs::read_to_string(state_path(temp.path(), "private-session-id")).unwrap();
        assert!(!raw.contains("private-session-id"));
        assert!(!state_path(temp.path(), "private-session-id")
            .to_string_lossy()
            .contains("private-session-id"));
        assert_eq!(
            load_in(temp.path(), "another-session").unwrap(),
            SessionWorkspaceState::default()
        );
        assert!(delete_in(temp.path(), "private-session-id").unwrap());
        assert!(!delete_in(temp.path(), "private-session-id").unwrap());
    }
}
