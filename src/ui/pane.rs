use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use std::time::Duration;
use tui_term::widget::PseudoTerminal;

use crate::app::App;
use crate::mux::PaneStatus;
use crate::ui::tabs;

/// Frames of the startup spinner. Braille dots read as motion even in a plain terminal.
const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

/// Progress indicator shown while a freshly spawned session has yet to draw anything.
fn draw_starting(f: &mut Frame, area: Rect, elapsed: Duration) {
    let frame = SPINNER[(elapsed.as_millis() / 120) as usize % SPINNER.len()];
    let seconds = elapsed.as_secs();

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  {frame}  Starting Copilot…"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
    ];
    // Only mention the wait once it is long enough to be worth reassuring about.
    if seconds >= 3 {
        lines.push(Line::from(Span::styled(
            format!("     {seconds}s — the CLI is still booting"),
            Style::default().fg(Color::DarkGray),
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

    // Copilot needs a few seconds before it draws anything; without this the pane just
    // looks frozen.
    let starting = pane.is_running() && pane.is_blank();
    if starting {
        draw_starting(f, terminal_area, pane.started_at.elapsed());
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

    let prefix = mux.prefix.label();

    // The hint is fixed-width and reserved first; tabs take whatever is left, so a long
    // session name can never push the prefix reminder off screen.
    let hint: Vec<Span> = match pane.status {
        PaneStatus::Running if mux.prefix_pending => vec![
            Span::styled(
                format!(" {prefix} "),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" d list  w switch  n/p cycle  x end "),
        ],
        PaneStatus::Running => vec![
            Span::raw(" "),
            Span::styled(prefix.clone(), Style::default().fg(Color::Cyan)),
            Span::raw(" for commands "),
        ],
        PaneStatus::Exited(code) => {
            let text = match code {
                Some(0) | None => "exited".to_string(),
                Some(code) => format!("exited with code {code}"),
            };
            vec![Span::styled(
                format!(" {text} — Enter to close "),
                Style::default().fg(Color::Yellow),
            )]
        }
    };
    let hint_width: usize = hint.iter().map(|span| span.content.chars().count()).sum();

    let focused_index = mux
        .panes
        .iter()
        .position(|candidate| candidate.id == pane.id)
        .unwrap_or(0);
    let sessions: Vec<(String, bool)> = mux
        .panes
        .iter()
        .map(|pane| (pane.title.clone(), pane.is_running()))
        .collect();
    let (tab_list, hidden) = tabs::layout(
        &sessions,
        focused_index,
        (layout[1].width as usize).saturating_sub(hint_width),
    );

    let mut spans: Vec<Span> = Vec::new();
    for tab in &tab_list {
        let style = if tab.active {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else if tab.running {
            Style::default().fg(Color::Gray)
        } else {
            Style::default().fg(Color::Red)
        };
        spans.push(Span::styled(tab.label.clone(), style));
    }
    if hidden > 0 {
        spans.push(Span::styled(
            format!(" +{hidden} "),
            Style::default().fg(Color::DarkGray),
        ));
    }
    spans.extend(hint);

    let status =
        Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::Rgb(30, 30, 40)));
    f.render_widget(status, layout[1]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::UserConfig;
    use crate::mux::{Pane, PaneSpec};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::sync::mpsc;

    /// Render the whole UI into an off-screen buffer and return it as plain text.
    fn render(app: &App) -> String {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|f| crate::ui::draw(f, app))
            .expect("draw succeeds");
        let buffer = terminal.backend().buffer().clone();
        buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
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
                session_id: None,
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

    #[test]
    fn a_session_that_has_drawn_nothing_shows_the_startup_spinner() {
        let mut app = mux_app();
        let events = app.mux.as_ref().expect("mux").events.clone();
        let pane = silent_pane(events);
        let id = pane.id;
        app.mux.as_mut().expect("mux").push(pane);
        app.view = crate::app::View::Attached(id);

        let text = render(&app);

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
            .feed_for_test(b"hello from copilot");

        let text = render(&app);

        assert!(
            !text.contains("Starting Copilot"),
            "the spinner must disappear as soon as the child draws, got:\n{text}"
        );
        assert!(text.contains("hello from copilot"));
        let _ = app.mux.as_mut().expect("mux").shutdown();
    }
}
