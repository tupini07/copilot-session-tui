use edtui::{EditorTheme, EditorView, LineNumbers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use ratatui::Frame;

use crate::scratchpad::Scratchpad;
use crate::theme::{fill_area, Theme, ThemeName};

pub fn draw_with_theme(f: &mut Frame, scratchpad: &mut Scratchpad, theme: Theme) {
    draw_in_with_theme(f, scratchpad, f.area(), true, theme);
}

pub fn draw_in_with_theme(
    f: &mut Frame,
    scratchpad: &mut Scratchpad,
    area: Rect,
    focused: bool,
    app_theme: Theme,
) {
    let status = scratchpad
        .status_message
        .as_deref()
        .map(|message| format!(" — {message}"))
        .unwrap_or_default();

    let background = editor_background(app_theme);
    let accent = if focused {
        app_theme.warning
    } else {
        app_theme.inactive
    };
    let editor_theme = scratchpad_editor_theme(app_theme, status, focused);
    let editor = EditorView::new(&mut scratchpad.state)
        .theme(editor_theme)
        .line_numbers(LineNumbers::None)
        .wrap(true)
        .tab_width(2);
    fill_area(f.buffer_mut(), area, background);
    f.render_widget(editor, area);

    let viewport_lines = area.height.saturating_sub(2) as usize;
    let content_lines = scratchpad.state.lines.len();
    if viewport_lines > 0 && content_lines > viewport_lines {
        let scrollbar_area = Rect {
            y: area.y.saturating_add(1),
            height: area.height.saturating_sub(2),
            ..area
        };
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some("│"))
            .track_style(Style::default().fg(app_theme.muted).bg(background))
            .thumb_symbol("█")
            .thumb_style(Style::default().fg(accent).bg(background));
        let mut scrollbar_state = ScrollbarState::new(content_lines)
            .position(scratchpad.state.cursor.row)
            .viewport_content_length(viewport_lines);
        f.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
    }
}

pub fn draw_help_with_theme(f: &mut Frame, area: Rect, theme: Theme) {
    let width = area.width.saturating_sub(4).min(54);
    let height = area.height.saturating_sub(2).min(17);
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    let lines = vec![
        help_line("Esc", "Close help", theme),
        help_line("Ctrl+S", "Save", theme),
        help_line("Mouse drag", "Select text", theme),
        help_line("Shift+Arrows", "Select text", theme),
        help_line("Ctrl/Alt+A", "Select all", theme),
        help_line("Ctrl+C/X/V", "Copy / cut / paste", theme),
        help_line("Ctrl+Z/Y", "Undo / redo", theme),
        help_line("Ctrl+W/Ctrl+BS", "Delete previous word", theme),
        help_line("Ctrl+Delete", "Delete next word", theme),
        help_line("Ctrl/Alt+L", "Add / toggle checkbox", theme),
        help_line("Shift+Tab", "Dedent line / selection", theme),
        help_line("Ctrl+Shift+K", "Delete current line", theme),
        help_line("Alt+Up/Down", "Move current line", theme),
        Line::from(""),
        Line::from(" Enter continues bullets, tasks, and numbered lists."),
    ];
    let block = Block::default()
        .title(" Scratchpad Help ")
        .borders(Borders::ALL)
        .style(help_panel_style(theme))
        .border_style(Style::default().fg(theme.warning));
    f.render_widget(Clear, popup);
    fill_area(f.buffer_mut(), popup, help_background(theme));
    f.render_widget(
        Paragraph::new(lines)
            .style(help_panel_style(theme))
            .block(block),
        popup,
    );
}

fn editor_background(theme: Theme) -> Color {
    if theme.name == ThemeName::Classic {
        Color::Black
    } else {
        theme.background
    }
}

fn scratchpad_editor_theme(theme: Theme, status: String, focused: bool) -> EditorTheme<'static> {
    let border_color = if focused {
        theme.warning
    } else {
        theme.inactive
    };
    let background = editor_background(theme);
    let selection_foreground = if theme.name == ThemeName::Classic {
        theme.selection_fg
    } else {
        theme.contrast_text(theme.selection_bg)
    };
    let cursor_style = if theme.name == ThemeName::Classic {
        Style::default().fg(Color::Black).bg(Color::White)
    } else {
        Style::default()
            .fg(selection_foreground)
            .bg(theme.selection_bg)
    };
    let editor_theme = EditorTheme::default()
        .base(Style::default().fg(theme.text).bg(background))
        .cursor_style(cursor_style)
        .selection_style(
            Style::default()
                .fg(selection_foreground)
                .bg(theme.selection_bg),
        )
        .line_numbers_style(Style::default().fg(theme.muted).bg(background))
        .block(
            Block::default()
                .title(format!(" Scratchpad{status} "))
                .borders(Borders::ALL)
                .style(panel_style(theme))
                .border_style(Style::default().fg(border_color)),
        )
        .hide_status_line();
    // An unfocused pane that still paints a cursor reads as if it has input focus.
    if focused {
        editor_theme
    } else {
        editor_theme.hide_cursor()
    }
}

fn help_background(theme: Theme) -> Color {
    if theme.name == ThemeName::Classic {
        Color::Reset
    } else {
        theme.surface
    }
}

fn panel_style(theme: Theme) -> Style {
    if theme.name == ThemeName::Classic {
        Style::default()
    } else {
        Style::default().fg(theme.text).bg(theme.background)
    }
}

fn help_panel_style(theme: Theme) -> Style {
    if theme.name == ThemeName::Classic {
        Style::default()
    } else {
        Style::default().fg(theme.text).bg(theme.surface)
    }
}

fn help_line(shortcut: &str, description: &str, theme: Theme) -> Line<'static> {
    Line::from(vec![
        Span::raw(" "),
        Span::styled(
            format!("{shortcut:<14}"),
            Style::default()
                .fg(theme.accent_alt)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(description.to_string()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{
        Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use edtui::{EditorMode, Index2, Lines};
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;

    fn render(content: &str, cursor_row: usize) -> String {
        render_buffer(
            content,
            Index2::new(cursor_row, 0),
            false,
            ThemeName::Classic.theme(),
        )
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
    }

    fn render_buffer(content: &str, cursor: Index2, focused: bool, theme: Theme) -> Buffer {
        let mut scratchpad = Scratchpad::open("scrollbar-render-test").unwrap();
        scratchpad.state.lines = Lines::from(content);
        scratchpad.state.cursor = cursor;
        let backend = TestBackend::new(20, 8);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| draw_in_with_theme(frame, &mut scratchpad, frame.area(), focused, theme))
            .unwrap();

        terminal.backend().buffer().clone()
    }

    #[test]
    fn light_theme_styles_editor_selection_cursor_and_scrollbar() {
        let theme = ThemeName::CatppuccinLatte.theme();
        let content = (1..=20)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let buffer = render_buffer(&content, Index2::new(0, 5), true, theme);

        assert_eq!(buffer[(0, 0)].fg, theme.warning);
        assert_eq!(buffer[(0, 0)].bg, theme.background);
        assert_eq!(buffer[(1, 1)].fg, theme.text);
        assert_eq!(buffer[(1, 1)].bg, theme.background);
        assert_eq!(buffer[(19, 1)].symbol(), "█");
        assert_eq!(buffer[(19, 1)].fg, theme.warning);
        let track = buffer
            .content()
            .iter()
            .find(|cell| cell.symbol() == "│" && cell.fg == theme.muted)
            .expect("scrollbar track");
        assert_eq!(track.fg, theme.muted);
        assert_eq!(track.bg, theme.background);

        let editor_theme = scratchpad_editor_theme(theme, String::new(), true);
        assert_eq!(editor_theme.base.fg, Some(theme.text));
        assert_eq!(editor_theme.base.bg, Some(theme.background));
        let selection_foreground = theme.contrast_text(theme.selection_bg);
        assert_eq!(editor_theme.cursor_style.fg, Some(selection_foreground));
        assert_eq!(editor_theme.cursor_style.bg, Some(theme.selection_bg));
        assert_eq!(editor_theme.selection_style.fg, Some(selection_foreground));
        assert_eq!(editor_theme.selection_style.bg, Some(theme.selection_bg));
        assert_eq!(editor_theme.line_numbers_style.fg, Some(theme.muted));
    }

    #[test]
    fn light_theme_styles_scratchpad_help_popup() {
        let theme = ThemeName::SolarizedLight.theme();
        let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();

        terminal
            .draw(|frame| draw_help_with_theme(frame, frame.area(), theme))
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(3, 1)].fg, theme.warning);
        assert_eq!(buffer[(3, 1)].bg, theme.surface);
        assert_eq!(buffer[(5, 2)].symbol(), "E");
        assert_eq!(buffer[(5, 2)].fg, theme.accent_alt);
        assert_eq!(buffer[(5, 2)].bg, theme.surface);
        assert_eq!(buffer[(20, 2)].fg, theme.text);
        assert_eq!(buffer[(20, 2)].bg, theme.surface);
    }

    #[test]
    fn long_scratchpad_renders_cursor_position_scrollbar() {
        let content = (1..=20)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");

        let at_top = render(&content, 0);
        let at_bottom = render(&content, 19);

        assert_eq!(at_top.chars().nth(39), Some('█'), "got:\n{at_top}");
        assert_eq!(at_bottom.chars().nth(139), Some('█'), "got:\n{at_bottom}");
        assert_ne!(at_top, at_bottom);
    }

    #[test]
    fn short_scratchpad_does_not_render_scrollbar() {
        let rendered = render("first\nsecond", 1);

        assert!(!rendered.contains('█'), "got:\n{rendered}");
    }

    #[test]
    fn long_lines_wrap_at_word_boundaries() {
        let rendered = render("currently trying to get", 0);
        let cells: Vec<char> = rendered.chars().collect();
        let rows: Vec<String> = cells.chunks(20).map(|row| row.iter().collect()).collect();

        assert!(rows[1].contains("currently trying"), "got:\n{rendered}");
        assert!(!rows[1].contains("trying t"), "got:\n{rendered}");
        assert!(rows[2].contains("to get"), "got:\n{rendered}");
    }

    #[test]
    fn arrows_navigate_visual_rows_inside_a_wrapped_line() {
        let mut scratchpad = Scratchpad::open("soft-wrap-navigation-test").unwrap();
        scratchpad.state.lines = Lines::from("currently trying to get");
        scratchpad.state.cursor = Index2::new(0, 5);
        let backend = TestBackend::new(20, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| draw_with_theme(frame, &mut scratchpad, ThemeName::Classic.theme()))
            .unwrap();

        scratchpad
            .handle_event(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)))
            .unwrap();
        assert_eq!(scratchpad.state.cursor, Index2::new(0, 22));

        scratchpad
            .handle_event(Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)))
            .unwrap();
        assert_eq!(scratchpad.state.cursor, Index2::new(0, 5));

        scratchpad
            .handle_event(Event::Key(KeyEvent::new(
                KeyCode::Down,
                KeyModifiers::SHIFT,
            )))
            .unwrap();
        assert_eq!(scratchpad.state.cursor, Index2::new(0, 22));
        assert!(scratchpad.state.selection.is_some());

        scratchpad
            .handle_event(Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT)))
            .unwrap();
        assert_eq!(scratchpad.state.cursor, Index2::new(0, 5));
    }

    #[test]
    fn mouse_drag_preserves_selection_anchor_until_release() {
        let mut scratchpad = Scratchpad::open("mouse-selection-test").unwrap();
        scratchpad.state.lines = Lines::from("abcdefghij");
        let backend = TestBackend::new(20, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| draw_with_theme(frame, &mut scratchpad, ThemeName::Classic.theme()))
            .unwrap();

        for (kind, column) in [
            (MouseEventKind::Down(MouseButton::Left), 2),
            (MouseEventKind::Drag(MouseButton::Left), 5),
            (MouseEventKind::Drag(MouseButton::Left), 9),
        ] {
            scratchpad
                .handle_event(Event::Mouse(MouseEvent {
                    kind,
                    column,
                    row: 1,
                    modifiers: KeyModifiers::NONE,
                }))
                .unwrap();
        }

        let selection = scratchpad.state.selection.as_ref().unwrap();
        assert_eq!(selection.start, Index2::new(0, 1));
        assert_eq!(selection.end, Index2::new(0, 8));
        assert_eq!(scratchpad.state.mode, EditorMode::Visual);

        scratchpad
            .handle_event(Event::Mouse(MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: 9,
                row: 1,
                modifiers: KeyModifiers::NONE,
            }))
            .unwrap();

        let selection = scratchpad.state.selection.as_ref().unwrap();
        assert_eq!(selection.start, Index2::new(0, 1));
        assert_eq!(selection.end, Index2::new(0, 8));
        assert_eq!(scratchpad.state.mode, EditorMode::Insert);
    }
}
