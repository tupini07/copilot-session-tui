use crate::config::PromptSnippet;
use crate::text;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnippetScope {
    Global,
    Project,
}

impl SnippetScope {
    pub fn label(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnippetScreen {
    List,
    Editor,
    ConfirmDelete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnippetEditorField {
    Name,
    Prompt,
    Scope,
}

impl SnippetEditorField {
    pub fn next(self, forward: bool) -> Self {
        match (self, forward) {
            (Self::Name, true) | (Self::Prompt, false) => Self::Scope,
            (Self::Scope, true) | (Self::Name, false) => Self::Prompt,
            (Self::Prompt, true) | (Self::Scope, false) => Self::Name,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SnippetModal {
    pub screen: SnippetScreen,
    pub selected: usize,
    pub global: Vec<PromptSnippet>,
    pub project: Vec<PromptSnippet>,
    pub original_global: Vec<PromptSnippet>,
    pub original_project: Vec<PromptSnippet>,
    pub project_root: Option<PathBuf>,
    pub editor_name: String,
    pub editor_name_cursor: usize,
    pub editor_prompt: String,
    pub editor_prompt_cursor: usize,
    pub editor_scope: SnippetScope,
    pub editor_field: SnippetEditorField,
    pub editing: Option<(SnippetScope, usize)>,
    pub error: Option<String>,
}

pub struct SnippetUpdate {
    pub global: Vec<PromptSnippet>,
    pub project: Vec<PromptSnippet>,
    pub original_global: Vec<PromptSnippet>,
    pub original_project: Vec<PromptSnippet>,
    pub project_root: Option<PathBuf>,
    pub global_dirty: bool,
    pub project_dirty: bool,
}

impl SnippetModal {
    pub fn new(
        global: Vec<PromptSnippet>,
        project: Vec<PromptSnippet>,
        project_root: Option<PathBuf>,
    ) -> Self {
        Self {
            screen: SnippetScreen::List,
            selected: 0,
            original_global: global.clone(),
            original_project: project.clone(),
            global,
            project,
            project_root,
            editor_name: String::new(),
            editor_name_cursor: 0,
            editor_prompt: String::new(),
            editor_prompt_cursor: 0,
            editor_scope: SnippetScope::Global,
            editor_field: SnippetEditorField::Name,
            editing: None,
            error: None,
        }
    }

    pub fn len(&self) -> usize {
        self.global.len() + self.project.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn entry(&self, index: usize) -> Option<(SnippetScope, usize, &PromptSnippet)> {
        if let Some(snippet) = self.global.get(index) {
            return Some((SnippetScope::Global, index, snippet));
        }
        let project_index = index.checked_sub(self.global.len())?;
        self.project
            .get(project_index)
            .map(|snippet| (SnippetScope::Project, project_index, snippet))
    }

    pub fn selected_entry(&self) -> Option<(SnippetScope, usize, &PromptSnippet)> {
        self.entry(self.selected)
    }

    pub fn select_previous(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn select_next(&mut self) {
        self.selected = (self.selected + 1).min(self.len().saturating_sub(1));
    }

    pub fn begin_add(&mut self) {
        self.screen = SnippetScreen::Editor;
        self.editor_name.clear();
        self.editor_name_cursor = 0;
        self.editor_prompt.clear();
        self.editor_prompt_cursor = 0;
        self.editor_scope = SnippetScope::Global;
        self.editor_field = SnippetEditorField::Name;
        self.editing = None;
        self.error = None;
    }

    pub fn begin_edit(&mut self) {
        let Some((scope, index, snippet)) = self.selected_entry() else {
            return;
        };
        let name = snippet.name.clone();
        let prompt = snippet.prompt.clone();
        self.screen = SnippetScreen::Editor;
        self.editor_name = name;
        self.editor_name_cursor = self.editor_name.chars().count();
        self.editor_prompt = prompt;
        self.editor_prompt_cursor = self.editor_prompt.chars().count();
        self.editor_scope = scope;
        self.editor_field = SnippetEditorField::Name;
        self.editing = Some((scope, index));
        self.error = None;
    }

    pub fn begin_delete(&mut self) {
        if !self.is_empty() {
            self.screen = SnippetScreen::ConfirmDelete;
            self.error = None;
        }
    }

    pub fn cancel_subscreen(&mut self) {
        self.screen = SnippetScreen::List;
        self.error = None;
    }

    pub fn insert_editor_text(&mut self, text: &str) {
        let (value, cursor) = match self.editor_field {
            SnippetEditorField::Name => (&mut self.editor_name, &mut self.editor_name_cursor),
            SnippetEditorField::Prompt => (&mut self.editor_prompt, &mut self.editor_prompt_cursor),
            SnippetEditorField::Scope => return,
        };
        let byte = byte_at_char(value, *cursor);
        value.insert_str(byte, text);
        *cursor += text.chars().count();
        self.error = None;
    }

    pub fn backspace_editor(&mut self) {
        let (value, cursor) = match self.editor_field {
            SnippetEditorField::Name => (&mut self.editor_name, &mut self.editor_name_cursor),
            SnippetEditorField::Prompt => (&mut self.editor_prompt, &mut self.editor_prompt_cursor),
            SnippetEditorField::Scope => return,
        };
        if *cursor == 0 {
            return;
        }
        let start = byte_at_char(value, *cursor - 1);
        let end = byte_at_char(value, *cursor);
        value.replace_range(start..end, "");
        *cursor -= 1;
        self.error = None;
    }

    pub fn delete_editor(&mut self) {
        let (value, cursor) = match self.editor_field {
            SnippetEditorField::Name => (&mut self.editor_name, &mut self.editor_name_cursor),
            SnippetEditorField::Prompt => (&mut self.editor_prompt, &mut self.editor_prompt_cursor),
            SnippetEditorField::Scope => return,
        };
        if *cursor >= value.chars().count() {
            return;
        }
        let start = byte_at_char(value, *cursor);
        let end = byte_at_char(value, *cursor + 1);
        value.replace_range(start..end, "");
        self.error = None;
    }

    /// Ctrl+W / Ctrl+Backspace, matching the scratchpad: at the start of a prompt
    /// line this only joins it onto the previous one instead of eating a whole word.
    pub fn delete_previous_word_editor(&mut self) {
        let (value, cursor) = match self.editor_field {
            SnippetEditorField::Name => (&mut self.editor_name, &mut self.editor_name_cursor),
            SnippetEditorField::Prompt => (&mut self.editor_prompt, &mut self.editor_prompt_cursor),
            SnippetEditorField::Scope => return,
        };
        let chars: Vec<char> = value.chars().collect();
        let end = (*cursor).min(chars.len());
        if end == 0 {
            return;
        }
        let line_start = chars[..end]
            .iter()
            .rposition(|character| *character == '\n')
            .map(|position| position + 1)
            .unwrap_or(0);
        let start = if end == line_start {
            end - 1
        } else {
            line_start + text::previous_word_start(&chars[line_start..end], end - line_start)
        };
        value.replace_range(byte_at_char(value, start)..byte_at_char(value, end), "");
        *cursor = start;
        self.error = None;
    }

    /// Ctrl+Delete, the forward mirror of [`Self::delete_previous_word_editor`].
    pub fn delete_next_word_editor(&mut self) {
        let (value, cursor) = match self.editor_field {
            SnippetEditorField::Name => (&mut self.editor_name, &mut self.editor_name_cursor),
            SnippetEditorField::Prompt => (&mut self.editor_prompt, &mut self.editor_prompt_cursor),
            SnippetEditorField::Scope => return,
        };
        let chars: Vec<char> = value.chars().collect();
        let start = (*cursor).min(chars.len());
        if start == chars.len() {
            return;
        }
        let line_end = chars[start..]
            .iter()
            .position(|character| *character == '\n')
            .map(|offset| start + offset)
            .unwrap_or(chars.len());
        let end = if start == line_end {
            start + 1
        } else {
            start + text::next_word_end(&chars[start..line_end], 0)
        };
        value.replace_range(byte_at_char(value, start)..byte_at_char(value, end), "");
        *cursor = start;
        self.error = None;
    }

    pub fn move_editor_cursor(&mut self, amount: isize) {
        let (value, cursor) = match self.editor_field {
            SnippetEditorField::Name => (&self.editor_name, &mut self.editor_name_cursor),
            SnippetEditorField::Prompt => (&self.editor_prompt, &mut self.editor_prompt_cursor),
            SnippetEditorField::Scope => return,
        };
        *cursor = if amount < 0 {
            cursor.saturating_sub(amount.unsigned_abs())
        } else {
            (*cursor + amount as usize).min(value.chars().count())
        };
    }

    pub fn move_editor_line_boundary(&mut self, end: bool) {
        let (value, cursor) = match self.editor_field {
            SnippetEditorField::Name => (&self.editor_name, &mut self.editor_name_cursor),
            SnippetEditorField::Prompt => (&self.editor_prompt, &mut self.editor_prompt_cursor),
            SnippetEditorField::Scope => return,
        };
        let chars: Vec<char> = value.chars().collect();
        *cursor = if end {
            chars[*cursor..]
                .iter()
                .position(|character| *character == '\n')
                .map(|offset| *cursor + offset)
                .unwrap_or(chars.len())
        } else {
            chars[..*cursor]
                .iter()
                .rposition(|character| *character == '\n')
                .map(|position| position + 1)
                .unwrap_or(0)
        };
    }

    pub fn move_prompt_cursor_vertical(&mut self, down: bool) {
        if self.editor_field != SnippetEditorField::Prompt {
            return;
        }
        let chars: Vec<char> = self.editor_prompt.chars().collect();
        let cursor = self.editor_prompt_cursor.min(chars.len());
        let line_start = chars[..cursor]
            .iter()
            .rposition(|character| *character == '\n')
            .map(|position| position + 1)
            .unwrap_or(0);
        let column = cursor - line_start;
        if down {
            let Some(line_end_offset) = chars[cursor..]
                .iter()
                .position(|character| *character == '\n')
            else {
                return;
            };
            let next_start = cursor + line_end_offset + 1;
            let next_end = chars[next_start..]
                .iter()
                .position(|character| *character == '\n')
                .map(|offset| next_start + offset)
                .unwrap_or(chars.len());
            self.editor_prompt_cursor = next_start + column.min(next_end - next_start);
        } else {
            if line_start == 0 {
                return;
            }
            let previous_end = line_start - 1;
            let previous_start = chars[..previous_end]
                .iter()
                .rposition(|character| *character == '\n')
                .map(|position| position + 1)
                .unwrap_or(0);
            self.editor_prompt_cursor = previous_start + column.min(previous_end - previous_start);
        }
    }
}

/// The digit labelling `index` in the list, or `None` past the tenth snippet.
///
/// Numbering is absolute rather than per-screen so a snippet keeps the same digit
/// once the list scrolls.
pub fn quick_use_label(index: usize) -> Option<char> {
    match index {
        0..=8 => char::from_digit(index as u32 + 1, 10),
        9 => Some('0'),
        _ => None,
    }
}

/// Inverse of [`quick_use_label`]: the snippet a pressed digit refers to.
pub fn quick_use_index(digit: char) -> Option<usize> {
    match digit {
        '0' => Some(9),
        '1'..='9' => digit.to_digit(10).map(|value| value as usize - 1),
        _ => None,
    }
}

fn byte_at_char(text: &str, index: usize) -> usize {
    text.char_indices()
        .nth(index)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snippet(name: &str) -> PromptSnippet {
        PromptSnippet {
            name: name.to_string(),
            prompt: format!("{name} prompt"),
        }
    }

    #[test]
    fn combined_list_keeps_global_before_project_and_maps_indices() {
        let modal = SnippetModal::new(
            vec![snippet("global one"), snippet("global two")],
            vec![snippet("project")],
            Some(PathBuf::from("repo")),
        );

        assert_eq!(modal.entry(0).unwrap().0, SnippetScope::Global);
        assert_eq!(modal.entry(1).unwrap().1, 1);
        assert_eq!(modal.entry(2).unwrap().0, SnippetScope::Project);
        assert_eq!(modal.entry(2).unwrap().1, 0);
    }

    #[test]
    fn add_defaults_to_global_and_edit_retains_scope() {
        let mut modal = SnippetModal::new(
            Vec::new(),
            vec![snippet("project")],
            Some(PathBuf::from("repo")),
        );
        modal.begin_add();
        assert_eq!(modal.editor_scope, SnippetScope::Global);

        modal.cancel_subscreen();
        modal.begin_edit();
        assert_eq!(modal.editor_scope, SnippetScope::Project);
        assert_eq!(modal.editor_name, "project");
    }

    #[test]
    fn editor_field_order_matches_the_visual_layout() {
        assert_eq!(
            SnippetEditorField::Name.next(true),
            SnippetEditorField::Scope
        );
        assert_eq!(
            SnippetEditorField::Scope.next(true),
            SnippetEditorField::Prompt
        );
        assert_eq!(
            SnippetEditorField::Prompt.next(true),
            SnippetEditorField::Name
        );

        assert_eq!(
            SnippetEditorField::Name.next(false),
            SnippetEditorField::Prompt
        );
        assert_eq!(
            SnippetEditorField::Prompt.next(false),
            SnippetEditorField::Scope
        );
        assert_eq!(
            SnippetEditorField::Scope.next(false),
            SnippetEditorField::Name
        );
    }

    #[test]
    fn editor_inserts_and_deletes_at_unicode_character_boundaries() {
        let mut modal = SnippetModal::new(Vec::new(), Vec::new(), None);
        modal.begin_add();
        modal.editor_name = "a🚀c".to_string();
        modal.editor_name_cursor = 2;

        modal.insert_editor_text("b");
        assert_eq!(modal.editor_name, "a🚀bc");
        modal.backspace_editor();
        assert_eq!(modal.editor_name, "a🚀c");
        modal.move_editor_cursor(-1);
        modal.delete_editor();
        assert_eq!(modal.editor_name, "ac");
    }

    #[test]
    fn quick_use_digits_round_trip_and_stop_after_ten_snippets() {
        assert_eq!(quick_use_label(0), Some('1'));
        assert_eq!(quick_use_label(8), Some('9'));
        assert_eq!(quick_use_label(9), Some('0'));
        assert_eq!(quick_use_label(10), None);

        for index in 0..10 {
            let digit = quick_use_label(index).expect("slot has a digit");
            assert_eq!(quick_use_index(digit), Some(index));
        }
        assert_eq!(quick_use_index('a'), None);
    }

    #[test]
    fn ctrl_w_deletes_the_previous_word_in_both_editor_fields() {
        let mut modal = SnippetModal::new(Vec::new(), Vec::new(), None);
        modal.begin_add();
        modal.editor_name = "first second   ".to_string();
        modal.editor_name_cursor = 15;
        modal.delete_previous_word_editor();
        assert_eq!(modal.editor_name, "first ");
        assert_eq!(modal.editor_name_cursor, 6);

        modal.editor_field = SnippetEditorField::Prompt;
        modal.editor_prompt = "review the diff".to_string();
        modal.editor_prompt_cursor = 15;
        modal.delete_previous_word_editor();
        assert_eq!(modal.editor_prompt, "review the ");
        assert_eq!(modal.editor_prompt_cursor, 11);
    }

    #[test]
    fn ctrl_delete_removes_the_next_word_and_its_trailing_space() {
        let mut modal = SnippetModal::new(Vec::new(), Vec::new(), None);
        modal.begin_add();
        modal.editor_field = SnippetEditorField::Prompt;
        modal.editor_prompt = "first second third".to_string();
        modal.editor_prompt_cursor = 6;

        modal.delete_next_word_editor();

        assert_eq!(modal.editor_prompt, "first third");
        assert_eq!(modal.editor_prompt_cursor, 6);
    }

    #[test]
    fn word_deletion_only_joins_lines_at_a_prompt_line_boundary() {
        let mut modal = SnippetModal::new(Vec::new(), Vec::new(), None);
        modal.begin_add();
        modal.editor_field = SnippetEditorField::Prompt;
        modal.editor_prompt = "first\nsecond".to_string();
        modal.editor_prompt_cursor = 6;

        modal.delete_previous_word_editor();
        assert_eq!(modal.editor_prompt, "firstsecond");
        assert_eq!(modal.editor_prompt_cursor, 5);

        modal.editor_prompt = "first\nsecond".to_string();
        modal.editor_prompt_cursor = 5;
        modal.delete_next_word_editor();
        assert_eq!(modal.editor_prompt, "firstsecond");
        assert_eq!(modal.editor_prompt_cursor, 5);
    }

    #[test]
    fn word_deletion_respects_unicode_boundaries_and_field_edges() {
        let mut modal = SnippetModal::new(Vec::new(), Vec::new(), None);
        modal.begin_add();
        modal.editor_name = "🚀 launch".to_string();
        modal.editor_name_cursor = 9;
        modal.delete_previous_word_editor();
        assert_eq!(modal.editor_name, "🚀 ");

        modal.editor_name_cursor = 0;
        modal.delete_previous_word_editor();
        assert_eq!(modal.editor_name, "🚀 ", "no-op at the start of the field");

        modal.editor_name_cursor = modal.editor_name.chars().count();
        modal.delete_next_word_editor();
        assert_eq!(modal.editor_name, "🚀 ", "no-op at the end of the field");

        modal.editor_field = SnippetEditorField::Scope;
        modal.delete_previous_word_editor();
        modal.delete_next_word_editor();
        assert_eq!(modal.editor_name, "🚀 ", "scope field has no text to edit");
    }

    #[test]
    fn prompt_cursor_moves_vertically_while_retaining_its_column() {
        let mut modal = SnippetModal::new(Vec::new(), Vec::new(), None);
        modal.begin_add();
        modal.editor_field = SnippetEditorField::Prompt;
        modal.editor_prompt = "abcd\nxy\n12345".to_string();
        modal.editor_prompt_cursor = 3;

        modal.move_prompt_cursor_vertical(true);
        assert_eq!(
            modal.editor_prompt_cursor, 7,
            "clamped to short second line"
        );
        modal.move_prompt_cursor_vertical(true);
        assert_eq!(modal.editor_prompt_cursor, 10, "same column on third line");
        modal.move_prompt_cursor_vertical(false);
        assert_eq!(modal.editor_prompt_cursor, 7);
    }
}
