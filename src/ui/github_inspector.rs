use crate::app::{App, FilesPane, GithubInspector, GithubInspectorScreen, GithubTab};
use crate::github::{DiscussionKind, GithubItem};
use crate::text;
use crate::ui::file_tree;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use ratatui::Frame;

pub fn is_prompt(app: &App) -> bool {
    app.github_inspector
        .as_ref()
        .is_some_and(|inspector| matches!(inspector.screen, GithubInspectorScreen::NumberPrompt))
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let Some(inspector) = app.github_inspector.as_ref() else {
        return;
    };
    match &inspector.screen {
        GithubInspectorScreen::NumberPrompt => draw_prompt(f, inspector),
        GithubInspectorScreen::Loading => draw_loading(f, inspector),
        GithubInspectorScreen::Error(message) => draw_error(f, inspector, message),
        GithubInspectorScreen::Ready(_) => draw_ready(f, app),
    }
}

fn draw_prompt(f: &mut Frame, inspector: &GithubInspector) {
    let area = centered_rect(58, 9, f.area());
    f.render_widget(Clear, area);
    let block = Block::default()
        .title(" Inspect GitHub item ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let error = inspector
        .prompt_error
        .as_deref()
        .map(|message| Span::styled(message, Style::default().fg(Color::Red)))
        .unwrap_or_else(|| {
            Span::styled(
                "Enter an issue or pull request number",
                Style::default().fg(Color::DarkGray),
            )
        });
    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::raw("  # "),
            Span::styled(
                inspector.input.clone(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("█", Style::default().fg(Color::Magenta)),
        ]),
        Line::from(""),
        Line::from(vec![Span::raw("  "), error]),
        Line::from(""),
        Line::from(vec![
            key("Enter"),
            Span::raw(" inspect   "),
            key("Esc"),
            Span::raw(" cancel"),
        ]),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_loading(f: &mut Frame, inspector: &GithubInspector) {
    let number = inspector.number.unwrap_or_default();
    let frame = spinner_frame();
    draw_message_screen(
        f,
        " GitHub Inspector ",
        Color::Magenta,
        vec![
            Line::from(""),
            Line::from(Span::styled(
                format!("  {frame}  Loading GitHub item #{number}..."),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  The attached session remains live while gh fetches the item.",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(""),
            Line::from(vec![key("Esc"), Span::raw(" close")]),
        ],
    );
}

fn draw_error(f: &mut Frame, inspector: &GithubInspector, message: &str) {
    let number = inspector.number.unwrap_or_default();
    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  Could not load GitHub item #{number}"),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    for line in text::wrap_text(message, f.area().width.saturating_sub(8) as usize) {
        lines.push(Line::from(format!("  {line}")));
    }
    lines.extend([
        Line::from(""),
        Line::from(vec![
            key("r"),
            Span::raw(" retry   "),
            key("Esc"),
            Span::raw(" close"),
        ]),
    ]);
    draw_message_screen(f, " GitHub Inspector — Error ", Color::Red, lines);
}

fn draw_message_screen(f: &mut Frame, title: &str, color: Color, lines: Vec<Line<'static>>) {
    let area = f.area();
    f.render_widget(Clear, area);
    let block = Block::default()
        .title(title.to_string())
        .borders(Borders::ALL)
        .border_style(Style::default().fg(color));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_ready(f: &mut Frame, app: &mut App) {
    let area = f.area();
    f.render_widget(Clear, area);
    let outer = Block::default()
        .title(" GitHub Inspector ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta));
    let inner = outer.inner(area);
    f.render_widget(outer, area);
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);

    let Some(inspector) = app.github_inspector.as_ref() else {
        return;
    };
    let Some(item) = inspector.ready_item() else {
        return;
    };
    draw_header(f, item, sections[0]);
    draw_tabs(f, inspector, item, sections[1]);

    if inspector.tab == GithubTab::Files && item.is_pull_request() {
        draw_files_tab(f, app, sections[2]);
        let inspector = app.github_inspector.as_ref().expect("inspector is active");
        draw_footer(f, inspector, sections[3]);
        return;
    }

    let content = sections[2];
    let viewport_width = content.width.saturating_sub(1) as usize;
    let viewport_height = content.height as usize;
    let lines = build_active_lines(inspector, item, viewport_width);
    let line_count = lines.len();
    let max_scroll = line_count.saturating_sub(viewport_height);

    if let Some(inspector) = app.github_inspector.as_mut() {
        inspector.max_scroll = max_scroll;
        let tab = inspector.tab.index();
        inspector.scroll_offsets[tab] = inspector.scroll_offsets[tab].min(max_scroll);
    }

    let actual_offset = app
        .github_inspector
        .as_ref()
        .map(|inspector| inspector.active_scroll())
        .unwrap_or_default();
    let paragraph = Paragraph::new(lines).scroll((actual_offset.min(u16::MAX as usize) as u16, 0));
    f.render_widget(paragraph, content);
    draw_scrollbar(f, content, line_count, viewport_height, actual_offset);

    let inspector = app.github_inspector.as_ref().expect("inspector is active");
    draw_footer(f, inspector, sections[3]);
}

/// Minimum content width before the changed-file tree and the diff can usefully
/// share a row; below it only the focused pane is shown.
const SPLIT_MIN_WIDTH: u16 = 76;
const TREE_MIN_WIDTH: u16 = 26;
const TREE_MAX_WIDTH: u16 = 52;

/// Draw the tree of changed files beside the selected file's diff.
fn draw_files_tab(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(inspector) = app.github_inspector.as_ref() else {
        return;
    };
    let Some(item) = inspector.ready_item() else {
        return;
    };
    let rows = file_tree::build_rows(item.files(), &inspector.collapsed_dirs);
    let split = area.width >= SPLIT_MIN_WIDTH && !rows.is_empty();
    let focus = inspector.files_pane;

    let (tree_area, diff_area) = if split {
        let tree_width = (area.width / 3).clamp(TREE_MIN_WIDTH, TREE_MAX_WIDTH);
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(tree_width), Constraint::Min(1)])
            .split(area);
        (chunks[0], chunks[1])
    } else if focus == FilesPane::Diff {
        (Rect::default(), area)
    } else {
        (area, Rect::default())
    };

    // A divider doubles as a focus cue, so it is obvious which pane takes keys.
    let tree_body = if split {
        let divider = Block::default()
            .borders(Borders::RIGHT)
            .border_style(Style::default().fg(if focus == FilesPane::Tree {
                Color::Magenta
            } else {
                Color::DarkGray
            }));
        let inner = divider.inner(tree_area);
        f.render_widget(divider, tree_area);
        inner
    } else {
        tree_area
    };

    let tree_lines = tree_row_lines(
        &rows,
        item.files(),
        inspector,
        tree_body.width as usize,
        focus,
    );
    let diff_width = diff_area.width.saturating_sub(1) as usize;
    let diff_content = diff_lines_for_selection(inspector, item, &rows, diff_width);
    let diff_height = diff_area.height as usize;
    let max_diff_scroll = diff_content.len().saturating_sub(diff_height);
    let max_diff_horizontal = diff_content
        .iter()
        .map(line_width)
        .max()
        .unwrap_or_default()
        .saturating_sub(diff_width);

    if let Some(inspector) = app.github_inspector.as_mut() {
        inspector.tree_area = tree_body;
        inspector.diff_area = diff_area;
        inspector.visible_tree_rows = tree_body.height as usize;
        inspector.max_diff_scroll = max_diff_scroll;
        inspector.max_diff_horizontal = max_diff_horizontal;
        inspector.diff_scroll = inspector.diff_scroll.min(max_diff_scroll);
        inspector.diff_horizontal = inspector.diff_horizontal.min(max_diff_horizontal);
        inspector.tree_selected = inspector.tree_selected.min(rows.len().saturating_sub(1));
        clamp_tree_offset(inspector, rows.len());
    }

    let inspector = app.github_inspector.as_ref().expect("inspector is active");
    if tree_body.width > 0 && tree_body.height > 0 {
        f.render_widget(
            Paragraph::new(tree_lines)
                .scroll((inspector.tree_offset.min(u16::MAX as usize) as u16, 0)),
            tree_body,
        );
        draw_scrollbar(
            f,
            tree_body,
            rows.len(),
            tree_body.height as usize,
            inspector.tree_offset,
        );
    }
    if diff_area.width > 0 && diff_area.height > 0 {
        // Inset the text so it does not touch the divider; clicks on the gutter
        // still land in `diff_area` and focus the pane.
        let diff_body = Rect {
            x: diff_area.x + 1,
            width: diff_area.width.saturating_sub(1),
            ..diff_area
        };
        f.render_widget(
            Paragraph::new(diff_content).scroll((
                inspector.diff_scroll.min(u16::MAX as usize) as u16,
                inspector.diff_horizontal.min(u16::MAX as usize) as u16,
            )),
            diff_body,
        );
        draw_scrollbar(
            f,
            diff_area,
            max_diff_scroll + diff_height,
            diff_height,
            inspector.diff_scroll,
        );
    }
}

/// Keep the selected row on screen.
fn clamp_tree_offset(inspector: &mut GithubInspector, row_count: usize) {
    let height = inspector.visible_tree_rows;
    if height == 0 {
        return;
    }
    let max_offset = row_count.saturating_sub(height);
    if inspector.tree_selected < inspector.tree_offset {
        inspector.tree_offset = inspector.tree_selected;
    } else if inspector.tree_selected >= inspector.tree_offset + height {
        inspector.tree_offset = inspector.tree_selected + 1 - height;
    }
    inspector.tree_offset = inspector.tree_offset.min(max_offset);
}

fn tree_row_lines(
    rows: &[file_tree::TreeRow],
    files: &[crate::github::ChangedFile],
    inspector: &GithubInspector,
    width: usize,
    focus: FilesPane,
) -> Vec<Line<'static>> {
    if rows.is_empty() {
        return vec![Line::from(Span::styled(
            " No changed files",
            Style::default().fg(Color::DarkGray),
        ))];
    }
    rows.iter()
        .enumerate()
        .map(|(index, row)| {
            tree_row_line(row, files, index == inspector.tree_selected, focus, width)
        })
        .collect()
}

/// Single-letter status marker, so the path keeps as much width as possible.
fn status_marker(status: &str) -> (&'static str, Color) {
    match status {
        "added" => ("A", Color::Green),
        "removed" => ("D", Color::Red),
        "modified" => ("M", Color::Yellow),
        "renamed" => ("R", Color::Magenta),
        _ => ("·", Color::DarkGray),
    }
}

fn tree_row_line(
    row: &file_tree::TreeRow,
    files: &[crate::github::ChangedFile],
    selected: bool,
    focus: FilesPane,
    width: usize,
) -> Line<'static> {
    let indent = " ".repeat(row.depth * 2 + 1);
    let (marker, marker_color, name_color, stats) = match &row.kind {
        file_tree::RowKind::Directory {
            expanded,
            additions,
            deletions,
            files: count,
            ..
        } => (
            if *expanded { "▾" } else { "▸" }.to_string(),
            Color::Blue,
            Color::Blue,
            format!("{count} · +{additions} -{deletions}"),
        ),
        file_tree::RowKind::File { index } => {
            let file = files.get(*index);
            let (marker, color) = file
                .map(|file| status_marker(&file.status))
                .unwrap_or(("·", Color::DarkGray));
            let stats = file
                .map(|file| format!("+{} -{}", file.additions, file.deletions))
                .unwrap_or_default();
            (marker.to_string(), color, Color::White, stats)
        }
    };

    // Reserve the stats column, then fit the name into whatever is left.
    let prefix = format!("{indent}{marker} ");
    let prefix_width = text::display_width(&prefix);
    let stats_width = if stats.is_empty() { 0 } else { stats.len() + 1 };
    let name_budget = width
        .saturating_sub(prefix_width)
        .saturating_sub(stats_width)
        .max(1);
    let name = text::truncate_to_width(&row.label, name_budget);
    let padding = width
        .saturating_sub(prefix_width + text::display_width(&name) + stats.len())
        .max(1);

    let (base, name_style, stats_style) = if selected {
        // The unfocused pane keeps a dimmer cursor so focus is never ambiguous.
        let highlight = if focus == FilesPane::Tree {
            Style::default().fg(Color::Black).bg(Color::Cyan)
        } else {
            Style::default().fg(Color::White).bg(Color::Rgb(60, 60, 80))
        }
        .add_modifier(Modifier::BOLD);
        (highlight, highlight, highlight)
    } else {
        (
            Style::default().fg(marker_color),
            Style::default().fg(name_color),
            Style::default().fg(Color::DarkGray),
        )
    };

    let mut spans = vec![Span::styled(prefix, base), Span::styled(name, name_style)];
    if !stats.is_empty() {
        spans.push(Span::styled(" ".repeat(padding), stats_style));
        spans.push(Span::styled(stats, stats_style));
    }
    Line::from(spans)
}

/// Diff for the selected row: a file's patch, or a summary for a directory.
fn diff_lines_for_selection(
    inspector: &GithubInspector,
    item: &GithubItem,
    rows: &[file_tree::TreeRow],
    width: usize,
) -> Vec<Line<'static>> {
    match rows.get(inspector.tree_selected).map(|row| &row.kind) {
        Some(file_tree::RowKind::Directory {
            path,
            files,
            additions,
            deletions,
            ..
        }) => vec![
            Line::from(Span::styled(
                format!(" {path}/ "),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
            )),
            summary_line(
                "Directory",
                format!("{files} changed files · +{additions} -{deletions}"),
            ),
            Line::from(""),
            Line::from(Span::styled(
                "Select a file to see its diff.",
                Style::default().fg(Color::DarkGray),
            )),
        ],
        _ => diff_lines(inspector, item, width),
    }
}

/// Like `field`, but flush left: the diff pane already has a one-column gutter.
fn summary_line(label: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label}: "),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(value, Style::default().fg(Color::White)),
    ])
}

fn draw_header(f: &mut Frame, item: &GithubItem, area: Rect) {
    let common = item.common();
    let kind = if item.is_pull_request() {
        "PR"
    } else {
        "Issue"
    };
    let state_color = match common.state.to_ascii_lowercase().as_str() {
        "open" => Color::Green,
        _ => Color::Magenta,
    };
    let line1 = Line::from(vec![
        Span::styled(
            format!(
                " {}  {kind} #{} ",
                common.repository.name_with_owner(),
                common.number
            ),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            common.state.to_ascii_uppercase(),
            Style::default()
                .fg(state_color)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    let line2 = Line::from(Span::styled(
        format!(" {}", common.title),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ));
    f.render_widget(Paragraph::new(vec![line1, line2]), area);
}

fn draw_tabs(f: &mut Frame, inspector: &GithubInspector, item: &GithubItem, area: Rect) {
    let tabs = if item.is_pull_request() {
        [
            Some(GithubTab::Overview),
            Some(GithubTab::Comments),
            Some(GithubTab::Files),
        ]
    } else {
        [Some(GithubTab::Overview), Some(GithubTab::Comments), None]
    };
    let mut spans = vec![Span::raw(" ")];
    for tab in tabs.into_iter().flatten() {
        let name = match tab {
            GithubTab::Overview => " Overview ",
            GithubTab::Comments => " Comments ",
            GithubTab::Files => " Files ",
        };
        let style = if inspector.tab == tab {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Magenta)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        spans.push(Span::styled(name, style));
        spans.push(Span::raw(" "));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::Rgb(30, 30, 40))),
        area,
    );
}

fn build_active_lines(
    inspector: &GithubInspector,
    item: &GithubItem,
    width: usize,
) -> Vec<Line<'static>> {
    match inspector.tab {
        GithubTab::Overview => overview_lines(item, width),
        GithubTab::Comments => comment_lines(item, width),
        // The Files tab draws its own split panes; an item without files has none.
        GithubTab::Files => vec![Line::from(Span::styled(
            " No changed files",
            Style::default().fg(Color::DarkGray),
        ))],
    }
}

fn overview_lines(item: &GithubItem, width: usize) -> Vec<Line<'static>> {
    let common = item.common();
    let mut lines = vec![
        field("Author", format!("@{}", common.author.login)),
        field("Created", common.created_at.clone()),
        field("Updated", common.updated_at.clone()),
        field("URL", common.url.clone()),
    ];
    if !common.labels.is_empty() {
        lines.push(field(
            "Labels",
            common
                .labels
                .iter()
                .map(|label| label.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    if let GithubItem::PullRequest(pull) = item {
        let state = if pull.merged {
            "merged"
        } else if pull.draft {
            "draft"
        } else {
            pull.common.state.as_str()
        };
        lines.extend([
            field("PR state", state.to_string()),
            field(
                "Branches",
                format!("{} <- {}", pull.base_ref, pull.head_ref),
            ),
            field(
                "Changes",
                format!(
                    "+{} -{} across {} files",
                    pull.additions, pull.deletions, pull.changed_files
                ),
            ),
        ]);
        if let Some(mergeable) = &pull.mergeable_state {
            lines.push(field("Mergeability", mergeable.clone()));
        }
    }
    lines.push(Line::from(""));
    lines.push(section("Description"));
    lines.push(Line::from(""));
    let body = if common.body.trim().is_empty() {
        "(no description)"
    } else {
        common.body.as_str()
    };
    lines.extend(
        text::wrap_text(body, width.saturating_sub(2).max(1))
            .into_iter()
            .map(|line| Line::from(format!(" {line}"))),
    );
    lines
}

fn comment_lines(item: &GithubItem, width: usize) -> Vec<Line<'static>> {
    let entries = item.discussion();
    if entries.is_empty() {
        return vec![Line::from(Span::styled(
            " No comments or reviews",
            Style::default().fg(Color::DarkGray),
        ))];
    }
    let mut lines = Vec::new();
    for entry in entries {
        let kind = match entry.kind {
            DiscussionKind::Comment => "Comment",
            DiscussionKind::Review => "Review",
            DiscussionKind::InlineReview => "Inline review",
        };
        let mut context = format!("{kind} by @{} · {}", entry.author.login, entry.created_at);
        if let Some(state) = &entry.review_state {
            context.push_str(&format!(" · {}", state.to_ascii_uppercase()));
        }
        if let Some(path) = &entry.path {
            context.push_str(&format!(" · {path}"));
            if let Some(line) = entry.line {
                context.push_str(&format!(":{line}"));
            }
        }
        lines.push(Line::from(Span::styled(
            format!(" {context}"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        let body = if entry.body.trim().is_empty() {
            "(no comment body)"
        } else {
            entry.body.as_str()
        };
        lines.extend(
            text::wrap_text(body, width.saturating_sub(3).max(1))
                .into_iter()
                .map(|line| Line::from(format!("   {line}"))),
        );
        lines.push(Line::from(Span::styled(
            "─".repeat(width.max(1)),
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines
}

fn diff_lines(inspector: &GithubInspector, item: &GithubItem, width: usize) -> Vec<Line<'static>> {
    let Some(file) = item.files().get(inspector.selected_file) else {
        return vec![Line::from(Span::styled(
            "Select a file to see its diff.",
            Style::default().fg(Color::DarkGray),
        ))];
    };
    let mut lines = vec![
        Line::from(Span::styled(
            format!(" {} ", text::truncate_to_width(&file.path, width.max(1))),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        summary_line(
            "File",
            format!(
                "{} · +{} -{} · {} changed lines",
                file.status, file.additions, file.deletions, file.changes
            ),
        ),
        Line::from(""),
    ];
    let Some(patch) = &file.patch else {
        lines.push(Line::from(Span::styled(
            "Diff unavailable: GitHub omitted the patch (the file may be binary or too large).",
            Style::default().fg(Color::Yellow),
        )));
        return lines;
    };
    lines.extend(patch.lines().map(|line| {
        let style = if line.starts_with("@@") {
            Style::default().fg(Color::Cyan)
        } else if line.starts_with("+++") || line.starts_with("---") {
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD)
        } else if line.starts_with('+') {
            Style::default().fg(Color::Green)
        } else if line.starts_with('-') {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Gray)
        };
        Line::from(Span::styled(line.to_string(), style))
    }));
    lines
}

fn draw_footer(f: &mut Frame, inspector: &GithubInspector, area: Rect) {
    let mut spans = vec![Span::raw(" "), key("Tab/Shift+Tab"), Span::raw(" tabs  ")];
    if inspector.tab == GithubTab::Files {
        if inspector.files_pane == FilesPane::Diff {
            spans.extend([
                key("↑↓/PgUp/PgDn"),
                Span::raw(" scroll  "),
                key("←→"),
                Span::raw(" horizontal  "),
                key("Esc"),
                Span::raw(" files"),
            ]);
        } else {
            spans.extend([
                key("↑↓"),
                Span::raw(" select  "),
                key("←→"),
                Span::raw(" fold  "),
                key("Enter"),
                Span::raw(" diff  "),
                key("Esc"),
                Span::raw(" close"),
            ]);
        }
    } else {
        spans.extend([
            key("↑↓/PgUp/PgDn"),
            Span::raw(" navigate  "),
            key("wheel"),
            Span::raw(" scroll  "),
            key("Esc"),
            Span::raw(" close"),
        ]);
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::Rgb(30, 30, 40))),
        area,
    );
}

fn draw_scrollbar(
    f: &mut Frame,
    area: Rect,
    line_count: usize,
    viewport_height: usize,
    offset: usize,
) {
    if viewport_height == 0 || line_count <= viewport_height {
        return;
    }
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None)
        .track_symbol(Some("│"))
        .track_style(Style::default().fg(Color::DarkGray))
        .thumb_symbol("█")
        .thumb_style(Style::default().fg(Color::Magenta));
    let mut state = ScrollbarState::new(line_count)
        .position(offset)
        .viewport_content_length(viewport_height);
    f.render_stateful_widget(scrollbar, area, &mut state);
}

fn field(label: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!(" {label}: "),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(value, Style::default().fg(Color::White)),
    ])
}

fn section(title: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!(" -- {title} --"),
        Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
    ))
}

fn key(label: &str) -> Span<'static> {
    Span::styled(
        label.to_string(),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
}

fn line_width(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|span| text::display_width(span.content.as_ref()))
        .sum()
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width.saturating_sub(2)).max(1);
    let height = height.min(area.height.saturating_sub(2)).max(1);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn spinner_frame() -> &'static str {
    const FRAMES: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
    let tick = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() / 120)
        .unwrap_or_default();
    FRAMES[tick as usize % FRAMES.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::GithubInspector;
    use crate::config::UserConfig;
    use crate::github::{
        Author, ChangedFile, DiscussionEntry, Issue, ItemCommon, Label, PullRequest, RepositoryRef,
    };
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn common(number: u64) -> ItemCommon {
        ItemCommon {
            repository: RepositoryRef {
                host: "github.com".to_string(),
                owner: "octo".to_string(),
                name: "widgets".to_string(),
            },
            number,
            title: "Inspector rendering".to_string(),
            state: "open".to_string(),
            author: Author {
                login: "monalisa".to_string(),
            },
            labels: vec![Label {
                name: "enhancement".to_string(),
                color: Some("00ff00".to_string()),
            }],
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-02T00:00:00Z".to_string(),
            url: format!("https://github.com/octo/widgets/issues/{number}"),
            body: "A description with enough words to wrap cleanly.".to_string(),
        }
    }

    fn issue() -> GithubItem {
        GithubItem::Issue(Issue {
            common: common(12),
            comments: vec![DiscussionEntry {
                kind: DiscussionKind::Comment,
                author: Author {
                    login: "reviewer".to_string(),
                },
                body: "Looks useful.".to_string(),
                created_at: "2026-01-03T00:00:00Z".to_string(),
                review_state: None,
                path: None,
                line: None,
            }],
        })
    }

    fn pull(patch: Option<&str>) -> GithubItem {
        GithubItem::PullRequest(PullRequest {
            common: common(34),
            draft: false,
            merged: false,
            mergeable_state: Some("clean".to_string()),
            base_ref: "main".to_string(),
            head_ref: "feature".to_string(),
            additions: 2,
            deletions: 1,
            changed_files: 1,
            discussion: vec![],
            files: vec![ChangedFile {
                path: "src/lib.rs".to_string(),
                status: "modified".to_string(),
                additions: 2,
                deletions: 1,
                changes: 3,
                patch: patch.map(str::to_string),
            }],
        })
    }

    fn app_with(item: GithubItem) -> App {
        let mut app = App::new(Vec::new(), UserConfig::default());
        let mut inspector = GithubInspector::number_prompt();
        inspector.screen = GithubInspectorScreen::Ready(item);
        inspector.select_first_tree_file();
        app.github_inspector = Some(inspector);
        app
    }

    fn render(app: &mut App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    /// The rendered screen split into rows, for assertions that care about
    /// column alignment rather than just the presence of text.
    fn render_rows(app: &mut App, width: u16, height: u16) -> Vec<String> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect()
            })
            .collect()
    }

    /// Locks in the shape of the tree: aggregated directory totals, status
    /// letters, per-file stats, and an automatically selected first file.
    #[test]
    fn the_tree_summarizes_directories_and_marks_file_status() {
        let mut item = pull(Some("@@ -1,2 +1,2 @@\n-old\n+new"));
        if let GithubItem::PullRequest(pull) = &mut item {
            for (path, status, add, del) in [
                ("src/ui/github_inspector.rs", "modified", 120, 44),
                ("src/ui/file_tree.rs", "added", 210, 0),
                ("docs/old.md", "removed", 0, 31),
            ] {
                pull.files.push(ChangedFile {
                    path: path.to_string(),
                    status: status.to_string(),
                    additions: add,
                    deletions: del,
                    changes: add + del,
                    patch: Some("@@ -1 +1 @@".to_string()),
                });
            }
        }
        let mut app = app_with(item);
        app.github_inspector.as_mut().unwrap().tab = GithubTab::Files;

        let rows = render_rows(&mut app, 110, 24);
        // Each row is `│<tree>│<diff>│`; split on the interior divider.
        let split = |row: &String| -> (String, String) {
            let chars: Vec<char> = row.chars().skip(1).collect();
            let divider = chars.iter().position(|c| *c == '│').unwrap_or(chars.len());
            (
                chars[..divider]
                    .iter()
                    .collect::<String>()
                    .trim()
                    .to_string(),
                chars[divider..].iter().collect(),
            )
        };
        let tree: Vec<String> = rows
            .iter()
            .skip(4)
            .map(|row| split(row).0)
            .take_while(|row| !row.is_empty())
            .collect();

        assert_eq!(
            tree,
            vec![
                "▾ docs                 1 · +0 -31",
                "D old.md                  +0 -31",
                "▾ src                3 · +332 -45",
                "▾ ui               2 · +330 -44",
                "A file_tree.rs         +210 -0",
                "M github_inspector.rs +120 -44",
                "M lib.rs                   +2 -1",
            ],
            "rendered:\n{}",
            rows.join("\n")
        );

        // `docs/old.md` sorts first, so its diff is already on screen.
        let diff: String = rows.iter().map(|row| split(row).1).collect();
        assert!(
            diff.contains("docs/old.md"),
            "rendered:\n{}",
            rows.join("\n")
        );
        assert!(
            diff.contains("File: removed"),
            "rendered:\n{}",
            rows.join("\n")
        );
    }

    #[test]
    fn issue_renders_only_overview_and_comments_tabs() {
        let mut app = app_with(issue());

        let text = render(&mut app, 100, 28);

        assert!(text.contains("Issue #12"), "got:\n{text}");
        assert!(text.contains("Overview"), "got:\n{text}");
        assert!(text.contains("Comments"), "got:\n{text}");
        assert!(!text.contains(" Files "), "got:\n{text}");
    }

    #[test]
    fn pull_request_files_show_a_tree_beside_the_diff() {
        let mut app = app_with(pull(Some("@@ -1,2 +1,3 @@\n-old\n+new\n context")));
        let inspector = app.github_inspector.as_mut().unwrap();
        inspector.tab = GithubTab::Files;

        let text = render(&mut app, 100, 28);

        // Tree on the left, and the selected file's diff without pressing Enter.
        assert!(text.contains("lib.rs"), "got:\n{text}");
        assert!(text.contains("@@ -1,2 +1,3 @@"), "got:\n{text}");
        assert!(text.contains("+new"), "got:\n{text}");
    }

    #[test]
    fn omitted_patch_explains_why_diff_is_unavailable() {
        let mut app = app_with(pull(None));
        let inspector = app.github_inspector.as_mut().unwrap();
        inspector.tab = GithubTab::Files;

        let text = render(&mut app, 100, 20);

        assert!(text.contains("Diff unavailable"), "got:\n{text}");
    }

    #[test]
    fn narrow_files_tab_shows_only_the_focused_pane() {
        let mut app = app_with(pull(Some("@@ -1,2 +1,3 @@\n+new")));
        let inspector = app.github_inspector.as_mut().unwrap();
        inspector.tab = GithubTab::Files;

        // Too narrow to split: the tree owns the width.
        let tree_only = render(&mut app, 60, 20);
        assert!(tree_only.contains("lib.rs"), "got:\n{tree_only}");
        assert!(!tree_only.contains("@@"), "got:\n{tree_only}");

        // Focusing the diff hands the same width to the patch.
        app.github_inspector.as_mut().unwrap().files_pane = FilesPane::Diff;
        let diff_only = render(&mut app, 60, 20);
        assert!(diff_only.contains("@@"), "got:\n{diff_only}");
    }

    #[test]
    fn directory_rows_summarize_instead_of_showing_a_diff() {
        let mut item = pull(Some("@@ -1 +1 @@"));
        if let GithubItem::PullRequest(pull) = &mut item {
            pull.files.push(crate::github::ChangedFile {
                path: "src/other.rs".to_string(),
                status: "added".to_string(),
                additions: 7,
                deletions: 0,
                changes: 7,
                patch: Some("@@ -0,0 +1 @@".to_string()),
            });
        }
        let mut app = app_with(item);
        let inspector = app.github_inspector.as_mut().unwrap();
        inspector.tab = GithubTab::Files;
        // Row 0 is the `src` directory holding both files.
        inspector.tree_selected = 0;

        let text = render(&mut app, 100, 20);

        assert!(text.contains("2 changed files"), "got:\n{text}");
    }

    #[test]
    fn narrow_inspector_does_not_panic() {
        let mut app = app_with(issue());

        let text = render(&mut app, 24, 8);

        assert!(text.contains("GitHub"), "got:\n{text}");
    }

    #[test]
    fn tiny_files_tab_does_not_panic() {
        let mut app = app_with(pull(Some("@@ -1 +1 @@\n+x")));
        app.github_inspector.as_mut().unwrap().tab = GithubTab::Files;

        for (width, height) in [(20, 6), (30, 4), (76, 5), (200, 60)] {
            let text = render(&mut app, width, height);
            assert!(!text.is_empty());
        }
    }
}
