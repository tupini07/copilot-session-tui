//! Layout for the tmux-style session tab bar.
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

/// Build tab labels that together fit within `available` columns.
///
/// Titles share the space evenly and are truncated with an ellipsis. The active tab is
/// never dropped: when there are too many sessions to show, the strip is windowed around
/// it and the hidden count is reported so the caller can render a `+n` marker.
pub fn layout(sessions: &[(String, bool)], focused: usize, available: usize) -> (Vec<Tab>, usize) {
    if sessions.is_empty() {
        return (Vec::new(), 0);
    }

    // "1:" plus a leading and trailing space.
    let chrome = |index: usize| index_width(index) + 3;
    let min_tab = chrome(sessions.len()) + MIN_TITLE;
    let max_visible = (available / min_tab).max(1).min(sessions.len());

    let start = window_start(focused, sessions.len(), max_visible);
    let visible = &sessions[start..start + max_visible];

    // Divide the remaining columns between the titles actually shown.
    let chrome_total: usize = (start..start + max_visible).map(|i| chrome(i + 1)).sum();
    let title_budget = available.saturating_sub(chrome_total) / max_visible.max(1);

    let tabs = visible
        .iter()
        .enumerate()
        .map(|(offset, (title, running))| {
            let index = start + offset + 1;
            Tab {
                label: format!(" {index}:{} ", truncate(title, title_budget.max(MIN_TITLE))),
                active: start + offset == focused,
                running: *running,
            }
        })
        .collect();

    (tabs, sessions.len() - max_visible)
}

const MIN_TITLE: usize = 6;

fn index_width(index: usize) -> usize {
    index.to_string().len()
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
    let chars: Vec<char> = title.chars().collect();
    if chars.len() <= budget {
        return title.to_string();
    }
    if budget <= 1 {
        return "…".to_string();
    }
    let kept: String = chars[..budget - 1].iter().collect();
    format!("{}…", kept.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sessions(names: &[&str]) -> Vec<(String, bool)> {
        names.iter().map(|n| (n.to_string(), true)).collect()
    }

    #[test]
    fn a_single_session_gets_the_whole_strip() {
        let (tabs, hidden) = layout(&sessions(&["copilot-session-tui"]), 0, 80);

        assert_eq!(tabs.len(), 1);
        assert_eq!(hidden, 0);
        assert_eq!(tabs[0].label, " 1:copilot-session-tui ");
        assert!(tabs[0].active);
    }

    #[test]
    fn every_session_is_numbered_for_the_prefix_jump_keys() {
        let (tabs, _) = layout(&sessions(&["alpha", "beta", "gamma"]), 1, 80);

        assert_eq!(tabs[0].label, " 1:alpha ");
        assert_eq!(tabs[1].label, " 2:beta ");
        assert_eq!(tabs[2].label, " 3:gamma ");
        assert!(tabs[1].active, "the focused tab must be highlighted");
    }

    #[test]
    fn long_titles_are_truncated_to_share_the_width() {
        let names = ["a-very-long-session-title-here"; 4];
        let (tabs, hidden) = layout(&sessions(&names), 0, 60);

        assert_eq!(hidden, 0);
        let total: usize = tabs.iter().map(|tab| tab.label.chars().count()).sum();
        assert!(total <= 60, "tabs overflowed the strip: {total}");
        assert!(tabs[0].label.contains('…'));
    }

    #[test]
    fn the_focused_tab_stays_visible_when_the_strip_overflows() {
        let names: Vec<String> = (1..=20).map(|i| format!("session-{i}")).collect();
        let sessions: Vec<(String, bool)> = names.into_iter().map(|name| (name, true)).collect();

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
        sessions[1].1 = false;

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
