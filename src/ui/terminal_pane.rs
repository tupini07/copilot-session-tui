use crate::terminal_pane::TerminalPane;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use tui_term::widget::{Cursor, PseudoTerminal};

use crate::theme::{apply_terminal_theme, fill_area, Theme, ThemeName};

pub fn draw_with_theme(
    frame: &mut Frame,
    terminal: &TerminalPane,
    focused: bool,
    area: Rect,
    theme: Theme,
) {
    match terminal.parser().read() {
        Ok(parser) => draw_screen(frame, parser.screen(), focused, area, theme),
        Err(_) => draw_unavailable(frame, focused, area, theme),
    }
}

fn draw_screen(frame: &mut Frame, screen: &vt100::Screen, focused: bool, area: Rect, theme: Theme) {
    let block = terminal_block(focused, theme);
    let terminal_area = block.inner(area);
    fill_area(frame.buffer_mut(), area, theme.background);
    frame.render_widget(block, area);

    let cursor = Cursor::default().visibility(focused);
    let widget = PseudoTerminal::new(screen).cursor(cursor);
    frame.render_widget(widget, terminal_area);
    apply_terminal_theme(frame.buffer_mut(), terminal_area, theme);
}

fn draw_unavailable(frame: &mut Frame, focused: bool, area: Rect, theme: Theme) {
    let block = terminal_block(focused, theme);
    let terminal_area = block.inner(area);
    fill_area(frame.buffer_mut(), area, theme.background);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new("Terminal parser is unavailable").style(panel_style(theme)),
        terminal_area,
    );
}

fn terminal_block(focused: bool, theme: Theme) -> Block<'static> {
    let border_color = if focused {
        theme.accent_alt
    } else {
        theme.inactive
    };
    Block::default()
        .title(" Terminal ")
        .borders(Borders::ALL)
        .style(panel_style(theme))
        .border_style(Style::default().fg(border_color))
}

fn panel_style(theme: Theme) -> Style {
    if theme.name == ThemeName::Classic {
        Style::default()
    } else {
        Style::default().fg(theme.text).bg(theme.background)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::style::{Color, Modifier};
    use ratatui::Terminal;

    fn render_stream(
        stream: &[u8],
        focused: bool,
        theme: Theme,
        width: u16,
        height: u16,
    ) -> Buffer {
        let mut parser = vt100::Parser::new(
            height.saturating_sub(2).max(1),
            width.saturating_sub(2).max(1),
            0,
        );
        parser.process(stream);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| draw_screen(frame, parser.screen(), focused, frame.area(), theme))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    fn light_theme_maps_only_terminal_defaults_and_standard_ansi_colors() {
        let theme = ThemeName::CatppuccinLatte.theme();
        let buffer = render_stream(
            concat!(
                "plain \x1b[1;4mstyled\x1b[0m ",
                "\x1b[31mred\x1b[0m ",
                "\x1b[38;5;42mhigh\x1b[0m ",
                "\x1b[38;2;1;2;3mtrue\x1b[0m"
            )
            .as_bytes(),
            false,
            theme,
            60,
            6,
        );

        assert_eq!(buffer[(0, 0)].fg, theme.inactive);
        assert_eq!(buffer[(0, 0)].bg, theme.background);
        assert_eq!(buffer[(1, 1)].fg, theme.text);
        assert_eq!(buffer[(1, 1)].bg, theme.background);
        assert!(buffer[(7, 1)].modifier.contains(Modifier::BOLD));
        assert!(buffer[(7, 1)].modifier.contains(Modifier::UNDERLINED));
        assert_eq!(buffer[(14, 1)].fg, theme.ansi[1]);
        assert_eq!(buffer[(18, 1)].fg, Color::Indexed(42));
        assert_eq!(buffer[(23, 1)].fg, Color::Rgb(1, 2, 3));
    }

    #[test]
    fn terminal_theme_preserves_the_rendered_cursor_modifier() {
        let theme = ThemeName::SolarizedLight.theme();
        let focused = render_stream(b"X\x08", true, theme, 10, 4);
        let unfocused = render_stream(b"X\x08", false, theme, 10, 4);

        assert!(focused[(1, 1)].modifier.contains(Modifier::REVERSED));
        assert!(!unfocused[(1, 1)].modifier.contains(Modifier::REVERSED));
        assert_eq!(focused[(1, 1)].fg, theme.text);
        assert_eq!(focused[(1, 1)].bg, theme.background);
    }

    #[test]
    fn classic_terminal_rendering_keeps_existing_defaults() {
        let buffer = render_stream(b"plain", false, ThemeName::Classic.theme(), 20, 4);

        assert_eq!(buffer[(0, 0)].fg, Color::DarkGray);
        assert_eq!(buffer[(1, 1)].fg, Color::Reset);
        assert_eq!(buffer[(1, 1)].bg, Color::Reset);
    }

    #[test]
    fn large_terminal_theme_mapping_stays_within_a_frame_budget() {
        let theme = ThemeName::CatppuccinLatte.theme();
        let started = std::time::Instant::now();
        let buffer = render_stream(b"ready", false, theme, 240, 70);

        assert_eq!(buffer.area, Rect::new(0, 0, 240, 70));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "240x70 render and terminal mapping took {:?}",
            started.elapsed()
        );
    }
}
