use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::{App, Mode};

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme();
    let chrome_text = super::foreground_on(theme, theme.chrome_bg);
    let line1 = match app.mode {
        Mode::Search => Line::from(vec![
            Span::styled(
                " / ",
                Style::default()
                    .fg(super::badge_foreground(theme, theme.warning))
                    .bg(theme.warning),
            ),
            Span::raw(" "),
            Span::styled(
                &app.search_query,
                Style::default().fg(super::semantic_foreground_on(
                    theme,
                    theme.warning,
                    theme.chrome_bg,
                )),
            ),
            Span::styled(
                "█",
                Style::default().fg(super::semantic_foreground_on(
                    theme,
                    theme.warning,
                    theme.chrome_bg,
                )),
            ),
            Span::raw("  "),
            key_span("Enter", theme),
            Span::raw(" confirm  "),
            key_span("Esc", theme),
            Span::raw(" cancel"),
        ]),
        Mode::Normal => {
            let mut spans = vec![
                Span::raw(" "),
                key_span("↑↓", theme),
                Span::raw(" Navigate  "),
                key_span("Enter", theme),
                Span::raw(" Resume  "),
                key_span("r", theme),
                Span::raw(" Rename  "),
                key_span("d", theme),
                Span::raw(" Delete  "),
                key_span("e", theme),
                Span::raw(" Scratchpad  "),
                key_span("/", theme),
                Span::raw(" Search  "),
                key_span("f", theme),
                Span::raw(" Filter  "),
                key_span("s", theme),
                Span::raw(" Sort"),
            ];
            if let Some(prefix) = app.prefix_label() {
                spans.push(Span::raw("  │  "));
                if app.help_pending() {
                    spans.push(Span::styled(
                        "Help: e scratchpad  Esc cancel",
                        Style::default()
                            .fg(super::badge_foreground(theme, theme.accent))
                            .bg(theme.accent)
                            .add_modifier(Modifier::BOLD),
                    ));
                } else if app.github_prefix_pending() {
                    spans.push(Span::styled(
                        "GitHub: i inspect  Esc cancel",
                        Style::default()
                            .fg(super::badge_foreground(theme, theme.accent))
                            .bg(theme.accent)
                            .add_modifier(Modifier::BOLD),
                    ));
                } else if app.prefix_pending() {
                    spans.push(Span::styled(
                        format!("{prefix} …"),
                        Style::default()
                            .fg(super::badge_foreground(theme, theme.accent))
                            .bg(theme.accent)
                            .add_modifier(Modifier::BOLD),
                    ));
                } else {
                    let running = app.running_pane_count();
                    let label = if running > 0 {
                        format!("{prefix} w  {running} running")
                    } else {
                        format!("mux {prefix}")
                    };
                    spans.push(Span::styled(
                        label,
                        Style::default().fg(super::semantic_foreground_on(
                            theme,
                            theme.accent,
                            theme.chrome_bg,
                        )),
                    ));
                }
            }
            Line::from(spans)
        }
        _ => Line::from(""),
    };

    let line2 = match app.mode {
        Mode::Normal => {
            let mut spans = vec![
                Span::raw(" "),
                key_span("c", theme),
                Span::raw(" Clear filter  "),
                key_span("n", theme),
                Span::raw(" New  "),
                key_span("N", theme),
                Span::raw(" Worktree  "),
                key_span("Space", theme),
                Span::raw(" Favorite  "),
                key_span("T", theme),
                Span::raw(" Favorite tabs  "),
                key_span(",", theme),
                Span::raw(" Global settings  "),
                key_span(".", theme),
                Span::raw(" Project settings  "),
                key_span("?", theme),
                Span::raw(" Help  "),
                key_span("q", theme),
                Span::raw(" Quit"),
            ];
            if let Some(info) = app
                .update_info
                .as_ref()
                .filter(|_| app.update_install_receiver.is_none())
            {
                spans.push(Span::raw("  │  "));
                spans.push(Span::styled(
                    format!("⬆ v{} → v{} ", info.current_version, info.latest_version),
                    Style::default()
                        .fg(super::semantic_foreground_on(
                            theme,
                            theme.success,
                            theme.chrome_bg,
                        ))
                        .add_modifier(Modifier::BOLD),
                ));
                spans.push(key_span("u", theme));
                spans.push(Span::raw(" Update"));
            }
            if let Some(ref msg) = app.status_message {
                spans.push(Span::raw("  │  "));
                spans.push(Span::styled(
                    msg.as_str(),
                    Style::default()
                        .fg(super::semantic_foreground_on(
                            theme,
                            theme.warning,
                            theme.chrome_bg,
                        ))
                        .add_modifier(Modifier::BOLD),
                ));
            }
            Line::from(spans)
        }
        _ => {
            if let Some(ref msg) = app.status_message {
                Line::from(Span::styled(
                    format!(" {}", msg),
                    Style::default()
                        .fg(super::semantic_foreground_on(
                            theme,
                            theme.warning,
                            theme.chrome_bg,
                        ))
                        .add_modifier(Modifier::BOLD),
                ))
            } else {
                Line::from("")
            }
        }
    };

    let paragraph = Paragraph::new(vec![line1, line2])
        .style(Style::default().fg(chrome_text).bg(theme.chrome_bg));

    f.render_widget(paragraph, area);
}

fn key_span(key: &str, theme: crate::theme::Theme) -> Span<'_> {
    Span::styled(
        key,
        Style::default()
            .fg(super::semantic_foreground_on(
                theme,
                theme.accent_alt,
                theme.chrome_bg,
            ))
            .add_modifier(Modifier::BOLD),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::UserConfig;
    use crate::theme::ThemeName;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn session_list_footer_advertises_favorite_tabs_shortcut() {
        let app = App::new(Vec::new(), UserConfig::default());
        let backend = TestBackend::new(180, 2);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| draw(frame, &app, frame.area()))
            .unwrap();

        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("T Favorite tabs"), "got:\n{text}");
    }

    #[test]
    fn solarized_light_status_bar_paints_chrome_and_readable_default_text() {
        let app = App::new(
            Vec::new(),
            UserConfig {
                theme: ThemeName::SolarizedLight,
                ..UserConfig::default()
            },
        );
        let mut terminal = Terminal::new(TestBackend::new(180, 2)).unwrap();

        terminal
            .draw(|frame| draw(frame, &app, frame.area()))
            .unwrap();

        let theme = app.theme();
        let buffer = terminal.backend().buffer();
        assert!(buffer
            .content()
            .iter()
            .all(|cell| cell.bg == theme.chrome_bg));
        let navigate = &buffer[(4, 0)];
        assert_eq!(navigate.symbol(), "N");
        assert_eq!(
            navigate.fg,
            crate::ui::foreground_on(theme, theme.chrome_bg)
        );
    }
}
