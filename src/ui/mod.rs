pub mod pane;
pub mod popups;
pub mod scratchpad;
pub mod session_detail;
pub mod session_list;
pub mod status_bar;
pub mod tabs;
pub mod terminal_pane;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::Frame;

use crate::app::{App, Mode, View, WorkspaceAreas, WorkspaceFocus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttachedLayout {
    pub chat: Rect,
    pub scratchpad: Option<Rect>,
    pub terminal: Option<Rect>,
    pub status: Rect,
}

pub fn draw(f: &mut Frame, app: &mut App) {
    if app.mode == Mode::Scratchpad {
        if let Some(scratchpad) = app.scratchpad.as_mut() {
            scratchpad::draw(f, scratchpad);
        }
        return;
    }

    let size = f.area();

    if matches!(app.view, View::Attached(_)) {
        let layout = attached_layout(size, app.scratchpad.is_some(), app.terminal.is_visible());
        app.workspace_areas = WorkspaceAreas {
            chat: layout.chat,
            scratchpad: layout.scratchpad,
            terminal: layout.terminal,
        };
        pane::draw_chat(f, app, layout.chat);
        if let (Some(area), Some(scratchpad)) = (layout.scratchpad, app.scratchpad.as_mut()) {
            scratchpad::draw_in(
                f,
                scratchpad,
                area,
                app.workspace_focus == WorkspaceFocus::Scratchpad,
            );
        }
        if let (Some(area), Some(terminal)) = (layout.terminal, app.terminal.active()) {
            terminal_pane::draw(
                f,
                terminal,
                app.workspace_focus == WorkspaceFocus::Terminal,
                area,
            );
        }
        pane::draw_status(f, app, layout.status);
        return;
    }

    let main_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title bar
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
        ratatui::text::Span::raw(format!("  {} sessions", app.filtered_indices.len())),
    ]));

    f.render_widget(title, main_layout[0]);

    let content_sections = if app.terminal.is_visible() {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(5),
                Constraint::Length(terminal_panel_height(main_layout[1].height)),
            ])
            .split(main_layout[1])
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(5), Constraint::Length(0)])
            .split(main_layout[1])
    };

    // Main content: session list + detail pane
    let content_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(content_sections[0]);

    session_list::draw(f, app, content_layout[0]);
    session_detail::draw(f, app, content_layout[1]);
    if let Some(terminal) = app.terminal.active().filter(|_| app.terminal.is_visible()) {
        terminal_pane::draw(f, terminal, app.terminal.is_focused(), content_sections[1]);
    }

    // Status bar
    status_bar::draw(f, app, main_layout[2]);

    // Popups overlay
    match app.mode {
        Mode::ConfirmDelete => popups::draw_delete_confirm(f, app),
        Mode::ConfirmForceDelete => popups::draw_force_delete_confirm(f, app),
        Mode::FilterProject => popups::draw_project_filter(f, app),
        Mode::Help => popups::draw_help(f, app),
        Mode::Rename => popups::draw_rename(f, app),
        Mode::Settings => popups::draw_settings(f, app),
        Mode::ProjectSettings => popups::draw_project_settings(f, app),
        Mode::BranchName => popups::draw_branch_name(f, app),
        Mode::PaneList => popups::draw_pane_list(f, app),
        _ => {}
    }

    if app.confirm_quit {
        popups::draw_quit_confirm(f, app);
    }

    // Drawn last so it covers everything: the next loop iteration blocks on Git, and
    // this frame is the only feedback the user gets until it returns.
    if let Some(pending) = app.pending_worktree.as_ref() {
        popups::draw_busy(
            f,
            "Creating worktree",
            &format!(
                "Branch '{}' — copying files and checking out…",
                pending.branch
            ),
        );
    }
}

pub fn terminal_panel_height(content_height: u16) -> u16 {
    if content_height < 10 {
        content_height / 2
    } else {
        (content_height * 2 / 5).clamp(7, 16)
    }
}

pub fn attached_layout(
    area: Rect,
    scratchpad_visible: bool,
    terminal_visible: bool,
) -> AttachedLayout {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);
    let content = vertical[0];
    let status = vertical[1];

    let (top, terminal) = if terminal_visible {
        let sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(5),
                Constraint::Length(terminal_panel_height(content.height)),
            ])
            .split(content);
        (sections[0], Some(sections[1]))
    } else {
        (content, None)
    };

    let (chat, scratchpad) = if scratchpad_visible {
        let sections = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
            .split(top);
        (sections[0], Some(sections[1]))
    } else {
        (top, None)
    };

    AttachedLayout {
        chat,
        scratchpad,
        terminal,
        status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attached_workspace_places_scratchpad_right_and_terminal_below() {
        let layout = attached_layout(Rect::new(0, 0, 120, 40), true, true);
        let scratchpad = layout.scratchpad.unwrap();
        let terminal = layout.terminal.unwrap();

        assert_eq!(layout.status.y, 39);
        assert_eq!(scratchpad.x, layout.chat.right());
        assert_eq!(scratchpad.y, layout.chat.y);
        assert_eq!(terminal.y, layout.chat.bottom());
        assert_eq!(terminal.width, 120);
        assert_eq!(layout.chat.width + scratchpad.width, 120);
    }

    #[test]
    fn hidden_tools_give_the_chat_the_full_content_area() {
        let layout = attached_layout(Rect::new(0, 0, 100, 30), false, false);

        assert_eq!(layout.chat, Rect::new(0, 0, 100, 29));
        assert!(layout.scratchpad.is_none());
        assert!(layout.terminal.is_none());
    }
}
