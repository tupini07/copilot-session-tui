use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use tui_term::widget::PseudoTerminal;

use crate::app::App;
use crate::mux::PaneStatus;

/// Draw the focused session pane plus a one-line status strip.
pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    let Some(mux) = app.mux.as_ref() else {
        return;
    };
    let Some(pane) = mux.focused_pane() else {
        return;
    };

    let terminal_area = layout[0];
    pane.with_screen(|screen| {
        let widget = PseudoTerminal::new(screen);
        f.render_widget(widget, terminal_area);
    });

    // Mirror the child's cursor into the outer terminal so typing feels native.
    if pane.is_running() {
        if let Some((row, col)) = pane.cursor() {
            let x = terminal_area.x.saturating_add(col);
            let y = terminal_area.y.saturating_add(row);
            if x < terminal_area.right() && y < terminal_area.bottom() {
                f.set_cursor_position((x, y));
            }
        }
    }

    let index = mux
        .panes
        .iter()
        .position(|candidate| candidate.id == pane.id)
        .map(|i| i + 1)
        .unwrap_or(1);
    let prefix = mux.prefix.label();

    let mut spans = vec![
        Span::styled(
            format!(" {}:{} ", index, pane.title),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ];

    match pane.status {
        PaneStatus::Running => {
            if mux.prefix_pending {
                spans.push(Span::styled(
                    format!("{prefix} …"),
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::raw("  d detach  n/p switch  x kill  w list"));
            } else {
                spans.push(Span::styled(
                    prefix.clone(),
                    Style::default().fg(Color::Cyan),
                ));
                spans.push(Span::raw(" for commands"));
                if mux.panes.len() > 1 {
                    spans.push(Span::raw(format!("  ·  {} sessions", mux.panes.len())));
                }
            }
        }
        PaneStatus::Exited(code) => {
            let text = match code {
                Some(0) | None => "exited".to_string(),
                Some(code) => format!("exited with code {code}"),
            };
            spans.push(Span::styled(
                format!("{text} — press Enter to close"),
                Style::default().fg(Color::Yellow),
            ));
        }
    }

    let status =
        Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::Rgb(30, 30, 40)));
    f.render_widget(status, layout[1]);
}
