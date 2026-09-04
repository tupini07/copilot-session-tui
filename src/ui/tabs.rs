//! Layout for the session tab bar.
//!
//! Kept separate from rendering so the width arithmetic — the part that actually goes
//! wrong — can be tested without a terminal.

/// One tab in the strip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tab {
    pub label: String,
    pub active: bool,
    pub running: bool,
}

/// One pane's contribution to the strip, before any width fitting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabSource {
    /// Fixed-width activity cell. Counted as chrome rather than title so truncation can
    /// never eat it — a tab that lost its spinner to a long name would be worse than a
    /// tab with a shorter name.
    pub marker: String,
    pub title: String,
    pub running: bool,
}

/// Build tab labels that together fit within `available` columns.
///
/// Titles share the space evenly and are truncated with an ellipsis. The active tab is
/// never dropped: when there are too many sessions to show, the strip is windowed around
/// it and the hidden count is reported so the caller can render a `+n` marker.
pub fn layout(sessions: &[TabSource], focused: usize, available: usize) -> (Vec<Tab>, usize) {
    if sessions.is_empty() {
        return (Vec::new(), 0);
    }

    let marker_width = |index: usize| crate::text::display_width(&sessions[index].marker);
    // Two spaces each side, the index and its separating space, then the status cell.
    let chrome = |index: usize, marker: usize| index_width(index) + 5 + marker;
    let widest_marker = (0..sessions.len()).map(marker_width).max().unwrap_or(0);
    let min_tab = chrome(sessions.len(), widest_marker) + MIN_TITLE;
    let max_visible = (available / min_tab).max(1).min(sessions.len());

    let start = window_start(focused, sessions.len(), max_visible);
    let visible = &sessions[start..start + max_visible];

    // Divide the remaining columns between the titles actually shown, then cap the
    // share. Without a ceiling, a wide terminal hands every tab an enormous budget and
    // one rambling session name stretches across most of the strip; Copilot rewrites
    // those names freely, so the length is not something the user controls.
    let chrome_total: usize = (start..start + max_visible)
        .map(|i| chrome(i + 1, marker_width(i)))
        .sum();
    let title_budget = (available.saturating_sub(chrome_total) / max_visible.max(1)).min(MAX_TITLE);

    let tabs = visible
        .iter()
        .enumerate()
        .map(|(offset, source)| {
            let index = start + offset + 1;
            Tab {
                label: format!(
                    "  {index} {}{}  ",
                    source.marker,
                    truncate(&source.title, title_budget.max(MIN_TITLE))
                ),
                active: start + offset == focused,
                running: source.running,
            }
        })
        .collect();

    (tabs, sessions.len() - max_visible)
}

const MIN_TITLE: usize = 6;

/// Longest title a tab will show before it is ellipsised, however much room there is.
///
/// Chosen to be generous enough that most session names survive whole, while keeping
/// every tab reachable at a glance rather than letting one push the others off-screen.
const MAX_TITLE: usize = 24;

fn index_width(index: usize) -> usize {
    index.to_string().len()
}

/// Index of the first visible tab, for callers mapping a rendered tab back to its pane.
///
/// Takes the already-computed visible count so hit-testing reuses whatever `layout`
/// decided rather than re-deriving the width arithmetic and risking a mismatch.
pub fn window_start_for(total: usize, visible: usize, focused: usize) -> usize {
    if total == 0 || visible == 0 {
        return 0;
    }
    window_start(focused, total, visible)
}

/// Slide the visible window so the focused tab is always inside it.
fn window_start(focused: usize, total: usize, visible: usize) -> usize {
    if visible >= total {
        return 0;
    }
    // Centre the focus, then clamp to the ends so the strip stays full.
    let half = visible / 2;
    focused.saturating_sub(half).min(total - visible)
}

fn truncate(title: &str, budget: usize) -> String {
    // Measured in terminal columns, not chars, so an emoji title cannot overflow its
    // tab and push the rest of the strip sideways.
    crate::text::truncate_to_width(title, budget)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Idle sessions: a blank two-column status cell, matching what the bar draws when
    /// nothing is running.
    fn sessions(names: &[&str]) -> Vec<TabSource> {
        names
            .iter()
            .map(|n| TabSource {
                marker: "  ".to_string(),
                title: n.to_string(),
                running: true,
            })
            .collect()
    }

    #[test]
    fn a_single_session_gets_the_whole_strip() {
        let (tabs, hidden) = layout(&sessions(&["copilot-session-tui"]), 0, 80);

        assert_eq!(tabs.len(), 1);
        assert_eq!(hidden, 0);
        assert_eq!(tabs[0].label, "  1   copilot-session-tui  ");
        assert!(tabs[0].active);
    }

    #[test]
    fn every_session_is_numbered_for_the_prefix_jump_keys() {
        let (tabs, _) = layout(&sessions(&["alpha", "beta", "gamma"]), 1, 80);

        assert_eq!(tabs[0].label, "  1   alpha  ");
        assert_eq!(tabs[1].label, "  2   beta  ");
        assert_eq!(tabs[2].label, "  3   gamma  ");
        assert!(tabs[1].active, "the focused tab must be highlighted");
    }

    /// A wide terminal used to hand each tab a share big enough to swallow any title,
    /// so one long Copilot-generated name stretched across the whole strip.
    #[test]
    fn a_long_title_is_capped_even_when_the_terminal_is_wide() {
        let (tabs, hidden) = layout(
            &sessions(&[
                "EmbViz",
                "Textures - Exporting schema7 material render list",
                "Stat Placemnt",
            ]),
            1,
            200,
        );

        assert_eq!(hidden, 0);
        assert!(
            tabs[1].label.contains('…'),
            "the long title must be ellipsised: {:?}",
            tabs[1].label
        );
        assert!(
            crate::text::display_width(&tabs[1].label) <= MAX_TITLE + 10,
            "a capped tab must not run away with the strip: {:?}",
            tabs[1].label
        );
        // Titles that already fit are left alone.
        assert!(tabs[0].label.contains("EmbViz"));
        assert!(!tabs[0].label.contains('…'));
        assert!(tabs[2].label.contains("Stat Placemnt"));
    }

    #[test]
    fn long_titles_are_truncated_to_share_the_width() {
        let names = ["a-very-long-session-title-here"; 4];
        let (tabs, hidden) = layout(&sessions(&names), 0, 60);

        assert_eq!(hidden, 0);
        let total: usize = tabs
            .iter()
            .map(|tab| crate::text::display_width(&tab.label))
            .sum();
        assert!(total <= 60, "tabs overflowed the strip: {total}");
        assert!(tabs[0].label.contains('…'));
    }

    #[test]
    fn emoji_titles_are_measured_in_columns_so_the_strip_never_overflows() {
        let names = ["🚀🚀🚀🚀🚀🚀 launch", "🎉🎉🎉🎉🎉🎉 party", "plain title"];
        let (tabs, _) = layout(&sessions(&names), 0, 40);

        let total: usize = tabs
            .iter()
            .map(|tab| crate::text::display_width(&tab.label))
            .sum();
        assert!(total <= 40, "emoji tabs overflowed the strip: {total}");
    }

    #[test]
    fn the_focused_tab_stays_visible_when_the_strip_overflows() {
        let names: Vec<String> = (1..=20).map(|i| format!("session-{i}")).collect();
        let sessions: Vec<TabSource> = names
            .into_iter()
            .map(|title| TabSource {
                marker: "  ".to_string(),
                title,
                running: true,
            })
            .collect();

        let (tabs, hidden) = layout(&sessions, 17, 40);

        assert!(hidden > 0, "expected some tabs to be hidden");
        assert!(
            tabs.iter().any(|tab| tab.active),
            "the focused session must always be shown"
        );
    }

    #[test]
    fn a_narrow_terminal_still_shows_the_focused_session() {
        let (tabs, _) = layout(&sessions(&["alpha", "beta", "gamma"]), 2, 10);

        assert!(!tabs.is_empty());
        assert!(tabs.iter().any(|tab| tab.active));
    }

    #[test]
    fn dead_sessions_are_flagged_so_they_can_be_styled_differently() {
        let mut sessions = sessions(&["alpha", "beta"]);
        sessions[1].running = false;

        let (tabs, _) = layout(&sessions, 0, 80);

        assert!(tabs[0].running);
        assert!(!tabs[1].running);
    }

    #[test]
    fn no_sessions_produces_no_tabs() {
        let (tabs, hidden) = layout(&[], 0, 80);

        assert!(tabs.is_empty());
        assert_eq!(hidden, 0);
    }
}
