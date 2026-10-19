//! The menu a right-click on a tab opens.
//!
//! The items are fixed when it opens and so is where it sits, so the frame that draws
//! it and the click that picks from it work from the same rectangle and cannot
//! disagree about which row is which.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::App;
use crate::mux::PaneId;
use crate::ui::popups::{prepare_popup, surface_style};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabMenuItem {
    MarkUnread,
    MarkRead,
}

impl TabMenuItem {
    fn label(self) -> &'static str {
        match self {
            Self::MarkUnread => "Mark as unread",
            Self::MarkRead => "Mark as read",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabMenu {
    /// The session the tab shows, which is the one the items act on.
    pub pane: PaneId,
    pub items: Vec<TabMenuItem>,
    pub selected: usize,
    pub area: Rect,
}

impl TabMenu {
    /// A menu for `pane`, hanging below the point that was clicked and kept inside
    /// `screen`.
    pub fn new(pane: PaneId, unread: bool, column: u16, row: u16, screen: Rect) -> Self {
        // Offering to mark unread what already is would do nothing; the other way
        // round is what someone looking at a marked tab wants.
        let items = vec![if unread {
            TabMenuItem::MarkRead
        } else {
            TabMenuItem::MarkUnread
        }];
        let label_width = items
            .iter()
            .map(|item| crate::text::display_width(item.label()))
            .max()
            .unwrap_or(0) as u16;
        // A space either side of the label, inside the border.
        let width = (label_width + 4).min(screen.width);
        let height = (items.len() as u16 + 2).min(screen.height);
        let x = column
            .min(screen.right().saturating_sub(width))
            .max(screen.x);
        let y = row
            .saturating_add(1)
            .min(screen.bottom().saturating_sub(height))
            .max(screen.y);
        Self {
            pane,
            items,
            selected: 0,
            area: Rect::new(x, y, width, height),
        }
    }

    /// The item on the row under the pointer, if it is on one.
    pub fn item_at(&self, column: u16, row: u16) -> Option<usize> {
        let inner = Block::default().borders(Borders::ALL).inner(self.area);
        if column < inner.x || column >= inner.right() || row < inner.y || row >= inner.bottom() {
            return None;
        }
        let index = (row - inner.y) as usize;
        (index < self.items.len()).then_some(index)
    }

    pub fn contains(&self, column: u16, row: u16) -> bool {
        column >= self.area.x
            && column < self.area.right()
            && row >= self.area.y
            && row < self.area.bottom()
    }

    pub fn select_next(&mut self, forward: bool) {
        let len = self.items.len();
        if len == 0 {
            return;
        }
        self.selected = if forward {
            (self.selected + 1) % len
        } else {
            (self.selected + len - 1) % len
        };
    }
}

pub fn draw(f: &mut Frame, app: &App) {
    let Some(menu) = app.tab_menu.as_ref() else {
        return;
    };
    let theme = app.theme();
    // Placed for the screen it was opened on; a resize since must not draw it off
    // the edge.
    let area = menu.area.intersection(f.area());
    if area.is_empty() {
        return;
    }
    prepare_popup(f, area, theme);
    let block = Block::default()
        .borders(Borders::ALL)
        .style(surface_style(theme))
        .border_style(Style::default().fg(theme.accent));
    // The highlight runs the full width of the menu, not just under a short label.
    let width = block.inner(area).width.saturating_sub(1) as usize;
    let lines: Vec<Line> = menu
        .items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let style = if index == menu.selected {
                Style::default()
                    .fg(theme.selection_fg)
                    .bg(theme.selection_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                surface_style(theme)
            };
            Line::from(Span::styled(format!(" {:<width$}", item.label()), style))
        })
        .collect();
    f.render_widget(
        Paragraph::new(lines)
            .style(surface_style(theme))
            .block(block),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_menu_opened_on_the_rightmost_tab_stays_on_screen_rather_than_losing_its_label() {
        let screen = Rect::new(0, 0, 80, 24);
        let menu = TabMenu::new(1, false, 79, 1, screen);

        assert!(menu.area.right() <= screen.right());
        assert_eq!(menu.area.y, 2, "it hangs below the row that was clicked");
        assert!(menu.area.width as usize >= "Mark as unread".len() + 2);
    }

    #[test]
    fn a_tab_already_unread_offers_to_mark_it_read_instead_of_a_no_op() {
        let screen = Rect::new(0, 0, 80, 24);

        assert_eq!(
            TabMenu::new(1, false, 0, 1, screen).items,
            [TabMenuItem::MarkUnread]
        );
        assert_eq!(
            TabMenu::new(1, true, 0, 1, screen).items,
            [TabMenuItem::MarkRead]
        );
    }

    #[test]
    fn only_a_row_holding_an_item_counts_as_picking_it_not_the_border_around_it() {
        let menu = TabMenu::new(1, false, 10, 1, Rect::new(0, 0, 80, 24));
        let area = menu.area;

        assert_eq!(menu.item_at(area.x + 1, area.y + 1), Some(0));
        assert_eq!(menu.item_at(area.x, area.y + 1), None, "left border");
        assert_eq!(menu.item_at(area.x + 1, area.y), None, "top border");
        assert!(menu.contains(area.x, area.y));
        assert!(!menu.contains(area.right(), area.y));
    }

    #[test]
    fn the_item_is_drawn_on_the_row_a_click_picks_it_from() {
        let mut app = App::new(Vec::new(), crate::config::UserConfig::default());
        let menu = TabMenu::new(1, false, 10, 1, Rect::new(0, 0, 40, 10));
        let area = menu.area;
        app.tab_menu = Some(menu);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 10)).unwrap();

        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        let row: String = (area.x..area.right())
            .map(|x| buffer[(x, area.y + 1)].symbol())
            .collect();
        assert!(row.contains("Mark as unread"), "got: {row:?}");
        assert_eq!(
            app.tab_menu
                .as_ref()
                .unwrap()
                .item_at(area.x + 2, area.y + 1),
            Some(0)
        );
    }
}
