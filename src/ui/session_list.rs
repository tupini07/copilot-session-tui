use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem};
use ratatui::Frame;

use crate::app::App;
use crate::text;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Sessions ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.filtered_indices.is_empty() {
        let hint = if app.new_session_dir().is_some() {
            "  No sessions found\n\n  n  Start a new session here\n  N  Start an isolated worktree session"
        } else {
            "  No sessions found"
        };
        let empty =
            ratatui::widgets::Paragraph::new(hint).style(Style::default().fg(Color::DarkGray));
        f.render_widget(empty, inner);
        return;
    }

    let has_project_filter = app.project_filter.is_some();
    let lines_per_item = if has_project_filter { 1 } else { 2 };
    let visible_items = inner.height as usize / lines_per_item;

    let items: Vec<ListItem> = app
        .filtered_indices
        .iter()
        .enumerate()
        .skip(app.scroll_offset)
        .take(visible_items)
        .map(|(display_idx, &real_idx)| {
            let session = &app.sessions[real_idx];
            let is_selected = display_idx == app.selected;

            // Panes we own read differently from a foreign `inuse` lock: ours can be
            // re-attached, theirs cannot.
            let active_indicator = if app.has_running_pane_for(&session.id) {
                Span::styled("▶ ", Style::default().fg(Color::Magenta))
            } else if session.is_active {
                Span::styled("● ", Style::default().fg(Color::Green))
            } else {
                Span::raw("  ")
            };
            let favorite_indicator = if app.is_favorite(&session.id) {
                Span::styled("★ ", Style::default().fg(Color::Yellow))
            } else {
                Span::raw("  ")
            };

            let name = session.display_name();
            // 4 columns for indicators, 1 space + 8 columns for the time column
            let max_name_width = (inner.width as usize).saturating_sub(13);
            let truncated_name = text::truncate_to_width(name, max_name_width);

            let name_style = if is_selected {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            let time = session.relative_time();

            let line = Line::from(vec![
                active_indicator,
                favorite_indicator,
                Span::styled(
                    text::pad_to_width(&truncated_name, max_name_width),
                    name_style,
                ),
                Span::styled(
                    format!(" {:>8}", time),
                    Style::default().fg(Color::DarkGray),
                ),
            ]);

            let lines = if has_project_filter {
                vec![line]
            } else {
                let project = session.project_name();
                let truncated_project = text::truncate_to_width(project, 15);
                let project_line = Line::from(vec![
                    Span::raw("    "),
                    Span::styled(
                        truncated_project,
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM),
                    ),
                ]);
                vec![line, project_line]
            };

            if is_selected {
                ListItem::new(lines).style(Style::default().bg(Color::DarkGray))
            } else {
                ListItem::new(lines)
            }
        })
        .collect();

    let list = List::new(items);
    f.render_widget(list, inner);
}

#[cfg(test)]
pub mod tests {
    use crate::app::App;
    use crate::config::UserConfig;
    use crate::session::Session;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::path::PathBuf;

    fn session_named(name: &str) -> Session {
        Session {
            id: "abcdef123456".to_string(),
            cwd: "C:/Workspace/zazen".to_string(),
            project_root: "C:/Workspace/zazen".to_string(),
            summary: Some(name.to_string()),
            created_at: None,
            updated_at: None,
            is_active: false,
            dir_path: PathBuf::from("."),
            edited_files: Vec::new(),
            last_user_message: None,
            turn_count: 0,
            tool_call_count: 0,
        }
    }

    pub fn render_with(name: &str, width: u16) -> String {
        let mut app = App::new(vec![session_named(name)], UserConfig::default());
        let backend = TestBackend::new(width, 12);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| crate::ui::draw(f, &mut app))
            .expect("draw succeeds");
        let buffer = terminal.backend().buffer().clone();
        buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
    }

    #[test]
    fn an_emoji_name_renders_without_panicking() {
        let text = render_with("🚀 Ship it", 60);
        assert!(text.contains("🚀"), "emoji must survive rendering:\n{text}");
    }

    #[test]
    fn a_long_emoji_name_is_truncated_on_a_character_boundary() {
        // Long enough to force truncation, with the emoji straddling the cut.
        let name = "Publishing to f-droid 🚀 and elsewhere too, at length";
        let text = render_with(name, 40);
        assert!(text.contains("Publishing"), "got:\n{text}");
    }

    /// Rows as grids of cell symbols, so positions can be compared in terminal columns
    /// rather than bytes — an emoji is one cell but four bytes.
    pub fn render_cells(name: &str, width: u16) -> Vec<Vec<String>> {
        let mut app = App::new(vec![session_named(name)], UserConfig::default());
        let backend = TestBackend::new(width, 12);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| crate::ui::draw(f, &mut app))
            .expect("draw succeeds");
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect()
            })
            .collect()
    }

    /// Column at which `needle` starts, counted in cells.
    fn column_of(rows: &[Vec<String>], needle: &str) -> usize {
        for row in rows {
            for start in 0..row.len() {
                if row[start..].concat().starts_with(needle) {
                    return start;
                }
            }
        }
        panic!("{needle:?} was not on screen");
    }

    #[test]
    fn emoji_names_do_not_panic_at_any_terminal_width() {
        // Byte-slicing a name used to abort here; the emoji straddles every cut point.
        let name = "🚀🚀🚀🚀🚀🚀🚀🚀🚀🚀🚀🚀🚀🚀🚀🚀🚀🚀🚀🚀 rocket session";
        for width in 20u16..=140 {
            let rendered = render_with(name, width);
            assert!(!rendered.is_empty(), "width {width} produced no output");
        }
    }

    #[test]
    fn an_emoji_name_keeps_the_timestamp_column_aligned() {
        // The emoji occupies two columns; if it is counted as one char the whole row
        // shifts and the right-hand time column no longer lines up.
        let plain = render_cells("ab plain name", 60);
        let emoji = render_cells("🚀 plain name", 60);

        assert_eq!(
            column_of(&plain, "unknown"),
            column_of(&emoji, "unknown"),
            "the emoji shifted the time column"
        );
    }
}
