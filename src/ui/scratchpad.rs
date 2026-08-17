use edtui::{EditorTheme, EditorView, LineNumbers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use ratatui::Frame;

use crate::scratchpad::Scratchpad;

pub fn draw(f: &mut Frame, scratchpad: &mut Scratchpad) {
    draw_in(f, scratchpad, f.area(), true);
}

pub fn draw_in(f: &mut Frame, scratchpad: &mut Scratchpad, area: Rect, focused: bool) {
    let status = scratchpad
        .status_message
        .as_deref()
        .map(|message| format!(" — {message}"))
        .unwrap_or_default();

    let border_color = if focused {
        Color::Yellow
    } else {
        Color::DarkGray
    };
    let theme = EditorTheme::default()
        .base(Style::default().fg(Color::White).bg(Color::Black))
        .selection_style(Style::default().fg(Color::Black).bg(Color::Cyan))
        .block(
            Block::default()
                .title(format!(" Scratchpad{status} "))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color)),
        )
        .hide_status_line();
    let editor = EditorView::new(&mut scratchpad.state)
        .theme(theme)
        .line_numbers(LineNumbers::None)
        .wrap(true)
        .tab_width(2);
    f.render_widget(editor, area);

    let viewport_lines = area.height.saturating_sub(2) as usize;
    let content_lines = scratchpad.state.lines.len();
    if viewport_lines > 0 && content_lines > viewport_lines {
        let scrollbar_area = Rect {
            y: area.y.saturating_add(1),
            height: area.height.saturating_sub(2),
            ..area
        };
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some("│"))
            .track_style(Style::default().fg(Color::DarkGray))
            .thumb_symbol("█")
            .thumb_style(Style::default().fg(border_color));
        let mut scrollbar_state = ScrollbarState::new(content_lines)
            .position(scratchpad.state.cursor.row)
            .viewport_content_length(viewport_lines);
        f.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
    }
}

pub fn draw_help(f: &mut Frame, area: Rect) {
    let width = area.width.saturating_sub(4).min(54);
    let height = area.height.saturating_sub(2).min(16);
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    let lines = vec![
        help_line("Esc", "Close help"),
        help_line("Ctrl+S", "Save"),
        help_line("Shift+Arrows", "Select text"),
        help_line("Ctrl+A", "Select all"),
        help_line("Ctrl+C/X/V", "Copy / cut / paste"),
        help_line("Ctrl+Z/Y", "Undo / redo"),
        help_line("Ctrl+W/Ctrl+BS", "Delete previous word"),
        help_line("Ctrl+Delete", "Delete next word"),
        help_line("Ctrl/Alt+L", "Add / toggle checkbox"),
        help_line("Shift+Tab", "Dedent line / selection"),
        help_line("Ctrl+Shift+K", "Delete current line"),
        help_line("Alt+Up/Down", "Move current line"),
        Line::from(""),
        Line::from(" Enter continues bullets, tasks, and numbered lists."),
    ];
    let block = Block::default()
        .title(" Scratchpad Help ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));
    f.render_widget(Clear, popup);
    f.render_widget(Paragraph::new(lines).block(block), popup);
}

fn help_line(shortcut: &str, description: &str) -> Line<'static> {
    Line::from(vec![
        Span::raw(" "),
        Span::styled(
            format!("{shortcut:<14}"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(description.to_string()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use edtui::{Index2, Lines};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn render(content: &str, cursor_row: usize) -> String {
        let mut scratchpad = Scratchpad::open("scrollbar-render-test").unwrap();
        scratchpad.state.lines = Lines::from(content);
        scratchpad.state.cursor = Index2::new(cursor_row, 0);
        let backend = TestBackend::new(20, 8);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| draw(frame, &mut scratchpad)).unwrap();

        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn long_scratchpad_renders_cursor_position_scrollbar() {
        let content = (1..=20)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");

        let at_top = render(&content, 0);
        let at_bottom = render(&content, 19);

        assert_eq!(at_top.chars().nth(39), Some('█'), "got:\n{at_top}");
        assert_eq!(at_bottom.chars().nth(139), Some('█'), "got:\n{at_bottom}");
        assert_ne!(at_top, at_bottom);
    }

    #[test]
    fn short_scratchpad_does_not_render_scrollbar() {
        let rendered = render("first\nsecond", 1);

        assert!(!rendered.contains('█'), "got:\n{rendered}");
    }

    #[test]
    fn long_lines_wrap_at_word_boundaries() {
        let rendered = render("currently trying to get", 0);
        let cells: Vec<char> = rendered.chars().collect();
        let rows: Vec<String> = cells.chunks(20).map(|row| row.iter().collect()).collect();

        assert!(rows[1].contains("currently trying"), "got:\n{rendered}");
        assert!(!rows[1].contains("trying t"), "got:\n{rendered}");
        assert!(rows[2].contains("to get"), "got:\n{rendered}");
    }

    #[test]
    fn arrows_navigate_visual_rows_inside_a_wrapped_line() {
        let mut scratchpad = Scratchpad::open("soft-wrap-navigation-test").unwrap();
        scratchpad.state.lines = Lines::from("currently trying to get");
        scratchpad.state.cursor = Index2::new(0, 5);
        let backend = TestBackend::new(20, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut scratchpad)).unwrap();

        scratchpad
            .handle_event(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)))
            .unwrap();
        assert_eq!(scratchpad.state.cursor, Index2::new(0, 22));

        scratchpad
            .handle_event(Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)))
            .unwrap();
        assert_eq!(scratchpad.state.cursor, Index2::new(0, 5));

        scratchpad
            .handle_event(Event::Key(KeyEvent::new(
                KeyCode::Down,
                KeyModifiers::SHIFT,
            )))
            .unwrap();
        assert_eq!(scratchpad.state.cursor, Index2::new(0, 22));
        assert!(scratchpad.state.selection.is_some());

        scratchpad
            .handle_event(Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT)))
            .unwrap();
        assert_eq!(scratchpad.state.cursor, Index2::new(0, 5));
    }
}
