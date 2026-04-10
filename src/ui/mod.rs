pub mod session_list;
pub mod session_detail;
pub mod status_bar;
pub mod popups;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};

use crate::app::{App, Mode};

pub fn draw(f: &mut Frame, app: &App) {
    let size = f.area();

    let main_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),  // title bar
            Constraint::Min(5),    // main content
            Constraint::Length(2), // status bar
        ])
        .split(size);

    // Title bar
    let filter_text = match &app.project_filter {
        Some(p) => {
            let name = std::path::Path::new(p)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(p);
            format!(" Filter: {} ", name)
        }
        None => " All Projects ".to_string(),
    };

    let sort_text = format!(" Sort: {} ", app.sort_label());

    let title = ratatui::widgets::Paragraph::new(ratatui::text::Line::from(vec![
        ratatui::text::Span::styled(
            " Copilot Session Manager ",
            ratatui::style::Style::default()
                .fg(ratatui::style::Color::Black)
                .bg(ratatui::style::Color::Cyan)
                .add_modifier(ratatui::style::Modifier::BOLD),
        ),
        ratatui::text::Span::raw("  "),
        ratatui::text::Span::styled(
            filter_text,
            ratatui::style::Style::default()
                .fg(ratatui::style::Color::Black)
                .bg(ratatui::style::Color::Yellow),
        ),
        ratatui::text::Span::raw("  "),
        ratatui::text::Span::styled(
            sort_text,
            ratatui::style::Style::default()
                .fg(ratatui::style::Color::Black)
                .bg(ratatui::style::Color::Magenta),
        ),
        ratatui::text::Span::raw(format!(
            "  {} sessions",
            app.filtered_indices.len()
        )),
    ]));

    f.render_widget(title, main_layout[0]);

    // Main content: session list + detail pane
    let content_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(45),
            Constraint::Percentage(55),
        ])
        .split(main_layout[1]);

    session_list::draw(f, app, content_layout[0]);
    session_detail::draw(f, app, content_layout[1]);

    // Status bar
    status_bar::draw(f, app, main_layout[2]);

    // Popups overlay
    match app.mode {
        Mode::ConfirmDelete => popups::draw_delete_confirm(f, app),
        Mode::FilterProject => popups::draw_project_filter(f, app),
        Mode::Help => popups::draw_help(f),
        Mode::Rename => popups::draw_rename(f, app),
        _ => {}
    }
}
