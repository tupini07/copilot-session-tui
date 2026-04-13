use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, Mode};

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let line1 = match app.mode {
        Mode::Search => {
            Line::from(vec![
                Span::styled(" / ", Style::default().fg(Color::Black).bg(Color::Yellow)),
                Span::raw(" "),
                Span::styled(&app.search_query, Style::default().fg(Color::Yellow)),
                Span::styled("█", Style::default().fg(Color::Yellow)),
                Span::raw("  "),
                Span::styled("Enter", Style::default().fg(Color::Cyan)),
                Span::raw(" confirm  "),
                Span::styled("Esc", Style::default().fg(Color::Cyan)),
                Span::raw(" cancel"),
            ])
        }
        Mode::Normal => {
            Line::from(vec![
                Span::raw(" "),
                key_span("↑↓"),
                Span::raw(" Navigate  "),
                key_span("Enter"),
                Span::raw(" Resume  "),
                key_span("r"),
                Span::raw(" Rename  "),
                key_span("d"),
                Span::raw(" Delete  "),
                key_span("/"),
                Span::raw(" Search  "),
                key_span("f"),
                Span::raw(" Filter  "),
                key_span("s"),
                Span::raw(" Sort"),
            ])
        }
        _ => Line::from(""),
    };

    let line2 = match app.mode {
        Mode::Normal => {
            let mut spans = vec![
                Span::raw(" "),
                key_span("c"),
                Span::raw(" Clear filter  "),
                key_span("n"),
                Span::raw(" New session  "),
                key_span(","),
                Span::raw(" Settings  "),
                key_span("?"),
                Span::raw(" Help  "),
                key_span("q"),
                Span::raw(" Quit"),
            ];
            if let Some(ref info) = app.update_info {
                spans.push(Span::raw("  │  "));
                spans.push(Span::styled(
                    format!("⬆ v{} → v{} ", info.current_version, info.latest_version),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ));
                spans.push(key_span("u"));
                spans.push(Span::raw(" Update"));
            }
            if let Some(ref msg) = app.status_message {
                spans.push(Span::raw("  │  "));
                spans.push(Span::styled(
                    msg.as_str(),
                    Style::default().fg(Color::Yellow),
                ));
            }
            Line::from(spans)
        }
        _ => {
            if let Some(ref msg) = app.status_message {
                Line::from(Span::styled(
                    format!(" {}", msg),
                    Style::default().fg(Color::Yellow),
                ))
            } else {
                Line::from("")
            }
        }
    };

    let paragraph = Paragraph::new(vec![line1, line2])
        .style(Style::default().bg(Color::Rgb(30, 30, 40)));

    f.render_widget(paragraph, area);
}

fn key_span(key: &str) -> Span<'_> {
    Span::styled(
        key,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
}
