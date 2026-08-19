use crate::app::{App, GithubInspector, GithubInspectorScreen, GithubTab};
use crate::github::{DiscussionKind, GithubItem};
use crate::text;
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

    let content = sections[2];
    let viewport_width = content.width.saturating_sub(1) as usize;
    let viewport_height = content.height as usize;
    let (lines, _offset, horizontal, is_diff) = build_active_lines(inspector, item, viewport_width);
    let line_count = lines.len();
    let max_scroll = line_count.saturating_sub(viewport_height);
    let max_horizontal = if is_diff {
        lines
            .iter()
            .map(line_width)
            .max()
            .unwrap_or_default()
            .saturating_sub(viewport_width)
    } else {
        0
    };

    if let Some(inspector) = app.github_inspector.as_mut() {
        if inspector.diff_open {
            inspector.max_diff_scroll = max_scroll;
            inspector.max_diff_horizontal = max_horizontal;
            inspector.diff_scroll = inspector.diff_scroll.min(max_scroll);
            inspector.diff_horizontal = inspector.diff_horizontal.min(max_horizontal);
        } else {
            inspector.max_scroll = max_scroll;
            let tab = inspector.tab.index();
            inspector.scroll_offsets[tab] = inspector.scroll_offsets[tab].min(max_scroll);
            if inspector.tab == GithubTab::Files {
                let file_count = inspector
                    .ready_item()
                    .map(|item| item.files().len())
                    .unwrap_or(0);
                inspector.visible_files = viewport_height;
                inspector.selected_file = inspector.selected_file.min(file_count.saturating_sub(1));
                let max_offset = file_count.saturating_sub(viewport_height.max(1));
                inspector.file_list_offset = inspector.file_list_offset.min(max_offset);
            }
        }
    }

    let actual_offset = app
        .github_inspector
        .as_ref()
        .map(|inspector| {
            if is_diff {
                inspector.diff_scroll
            } else if inspector.tab == GithubTab::Files {
                inspector.file_list_offset
            } else {
                inspector.active_scroll()
            }
        })
        .unwrap_or_default();
    let actual_horizontal = if is_diff {
        app.github_inspector
            .as_ref()
            .map(|inspector| inspector.diff_horizontal)
            .unwrap_or_default()
    } else {
        horizontal
    };
    let paragraph = Paragraph::new(lines).scroll((
        actual_offset.min(u16::MAX as usize) as u16,
        actual_horizontal.min(u16::MAX as usize) as u16,
    ));
    f.render_widget(paragraph, content);
    draw_scrollbar(f, content, line_count, viewport_height, actual_offset);

    let inspector = app.github_inspector.as_ref().expect("inspector is active");
    draw_footer(f, inspector, sections[3]);
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
) -> (Vec<Line<'static>>, usize, usize, bool) {
    if inspector.diff_open {
        return (
            diff_lines(inspector, item),
            inspector.diff_scroll,
            inspector.diff_horizontal,
            true,
        );
    }
    match inspector.tab {
        GithubTab::Overview => (
            overview_lines(item, width),
            inspector.active_scroll(),
            0,
            false,
        ),
        GithubTab::Comments => (
            comment_lines(item, width),
            inspector.active_scroll(),
            0,
            false,
        ),
        GithubTab::Files => (
            file_lines(inspector, item),
            inspector.file_list_offset,
            0,
            false,
        ),
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

fn file_lines(inspector: &GithubInspector, item: &GithubItem) -> Vec<Line<'static>> {
    let files = item.files();
    if files.is_empty() {
        return vec![Line::from(Span::styled(
            " No changed files",
            Style::default().fg(Color::DarkGray),
        ))];
    }
    files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            let style = if index == inspector.selected_file {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            Line::from(Span::styled(
                format!(
                    " {:<9}  +{:<5} -{:<5} {}",
                    file.status, file.additions, file.deletions, file.path
                ),
                style,
            ))
        })
        .collect()
}

fn diff_lines(inspector: &GithubInspector, item: &GithubItem) -> Vec<Line<'static>> {
    let Some(file) = item.files().get(inspector.selected_file) else {
        return vec![Line::from(" No file selected")];
    };
    let mut lines = vec![
        Line::from(Span::styled(
            format!(" {} ", file.path),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        field(
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
            " Diff unavailable: GitHub omitted the patch (the file may be binary or too large).",
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
    let mut spans = vec![
        Span::raw(" "),
        key("Tab/Shift+Tab"),
        Span::raw(" tabs  "),
        key("↑↓/PgUp/PgDn"),
        Span::raw(" navigate  "),
        key("wheel"),
        Span::raw(" scroll  "),
    ];
    if inspector.diff_open {
        spans.extend([
            key("←→"),
            Span::raw(" horizontal  "),
            key("Esc"),
            Span::raw(" files"),
        ]);
    } else if inspector.tab == GithubTab::Files {
        spans.extend([
            key("Enter"),
            Span::raw(" diff  "),
            key("Esc"),
            Span::raw(" close"),
        ]);
    } else {
        spans.extend([key("Esc"), Span::raw(" close")]);
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
    fn pull_request_diff_renders_unified_patch() {
        let mut app = app_with(pull(Some("@@ -1,2 +1,3 @@\n-old\n+new\n context")));
        let inspector = app.github_inspector.as_mut().unwrap();
        inspector.tab = GithubTab::Files;
        inspector.diff_open = true;

        let text = render(&mut app, 100, 28);

        assert!(text.contains("src/lib.rs"), "got:\n{text}");
        assert!(text.contains("@@ -1,2 +1,3 @@"), "got:\n{text}");
        assert!(text.contains("+new"), "got:\n{text}");
    }

    #[test]
    fn omitted_patch_explains_why_diff_is_unavailable() {
        let mut app = app_with(pull(None));
        let inspector = app.github_inspector.as_mut().unwrap();
        inspector.tab = GithubTab::Files;
        inspector.diff_open = true;

        let text = render(&mut app, 80, 20);

        assert!(text.contains("Diff unavailable"), "got:\n{text}");
    }

    #[test]
    fn narrow_inspector_does_not_panic() {
        let mut app = app_with(issue());

        let text = render(&mut app, 24, 8);

        assert!(text.contains("GitHub"), "got:\n{text}");
    }
}
