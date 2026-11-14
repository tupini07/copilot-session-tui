//! The list of what is running behind a session, and which of it counts as work.
//!
//! Opened when asked for and never by itself. A command starting in the background is
//! routine; a popup for each would be dismissed unread within a day.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::App;
use crate::background::{age, ProcessList, Row, MARKER};
use crate::text;

/// Rows of the popup that are not list: the border, the blank lines, the note and the keys.
const CHROME_ROWS: u16 = 7;

pub fn draw(f: &mut Frame, app: &App) {
    let Some(popup) = app.background_popup.as_ref() else {
        return;
    };
    let theme = app.theme();
    let rows = app.background_rows();
    let selected = popup.selected_index(&rows);
    let title = app
        .mux
        .as_ref()
        .and_then(|mux| mux.pane(popup.pane))
        .map(|pane| pane.title.clone())
        .unwrap_or_default();

    let screen = f.area();
    let width = screen
        .width
        .saturating_sub(8)
        .clamp(40, 100)
        .min(screen.width);
    let wanted = rows.len().max(1) as u16 + CHROME_ROWS;
    let height = wanted
        .min(screen.height.saturating_sub(4))
        .max(8)
        .min(screen.height);
    let area = Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height - height) / 2,
        width,
        height,
    );
    super::popups::prepare_popup(f, area, theme);

    let inner_width = width.saturating_sub(2) as usize;
    let visible = height.saturating_sub(CHROME_ROWS) as usize;
    // Keep the selection in view by starting the window as late as it must.
    let first = (selected + 1).saturating_sub(visible.max(1));

    let mut lines = vec![Line::from("")];
    if rows.is_empty() {
        lines.push(Line::from(Span::styled(
            "  Nothing is running in the background of this session.",
            Style::default().fg(theme.muted),
        )));
    }
    for (index, row) in rows.iter().enumerate().skip(first).take(visible.max(1)) {
        lines.push(row_line(row, index == selected, inner_width, app));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("  {}", note(&popup.processes, rows.len())),
        Style::default().fg(theme.muted),
    )));
    lines.push(Line::from(""));
    let key = |name: &'static str| {
        Span::styled(
            name,
            Style::default()
                .fg(theme.accent_alt)
                .add_modifier(Modifier::BOLD),
        )
    };
    lines.push(Line::from(vec![
        Span::raw("  "),
        key("Space"),
        Span::raw(" watch / stop watching   "),
        key("r"),
        Span::raw(" list processes again   "),
        key("Esc"),
        Span::raw(" close"),
    ]));

    let block = Block::default()
        .title(format!(
            " Background work: {} ",
            text::truncate_to_width(&title, inner_width.saturating_sub(22))
        ))
        .borders(Borders::ALL)
        .style(super::popups::surface_style(theme))
        .border_style(Style::default().fg(theme.accent_alt));
    f.render_widget(
        Paragraph::new(lines)
            .style(super::popups::surface_style(theme))
            .block(block),
        area,
    );
}

/// What the line under the list says: how the process half of it is doing.
fn note(processes: &ProcessList, rows: usize) -> String {
    match processes {
        ProcessList::Loading => "Listing this session's processes…".to_string(),
        ProcessList::Unavailable(reason) => reason.clone(),
        ProcessList::Loaded(_) if rows == 0 => {
            "Commands Copilot leaves running, and what its shells start, are listed here."
                .to_string()
        }
        ProcessList::Loaded(_) => {
            "Watched work shows on the tab until it ends. Nothing else is held back.".to_string()
        }
    }
}

fn row_line<'a>(row: &Row, selected: bool, width: usize, app: &App) -> Line<'a> {
    let theme = app.theme();
    let pointer = if selected { "> " } else { "  " };
    let marker = if row.watched { MARKER } else { "  " };
    let age_text = row.age.map(age).unwrap_or_default();
    let state = if row.watched { "watching" } else { "" };
    // Fixed columns on the right, so the eye can run down ages and kinds; the label
    // takes what is left and is cut short, never wrapped.
    let tail = format!(" {age_text:>4}  {:<8}  {state:<8} ", row.kind);
    let label_width = width
        .saturating_sub(text::display_width(pointer) + text::display_width(marker))
        .saturating_sub(text::display_width(&tail));
    let label = text::truncate_to_width(&row.label, label_width);
    let padding = " ".repeat(label_width.saturating_sub(text::display_width(&label)));

    let base = if selected {
        super::row_selection_style(theme)
    } else {
        super::popups::surface_style(theme)
    };
    let emphasis = if row.watched {
        base.add_modifier(Modifier::BOLD)
    } else {
        base
    };
    Line::from(vec![
        Span::styled(pointer, base),
        Span::styled(marker, emphasis),
        Span::styled(format!("{label}{padding}"), emphasis),
        Span::styled(tail, base),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::background::RowKey;
    use std::time::Duration;

    fn text_of(line: &Line) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn a_row_keeps_its_age_and_kind_in_fixed_columns_however_long_the_command_is() {
        let app = App::new(Vec::new(), crate::config::UserConfig::default());
        let long = Row {
            key: RowKey::Command("a".into()),
            label: "python train.py --with a very long list of flags that goes on and on".into(),
            age: Some(Duration::from_secs(7 * 60)),
            watched: true,
            kind: "process",
        };
        let short = Row {
            key: RowKey::Command("b".into()),
            label: "Run dev server".into(),
            age: Some(Duration::from_secs(2 * 3600)),
            watched: false,
            kind: "command",
        };

        let first = text_of(&row_line(&long, true, 60, &app));
        let second = text_of(&row_line(&short, false, 60, &app));

        assert_eq!(text::display_width(&first), 60, "got {first:?}");
        assert_eq!(text::display_width(&second), 60, "got {second:?}");
        assert!(first.starts_with("> ◷ python train.py"), "got {first:?}");
        assert!(
            first.ends_with("   7m  process   watching "),
            "got {first:?}"
        );
        assert!(second.starts_with("    Run dev server"), "got {second:?}");
        assert!(
            second.ends_with("   2h  command            "),
            "got {second:?}"
        );
    }

    #[test]
    fn the_note_says_why_there_is_no_process_list_instead_of_showing_an_empty_one() {
        let reason = "Processes of a tmux-backed session run inside tmux.".to_string();
        assert_eq!(note(&ProcessList::Unavailable(reason.clone()), 0), reason);
        assert!(note(&ProcessList::Loading, 2).contains("Listing"));
    }
}
