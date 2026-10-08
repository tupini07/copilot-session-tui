//! Tabs that hold several sessions, laid out side by side and stacked.
//!
//! A tab is a window in tmux's sense: one or more sessions arranged by a layout tree.
//! A new session opens in a tab of its own, and splitting pulls a session from another
//! tab into this one. The tree is what allows mixed layouts — one tall session beside
//! two stacked ones is a row of two whose second member is a stack.
//!
//! `MuxState::focused` stays the single answer to "which session has the keyboard";
//! the focused tab is whichever one holds that session. A window only remembers which
//! of its sessions had focus last, so coming back to the tab returns to it.

use serde::{Deserialize, Serialize};

use super::PaneId;

/// The share a member of a split gets until someone resizes it. Only ratios matter,
/// and resizing rewrites them in cells, so a round number is all this needs to be.
pub const DEFAULT_WEIGHT: u16 = 100;

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

/// A direction to move focus in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitStep {
    Left,
    Right,
    Up,
    Down,
}

/// How a tab's sessions are arranged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutNode {
    Pane(PaneId),
    Split {
        direction: SplitDirection,
        children: Vec<LayoutNode>,
        /// One per child; relative.
        weights: Vec<u16>,
    },
}

impl LayoutNode {
    /// Every session in the layout, in reading order.
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect_panes(&mut out);
        out
    }

    fn collect_panes(&self, out: &mut Vec<PaneId>) {
        match self {
            Self::Pane(id) => out.push(*id),
            Self::Split { children, .. } => {
                for child in children {
                    child.collect_panes(out);
                }
            }
        }
    }

    pub fn contains(&self, id: PaneId) -> bool {
        match self {
            Self::Pane(pane) => *pane == id,
            Self::Split { children, .. } => children.iter().any(|child| child.contains(id)),
        }
    }

    pub fn first_pane(&self) -> PaneId {
        match self {
            Self::Pane(id) => *id,
            Self::Split { children, .. } => children[0].first_pane(),
        }
    }

    /// Child indices from the root to `id`'s leaf.
    pub fn path_to(&self, id: PaneId) -> Option<Vec<usize>> {
        match self {
            Self::Pane(pane) => (*pane == id).then(Vec::new),
            Self::Split { children, .. } => children.iter().enumerate().find_map(|(i, child)| {
                child.path_to(id).map(|mut rest| {
                    rest.insert(0, i);
                    rest
                })
            }),
        }
    }

    pub fn node_at_mut(&mut self, path: &[usize]) -> Option<&mut LayoutNode> {
        match path.split_first() {
            None => Some(self),
            Some((&index, rest)) => match self {
                Self::Pane(_) => None,
                Self::Split { children, .. } => children.get_mut(index)?.node_at_mut(rest),
            },
        }
    }

    /// Take `id` out, returning the session that now occupies its place — the one
    /// the keyboard should go to if `id` had it. `None` when `id` is not here or is
    /// the whole layout, which only the window holding it can deal with.
    pub fn remove(&mut self, id: PaneId) -> Option<PaneId> {
        let successor = self.remove_inner(id)?;
        self.normalize();
        Some(successor)
    }

    fn remove_inner(&mut self, id: PaneId) -> Option<PaneId> {
        let Self::Split {
            children, weights, ..
        } = self
        else {
            return None;
        };
        let index = children.iter().position(|child| child.contains(id))?;
        if children[index] == Self::Pane(id) {
            children.remove(index);
            weights.remove(index);
            let next = children.get(index).or_else(|| children.last())?;
            return Some(next.first_pane());
        }
        children[index].remove_inner(id)
    }

    /// Put `new` beside `anchor` in `direction`.
    ///
    /// Splitting the way `anchor`'s own split already runs adds a member to it; the
    /// other way nests a new split in `anchor`'s place, which is how a column comes to
    /// hold a stack. Same as tmux splitting the active pane.
    pub fn insert_beside(&mut self, anchor: PaneId, new: PaneId, direction: SplitDirection) {
        if self.contains(new) {
            return;
        }
        let Some(path) = self.path_to(anchor) else {
            return;
        };
        if let Some((&index, parent_path)) = path.split_last() {
            if let Some(Self::Split {
                direction: parent_direction,
                children,
                weights,
            }) = self.node_at_mut(parent_path)
            {
                if *parent_direction == direction {
                    let average = average_weight(weights);
                    children.insert(index + 1, Self::Pane(new));
                    weights.insert(index + 1, average);
                    return;
                }
            }
        }
        if let Some(leaf) = self.node_at_mut(&path) {
            *leaf = Self::Split {
                direction,
                children: vec![Self::Pane(anchor), Self::Pane(new)],
                weights: vec![DEFAULT_WEIGHT, DEFAULT_WEIGHT],
            };
        }
        self.normalize();
    }

    /// Put `new` exactly where `old` is.
    pub fn replace(&mut self, old: PaneId, new: PaneId) -> bool {
        match self {
            Self::Pane(id) if *id == old => {
                *id = new;
                true
            }
            Self::Pane(_) => false,
            Self::Split { children, .. } => {
                children.iter_mut().any(|child| child.replace(old, new))
            }
        }
    }

    /// Change the direction of the split that `id` sits directly in. Returns whether
    /// anything changed.
    pub fn turn_parent(&mut self, id: PaneId, direction: SplitDirection) -> bool {
        let Some(path) = self.path_to(id) else {
            return false;
        };
        let Some((_, parent_path)) = path.split_last() else {
            return false;
        };
        let changed = match self.node_at_mut(parent_path) {
            Some(Self::Split {
                direction: current, ..
            }) if *current != direction => {
                *current = direction;
                true
            }
            _ => false,
        };
        if changed {
            self.normalize();
        }
        changed
    }

    /// Every split back to equal shares.
    pub fn equalize(&mut self) {
        if let Self::Split {
            children, weights, ..
        } = self
        {
            weights
                .iter_mut()
                .for_each(|weight| *weight = DEFAULT_WEIGHT);
            children.iter_mut().for_each(LayoutNode::equalize);
        }
    }

    /// Keep the tree in the one shape the layout code assumes: no split of a single
    /// member, and no split directly inside another running the same way.
    ///
    /// The second matters beyond tidiness. Borders between neighbours are drawn once,
    /// by the later one; that rule only holds at every depth when each nested split
    /// runs across its parent.
    pub fn normalize(&mut self) {
        let Self::Split {
            direction,
            children,
            weights,
        } = self
        else {
            return;
        };
        for child in children.iter_mut() {
            child.normalize();
        }
        let mut flat_children = Vec::with_capacity(children.len());
        let mut flat_weights = Vec::with_capacity(weights.len());
        for (child, weight) in children.drain(..).zip(weights.drain(..)) {
            match child {
                Self::Split {
                    direction: inner,
                    children: grandchildren,
                    weights: inner_weights,
                } if inner == *direction => {
                    // The nested members share their parent's slot in proportion.
                    let total: u32 = inner_weights.iter().map(|w| u32::from(*w).max(1)).sum();
                    for (grandchild, inner_weight) in grandchildren.into_iter().zip(inner_weights) {
                        let share = u32::from(weight).max(1) * u32::from(inner_weight).max(1)
                            / total.max(1);
                        flat_children.push(grandchild);
                        flat_weights.push(u16::try_from(share.max(1)).unwrap_or(u16::MAX));
                    }
                }
                other => {
                    flat_children.push(other);
                    flat_weights.push(weight);
                }
            }
        }
        *children = flat_children;
        *weights = flat_weights;
        if children.len() == 1 {
            *self = children.remove(0);
        }
    }

    pub fn save(&self, session_of: &impl Fn(PaneId) -> Option<String>) -> Option<SavedNode> {
        match self {
            Self::Pane(id) => session_of(*id).map(SavedNode::Pane),
            Self::Split {
                direction,
                children,
                weights,
            } => Some(SavedNode::Split {
                direction: *direction,
                children: children
                    .iter()
                    .map(|child| child.save(session_of))
                    .collect::<Option<Vec<_>>>()?,
                weights: weights.clone(),
            }),
        }
    }
}

fn average_weight(weights: &[u16]) -> u16 {
    let total: u32 = weights.iter().map(|weight| u32::from(*weight)).sum();
    u16::try_from(total / weights.len().max(1) as u32)
        .unwrap_or(DEFAULT_WEIGHT)
        .max(1)
}

pub type WindowId = u64;

/// One tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub id: WindowId,
    pub layout: LayoutNode,
    /// The focused session temporarily has the whole tab. The arrangement is kept so
    /// that unzooming puts everything back where it was.
    pub zoomed: bool,
    /// The session that last had the keyboard here, so returning to the tab returns
    /// to it rather than to whichever happens to be first.
    pub last_focused: PaneId,
}

impl Window {
    pub fn single(id: WindowId, pane: PaneId) -> Self {
        Self {
            id,
            layout: LayoutNode::Pane(pane),
            zoomed: false,
            last_focused: pane,
        }
    }

    pub fn is_split(&self) -> bool {
        matches!(self.layout, LayoutNode::Split { .. })
    }
}

/// A layout written down by session rather than pane, which is what survives CST
/// restarting itself after an update: pane ids are handed out afresh, session ids
/// are not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SavedNode {
    Pane(String),
    Split {
        direction: SplitDirection,
        children: Vec<SavedNode>,
        weights: Vec<u16>,
    },
}

impl SavedNode {
    /// Drop sessions that will not be there, collapsing whatever that leaves empty.
    pub fn retain(self, keep: &impl Fn(&str) -> bool) -> Option<Self> {
        match self {
            Self::Pane(session) => keep(&session).then_some(Self::Pane(session)),
            Self::Split {
                direction,
                children,
                weights,
            } => {
                let (children, weights): (Vec<_>, Vec<_>) = children
                    .into_iter()
                    .zip(weights.into_iter().chain(std::iter::repeat(DEFAULT_WEIGHT)))
                    .filter_map(|(child, weight)| child.retain(keep).map(|child| (child, weight)))
                    .unzip();
                match children.len() {
                    0 => None,
                    1 => children.into_iter().next(),
                    _ => Some(Self::Split {
                        direction,
                        children,
                        weights,
                    }),
                }
            }
        }
    }

    #[cfg(test)]
    pub fn sessions(&self) -> Vec<String> {
        match self {
            Self::Pane(session) => vec![session.clone()],
            Self::Split { children, .. } => children.iter().flat_map(Self::sessions).collect(),
        }
    }

    /// Back into a layout, by whichever sessions `pane_of` can find.
    pub fn restore(&self, pane_of: &impl Fn(&str) -> Option<PaneId>) -> Option<LayoutNode> {
        let mut node = match self {
            Self::Pane(session) => LayoutNode::Pane(pane_of(session)?),
            Self::Split {
                direction,
                children,
                weights,
            } => {
                let (children, weights): (Vec<_>, Vec<_>) = children
                    .iter()
                    .zip(
                        weights
                            .iter()
                            .copied()
                            .chain(std::iter::repeat(DEFAULT_WEIGHT)),
                    )
                    .filter_map(|(child, weight)| child.restore(pane_of).map(|node| (node, weight)))
                    .unzip();
                if children.is_empty() {
                    return None;
                }
                LayoutNode::Split {
                    direction: *direction,
                    children,
                    weights,
                }
            }
        };
        node.normalize();
        Some(node)
    }
}

/// A tab as written down for the update restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedWindow {
    pub layout: SavedNode,
    #[serde(default)]
    pub zoomed: bool,
    #[serde(default)]
    pub focused_session_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn columns(children: Vec<LayoutNode>) -> LayoutNode {
        let weights = vec![DEFAULT_WEIGHT; children.len()];
        LayoutNode::Split {
            direction: SplitDirection::Columns,
            children,
            weights,
        }
    }

    #[test]
    fn splitting_the_same_way_adds_a_member_beside_the_anchor() {
        let mut layout = LayoutNode::Pane(1);
        layout.insert_beside(1, 2, SplitDirection::Columns);
        layout.insert_beside(1, 3, SplitDirection::Columns);
        assert_eq!(layout.panes(), vec![1, 3, 2]);
        assert!(matches!(&layout, LayoutNode::Split { children, .. } if children.len() == 3));
    }

    #[test]
    fn splitting_across_nests_so_one_column_can_hold_a_stack() {
        let mut layout = LayoutNode::Pane(1);
        layout.insert_beside(1, 2, SplitDirection::Columns);
        layout.insert_beside(2, 3, SplitDirection::Rows);
        let LayoutNode::Split {
            direction,
            children,
            ..
        } = &layout
        else {
            panic!("a split");
        };
        assert_eq!(*direction, SplitDirection::Columns);
        assert_eq!(children[0], LayoutNode::Pane(1));
        assert!(matches!(
            &children[1],
            LayoutNode::Split { direction: SplitDirection::Rows, children, .. } if children.len() == 2
        ));
    }

    #[test]
    fn removing_a_member_collapses_what_is_left_and_names_who_takes_its_place() {
        let mut layout = LayoutNode::Pane(1);
        layout.insert_beside(1, 2, SplitDirection::Columns);
        layout.insert_beside(2, 3, SplitDirection::Rows);
        assert_eq!(layout.remove(2), Some(3));
        assert_eq!(
            layout,
            columns(vec![LayoutNode::Pane(1), LayoutNode::Pane(3)])
        );
        assert_eq!(layout.remove(1), Some(3));
        assert_eq!(layout, LayoutNode::Pane(3));
        assert_eq!(
            layout.remove(3),
            None,
            "the last one is the window's to remove"
        );
    }

    #[test]
    fn a_nested_split_turned_to_match_its_parent_merges_into_it() {
        let mut layout = LayoutNode::Pane(1);
        layout.insert_beside(1, 2, SplitDirection::Columns);
        layout.insert_beside(2, 3, SplitDirection::Rows);
        assert!(layout.turn_parent(3, SplitDirection::Columns));
        assert_eq!(layout.panes(), vec![1, 2, 3]);
        assert!(matches!(&layout, LayoutNode::Split { children, .. } if children.len() == 3));
    }

    #[test]
    fn a_saved_layout_comes_back_without_sessions_that_did_not_return() {
        let mut layout = LayoutNode::Pane(1);
        layout.insert_beside(1, 2, SplitDirection::Columns);
        layout.insert_beside(2, 3, SplitDirection::Rows);
        let saved = layout.save(&|id| Some(format!("s{id}"))).unwrap();
        let kept = saved.clone().retain(&|session| session != "s3").unwrap();
        assert_eq!(kept.sessions(), vec!["s1", "s2"]);

        let restored = saved
            .restore(&|session| {
                session
                    .strip_prefix('s')?
                    .parse()
                    .ok()
                    .filter(|id| *id != 2)
            })
            .unwrap();
        assert_eq!(
            restored,
            columns(vec![LayoutNode::Pane(1), LayoutNode::Pane(3)])
        );
    }
}
