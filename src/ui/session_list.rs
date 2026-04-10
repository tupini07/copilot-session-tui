use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem};

use crate::app::App;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Sessions ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.filtered_indices.is_empty() {
        let empty = ratatui::widgets::Paragraph::new("  No sessions found")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(empty, inner);
        return;
    }

    let visible_height = inner.height as usize;

    let items: Vec<ListItem> = app
        .filtered_indices
        .iter()
        .enumerate()
        .skip(app.scroll_offset)
        .take(visible_height)
        .map(|(display_idx, &real_idx)| {
            let session = &app.sessions[real_idx];
            let is_selected = display_idx == app.selected;

            let indicator = if session.is_active {
                Span::styled("● ", Style::default().fg(Color::Green))
            } else {
                Span::raw("  ")
            };

            let name = session.display_name();
            let truncated_name = if name.len() > 28 {
                format!("{}...", &name[..25])
            } else {
                name.to_string()
            };

            let name_style = if is_selected {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            let time = session.relative_time();
            let project = session.project_name();
            let truncated_project = if project.len() > 15 {
                format!("{}...", &project[..12])
            } else {
                project.to_string()
            };

            let line = Line::from(vec![
                indicator,
                Span::styled(
                    format!("{:<28}", truncated_name),
                    name_style,
                ),
                Span::styled(
                    format!(" {:>8}", time),
                    Style::default().fg(Color::DarkGray),
                ),
            ]);

            let project_line = Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    truncated_project,
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM),
                ),
            ]);

            if is_selected {
                ListItem::new(vec![line, project_line])
                    .style(Style::default().bg(Color::DarkGray))
            } else {
                ListItem::new(vec![line, project_line])
            }
        })
        .collect();

    let list = List::new(items);
    f.render_widget(list, inner);
}
