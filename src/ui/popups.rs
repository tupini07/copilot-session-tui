use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, DeleteTarget, SettingsEditField};

pub(crate) fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
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

/// Warn before quitting while sessions are still running.
///
/// Without a daemon, quitting CST really does terminate every pane — this is the main
/// behavioural difference from tmux, so it must be stated plainly.
pub fn draw_quit_confirm(f: &mut Frame, app: &App) {
    let running: Vec<String> = app
        .mux
        .as_ref()
        .map(|mux| {
            mux.panes
                .iter()
                .filter(|pane| pane.is_running())
                .map(|pane| pane.title.clone())
                .collect()
        })
        .unwrap_or_default();

    let height = (running.len() + 8).min(20) as u16;
    let percent_y = ((height as f32 / f.area().height as f32) * 100.0).min(70.0) as u16;
    let area = centered_rect(60, percent_y.max(30), f.area());
    f.render_widget(Clear, area);

    let mut text = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  Quit and end {} running session(s)?", running.len()),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    for title in running.iter().take(8) {
        text.push(Line::from(Span::styled(
            format!("    • {title}"),
            Style::default().fg(Color::Cyan),
        )));
    }
    if running.len() > 8 {
        text.push(Line::from(Span::styled(
            format!("    … and {} more", running.len() - 8),
            Style::default().fg(Color::DarkGray),
        )));
    }
    text.push(Line::from(""));
    text.push(Line::from(Span::styled(
        "  Sessions do not survive CST exiting.",
        Style::default().fg(Color::Yellow),
    )));
    text.push(Line::from(""));
    text.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            "y",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" quit and end them    "),
        Span::styled(
            "n",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" keep working"),
    ]));

    let block = Block::default()
        .title(" Quit ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red));
    let paragraph = Paragraph::new(text).block(block).wrap(Wrap { trim: false });
    f.render_widget(paragraph, area);
}

/// Switcher over the panes this CST instance owns, opened with `prefix w`.
/// Last path component of a pane's working directory, to disambiguate same-named
/// sessions living in different projects.
fn project_label(cwd: &std::path::Path) -> String {
    cwd.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Blocking-work notice, drawn on the frame before the work actually starts.
pub fn draw_busy(f: &mut Frame, title: &str, detail: &str) {
    let area = centered_rect(50, 20, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(format!(" {title} "))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let text = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  ⠿  {detail}"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  This can take a few seconds.",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    f.render_widget(Paragraph::new(text).wrap(Wrap { trim: true }), inner);
}

pub fn draw_pane_list(f: &mut Frame, app: &App) {
    let Some(mux) = app.mux.as_ref() else {
        return;
    };

    let height = (mux.panes.len() + 5).min(20) as u16;
    let percent_y = ((height as f32 / f.area().height as f32) * 100.0).min(70.0) as u16;
    let area = centered_rect(60, percent_y.max(30), f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(" Sessions ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    let items: Vec<ListItem> = mux
        .panes
        .iter()
        .enumerate()
        .map(|(index, pane)| {
            let title = app.pane_session_title(&pane.session_id, &pane.title);
            let title = if pane.needs_attention() {
                format!("? {title}")
            } else {
                title.to_string()
            };
            let selected = index == app.pane_selected;
            let base = if selected {
                Style::default()
                    .fg(Color::White)
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            let (marker, marker_style) = if !pane.is_running() {
                ("✖", Style::default().fg(Color::Red))
            } else if mux.focused == Some(pane.id) {
                ("●", Style::default().fg(Color::Green))
            } else {
                ("○", Style::default().fg(Color::DarkGray))
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!(" {} ", index + 1), base.fg(Color::Cyan)),
                Span::styled(marker, marker_style),
                Span::styled(format!(" {title}"), base),
                // Panes routinely span several projects, so the title alone is ambiguous.
                Span::styled(
                    format!("  {}", project_label(&pane.cwd)),
                    base.fg(Color::DarkGray),
                ),
            ]))
        })
        .collect();

    f.render_widget(List::new(items), chunks[0]);

    let hint = Line::from(vec![
        Span::raw(" "),
        Span::styled("↑↓", Style::default().fg(Color::Cyan)),
        Span::raw(" select  "),
        Span::styled("Enter", Style::default().fg(Color::Cyan)),
        Span::raw(" attach  "),
        Span::styled("x", Style::default().fg(Color::Cyan)),
        Span::raw(" end  "),
        Span::styled("Esc", Style::default().fg(Color::Cyan)),
        Span::raw(" close"),
    ]);
    f.render_widget(Paragraph::new(hint), chunks[1]);
}

pub fn draw_delete_confirm(f: &mut Frame, app: &App) {
    let area = centered_rect(55, 36, f.area());
    f.render_widget(Clear, area);

    let name = app
        .selected_session()
        .map(|s| s.display_name().to_string())
        .unwrap_or_default();

    let managed = matches!(app.pending_delete, Some(DeleteTarget::Managed { .. }));
    let dirty = matches!(
        app.pending_delete,
        Some(DeleteTarget::Managed { dirty: true, .. })
    );
    let action = if managed {
        "  Delete this session and its TUI-managed worktree?"
    } else {
        "  Delete this session?"
    };

    let mut text = vec![
        Line::from(""),
        Line::from(Span::styled(
            action,
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(format!("  {}", name)),
        Line::from(""),
    ];
    if managed {
        text.push(Line::from(Span::styled(
            "  The worktree will be removed before session metadata.",
            Style::default().fg(Color::Yellow),
        )));
    }
    if dirty {
        text.push(Line::from(Span::styled(
            "  This worktree is dirty; another force confirmation follows.",
            Style::default().fg(Color::Red),
        )));
    }
    text.extend([
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(
                "y",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" Yes  "),
            Span::styled(
                "any key",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" Cancel"),
        ]),
    ]);

    let block = Block::default()
        .title(" Confirm Delete ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red));

    let paragraph = Paragraph::new(text).block(block).wrap(Wrap { trim: false });
    f.render_widget(paragraph, area);
}

pub fn draw_force_delete_confirm(f: &mut Frame, app: &App) {
    let area = centered_rect(60, 36, f.area());
    f.render_widget(Clear, area);
    let path = match &app.pending_delete {
        Some(DeleteTarget::Managed { entry, .. }) => entry.path.display().to_string(),
        _ => String::new(),
    };
    let text = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  FORCE REMOVE DIRTY WORKTREE?",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(format!("  {path}")),
        Line::from(""),
        Line::from(Span::styled(
            "  Modified, staged, and untracked files will be permanently lost.",
            Style::default().fg(Color::Red),
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw("  Press "),
            Span::styled(
                "Shift+Y",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" to force delete; any other key cancels"),
        ]),
    ];
    let block = Block::default()
        .title(" Destructive Confirmation ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red));
    f.render_widget(
        Paragraph::new(text).block(block).wrap(Wrap { trim: false }),
        area,
    );
}

pub fn draw_takeover_confirm(f: &mut Frame, app: &App) {
    let Some(target) = app.pending_takeover.as_ref() else {
        return;
    };
    let area = centered_rect(62, 50, f.area());
    f.render_widget(Clear, area);
    let block = Block::default()
        .title(" Take Over Active Session? ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let owners = if target.pids.len() == 1 {
        format!("Copilot PID {}", target.pids[0])
    } else {
        format!("{} Copilot processes", target.pids.len())
    };
    f.render_widget(
        Paragraph::new(vec![
            Line::from(""),
            Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    &target.title,
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                format!("  This session is active in another process ({owners})."),
                Style::default().fg(Color::Yellow),
            )),
            Line::from(Span::styled(
                "  Taking over ends that exact Copilot process, then resumes the",
                Style::default().fg(Color::Gray),
            )),
            Line::from(Span::styled(
                "  session in this CST instance. In-flight work will be interrupted.",
                Style::default().fg(Color::Gray),
            )),
            Line::from(""),
            Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    "y/Enter",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" take over    "),
                Span::styled(
                    "n/Esc",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" cancel"),
            ]),
        ])
        .wrap(Wrap { trim: false }),
        inner,
    );
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
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
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
        all_items.push((
            0,
            ListItem::new(Line::from(Span::styled("  All Projects", all_style))),
        ));
    }

    for (list_i, &proj_idx) in filtered.iter().enumerate() {
        let project = &app.unique_projects[proj_idx];
        let item_index = if has_all_option { list_i + 1 } else { list_i };
        let is_selected = app.project_selected == item_index;
        let is_active = app.project_filter.as_deref() == Some(project.as_str());

        let prefix = if is_active { "● " } else { "  " };
        let name = std::path::Path::new(project)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(project);

        let display = if app
            .cwd_project
            .as_deref()
            .is_some_and(|p| p.eq_ignore_ascii_case(project))
        {
            format!("{}{} (current dir)", prefix, name)
        } else {
            format!("{}{}", prefix, name)
        };

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

        all_items.push((
            item_index,
            ListItem::new(Line::from(Span::styled(display, style))),
        ));
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

pub fn draw_help(f: &mut Frame, app: &mut App) {
    let area = centered_rect(55, 70, f.area());
    f.render_widget(Clear, area);

    let mut text = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Copilot Session Manager - Keyboard Shortcuts",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!("  CST v{}", env!("CARGO_PKG_VERSION")),
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
        help_line("↑/k ↓/j", "Navigate sessions"),
        help_line("Home/End", "Jump to first/last"),
        help_line("Enter", "Resume selected session"),
        help_line("n", "New session in current project"),
        help_line("N", "New isolated worktree session"),
        help_line("Space", "Toggle selected session favorite"),
        help_line("g", "Grab a favorite, then ↑/↓ to reorder"),
        help_line("T", "Open favorites in Windows Terminal tabs"),
        help_line("e", "Open selected session scratchpad"),
        help_line("r", "Rename selected session"),
        help_line("d", "Delete selected session"),
        Line::from(""),
        help_line("/", "Search / fuzzy filter"),
        help_line("f/p", "Filter by project (type to search)"),
        help_line("c", "Clear project filter"),
        help_line("s", "Cycle sort order"),
        Line::from(""),
        help_line(",", "Global settings"),
        help_line(".", "Filtered-project settings"),
        help_line("?", "Toggle this help"),
        help_line("u", "Update (when available)"),
        help_line("q/Esc", "Quit"),
        help_line("Ctrl+C", "Force quit"),
        Line::from(""),
    ];

    if let Some(prefix) = app.prefix_label() {
        text.push(Line::from(Span::styled(
            format!("  Multiplexer (prefix {prefix})"),
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        )));
        text.push(help_line(
            &format!("{prefix} d"),
            "Back to the list (session keeps running)",
        ));
        text.push(help_line(&format!("{prefix} w"), "Session switcher"));
        text.push(help_line(&format!("{prefix} c"), "Focus the session chat"));
        text.push(help_line(
            &format!("{prefix} e"),
            "Toggle/focus the session scratchpad",
        ));
        text.push(help_line(
            &format!("{prefix} t"),
            "Toggle/focus the session terminal",
        ));
        text.push(help_line(&format!("{prefix} s"), "Open prompt snippets"));
        text.push(help_line(
            &format!("{prefix} u"),
            "Install update without stopping sessions",
        ));
        text.push(help_line(
            &format!("{prefix} C-g i"),
            "Inspect a GitHub issue or pull request",
        ));
        text.push(help_line(
            &format!("{prefix} C-h e"),
            "Open scratchpad shortcut help",
        ));
        text.push(help_line(
            &format!("{prefix} n/p"),
            "Next / previous session",
        ));
        text.push(help_line(
            &format!("{prefix} 1-9"),
            "Jump to session by number",
        ));
        text.push(help_line(
            &format!("{prefix} x"),
            "End the focused session for good",
        ));
        text.push(help_line(
            &format!("{prefix} q"),
            "End the focused session and quit CST",
        ));
        text.push(help_line(
            &format!("{prefix} {prefix}"),
            "Send the prefix key itself",
        ));
        text.push(help_line("", "These work from this list too"));
        text.push(Line::from(""));
        text.push(Line::from(Span::styled(
            "  GitHub references in the chat",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        )));
        text.extend(reference_legend());
        text.push(Line::from(""));
    }

    text.push(Line::from(Span::styled(
        "  ↑/↓ PageUp/PageDown scroll · Esc/q/Enter/? close",
        Style::default().fg(Color::DarkGray),
    )));

    // The list has outgrown the popup, so without scrolling the tail of it --
    // including the multiplexer bindings -- simply cannot be read.
    let viewport = area.height.saturating_sub(2) as usize;
    let max_scroll = text.len().saturating_sub(viewport);
    app.help_scroll = app.help_scroll.min(max_scroll);

    let title = if max_scroll > 0 {
        format!(" Help ({}/{}) ", app.help_scroll + 1, max_scroll + 1)
    } else {
        " Help ".to_string()
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let paragraph = Paragraph::new(text)
        .block(block)
        .scroll((u16::try_from(app.help_scroll).unwrap_or(u16::MAX), 0));
    f.render_widget(paragraph, area);
}

/// Shows what the colours on a `#1234` in the chat mean.
///
/// The kind lives in the hash and the state in the number, which is not
/// guessable, so the help screen spells it out.
fn reference_legend() -> Vec<Line<'static>> {
    let sample = |hash: Color, number: Color, label: &str| -> Vec<Span<'static>> {
        let link = Modifier::UNDERLINED | Modifier::BOLD;
        vec![
            Span::styled("#", Style::default().fg(hash).add_modifier(link)),
            Span::styled("12", Style::default().fg(number).add_modifier(link)),
            Span::styled(format!(" {label}"), Style::default().fg(Color::White)),
        ]
    };

    let mut kinds = vec![Span::raw("  ")];
    kinds.extend(sample(Color::Yellow, Color::Green, "issue     "));
    kinds.extend(sample(Color::Cyan, Color::Green, "pull request"));

    let mut states = vec![Span::raw("  ")];
    states.extend(sample(Color::Cyan, Color::Green, "open  "));
    states.extend(sample(Color::Cyan, Color::Red, "closed  "));
    states.extend(sample(Color::Cyan, Color::Magenta, "merged  "));
    states.extend(sample(Color::Cyan, Color::Gray, "draft"));

    vec![Line::from(kinds), Line::from(states)]
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

pub fn draw_settings(f: &mut Frame, app: &App) {
    let area = centered_rect(65, 78, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(" Global Settings ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    let mut setting_lines = vec![1usize, 4, 7, 10, 13, 16, 19, 22];

    lines.push(Line::from(""));

    // Row 0: Yolo mode
    let yolo_value = if app.config.yolo { "ON" } else { "OFF" };
    let yolo_color = if app.config.yolo {
        Color::Green
    } else {
        Color::Red
    };
    lines.push(settings_row(
        "Yolo Mode",
        yolo_value,
        yolo_color,
        app.settings_selected == 0,
        false,
    ));
    lines.push(Line::from(Span::styled(
        "    Pass --yolo flag (allow all permissions)",
        Style::default().fg(Color::DarkGray),
    )));

    lines.push(Line::from(""));

    // Row 1: Model
    let model_editing = app.settings_editing == Some(SettingsEditField::Model);
    let model_display = if model_editing {
        format!("{}█", app.settings_input)
    } else {
        app.config
            .model
            .as_deref()
            .unwrap_or("(default)")
            .to_string()
    };
    let model_color = if app.config.model.is_some() {
        Color::Cyan
    } else {
        Color::DarkGray
    };
    lines.push(settings_row(
        "Model",
        &model_display,
        model_color,
        app.settings_selected == 1,
        model_editing,
    ));
    lines.push(Line::from(Span::styled(
        "    Default for new sessions only (e.g. gpt-5.2, claude-sonnet-4)",
        Style::default().fg(Color::DarkGray),
    )));

    lines.push(Line::from(""));

    // Row 2: Reasoning effort
    let effort_display = app
        .config
        .reasoning_effort
        .as_deref()
        .unwrap_or("(default)");
    let effort_color = if app.config.reasoning_effort.is_some() {
        Color::Yellow
    } else {
        Color::DarkGray
    };
    lines.push(settings_row(
        "Reasoning Effort",
        effort_display,
        effort_color,
        app.settings_selected == 2,
        false,
    ));
    lines.push(Line::from(Span::styled(
        "    Default for new sessions only (low/medium/high/xhigh)",
        Style::default().fg(Color::DarkGray),
    )));

    lines.push(Line::from(""));
    let prefix_editing = app.settings_editing == Some(SettingsEditField::BranchPrefix);
    let prefix_display = if prefix_editing {
        format!("{}█", app.settings_input)
    } else {
        app.config.worktree.branch_prefix.clone()
    };
    lines.push(settings_row(
        "Branch Prefix",
        &prefix_display,
        Color::Cyan,
        app.settings_selected == 3,
        prefix_editing,
    ));
    lines.push(Line::from(Span::styled(
        "    Default branch prefix for isolated sessions",
        Style::default().fg(Color::DarkGray),
    )));

    lines.push(Line::from(""));
    let root_editing = app.settings_editing == Some(SettingsEditField::WorktreeRoot);
    let root_display = if root_editing {
        format!("{}█", app.settings_input)
    } else {
        app.config.worktree.root.to_string_lossy().to_string()
    };
    lines.push(settings_row(
        "Worktree Root",
        &root_display,
        Color::Cyan,
        app.settings_selected == 4,
        root_editing,
    ));
    lines.push(Line::from(Span::styled(
        "    Relative paths resolve from the global config directory",
        Style::default().fg(Color::DarkGray),
    )));

    lines.push(Line::from(""));
    let mux_value = if app.mux_on_disk { "ON" } else { "OFF" };
    let mux_color = if app.mux_on_disk {
        Color::Green
    } else {
        Color::Red
    };
    lines.push(settings_row(
        "Multiplexer",
        mux_value,
        mux_color,
        app.settings_selected == 5,
        false,
    ));
    lines.push(Line::from(Span::styled(
        "    Run sessions inside CST as panes (applies on restart)",
        Style::default().fg(Color::DarkGray),
    )));

    lines.push(Line::from(""));
    let mux_prefix_editing = app.settings_editing == Some(SettingsEditField::MuxPrefix);
    let mux_prefix_display = if mux_prefix_editing {
        format!("{}█", app.settings_input)
    } else {
        app.config.mux_prefix.clone()
    };
    lines.push(settings_row(
        "Mux Prefix",
        &mux_prefix_display,
        Color::Cyan,
        app.settings_selected == 6,
        mux_prefix_editing,
    ));
    lines.push(Line::from(Span::styled(
        "    Prefix key for pane commands, e.g. C-b, C-g, C-a",
        Style::default().fg(Color::DarkGray),
    )));

    lines.push(Line::from(""));
    let shell_editing = app.settings_editing == Some(SettingsEditField::TerminalShell);
    let shell_display = if shell_editing {
        format!("{}█", app.settings_input)
    } else {
        app.config
            .terminal
            .shell
            .as_deref()
            .unwrap_or("(platform default)")
            .to_string()
    };
    let shell_color = if app.config.terminal.shell.is_some() {
        Color::Cyan
    } else {
        Color::DarkGray
    };
    lines.push(settings_row(
        "Terminal Shell",
        &shell_display,
        shell_color,
        app.settings_selected == 7,
        shell_editing,
    ));
    lines.push(Line::from(Span::styled(
        "    Executable or path; blank uses the platform default",
        Style::default().fg(Color::DarkGray),
    )));

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Notifications (ntfy HTTP)",
        Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    setting_lines.push(lines.len());
    let notifications_enabled = app.config.notifications.enabled;
    lines.push(settings_row(
        "Notifications",
        if notifications_enabled { "ON" } else { "OFF" },
        if notifications_enabled {
            Color::Green
        } else {
            Color::Red
        },
        app.settings_selected == 8,
        false,
    ));
    lines.push(Line::from(Span::styled(
        "    Publish ready/error history directly over HTTP",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(""));

    setting_lines.push(lines.len());
    let server_editing = app.settings_editing == Some(SettingsEditField::NtfyServer);
    let server_display = if server_editing {
        format!("{}█", app.settings_input)
    } else {
        app.config.notifications.server.clone()
    };
    lines.push(settings_row(
        "ntfy Server",
        &server_display,
        Color::Cyan,
        app.settings_selected == 9,
        server_editing,
    ));
    lines.push(Line::from(Span::styled(
        "    Defaults to https://ntfy.sh; self-hosted URLs are supported",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(""));

    setting_lines.push(lines.len());
    let topic_editing = app.settings_editing == Some(SettingsEditField::NtfyTopic);
    let topic_display = if topic_editing {
        format!("{}█", app.settings_input)
    } else if app.config.notifications.topic.is_empty() {
        "(not configured)".to_string()
    } else {
        "•••••••••••• (configured)".to_string()
    };
    lines.push(settings_row(
        "ntfy Topic",
        &topic_display,
        if app.config.notifications.topic.is_empty() && app.settings_input.is_empty() {
            Color::DarkGray
        } else {
            Color::Cyan
        },
        app.settings_selected == 10,
        topic_editing,
    ));
    lines.push(Line::from(Span::styled(
        "    Secret topic; never shown outside edit mode",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(""));

    setting_lines.push(lines.len());
    lines.push(settings_row(
        "Ready Events",
        if app.config.notifications.ready {
            "ON"
        } else {
            "OFF"
        },
        if app.config.notifications.ready {
            Color::Green
        } else {
            Color::Red
        },
        app.settings_selected == 11,
        false,
    ));
    lines.push(Line::from(Span::styled(
        "    Questions, approvals, or completed work ready for review",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(""));

    setting_lines.push(lines.len());
    lines.push(settings_row(
        "Error Events",
        if app.config.notifications.error {
            "ON"
        } else {
            "OFF"
        },
        if app.config.notifications.error {
            Color::Green
        } else {
            Color::Red
        },
        app.settings_selected == 12,
        false,
    ));
    lines.push(Line::from(Span::styled(
        "    Copilot OSC progress error state",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(""));

    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("Enter/Space", Style::default().fg(Color::Magenta)),
        Span::raw(" Edit/Toggle  "),
        Span::styled("Esc/,", Style::default().fg(Color::Magenta)),
        Span::raw(" Save & Close"),
    ]));

    let viewport = inner.height as usize;
    let selected_line = setting_lines
        .get(app.settings_selected)
        .copied()
        .unwrap_or_default();
    let max_scroll = lines.len().saturating_sub(viewport);
    let scroll = selected_line
        .saturating_sub(2)
        .min(max_scroll)
        .min(u16::MAX as usize) as u16;
    let paragraph = Paragraph::new(lines).scroll((scroll, 0));
    f.render_widget(paragraph, inner);
}

pub fn draw_project_settings(f: &mut Frame, app: &App) {
    let area = centered_rect(70, 50, f.area());
    f.render_widget(Clear, area);
    let block = Block::default()
        .title(" Project Settings (.cst.json) ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Blue));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(settings) = app.project_settings.as_ref() else {
        return;
    };
    let prefix_override = settings.branch_prefix_override().is_some();
    let root_override = settings.root_override().is_some();
    let prefix_value = if app.project_settings_editing && app.project_settings_selected == 0 {
        format!("{}█", app.project_settings_input)
    } else {
        settings.effective_branch_prefix().to_string()
    };
    let root_value = if app.project_settings_editing && app.project_settings_selected == 1 {
        format!("{}█", app.project_settings_input)
    } else {
        settings.effective_root().to_string_lossy().to_string()
    };

    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  Repository: {}", settings.repository_root.display()),
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
        project_settings_row(
            "Branch Prefix",
            &prefix_value,
            prefix_override,
            app.project_settings_selected == 0,
            app.project_settings_editing && app.project_settings_selected == 0,
        ),
        Line::from(Span::styled(
            "    Effective prefix used to prepopulate isolated branches",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
        project_settings_row(
            "Worktree Root",
            &root_value,
            root_override,
            app.project_settings_selected == 1,
            app.project_settings_editing && app.project_settings_selected == 1,
        ),
        Line::from(Span::styled(
            "    Relative overrides resolve from the repository root",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled("Space", Style::default().fg(Color::Blue)),
            Span::raw(" Inherit/Override  "),
            Span::styled("Enter", Style::default().fg(Color::Blue)),
            Span::raw(" Edit  "),
            Span::styled("Esc/.", Style::default().fg(Color::Blue)),
            Span::raw(" Save & Close"),
        ]),
    ];
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

pub fn draw_branch_name(f: &mut Frame, app: &App) {
    let area = centered_rect(65, 28, f.area());
    f.render_widget(Clear, area);
    let text = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Branch for isolated worktree session:",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(&app.branch_input, Style::default().fg(Color::White)),
            Span::styled("█", Style::default().fg(Color::Cyan)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled("Enter", Style::default().fg(Color::Cyan)),
            Span::raw(" Create  "),
            Span::styled("Esc", Style::default().fg(Color::Cyan)),
            Span::raw(" Cancel"),
        ]),
    ];
    let block = Block::default()
        .title(" New Isolated Session ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    f.render_widget(Paragraph::new(text).block(block), area);
}

fn settings_row<'a>(
    label: &str,
    value: &str,
    value_color: Color,
    is_selected: bool,
    is_editing: bool,
) -> Line<'a> {
    let pointer = if is_selected { "▸ " } else { "  " };
    let label_style = if is_selected {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };
    let value_style = if is_editing {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(value_color)
    };

    Line::from(vec![
        Span::styled(pointer.to_string(), Style::default().fg(Color::Magenta)),
        Span::styled(format!("{:<20}", label), label_style),
        Span::styled(value.to_string(), value_style),
    ])
}

fn project_settings_row<'a>(
    label: &str,
    value: &str,
    is_override: bool,
    is_selected: bool,
    is_editing: bool,
) -> Line<'a> {
    let pointer = if is_selected { "▸ " } else { "  " };
    let state = if is_override { "Override" } else { "Inherited" };
    let state_color = if is_override {
        Color::Yellow
    } else {
        Color::DarkGray
    };
    let value_color = if is_editing {
        Color::Yellow
    } else {
        Color::Cyan
    };
    Line::from(vec![
        Span::styled(pointer.to_string(), Style::default().fg(Color::Blue)),
        Span::styled(format!("{:<18}", label), Style::default().fg(Color::White)),
        Span::styled(format!("[{state:<9}] "), Style::default().fg(state_color)),
        Span::styled(value.to_string(), Style::default().fg(value_color)),
    ])
}

#[cfg(test)]
mod help_tests {
    use super::*;
    use crate::app::App;
    use crate::config::UserConfig;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn rendered_with(
        config: UserConfig,
        width: u16,
        height: u16,
        scroll: usize,
    ) -> (String, usize) {
        let mut app = App::new(Vec::new(), config);
        app.mode = crate::app::Mode::Help;
        app.help_scroll = scroll;
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw_help(f, &mut app)).expect("draw");
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        (text, app.help_scroll)
    }

    fn rendered(width: u16, height: u16, scroll: usize) -> (String, usize) {
        rendered_with(UserConfig::default(), width, height, scroll)
    }

    const FOOTER: &str = "Esc/q/Enter/? close";

    #[test]
    fn a_help_screen_taller_than_its_popup_can_be_scrolled_to_the_end() {
        let (top, _) = rendered(120, 40, 0);
        assert!(top.contains("Keyboard Shortcuts"), "got:\n{top}");
        assert!(
            !top.contains(FOOTER),
            "the help fits after all, so scrolling is pointless:\n{top}"
        );

        let (bottom, clamped) = rendered(120, 40, usize::MAX);
        assert!(bottom.contains(FOOTER), "got:\n{bottom}");
        assert!(
            clamped < usize::MAX,
            "End should be clamped to the last page, got {clamped}"
        );
    }

    #[test]
    fn a_help_screen_that_fits_is_not_scrolled() {
        let (text, clamped) = rendered(200, 100, usize::MAX);

        assert!(text.contains("Keyboard Shortcuts"), "got:\n{text}");
        assert!(
            text.contains(&format!("CST v{}", env!("CARGO_PKG_VERSION"))),
            "running version missing:\n{text}"
        );
        assert!(text.contains(FOOTER), "got:\n{text}");
        assert_eq!(clamped, 0);
    }

    #[test]
    fn the_reference_legend_is_reachable_under_the_multiplexer_section() {
        let config = UserConfig {
            mux: true,
            ..UserConfig::default()
        };
        let (text, _) = rendered_with(config, 200, 100, 0);

        assert!(
            text.contains("GitHub references in the chat"),
            "got:\n{text}"
        );
        assert!(text.contains("C-b C-g i"), "got:\n{text}");
        assert!(text.contains("Inspect a GitHub issue"), "got:\n{text}");
        assert!(text.contains("pull request"), "got:\n{text}");
    }

    #[test]
    fn takeover_popup_names_the_session_and_interruption_risk() {
        let mut app = App::new(Vec::new(), UserConfig::default());
        app.pending_takeover = Some(crate::app::TakeoverTarget {
            id: "session-id".to_string(),
            cwd: "C:/repo".to_string(),
            title: "Important work".to_string(),
            dir_path: std::path::PathBuf::from("session-id"),
            pids: vec![1234],
        });
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();

        terminal
            .draw(|frame| draw_takeover_confirm(frame, &app))
            .unwrap();

        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("Take Over Active Session"), "got:\n{text}");
        assert!(text.contains("Important work"), "got:\n{text}");
        assert!(text.contains("Copilot PID"), "got:\n{text}");
        assert!(text.contains("1234"), "got:\n{text}");
        assert!(text.contains("In-flight work will be"), "got:\n{text}");
        assert!(text.contains("interrupted."), "got:\n{text}");
        assert!(text.contains("y/Enter take over"), "got:\n{text}");
    }

    #[test]
    fn notification_settings_mask_the_saved_topic_but_reveal_it_while_editing() {
        let mut app = App::new(Vec::new(), UserConfig::default());
        app.settings_selected = 12;
        app.config.notifications.enabled = true;
        app.config.notifications.topic = "super_secret_phone_topic".to_string();
        let mut terminal = Terminal::new(TestBackend::new(100, 35)).unwrap();

        terminal.draw(|frame| draw_settings(frame, &app)).unwrap();

        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("Notifications (ntfy HTTP)"), "got:\n{text}");
        assert!(text.contains("Error Events"), "got:\n{text}");
        assert!(text.contains("Save & Close"), "got:\n{text}");
        assert!(text.contains("(configured)"), "got:\n{text}");
        assert!(
            !text.contains("super_secret_phone_topic"),
            "topic leaked outside edit mode:\n{text}"
        );

        app.settings_selected = 10;
        app.settings_editing = Some(SettingsEditField::NtfyTopic);
        app.settings_input = "super_secret_phone_topic".to_string();
        terminal.draw(|frame| draw_settings(frame, &app)).unwrap();
        let editing: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            editing.contains("super_secret_phone_topic█"),
            "editable topic and cursor missing:\n{editing}"
        );
    }
}
