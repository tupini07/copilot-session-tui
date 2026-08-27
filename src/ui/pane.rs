use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use std::time::Duration;
use tui_term::widget::PseudoTerminal;

use crate::app::App;
use crate::mux::{PaneStatus, PrefixState};
use crate::text;
use crate::theme::{apply_terminal_theme, fill_area, Theme, ThemeName};
use crate::ui::tabs;

/// Frames of the startup spinner. Braille dots read as motion even in a plain terminal.
const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

/// Progress indicator shown while a freshly spawned session has yet to draw anything.
fn draw_starting(f: &mut Frame, area: Rect, elapsed: Duration, theme: Theme) {
    let frame = SPINNER[(elapsed.as_millis() / 120) as usize % SPINNER.len()];
    let seconds = elapsed.as_secs();

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  {frame}  Starting Copilot…"),
            Style::default()
                .fg(theme.accent_alt)
                .add_modifier(Modifier::BOLD),
        )),
    ];
    // Only mention the wait once it is long enough to be worth reassuring about.
    if seconds >= 3 {
        lines.push(Line::from(Span::styled(
            format!("     {seconds}s — the CLI is still booting"),
            Style::default().fg(theme.muted),
        )));
    }

    let height = lines.len() as u16;
    let box_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: height.min(area.height),
    };
    f.render_widget(Paragraph::new(lines), box_area);
}

pub fn draw_chat(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme();
    let Some(mux) = app.mux.as_ref() else {
        return;
    };
    let Some(pane) = mux.focused_pane() else {
        return;
    };

    let border_color = if app.workspace_focus == crate::app::WorkspaceFocus::Chat {
        theme.accent_alt
    } else {
        theme.inactive
    };
    let block = Block::default()
        .title(" Chat ")
        .borders(Borders::ALL)
        .style(panel_style(theme))
        .border_style(Style::default().fg(border_color));
    let terminal_area = block.inner(area);
    fill_area(f.buffer_mut(), area, theme.background);
    f.render_widget(block, area);

    pane.with_screen(|screen| {
        let widget = PseudoTerminal::new(screen);
        f.render_widget(widget, terminal_area);
        apply_terminal_theme(f.buffer_mut(), terminal_area, theme);
    });
    decorate_references(f, app, terminal_area, theme);

    // Copilot needs a few seconds before it draws anything; without this the pane just
    // looks frozen.
    let starting = pane.is_running() && pane.is_blank();
    if starting {
        draw_starting(f, terminal_area, pane.started_at.elapsed(), theme);
    }

    // Mirror the child's cursor into the outer terminal so typing feels native.
    if pane.is_running() && !starting {
        if let Some((row, col)) = pane.cursor() {
            let x = terminal_area.x.saturating_add(col);
            let y = terminal_area.y.saturating_add(row);
            if x < terminal_area.right() && y < terminal_area.bottom() {
                f.set_cursor_position((x, y));
            }
        }
    }
}

/// Colour and underline `#1234` in the rendered pane according to what it is.
///
/// This works on the already-drawn cells rather than the child's output, so it
/// stays correct however the terminal widget chose to lay the text out, and a
/// reference that is not yet resolved simply stays plain.
fn decorate_references(f: &mut Frame, app: &App, area: Rect, theme: Theme) {
    restyle_references(f.buffer_mut(), area, theme, &|number| {
        app.github_reference_status(number)
    });
}

fn restyle_references(
    buffer: &mut ratatui::buffer::Buffer,
    area: Rect,
    theme: Theme,
    lookup: &dyn Fn(u64) -> Option<crate::github::ReferenceStatus>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    for y in area.top()..area.bottom() {
        let mut x = area.left();
        while x < area.right() {
            if buffer[(x, y)].symbol() != "#" {
                x += 1;
                continue;
            }
            let mut end = x + 1;
            let mut digits = String::new();
            while end < area.right() {
                let symbol = buffer[(end, y)].symbol();
                match symbol.chars().next() {
                    Some(character) if character.is_ascii_digit() => digits.push(character),
                    _ => break,
                }
                end += 1;
            }
            if digits.is_empty() {
                x += 1;
                continue;
            }
            // Mirror the scanner: a hash glued to a word is part of that word,
            // not a reference.
            let glued = x > area.left()
                && buffer[(x - 1, y)]
                    .symbol()
                    .chars()
                    .next()
                    .is_some_and(|character| character.is_ascii_alphanumeric());
            if glued {
                x = end;
                continue;
            }
            if let Some(status) = digits.parse::<u64>().ok().and_then(lookup) {
                buffer[(x, y)].set_style(reference_marker_style(theme, status));
                for cell in (x + 1)..end {
                    buffer[(cell, y)].set_style(reference_style(theme, status));
                }
            }
            x = end;
        }
    }
}

/// Colour of the leading `#`, which carries the kind.
///
/// State alone cannot say this: GitHub shows an open issue and an open pull
/// request in the same green, so without a second channel the two are
/// indistinguishable. Tinting the hash leaves the layout untouched, which
/// swapping in an icon glyph would not — a wide character would shift the rest
/// of the child's line.
fn reference_marker_style(theme: Theme, status: crate::github::ReferenceStatus) -> Style {
    use crate::github::ReferenceKind;
    let color = match status.kind {
        ReferenceKind::Issue => theme.warning,
        ReferenceKind::PullRequest => theme.accent_alt,
    };
    Style::default()
        .fg(color)
        .add_modifier(Modifier::UNDERLINED | Modifier::BOLD)
}

/// Colour of the number, which carries the state; the underline says "this is a link".
fn reference_style(theme: Theme, status: crate::github::ReferenceStatus) -> Style {
    use crate::github::{ReferenceKind, ReferenceState};
    let color = match status.state {
        ReferenceState::Open => theme.success,
        ReferenceState::Closed => match status.kind {
            // A closed issue and a closed pull request mean different things,
            // and GitHub itself colours them differently.
            ReferenceKind::Issue => theme.accent,
            ReferenceKind::PullRequest => theme.error,
        },
        ReferenceState::Merged => theme.accent,
        ReferenceState::Draft if theme.name == ThemeName::Classic => Color::Gray,
        ReferenceState::Draft => theme.muted,
    };
    Style::default()
        .fg(color)
        .add_modifier(Modifier::UNDERLINED | Modifier::BOLD)
}

pub fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme();
    let Some(mux) = app.mux.as_ref() else {
        return;
    };
    let Some(pane) = mux.focused_pane() else {
        return;
    };
    let prefix = mux.prefix.label();

    // The hint is fixed-width and reserved first; tabs take whatever is left, so a long
    // session name can never push the prefix reminder off screen.
    let hint: Vec<Span> = match pane.status {
        PaneStatus::Running if mux.prefix_state == PrefixState::Help => vec![
            Span::styled(" Help ", badge_style(theme, theme.warning)),
            Span::raw(" e scratchpad  Esc cancel "),
        ],
        PaneStatus::Running if mux.prefix_state == PrefixState::Github => vec![
            Span::styled(" GitHub ", badge_style(theme, theme.accent)),
            Span::raw(" i inspect  Esc cancel "),
        ],
        PaneStatus::Running if mux.prefix_state == PrefixState::Root => vec![
            Span::styled(format!(" {prefix} "), badge_style(theme, theme.warning)),
            Span::raw(root_command_hint(&prefix)),
        ],
        PaneStatus::Running
            if mux.prefix_state == PrefixState::Idle && app.update_notice.is_some() =>
        {
            vec![
                Span::styled(" Update ", badge_style(theme, theme.accent_alt)),
                Span::raw(format!(
                    " {} ",
                    text::truncate_to_width(
                        app.update_notice.as_deref().unwrap_or_default(),
                        area.width.saturating_sub(11) as usize,
                    )
                )),
            ]
        }
        PaneStatus::Running => vec![
            Span::raw(" "),
            Span::styled(prefix.clone(), Style::default().fg(theme.accent_alt)),
            Span::raw(" for commands "),
        ],
        PaneStatus::Exited(code) => {
            let text = match code {
                Some(0) | None => "exited".to_string(),
                Some(code) => format!("exited with code {code}"),
            };
            vec![Span::styled(
                format!(" {text} — Enter to close "),
                Style::default().fg(theme.warning),
            )]
        }
    };
    let hint_width: usize = hint
        .iter()
        .map(|span| text::display_width(&span.content))
        .sum();

    let focused_index = mux
        .panes
        .iter()
        .position(|candidate| candidate.id == pane.id)
        .unwrap_or(0);
    let sessions: Vec<(String, bool)> = mux
        .panes
        .iter()
        .map(|pane| (pane.display_title(), pane.is_running()))
        .collect();
    let (tab_list, hidden) = tabs::layout(
        &sessions,
        focused_index,
        (area.width as usize).saturating_sub(hint_width),
    );

    let mut spans: Vec<Span> = Vec::new();
    for tab in &tab_list {
        let style = if tab.active {
            badge_style(theme, theme.accent_alt)
        } else if tab.running {
            Style::default().fg(theme.ansi[7])
        } else {
            Style::default().fg(theme.error)
        };
        spans.push(Span::styled(tab.label.clone(), style));
    }
    if hidden > 0 {
        spans.push(Span::styled(
            format!(" +{hidden} "),
            Style::default().fg(theme.muted),
        ));
    }
    spans.extend(hint);

    fill_area(f.buffer_mut(), area, theme.chrome_bg);
    let status = Paragraph::new(Line::from(spans)).style(status_style(theme));
    f.render_widget(status, area);
}

fn panel_style(theme: Theme) -> Style {
    if theme.name == ThemeName::Classic {
        Style::default()
    } else {
        Style::default().fg(theme.text).bg(theme.background)
    }
}

fn status_style(theme: Theme) -> Style {
    let style = Style::default().bg(theme.chrome_bg);
    if theme.name == ThemeName::Classic {
        style
    } else {
        style.fg(theme.text)
    }
}

fn badge_style(theme: Theme, background: Color) -> Style {
    let foreground = if theme.name == ThemeName::Classic {
        Color::Black
    } else {
        theme.contrast_text(background)
    };
    Style::default()
        .fg(foreground)
        .bg(background)
        .add_modifier(Modifier::BOLD)
}

fn root_command_hint(prefix: &str) -> String {
    format!(
        " c chat e scratch t term s snippets u update C-h help C-g gh d list w switch \
         n/p cycle 1-9 jump x end q quit {prefix} send Esc "
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::UserConfig;
    use crate::github::{ReferenceKind, ReferenceState, ReferenceStatus};
    use crate::mux::{Pane, PaneSpec};
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;
    use std::sync::mpsc;

    #[test]
    fn live_prefix_menu_covers_every_root_command() {
        let hint = root_command_hint("C-b");
        for entry in [
            "c chat",
            "e scratch",
            "t term",
            "s snippets",
            "u update",
            "C-h help",
            "C-g gh",
            "d list",
            "w switch",
            "n/p cycle",
            "1-9 jump",
            "x end",
            "q quit",
            "C-b send",
            "Esc",
        ] {
            assert!(hint.contains(entry), "missing {entry:?} from {hint:?}");
        }
        assert!(
            text::display_width(&hint) <= 125,
            "keep the complete menu usable in a typical 128-column terminal: {hint:?}"
        );
    }

    #[test]
    fn known_references_are_styled_and_others_left_alone() {
        let area = Rect::new(0, 0, 40, 2);
        let mut buffer = Buffer::empty(area);
        for (index, character) in "Fixed #11 in #12, room 314, v#9".chars().enumerate() {
            buffer[(index as u16, 0)].set_symbol(&character.to_string());
        }

        restyle_references(
            &mut buffer,
            area,
            ThemeName::Classic.theme(),
            &|number| match number {
                11 => Some(ReferenceStatus {
                    kind: ReferenceKind::Issue,
                    state: ReferenceState::Open,
                }),
                12 => Some(ReferenceStatus {
                    kind: ReferenceKind::PullRequest,
                    state: ReferenceState::Merged,
                }),
                // Resolvable on its own, but here it only appears glued to `v#`.
                9 => Some(ReferenceStatus {
                    kind: ReferenceKind::Issue,
                    state: ReferenceState::Open,
                }),
                _ => None,
            },
        );

        // The number carries the state...
        for x in 7..9 {
            assert_eq!(buffer[(x, 0)].style().fg, Some(Color::Green), "cell {x}");
        }
        for x in 14..16 {
            assert_eq!(buffer[(x, 0)].style().fg, Some(Color::Magenta), "cell {x}");
        }
        // ...and the hash carries the kind, which the state alone cannot: both of
        // these would be green if they were open.
        assert_eq!(buffer[(6, 0)].style().fg, Some(Color::Yellow), "issue hash");
        assert_eq!(buffer[(13, 0)].style().fg, Some(Color::Cyan), "pull hash");
        assert!(buffer[(6, 0)]
            .style()
            .add_modifier
            .contains(Modifier::UNDERLINED));
        // A number nobody could resolve, and a hash glued to a word, stay untouched.
        assert_eq!(buffer[(23, 0)].style().fg, Some(Color::Reset));
        assert_eq!(buffer[(30, 0)].style().fg, Some(Color::Reset));
    }

    #[test]
    fn an_open_issue_and_an_open_pull_request_are_distinguishable() {
        let area = Rect::new(0, 0, 20, 1);
        let mut buffer = Buffer::empty(area);
        for (index, character) in "#11 #12".chars().enumerate() {
            buffer[(index as u16, 0)].set_symbol(&character.to_string());
        }

        restyle_references(&mut buffer, area, ThemeName::Classic.theme(), &|number| {
            Some(ReferenceStatus {
                kind: if number == 11 {
                    ReferenceKind::Issue
                } else {
                    ReferenceKind::PullRequest
                },
                state: ReferenceState::Open,
            })
        });

        // Both are open, so both numbers are green; only the hash tells them apart.
        assert_eq!(buffer[(1, 0)].style().fg, Some(Color::Green));
        assert_eq!(buffer[(5, 0)].style().fg, Some(Color::Green));
        assert_ne!(buffer[(0, 0)].style().fg, buffer[(4, 0)].style().fg);
    }

    /// Render the whole UI into an off-screen buffer and return it as plain text.
    fn render(app: &mut App) -> String {
        render_buffer(app, 80, 24)
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
    }

    fn render_buffer(app: &mut App, width: u16, height: u16) -> Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|f| crate::ui::draw(f, app))
            .expect("draw succeeds");
        terminal.backend().buffer().clone()
    }

    /// A child that stays alive without printing anything, standing in for Copilot's
    /// several-second boot.
    fn silent_pane(events: mpsc::Sender<crate::mux::MuxEvent>) -> Pane {
        let (program, args) = if cfg!(windows) {
            (
                "cmd.exe".to_string(),
                vec!["/c".to_string(), "ping -n 30 127.0.0.1 >nul".to_string()],
            )
        } else {
            (
                "/bin/sh".to_string(),
                vec!["-c".to_string(), "sleep 30".to_string()],
            )
        };
        Pane::spawn(
            PaneSpec {
                id: 1,
                title: "booting".to_string(),
                cwd: std::env::temp_dir(),
                session_id: "booting-session".to_string(),
                program,
                args,
            },
            24,
            80,
            events,
        )
        .expect("pane spawns")
    }

    fn mux_app() -> App {
        let config = UserConfig {
            mux: true,
            ..UserConfig::default()
        };
        App::new(Vec::new(), config)
    }

    fn mux_app_with_theme(theme: ThemeName) -> App {
        let config = UserConfig {
            mux: true,
            theme,
            ..UserConfig::default()
        };
        App::new(Vec::new(), config)
    }

    fn rgb_components(color: Color) -> (u8, u8, u8) {
        match color {
            Color::Rgb(r, g, b) => (r, g, b),
            other => panic!("expected RGB color, got {other:?}"),
        }
    }

    fn contrast_ratio(foreground: Color, background: Color) -> f64 {
        let luminance = |color| {
            let (r, g, b) = rgb_components(color);
            let channel = |value: u8| {
                let value = f64::from(value) / 255.0;
                if value <= 0.04045 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
        };
        let foreground = luminance(foreground);
        let background = luminance(background);
        (foreground.max(background) + 0.05) / (foreground.min(background) + 0.05)
    }

    #[test]
    fn light_theme_styles_attached_chrome_and_startup_spinner() {
        let mut app = mux_app_with_theme(ThemeName::SolarizedLight);
        let theme = app.theme();
        let events = app.mux.as_ref().expect("mux").events.clone();
        let pane = silent_pane(events);
        let id = pane.id;
        app.mux.as_mut().expect("mux").push(pane);
        app.view = crate::app::View::Attached(id);

        let buffer = render_buffer(&mut app, 80, 24);

        assert_eq!(buffer[(0, 0)].fg, theme.accent_alt);
        assert_eq!(buffer[(0, 0)].bg, theme.background);
        assert_eq!(buffer[(6, 2)].symbol(), "S");
        assert_eq!(buffer[(6, 2)].fg, theme.accent_alt);
        assert_eq!(buffer[(6, 2)].bg, theme.background);
        assert_eq!(buffer[(0, 23)].bg, theme.accent_alt);
        let _ = app.mux.as_mut().expect("mux").shutdown();
    }

    #[test]
    fn light_themes_keep_copilot_edit_output_readable_and_terminal_faithful() {
        let transcript = concat!(
            "ordinary \x1b[1;3mstyled\x1b[0m \x1b[38;5;42mhigh\x1b[0m ",
            "\x1b[38;2;1;2;3mtrue\x1b[0m\r\n",
            "\x1b[1;36mEdit\x1b[0m src/lib.rs\r\n",
            "\x1b[90m 12 │ \x1b[35mfn\x1b[39m demo() {\x1b[0m\r\n",
            "\x1b[48;2;18;52;31m\x1b[32m+\x1b[39m \x1b[90m13 │ ",
            "\x1b[39m\x1b[36mlet\x1b[39m added = \x1b[33mtrue\x1b[39m;",
            "\x1b[0m\r\n",
            "\x1b[48;2;62;25;31m\x1b[31m-\x1b[39m \x1b[90m13 │ ",
            "\x1b[39m\x1b[36mlet\x1b[39m deleted = \x1b[33mfalse\x1b[39m;",
            "\x1b[0m"
        );

        for name in ThemeName::LIGHT {
            let mut app = mux_app_with_theme(name);
            let theme = app.theme();
            let events = app.mux.as_ref().expect("mux").events.clone();
            let mut pane = silent_pane(events);
            let id = pane.id;
            pane.feed_synthetic(transcript.as_bytes());
            app.mux.as_mut().expect("mux").push(pane);
            app.view = crate::app::View::Attached(id);

            let buffer = render_buffer(&mut app, 80, 24);
            let x = 1;
            let y = 1;
            let add_bg = Color::Rgb(18, 52, 31);
            let delete_bg = Color::Rgb(62, 25, 31);

            assert_eq!(buffer[(x, y)].fg, theme.text, "{}", name.label());
            assert_eq!(buffer[(x, y)].bg, theme.background, "{}", name.label());
            assert!(
                buffer[(x + 9, y)].modifier.contains(Modifier::BOLD),
                "{}",
                name.label()
            );
            assert!(
                buffer[(x + 9, y)].modifier.contains(Modifier::ITALIC),
                "{}",
                name.label()
            );
            assert_eq!(
                buffer[(x + 16, y)].fg,
                Color::Indexed(42),
                "{}",
                name.label()
            );
            assert_eq!(
                buffer[(x + 21, y)].fg,
                Color::Rgb(1, 2, 3),
                "{}",
                name.label()
            );

            assert_eq!(buffer[(x + 1, y + 2)].fg, theme.ansi[8]);
            assert_eq!(buffer[(x + 6, y + 2)].fg, theme.ansi[5]);
            assert!(contrast_ratio(theme.ansi[8], theme.background) >= 2.4);

            assert_eq!(buffer[(x, y + 3)].fg, theme.ansi[2]);
            assert_eq!(buffer[(x, y + 3)].bg, add_bg);
            assert_eq!(buffer[(x + 7, y + 3)].fg, theme.ansi[6]);
            assert_eq!(buffer[(x + 11, y + 3)].fg, theme.contrast_text(add_bg));
            assert_eq!(buffer[(x + 11, y + 3)].bg, add_bg);
            assert!(contrast_ratio(theme.ansi[2], add_bg) >= 2.8);
            assert!(contrast_ratio(theme.ansi[6], add_bg) >= 2.8);

            assert_eq!(buffer[(x, y + 4)].fg, theme.ansi[1]);
            assert_eq!(buffer[(x, y + 4)].bg, delete_bg);
            assert_eq!(buffer[(x + 7, y + 4)].fg, theme.ansi[6]);
            assert_eq!(buffer[(x + 11, y + 4)].fg, theme.contrast_text(delete_bg));
            assert_eq!(buffer[(x + 11, y + 4)].bg, delete_bg);
            assert!(contrast_ratio(theme.ansi[1], delete_bg) >= 2.8);
            assert!(contrast_ratio(theme.ansi[6], delete_bg) >= 2.8);

            let _ = app.mux.as_mut().expect("mux").shutdown();
        }
    }

    #[test]
    fn themed_references_keep_terminal_backgrounds_and_modifiers() {
        let area = Rect::new(0, 0, 4, 1);
        let mut buffer = Buffer::empty(area);
        let theme = ThemeName::CatppuccinLatte.theme();
        for (index, character) in "#123".chars().enumerate() {
            buffer[(index as u16, 0)]
                .set_symbol(&character.to_string())
                .set_style(
                    Style::default()
                        .fg(theme.text)
                        .bg(theme.diff_add_bg)
                        .add_modifier(Modifier::ITALIC),
                );
        }

        restyle_references(&mut buffer, area, theme, &|_| {
            Some(ReferenceStatus {
                kind: ReferenceKind::Issue,
                state: ReferenceState::Open,
            })
        });

        assert_eq!(buffer[(0, 0)].fg, theme.warning);
        assert_eq!(buffer[(1, 0)].fg, theme.success);
        for x in 0..4 {
            assert_eq!(buffer[(x, 0)].bg, theme.diff_add_bg);
            assert!(buffer[(x, 0)].modifier.contains(Modifier::ITALIC));
            assert!(buffer[(x, 0)].modifier.contains(Modifier::UNDERLINED));
            assert!(buffer[(x, 0)].modifier.contains(Modifier::BOLD));
        }
    }

    #[test]
    fn a_session_that_has_drawn_nothing_shows_the_startup_spinner() {
        let mut app = mux_app();
        let events = app.mux.as_ref().expect("mux").events.clone();
        let pane = silent_pane(events);
        let id = pane.id;
        app.mux.as_mut().expect("mux").push(pane);
        app.view = crate::app::View::Attached(id);

        let text = render(&mut app);

        assert!(
            text.contains("Starting Copilot"),
            "a blank pane must show the spinner, got:\n{text}"
        );
        let _ = app.mux.as_mut().expect("mux").shutdown();
    }

    #[test]
    fn the_spinner_clears_once_the_session_paints_something() {
        let mut app = mux_app();
        let events = app.mux.as_ref().expect("mux").events.clone();
        let pane = silent_pane(events);
        let id = pane.id;
        app.mux.as_mut().expect("mux").push(pane);
        app.view = crate::app::View::Attached(id);

        // Stand in for Copilot's first frame arriving.
        app.mux
            .as_mut()
            .expect("mux")
            .focused_pane_mut()
            .expect("pane")
            .feed_synthetic(b"hello from copilot");

        let text = render(&mut app);

        assert!(
            !text.contains("Starting Copilot"),
            "the spinner must disappear as soon as the child draws, got:\n{text}"
        );
        assert!(text.contains("hello from copilot"));
        let _ = app.mux.as_mut().expect("mux").shutdown();
    }

    #[test]
    fn attached_status_shows_non_disruptive_update_progress() {
        let mut app = mux_app();
        let events = app.mux.as_ref().expect("mux").events.clone();
        let pane = silent_pane(events);
        let id = pane.id;
        app.mux.as_mut().expect("mux").push(pane);
        app.view = crate::app::View::Attached(id);
        app.update_notice = Some("Installing v0.19.0; running sessions stay open...".to_string());

        let text = render(&mut app);

        assert!(text.contains("Update"), "got:\n{text}");
        assert!(text.contains("Installing v0.19.0"), "got:\n{text}");
        assert!(text.contains("running sessions stay open"), "got:\n{text}");
        let _ = app.mux.as_mut().expect("mux").shutdown();
    }

    #[test]
    fn the_quit_prompt_is_visible_without_leaving_the_attached_pane() {
        let mut app = mux_app();
        let events = app.mux.as_ref().expect("mux").events.clone();
        let pane = silent_pane(events);
        let id = pane.id;
        app.mux.as_mut().expect("mux").push(pane);
        app.view = crate::app::View::Attached(id);
        app.confirm_quit = true;

        let text = render(&mut app);

        assert!(
            text.contains("Quit and end 1 running session(s)?"),
            "prefix q must be answerable from the pane it was pressed in, got:\n{text}"
        );
        let _ = app.mux.as_mut().expect("mux").shutdown();
    }

    #[test]
    fn attached_workspace_renders_chat_scratchpad_and_terminal_together() {
        let mut app = mux_app();
        let events = app.mux.as_ref().expect("mux").events.clone();
        let pane = silent_pane(events);
        let id = pane.id;
        app.mux.as_mut().expect("mux").push(pane);
        app.view = crate::app::View::Attached(id);
        app.scratchpad =
            Some(crate::scratchpad::Scratchpad::open("workspace-render-test").unwrap());
        app.scratchpad
            .as_mut()
            .unwrap()
            .handle_event(crossterm::event::Event::Key(
                crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::Char('x'),
                    crossterm::event::KeyModifiers::NONE,
                ),
            ))
            .unwrap();
        app.scratchpad_owner = Some(id);
        app.scratchpad_open.insert(id);
        let directory = tempfile::tempdir().unwrap();
        app.terminal
            .activate(
                "workspace-render-test".to_string(),
                "Shell".to_string(),
                directory.path().to_string_lossy().to_string(),
                &crate::config::TerminalConfig::default(),
            )
            .unwrap();
        app.terminal_owner = Some(id);
        app.terminal_open.insert(id);
        app.workspace_focus = crate::app::WorkspaceFocus::Terminal;

        let text = render(&mut app);

        assert!(text.contains("Scratchpad"), "got:\n{text}");
        assert!(!text.contains("[modified]"), "got:\n{text}");
        assert!(text.contains("Terminal"), "got:\n{text}");
        assert!(!text.contains("Terminal ["), "got:\n{text}");
        assert!(text.contains("Starting Copilot"), "got:\n{text}");
        assert!(!text.contains("Alt+H"), "got:\n{text}");
        assert!(!text.contains("Save/close"), "got:\n{text}");
        assert!(!text.contains("focused"), "got:\n{text}");

        app.workspace_help = Some(crate::app::WorkspaceHelp::Scratchpad);
        let text = render(&mut app);
        assert!(text.contains("Scratchpad Help"), "got:\n{text}");
        assert!(text.contains("Ctrl/Alt+L"), "got:\n{text}");
        assert!(text.contains("Shift+Tab"), "got:\n{text}");
        assert!(text.contains("Ctrl+Shift+K"), "got:\n{text}");

        app.terminal.shutdown();
        let _ = app.mux.as_mut().expect("mux").shutdown();
    }
}
