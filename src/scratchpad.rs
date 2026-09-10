use anyhow::{Context, Result};
use crossterm::event::{Event, KeyCode, KeyEventKind};
use edtui::Index2;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::editor::{is_ctrl, is_supported_key, TextEditor};
use crate::workspace_state::{self, CursorPosition};

const AUTOSAVE_DELAY: Duration = Duration::from_millis(500);

pub enum InputOutcome {
    Continue,
    Close,
}

pub struct Scratchpad {
    pub editor: TextEditor,
    path: PathBuf,
    state_root: PathBuf,
    session_id: String,
    dirty: bool,
    last_edit: Option<Instant>,
    pub status_message: Option<String>,
}

impl Scratchpad {
    pub fn open(session_id: &str) -> Result<Self> {
        Self::open_in(&scratchpad_root(), session_id)
    }

    /// Build a scratchpad backed by a throwaway root, for generating documentation
    /// screenshots without touching the notes the user has saved.
    #[cfg(feature = "screenshots")]
    pub fn synthetic(root: &Path, session_id: &str, content: &str) -> Result<Self> {
        write_atomic(&scratchpad_path_in(root, session_id), content)?;
        Self::open_in(root, session_id)
    }

    fn open_in(root: &Path, session_id: &str) -> Result<Self> {
        let path = scratchpad_path_in(root, session_id);
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("Failed to read scratchpad: {}", path.display()));
            }
        };

        let mut editor = TextEditor::new(&content).with_markdown();
        let saved = workspace_state::load_in(root, session_id)?;
        editor.set_cursor(Index2::new(saved.cursor.row, saved.cursor.col));

        Ok(Self {
            editor,
            path,
            state_root: root.to_path_buf(),
            session_id: session_id.to_string(),
            dirty: false,
            last_edit: None,
            status_message: None,
        })
    }

    pub fn handle_event(&mut self, event: Event) -> Result<InputOutcome> {
        if let Event::Key(key) = &event {
            if key.kind != KeyEventKind::Press {
                return Ok(InputOutcome::Continue);
            }
            if key.code == KeyCode::Esc {
                self.save()?;
                return Ok(InputOutcome::Close);
            }
            if is_ctrl(key, 's') {
                self.save()?;
                self.status_message = Some("Scratchpad saved".to_string());
                return Ok(InputOutcome::Continue);
            }
            if !is_supported_key(key.code) {
                return Ok(InputOutcome::Continue);
            }
        }

        self.status_message = None;
        if self.editor.handle_event(event) {
            self.dirty = true;
            self.last_edit = Some(Instant::now());
        }
        if let Some(error) = self.editor.take_clipboard_error() {
            self.status_message = Some(error);
        }
        Ok(InputOutcome::Continue)
    }
    pub fn autosave_if_due(&mut self) -> Result<()> {
        if self.dirty
            && self
                .last_edit
                .is_some_and(|last_edit| last_edit.elapsed() >= AUTOSAVE_DELAY)
        {
            if let Err(error) = self.save() {
                self.last_edit = Some(Instant::now());
                return Err(error);
            }
        }
        Ok(())
    }

    pub fn autosave_pending(&self) -> bool {
        self.dirty
    }

    pub fn save(&mut self) -> Result<()> {
        if self.dirty || !self.path.exists() {
            write_atomic(&self.path, &self.content())?;
            self.dirty = false;
            self.last_edit = None;
        }
        workspace_state::set_cursor_in(
            &self.state_root,
            &self.session_id,
            CursorPosition {
                row: self.editor.cursor().row,
                col: self.editor.cursor().col,
            },
        )
    }

    #[cfg(test)]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    fn content(&self) -> String {
        self.editor.text()
    }
}

pub fn delete(session_id: &str) -> Result<bool> {
    delete_in(&scratchpad_root(), session_id)
}

fn delete_in(root: &Path, session_id: &str) -> Result<bool> {
    let path = scratchpad_path_in(root, session_id);
    match fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => {
            Err(error).with_context(|| format!("Failed to delete scratchpad: {}", path.display()))
        }
    }
}

fn scratchpad_root() -> PathBuf {
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

fn scratchpad_path_in(root: &Path, session_id: &str) -> PathBuf {
    let digest = Sha256::digest(session_id.as_bytes());
    root.join(format!("{digest:x}.txt"))
}

fn write_atomic(path: &Path, content: &str) -> Result<()> {
    let parent = path
        .parent()
        .context("Scratchpad path has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "Failed to create scratchpad directory: {}",
            parent.display()
        )
    })?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).with_context(|| {
        format!(
            "Failed to create temporary scratchpad in {}",
            parent.display()
        )
    })?;
    temp.as_file_mut()
        .write_all(content.as_bytes())
        .context("Failed to write scratchpad")?;
    temp.as_file_mut().sync_all()?;
    temp.persist(path)
        .map_err(|error| error.error)
        .with_context(|| {
            format!(
                "Failed to replace scratchpad atomically: {}",
                path.display()
            )
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers};
    use edtui::{EditorMode, Lines};

    #[test]
    fn edtui_editing_chord_marks_dirty_and_is_persisted() {
        let temp = tempfile::tempdir().unwrap();
        let path = scratchpad_path_in(temp.path(), "dirty-chord");
        fs::write(&path, "delete this").unwrap();
        let mut scratchpad = Scratchpad::open_in(temp.path(), "dirty-chord").unwrap();

        scratchpad
            .handle_event(key(KeyCode::Char('k'), KeyModifiers::CONTROL))
            .unwrap();
        assert!(scratchpad.is_dirty());
        scratchpad.save().unwrap();

        assert_ne!(fs::read_to_string(path).unwrap(), "delete this");
    }

    fn test_scratchpad(content: &str) -> (tempfile::TempDir, Scratchpad) {
        let temp = tempfile::tempdir().unwrap();
        let mut scratchpad = Scratchpad::open_in(temp.path(), "test-session").unwrap();
        scratchpad.editor.state.lines = Lines::from(content);
        (temp, scratchpad)
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    #[test]
    fn enter_continues_a_list_and_undo_restores_it() {
        let (_temp, mut scratchpad) = test_scratchpad("- first");
        scratchpad.editor.state.cursor = Index2::new(0, 7);

        scratchpad
            .handle_event(key(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(scratchpad.content(), "- first\n- ");

        scratchpad
            .handle_event(key(KeyCode::Char('z'), KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(scratchpad.content(), "- first");
    }

    #[test]
    fn enter_on_an_empty_marker_ends_the_list() {
        let (_temp, mut scratchpad) = test_scratchpad("  1. ");
        scratchpad.editor.state.cursor = Index2::new(0, 5);

        scratchpad
            .handle_event(key(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();

        assert_eq!(scratchpad.content(), "");
        assert_eq!(scratchpad.editor.state.cursor, Index2::new(0, 0));
    }

    #[test]
    fn checkbox_shortcuts_add_and_toggle_task_markers() {
        let (_temp, mut scratchpad) = test_scratchpad("  write tests");
        scratchpad.editor.state.cursor = Index2::new(0, 8);

        scratchpad
            .handle_event(key(KeyCode::Char('l'), KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(scratchpad.content(), "  - [ ] write tests");
        assert_eq!(scratchpad.editor.state.cursor.col, 14);

        scratchpad
            .handle_event(key(KeyCode::Char('l'), KeyModifiers::ALT))
            .unwrap();
        assert_eq!(scratchpad.content(), "  - [x] write tests");

        scratchpad
            .handle_event(key(KeyCode::Char('L'), KeyModifiers::ALT))
            .unwrap();
        assert_eq!(scratchpad.content(), "  - [ ] write tests");
    }

    #[test]
    fn checkbox_shortcut_preserves_existing_list_marker_and_is_undoable() {
        let (_temp, mut scratchpad) = test_scratchpad("* write tests");
        scratchpad.editor.state.cursor = Index2::new(0, 7);

        scratchpad
            .handle_event(key(KeyCode::Char('l'), KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(scratchpad.content(), "* [ ] write tests");
        assert_eq!(scratchpad.editor.state.cursor.col, 11);

        scratchpad
            .handle_event(key(KeyCode::Char('z'), KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(scratchpad.content(), "* write tests");
    }

    #[test]
    fn ctrl_w_and_ctrl_backspace_delete_the_previous_word() {
        for (code, modifiers) in [
            (KeyCode::Char('w'), KeyModifiers::CONTROL),
            (KeyCode::Backspace, KeyModifiers::CONTROL),
        ] {
            let (_temp, mut scratchpad) = test_scratchpad("first second   ");
            scratchpad.editor.state.cursor = Index2::new(0, 15);

            scratchpad.handle_event(key(code, modifiers)).unwrap();

            assert_eq!(scratchpad.content(), "first ");
            assert_eq!(scratchpad.editor.state.cursor, Index2::new(0, 6));
            scratchpad
                .handle_event(key(KeyCode::Char('z'), KeyModifiers::CONTROL))
                .unwrap();
            assert_eq!(scratchpad.content(), "first second   ");
        }
    }

    #[test]
    fn ctrl_delete_deletes_the_next_word_and_trailing_space() {
        let (_temp, mut scratchpad) = test_scratchpad("first second third");
        scratchpad.editor.state.cursor = Index2::new(0, 6);

        scratchpad
            .handle_event(key(KeyCode::Delete, KeyModifiers::CONTROL))
            .unwrap();

        assert_eq!(scratchpad.content(), "first third");
        assert_eq!(scratchpad.editor.state.cursor, Index2::new(0, 6));
    }

    #[test]
    fn word_deletion_joins_lines_at_the_boundary() {
        let (_temp, mut scratchpad) = test_scratchpad("first\nsecond");
        scratchpad.editor.state.cursor = Index2::new(1, 0);

        scratchpad
            .handle_event(key(KeyCode::Char('w'), KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(scratchpad.content(), "firstsecond");
        assert_eq!(scratchpad.editor.state.cursor, Index2::new(0, 5));

        scratchpad
            .handle_event(key(KeyCode::Char('z'), KeyModifiers::CONTROL))
            .unwrap();
        scratchpad.editor.state.cursor = Index2::new(0, 5);
        scratchpad
            .handle_event(key(KeyCode::Delete, KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(scratchpad.content(), "firstsecond");
        assert_eq!(scratchpad.editor.state.cursor, Index2::new(0, 5));
    }

    #[test]
    fn shift_tab_dedents_spaces_and_tabs_and_is_undoable() {
        for (content, expected, cursor_before, cursor_after) in [
            ("  indented", "indented", 6, 4),
            ("\tindented", "indented", 5, 4),
            (" indented", "indented", 5, 4),
        ] {
            let (_temp, mut scratchpad) = test_scratchpad(content);
            scratchpad.editor.state.cursor = Index2::new(0, cursor_before);

            scratchpad
                .handle_event(key(KeyCode::BackTab, KeyModifiers::SHIFT))
                .unwrap();

            assert_eq!(scratchpad.content(), expected);
            assert_eq!(scratchpad.editor.state.cursor, Index2::new(0, cursor_after));
            scratchpad
                .handle_event(key(KeyCode::Char('z'), KeyModifiers::CONTROL))
                .unwrap();
            assert_eq!(scratchpad.content(), content);
        }
    }

    #[test]
    fn shift_tab_dedents_every_selected_line() {
        let (_temp, mut scratchpad) = test_scratchpad("  first\n\tsecond\nthird");
        scratchpad.editor.state.cursor = Index2::new(0, 2);
        scratchpad
            .handle_event(key(KeyCode::Down, KeyModifiers::SHIFT))
            .unwrap();

        scratchpad
            .handle_event(key(KeyCode::BackTab, KeyModifiers::SHIFT))
            .unwrap();

        assert_eq!(scratchpad.content(), "first\nsecond\nthird");
        assert_eq!(scratchpad.editor.state.cursor, Index2::new(1, 1));
        let selection = scratchpad.editor.state.selection.as_ref().unwrap();
        assert_eq!(selection.start, Index2::new(0, 0));
        assert_eq!(selection.end, Index2::new(1, 1));
    }

    #[test]
    fn enter_continues_and_ends_checkbox_lists() {
        let (_temp, mut scratchpad) = test_scratchpad("- [x] first");
        scratchpad.editor.state.cursor = Index2::new(0, 11);

        scratchpad
            .handle_event(key(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(scratchpad.content(), "- [x] first\n- [ ] ");

        scratchpad
            .handle_event(key(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(scratchpad.content(), "- [x] first\n");
    }

    #[test]
    fn moving_a_line_is_undoable() {
        let (_temp, mut scratchpad) = test_scratchpad("first\nsecond");
        scratchpad.editor.state.cursor = Index2::new(0, 2);

        scratchpad
            .handle_event(key(KeyCode::Down, KeyModifiers::ALT))
            .unwrap();
        assert_eq!(scratchpad.content(), "second\nfirst");
        assert_eq!(scratchpad.editor.state.cursor.row, 1);

        scratchpad
            .handle_event(key(KeyCode::Char('z'), KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(scratchpad.content(), "first\nsecond");
    }

    #[test]
    fn unsupported_terminal_keys_are_ignored() {
        let (_temp, mut scratchpad) = test_scratchpad("text");

        scratchpad
            .handle_event(key(KeyCode::F(1), KeyModifiers::NONE))
            .unwrap();

        assert_eq!(scratchpad.content(), "text");
    }

    #[test]
    fn shift_right_selects_one_character_for_replacement() {
        let (_temp, mut scratchpad) = test_scratchpad("abc");
        scratchpad.editor.state.cursor = Index2::new(0, 1);

        scratchpad
            .handle_event(key(KeyCode::Right, KeyModifiers::SHIFT))
            .unwrap();
        scratchpad
            .handle_event(key(KeyCode::Char('X'), KeyModifiers::SHIFT))
            .unwrap();

        assert_eq!(scratchpad.content(), "aXc");
    }

    #[test]
    fn ctrl_or_alt_a_selects_the_entire_buffer() {
        for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            let (_temp, mut scratchpad) = test_scratchpad("first\nsecond");
            scratchpad.editor.state.cursor = Index2::new(0, 3);

            scratchpad
                .handle_event(key(KeyCode::Char('a'), modifiers))
                .unwrap();

            let selection = scratchpad.editor.state.selection.as_ref().unwrap();
            assert_eq!(selection.start, Index2::new(0, 0));
            assert_eq!(selection.end, Index2::new(1, 6));
            assert_eq!(scratchpad.editor.state.mode, EditorMode::Insert);
        }
    }

    #[test]
    fn scratchpad_round_trips_and_deletes() {
        let temp = tempfile::tempdir().unwrap();
        let mut scratchpad = Scratchpad::open_in(temp.path(), "session/id").unwrap();
        scratchpad.editor.state.lines = Lines::from("first\nsecond");
        scratchpad.dirty = true;
        scratchpad.save().unwrap();

        let reopened = Scratchpad::open_in(temp.path(), "session/id").unwrap();
        assert_eq!(reopened.content(), "first\nsecond");
        assert!(delete_in(temp.path(), "session/id").unwrap());
        assert!(!delete_in(temp.path(), "session/id").unwrap());
    }

    #[test]
    fn cursor_position_restores_and_clamps_to_content() {
        let temp = tempfile::tempdir().unwrap();
        let mut scratchpad = Scratchpad::open_in(temp.path(), "cursor-session").unwrap();
        scratchpad.editor.state.lines = Lines::from("first\nsecond");
        scratchpad.editor.state.cursor = Index2::new(1, 4);
        scratchpad.dirty = true;
        scratchpad.save().unwrap();

        let mut restored = Scratchpad::open_in(temp.path(), "cursor-session").unwrap();
        assert_eq!(restored.editor.state.cursor, Index2::new(1, 4));

        restored.editor.state.cursor = Index2::new(0, 3);
        assert!(!restored.is_dirty());
        restored.save().unwrap();
        let restored_after_cursor_only_move =
            Scratchpad::open_in(temp.path(), "cursor-session").unwrap();
        assert_eq!(
            restored_after_cursor_only_move.editor.state.cursor,
            Index2::new(0, 3)
        );

        fs::write(scratchpad_path_in(temp.path(), "cursor-session"), "short").unwrap();
        workspace_state::set_cursor_in(
            temp.path(),
            "cursor-session",
            CursorPosition { row: 99, col: 99 },
        )
        .unwrap();
        let clamped = Scratchpad::open_in(temp.path(), "cursor-session").unwrap();
        assert_eq!(clamped.editor.state.cursor, Index2::new(0, 5));
    }

    #[test]
    fn scratchpad_filename_does_not_expose_session_id() {
        let root = Path::new("scratchpads");
        let path = scratchpad_path_in(root, "../unsafe/session");

        assert_eq!(path.parent(), Some(root));
        assert_eq!(
            path.extension().and_then(|value| value.to_str()),
            Some("txt")
        );
        assert!(!path.to_string_lossy().contains("unsafe"));
    }
}
