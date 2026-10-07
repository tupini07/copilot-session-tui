pub mod callbacks;
pub mod keys;
pub mod pane;
pub mod pty;
pub mod split;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

pub use pane::{Pane, PaneId, PaneSpec, PaneStatus};
pub use split::{SplitDirection, SplitLayout, SplitStep};

/// Events the UI loop must wake up for, from PTYs and from the terminal.
pub enum MuxEvent {
    Output(PaneId, callbacks::PaneSignals),
    Exited(PaneId, Option<u32>),
    SessionLifecycle(PaneId, crate::events::lifecycle::LifecycleEvent),
    HookLifecycle(PaneId, crate::events::hooks::HookLifecycleEvent),
    HookReadyConfirmed(PaneId, u64),
    HostSequence(PaneId, Vec<u8>),
    ConfigChanged,
    /// Notices on disk have changed and the UI thread should act on them.
    ///
    /// Carries nothing: the notices are already persisted, and only the UI thread knows
    /// which panes can take one. Sending the decision instead would mean two places
    /// holding the same list.
    ThreadNoticesChanged,
    /// The thread watcher could not do its job, with a sentence saying why.
    ThreadWatchFailed(String),
    /// A watched thread looks like it is going in circles. Reported, never acted on.
    ThreadStalled(String),
    Term(crossterm::event::Event),
}

/// A parsed prefix key such as `C-b`, `M-x` or `C-Space`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyChord {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl KeyChord {
    pub fn matches(&self, key: &KeyEvent) -> bool {
        // Compare chars case-insensitively: Ctrl-b and Ctrl-B are the same chord.
        let same_code = match (self.code, key.code) {
            (KeyCode::Char(a), KeyCode::Char(b)) => a.eq_ignore_ascii_case(&b),
            (a, b) => a == b,
        };
        same_code && self.modifiers == relevant_modifiers(key.modifiers)
    }

    /// Parse `C-b`, `M-x`, `C-M-a`, `C-Space`, `F5`… Returns `None` on bad input so
    /// callers can fall back to the default rather than failing startup.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }

        let mut modifiers = KeyModifiers::NONE;
        let mut rest = text;
        loop {
            let lower = rest.to_ascii_lowercase();
            if lower.starts_with("c-") || lower.starts_with("ctrl-") {
                modifiers |= KeyModifiers::CONTROL;
                rest = &rest[rest.find('-')? + 1..];
            } else if lower.starts_with("m-") || lower.starts_with("alt-") {
                modifiers |= KeyModifiers::ALT;
                rest = &rest[rest.find('-')? + 1..];
            } else if lower.starts_with("s-") || lower.starts_with("shift-") {
                modifiers |= KeyModifiers::SHIFT;
                rest = &rest[rest.find('-')? + 1..];
            } else {
                break;
            }
        }

        let code = match rest.to_ascii_lowercase().as_str() {
            "space" => KeyCode::Char(' '),
            "tab" => KeyCode::Tab,
            "enter" | "return" => KeyCode::Enter,
            "esc" | "escape" => KeyCode::Esc,
            other => {
                if let Some(number) = other.strip_prefix('f') {
                    match number.parse::<u8>() {
                        Ok(n) if (1..=12).contains(&n) => KeyCode::F(n),
                        _ => return None,
                    }
                } else {
                    let mut chars = other.chars();
                    let c = chars.next()?;
                    if chars.next().is_some() {
                        return None;
                    }
                    KeyCode::Char(c)
                }
            }
        };

        // A bare character with no modifiers would swallow ordinary typing.
        if modifiers.is_empty() && matches!(code, KeyCode::Char(_)) {
            return None;
        }

        Some(Self { code, modifiers })
    }

    pub fn label(&self) -> String {
        let mut label = String::new();
        if self.modifiers.contains(KeyModifiers::CONTROL) {
            label.push_str("C-");
        }
        if self.modifiers.contains(KeyModifiers::ALT) {
            label.push_str("M-");
        }
        if self.modifiers.contains(KeyModifiers::SHIFT) {
            label.push_str("S-");
        }
        match self.code {
            KeyCode::Char(' ') => label.push_str("Space"),
            KeyCode::Char(c) => label.push(c),
            other => label.push_str(&format!("{other:?}")),
        }
        label
    }

    pub fn literal_key_event(self) -> KeyEvent {
        let mut modifiers = self.modifiers;
        let code = match self.code {
            KeyCode::Char(character) if modifiers.contains(KeyModifiers::SHIFT) => {
                modifiers.remove(KeyModifiers::SHIFT);
                KeyCode::Char(shifted_character(character))
            }
            code => code,
        };
        KeyEvent::new(code, modifiers)
    }
}

fn shifted_character(character: char) -> char {
    match character {
        'a'..='z' => character.to_ascii_uppercase(),
        '1' => '!',
        '2' => '@',
        '3' => '#',
        '4' => '$',
        '5' => '%',
        '6' => '^',
        '7' => '&',
        '8' => '*',
        '9' => '(',
        '0' => ')',
        '-' => '_',
        '=' => '+',
        '[' => '{',
        ']' => '}',
        '\\' => '|',
        ';' => ':',
        '\'' => '"',
        ',' => '<',
        '.' => '>',
        '/' => '?',
        '`' => '~',
        character => character,
    }
}

/// Ignore modifiers that terminals report inconsistently (e.g. KEYPAD/SUPER on Windows).
fn relevant_modifiers(modifiers: KeyModifiers) -> KeyModifiers {
    modifiers & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT)
}

/// What the prefix key sequence resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefixCommand {
    Detach,
    NextPane,
    PreviousPane,
    KillPane,
    EndPersistentSession,
    PaneList,
    Chat,
    Scratchpad,
    Terminal,
    Snippets,
    Update,
    Help,
    Github,
    /// `prefix q` — quit CST and close its pane clients.
    Quit,
    /// `prefix m` — enter the sticky mode that slides the focused tab.
    MoveTab,
    SelectIndex(usize),
    /// `prefix |` and `prefix -` — pick a session to show beside the focused one.
    Split(SplitDirection),
    /// `prefix` then an arrow — move to the neighbouring split.
    FocusSplit(SplitStep),
    /// `prefix z` — give the focused split the whole screen, or put the split back.
    ZoomSplit,
    /// `prefix b` — break the focused session out of the split, keeping its tab.
    UnsplitFocused,
    /// `prefix r` — enter the sticky mode that resizes the focused split.
    ResizeSplit,
    /// `prefix prefix` — search every CST command.
    CommandPalette,
    Cancel,
}

#[cfg(test)]
pub fn resolve_prefix_command(key: &KeyEvent, prefix: &KeyChord) -> Option<PrefixCommand> {
    let tmux_keys = crate::config::TmuxKeyConfig::default();
    resolve_prefix_command_with_tmux_keys(key, prefix, Some(&tmux_keys))
}

/// Every character the built-in match below claims. The configured tmux end key is
/// validated against this and only consulted after the built-ins, so drift here makes
/// a configured key inert — it can never steal a multiplexer command.
pub(crate) const PREFIX_COMMAND_KEYS: &[char] = &[
    'd', 'n', 'p', 'x', 'w', 'c', 'e', 't', 's', 'u', 'q', 'm', 'h', 'g', 'z', '|', '-', 'b', 'r',
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9',
];

/// `tmux_keys` is `None` when tmux-backed sessions cannot work here, which keeps the
/// configured end key as inert after the prefix as it would be on a build without
/// the feature.
pub fn resolve_prefix_command_with_tmux_keys(
    key: &KeyEvent,
    prefix: &KeyChord,
    tmux_keys: Option<&crate::config::TmuxKeyConfig>,
) -> Option<PrefixCommand> {
    if prefix.matches(key) {
        return Some(PrefixCommand::CommandPalette);
    }
    let builtin = match key.code {
        KeyCode::Char('d') => Some(PrefixCommand::Detach),
        KeyCode::Char('n') => Some(PrefixCommand::NextPane),
        KeyCode::Char('p') => Some(PrefixCommand::PreviousPane),
        KeyCode::Char('x') => Some(PrefixCommand::KillPane),
        KeyCode::Char('w') => Some(PrefixCommand::PaneList),
        KeyCode::Char('c') => Some(PrefixCommand::Chat),
        KeyCode::Char('e') => Some(PrefixCommand::Scratchpad),
        KeyCode::Char('t') => Some(PrefixCommand::Terminal),
        KeyCode::Char('s') => Some(PrefixCommand::Snippets),
        KeyCode::Char('u') => Some(PrefixCommand::Update),
        KeyCode::Char('q') => Some(PrefixCommand::Quit),
        KeyCode::Char('m') => Some(PrefixCommand::MoveTab),
        KeyCode::Char('h') => Some(PrefixCommand::Help),
        KeyCode::Char('g') => Some(PrefixCommand::Github),
        KeyCode::Char('|') => Some(PrefixCommand::Split(SplitDirection::Columns)),
        KeyCode::Char('-') => Some(PrefixCommand::Split(SplitDirection::Rows)),
        KeyCode::Char('z') => Some(PrefixCommand::ZoomSplit),
        KeyCode::Char('b') => Some(PrefixCommand::UnsplitFocused),
        KeyCode::Char('r') => Some(PrefixCommand::ResizeSplit),
        KeyCode::Left => Some(PrefixCommand::FocusSplit(SplitStep::Left)),
        KeyCode::Right => Some(PrefixCommand::FocusSplit(SplitStep::Right)),
        KeyCode::Up => Some(PrefixCommand::FocusSplit(SplitStep::Up)),
        KeyCode::Down => Some(PrefixCommand::FocusSplit(SplitStep::Down)),
        KeyCode::Char(character)
            if character.eq_ignore_ascii_case(&'h')
                && key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            Some(PrefixCommand::Help)
        }
        KeyCode::Char(character)
            if character.eq_ignore_ascii_case(&'g')
                && key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            Some(PrefixCommand::Github)
        }
        KeyCode::Backspace => Some(PrefixCommand::Help),
        KeyCode::Char(c) if c.is_ascii_digit() => {
            Some(PrefixCommand::SelectIndex(c.to_digit(10)? as usize))
        }
        KeyCode::Esc => Some(PrefixCommand::Cancel),
        _ => None,
    };
    if builtin.is_some() {
        return builtin;
    }
    // Only a key no built-in claims can reach the configured tmux end shortcut, so a
    // bad configuration is at worst inert.
    if tmux_keys.is_some_and(|keys| keys.matches_end_session(key.code)) {
        return Some(PrefixCommand::EndPersistentSession);
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpCommand {
    Scratchpad,
    Cancel,
}

pub fn resolve_help_command(key: &KeyEvent) -> Option<HelpCommand> {
    match key.code {
        KeyCode::Char('e') => Some(HelpCommand::Scratchpad),
        KeyCode::Esc => Some(HelpCommand::Cancel),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GithubCommand {
    Inspect,
    Cancel,
}

pub fn resolve_github_command(key: &KeyEvent) -> Option<GithubCommand> {
    match key.code {
        KeyCode::Char('i') => Some(GithubCommand::Inspect),
        KeyCode::Esc => Some(GithubCommand::Cancel),
        _ => None,
    }
}

/// A sticky sub-mode entered from the prefix menu.
///
/// Unlike the one-shot `Help` and `Github` menus, a transient mode survives the key
/// that acts on it, so a repeated adjustment costs one keystroke instead of three.
/// Every key goes to CST while one is active, because the capture gate already
/// claims everything when `PrefixState` is not `Idle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransientMode {
    /// Left/Right slide the focused tab along the strip.
    MoveTab,
    /// The arrows shrink and grow the focused split.
    ResizeSplit,
}

impl TransientMode {
    /// Badge shown in the status bar while the mode is active.
    pub fn badge(self) -> &'static str {
        match self {
            Self::MoveTab => " Move tab ",
            Self::ResizeSplit => " Resize ",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::MoveTab => " ←/→ move  Esc done ",
            Self::ResizeSplit => " ←/↑ shrink  →/↓ grow  Esc done ",
        }
    }
}

/// What a key means inside a transient mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransientCommand {
    /// Act in the mode's own direction; `true` is rightwards/forwards.
    Step(bool),
    Leave,
    /// Not ours — leave the mode and let the key be handled normally.
    Passthrough,
}

pub fn resolve_transient_command(mode: TransientMode, key: &KeyEvent) -> TransientCommand {
    // The vim-style letters are a convenience for the bare keys only. Ctrl+L clears a
    // screen and Ctrl+H is a backspace; a mode that swallowed those to nudge a tab
    // sideways would be surprising, and the prefix chord has to stay reachable so
    // there is always a way back to the command menu.
    let plain = !key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
    match mode {
        TransientMode::MoveTab => match key.code {
            KeyCode::Left => TransientCommand::Step(false),
            KeyCode::Right => TransientCommand::Step(true),
            KeyCode::Char('h') if plain => TransientCommand::Step(false),
            KeyCode::Char('l') if plain => TransientCommand::Step(true),
            KeyCode::Esc | KeyCode::Enter => TransientCommand::Leave,
            _ => TransientCommand::Passthrough,
        },
        // Both axes, so the same keys work whichever way the split runs.
        TransientMode::ResizeSplit => match key.code {
            KeyCode::Left | KeyCode::Up => TransientCommand::Step(false),
            KeyCode::Right | KeyCode::Down => TransientCommand::Step(true),
            KeyCode::Char('h' | 'k') if plain => TransientCommand::Step(false),
            KeyCode::Char('l' | 'j') if plain => TransientCommand::Step(true),
            KeyCode::Esc | KeyCode::Enter => TransientCommand::Leave,
            _ => TransientCommand::Passthrough,
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefixState {
    Idle,
    Root,
    Help,
    Github,
    /// A sticky mode; see [`TransientMode`].
    Transient(TransientMode),
}

/// All panes owned by this CST instance.
pub struct MuxState {
    pub panes: Vec<Pane>,
    pub focused: Option<PaneId>,
    /// Sessions sharing the screen. While this is `Some`, `focused` is always one of
    /// its slots; [`MuxState::focus`] is what keeps that true.
    pub split: Option<SplitLayout>,
    pub prefix: KeyChord,
    pub prefix_state: PrefixState,
    next_id: PaneId,
    pub events: Sender<MuxEvent>,
    pub receiver: Receiver<MuxEvent>,
}

impl MuxState {
    pub fn new(prefix: KeyChord) -> Self {
        let (events, receiver) = std::sync::mpsc::channel();
        Self {
            panes: Vec::new(),
            focused: None,
            split: None,
            prefix,
            prefix_state: PrefixState::Idle,
            next_id: 1,
            events,
            receiver,
        }
    }

    pub fn allocate_id(&mut self) -> PaneId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn pane(&self, id: PaneId) -> Option<&Pane> {
        self.panes.iter().find(|pane| pane.id == id)
    }

    pub fn pane_mut(&mut self, id: PaneId) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|pane| pane.id == id)
    }

    pub fn focused_pane(&self) -> Option<&Pane> {
        self.focused.and_then(|id| self.pane(id))
    }

    pub fn focused_pane_mut(&mut self) -> Option<&mut Pane> {
        let id = self.focused?;
        self.pane_mut(id)
    }

    /// The number this pane's tab is labelled with, counting from 1 as the strip does.
    pub fn tab_number(&self, id: PaneId) -> Option<usize> {
        self.panes
            .iter()
            .position(|pane| pane.id == id)
            .map(|index| index + 1)
    }

    /// Existing pane for a Copilot session id, so Enter re-focuses instead of duplicating.
    pub fn pane_for_session(&self, session_id: &str) -> Option<PaneId> {
        self.panes
            .iter()
            .find(|pane| pane.session_id == session_id)
            .map(|pane| pane.id)
    }

    pub fn push(&mut self, pane: Pane) -> PaneId {
        let id = pane.id;
        self.panes.push(pane);
        self.focus(id);
        id
    }

    /// Give `id` the keyboard.
    ///
    /// With a split on screen, a session that is not already in it takes the focused
    /// slot's place, the way choosing a window in tmux swaps it into the active pane.
    /// The alternative — focusing something off screen — would leave the user typing
    /// into a session they cannot see.
    pub fn focus(&mut self, id: PaneId) {
        if self.pane(id).is_none() {
            return;
        }
        if let Some(split) = self.split.as_mut() {
            if !split.contains(id) {
                let swapped = self
                    .focused
                    .is_some_and(|current| split.replace(current, id));
                if !swapped {
                    self.split = None;
                }
            }
        }
        self.focused = Some(id);
    }

    pub fn remove(&mut self, id: PaneId) {
        if let Some(index) = self.panes.iter().position(|pane| pane.id == id) {
            let pane = self.panes.remove(index);
            let _ = pane.kill();
            // A session leaving a split hands focus to the slot that closes over it,
            // so the keyboard stays on something that is still on screen.
            let mut successor = None;
            if let Some(split) = self.split.as_mut() {
                if let Some(slot) = split.remove(id) {
                    successor = split
                        .slots
                        .get(slot)
                        .or_else(|| split.slots.last())
                        .copied();
                }
                if split.is_degenerate() {
                    self.split = None;
                }
            }
            if self.focused == Some(id) {
                self.focused = successor.or_else(|| {
                    self.panes
                        .get(index)
                        .or_else(|| self.panes.last())
                        .map(|pane| pane.id)
                });
            }
        }
    }

    pub fn cycle(&mut self, forward: bool) {
        if self.panes.len() < 2 {
            return;
        }
        let Some(current) = self
            .focused
            .and_then(|id| self.panes.iter().position(|pane| pane.id == id))
        else {
            if let Some(first) = self.panes.first().map(|pane| pane.id) {
                self.focus(first);
            }
            return;
        };
        let len = self.panes.len();
        let next = if forward {
            (current + 1) % len
        } else {
            (current + len - 1) % len
        };
        self.focus(self.panes[next].id);
    }

    pub fn select_index(&mut self, index: usize) {
        if let Some(id) = self.panes.get(index).map(|pane| pane.id) {
            self.focus(id);
        }
    }

    /// Put `id` on screen beside the focused session, starting a split if there is
    /// none, and focus it.
    ///
    /// The whole layout takes `direction`: splits are flat, so asking for a stacked
    /// split in a row of columns turns the row into a stack rather than nesting one.
    pub fn split_with(&mut self, direction: SplitDirection, id: PaneId) -> bool {
        let Some(focused) = self.focused else {
            return false;
        };
        if id == focused || self.pane(id).is_none() {
            return false;
        }
        match self.split.as_mut() {
            Some(split) if split.contains(id) => return false,
            Some(split) => {
                split.direction = direction;
                split.zoomed = false;
                split.insert_after(focused, id);
            }
            None => self.split = Some(SplitLayout::new(direction, focused, id)),
        }
        self.focused = Some(id);
        true
    }

    /// Take the focused session out of the split. Its tab stays; focus moves to the
    /// slot that closes over it, and a split down to one session stops being one.
    pub fn unsplit_focused(&mut self) -> bool {
        let (Some(focused), Some(split)) = (self.focused, self.split.as_mut()) else {
            return false;
        };
        let Some(slot) = split.remove(focused) else {
            return false;
        };
        let successor = split
            .slots
            .get(slot)
            .or_else(|| split.slots.last())
            .copied();
        if split.is_degenerate() {
            self.split = None;
        }
        self.focused = successor.or(Some(focused));
        true
    }

    /// Move focus to the neighbouring slot. Returns whether it moved.
    pub fn focus_split(&mut self, step: SplitStep) -> bool {
        let Some(target) = self
            .visible_split()
            .zip(self.focused)
            .and_then(|(split, focused)| split.neighbour(focused, step))
        else {
            return false;
        };
        self.focused = Some(target);
        true
    }

    pub fn toggle_split_zoom(&mut self) -> bool {
        match self.split.as_mut() {
            Some(split) => {
                split.zoomed = !split.zoomed;
                true
            }
            None => false,
        }
    }

    pub fn save_split(&self) -> Option<split::SavedSplit> {
        let split = self.split.as_ref()?;
        Some(split::SavedSplit {
            direction: split.direction,
            session_ids: split
                .slots
                .iter()
                .filter_map(|id| self.pane(*id).map(|pane| pane.session_id.clone()))
                .collect(),
            weights: split.weights.clone(),
            zoomed: split.zoomed,
        })
    }

    /// Rebuild a saved split from whichever of its sessions are open now. Focus stays
    /// where it is if that session is in the split, and otherwise lands on its first.
    pub fn restore_split(&mut self, saved: &split::SavedSplit) {
        let mut slots = Vec::new();
        let mut weights = Vec::new();
        for (index, session_id) in saved.session_ids.iter().enumerate() {
            if let Some(id) = self.pane_for_session(session_id) {
                if !slots.contains(&id) {
                    slots.push(id);
                    weights.push(
                        saved
                            .weights
                            .get(index)
                            .copied()
                            .unwrap_or(split::DEFAULT_WEIGHT),
                    );
                }
            }
        }
        if slots.len() < 2 {
            return;
        }
        if !self.focused.is_some_and(|id| slots.contains(&id)) {
            self.focused = slots.first().copied();
        }
        self.split = Some(SplitLayout {
            direction: saved.direction,
            slots,
            weights,
            zoomed: saved.zoomed,
        });
    }

    /// The split as it should be drawn: absent while zoomed, because then only the
    /// focused session is on screen.
    pub fn visible_split(&self) -> Option<&SplitLayout> {
        self.split.as_ref().filter(|split| !split.zoomed)
    }

    /// Moves `id` to `index`, keeping every other pane in its relative order.
    ///
    /// Returns whether anything moved, so a caller can tell a no-op at the end of
    /// the strip apart from a real reorder and leave the user a hint either way.
    pub fn move_pane_to(&mut self, id: PaneId, index: usize) -> bool {
        let Some(from) = self.panes.iter().position(|pane| pane.id == id) else {
            return false;
        };
        let to = index.min(self.panes.len().saturating_sub(1));
        if from == to {
            return false;
        }
        let pane = self.panes.remove(from);
        self.panes.insert(to, pane);
        true
    }

    /// Moves the focused pane one slot along the strip. Deliberately stops at the
    /// ends rather than wrapping: dragging a tab off one edge and having it appear
    /// at the other is disorienting, and `cycle` already exists for going around.
    pub fn move_focused_pane(&mut self, forward: bool) -> bool {
        let Some(id) = self.focused else {
            return false;
        };
        let Some(from) = self.panes.iter().position(|pane| pane.id == id) else {
            return false;
        };
        let to = if forward {
            from + 1
        } else {
            match from.checked_sub(1) {
                Some(to) => to,
                None => return false,
            }
        };
        if to >= self.panes.len() {
            return false;
        }
        self.panes.swap(from, to);
        true
    }

    pub fn running_count(&self) -> usize {
        self.panes.iter().filter(|pane| pane.is_running()).count()
    }

    /// Catch exits whose notification never reached the event loop, so a quit
    /// confirmation never lists sessions that are already gone.
    pub fn reap(&mut self) {
        for pane in &mut self.panes {
            pane.poll_exit();
        }
    }

    /// Directory of the focused pane, used for the shell auto-`cd` on exit.
    pub fn focused_cwd(&self) -> Option<PathBuf> {
        self.focused_pane().map(|pane| pane.cwd.clone())
    }

    pub fn resize_all(&mut self, rows: u16, cols: u16) {
        for pane in &mut self.panes {
            let _ = pane.resize(rows, cols);
        }
    }

    #[cfg(test)]
    pub fn resize_all_at(&mut self, x: u16, y: u16, rows: u16, cols: u16) {
        for pane in &mut self.panes {
            let _ = pane.resize_at(x, y, rows, cols);
        }
    }

    pub fn shutdown(&mut self) -> Result<()> {
        self.shutdown_with_report().map(|_| ())
    }

    pub fn shutdown_with_report(&mut self) -> Result<Vec<PaneId>> {
        let mut failures = Vec::new();
        let mut terminated = Vec::new();
        for pane in &mut self.panes {
            match pane.shutdown() {
                Ok(true) => terminated.push(pane.id),
                Ok(false) => {}
                Err(error) => failures.push(format!("'{}': {error}", pane.title)),
            }
        }
        if !failures.is_empty() {
            anyhow::bail!("Could not end all sessions: {}", failures.join("; "));
        }
        self.panes.clear();
        self.focused = None;
        self.split = None;
        Ok(terminated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    /// A multiplexer holding real, idle panes `1..=count`, focused on the first.
    fn mux_with_panes(count: PaneId) -> MuxState {
        let mut mux = MuxState::new(KeyChord::parse("C-b").unwrap());
        for id in 1..=count {
            let (program, args) = if cfg!(windows) {
                (
                    "cmd.exe".to_string(),
                    vec!["/c".to_string(), "ping -n 30 127.0.0.1 >nul".to_string()],
                )
            } else {
                (
                    "/bin/sh".to_string(),
                    vec!["-c".to_string(), "sleep 30".to_string()],
                )
            };
            let pane = Pane::spawn(
                PaneSpec {
                    id,
                    title: format!("Session {id}"),
                    cwd: std::env::temp_dir(),
                    session_id: format!("session-{id}"),
                    program,
                    args,
                    events_path: None,
                    terminal_light_mode: Some(false),
                    hooks_active: false,
                },
                24,
                80,
                mux.events.clone(),
            )
            .unwrap();
            mux.panes.push(pane);
        }
        mux.focused = Some(1);
        mux
    }

    #[test]
    fn focusing_a_session_off_screen_swaps_it_into_the_focused_split_so_typing_stays_visible() {
        let mut mux = mux_with_panes(3);
        assert!(mux.split_with(SplitDirection::Columns, 2));
        assert_eq!(mux.focused, Some(2));

        mux.focus(3);
        assert_eq!(mux.focused, Some(3));
        assert_eq!(mux.split.as_ref().unwrap().slots, vec![1, 3]);

        // Focusing one already on screen only moves focus.
        mux.focus(1);
        assert_eq!(mux.split.as_ref().unwrap().slots, vec![1, 3]);
        assert_eq!(mux.focused, Some(1));

        // Cycling and jumping by number go the same way.
        mux.select_index(1);
        assert_eq!(mux.split.as_ref().unwrap().slots, vec![2, 3]);
        let _ = mux.shutdown();
    }

    #[test]
    fn ending_a_split_session_hands_focus_to_the_slot_that_closes_over_it() {
        let mut mux = mux_with_panes(3);
        mux.split_with(SplitDirection::Columns, 2);
        mux.split_with(SplitDirection::Columns, 3);
        assert_eq!(mux.split.as_ref().unwrap().slots, vec![1, 2, 3]);

        mux.focused = Some(2);
        mux.remove(2);
        assert_eq!(mux.split.as_ref().unwrap().slots, vec![1, 3]);
        assert_eq!(mux.focused, Some(3));

        mux.remove(3);
        assert!(mux.split.is_none(), "one session left is not a split");
        assert_eq!(mux.focused, Some(1));
        let _ = mux.shutdown();
    }

    #[test]
    fn breaking_a_session_out_of_a_split_keeps_its_tab_and_focuses_a_neighbour() {
        let mut mux = mux_with_panes(3);
        mux.split_with(SplitDirection::Rows, 2);
        mux.split_with(SplitDirection::Rows, 3);
        mux.focused = Some(2);

        assert!(mux.unsplit_focused());
        assert_eq!(mux.split.as_ref().unwrap().slots, vec![1, 3]);
        assert_eq!(mux.focused, Some(3));
        assert!(mux.pane(2).is_some(), "the session itself is untouched");

        assert!(mux.unsplit_focused());
        assert!(mux.split.is_none());
        assert!(!mux.unsplit_focused(), "nothing left to break out of");
        let _ = mux.shutdown();
    }

    #[test]
    fn splitting_the_other_way_turns_the_whole_layout_rather_than_nesting() {
        let mut mux = mux_with_panes(3);
        mux.split_with(SplitDirection::Columns, 2);
        mux.split_with(SplitDirection::Rows, 3);
        let split = mux.split.as_ref().unwrap();
        assert_eq!(split.direction, SplitDirection::Rows);
        assert_eq!(split.slots, vec![1, 2, 3]);
        assert!(
            !mux.split_with(SplitDirection::Rows, 1),
            "already on screen"
        );
        let _ = mux.shutdown();
    }

    #[test]
    fn arrows_move_between_splits_but_not_while_one_is_zoomed() {
        let mut mux = mux_with_panes(2);
        mux.split_with(SplitDirection::Columns, 2);
        assert!(mux.focus_split(SplitStep::Left));
        assert_eq!(mux.focused, Some(1));
        assert!(
            !mux.focus_split(SplitStep::Left),
            "no wrapping off the edge"
        );

        assert!(mux.toggle_split_zoom());
        assert!(mux.visible_split().is_none());
        assert!(
            !mux.focus_split(SplitStep::Right),
            "moving into a split nobody can see would type blind"
        );
        let _ = mux.shutdown();
    }

    #[test]
    fn a_saved_split_comes_back_by_session_even_though_pane_ids_change() {
        let mut before = mux_with_panes(3);
        before.split_with(SplitDirection::Columns, 3);
        before.split.as_mut().unwrap().weights = vec![150, 50];
        let saved = before.save_split().unwrap();
        assert_eq!(saved.session_ids, vec!["session-1", "session-3"]);
        let _ = before.shutdown();

        let mut after = mux_with_panes(3);
        after.focused = Some(2);
        after.restore_split(&saved);
        let split = after.split.as_ref().unwrap();
        assert_eq!(split.slots, vec![1, 3]);
        assert_eq!(split.weights, vec![150, 50]);
        assert_eq!(after.focused, Some(1), "focus has to land inside the split");
        let _ = after.shutdown();
    }

    #[test]
    fn split_keys_resolve_and_are_all_reserved_from_the_configurable_tmux_key() {
        let chord = KeyChord::parse("C-b").unwrap();
        let none = KeyModifiers::NONE;
        let cases = [
            (
                KeyCode::Char('|'),
                PrefixCommand::Split(SplitDirection::Columns),
            ),
            (
                KeyCode::Char('-'),
                PrefixCommand::Split(SplitDirection::Rows),
            ),
            (KeyCode::Char('z'), PrefixCommand::ZoomSplit),
            (KeyCode::Char('b'), PrefixCommand::UnsplitFocused),
            (KeyCode::Char('r'), PrefixCommand::ResizeSplit),
            (KeyCode::Left, PrefixCommand::FocusSplit(SplitStep::Left)),
            (KeyCode::Down, PrefixCommand::FocusSplit(SplitStep::Down)),
        ];
        for (code, command) in cases {
            assert_eq!(
                resolve_prefix_command(&key(code, none), &chord),
                Some(command)
            );
            if let KeyCode::Char(character) = code {
                assert!(PREFIX_COMMAND_KEYS.contains(&character), "{character}");
            }
        }
        // `|` usually needs Shift; the terminal reporting it must not stop it matching.
        assert_eq!(
            resolve_prefix_command(&key(KeyCode::Char('|'), KeyModifiers::SHIFT), &chord),
            Some(PrefixCommand::Split(SplitDirection::Columns))
        );
    }

    #[test]
    fn parses_common_chord_spellings() {
        let ctrl_b = KeyChord::parse("C-b").unwrap();
        assert_eq!(ctrl_b.code, KeyCode::Char('b'));
        assert_eq!(ctrl_b.modifiers, KeyModifiers::CONTROL);

        assert_eq!(KeyChord::parse("ctrl-b"), Some(ctrl_b));
        assert_eq!(KeyChord::parse("C-B"), KeyChord::parse("C-b"));
        assert_eq!(KeyChord::parse("C-Space").unwrap().code, KeyCode::Char(' '));
        assert_eq!(KeyChord::parse("M-x").unwrap().modifiers, KeyModifiers::ALT);
        assert_eq!(KeyChord::parse("F5").unwrap().code, KeyCode::F(5));
    }

    #[test]
    fn literal_shifted_character_chords_preserve_the_shifted_character() {
        let shifted = KeyChord::parse("S-x").unwrap().literal_key_event();
        assert_eq!(shifted.code, KeyCode::Char('X'));
        assert!(!shifted.modifiers.contains(KeyModifiers::SHIFT));

        let alt_shifted = KeyChord::parse("M-S-1").unwrap().literal_key_event();
        assert_eq!(alt_shifted.code, KeyCode::Char('!'));
        assert_eq!(alt_shifted.modifiers, KeyModifiers::ALT);
    }

    #[test]
    fn rejects_chords_that_would_swallow_typing() {
        assert!(KeyChord::parse("b").is_none());
        assert!(KeyChord::parse("").is_none());
        assert!(KeyChord::parse("C-").is_none());
        assert!(KeyChord::parse("C-nope").is_none());
        assert!(KeyChord::parse("F42").is_none());
    }

    #[test]
    fn matching_ignores_case_and_irrelevant_modifiers() {
        let chord = KeyChord::parse("C-b").unwrap();
        assert!(chord.matches(&key(KeyCode::Char('b'), KeyModifiers::CONTROL)));
        assert!(chord.matches(&key(KeyCode::Char('B'), KeyModifiers::CONTROL)));
        assert!(!chord.matches(&key(KeyCode::Char('b'), KeyModifiers::NONE)));
        assert!(!chord.matches(&key(
            KeyCode::Char('b'),
            KeyModifiers::CONTROL | KeyModifiers::ALT
        )));
    }

    #[test]
    fn label_round_trips() {
        for text in ["C-b", "M-x", "C-Space"] {
            let chord = KeyChord::parse(text).unwrap();
            assert_eq!(KeyChord::parse(&chord.label()), Some(chord));
        }
    }

    #[test]
    fn double_prefix_sends_a_literal() {
        let chord = KeyChord::parse("C-b").unwrap();
        let command =
            resolve_prefix_command(&key(KeyCode::Char('b'), KeyModifiers::CONTROL), &chord);
        assert_eq!(command, Some(PrefixCommand::CommandPalette));
    }

    #[test]
    fn prefix_commands_map_to_actions() {
        let chord = KeyChord::parse("C-b").unwrap();
        let none = KeyModifiers::NONE;
        assert_eq!(
            resolve_prefix_command(&key(KeyCode::Char('d'), none), &chord),
            Some(PrefixCommand::Detach)
        );
        assert_eq!(
            resolve_prefix_command(&key(KeyCode::Char('x'), none), &chord),
            Some(PrefixCommand::KillPane)
        );
        assert_eq!(
            resolve_prefix_command(&key(KeyCode::Char('c'), none), &chord),
            Some(PrefixCommand::Chat)
        );
        assert_eq!(
            resolve_prefix_command(&key(KeyCode::Char('e'), none), &chord),
            Some(PrefixCommand::Scratchpad)
        );
        assert_eq!(
            resolve_prefix_command(&key(KeyCode::Char('t'), none), &chord),
            Some(PrefixCommand::Terminal)
        );
        assert_eq!(
            resolve_prefix_command(&key(KeyCode::Char('s'), none), &chord),
            Some(PrefixCommand::Snippets)
        );
        assert_eq!(
            resolve_prefix_command(&key(KeyCode::Char('m'), none), &chord),
            Some(PrefixCommand::MoveTab)
        );
        assert_eq!(
            resolve_prefix_command(&key(KeyCode::Char('M'), KeyModifiers::SHIFT), &chord),
            None
        );
        assert_eq!(
            resolve_prefix_command(&key(KeyCode::Char('u'), none), &chord),
            Some(PrefixCommand::Update)
        );
        assert_eq!(
            resolve_prefix_command(&key(KeyCode::Char('h'), KeyModifiers::CONTROL), &chord),
            Some(PrefixCommand::Help)
        );
        assert_eq!(
            resolve_prefix_command(&key(KeyCode::Char('g'), KeyModifiers::CONTROL), &chord),
            Some(PrefixCommand::Github)
        );
        assert_eq!(
            resolve_prefix_command(&key(KeyCode::Char('h'), none), &chord),
            Some(PrefixCommand::Help)
        );
        assert_eq!(
            resolve_prefix_command(&key(KeyCode::Char('g'), none), &chord),
            Some(PrefixCommand::Github)
        );
        assert_eq!(
            resolve_prefix_command(&key(KeyCode::Char('q'), none), &chord),
            Some(PrefixCommand::Quit)
        );
        assert_eq!(
            resolve_prefix_command(&key(KeyCode::Backspace, none), &chord),
            Some(PrefixCommand::Help)
        );
        assert_eq!(
            resolve_prefix_command(&key(KeyCode::Char('2'), none), &chord),
            Some(PrefixCommand::SelectIndex(2))
        );
        assert_eq!(
            resolve_prefix_command(&key(KeyCode::Char('y'), none), &chord),
            None
        );
    }

    #[test]
    fn persistent_end_prefix_follows_configured_key_without_shadowing_move_mode() {
        let chord = KeyChord::parse("C-b").unwrap();
        let keys = crate::config::TmuxKeyConfig {
            resume_session: "t".to_string(),
            new_session: "v".to_string(),
            new_worktree: "V".to_string(),
            end_session: "!".to_string(),
        };

        assert_eq!(
            resolve_prefix_command_with_tmux_keys(
                &key(KeyCode::Char('v'), KeyModifiers::NONE),
                &chord,
                Some(&keys),
            ),
            None
        );
        assert_eq!(
            resolve_prefix_command_with_tmux_keys(
                &key(KeyCode::Char('V'), KeyModifiers::SHIFT),
                &chord,
                Some(&keys),
            ),
            None
        );
        assert_eq!(
            resolve_prefix_command_with_tmux_keys(
                &key(KeyCode::Char('!'), KeyModifiers::SHIFT),
                &chord,
                Some(&keys),
            ),
            Some(PrefixCommand::EndPersistentSession)
        );
        assert_eq!(
            resolve_prefix_command_with_tmux_keys(
                &key(KeyCode::Char('m'), KeyModifiers::NONE),
                &chord,
                Some(&keys),
            ),
            Some(PrefixCommand::MoveTab)
        );
    }

    #[test]
    fn end_prefix_stays_inert_where_tmux_sessions_cannot_work() {
        let chord = KeyChord::parse("C-b").unwrap();

        assert_eq!(
            resolve_prefix_command_with_tmux_keys(
                &key(KeyCode::Char('X'), KeyModifiers::SHIFT),
                &chord,
                None,
            ),
            None
        );
    }

    #[test]
    fn group_namespaces_remain_reachable_when_control_key_is_the_prefix() {
        let prefix = KeyChord::parse("C-g").unwrap();
        assert_eq!(
            resolve_prefix_command(&key(KeyCode::Char('g'), KeyModifiers::CONTROL), &prefix),
            Some(PrefixCommand::CommandPalette)
        );
        assert_eq!(
            resolve_prefix_command(&key(KeyCode::Char('g'), KeyModifiers::NONE), &prefix),
            Some(PrefixCommand::Github)
        );
        assert_eq!(
            resolve_prefix_command(&key(KeyCode::Char('h'), KeyModifiers::NONE), &prefix),
            Some(PrefixCommand::Help)
        );
    }

    #[test]
    fn help_commands_map_to_topics() {
        assert_eq!(
            resolve_help_command(&key(KeyCode::Char('e'), KeyModifiers::NONE)),
            Some(HelpCommand::Scratchpad)
        );
        assert_eq!(
            resolve_help_command(&key(KeyCode::Esc, KeyModifiers::NONE)),
            Some(HelpCommand::Cancel)
        );
    }

    #[test]
    fn github_commands_map_to_inspection() {
        assert_eq!(
            resolve_github_command(&key(KeyCode::Char('i'), KeyModifiers::NONE)),
            Some(GithubCommand::Inspect)
        );
        assert_eq!(
            resolve_github_command(&key(KeyCode::Esc, KeyModifiers::NONE)),
            Some(GithubCommand::Cancel)
        );
    }
}
