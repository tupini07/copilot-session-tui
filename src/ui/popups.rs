use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};

use crate::app::App;

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

pub fn draw_delete_confirm(f: &mut Frame, app: &App) {
    let area = centered_rect(40, 30, f.area());
    f.render_widget(Clear, area);

    let name = app
        .selected_session()
        .map(|s| s.display_name().to_string())
        .unwrap_or_default();

    let text = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Delete this session?",
            Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(format!("  {}", name)),
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled("y", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::raw(" Yes  "),
            Span::styled("any key", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
            Span::raw(" Cancel"),
        ]),
    ];

    let block = Block::default()
        .title(" Confirm Delete ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red));

    let paragraph = Paragraph::new(text).block(block).wrap(Wrap { trim: false });
    f.render_widget(paragraph, area);
}

pub fn draw_rename(f: &mut Frame, app: &App) {
    let area = centered_rect(50, 20, f.area());
    f.render_widget(Clear, area);

    let text = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Enter new name:",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(&app.rename_input, Style::default().fg(Color::White)),
            Span::styled("█", Style::default().fg(Color::Yellow)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled("Enter", Style::default().fg(Color::Cyan)),
            Span::raw(" Save  "),
            Span::styled("Esc", Style::default().fg(Color::Cyan)),
            Span::raw(" Cancel"),
        ]),
    ];

    let block = Block::default()
        .title(" Rename Session ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));

    let paragraph = Paragraph::new(text).block(block);
    f.render_widget(paragraph, area);
}

pub fn draw_project_filter(f: &mut Frame, app: &App) {
    let filtered = app.filtered_project_indices();
    let item_count = filtered.len() + 1; // +1 for "All Projects"
    let height = (item_count + 5).min(20) as u16; // +5 for search input, borders, padding
    let percent_y = ((height as f32 / f.area().height as f32) * 100.0).min(80.0) as u16;
    let area = centered_rect(50, percent_y.max(25), f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(" Select Project ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(area);
    f.render_widget(block, area);

    // Split inner area: search input at top, list below
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1), Constraint::Min(1)])
        .split(inner);

    // Search input
    let search_line = Line::from(vec![
        Span::styled(" 🔍 ", Style::default().fg(Color::Yellow)),
        Span::styled(&app.project_search_query, Style::default().fg(Color::White)),
        Span::styled("█", Style::default().fg(Color::Yellow)),
    ]);
    f.render_widget(Paragraph::new(search_line), chunks[0]);

    // Separator
    let sep = Line::from(Span::styled(
        "─".repeat(chunks[1].width as usize),
        Style::default().fg(Color::DarkGray),
    ));
    f.render_widget(Paragraph::new(sep), chunks[1]);

    // Project list
    let has_all_option = app.project_search_query.is_empty();
    let visible_rows = chunks[2].height as usize;

    // Build all logical items with their indices
    let mut all_items: Vec<(usize, ListItem)> = Vec::new();

    // "All Projects" option (only shown when search is empty)
    if has_all_option {
        let all_style = if app.project_selected == 0 {
            Style::default()
                .fg(Color::White)
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        all_items.push((0, ListItem::new(Line::from(Span::styled(
            "  All Projects",
            all_style,
        )))));
    }

    for (list_i, &proj_idx) in filtered.iter().enumerate() {
        let project = &app.unique_projects[proj_idx];
        let item_index = if has_all_option {
            list_i + 1
        } else {
            list_i
        };
        let is_selected = app.project_selected == item_index;
        let is_active = app.project_filter.as_deref() == Some(project.as_str());

        let prefix = if is_active { "● " } else { "  " };
        let name = std::path::Path::new(project)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(project);

        let display = format!("{}{}", prefix, name);

        let style = if is_selected {
            Style::default()
                .fg(Color::White)
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD)
        } else if is_active {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::White)
        };

        all_items.push((item_index, ListItem::new(Line::from(Span::styled(display, style)))));
    }

    let items: Vec<ListItem> = all_items
        .into_iter()
        .skip(app.project_scroll_offset)
        .take(visible_rows)
        .map(|(_, item)| item)
        .collect();

    let list = List::new(items);
    f.render_widget(list, chunks[2]);
}

pub fn draw_help(f: &mut Frame) {
    let area = centered_rect(55, 70, f.area());
    f.render_widget(Clear, area);

    let text = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Copilot Session Manager - Keyboard Shortcuts",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        help_line("↑/k ↓/j", "Navigate sessions"),
        help_line("Home/End", "Jump to first/last"),
        help_line("Enter", "Resume selected session"),
        help_line("r", "Rename selected session"),
        help_line("d", "Delete selected session"),
        Line::from(""),
        help_line("/", "Search / fuzzy filter"),
        help_line("f/p", "Filter by project (type to search)"),
        help_line("c", "Clear project filter"),
        help_line("s", "Cycle sort order"),
        Line::from(""),
        help_line("?", "Toggle this help"),
        help_line("u", "Update (when available)"),
        help_line("q/Esc", "Quit"),
        help_line("Ctrl+C", "Force quit"),
        Line::from(""),
        Line::from(Span::styled(
            "  Press any key to close",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let block = Block::default()
        .title(" Help ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let paragraph = Paragraph::new(text).block(block);
    f.render_widget(paragraph, area);
}

fn help_line(key: &str, desc: &str) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("{:<12}", key),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(desc.to_string(), Style::default().fg(Color::White)),
    ])
}
