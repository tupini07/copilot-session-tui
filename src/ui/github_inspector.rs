use crate::app::{
    App, DiffRenderCache, FilesPane, GithubInspector, GithubInspectorScreen, GithubTab,
};
use crate::github::{DiscussionKind, GithubItem};
use crate::text;
use crate::theme::Theme;
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

/// Load an invented pull request into the inspector, so the documentation
/// screenshots can show the Files tab without contacting GitHub.
#[cfg(feature = "screenshots")]
pub fn install_demo_pull_request(app: &mut App) {
    use crate::github::{Author, ChangedFile, ItemCommon, Label, PullRequest, RepositoryRef};

    let file =
        |path: &str, status: &str, additions: u64, deletions: u64, patch: &str| ChangedFile {
            path: path.to_string(),
            status: status.to_string(),
            additions,
            deletions,
            changes: additions + deletions,
            patch: Some(patch.to_string()),
        };

    let item = GithubItem::PullRequest(PullRequest {
        common: ItemCommon {
            repository: RepositoryRef {
                host: "github.com".to_string(),
                owner: "tupini07".to_string(),
                name: "copilot-session-tui".to_string(),
            },
            number: 42,
            title: "Show PR changed files as a tree beside a live diff".to_string(),
            state: "open".to_string(),
            author: Author {
                login: "tupini07".to_string(),
            },
            labels: vec![Label {
                name: "enhancement".to_string(),
                color: Some("a2eeef".to_string()),
            }],
            created_at: "2026-08-19T18:04:00Z".to_string(),
            updated_at: "2026-08-19T21:37:00Z".to_string(),
            url: "https://github.com/tupini07/copilot-session-tui/pull/42".to_string(),
            body: "The flat list wasted most of the screen and hid the diff behind Enter."
                .to_string(),
        },
        draft: false,
        merged: false,
        mergeable_state: Some("clean".to_string()),
        base_ref: "main".to_string(),
        head_ref: "pr-files-tree".to_string(),
        additions: 704,
        deletions: 233,
        changed_files: 6,
        discussion: Vec::new(),
        files: vec![
            file(
                "src/ui/file_tree.rs",
                "added",
                306,
                0,
                "@@ -0,0 +1,42 @@\n+use std::collections::{BTreeMap, BTreeSet};\n+\n+/// One row of the changed-file tree, already flattened for display.\n+pub struct TreeRow {\n+    pub depth: usize,\n+    pub label: String,\n+    pub file: Option<usize>,\n+}\n+\n+/// Rows of the changed-file tree, in display order.\n+pub fn build_rows(\n+    files: &[ChangedFile],\n+    collapsed: &BTreeSet<String>,\n+) -> Vec<TreeRow> {\n+    let mut root = Dir::default();\n+    for (index, file) in files.iter().enumerate() {\n+        root.insert(&file.path, index, file);\n+    }\n+    // A directory with a single child adds a level of indent without adding\n+    // any information, so the two are folded into one row.\n+    root.collapse_single_child_chains();\n+    flatten(&root, collapsed)\n+}",
            ),
            file(
                "src/ui/github_inspector.rs",
                "modified",
                218,
                96,
                "@@ -196,12 +196,28 @@ fn draw_ready(f: &mut Frame, app: &mut App) {\n-    let lines = file_lines(inspector, item, width);\n-    render_list(f, lines, area);\n+    // A divider doubles as a focus cue, so it is obvious which pane takes keys.\n+    let (tree_area, diff_area) = if split {\n+        let tree_width = (area.width / 3).clamp(TREE_MIN_WIDTH, TREE_MAX_WIDTH);\n+        let chunks = Layout::default()\n+            .direction(Direction::Horizontal)\n+            .constraints([Constraint::Length(tree_width), Constraint::Min(1)])\n+            .split(area);\n+        (chunks[0], chunks[1])\n+    } else if focus == FilesPane::Diff {\n+        (Rect::default(), area)\n+    } else {\n+        (area, Rect::default())\n+    };",
            ),
            file(
                "src/ui/mod.rs",
                "modified",
                1,
                0,
                "@@ -1,3 +1,4 @@\n+pub mod file_tree;\n pub mod github_inspector;\n pub mod pane;",
            ),
            file(
                "src/mux_input.rs",
                "modified",
                164,
                31,
                "@@ -300,6 +300,14 @@ fn handle_ready_github_key(app: &mut App, key: KeyEvent) {\n+        KeyCode::Left if files_tab => collapse_or_ascend_github_tree(app),\n+        KeyCode::Right if files_tab => expand_or_enter_github_tree(app),",
            ),
            file(
                "src/app.rs",
                "modified",
                14,
                6,
                "@@ -240,6 +240,20 @@ impl GithubInspector {\n+    /// Put the tree cursor on the first file so the diff pane has something to\n+    /// show as soon as a pull request opens.\n+    pub fn select_first_tree_file(&mut self) {",
            ),
            file(
                "README.md",
                "modified",
                9,
                2,
                "@@ -221,8 +221,16 @@ GitHub inspection is available while attached to a mux session.\n+The **Files** tab splits into a directory tree of the changed files and the\n+selected file's diff, which follows the cursor without pressing Enter.",
            ),
        ],
        patches_loaded: true,
    });

    let mut inspector = GithubInspector::number_prompt();
    inspector.screen = GithubInspectorScreen::Ready(item);
    inspector.tab = GithubTab::Files;
    inspector.select_first_tree_file();
    app.github_inspector = Some(inspector);
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let theme = app.theme();
    let Some(inspector) = app.github_inspector.as_ref() else {
        return;
    };
    match &inspector.screen {
        GithubInspectorScreen::NumberPrompt => draw_prompt(f, inspector, theme),
        GithubInspectorScreen::Loading => draw_loading(f, inspector, theme),
        GithubInspectorScreen::Choose {
            issue_or_pull_request,
            discussion,
        } => draw_choice(f, issue_or_pull_request, discussion, theme),
        GithubInspectorScreen::Error(message) => draw_error(f, inspector, message, theme),
        GithubInspectorScreen::Ready(_) => draw_ready(f, app, theme),
    }
}

fn draw_choice(
    f: &mut Frame,
    issue_or_pull_request: &GithubItem,
    discussion: &GithubItem,
    theme: Theme,
) {
    let number = issue_or_pull_request.common().number;
    let issue_kind = if issue_or_pull_request.is_pull_request() {
        "pull request"
    } else {
        "issue"
    };
    draw_message_screen(
        f,
        " GitHub Inspector — Choose Item ",
        theme.warning,
        vec![
            Line::from(""),
            Line::from(Span::styled(
                format!("  GitHub #{number} identifies more than one item"),
                Style::default()
                    .fg(theme.warning)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(vec![
                Span::raw("  "),
                key("i", theme),
                Span::styled(
                    format!(" {issue_kind}: {}", issue_or_pull_request.common().title),
                    Style::default().fg(theme.text),
                ),
            ]),
            Line::from(vec![
                Span::raw("  "),
                key("d", theme),
                Span::styled(
                    format!(" discussion: {}", discussion.common().title),
                    Style::default().fg(theme.text),
                ),
            ]),
            Line::from(""),
            Line::from(vec![
                key("i", theme),
                Span::raw(format!(" open {issue_kind}   ")),
                key("d", theme),
                Span::raw(" open discussion   "),
                key("q", theme),
                Span::raw(" close"),
            ]),
        ],
        theme,
    );
}

fn draw_prompt(f: &mut Frame, inspector: &GithubInspector, theme: Theme) {
    let area = centered_rect(58, 9, f.area());
    f.render_widget(Clear, area);
    let block = Block::default()
        .title(" Inspect GitHub item ")
        .borders(Borders::ALL)
        .style(Style::default().fg(theme.text).bg(theme.surface))
        .border_style(Style::default().fg(theme.accent));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let error = inspector
        .prompt_error
        .as_deref()
        .map(|message| Span::styled(message, Style::default().fg(theme.error)))
        .unwrap_or_else(|| {
            Span::styled(
                "Number, `d 2291`, or a GitHub item URL",
                Style::default().fg(theme.muted),
            )
        });
    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(
                inspector.input.clone(),
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled("█", Style::default().fg(theme.accent)),
        ]),
        Line::from(""),
        Line::from(vec![Span::raw("  "), error]),
        Line::from(""),
        Line::from(vec![
            key("Enter", theme),
            Span::raw(" inspect   "),
            key("Esc", theme),
            Span::raw(" cancel"),
        ]),
    ];
    f.render_widget(
        Paragraph::new(lines).style(Style::default().fg(theme.text).bg(theme.surface)),
        inner,
    );
}

fn draw_loading(f: &mut Frame, inspector: &GithubInspector, theme: Theme) {
    let number = inspector.number.unwrap_or_default();
    let frame = super::spinner_frame();
    draw_message_screen(
        f,
        " GitHub Inspector ",
        theme.accent,
        vec![
            Line::from(""),
            Line::from(Span::styled(
                format!("  {frame}  Loading GitHub item #{number}..."),
                Style::default().fg(theme.info).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  The attached session remains live while gh fetches the item.",
                Style::default().fg(theme.muted),
            )),
            Line::from(""),
            Line::from(vec![key("q", theme), Span::raw(" close")]),
        ],
        theme,
    );
}

fn draw_error(f: &mut Frame, inspector: &GithubInspector, message: &str, theme: Theme) {
    let number = inspector.number.unwrap_or_default();
    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  Could not load GitHub item #{number}"),
            Style::default()
                .fg(theme.error)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    for line in text::wrap_text(message, f.area().width.saturating_sub(8) as usize) {
        lines.push(Line::from(format!("  {line}")));
    }
    lines.extend([
        Line::from(""),
        Line::from(vec![
            key("r", theme),
            Span::raw(" retry   "),
            key("q", theme),
            Span::raw(" close"),
        ]),
    ]);
    draw_message_screen(f, " GitHub Inspector — Error ", theme.error, lines, theme);
}

fn draw_message_screen(
    f: &mut Frame,
    title: &str,
    color: Color,
    lines: Vec<Line<'static>>,
    theme: Theme,
) {
    let area = f.area();
    f.render_widget(Clear, area);
    let block = Block::default()
        .title(title.to_string())
        .borders(Borders::ALL)
        .style(Style::default().fg(theme.text).bg(theme.background))
        .border_style(Style::default().fg(color));
    f.render_widget(
        Paragraph::new(lines)
            .style(Style::default().fg(theme.text).bg(theme.background))
            .block(block),
        area,
    );
}

fn draw_ready(f: &mut Frame, app: &mut App, theme: Theme) {
    let area = f.area();
    f.render_widget(Clear, area);
    let outer = Block::default()
        .title(" GitHub Inspector ")
        .borders(Borders::ALL)
        .style(Style::default().fg(theme.text).bg(theme.background))
        .border_style(Style::default().fg(theme.accent));
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
    draw_header(f, item, sections[0], theme);
    draw_tabs(f, inspector, item, sections[1], theme);

    if inspector.tab == GithubTab::Files && item.is_pull_request() {
        draw_files_tab(f, app, sections[2], theme);
        let inspector = app.github_inspector.as_ref().expect("inspector is active");
        draw_footer(f, inspector, sections[3], theme);
        return;
    }

    let content = sections[2];
    let viewport_width = content.width.saturating_sub(1) as usize;
    let viewport_height = content.height as usize;
    let lines = build_active_lines(inspector, item, viewport_width, theme);
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
    let paragraph = Paragraph::new(lines)
        .style(Style::default().fg(theme.text).bg(theme.background))
        .scroll((actual_offset.min(u16::MAX as usize) as u16, 0));
    f.render_widget(paragraph, content);
    draw_scrollbar(
        f,
        content,
        line_count,
        viewport_height,
        actual_offset,
        theme,
    );

    let inspector = app.github_inspector.as_ref().expect("inspector is active");
    draw_footer(f, inspector, sections[3], theme);
}

/// Minimum content width before the changed-file tree and the diff can usefully
/// share a row; below it only the focused pane is shown.
const SPLIT_MIN_WIDTH: u16 = 76;
const TREE_MIN_WIDTH: u16 = 26;
const TREE_MAX_WIDTH: u16 = 52;

/// Draw the tree of changed files beside the selected file's diff.
fn draw_files_tab(f: &mut Frame, app: &mut App, area: Rect, theme: Theme) {
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
                theme.accent
            } else {
                theme.inactive
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
        theme,
    );
    let diff_width = diff_area.width.saturating_sub(1) as usize;
    let PreparedDiff {
        local_lines,
        updated_cache,
        line_count: diff_line_count,
        max_width: diff_max_width,
    } = prepare_diff(inspector, item, &rows, theme);
    let diff_height = diff_area.height as usize;
    let max_diff_scroll = diff_line_count.saturating_sub(diff_height);
    let max_diff_horizontal = diff_max_width.saturating_sub(diff_width);

    if let Some(inspector) = app.github_inspector.as_mut() {
        inspector.tree_area = tree_body;
        inspector.diff_area = diff_area;
        inspector.visible_tree_rows = tree_body.height as usize;
        inspector.max_diff_scroll = max_diff_scroll;
        inspector.max_diff_horizontal = max_diff_horizontal;
        if let Some(cache) = updated_cache {
            inspector.diff_render_cache = Some(cache);
        }
        inspector.diff_scroll = inspector.diff_scroll.min(max_diff_scroll);
        inspector.diff_horizontal = inspector.diff_horizontal.min(max_diff_horizontal);
        inspector.tree_selected = inspector.tree_selected.min(rows.len().saturating_sub(1));
        clamp_tree_offset(inspector, rows.len());
    }

    let inspector = app.github_inspector.as_ref().expect("inspector is active");
    if tree_body.width > 0 && tree_body.height > 0 {
        f.render_widget(
            Paragraph::new(tree_lines)
                .style(Style::default().fg(theme.text).bg(theme.background))
                .scroll((inspector.tree_offset.min(u16::MAX as usize) as u16, 0)),
            tree_body,
        );
        draw_scrollbar(
            f,
            tree_body,
            rows.len(),
            tree_body.height as usize,
            inspector.tree_offset,
            theme,
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
        let full_diff = local_lines.as_deref().unwrap_or_else(|| {
            inspector
                .diff_render_cache
                .as_ref()
                .map(|cache| cache.lines.as_slice())
                .unwrap_or(&[])
        });
        let visible_diff = super::diff::viewport_lines(
            full_diff,
            inspector.diff_scroll,
            diff_height,
            inspector.diff_horizontal,
            diff_body.width as usize,
            theme,
        );
        f.render_widget(
            Paragraph::new(visible_diff)
                .style(Style::default().fg(theme.text).bg(theme.diff_context_bg)),
            diff_body,
        );
        draw_scrollbar(
            f,
            diff_area,
            max_diff_scroll + diff_height,
            diff_height,
            inspector.diff_scroll,
            theme,
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
    theme: Theme,
) -> Vec<Line<'static>> {
    if rows.is_empty() {
        return vec![Line::from(Span::styled(
            " No changed files",
            Style::default().fg(theme.muted),
        ))];
    }
    rows.iter()
        .enumerate()
        .map(|(index, row)| {
            tree_row_line(
                row,
                files,
                index == inspector.tree_selected,
                focus,
                width,
                theme,
            )
        })
        .collect()
}

/// Single-letter status marker, so the path keeps as much width as possible.
fn status_marker(status: &str, theme: Theme) -> (&'static str, Color) {
    match status {
        "added" => ("A", theme.success),
        "removed" => ("D", theme.error),
        "modified" => ("M", theme.warning),
        "renamed" => ("R", theme.accent),
        _ => ("·", theme.muted),
    }
}

fn tree_row_line(
    row: &file_tree::TreeRow,
    files: &[crate::github::ChangedFile],
    selected: bool,
    focus: FilesPane,
    width: usize,
    theme: Theme,
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
            theme.directory,
            theme.directory,
            format!("{count} · +{additions} -{deletions}"),
        ),
        file_tree::RowKind::File { index } => {
            let file = files.get(*index);
            let (marker, color) = file
                .map(|file| status_marker(&file.status, theme))
                .unwrap_or(("·", theme.muted));
            let stats = file
                .map(|file| format!("+{} -{}", file.additions, file.deletions))
                .unwrap_or_default();
            (marker.to_string(), color, theme.text, stats)
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
            Style::default()
                .fg(theme.selection_fg)
                .bg(theme.selection_bg)
        } else {
            Style::default().fg(theme.text).bg(theme.chrome_bg)
        }
        .add_modifier(Modifier::BOLD);
        (highlight, highlight, highlight)
    } else {
        (
            Style::default().fg(marker_color),
            Style::default().fg(name_color),
            Style::default().fg(theme.muted),
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
struct PreparedDiff {
    local_lines: Option<Vec<Line<'static>>>,
    updated_cache: Option<DiffRenderCache>,
    line_count: usize,
    max_width: usize,
}

fn prepare_diff(
    inspector: &GithubInspector,
    item: &GithubItem,
    rows: &[file_tree::TreeRow],
    theme: Theme,
) -> PreparedDiff {
    match rows.get(inspector.tree_selected).map(|row| &row.kind) {
        Some(file_tree::RowKind::Directory {
            path,
            files,
            additions,
            deletions,
            ..
        }) => prepared_local(vec![
            Line::from(Span::styled(
                format!(" {path}/ "),
                Style::default()
                    .fg(theme.selection_fg)
                    .bg(theme.directory)
                    .add_modifier(Modifier::BOLD),
            )),
            summary_line(
                "Directory",
                format!("{files} changed files · +{additions} -{deletions}"),
                theme,
            ),
            Line::from(""),
            Line::from(Span::styled(
                "Select a file to see its diff.",
                Style::default().fg(theme.muted),
            )),
        ]),
        _ => {
            if item.files().get(inspector.selected_file).is_none() {
                return prepared_local(vec![Line::from(Span::styled(
                    "Select a file to see its diff.",
                    Style::default().fg(theme.muted),
                ))]);
            }
            if let Some(cache) = inspector
                .diff_render_cache
                .as_ref()
                .filter(|cache| cache.file_index == inspector.selected_file)
            {
                return PreparedDiff {
                    local_lines: None,
                    updated_cache: None,
                    line_count: cache.line_count,
                    max_width: cache.max_width,
                };
            }
            let lines = diff_lines(inspector, item, theme);
            let line_count = lines.len();
            let max_width = lines.iter().map(line_width).max().unwrap_or_default();
            let cache = DiffRenderCache {
                file_index: inspector.selected_file,
                line_count,
                max_width,
                lines,
            };
            PreparedDiff {
                local_lines: None,
                updated_cache: Some(cache),
                line_count,
                max_width,
            }
        }
    }
}

fn prepared_local(lines: Vec<Line<'static>>) -> PreparedDiff {
    PreparedDiff {
        line_count: lines.len(),
        max_width: lines.iter().map(line_width).max().unwrap_or_default(),
        local_lines: Some(lines),
        updated_cache: None,
    }
}

/// Like `field`, but flush left: the diff pane already has a one-column gutter.
fn summary_line(label: &str, value: String, theme: Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label}: "),
            Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(value, Style::default().fg(theme.text)),
    ])
}

fn draw_header(f: &mut Frame, item: &GithubItem, area: Rect, theme: Theme) {
    let common = item.common();
    let kind = if item.is_pull_request() {
        "PR"
    } else if item.is_discussion() {
        "Discussion"
    } else {
        "Issue"
    };
    let state = semantic_item_state(item);
    let state_color = match state.as_str() {
        "open" => theme.success,
        "closed" if item.is_pull_request() => theme.error,
        "draft" => theme.muted,
        _ => theme.accent,
    };
    let line1 = Line::from(vec![
        Span::styled(
            format!(
                " {}  {kind} #{} ",
                common.repository.name_with_owner(),
                common.number
            ),
            Style::default()
                .fg(theme.selection_fg)
                .bg(theme.info)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            state.to_ascii_uppercase(),
            Style::default()
                .fg(state_color)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    let line2 = Line::from(Span::styled(
        format!(" {}", common.title),
        Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
    ));
    f.render_widget(
        Paragraph::new(vec![line1, line2])
            .style(Style::default().fg(theme.text).bg(theme.background)),
        area,
    );
}

fn semantic_item_state(item: &GithubItem) -> String {
    match item {
        GithubItem::PullRequest(pull) if pull.merged => "merged".to_string(),
        GithubItem::PullRequest(pull) if pull.draft => "draft".to_string(),
        _ => item.common().state.to_ascii_lowercase(),
    }
}

fn draw_tabs(
    f: &mut Frame,
    inspector: &GithubInspector,
    item: &GithubItem,
    area: Rect,
    theme: Theme,
) {
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
                .fg(theme.selection_fg)
                .bg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.inactive)
        };
        spans.push(Span::styled(name, style));
        spans.push(Span::raw(" "));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans))
            .style(Style::default().fg(theme.text).bg(theme.chrome_bg)),
        area,
    );
}

fn build_active_lines(
    inspector: &GithubInspector,
    item: &GithubItem,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    match inspector.tab {
        GithubTab::Overview => overview_lines(item, width, theme),
        GithubTab::Comments => comment_lines(item, width, theme),
        // The Files tab draws its own split panes; an item without files has none.
        GithubTab::Files => vec![Line::from(Span::styled(
            " No changed files",
            Style::default().fg(theme.muted),
        ))],
    }
}

fn overview_lines(item: &GithubItem, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let common = item.common();
    let mut lines = vec![
        field("Author", format!("@{}", common.author.login), theme),
        field("Created", common.created_at.clone(), theme),
        field("Updated", common.updated_at.clone(), theme),
        field("URL", common.url.clone(), theme),
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
            theme,
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
            field("PR state", state.to_string(), theme),
            field(
                "Branches",
                format!("{} <- {}", pull.base_ref, pull.head_ref),
                theme,
            ),
            field(
                "Changes",
                format!(
                    "+{} -{} across {} files",
                    pull.additions, pull.deletions, pull.changed_files
                ),
                theme,
            ),
        ]);
        if let Some(mergeable) = &pull.mergeable_state {
            lines.push(field("Mergeability", mergeable.clone(), theme));
        }
    } else if let GithubItem::Discussion(discussion) = item {
        lines.extend([
            field("Category", discussion.category.clone(), theme),
            field(
                "Answer",
                if !discussion.answerable {
                    "not available for this category".to_string()
                } else if discussion.answered {
                    discussion
                        .answer_chosen_at
                        .as_ref()
                        .map(|date| format!("accepted · {date}"))
                        .unwrap_or_else(|| "accepted".to_string())
                } else {
                    "not answered".to_string()
                },
                theme,
            ),
            field("Upvotes", discussion.upvote_count.to_string(), theme),
        ]);
        if let Some(reactions) = reactions_text(&discussion.reactions) {
            lines.push(field("Reactions", reactions, theme));
        }
    }
    lines.push(Line::from(""));
    lines.push(section("Description", theme));
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

fn comment_lines(item: &GithubItem, width: usize, theme: Theme) -> Vec<Line<'static>> {
    if let GithubItem::Discussion(discussion) = item {
        return discussion_comment_lines(discussion, width, theme);
    }
    let entries = item.discussion();
    if entries.is_empty() {
        return vec![Line::from(Span::styled(
            " No comments or reviews",
            Style::default().fg(theme.muted),
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
            Style::default().fg(theme.info).add_modifier(Modifier::BOLD),
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
            Style::default().fg(theme.inactive),
        )));
    }
    lines
}

fn discussion_comment_lines(
    discussion: &crate::github::RepositoryDiscussion,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    if discussion.comments.is_empty() {
        return vec![Line::from(Span::styled(
            " No comments",
            Style::default().fg(theme.muted),
        ))];
    }
    let mut lines = Vec::new();
    for comment in &discussion.comments {
        push_discussion_comment(&mut lines, comment, 0, width, theme);
        lines.push(Line::from(Span::styled(
            "─".repeat(width.max(1)),
            Style::default().fg(theme.inactive),
        )));
    }
    lines
}

fn push_discussion_comment(
    lines: &mut Vec<Line<'static>>,
    comment: &crate::github::DiscussionComment,
    depth: usize,
    width: usize,
    theme: Theme,
) {
    let indent = "  ".repeat(depth + 1);
    let answer = if comment.is_answer {
        " · ACCEPTED ANSWER"
    } else {
        ""
    };
    let reactions = reactions_text(&comment.reactions)
        .map(|value| format!(" · {value}"))
        .unwrap_or_default();
    lines.push(Line::from(Span::styled(
        format!(
            "{indent}@{} · {} · ▲ {}{reactions}{answer}",
            comment.author.login, comment.created_at, comment.upvote_count
        ),
        Style::default()
            .fg(if comment.is_answer {
                theme.success
            } else if depth > 0 {
                theme.accent_alt
            } else {
                theme.info
            })
            .add_modifier(Modifier::BOLD),
    )));
    let body = if comment.body.trim().is_empty() {
        "(no comment body)"
    } else {
        comment.body.as_str()
    };
    let body_indent = "  ".repeat(depth + 2);
    lines.extend(
        text::wrap_text(
            body,
            width
                .saturating_sub(text::display_width(&body_indent))
                .max(1),
        )
        .into_iter()
        .map(|line| Line::from(format!("{body_indent}{line}"))),
    );
    for reply in &comment.replies {
        push_discussion_comment(lines, reply, depth + 1, width, theme);
    }
}

fn reactions_text(reactions: &[crate::github::ReactionCount]) -> Option<String> {
    (!reactions.is_empty()).then(|| {
        reactions
            .iter()
            .map(|reaction| format!("{} {}", reaction.content, reaction.count))
            .collect::<Vec<_>>()
            .join(" · ")
    })
}

fn diff_lines(inspector: &GithubInspector, item: &GithubItem, theme: Theme) -> Vec<Line<'static>> {
    let Some(file) = item.files().get(inspector.selected_file) else {
        return vec![Line::from(Span::styled(
            "Select a file to see its diff.",
            Style::default().fg(theme.muted),
        ))];
    };
    let mut lines = vec![
        Line::from(Span::styled(
            format!(" {} ", file.path),
            Style::default()
                .fg(theme.selection_fg)
                .bg(theme.info)
                .add_modifier(Modifier::BOLD),
        )),
        summary_line(
            "File",
            format!(
                "{} · +{} -{} · {} changed lines",
                file.status, file.additions, file.deletions, file.changes
            ),
            theme,
        ),
        Line::from(""),
    ];
    let Some(patch) = &file.patch else {
        let pending = matches!(item, GithubItem::PullRequest(pull) if !pull.patches_loaded);
        lines.push(Line::from(if pending {
            Span::styled(
                "Loading diff…",
                Style::default().fg(theme.info).add_modifier(Modifier::DIM),
            )
        } else {
            Span::styled(
                "Diff unavailable: GitHub omitted the patch (the file may be binary or too large).",
                Style::default().fg(theme.warning),
            )
        }));
        return lines;
    };
    lines.extend(super::diff::render_patch(&file.path, patch, theme));
    lines
}

fn draw_footer(f: &mut Frame, inspector: &GithubInspector, area: Rect, theme: Theme) {
    let mut spans = vec![
        Span::raw(" "),
        key("Tab/Shift+Tab", theme),
        Span::raw(" tabs  "),
    ];
    if inspector.tab == GithubTab::Files {
        if inspector.files_pane == FilesPane::Diff {
            spans.extend([
                key("↑↓/PgUp/PgDn", theme),
                Span::raw(" scroll  "),
                key("←→", theme),
                Span::raw(" horizontal  "),
                key("Esc", theme),
                Span::raw(" files  "),
                key("q", theme),
                Span::raw(" close"),
            ]);
        } else {
            spans.extend([
                key("↑↓", theme),
                Span::raw(" select  "),
                key("←→", theme),
                Span::raw(" fold  "),
                key("Enter", theme),
                Span::raw(" diff  "),
                key("q", theme),
                Span::raw(" close"),
            ]);
        }
    } else {
        spans.extend([
            key("↑↓/PgUp/PgDn", theme),
            Span::raw(" navigate  "),
            key("wheel", theme),
            Span::raw(" scroll  "),
            key("q", theme),
            Span::raw(" close"),
        ]);
    }
    f.render_widget(
        Paragraph::new(Line::from(spans))
            .style(Style::default().fg(theme.text).bg(theme.chrome_bg)),
        area,
    );
}

fn draw_scrollbar(
    f: &mut Frame,
    area: Rect,
    line_count: usize,
    viewport_height: usize,
    offset: usize,
    theme: Theme,
) {
    if viewport_height == 0 || line_count <= viewport_height {
        return;
    }
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None)
        .track_symbol(Some("│"))
        .track_style(Style::default().fg(theme.inactive))
        .thumb_symbol("█")
        .thumb_style(Style::default().fg(theme.accent));
    // With an explicit viewport length Ratatui expects the number of possible
    // positions, not the total line count. Passing `line_count` makes the thumb stop
    // early even after the text has reached its real maximum offset.
    let positions = line_count.saturating_sub(viewport_height).saturating_add(1);
    let mut state = ScrollbarState::new(positions)
        .position(offset)
        .viewport_content_length(viewport_height);
    f.render_stateful_widget(scrollbar, area, &mut state);
}

fn field(label: &str, value: String, theme: Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!(" {label}: "),
            Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(value, Style::default().fg(theme.text)),
    ])
}

fn section(title: &str, theme: Theme) -> Line<'static> {
    Line::from(Span::styled(
        format!(" -- {title} --"),
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    ))
}

fn key(label: &str, theme: Theme) -> Span<'static> {
    Span::styled(
        label.to_string(),
        Style::default()
            .fg(theme.accent_alt)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::GithubInspector;
    use crate::config::UserConfig;
    use crate::github::{
        Author, ChangedFile, DiscussionComment, DiscussionEntry, Issue, ItemCommon, Label,
        PullRequest, ReactionCount, RepositoryDiscussion, RepositoryRef,
    };
    use crate::theme::ThemeName;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    const TEST_THEMES: [ThemeName; 3] = [
        ThemeName::Nord,
        ThemeName::CatppuccinLatte,
        ThemeName::SolarizedLight,
    ];

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

    fn rendered_scrollbar(line_count: usize, viewport: usize, offset: usize) -> Vec<String> {
        let backend = TestBackend::new(2, viewport as u16);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                draw_scrollbar(
                    frame,
                    Rect::new(0, 0, 2, viewport as u16),
                    line_count,
                    viewport,
                    offset,
                    ThemeName::Classic.theme(),
                );
            })
            .unwrap();
        (0..viewport as u16)
            .map(|row| {
                terminal
                    .backend()
                    .buffer()
                    .cell((1, row))
                    .unwrap()
                    .symbol()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn scrollbar_thumb_reaches_the_end_at_the_real_maximum_offset() {
        let at_top = rendered_scrollbar(20, 10, 0);
        let at_bottom = rendered_scrollbar(20, 10, 10);

        assert_eq!(at_top.first().map(String::as_str), Some("█"));
        assert_eq!(at_top.last().map(String::as_str), Some("│"));
        assert_eq!(at_bottom.first().map(String::as_str), Some("│"));
        assert_eq!(at_bottom.last().map(String::as_str), Some("█"));
    }

    #[test]
    fn content_that_fits_has_no_scrollbar() {
        assert!(rendered_scrollbar(10, 10, 0)
            .iter()
            .all(|symbol| symbol == " "));
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

    fn discussion() -> GithubItem {
        let mut common = common(2291);
        common.title = "Coordinate rendering agents".to_string();
        common.url = "https://github.com/octo/widgets/discussions/2291".to_string();
        GithubItem::Discussion(RepositoryDiscussion {
            common,
            category: "Agents".to_string(),
            answerable: true,
            answered: true,
            answer_chosen_at: Some("2026-01-04T00:00:00Z".to_string()),
            upvote_count: 7,
            reactions: vec![ReactionCount {
                content: "HEART".to_string(),
                count: 3,
            }],
            comments: vec![DiscussionComment {
                author: Author {
                    login: "answer-agent".to_string(),
                },
                body: "Use the merged schema.".to_string(),
                created_at: "2026-01-03T00:00:00Z".to_string(),
                upvote_count: 4,
                reactions: Vec::new(),
                is_answer: true,
                replies: vec![DiscussionComment {
                    author: Author {
                        login: "reply-agent".to_string(),
                    },
                    body: "Acknowledged.".to_string(),
                    created_at: "2026-01-03T01:00:00Z".to_string(),
                    upvote_count: 1,
                    reactions: Vec::new(),
                    is_answer: false,
                    replies: Vec::new(),
                }],
            }],
        })
    }

    #[test]
    fn discussion_overview_and_comments_show_discussion_metadata_and_threads() {
        let item = discussion();
        let overview = overview_lines(&item, 100, ThemeName::Nord.theme())
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let comments = comment_lines(&item, 100, ThemeName::Nord.theme())
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(overview.contains("Category: Agents"), "got:\n{overview}");
        assert!(overview.contains("Answer: accepted"), "got:\n{overview}");
        assert!(overview.contains("Upvotes: 7"), "got:\n{overview}");
        assert!(overview.contains("HEART 3"), "got:\n{overview}");
        assert!(comments.contains("ACCEPTED ANSWER"), "got:\n{comments}");
        assert!(comments.contains("@reply-agent"), "got:\n{comments}");
        assert!(comments.contains("Acknowledged."), "got:\n{comments}");
    }

    #[test]
    fn collision_screen_names_both_items() {
        let mut app = App::new(Vec::new(), UserConfig::default());
        app.github_inspector = Some(GithubInspector::number_prompt());
        app.github_inspector.as_mut().unwrap().screen = GithubInspectorScreen::Choose {
            issue_or_pull_request: Box::new(issue()),
            discussion: Box::new(discussion()),
        };
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();

        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(
            text.contains("identifies more than one item"),
            "got:\n{text}"
        );
        assert!(
            text.contains("discussion: Coordinate rendering agents"),
            "got:\n{text}"
        );
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
            patches_loaded: true,
        })
    }

    fn header_text(item: &GithubItem) -> String {
        let mut terminal = Terminal::new(TestBackend::new(100, 2)).unwrap();
        terminal
            .draw(|frame| draw_header(frame, item, frame.area(), ThemeName::Classic.theme()))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn pull_request_header_distinguishes_merged_from_closed() {
        let mut merged = pull(None);
        let GithubItem::PullRequest(pull) = &mut merged else {
            unreachable!()
        };
        pull.common.state = "closed".to_string();
        pull.merged = true;
        assert!(header_text(&merged).contains("MERGED"));
        assert!(!header_text(&merged).contains("CLOSED"));

        let mut closed = merged.clone();
        let GithubItem::PullRequest(pull) = &mut closed else {
            unreachable!()
        };
        pull.merged = false;
        assert!(header_text(&closed).contains("CLOSED"));
        assert!(!header_text(&closed).contains("MERGED"));
    }

    fn app_with(item: GithubItem) -> App {
        app_with_theme(item, ThemeName::Classic)
    }

    fn app_with_theme(item: GithubItem, theme: ThemeName) -> App {
        let mut app = App::new(
            Vec::new(),
            UserConfig {
                theme,
                ..Default::default()
            },
        );
        let mut inspector = GithubInspector::number_prompt();
        inspector.screen = GithubInspectorScreen::Ready(item);
        inspector.select_first_tree_file();
        app.github_inspector = Some(inspector);
        app
    }

    #[test]
    fn inspector_selections_follow_dark_and_light_themes() {
        let item = pull(Some("@@ -1 +1 @@\n-old\n+new"));
        let rows = file_tree::build_rows(item.files(), &Default::default());
        let row = rows
            .iter()
            .find(|row| matches!(&row.kind, file_tree::RowKind::File { .. }))
            .expect("tree contains the changed file");

        for name in TEST_THEMES {
            let theme = name.theme();
            let active = tree_row_line(row, item.files(), true, FilesPane::Tree, 32, theme);
            assert!(active.spans.iter().all(|span| {
                span.style.fg == Some(theme.selection_fg)
                    && span.style.bg == Some(theme.selection_bg)
            }));

            let inactive = tree_row_line(row, item.files(), true, FilesPane::Diff, 32, theme);
            assert!(inactive.spans.iter().all(|span| {
                span.style.fg == Some(theme.text) && span.style.bg == Some(theme.chrome_bg)
            }));

            let inspector = GithubInspector::number_prompt();
            let backend = TestBackend::new(40, 1);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| {
                    draw_tabs(frame, &inspector, &item, Rect::new(0, 0, 40, 1), theme);
                })
                .unwrap();
            let selected_tab = terminal.backend().buffer().cell((1, 0)).unwrap();
            assert_eq!(selected_tab.bg, theme.accent, "{name:?}");
            assert_eq!(selected_tab.fg, theme.selection_fg, "{name:?}");
        }
    }

    #[test]
    fn classic_inspector_keeps_existing_status_and_selection_colours() {
        let theme = ThemeName::Classic.theme();
        assert_eq!(status_marker("added", theme), ("A", Color::Green));
        assert_eq!(status_marker("removed", theme), ("D", Color::Red));
        assert_eq!(status_marker("modified", theme), ("M", Color::Yellow));
        assert_eq!(status_marker("renamed", theme), ("R", Color::Magenta));

        let item = pull(Some("@@ -1 +1 @@\n-old\n+new"));
        let rows = file_tree::build_rows(item.files(), &Default::default());
        let row = rows
            .iter()
            .find(|row| matches!(&row.kind, file_tree::RowKind::File { .. }))
            .unwrap();
        let selected = tree_row_line(row, item.files(), true, FilesPane::Tree, 32, theme);
        assert!(selected.spans.iter().all(|span| {
            span.style.fg == Some(Color::Black) && span.style.bg == Some(Color::Cyan)
        }));

        let inspector = GithubInspector::number_prompt();
        let backend = TestBackend::new(40, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                draw_tabs(frame, &inspector, &item, Rect::new(0, 0, 40, 1), theme);
            })
            .unwrap();
        let selected_tab = terminal.backend().buffer().cell((1, 0)).unwrap();
        assert_eq!(selected_tab.fg, Color::Black);
        assert_eq!(selected_tab.bg, Color::Magenta);
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
        assert!(text.contains("+│ new"), "got:\n{text}");
        assert!(text.contains("1    -│ old"), "got:\n{text}");
    }

    #[test]
    fn highlighted_diff_cache_survives_repaints_and_resizes() {
        let mut app = app_with(pull(Some("@@ -1 +1 @@\n-let old = 1;\n+let new = 2;")));
        app.github_inspector.as_mut().unwrap().tab = GithubTab::Files;

        render(&mut app, 100, 28);
        let first = app
            .github_inspector
            .as_ref()
            .unwrap()
            .diff_render_cache
            .clone()
            .expect("first draw populates the cache");
        render(&mut app, 100, 28);
        assert_eq!(
            app.github_inspector.as_ref().unwrap().diff_render_cache,
            Some(first.clone()),
            "an unchanged repaint reuses the rendered lines"
        );

        render(&mut app, 120, 28);
        assert_eq!(
            app.github_inspector.as_ref().unwrap().diff_render_cache,
            Some(first),
            "horizontal viewport cropping does not require re-highlighting"
        );
    }

    #[test]
    fn theme_preview_invalidation_rebuilds_the_diff_cache() {
        let mut app = app_with(pull(Some("@@ -1 +1 @@\n-let old = 1;\n+let new = 2;")));
        app.github_inspector.as_mut().unwrap().tab = GithubTab::Files;

        render(&mut app, 100, 28);
        let classic = app
            .github_inspector
            .as_ref()
            .unwrap()
            .diff_render_cache
            .clone()
            .expect("classic draw populates the cache");

        app.open_theme_picker();
        app.move_theme_picker(2);
        assert_eq!(app.theme_name(), ThemeName::Nord);
        assert!(
            app.github_inspector
                .as_ref()
                .unwrap()
                .diff_render_cache
                .is_none(),
            "theme preview invalidates theme-dependent highlighted lines"
        );

        render(&mut app, 100, 28);
        let nord = app
            .github_inspector
            .as_ref()
            .unwrap()
            .diff_render_cache
            .clone()
            .expect("preview redraw repopulates the cache");
        assert_ne!(classic, nord);
        assert!(nord.lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.style.bg == Some(ThemeName::Nord.theme().diff_add_bg))
        }));
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
