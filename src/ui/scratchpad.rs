use edtui::{EditorTheme, EditorView, LineNumbers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::scratchpad::Scratchpad;

pub fn draw(f: &mut Frame, scratchpad: &mut Scratchpad) {
    draw_in(f, scratchpad, f.area(), true);
}

pub fn draw_in(f: &mut Frame, scratchpad: &mut Scratchpad, area: Rect, focused: bool) {
    let dirty = if scratchpad.is_dirty() {
        " [modified]"
    } else {
        ""
    };
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
                .title(format!(" Scratchpad{dirty}{status} "))
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
        help_line("Ctrl+Shift+K", "Delete current line"),
        help_line("Alt+Up/Down", "Move current line"),
        Line::from(""),
        Line::from(" Enter continues bullets and numbered lists."),
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
