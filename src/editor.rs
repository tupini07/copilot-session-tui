//! The text editor behind the scratchpad and the snippet form.
//!
//! Both are edtui in insert mode with the same shortcut layer on top, so a
//! keystroke that works in one works in the other. The differences are declared
//! at construction: a single-line field refuses newlines, and the markdown
//! affordances (list continuation, checkboxes) are opt-in.

use arboard::Clipboard;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind};
use edtui::actions::cpaste::PasteOverSelection;
use edtui::actions::motion::MoveToLastRow;
use edtui::actions::{
    CopyLine, CopySelection, DeleteLine, DeleteSelection, InsertChar, LineBreak, MoveBackward,
    MoveDown, MoveForward, MoveToEndOfLine, MoveToStartOfLine, MoveUp, PasteBefore, Redo,
    SwitchMode, Undo,
};
use edtui::clipboard::ClipboardTrait;
use edtui::{EditorEventHandler, EditorMode, EditorState, Index2, Lines, RowIndex};
use std::cell::RefCell;
use std::rc::Rc;

use crate::text::{next_word_end, previous_word_start};

pub struct TextEditor {
    pub state: EditorState,
    handler: EditorEventHandler,
    clipboard_error: Rc<RefCell<Option<String>>>,
    /// Kept alongside edtui's own flag so the indent shortcuts stay off a field.
    single_line: bool,
    /// List continuation and checkboxes suit notes, not a prompt.
    markdown: bool,
}

impl TextEditor {
    pub fn new(content: &str) -> Self {
        let clipboard_error = Rc::new(RefCell::new(None));
        let mut state = EditorState::new(Lines::from(content));
        state.mode = EditorMode::Insert;
        state.set_clipboard(TextClipboard::new(Rc::clone(&clipboard_error)));
        Self {
            state,
            handler: EditorEventHandler::emacs_mode(),
            clipboard_error,
            single_line: false,
            markdown: false,
        }
    }

    /// A form field holding one value. edtui refuses the line breaks, including
    /// the ones inside a paste; we only have to keep the indent shortcuts off it.
    #[must_use]
    pub fn single_line(mut self) -> Self {
        self.single_line = true;
        self.state.set_single_line(true);
        self
    }

    #[must_use]
    pub fn with_markdown(mut self) -> Self {
        self.markdown = true;
        self
    }

    pub fn text(&self) -> String {
        self.state.lines.to_string()
    }

    /// Replaces the whole buffer and parks the cursor after the last character.
    #[cfg(test)]
    pub fn set_text(&mut self, content: &str) {
        self.state.lines = Lines::from(content);
        self.state.selection = None;
        self.move_cursor_to_end();
    }

    pub fn cursor(&self) -> Index2 {
        self.state.cursor
    }

    /// Places the cursor at `cursor`, clamped to the content.
    pub fn set_cursor(&mut self, cursor: Index2) {
        let row = cursor.row.min(self.state.lines.len().saturating_sub(1));
        let col = cursor
            .col
            .min(self.state.lines.len_col(row).unwrap_or_default());
        self.state.cursor = Index2::new(row, col);
    }

    pub fn move_cursor_to_end(&mut self) {
        let row = self.state.lines.len().saturating_sub(1);
        self.set_cursor(Index2::new(row, usize::MAX));
    }

    /// The clipboard warning raised by the last event, if any.
    pub fn take_clipboard_error(&mut self) -> Option<String> {
        self.clipboard_error.borrow_mut().take()
    }

    /// Applies `event` and reports whether it may have changed the text.
    pub fn handle_event(&mut self, event: Event) -> bool {
        // Pasted text is rendered straight back to the terminal, so an escape
        // sequence smuggled through the clipboard would be executed rather than
        // shown. Newlines and tabs are the only control characters that mean
        // anything here.
        let event = match event {
            Event::Paste(text) => Event::Paste(sanitize_pasted_text(&text)),
            event => event,
        };
        let is_mouse_drag = matches!(
            &event,
            Event::Mouse(mouse) if matches!(mouse.kind, MouseEventKind::Drag(_))
        );

        if let Event::Key(key) = &event {
            if key.kind != KeyEventKind::Press || !is_supported_key(key.code) {
                return false;
            }
        }

        if !is_vertical_navigation(&event) {
            self.state.reset_vertical_goal();
        }
        let may_edit = event_may_edit(&event);
        if !self.handle_shortcut(&event) {
            self.prepare_selection_for_input(&event);
            self.handler.on_event(event, &mut self.state);
            if self.state.mode != EditorMode::Insert && !is_mouse_drag {
                self.state.mode = EditorMode::Insert;
            }
        }
        may_edit
    }

    fn handle_shortcut(&mut self, event: &Event) -> bool {
        let Event::Key(key) = event else {
            return false;
        };

        if key.code == KeyCode::BackTab
            || (key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT))
        {
            if !self.single_line {
                self.dedent();
            }
            return true;
        }

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

        if is_ctrl(key, 'a') || is_alt(key, 'a') {
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
        if is_ctrl(key, 'w')
            || (key.code == KeyCode::Backspace && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            self.delete_previous_word();
            return true;
        }
        if key.code == KeyCode::Delete && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.delete_next_word();
            return true;
        }
        if self.markdown && (is_ctrl(key, 'l') || is_alt(key, 'l')) {
            self.toggle_checkbox();
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
        if self.markdown && key.code == KeyCode::Enter && self.auto_list_enter() {
            return true;
        }

        false
    }

    fn dedent(&mut self) {
        let (start_row, end_row) = self
            .state
            .selection
            .as_ref()
            .map(|selection| (selection.start().row, selection.end().row))
            .unwrap_or((self.state.cursor.row, self.state.cursor.row));
        let removals: Vec<usize> = (start_row..=end_row)
            .map(|row| {
                self.state
                    .lines
                    .get(RowIndex::new(row))
                    .map_or(0, |line| indentation_to_remove(line))
            })
            .collect();
        if removals.iter().all(|removed| *removed == 0) {
            return;
        }

        capture_custom_edit(&mut self.state);
        for (row, removed) in (start_row..=end_row).zip(removals.iter().copied()) {
            if removed == 0 {
                continue;
            }
            if let Some(line) = self.state.lines.get_mut(RowIndex::new(row)) {
                line.drain(..removed);
            }
        }

        let removed_on_cursor_row = removals[self.state.cursor.row - start_row];
        self.state.cursor.col = self.state.cursor.col.saturating_sub(removed_on_cursor_row);
        if let Some(selection) = self.state.selection.as_mut() {
            let start_removed = removals[selection.start.row - start_row];
            let end_removed = removals[selection.end.row - start_row];
            selection.start.col = selection.start.col.saturating_sub(start_removed);
            selection.end.col = selection.end.col.saturating_sub(end_removed);
            if let Some(anchor) = selection.anchor.as_mut() {
                let anchor_removed = removals[anchor.row - start_row];
                anchor.col = anchor.col.saturating_sub(anchor_removed);
            }
        }
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

    fn delete_previous_word(&mut self) {
        if self.state.selection.is_some() {
            self.state.execute(DeleteSelection);
            return;
        }

        let row = self.state.cursor.row;
        let col = self.state.cursor.col;
        if col > 0 {
            let Some(line) = self.state.lines.get(RowIndex::new(row)) else {
                return;
            };
            let start = previous_word_start(line, col);
            capture_custom_edit(&mut self.state);
            if let Some(line) = self.state.lines.get_mut(RowIndex::new(row)) {
                line.drain(start..col);
            }
            self.state.cursor.col = start;
        } else if row > 0 {
            capture_custom_edit(&mut self.state);
            let current = self.state.lines.remove(RowIndex::new(row));
            let previous_row = row - 1;
            if let Some(previous) = self.state.lines.get_mut(RowIndex::new(previous_row)) {
                let previous_len = previous.len();
                previous.extend(current);
                self.state.cursor = Index2::new(previous_row, previous_len);
            }
        }
    }

    fn delete_next_word(&mut self) {
        if self.state.selection.is_some() {
            self.state.execute(DeleteSelection);
            return;
        }

        let row = self.state.cursor.row;
        let col = self.state.cursor.col;
        let Some(line) = self.state.lines.get(RowIndex::new(row)) else {
            return;
        };
        if col < line.len() {
            let end = next_word_end(line, col);
            capture_custom_edit(&mut self.state);
            if let Some(line) = self.state.lines.get_mut(RowIndex::new(row)) {
                line.drain(col..end);
            }
        } else if row + 1 < self.state.lines.len() {
            capture_custom_edit(&mut self.state);
            let next = self.state.lines.remove(RowIndex::new(row + 1));
            if let Some(line) = self.state.lines.get_mut(RowIndex::new(row)) {
                line.extend(next);
            }
        }
    }

    fn toggle_checkbox(&mut self) {
        let row = self.state.cursor.row;
        let Some(line) = self.state.lines.get(RowIndex::new(row)) else {
            return;
        };
        let (replacement, cursor_col) = toggle_checkbox_line(line, self.state.cursor.col);

        capture_custom_edit(&mut self.state);
        if let Some(line) = self.state.lines.get_mut(RowIndex::new(row)) {
            *line = replacement;
        }
        self.state.cursor.col = cursor_col;
        self.state.selection = None;
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

fn sanitize_pasted_text(text: &str) -> String {
    text.replace("\r\n", "\n")
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        .collect()
}

/// Opens an undo step for an edit made by writing to `lines` directly, which
/// edtui's own actions would have recorded for us.
fn capture_custom_edit(state: &mut EditorState) {
    let cursor = state.cursor;
    let selection = state.selection.clone();
    state.mode = EditorMode::Normal;
    state.execute(SwitchMode(EditorMode::Insert));
    state.cursor = cursor;
    state.selection = selection;
}

pub fn is_ctrl(key: &KeyEvent, character: char) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char(value) if value.eq_ignore_ascii_case(&character))
}

fn is_alt(key: &KeyEvent, character: char) -> bool {
    key.modifiers.contains(KeyModifiers::ALT)
        && matches!(key.code, KeyCode::Char(value) if value.eq_ignore_ascii_case(&character))
}

fn is_vertical_navigation(event: &Event) -> bool {
    matches!(
        event,
        Event::Key(KeyEvent {
            code: KeyCode::Up | KeyCode::Down,
            modifiers,
            ..
        }) if !modifiers.contains(KeyModifiers::ALT)
    )
}

fn event_may_edit(event: &Event) -> bool {
    let Event::Key(key) = event else {
        return matches!(event, Event::Paste(_));
    };
    if key.kind != KeyEventKind::Press {
        return false;
    }
    if is_ctrl(key, 'a') || is_alt(key, 'a') || is_ctrl(key, 'c') {
        return false;
    }
    if matches!(
        key.code,
        KeyCode::Up
            | KeyCode::Down
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::PageUp
            | KeyCode::PageDown
    ) && !key.modifiers.contains(KeyModifiers::ALT)
    {
        return false;
    }
    // Be conservative for every other supported event. edtui has more editing
    // chords than CST customizes (including AltGr normalization); an unnecessary
    // autosave is harmless, while a missed mutation can lose scratchpad content.
    is_supported_key(key.code)
}

pub fn is_supported_key(key: KeyCode) -> bool {
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
            | KeyCode::BackTab
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

fn toggle_checkbox_line(line: &[char], cursor_col: usize) -> (Vec<char>, usize) {
    let indent_end = line
        .iter()
        .position(|character| !matches!(character, ' ' | '\t'))
        .unwrap_or(line.len());
    let has_list_marker = line
        .get(indent_end)
        .is_some_and(|character| matches!(character, '-' | '*' | '+'))
        && line
            .get(indent_end + 1)
            .is_some_and(|character| character.is_whitespace());
    let checkbox_start = if has_list_marker {
        indent_end + 2
    } else {
        indent_end
    };

    if let Some(checked) = checkbox_marker(line, checkbox_start) {
        let mut replacement = line.to_vec();
        replacement[checkbox_start + 1] = if checked { ' ' } else { 'x' };
        return (replacement, cursor_col);
    }

    let marker: Vec<char> = if has_list_marker {
        "[ ] ".chars().collect()
    } else {
        "- [ ] ".chars().collect()
    };
    let mut replacement = line.to_vec();
    replacement.splice(checkbox_start..checkbox_start, marker.iter().copied());
    let cursor_col = if cursor_col >= checkbox_start {
        cursor_col + marker.len()
    } else {
        cursor_col
    };
    (replacement, cursor_col)
}

fn checkbox_marker(line: &[char], start: usize) -> Option<bool> {
    if line.get(start) != Some(&'[') || line.get(start + 2) != Some(&']') {
        return None;
    }
    match line.get(start + 1) {
        Some(' ') => Some(false),
        Some('x' | 'X') => Some(true),
        _ => None,
    }
}

fn indentation_to_remove(line: &[char]) -> usize {
    if line.first() == Some(&'\t') {
        1
    } else {
        line.iter()
            .take(2)
            .take_while(|character| **character == ' ')
            .count()
    }
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
        if checkbox_marker(line, index).is_some()
            && line
                .get(index + 3)
                .is_some_and(|character| character.is_whitespace())
        {
            index += 4;
            format!("{indent}{marker} [ ] ")
        } else {
            format!("{indent}{marker} ")
        }
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
                        "System clipboard read failed; pasted internally: {error}"
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

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    #[test]
    fn edit_classification_does_not_dirty_navigation_or_copy() {
        assert!(!event_may_edit(&key(KeyCode::Left, KeyModifiers::NONE)));
        assert!(!event_may_edit(&key(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        )));
        assert!(event_may_edit(&key(
            KeyCode::Char('x'),
            KeyModifiers::CONTROL
        )));
        assert!(event_may_edit(&key(KeyCode::Char('a'), KeyModifiers::NONE)));
        assert!(event_may_edit(&Event::Paste("many\nlines".to_string())));
        for character in ['k', 'o', 'j', 'h', 'd', 'u'] {
            assert!(
                event_may_edit(&key(KeyCode::Char(character), KeyModifiers::CONTROL)),
                "edtui Ctrl+{character} may edit"
            );
        }
        assert!(
            event_may_edit(&key(
                KeyCode::Char('@'),
                KeyModifiers::CONTROL | KeyModifiers::ALT
            )),
            "AltGr characters may be normalized into inserted text"
        );
        assert!(!event_may_edit(&key(KeyCode::Right, KeyModifiers::CONTROL)));
    }

    #[test]
    fn list_prefix_continues_bullets_and_numbers() {
        let prefix = |line: &str| {
            list_prefix(&line.chars().collect::<Vec<char>>()).map(|prefix| prefix.continuation)
        };

        assert_eq!(prefix("- item"), Some("- ".to_string()));
        assert_eq!(prefix("  - idea"), Some("  - ".to_string()));
        assert_eq!(prefix("  * item"), Some("  * ".to_string()));
        assert_eq!(prefix("3. item"), Some("4. ".to_string()));
        assert_eq!(prefix("9. idea"), Some("10. ".to_string()));
        assert_eq!(prefix("2) item"), Some("3) ".to_string()));
        assert_eq!(prefix("- [x] done"), Some("- [ ] ".to_string()));
        assert_eq!(prefix("plain text"), None);
        assert_eq!(prefix("3.no space"), None);
    }

    #[test]
    fn empty_list_marker_ends_the_list() {
        let line: Vec<char> = "- ".chars().collect();
        let prefix = list_prefix(&line).unwrap();

        assert!(!prefix.has_content);
        assert_eq!(prefix.marker_end, 2);
    }

    #[test]
    fn a_single_line_field_refuses_newlines_from_enter_and_paste() {
        let mut editor = TextEditor::new("name").single_line();
        editor.move_cursor_to_end();

        editor.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(editor.text(), "name");

        editor.handle_event(Event::Paste("one\ntwo".to_string()));
        assert_eq!(editor.state.lines.len(), 1, "got: {:?}", editor.text());
        assert!(
            editor.text().contains("one two"),
            "got: {:?}",
            editor.text()
        );
    }

    #[test]
    fn markdown_affordances_are_opt_in() {
        let mut plain = TextEditor::new("- item");
        plain.move_cursor_to_end();
        plain.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(plain.text(), "- item\n", "no list continuation without it");

        let mut notes = TextEditor::new("- item").with_markdown();
        notes.move_cursor_to_end();
        notes.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(notes.text(), "- item\n- ");
    }
}
