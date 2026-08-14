use anyhow::{Context, Result};
use arboard::Clipboard;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use edtui::actions::cpaste::PasteOverSelection;
use edtui::actions::motion::MoveToLastRow;
use edtui::actions::{
    CopyLine, CopySelection, DeleteLine, DeleteSelection, InsertChar, LineBreak, MoveBackward,
    MoveDown, MoveForward, MoveToEndOfLine, MoveToStartOfLine, MoveUp, PasteBefore, Redo,
    SwitchMode, Undo,
};
use edtui::clipboard::ClipboardTrait;
use edtui::{EditorEventHandler, EditorMode, EditorState, Index2, Lines, RowIndex};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

const AUTOSAVE_DELAY: Duration = Duration::from_millis(500);

pub enum InputOutcome {
    Continue,
    Close,
}

pub struct Scratchpad {
    pub session_name: String,
    pub state: EditorState,
    handler: EditorEventHandler,
    path: PathBuf,
    dirty: bool,
    last_edit: Option<Instant>,
    pub status_message: Option<String>,
    clipboard_error: Rc<RefCell<Option<String>>>,
}

impl Scratchpad {
    pub fn open(session_id: &str, session_name: String) -> Result<Self> {
        Self::open_in(&scratchpad_root(), session_id, session_name)
    }

    fn open_in(root: &Path, session_id: &str, session_name: String) -> Result<Self> {
        let path = scratchpad_path_in(root, session_id);
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("Failed to read scratchpad: {}", path.display()));
            }
        };

        let clipboard_error = Rc::new(RefCell::new(None));
        let mut state = EditorState::new(Lines::from(content));
        state.mode = EditorMode::Insert;
        state.set_clipboard(TextClipboard::new(Rc::clone(&clipboard_error)));

        Ok(Self {
            session_name,
            state,
            handler: EditorEventHandler::emacs_mode(),
            path,
            dirty: false,
            last_edit: None,
            status_message: None,
            clipboard_error,
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
        let before = self.content();
        if !self.handle_shortcut(&event) {
            self.prepare_selection_for_input(&event);
            self.handler.on_event(event, &mut self.state);
            if self.state.mode != EditorMode::Insert {
                self.state.mode = EditorMode::Insert;
            }
        }

        if self.content() != before {
            self.dirty = true;
            self.last_edit = Some(Instant::now());
        }
        if let Some(error) = self.clipboard_error.borrow_mut().take() {
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
            self.status_message = Some("Autosaved".to_string());
        }
        Ok(())
    }

    pub fn save(&mut self) -> Result<()> {
        if !self.dirty && self.path.exists() {
            return Ok(());
        }
        write_atomic(&self.path, &self.content())?;
        self.dirty = false;
        self.last_edit = None;
        Ok(())
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    fn content(&self) -> String {
        self.state.lines.to_string()
    }

    fn handle_shortcut(&mut self, event: &Event) -> bool {
        let Event::Key(key) = event else {
            return false;
        };

        if key.modifiers.contains(KeyModifiers::SHIFT) {
            if self.state.selection.is_none() {
                match key.code {
                    KeyCode::Right => {
                        self.begin_selection();
                        self.state.mode = EditorMode::Insert;
                        return true;
                    }
                    KeyCode::Left => {
                        self.state.execute(MoveBackward(1));
                        self.begin_selection();
                        self.state.mode = EditorMode::Insert;
                        return true;
                    }
                    _ => {}
                }
            }
            let movement: Option<Box<dyn edtui::actions::Execute>> = match key.code {
                KeyCode::Left => Some(Box::new(MoveBackward(1))),
                KeyCode::Right => Some(Box::new(MoveForward(1))),
                KeyCode::Up if !key.modifiers.contains(KeyModifiers::ALT) => {
                    Some(Box::new(MoveUp(1)))
                }
                KeyCode::Down if !key.modifiers.contains(KeyModifiers::ALT) => {
                    Some(Box::new(MoveDown(1)))
                }
                KeyCode::Home => Some(Box::new(MoveToStartOfLine())),
                KeyCode::End => Some(Box::new(MoveToEndOfLine())),
                _ => None,
            };
            if let Some(mut movement) = movement {
                self.begin_selection();
                movement.execute(&mut self.state);
                self.state.mode = EditorMode::Insert;
                return true;
            }
        }

        if is_ctrl(key, 'a') {
            self.state.cursor = Index2::new(0, 0);
            self.state.execute(SwitchMode(EditorMode::Visual));
            self.state.execute(MoveToLastRow());
            self.state.execute(MoveToEndOfLine());
            self.state.mode = EditorMode::Insert;
            return true;
        }
        if is_ctrl(key, 'c') {
            if self.state.selection.is_some() {
                self.state.execute(CopySelection);
            } else {
                self.state.execute(CopyLine);
            }
            return true;
        }
        if is_ctrl(key, 'x') {
            if self.state.selection.is_some() {
                self.state.execute(DeleteSelection);
            } else {
                self.state.execute(DeleteLine(1));
            }
            return true;
        }
        if is_ctrl(key, 'v') {
            if self.state.selection.is_some() {
                self.state.execute(PasteOverSelection);
            } else {
                self.state.execute(PasteBefore);
            }
            return true;
        }
        if is_ctrl(key, 'z') {
            self.state.execute(Undo);
            return true;
        }
        if is_ctrl(key, 'y') {
            self.state.execute(Redo);
            return true;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && key.modifiers.contains(KeyModifiers::SHIFT)
            && matches!(key.code, KeyCode::Char('k' | 'K'))
        {
            self.state.execute(DeleteLine(1));
            return true;
        }
        if key.modifiers.contains(KeyModifiers::ALT) {
            match key.code {
                KeyCode::Up => {
                    self.move_line(-1);
                    return true;
                }
                KeyCode::Down => {
                    self.move_line(1);
                    return true;
                }
                _ => {}
            }
        }
        if key.code == KeyCode::Enter && self.auto_list_enter() {
            return true;
        }

        false
    }

    fn begin_selection(&mut self) {
        if self.state.selection.is_none() {
            self.state.execute(SwitchMode(EditorMode::Visual));
        } else {
            self.state.mode = EditorMode::Visual;
        }
    }

    fn prepare_selection_for_input(&mut self, event: &Event) {
        let Event::Key(key) = event else {
            return;
        };
        if self.state.selection.is_none() {
            return;
        }

        let replaces_selection = matches!(
            key.code,
            KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Delete | KeyCode::Enter | KeyCode::Tab
        ) && !key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::ALT);
        if replaces_selection {
            self.state.execute(DeleteSelection);
        } else if matches!(
            key.code,
            KeyCode::Left
                | KeyCode::Right
                | KeyCode::Up
                | KeyCode::Down
                | KeyCode::Home
                | KeyCode::End
        ) {
            self.state.selection = None;
        }
    }

    fn auto_list_enter(&mut self) -> bool {
        if self.state.selection.is_some() {
            return false;
        }
        let row = self.state.cursor.row;
        let Some(line) = self.state.lines.get(RowIndex::new(row)) else {
            return false;
        };
        let Some(list) = list_prefix(line) else {
            return false;
        };

        capture_custom_edit(&mut self.state);
        if list.has_content {
            self.state.execute(LineBreak(1));
            for character in list.continuation.chars() {
                self.state.execute(InsertChar(character));
            }
        } else if self.state.cursor.col >= list.marker_end {
            if let Some(line) = self.state.lines.get_mut(RowIndex::new(row)) {
                line.drain(..list.marker_end);
            }
            self.state.cursor.col = 0;
        } else {
            self.state.execute(LineBreak(1));
        }
        true
    }

    fn move_line(&mut self, direction: isize) {
        if self.state.lines.len() < 2 {
            return;
        }
        let row = self.state.cursor.row;
        let target = if direction < 0 {
            row.saturating_sub(1)
        } else {
            (row + 1).min(self.state.lines.len() - 1)
        };
        if row == target {
            return;
        }

        capture_custom_edit(&mut self.state);
        let line = self.state.lines.remove(RowIndex::new(row));
        self.state.lines.insert(RowIndex::new(target), line);
        self.state.cursor.row = target;
        self.state.cursor.col = self
            .state
            .cursor
            .col
            .min(self.state.lines.len_col(target).unwrap_or_default());
        self.state.selection = None;
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

fn capture_custom_edit(state: &mut EditorState) {
    let cursor = state.cursor;
    let selection = state.selection.clone();
    state.mode = EditorMode::Normal;
    state.execute(SwitchMode(EditorMode::Insert));
    state.cursor = cursor;
    state.selection = selection;
}

fn is_ctrl(key: &KeyEvent, character: char) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char(value) if value.eq_ignore_ascii_case(&character))
}

fn is_supported_key(key: KeyCode) -> bool {
    matches!(
        key,
        KeyCode::Char(_)
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Enter
            | KeyCode::Esc
            | KeyCode::Backspace
            | KeyCode::Delete
            | KeyCode::Tab
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::PageUp
            | KeyCode::PageDown
    )
}

struct ListPrefix {
    continuation: String,
    marker_end: usize,
    has_content: bool,
}

fn list_prefix(line: &[char]) -> Option<ListPrefix> {
    let mut index = line
        .iter()
        .position(|character| !matches!(character, ' ' | '\t'))
        .unwrap_or(line.len());
    let indent: String = line[..index].iter().collect();
    if index >= line.len() {
        return None;
    }

    let continuation = if matches!(line[index], '-' | '*' | '+')
        && line
            .get(index + 1)
            .is_some_and(|character| character.is_whitespace())
    {
        let marker = line[index];
        index += 2;
        format!("{indent}{marker} ")
    } else if line[index].is_ascii_digit() {
        let number_start = index;
        while line.get(index).is_some_and(char::is_ascii_digit) {
            index += 1;
        }
        let delimiter = *line.get(index)?;
        if !matches!(delimiter, '.' | ')')
            || !line
                .get(index + 1)
                .is_some_and(|character| character.is_whitespace())
        {
            return None;
        }
        let number: usize = line[number_start..index]
            .iter()
            .collect::<String>()
            .parse()
            .ok()?;
        index += 2;
        format!("{indent}{}{delimiter} ", number.saturating_add(1))
    } else {
        return None;
    };

    Some(ListPrefix {
        continuation,
        marker_end: index,
        has_content: line[index..]
            .iter()
            .any(|character| !character.is_whitespace()),
    })
}

struct TextClipboard {
    system: Option<Clipboard>,
    fallback: String,
    error: Rc<RefCell<Option<String>>>,
}

impl TextClipboard {
    fn new(error: Rc<RefCell<Option<String>>>) -> Self {
        let system = match Clipboard::new() {
            Ok(clipboard) => Some(clipboard),
            Err(clipboard_error) => {
                *error.borrow_mut() = Some(format!(
                    "System clipboard unavailable; using internal clipboard: {clipboard_error}"
                ));
                None
            }
        };
        Self {
            system,
            fallback: String::new(),
            error,
        }
    }
}

impl ClipboardTrait for TextClipboard {
    fn set_text(&mut self, text: String) {
        self.fallback.clone_from(&text);
        if let Some(clipboard) = &mut self.system {
            if let Err(error) = clipboard.set_text(text) {
                *self.error.borrow_mut() = Some(format!(
                    "System clipboard write failed; copied internally: {error}"
                ));
            }
        }
    }

    fn get_text(&mut self) -> String {
        if let Some(clipboard) = &mut self.system {
            match clipboard.get_text() {
                Ok(text) => return text,
                Err(error) => {
                    *self.error.borrow_mut() = Some(format!(
                        "System clipboard read failed; using internal text: {error}"
                    ));
                }
            }
        }
        self.fallback.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_scratchpad(content: &str) -> (tempfile::TempDir, Scratchpad) {
        let temp = tempfile::tempdir().unwrap();
        let mut scratchpad =
            Scratchpad::open_in(temp.path(), "test-session", "Test session".to_string()).unwrap();
        scratchpad.state.lines = Lines::from(content);
        (temp, scratchpad)
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    #[test]
    fn list_prefix_continues_bullets_and_numbers() {
        let bullet: Vec<char> = "  - idea".chars().collect();
        let numbered: Vec<char> = "9. idea".chars().collect();

        let bullet = list_prefix(&bullet).unwrap();
        assert_eq!(bullet.continuation, "  - ");
        assert!(bullet.has_content);

        let numbered = list_prefix(&numbered).unwrap();
        assert_eq!(numbered.continuation, "10. ");
        assert!(numbered.has_content);
    }

    #[test]
    fn empty_list_marker_ends_the_list() {
        let line: Vec<char> = "* ".chars().collect();
        let prefix = list_prefix(&line).unwrap();

        assert_eq!(prefix.marker_end, 2);
        assert!(!prefix.has_content);
    }

    #[test]
    fn enter_continues_a_list_and_undo_restores_it() {
        let (_temp, mut scratchpad) = test_scratchpad("- first");
        scratchpad.state.cursor = Index2::new(0, 7);

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
        scratchpad.state.cursor = Index2::new(0, 5);

        scratchpad
            .handle_event(key(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();

        assert_eq!(scratchpad.content(), "");
        assert_eq!(scratchpad.state.cursor, Index2::new(0, 0));
    }

    #[test]
    fn moving_a_line_is_undoable() {
        let (_temp, mut scratchpad) = test_scratchpad("first\nsecond");
        scratchpad.state.cursor = Index2::new(0, 2);

        scratchpad
            .handle_event(key(KeyCode::Down, KeyModifiers::ALT))
            .unwrap();
        assert_eq!(scratchpad.content(), "second\nfirst");
        assert_eq!(scratchpad.state.cursor.row, 1);

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
        scratchpad.state.cursor = Index2::new(0, 1);

        scratchpad
            .handle_event(key(KeyCode::Right, KeyModifiers::SHIFT))
            .unwrap();
        scratchpad
            .handle_event(key(KeyCode::Char('X'), KeyModifiers::SHIFT))
            .unwrap();

        assert_eq!(scratchpad.content(), "aXc");
    }

    #[test]
    fn scratchpad_round_trips_and_deletes() {
        let temp = tempfile::tempdir().unwrap();
        let mut scratchpad =
            Scratchpad::open_in(temp.path(), "session/id", "Test session".to_string()).unwrap();
        scratchpad.state.lines = Lines::from("first\nsecond");
        scratchpad.dirty = true;
        scratchpad.save().unwrap();

        let reopened =
            Scratchpad::open_in(temp.path(), "session/id", "Test session".to_string()).unwrap();
        assert_eq!(reopened.content(), "first\nsecond");
        assert!(delete_in(temp.path(), "session/id").unwrap());
        assert!(!delete_in(temp.path(), "session/id").unwrap());
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
