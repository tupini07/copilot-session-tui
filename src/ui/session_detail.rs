use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::App;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme();
    let block = Block::default()
        .title(" Details ")
        .borders(Borders::ALL)
        .style(Style::default().fg(theme.text).bg(theme.background))
        .border_style(Style::default().fg(theme.muted));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let session = match app.selected_session() {
        Some(s) => s,
        None => {
            let empty = Paragraph::new("  Select a session to view details").style(
                Style::default()
                    .fg(super::semantic_foreground_on(
                        theme,
                        theme.muted,
                        theme.background,
                    ))
                    .bg(theme.background),
            );
            f.render_widget(empty, inner);
            return;
        }
    };

    let mut lines: Vec<Line> = Vec::new();

    // Name (full, untruncated)
    lines.push(Line::from(vec![
        Span::styled(
            "  Name: ",
            Style::default()
                .fg(super::semantic_foreground_on(
                    theme,
                    theme.warning,
                    theme.background,
                ))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            session.display_name(),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
    ]));

    lines.push(Line::from(""));

    // ID
    lines.push(Line::from(vec![
        Span::styled(
            "  ID: ",
            Style::default()
                .fg(super::semantic_foreground_on(
                    theme,
                    theme.warning,
                    theme.background,
                ))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(&session.id, Style::default().fg(theme.text)),
    ]));

    lines.push(Line::from(""));

    // Project / CWD
    lines.push(Line::from(vec![
        Span::styled(
            "  Project: ",
            Style::default()
                .fg(super::semantic_foreground_on(
                    theme,
                    theme.warning,
                    theme.background,
                ))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            &session.cwd,
            Style::default().fg(super::semantic_foreground_on(
                theme,
                theme.directory,
                theme.background,
            )),
        ),
    ]));

    lines.push(Line::from(""));

    // Created
    if let Some(created) = session.created_at {
        lines.push(Line::from(vec![
            Span::styled(
                "  Created: ",
                Style::default()
                    .fg(super::semantic_foreground_on(
                        theme,
                        theme.warning,
                        theme.background,
                    ))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                created.format("%b %d, %Y %I:%M %p").to_string(),
                Style::default().fg(theme.text),
            ),
        ]));
    }

    // Last used
    lines.push(Line::from(vec![
        Span::styled(
            "  Last used: ",
            Style::default()
                .fg(super::semantic_foreground_on(
                    theme,
                    theme.warning,
                    theme.background,
                ))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(session.relative_time(), Style::default().fg(theme.text)),
    ]));

    // Status
    let status = if session.is_active {
        Span::styled(
            "● Active",
            Style::default()
                .fg(super::semantic_foreground_on(
                    theme,
                    theme.success,
                    theme.background,
                ))
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(
            "○ Inactive",
            Style::default().fg(super::semantic_foreground_on(
                theme,
                theme.inactive,
                theme.background,
            )),
        )
    };
    lines.push(Line::from(vec![
        Span::styled(
            "  Status: ",
            Style::default()
                .fg(super::semantic_foreground_on(
                    theme,
                    theme.warning,
                    theme.background,
                ))
                .add_modifier(Modifier::BOLD),
        ),
        status,
    ]));

    lines.push(Line::from(""));

    // Session stats
    if session.turn_count > 0 || session.tool_call_count > 0 {
        lines.push(Line::from(vec![
            Span::styled(
                "  Stats: ",
                Style::default()
                    .fg(super::semantic_foreground_on(
                        theme,
                        theme.warning,
                        theme.background,
                    ))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(
                    "{} turns, {} tool calls",
                    session.turn_count, session.tool_call_count
                ),
                Style::default().fg(theme.text),
            ),
        ]));
        lines.push(Line::from(""));
    }

    // Edited files
    if !session.edited_files.is_empty() {
        lines.push(Line::from(Span::styled(
            "  ── Edited Files ──",
            Style::default()
                .fg(super::semantic_foreground_on(
                    theme,
                    theme.accent,
                    theme.background,
                ))
                .add_modifier(Modifier::BOLD),
        )));

        let max_files = 12;
        for (i, file) in session.edited_files.iter().enumerate() {
            if i >= max_files {
                lines.push(Line::from(Span::styled(
                    format!("  ... and {} more", session.edited_files.len() - max_files),
                    Style::default().fg(super::semantic_foreground_on(
                        theme,
                        theme.muted,
                        theme.background,
                    )),
                )));
                break;
            }
            // Show just the filename or relative path
            let display = shorten_path(file);
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    "• ",
                    Style::default().fg(super::semantic_foreground_on(
                        theme,
                        theme.muted,
                        theme.background,
                    )),
                ),
                Span::styled(display, Style::default().fg(theme.text)),
            ]));
        }

        lines.push(Line::from(""));
    }

    // Last user message
    if let Some(ref msg) = session.last_user_message {
        lines.push(Line::from(Span::styled(
            "  ── Last Message ──",
            Style::default()
                .fg(super::semantic_foreground_on(
                    theme,
                    theme.accent,
                    theme.background,
                ))
                .add_modifier(Modifier::BOLD),
        )));

        // Word-wrap the message preview
        let max_width = (inner.width as usize).saturating_sub(4);
        let wrapped = textwrap(msg, max_width);
        for line_text in wrapped.iter().take(4) {
            lines.push(Line::from(Span::styled(
                format!("  {}", line_text),
                Style::default().fg(super::semantic_foreground_on(
                    theme,
                    theme.muted,
                    theme.background,
                )),
            )));
        }
    }

    let paragraph = Paragraph::new(lines)
        .style(Style::default().fg(theme.text).bg(theme.background))
        .wrap(Wrap { trim: false });
    f.render_widget(paragraph, inner);
}

fn shorten_path(path: &str) -> String {
    // Try to show just the last 2-3 path components
    let parts: Vec<&str> = path.split(['/', '\\']).collect();
    if parts.len() <= 3 {
        parts.join("/")
    } else {
        format!(".../{}", parts[parts.len() - 3..].join("/"))
    }
}

fn textwrap(text: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return vec![text.to_string()];
    }
    let mut result = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current = word.to_string();
        } else if crate::text::display_width(&current) + 1 + crate::text::display_width(word)
            <= max_width
        {
            current.push(' ');
            current.push_str(word);
        } else {
            result.push(current);
            current = word.to_string();
        }
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::UserConfig;
    use crate::theme::ThemeName;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn catppuccin_latte_empty_detail_paints_its_background_and_text() {
        let app = App::new(
            Vec::new(),
            UserConfig {
                theme: ThemeName::CatppuccinLatte,
                ..UserConfig::default()
            },
        );
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();

        terminal
            .draw(|frame| draw(frame, &app, frame.area()))
            .unwrap();

        let theme = app.theme();
        let buffer = terminal.backend().buffer();
        assert!(buffer
            .content()
            .iter()
            .all(|cell| cell.bg == theme.background));
        assert_eq!(buffer[(3, 1)].symbol(), "S");
        assert_eq!(buffer[(3, 1)].fg, theme.text);
    }
}
