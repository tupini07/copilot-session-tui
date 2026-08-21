use crate::app::App;
use crate::snippets::{SnippetEditorField, SnippetScope, SnippetScreen};
use crate::text;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

pub fn draw(f: &mut Frame, app: &App) {
    let Some(modal) = app.snippet_modal.as_ref() else {
        return;
    };
    let area = super::popups::centered_rect(72, 76, f.area());
    f.render_widget(Clear, area);
    match modal.screen {
        SnippetScreen::List | SnippetScreen::ConfirmDelete => draw_list(f, app, area),
        SnippetScreen::Editor => draw_editor(f, app, area),
    }
    if modal.screen == SnippetScreen::ConfirmDelete {
        draw_delete_confirm(f, app, area);
    }
}

fn draw_list(f: &mut Frame, app: &App, area: Rect) {
    let modal = app.snippet_modal.as_ref().expect("snippet modal");
    let block = Block::default()
        .title(" Prompt Snippets ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(2),
            Constraint::Length(if modal.error.is_some() { 1 } else { 0 }),
            Constraint::Length(2),
        ])
        .split(inner);

    let preview_width = chunks[0].width.saturating_sub(20) as usize;
    let visible_rows = chunks[0].height.max(1) as usize;
    let start = modal
        .selected
        .saturating_add(1)
        .saturating_sub(visible_rows);
    let end = (start + visible_rows).min(modal.len());
    let items: Vec<ListItem> = (start..end)
        .filter_map(|index| modal.entry(index).map(|entry| (index, entry)))
        .map(|(index, (scope, _, snippet))| {
            let selected = index == modal.selected;
            let style = if selected {
                Style::default()
                    .fg(Color::White)
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            let scope_style = match scope {
                SnippetScope::Global => Style::default().fg(Color::Cyan),
                SnippetScope::Project => Style::default().fg(Color::Yellow),
            };
            let preview = safe_terminal_text(&snippet.prompt).replace(['\r', '\n'], " ↵ ");
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(" {:<7} ", scope.label()),
                    scope_style.add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    text::pad_to_width(&safe_single_line_text(&snippet.name), 24),
                    style,
                ),
                Span::styled(
                    text::truncate_to_width(&preview, preview_width),
                    style.fg(Color::Gray),
                ),
            ]))
        })
        .collect();
    if items.is_empty() {
        f.render_widget(
            Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  No snippets yet — press a to add one.",
                    Style::default().fg(Color::DarkGray),
                )),
            ]),
            chunks[0],
        );
    } else {
        f.render_widget(List::new(items), chunks[0]);
    }

    if let Some(error) = modal.error.as_deref() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" {error}"),
                Style::default().fg(Color::Red),
            ))),
            chunks[1],
        );
    }
    let project = modal
        .project_root
        .as_deref()
        .and_then(|root| root.file_name())
        .and_then(|name| name.to_str())
        .map(|name| format!("  project: {name}"))
        .unwrap_or_else(|| "  no Git project (global snippets only)".to_string());
    f.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                key("↑↓"),
                Span::raw(" select  "),
                key("Enter"),
                Span::raw(" use  "),
                key("a"),
                Span::raw(" add  "),
                key("e"),
                Span::raw(" edit  "),
                key("d"),
                Span::raw(" delete  "),
                key("q/Esc"),
                Span::raw(" close"),
            ]),
            Line::from(Span::styled(project, Style::default().fg(Color::DarkGray))),
        ]),
        chunks[2],
    );
}

fn draw_editor(f: &mut Frame, app: &App, area: Rect) {
    let modal = app.snippet_modal.as_ref().expect("snippet modal");
    let title = if modal.editing.is_some() {
        " Edit Prompt Snippet "
    } else {
        " Add Prompt Snippet "
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(if modal.error.is_some() { 1 } else { 0 }),
            Constraint::Length(2),
        ])
        .split(inner);

    let name = if modal.editor_field == SnippetEditorField::Name {
        with_cursor(
            &safe_single_line_text(&modal.editor_name),
            modal.editor_name_cursor,
        )
    } else {
        safe_single_line_text(&modal.editor_name)
    };
    f.render_widget(
        Paragraph::new(name).block(field_block(
            " Name ",
            modal.editor_field == SnippetEditorField::Name,
        )),
        chunks[0],
    );

    let scope = match modal.editor_scope {
        SnippetScope::Global => " Global — available in every project ",
        SnippetScope::Project => " Project — only this repository ",
    };
    f.render_widget(
        Paragraph::new(scope).block(field_block(
            " Scope (Space or Ctrl+G toggles) ",
            modal.editor_field == SnippetEditorField::Scope,
        )),
        chunks[1],
    );

    let prompt = if modal.editor_field == SnippetEditorField::Prompt {
        with_cursor(
            &safe_terminal_text(&modal.editor_prompt),
            modal.editor_prompt_cursor,
        )
    } else {
        safe_terminal_text(&modal.editor_prompt)
    };
    let prompt_width = chunks[2].width.saturating_sub(2).max(1) as usize;
    let prompt_height = chunks[2].height.saturating_sub(2).max(1) as usize;
    let cursor_row = prompt_cursor_row(
        &modal.editor_prompt,
        modal.editor_prompt_cursor,
        prompt_width,
    );
    let prompt_scroll = cursor_row.saturating_sub(prompt_height.saturating_sub(1));
    f.render_widget(
        Paragraph::new(prompt)
            .wrap(Wrap { trim: false })
            .scroll((u16::try_from(prompt_scroll).unwrap_or(u16::MAX), 0))
            .block(field_block(
                " Prompt ",
                modal.editor_field == SnippetEditorField::Prompt,
            )),
        chunks[2],
    );

    if let Some(error) = modal.error.as_deref() {
        f.render_widget(
            Paragraph::new(Span::styled(error, Style::default().fg(Color::Red))),
            chunks[3],
        );
    }
    f.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                key("Tab/Shift+Tab"),
                Span::raw(" field  "),
                key("Enter"),
                Span::raw(" newline in prompt  "),
                key("Ctrl+S"),
                Span::raw(" save  "),
                key("Esc"),
                Span::raw(" cancel"),
            ]),
            Line::from(Span::styled(
                "Using a snippet pastes it into chat without sending.",
                Style::default().fg(Color::DarkGray),
            )),
        ]),
        chunks[4],
    );
}

fn draw_delete_confirm(f: &mut Frame, app: &App, parent: Rect) {
    let modal = app.snippet_modal.as_ref().expect("snippet modal");
    let name = modal
        .selected_entry()
        .map(|(_, _, snippet)| snippet.name.as_str())
        .unwrap_or("this snippet");
    let area = super::popups::centered_rect(62, 28, parent);
    f.render_widget(Clear, area);
    let block = Block::default()
        .title(" Delete Snippet? ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red));
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(
        Paragraph::new(vec![
            Line::from(""),
            Line::from(vec![
                Span::raw(" Delete "),
                Span::styled(
                    safe_single_line_text(name),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("?"),
            ]),
            Line::from(""),
            Line::from(vec![
                key("y/Enter"),
                Span::raw(" delete permanently  "),
                key("n/Esc"),
                Span::raw(" cancel"),
            ]),
        ]),
        inner,
    );
}

fn field_block(title: &str, active: bool) -> Block<'_> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if active { Color::Cyan } else { Color::DarkGray }))
}

fn key(text: &str) -> Span<'_> {
    Span::styled(
        text,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
}

fn with_cursor(text: &str, cursor: usize) -> String {
    let byte = text
        .char_indices()
        .nth(cursor)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len());
    let mut rendered = text.to_string();
    rendered.insert(byte, '█');
    rendered
}

fn safe_terminal_text(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_control() && !matches!(character, '\r' | '\n' | '\t') {
                '�'
            } else {
                character
            }
        })
        .collect()
}

fn safe_single_line_text(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_control() {
                '�'
            } else {
                character
            }
        })
        .collect()
}

fn prompt_cursor_row(text: &str, cursor: usize, width: usize) -> usize {
    let mut row = 0;
    let mut column = 0;
    for character in text.chars().take(cursor) {
        if character == '\n' {
            row += 1;
            column = 0;
            continue;
        }
        let character_width = text::display_width(&character.to_string());
        if column + character_width > width {
            row += 1;
            column = 0;
        }
        column += character_width;
        if column >= width {
            row += column / width;
            column %= width;
        }
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{PromptSnippet, UserConfig};
    use crate::snippets::SnippetModal;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::path::PathBuf;

    fn render(app: &App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(120, 36)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn list_labels_global_and_project_snippets_and_advertises_actions() {
        let mut app = App::new(Vec::new(), UserConfig::default());
        app.snippet_modal = Some(SnippetModal::new(
            vec![PromptSnippet {
                name: "Review".to_string(),
                prompt: "Review this carefully".to_string(),
            }],
            vec![PromptSnippet {
                name: "Build".to_string(),
                prompt: "Build this repository".to_string(),
            }],
            Some(PathBuf::from("project")),
        ));

        let text = render(&app);

        assert!(text.contains("Prompt Snippets"), "got:\n{text}");
        assert!(text.contains("global"), "got:\n{text}");
        assert!(text.contains("project"), "got:\n{text}");
        assert!(text.contains("Enter use"), "got:\n{text}");
        assert!(text.contains("d delete"), "got:\n{text}");
    }

    #[test]
    fn editor_explains_scope_and_that_use_does_not_send() {
        let mut app = App::new(Vec::new(), UserConfig::default());
        let mut modal = SnippetModal::new(Vec::new(), Vec::new(), None);
        modal.begin_add();
        app.snippet_modal = Some(modal);

        let text = render(&app);

        assert!(text.contains("Add Prompt Snippet"), "got:\n{text}");
        assert!(text.contains("available in every project"), "got:\n{text}");
        assert!(text.contains("without sending"), "got:\n{text}");
        assert!(text.contains("Ctrl+S save"), "got:\n{text}");
    }

    #[test]
    fn prompt_cursor_row_counts_explicit_lines_and_wrapping() {
        assert_eq!(prompt_cursor_row("abcd", 4, 4), 1);
        assert_eq!(prompt_cursor_row("ab\ncd", 5, 20), 1);
        assert_eq!(prompt_cursor_row("🚀🚀", 2, 3), 1);
    }

    #[test]
    fn long_list_keeps_the_selected_snippet_visible() {
        let mut app = App::new(Vec::new(), UserConfig::default());
        let snippets: Vec<PromptSnippet> = (0..40)
            .map(|index| PromptSnippet {
                name: format!("Snippet {index}"),
                prompt: format!("Prompt {index}"),
            })
            .collect();
        let mut modal = SnippetModal::new(snippets, Vec::new(), None);
        modal.selected = 39;
        app.snippet_modal = Some(modal);

        let text = render(&app);

        assert!(text.contains("Snippet 39"), "got:\n{text}");
        assert!(
            !text.contains("Snippet 0"),
            "list should have scrolled:\n{text}"
        );
    }

    #[test]
    fn repository_control_characters_are_never_rendered_verbatim() {
        let mut app = App::new(Vec::new(), UserConfig::default());
        app.snippet_modal = Some(SnippetModal::new(
            Vec::new(),
            vec![PromptSnippet {
                name: "unsafe\u{1b}[2J".to_string(),
                prompt: "prompt\u{7}".to_string(),
            }],
            Some(PathBuf::from("project")),
        ));

        let text = render(&app);

        assert!(!text.contains('\u{1b}'));
        assert!(!text.contains('\u{7}'));
        assert!(text.contains('�'));
    }
}
