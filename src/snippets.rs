use crate::config::PromptSnippet;
use crate::editor::TextEditor;
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

pub struct SnippetModal {
    pub screen: SnippetScreen,
    pub selected: usize,
    pub global: Vec<PromptSnippet>,
    pub project: Vec<PromptSnippet>,
    pub original_global: Vec<PromptSnippet>,
    pub original_project: Vec<PromptSnippet>,
    pub project_root: Option<PathBuf>,
    pub editor_name: TextEditor,
    pub editor_prompt: TextEditor,
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
            editor_name: name_editor(""),
            editor_prompt: prompt_editor(""),
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
        self.editor_name = name_editor("");
        self.editor_prompt = prompt_editor("");
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
        self.editor_name = name_editor(&name);
        self.editor_prompt = prompt_editor(&prompt);
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

    /// The editor behind the focused field, or `None` while the focus is on
    /// the scope toggle, which is not text.
    pub fn focused_editor(&mut self) -> Option<&mut TextEditor> {
        match self.editor_field {
            SnippetEditorField::Name => Some(&mut self.editor_name),
            SnippetEditorField::Prompt => Some(&mut self.editor_prompt),
            SnippetEditorField::Scope => None,
        }
    }
}

/// The name is one line: Enter belongs to the form, and a pasted newline would
/// produce a snippet the list cannot show.
fn name_editor(content: &str) -> TextEditor {
    let mut editor = TextEditor::new(content).single_line();
    editor.move_cursor_to_end();
    editor
}

fn prompt_editor(content: &str) -> TextEditor {
    let mut editor = TextEditor::new(content);
    editor.move_cursor_to_end();
    editor
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
        assert_eq!(modal.editor_name.text(), "project");
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
}
