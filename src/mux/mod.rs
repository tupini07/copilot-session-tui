pub mod callbacks;
pub mod keys;
pub mod pane;
pub mod pty;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

pub use pane::{Pane, PaneId, PaneSpec, PaneStatus};

/// Events the UI loop must wake up for, from PTYs and from the terminal.
pub enum MuxEvent {
    Output(PaneId),
    Exited(PaneId, Option<u32>),
    HostSequence(Vec<u8>),
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
    PaneList,
    Chat,
    Scratchpad,
    Terminal,
    Snippets,
    Help,
    Github,
    /// `prefix q` — end the focused session and CST together.
    Quit,
    SelectIndex(usize),
    /// `prefix prefix` — send a literal prefix keystroke to the child.
    Literal,
    Cancel,
}

pub fn resolve_prefix_command(key: &KeyEvent, prefix: &KeyChord) -> Option<PrefixCommand> {
    if prefix.matches(key) {
        return Some(PrefixCommand::Literal);
    }
    match key.code {
        KeyCode::Char('d') => Some(PrefixCommand::Detach),
        KeyCode::Char('n') => Some(PrefixCommand::NextPane),
        KeyCode::Char('p') => Some(PrefixCommand::PreviousPane),
        KeyCode::Char('x') => Some(PrefixCommand::KillPane),
        KeyCode::Char('w') => Some(PrefixCommand::PaneList),
        KeyCode::Char('c') => Some(PrefixCommand::Chat),
        KeyCode::Char('e') => Some(PrefixCommand::Scratchpad),
        KeyCode::Char('t') => Some(PrefixCommand::Terminal),
        KeyCode::Char('s') => Some(PrefixCommand::Snippets),
        KeyCode::Char('q') => Some(PrefixCommand::Quit),
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
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefixState {
    Idle,
    Root,
    Help,
    Github,
}

/// All panes owned by this CST instance.
pub struct MuxState {
    pub panes: Vec<Pane>,
    pub focused: Option<PaneId>,
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
        self.focused = Some(id);
        id
    }

    pub fn remove(&mut self, id: PaneId) {
        if let Some(index) = self.panes.iter().position(|pane| pane.id == id) {
            let pane = self.panes.remove(index);
            let _ = pane.kill();
            if self.focused == Some(id) {
                self.focused = self
                    .panes
                    .get(index)
                    .or_else(|| self.panes.last())
                    .map(|pane| pane.id);
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
            self.focused = self.panes.first().map(|pane| pane.id);
            return;
        };
        let len = self.panes.len();
        let next = if forward {
            (current + 1) % len
        } else {
            (current + len - 1) % len
        };
        self.focused = Some(self.panes[next].id);
    }

    pub fn select_index(&mut self, index: usize) {
        if let Some(pane) = self.panes.get(index) {
            self.focused = Some(pane.id);
        }
    }

    pub fn running_count(&self) -> usize {
        self.panes.iter().filter(|pane| pane.is_running()).count()
    }

    /// Running panes other than the focused one — the sessions a quit would end
    /// without the user seeing them go.
    pub fn background_running_count(&self) -> usize {
        self.panes
            .iter()
            .filter(|pane| pane.is_running() && Some(pane.id) != self.focused)
            .count()
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

    pub fn resize_all_at(&mut self, x: u16, y: u16, rows: u16, cols: u16) {
        for pane in &mut self.panes {
            let _ = pane.resize_at(x, y, rows, cols);
        }
    }

    pub fn shutdown(&mut self) -> Result<()> {
        for pane in &self.panes {
            let _ = pane.kill();
        }
        self.panes.clear();
        self.focused = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
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
        assert_eq!(command, Some(PrefixCommand::Literal));
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
            resolve_prefix_command(&key(KeyCode::Char('h'), KeyModifiers::CONTROL), &chord),
            Some(PrefixCommand::Help)
        );
        assert_eq!(
            resolve_prefix_command(&key(KeyCode::Char('g'), KeyModifiers::CONTROL), &chord),
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
            resolve_prefix_command(&key(KeyCode::Char('z'), none), &chord),
            None
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
