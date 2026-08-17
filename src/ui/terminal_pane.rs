use crate::terminal_pane::TerminalPane;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use tui_term::widget::{Cursor, PseudoTerminal};

pub fn draw(frame: &mut Frame, terminal: &TerminalPane, focused: bool, area: Rect) {
    let border_color = if focused {
        Color::Cyan
    } else {
        Color::DarkGray
    };
    let block = Block::default()
        .title(" Terminal ")
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
