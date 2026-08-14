use crate::terminal_pane::{TerminalPane, TerminalStatus};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use tui_term::widget::{Cursor, PseudoTerminal};

pub fn draw(frame: &mut Frame, terminal: &TerminalPane, focused: bool, area: Rect) {
    let border_color = if focused {
        Color::Cyan
    } else {
        Color::DarkGray
    };
    let status = terminal.status();
    let status_color = match status {
        TerminalStatus::Running => Color::Green,
        TerminalStatus::Exited(_) => Color::Yellow,
        TerminalStatus::Failed(_) => Color::Red,
    };
    let title = Line::styled(
        format!(
            " Terminal: {} [{}] - prefix t toggles ",
            terminal.session_name, status
        ),
        Style::default().fg(status_color),
    );
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    match terminal.parser().read() {
        Ok(parser) => {
            let cursor = Cursor::default().visibility(focused);
            let widget = PseudoTerminal::new(parser.screen())
                .block(block)
                .cursor(cursor)
                .style(Style::default().bg(Color::Black));
            frame.render_widget(widget, area);
        }
        Err(_) => {
            frame.render_widget(
                Paragraph::new("Terminal parser is unavailable").block(block),
                area,
            );
        }
    }
}
