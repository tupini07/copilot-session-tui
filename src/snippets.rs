use crate::config::PromptSnippet;
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
