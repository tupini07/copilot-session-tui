//! Several sessions' chats on screen at once, side by side or stacked.
//!
//! A split is only a statement about which tabs share the screen. Tabs stay one per
//! session, and `MuxState::focused` stays the one answer to "which session gets the
//! keyboard"; the split never holds a second copy of that. What it adds is the order
//! the visible sessions are laid out in and how much room each gets.
//!
//! It is deliberately flat — one direction, no nesting. Watching several long-running
//! sessions at once is the whole use case, and a row of columns covers it.

use serde::{Deserialize, Serialize};

use super::PaneId;

/// The share a slot gets until someone resizes it. Only the ratio between slots
/// matters; a round number leaves room to nudge either way in whole steps.
pub const DEFAULT_WEIGHT: u16 = 100;

/// Smallest share a slot may be resized down to, relative to [`DEFAULT_WEIGHT`].
/// The layout enforces the real limit in cells; this only stops a weight reaching
/// zero, which would make the slot vanish rather than shrink.
pub const MIN_WEIGHT: u16 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitDirection {
    /// Side by side.
    Columns,
    /// One above the other.
    Rows,
}

impl SplitDirection {
    pub fn label(self) -> &'static str {
        match self {
            Self::Columns => "side by side",
            Self::Rows => "stacked",
        }
    }
}

/// A direction to move focus between slots in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitStep {
    Left,
    Right,
    Up,
    Down,
}

impl SplitStep {
    /// How far along the slot order this step moves in a layout of `direction`, or
    /// `None` when it runs across the layout instead — Up in a row of columns has
    /// nowhere to go.
    fn offset(self, direction: SplitDirection) -> Option<isize> {
        match (direction, self) {
            (SplitDirection::Columns, Self::Left) | (SplitDirection::Rows, Self::Up) => Some(-1),
            (SplitDirection::Columns, Self::Right) | (SplitDirection::Rows, Self::Down) => Some(1),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitLayout {
    pub direction: SplitDirection,
    /// On-screen order. Always at least two while the split exists.
    pub slots: Vec<PaneId>,
    /// One per slot; relative, not cells.
    pub weights: Vec<u16>,
    /// The focused slot temporarily has the whole area. The arrangement is kept so
    /// that unzooming puts everything back where it was.
    pub zoomed: bool,
}

impl SplitLayout {
    pub fn new(direction: SplitDirection, first: PaneId, second: PaneId) -> Self {
        Self {
            direction,
            slots: vec![first, second],
            weights: vec![DEFAULT_WEIGHT, DEFAULT_WEIGHT],
            zoomed: false,
        }
    }

    pub fn contains(&self, id: PaneId) -> bool {
        self.slots.contains(&id)
    }

    pub fn position(&self, id: PaneId) -> Option<usize> {
        self.slots.iter().position(|slot| *slot == id)
    }

    /// Add `id` right after `anchor`, or at the end when `anchor` is not on screen.
    ///
    /// The newcomer gets the average share rather than the default, so adding a
    /// split to a layout someone has already resized does not undo their resizing.
    pub fn insert_after(&mut self, anchor: PaneId, id: PaneId) {
        if self.contains(id) {
            return;
        }
        let total: u32 = self.weights.iter().map(|weight| u32::from(*weight)).sum();
        let average = (total / self.weights.len().max(1) as u32).max(u32::from(MIN_WEIGHT));
        let at = self
            .position(anchor)
            .map_or(self.slots.len(), |index| index + 1);
        self.slots.insert(at, id);
        self.weights
            .insert(at, u16::try_from(average).unwrap_or(DEFAULT_WEIGHT));
    }

    /// Returns the index the slot had, so a caller can pick its neighbour.
    pub fn remove(&mut self, id: PaneId) -> Option<usize> {
        let index = self.position(id)?;
        self.slots.remove(index);
        self.weights.remove(index);
        Some(index)
    }

    /// Put `new` where `old` is, keeping its share of the space.
    pub fn replace(&mut self, old: PaneId, new: PaneId) -> bool {
        match self.position(old) {
            Some(index) if !self.contains(new) => {
                self.slots[index] = new;
                true
            }
            _ => false,
        }
    }

    /// The slot one step from `id`, if there is one that way. Does not wrap: with
    /// three columns, Left from the first is nothing, which is what the arrows on the
    /// screen suggest.
    pub fn neighbour(&self, id: PaneId, step: SplitStep) -> Option<PaneId> {
        let offset = step.offset(self.direction)?;
        let index = self.position(id)?;
        let target = index.checked_add_signed(offset)?;
        self.slots.get(target).copied()
    }

    /// Too few slots left to be a split at all.
    pub fn is_degenerate(&self) -> bool {
        self.slots.len() < 2
    }
}

/// A split written down by session rather than pane, which is what survives CST
/// restarting itself after an update: pane ids are handed out afresh, session ids
/// are not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedSplit {
    pub direction: SplitDirection,
    pub session_ids: Vec<String>,
    pub weights: Vec<u16>,
    #[serde(default)]
    pub zoomed: bool,
}

impl SavedSplit {
    /// Drop sessions that will not be coming back. `None` when fewer than two remain,
    /// since that is no longer a split.
    pub fn retain_sessions(mut self, keep: impl Fn(&str) -> bool) -> Option<Self> {
        let mut weights = self.weights.into_iter();
        let (session_ids, kept_weights): (Vec<String>, Vec<u16>) = self
            .session_ids
            .into_iter()
            .map(|session_id| (session_id, weights.next().unwrap_or(DEFAULT_WEIGHT)))
            .filter(|(session_id, _)| keep(session_id))
            .unzip();
        self.session_ids = session_ids;
        self.weights = kept_weights;
        (self.session_ids.len() >= 2).then_some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inserting_after_the_focused_slot_places_the_new_session_beside_it_not_at_the_end() {
        let mut split = SplitLayout::new(SplitDirection::Columns, 1, 2);
        split.insert_after(1, 3);
        assert_eq!(split.slots, vec![1, 3, 2]);
        assert_eq!(split.weights.len(), 3);
    }

    #[test]
    fn a_new_slot_takes_the_average_share_so_earlier_resizing_survives_it() {
        let mut split = SplitLayout::new(SplitDirection::Columns, 1, 2);
        split.weights = vec![150, 50];
        split.insert_after(2, 3);
        assert_eq!(split.weights, vec![150, 50, 100]);
    }

    #[test]
    fn inserting_a_session_already_on_screen_changes_nothing() {
        let mut split = SplitLayout::new(SplitDirection::Columns, 1, 2);
        split.insert_after(1, 2);
        assert_eq!(split.slots, vec![1, 2]);
    }

    #[test]
    fn neighbours_follow_the_layout_direction_and_never_wrap() {
        let mut split = SplitLayout::new(SplitDirection::Columns, 1, 2);
        split.insert_after(2, 3);
        assert_eq!(split.neighbour(2, SplitStep::Left), Some(1));
        assert_eq!(split.neighbour(2, SplitStep::Right), Some(3));
        assert_eq!(split.neighbour(3, SplitStep::Right), None);
        assert_eq!(split.neighbour(1, SplitStep::Left), None);
        assert_eq!(
            split.neighbour(2, SplitStep::Up),
            None,
            "up in a row of columns runs across the layout, not along it"
        );

        split.direction = SplitDirection::Rows;
        assert_eq!(split.neighbour(2, SplitStep::Up), Some(1));
        assert_eq!(split.neighbour(2, SplitStep::Down), Some(3));
    }

    #[test]
    fn replacing_keeps_the_slot_position_and_refuses_a_session_already_shown() {
        let mut split = SplitLayout::new(SplitDirection::Rows, 1, 2);
        assert!(split.replace(1, 5));
        assert_eq!(split.slots, vec![5, 2]);
        assert!(!split.replace(5, 2), "one session cannot fill two slots");
    }
}
