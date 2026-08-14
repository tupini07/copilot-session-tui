use edtui::{EditorTheme, EditorView, LineNumbers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::scratchpad::Scratchpad;

pub fn draw(f: &mut Frame, scratchpad: &mut Scratchpad) {
    draw_in(f, scratchpad, f.area(), true);
}

pub fn draw_in(f: &mut Frame, scratchpad: &mut Scratchpad, area: Rect, focused: bool) {
    let [title_area, editor_area, status_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(2),
    ])
    .areas(area);

    let dirty = if scratchpad.is_dirty() {
        " • modified"
    } else {
        ""
    };
    let title = Paragraph::new(Line::from(vec![
        Span::styled(
            " Scratchpad ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("  {}{dirty}", scratchpad.session_name)),
    ]));
    f.render_widget(title, title_area);

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
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color)),
        )
        .hide_status_line();
    let editor = EditorView::new(&mut scratchpad.state)
        .theme(theme)
        .line_numbers(LineNumbers::None)
        .wrap(true)
        .tab_width(2);
    f.render_widget(editor, editor_area);

    let first_line = Line::from(vec![
        key("Esc"),
        Span::raw(" Save/close  "),
        key("Ctrl+S"),
        Span::raw(" Save  "),
        key("Shift+Arrows"),
        Span::raw(" Select  "),
        key("Ctrl+C/X/V"),
        Span::raw(" Copy/cut/paste"),
    ]);
    let second_line = if let Some(message) = &scratchpad.status_message {
        Line::from(Span::styled(
            format!(" {message}"),
            Style::default().fg(Color::Yellow),
        ))
    } else {
        Line::from(vec![
            Span::raw(" "),
            key("Ctrl+Z/Y"),
            Span::raw(" Undo/redo  "),
            key("Ctrl+Shift+K"),
            Span::raw(" Delete line  "),
            key("Alt+↑/↓"),
            Span::raw(" Move line"),
        ])
    };
    f.render_widget(Paragraph::new(vec![first_line, second_line]), status_area);
}

fn key(value: &str) -> Span<'_> {
    Span::styled(
        value,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
}
