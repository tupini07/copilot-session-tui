use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::events::lifecycle::{InputKind, LifecycleEvent, LifecycleMonitor};

use super::callbacks::{PaneCallbacks, PaneSignalEvent, PaneSignals};
use super::keys;
use super::pty::{pty_size, PtyChunk, PtySession};
use super::MuxEvent;

pub type PaneId = u64;

type PaneParser = Arc<Mutex<vt100::Parser<PaneCallbacks>>>;

#[derive(Debug, Clone, Copy)]
struct Viewport {
    x: u16,
    y: u16,
    rows: u16,
    cols: u16,
}

impl Viewport {
    fn coordinates(self, column: u16, row: u16) -> Option<(u16, u16)> {
        let column = column.checked_sub(self.x)?;
        let row = row.checked_sub(self.y)?;
        (column < self.cols && row < self.rows).then_some((column + 1, row + 1))
    }

    fn clamped_coordinates(self, column: u16, row: u16) -> (u16, u16) {
        let max_column = self.x.saturating_add(self.cols.saturating_sub(1));
        let max_row = self.y.saturating_add(self.rows.saturating_sub(1));
        (
            column.clamp(self.x, max_column) - self.x + 1,
            row.clamp(self.y, max_row) - self.y + 1,
        )
    }

    fn cell_coordinates(self, column: u16, row: u16) -> Option<(u16, u16)> {
        let column = column.checked_sub(self.x)?;
        let row = row.checked_sub(self.y)?;
        (column < self.cols && row < self.rows).then_some((row, column))
    }
}

/// Why a pane stopped running, if it has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneStatus {
    Running,
    Exited(Option<u32>),
}

/// One Copilot session hosted inside CST: a PTY plus the terminal state it drives.
pub struct Pane {
    pub id: PaneId,
    /// Copilot session id. Known before the child starts: resumes are given one and new
    /// sessions are told theirs via `--session-id`, so a pane can always key per-session
    /// state (scratchpad, terminal, panel layout) on something stable and unique.
    pub session_id: String,
    pub title: String,
    pub cwd: PathBuf,
    pub status: PaneStatus,
    /// When the child was spawned, so the UI can say how long a slow start has taken.
    pub started_at: Instant,
    parser: PaneParser,
    has_visible_output: Arc<AtomicBool>,
    _lifecycle_monitor: Option<LifecycleMonitor>,
    pty: PtySession,
    mouse_captured: bool,
    viewport: Viewport,
    working: bool,
    progress_state: crate::host_terminal::ProgressState,
    pending_inputs: Vec<(String, InputKind)>,
    needs_attention: bool,
    ready_sent_in_cycle: bool,
    error_sent_in_cycle: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PaneSignalOutcome {
    pub bell: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneNotification {
    Ready,
    Question,
    PlanApproval,
    Error,
}

/// Everything needed to start a pane, grouped so callers don't juggle a long argument list.
pub struct PaneSpec {
    pub id: PaneId,
    pub title: String,
    pub cwd: PathBuf,
    pub session_id: String,
    pub program: String,
    pub args: Vec<String>,
    pub events_path: Option<PathBuf>,
}

impl Pane {
    pub fn spawn(spec: PaneSpec, rows: u16, cols: u16, events: Sender<MuxEvent>) -> Result<Self> {
        let PaneSpec {
            id,
            title,
            cwd,
            session_id,
            program,
            args,
            events_path,
        } = spec;
        let size = pty_size(rows, cols);
        let lifecycle_baseline = events_path
            .as_deref()
            .map(crate::events::lifecycle::capture_file_baseline)
            .transpose()?;

        let (chunk_tx, chunk_rx) = std::sync::mpsc::channel();
        let pty = PtySession::spawn(&program, &args, Some(&cwd), size, chunk_tx)?;

        // Device-status replies must reach the child, or ConPTY stalls on startup.
        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            size.rows,
            size.cols,
            // Scrollback keeps output above the viewport reachable once copy-mode lands.
            5000,
            PaneCallbacks::new(id, pty.writer_handle(), events.clone()),
        )));

        let pump_parser = Arc::clone(&parser);
        let has_visible_output = Arc::new(AtomicBool::new(false));
        let pump_has_visible_output = Arc::clone(&has_visible_output);
        let lifecycle_events = events.clone();
        std::thread::spawn(move || {
            while let Ok(chunk) = chunk_rx.recv() {
                match chunk {
                    PtyChunk::Output(bytes) => {
                        let signals = if let Ok(mut parser) = pump_parser.lock() {
                            parser.process(&bytes);
                            if !pump_has_visible_output.load(Ordering::Relaxed)
                                && Self::screen_has_visible_text(parser.screen())
                            {
                                pump_has_visible_output.store(true, Ordering::Release);
                            }
                            parser.callbacks_mut().take_signals()
                        } else {
                            PaneSignals::default()
                        };
                        // The UI thread coalesces these; one wakeup per chunk is fine.
                        if events.send(MuxEvent::Output(id, signals)).is_err() {
                            return;
                        }
                    }
                    PtyChunk::Exited(code) => {
                        let _ = events.send(MuxEvent::Exited(id, code));
                        return;
                    }
                }
            }
        });
        let lifecycle_monitor = events_path.zip(lifecycle_baseline).map(|(path, baseline)| {
            LifecycleMonitor::start_from_baseline(path, baseline, move |event| {
                lifecycle_events
                    .send(MuxEvent::SessionLifecycle(id, event))
                    .is_ok()
            })
        });

        Ok(Self {
            id,
            session_id,
            title,
            cwd,
            status: PaneStatus::Running,
            started_at: Instant::now(),
            parser,
            has_visible_output,
            _lifecycle_monitor: lifecycle_monitor,
            pty,
            mouse_captured: false,
            viewport: Viewport {
                x: 0,
                y: 0,
                rows: size.rows,
                cols: size.cols,
            },
            working: false,
            progress_state: crate::host_terminal::ProgressState::Clear,
            pending_inputs: Vec::new(),
            needs_attention: false,
            ready_sent_in_cycle: false,
            error_sent_in_cycle: false,
        })
    }

    pub fn is_running(&self) -> bool {
        self.status == PaneStatus::Running
    }

    pub fn is_working(&self) -> bool {
        self.working
    }

    pub fn effective_progress_state(&self) -> crate::host_terminal::ProgressState {
        if self.requires_user_action() {
            crate::host_terminal::ProgressState::Clear
        } else {
            self.progress_state
        }
    }

    pub fn record_progress_state(&mut self, state: crate::host_terminal::ProgressState) {
        self.progress_state = state;
    }

    /// True while the child has produced nothing visible yet.
    ///
    /// Copilot takes a few seconds to draw its first frame, and ConPTY emits control
    /// sequences long before any text, so "has the screen got anything on it" is a more
    /// honest readiness signal than "have we received bytes".
    pub fn is_blank(&self) -> bool {
        !self.has_visible_output.load(Ordering::Acquire)
    }

    /// Feed bytes straight into the parser, standing in for child output in tests
    /// and in the generated documentation screenshots.
    #[cfg(any(test, feature = "screenshots"))]
    pub fn feed_synthetic(&mut self, bytes: &[u8]) {
        if let Ok(mut parser) = self.parser.lock() {
            parser.process(bytes);
            if Self::screen_has_visible_text(parser.screen()) {
                self.has_visible_output.store(true, Ordering::Release);
            }
        }
    }

    fn screen_has_visible_text(screen: &vt100::Screen) -> bool {
        let (rows, cols) = screen.size();
        (0..rows).any(|row| {
            (0..cols).any(|col| {
                screen.cell(row, col).is_some_and(|cell| {
                    cell.contents()
                        .chars()
                        .any(|character| !character.is_whitespace())
                })
            })
        })
    }

    pub fn mark_exited(&mut self, code: Option<u32>) {
        self.status = PaneStatus::Exited(code);
        self._lifecycle_monitor.take();
        self.working = false;
        self.progress_state = crate::host_terminal::ProgressState::Clear;
        self.pending_inputs.clear();
    }

    /// Run `f` against the current screen. The parser lock is held for the call, so
    /// keep it to rendering and cheap queries.
    pub fn with_screen<T>(&self, f: impl FnOnce(&vt100::Screen) -> T) -> Option<T> {
        let parser = self.parser.lock().ok()?;
        Some(f(parser.screen()))
    }

    pub fn application_cursor(&self) -> bool {
        self.with_screen(|screen| screen.application_cursor())
            .unwrap_or(false)
    }

    pub fn bracketed_paste(&self) -> bool {
        self.with_screen(|screen| screen.bracketed_paste())
            .unwrap_or(false)
    }

    /// GitHub issue/PR reference under an outer-terminal coordinate, if any.
    pub fn github_reference_at(&self, column: u16, row: u16) -> Option<u64> {
        let (row, column) = self.viewport.cell_coordinates(column, row)?;
        self.with_screen(|screen| github_reference_at(screen, row, column))
            .flatten()
    }

    /// Every `#1234` currently on the pane's screen.
    ///
    /// Used to look up what those numbers point at, so they can be decorated.
    pub fn github_references(&self) -> Vec<u64> {
        self.with_screen(github_references).unwrap_or_default()
    }

    /// Cursor position and visibility, for mirroring into the outer terminal.
    pub fn cursor(&self) -> Option<(u16, u16)> {
        self.with_screen(|screen| {
            if screen.hide_cursor() {
                None
            } else {
                Some(screen.cursor_position())
            }
        })
        .flatten()
    }

    /// Apply title/progress/bell signals captured from one exact PTY output chunk.
    pub fn apply_signals(
        &mut self,
        signals: PaneSignals,
        attended: bool,
    ) -> (PaneSignalOutcome, Vec<PaneNotification>) {
        let mut outcome = PaneSignalOutcome::default();
        let mut notifications = Vec::new();
        if let Some(title) = signals.title {
            let title = title.trim().to_string();
            if !title.is_empty() {
                self.title = title;
            }
        }

        for event in signals.events {
            match event {
                PaneSignalEvent::Progress(progress) if progress.is_working() => {
                    self.progress_state = progress;
                    if !self.working {
                        self.ready_sent_in_cycle = false;
                        self.error_sent_in_cycle = false;
                    }
                    self.working = true;
                    if !self.requires_user_action() {
                        self.needs_attention = false;
                    }
                    if progress == crate::host_terminal::ProgressState::Error
                        && !self.error_sent_in_cycle
                    {
                        notifications.push(PaneNotification::Error);
                        self.error_sent_in_cycle = true;
                    }
                }
                PaneSignalEvent::Progress(_) if self.working => {
                    self.progress_state = crate::host_terminal::ProgressState::Clear;
                    self.working = false;
                    if !self.requires_user_action() {
                        self.needs_attention = !attended;
                    }
                    if !self.requires_user_action()
                        && !self.error_sent_in_cycle
                        && !self.ready_sent_in_cycle
                    {
                        notifications.push(PaneNotification::Ready);
                        self.ready_sent_in_cycle = true;
                    }
                }
                PaneSignalEvent::Progress(_) => {
                    self.progress_state = crate::host_terminal::ProgressState::Clear;
                }
                PaneSignalEvent::Bell => {
                    outcome.bell = true;
                    if !attended && !self.requires_user_action() {
                        self.needs_attention = true;
                    }
                    if !self.requires_user_action()
                        && !self.error_sent_in_cycle
                        && !self.ready_sent_in_cycle
                    {
                        notifications.push(PaneNotification::Ready);
                        self.ready_sent_in_cycle = true;
                    }
                }
            }
        }
        (outcome, notifications)
    }

    pub fn apply_lifecycle(&mut self, event: LifecycleEvent) -> Option<PaneNotification> {
        match event {
            LifecycleEvent::InputRequested { tool_call_id, kind } => {
                if self
                    .pending_inputs
                    .iter()
                    .any(|(pending_id, _)| pending_id == &tool_call_id)
                {
                    return None;
                }
                self.pending_inputs.push((tool_call_id, kind));
                self.ready_sent_in_cycle = true;
                Some(match kind {
                    InputKind::Question => PaneNotification::Question,
                    InputKind::PlanApproval => PaneNotification::PlanApproval,
                })
            }
            LifecycleEvent::InputResolved { tool_call_id, kind } => {
                let previous_len = self.pending_inputs.len();
                self.pending_inputs.retain(|(pending_id, pending_kind)| {
                    pending_id != &tool_call_id || *pending_kind != kind
                });
                if previous_len != self.pending_inputs.len() && self.pending_inputs.is_empty() {
                    self.needs_attention = false;
                    self.ready_sent_in_cycle = false;
                    self.error_sent_in_cycle = false;
                }
                None
            }
            LifecycleEvent::Reset => {
                if !self.pending_inputs.is_empty() {
                    self.pending_inputs.clear();
                    self.needs_attention = false;
                    self.ready_sent_in_cycle = false;
                    self.error_sent_in_cycle = false;
                }
                None
            }
        }
    }

    #[cfg(test)]
    pub fn refresh_from_callbacks(
        &mut self,
        attended: bool,
    ) -> (PaneSignalOutcome, Vec<PaneNotification>) {
        let signals = self
            .parser
            .lock()
            .map(|mut parser| parser.callbacks_mut().take_signals())
            .unwrap_or_default();
        self.apply_signals(signals, attended)
    }

    pub fn needs_attention(&self) -> bool {
        self.needs_attention || self.requires_user_action()
    }

    pub fn requires_user_action(&self) -> bool {
        !self.pending_inputs.is_empty()
    }

    pub fn acknowledge_attention(&mut self) {
        self.needs_attention = false;
    }

    pub fn display_title(&self) -> String {
        if self.needs_attention() {
            format!("? {}", self.title)
        } else {
            self.title.clone()
        }
    }

    pub fn send_key(&mut self, key: &KeyEvent) -> Result<()> {
        if !self.is_running() {
            return Ok(());
        }
        // DECCKM only changes cursor/navigation key encodings. Reading it for every
        // ordinary character needlessly contended with the parser thread that is
        // producing Copilot's echo.
        let application_cursor = if matches!(
            key.code,
            KeyCode::Up
                | KeyCode::Down
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Home
                | KeyCode::End
        ) {
            self.application_cursor()
        } else {
            false
        };
        if let Some(bytes) = keys::encode(key, application_cursor) {
            self.pty.write(&bytes)?;
        }
        Ok(())
    }

    pub fn send_paste(&mut self, text: &str) -> Result<()> {
        if !self.is_running() {
            return Ok(());
        }
        let bytes = keys::encode_paste(text, self.bracketed_paste());
        self.pty.write(&bytes)
    }

    /// Paste a snippet without allowing a line break to become an Enter keypress.
    pub fn send_prompt_snippet(&mut self, text: &str) -> Result<()> {
        if !self.is_running() {
            anyhow::bail!("the focused session is no longer running");
        }
        if text.is_empty() {
            anyhow::bail!("the snippet prompt is empty");
        }
        if text
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\r' | '\n' | '\t'))
        {
            anyhow::bail!("snippet prompts cannot contain terminal control characters");
        }
        let bracketed = self.bracketed_paste();
        if (text.contains('\r') || text.contains('\n')) && !bracketed {
            anyhow::bail!(
                "Copilot is not ready for multiline paste yet; wait for startup to finish"
            );
        }
        let bytes = keys::encode_paste(text, bracketed);
        self.pty.write(&bytes)
    }

    pub fn handle_mouse(&mut self, event: MouseEvent) -> Result<()> {
        if !self.is_running() {
            return Ok(());
        }
        let Some((mode, encoding)) = self.with_screen(|screen| {
            (
                screen.mouse_protocol_mode(),
                screen.mouse_protocol_encoding(),
            )
        }) else {
            return Ok(());
        };
        let coordinates = self.viewport.coordinates(event.column, event.row);

        if mode == vt100::MouseProtocolMode::None {
            if coordinates.is_some() {
                match event.kind {
                    MouseEventKind::ScrollUp => self.scroll(3),
                    MouseEventKind::ScrollDown => self.scroll(-3),
                    _ => Ok(()),
                }
            } else {
                Ok(())
            }
        } else {
            if matches!(event.kind, MouseEventKind::Down(_)) && coordinates.is_some() {
                self.mouse_captured = true;
            }
            let coordinates = coordinates.or_else(|| {
                (self.mouse_captured
                    && matches!(event.kind, MouseEventKind::Drag(_) | MouseEventKind::Up(_)))
                .then(|| self.viewport.clamped_coordinates(event.column, event.row))
            });
            let sequence = coordinates
                .and_then(|position| keys::encode_mouse(event, position, mode, encoding));
            if matches!(event.kind, MouseEventKind::Up(_)) {
                self.mouse_captured = false;
            }
            if let Some(sequence) = sequence {
                self.pty.write(&sequence)?;
            }
            Ok(())
        }
    }

    fn scroll(&mut self, rows: isize) -> Result<()> {
        let mut parser = self
            .parser
            .lock()
            .map_err(|_| anyhow::anyhow!("Terminal parser lock was poisoned"))?;
        let current = parser.screen().scrollback();
        let next = if rows.is_negative() {
            current.saturating_sub(rows.unsigned_abs())
        } else {
            current.saturating_add(rows as usize)
        };
        parser.screen_mut().set_scrollback(next);
        Ok(())
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        self.resize_at(0, 0, rows, cols)
    }

    pub fn resize_at(&mut self, x: u16, y: u16, rows: u16, cols: u16) -> Result<()> {
        let size = pty_size(rows, cols);
        if let Ok(mut parser) = self.parser.lock() {
            parser.screen_mut().set_size(size.rows, size.cols);
        }
        self.pty.resize(size)?;
        self.viewport = Viewport {
            x,
            y,
            rows: size.rows,
            cols: size.cols,
        };
        Ok(())
    }

    pub fn kill(&self) -> Result<()> {
        self.pty.kill()
    }

    pub fn shutdown(&mut self) -> Result<()> {
        let code = self
            .pty
            .terminate_and_wait(std::time::Duration::from_secs(3))?;
        self.mark_exited(code);
        Ok(())
    }

    /// Pick up an exit that happened without the reader thread noticing yet.
    pub fn poll_exit(&mut self) {
        if self.is_running() {
            if let Some(code) = self.pty.try_wait() {
                self.mark_exited(Some(code));
            }
        }
    }
}

/// Text of one screen row, as characters, for reference scanning.
fn row_characters(screen: &vt100::Screen, row: u16, cols: u16) -> Vec<char> {
    (0..cols)
        .map(|col| {
            screen
                .cell(row, col)
                .and_then(|cell| cell.contents().chars().next())
                .unwrap_or(' ')
        })
        .collect()
}

/// Every distinct `#1234` on screen, in no particular order.
fn github_references(screen: &vt100::Screen) -> Vec<u64> {
    let (rows, cols) = screen.size();
    let mut found = std::collections::BTreeSet::new();
    for row in 0..rows {
        let characters = row_characters(screen, row, cols);
        for (index, character) in characters.iter().enumerate() {
            if *character != '#' {
                continue;
            }
            let start = index + 1;
            let mut end = start;
            while characters.get(end).is_some_and(char::is_ascii_digit) {
                end += 1;
            }
            if end == start {
                continue;
            }
            // A digit immediately before the hash means this is part of a
            // longer token such as a colour code, not a reference.
            if index > 0 && characters[index - 1].is_ascii_alphanumeric() {
                continue;
            }
            if let Ok(number) = characters[start..end]
                .iter()
                .collect::<String>()
                .parse::<u64>()
            {
                if number > 0 {
                    found.insert(number);
                }
            }
        }
    }
    found.into_iter().collect()
}

fn github_reference_at(screen: &vt100::Screen, row: u16, column: u16) -> Option<u64> {
    let (_, cols) = screen.size();
    let characters = row_characters(screen, row, cols);
    let clicked = *characters.get(column as usize)?;
    if clicked != '#' && !clicked.is_ascii_digit() {
        return None;
    }

    let mut hash = column as usize;
    if clicked.is_ascii_digit() {
        while hash > 0 && characters[hash - 1].is_ascii_digit() {
            hash -= 1;
        }
        hash = hash.checked_sub(1)?;
    }
    if characters.get(hash) != Some(&'#') {
        return None;
    }

    let start = hash + 1;
    let mut end = start;
    while characters.get(end).is_some_and(char::is_ascii_digit) {
        end += 1;
    }
    if end == start || column as usize >= end {
        return None;
    }
    characters[start..end]
        .iter()
        .collect::<String>()
        .parse::<u64>()
        .ok()
        .filter(|number| *number > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};
    use std::io::Write;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    #[test]
    fn viewport_translates_outer_mouse_coordinates() {
        let viewport = Viewport {
            x: 2,
            y: 1,
            rows: 10,
            cols: 20,
        };

        assert_eq!(viewport.coordinates(2, 1), Some((1, 1)));
        assert_eq!(viewport.coordinates(21, 10), Some((20, 10)));
        assert_eq!(viewport.coordinates(1, 1), None);
        assert_eq!(viewport.clamped_coordinates(99, 99), (20, 10));
        assert_eq!(viewport.cell_coordinates(2, 1), Some((0, 0)));
        assert_eq!(viewport.cell_coordinates(21, 10), Some((9, 19)));
    }

    #[test]
    fn github_reference_is_detected_on_hash_or_digits() {
        let mut parser = vt100::Parser::new(4, 40, 0);
        parser.process(b"Findings posted to #2029 and #77.");
        let screen = parser.screen();

        for column in 19..=23 {
            assert_eq!(github_reference_at(screen, 0, column), Some(2029));
        }
        assert_eq!(github_reference_at(screen, 0, 18), None);
        assert_eq!(github_reference_at(screen, 0, 24), None);
        assert_eq!(github_reference_at(screen, 0, 30), Some(77));
    }

    #[test]
    fn ordinary_numbers_are_not_github_references() {
        let mut parser = vt100::Parser::new(2, 30, 0);
        parser.process(b"room 376 still 126");

        assert_eq!(github_reference_at(parser.screen(), 0, 6), None);
        assert_eq!(github_reference_at(parser.screen(), 0, 16), None);
    }

    #[test]
    fn screen_scan_finds_every_reference_once() {
        let mut parser = vt100::Parser::new(4, 40, 0);
        parser.process(b"Opened #2029, see #77 and #2029 again.\r\nColour #ff00aa is not one.");

        let mut found = github_references(parser.screen());
        found.sort_unstable();

        // `#ff00aa` yields no digits before the letters, so it never becomes a
        // reference; the duplicate is asked about only once.
        assert_eq!(found, vec![77, 2029]);
    }

    #[test]
    fn screen_scan_ignores_hashes_glued_to_a_word() {
        let mut parser = vt100::Parser::new(2, 40, 0);
        parser.process(b"abc#12 x#9");

        assert!(github_references(parser.screen()).is_empty());
    }

    fn shell_command(script: &str) -> (String, Vec<String>) {
        if cfg!(windows) {
            (
                "cmd.exe".to_string(),
                vec!["/c".to_string(), script.to_string()],
            )
        } else {
            (
                "/bin/sh".to_string(),
                vec!["-c".to_string(), script.to_string()],
            )
        }
    }

    fn test_spec(id: PaneId, program: String, args: Vec<String>) -> PaneSpec {
        PaneSpec {
            id,
            title: "test".to_string(),
            cwd: std::env::temp_dir(),
            session_id: format!("test-session-{id}"),
            program,
            args,
            events_path: None,
        }
    }

    fn wait_for_exit(rx: &mpsc::Receiver<MuxEvent>, pane: &mut Pane) -> Option<u32> {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(MuxEvent::Exited(_, code)) => {
                    pane.mark_exited(code);
                    return code;
                }
                Ok(_) => continue,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        None
    }

    fn wait_for_lifecycle(rx: &mpsc::Receiver<MuxEvent>) -> LifecycleEvent {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(MuxEvent::SessionLifecycle(_, event)) => return event,
                Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        panic!("timed out waiting for session lifecycle event");
    }

    #[test]
    fn pane_monitor_forwards_lifecycle_appends_after_spawn() {
        let directory = tempfile::tempdir().unwrap();
        let events_path = directory.path().join("events.jsonl");
        std::fs::write(&events_path, "{\"type\":\"session.start\"}\n").unwrap();
        let (tx, rx) = mpsc::channel();
        let (program, args) = shell_command(if cfg!(windows) {
            "ping -n 30 127.0.0.1 > nul"
        } else {
            "sleep 30"
        });
        let mut spec = test_spec(39, program, args);
        spec.events_path = Some(events_path.clone());
        let mut pane = Pane::spawn(spec, 24, 80, tx).unwrap();

        let mut events = std::fs::OpenOptions::new()
            .append(true)
            .open(&events_path)
            .unwrap();
        writeln!(
            events,
            r#"{{"type":"tool.execution_start","data":{{"toolName":"ask_user","toolCallId":"question-1"}}}}"#
        )
        .unwrap();
        events.flush().unwrap();
        assert_eq!(
            wait_for_lifecycle(&rx),
            LifecycleEvent::InputRequested {
                tool_call_id: "question-1".into(),
                kind: InputKind::Question,
            }
        );

        writeln!(
            events,
            r#"{{"type":"tool.execution_complete","data":{{"toolCallId":"question-1","success":true}}}}"#
        )
        .unwrap();
        events.flush().unwrap();
        assert_eq!(
            wait_for_lifecycle(&rx),
            LifecycleEvent::InputResolved {
                tool_call_id: "question-1".into(),
                kind: InputKind::Question,
            }
        );
        pane.shutdown().unwrap();
    }

    #[test]
    fn pane_renders_child_output_into_the_screen() {
        let (tx, rx) = mpsc::channel();
        let (program, args) = shell_command("echo pane-hello");
        let mut pane = Pane::spawn(test_spec(1, program, args), 24, 80, tx).unwrap();

        wait_for_exit(&rx, &mut pane);

        let contents = pane.with_screen(|screen| screen.contents()).unwrap();
        assert!(
            contents.contains("pane-hello"),
            "screen did not contain the output: {contents:?}"
        );
        assert!(!pane.is_running());
    }

    #[test]
    fn a_child_that_enables_bracketed_paste_is_detected() {
        // Copilot CLI turns the mode on at startup; the paste path relies on seeing it
        // so pasted newlines travel as text instead of Enter keystrokes.
        let (tx, rx) = mpsc::channel();
        let (program, args) = shell_command("echo done");
        let mut pane = Pane::spawn(test_spec(1, program, args), 24, 80, tx).unwrap();

        assert!(!pane.bracketed_paste());
        pane.feed_synthetic(b"\x1b[?2004h");
        assert!(pane.bracketed_paste());
        pane.send_prompt_snippet("line one\nline two").unwrap();
        pane.feed_synthetic(b"\x1b[?2004l");
        assert!(!pane.bracketed_paste());

        wait_for_exit(&rx, &mut pane);
    }

    #[test]
    fn startup_readiness_is_cached_without_rebuilding_screen_contents() {
        let (tx, _) = mpsc::channel();
        let (program, args) = shell_command(if cfg!(windows) {
            "ping -n 30 127.0.0.1 > nul"
        } else {
            "sleep 30"
        });
        let mut pane = Pane::spawn(test_spec(1, program, args), 24, 80, tx).unwrap();

        assert!(pane.is_blank());
        pane.feed_synthetic(b"\x1b[?2004h\x1b[6n");
        assert!(pane.is_blank(), "control sequences are not visible output");
        pane.feed_synthetic(b"Copilot ready");
        assert!(!pane.is_blank());
        assert!(!pane.is_blank(), "subsequent checks are atomic reads");

        pane.shutdown().unwrap();
    }

    #[test]
    fn background_work_completion_sets_attention_until_acknowledged() {
        let (tx, _) = mpsc::channel();
        let (program, args) = shell_command(if cfg!(windows) {
            "ping -n 30 127.0.0.1 > nul"
        } else {
            "sleep 30"
        });
        let mut pane = Pane::spawn(test_spec(31, program, args), 24, 80, tx).unwrap();

        pane.feed_synthetic(b"\x1b]9;4;3;0\x1b\\");
        pane.refresh_from_callbacks(false);
        assert!(
            !pane.needs_attention(),
            "starting work clears old attention"
        );

        pane.feed_synthetic(b"\x1b]9;4;0;0\x1b\\");
        let (_, notifications) = pane.refresh_from_callbacks(false);
        assert_eq!(notifications, vec![PaneNotification::Ready]);
        assert!(pane.needs_attention());
        assert!(pane.display_title().starts_with("? "));

        pane.acknowledge_attention();
        assert!(!pane.needs_attention());
        assert_eq!(pane.display_title(), pane.title);
        pane.shutdown().unwrap();
    }

    #[test]
    fn attended_completion_does_not_create_an_attention_marker() {
        let (tx, _) = mpsc::channel();
        let (program, args) = shell_command(if cfg!(windows) {
            "ping -n 30 127.0.0.1 > nul"
        } else {
            "sleep 30"
        });
        let mut pane = Pane::spawn(test_spec(32, program, args), 24, 80, tx).unwrap();

        pane.feed_synthetic(b"\x1b]9;4;3;0\x1b\\");
        pane.refresh_from_callbacks(true);
        pane.feed_synthetic(b"\x1b]9;4;0;0\x1b\\");
        let (_, notifications) = pane.refresh_from_callbacks(true);

        assert!(!pane.needs_attention());
        assert_eq!(
            notifications,
            vec![PaneNotification::Ready],
            "phone history is independent of focus"
        );
        pane.shutdown().unwrap();
    }

    #[test]
    fn question_state_overrides_progress_until_matching_completion() {
        let (tx, _) = mpsc::channel();
        let (program, args) = shell_command(if cfg!(windows) {
            "ping -n 30 127.0.0.1 > nul"
        } else {
            "sleep 30"
        });
        let mut pane = Pane::spawn(test_spec(37, program, args), 24, 80, tx).unwrap();
        pane.apply_signals(
            PaneSignals {
                events: vec![PaneSignalEvent::Progress(
                    crate::host_terminal::ProgressState::Indeterminate,
                )],
                ..Default::default()
            },
            true,
        );

        assert_eq!(
            pane.apply_lifecycle(LifecycleEvent::InputRequested {
                tool_call_id: "question-1".into(),
                kind: InputKind::Question,
            }),
            Some(PaneNotification::Question)
        );
        assert!(pane.requires_user_action());
        assert_eq!(
            pane.effective_progress_state(),
            crate::host_terminal::ProgressState::Clear
        );
        pane.acknowledge_attention();
        assert!(
            pane.needs_attention(),
            "focus cannot dismiss a live question"
        );
        assert!(pane.display_title().starts_with("? "));

        let (_, notifications) = pane.apply_signals(
            PaneSignals {
                events: vec![PaneSignalEvent::Progress(
                    crate::host_terminal::ProgressState::Indeterminate,
                )],
                ..Default::default()
            },
            true,
        );
        assert!(notifications.is_empty());
        assert_eq!(
            pane.effective_progress_state(),
            crate::host_terminal::ProgressState::Clear
        );

        pane.apply_lifecycle(LifecycleEvent::InputResolved {
            tool_call_id: "another-question".into(),
            kind: InputKind::Question,
        });
        assert!(pane.requires_user_action());
        pane.apply_lifecycle(LifecycleEvent::InputResolved {
            tool_call_id: "question-1".into(),
            kind: InputKind::Question,
        });
        assert!(!pane.requires_user_action());
        assert_eq!(
            pane.effective_progress_state(),
            crate::host_terminal::ProgressState::Indeterminate
        );
        assert_eq!(pane.display_title(), pane.title);
        pane.shutdown().unwrap();
    }

    #[test]
    fn all_pending_inputs_must_resolve_and_reset_clears_stale_waits() {
        let (tx, _) = mpsc::channel();
        let (program, args) = shell_command(if cfg!(windows) {
            "ping -n 30 127.0.0.1 > nul"
        } else {
            "sleep 30"
        });
        let mut pane = Pane::spawn(test_spec(38, program, args), 24, 80, tx).unwrap();

        assert_eq!(
            pane.apply_lifecycle(LifecycleEvent::InputRequested {
                tool_call_id: "question".into(),
                kind: InputKind::Question,
            }),
            Some(PaneNotification::Question)
        );
        assert_eq!(
            pane.apply_lifecycle(LifecycleEvent::InputRequested {
                tool_call_id: "plan".into(),
                kind: InputKind::PlanApproval,
            }),
            Some(PaneNotification::PlanApproval)
        );
        pane.apply_lifecycle(LifecycleEvent::InputResolved {
            tool_call_id: "question".into(),
            kind: InputKind::Question,
        });
        assert!(pane.requires_user_action());

        pane.apply_lifecycle(LifecycleEvent::Reset);
        assert!(!pane.requires_user_action());
        assert!(!pane.needs_attention());
        pane.shutdown().unwrap();
    }

    #[test]
    fn working_and_clear_in_one_output_chunk_still_requests_attention() {
        let (tx, _) = mpsc::channel();
        let (program, args) = shell_command(if cfg!(windows) {
            "ping -n 30 127.0.0.1 > nul"
        } else {
            "sleep 30"
        });
        let mut pane = Pane::spawn(test_spec(33, program, args), 24, 80, tx).unwrap();

        pane.feed_synthetic(b"\x1b]9;4;3;0\x1b\\answer\x1b]9;4;0;0\x1b\\");
        assert_eq!(
            pane.refresh_from_callbacks(false).1,
            vec![PaneNotification::Ready]
        );

        assert!(pane.needs_attention());
        pane.shutdown().unwrap();
    }

    #[test]
    fn background_bell_requests_attention() {
        let (tx, _) = mpsc::channel();
        let (program, args) = shell_command(if cfg!(windows) {
            "ping -n 30 127.0.0.1 > nul"
        } else {
            "sleep 30"
        });
        let mut pane = Pane::spawn(test_spec(34, program, args), 24, 80, tx).unwrap();

        pane.feed_synthetic(b"\x07");
        let (outcome, notifications) = pane.refresh_from_callbacks(false);
        assert!(outcome.bell);
        assert_eq!(notifications, vec![PaneNotification::Ready]);
        assert!(pane.needs_attention());

        pane.acknowledge_attention();
        pane.feed_synthetic(b"\x07");
        let (_, notifications) = pane.refresh_from_callbacks(true);
        assert!(
            notifications.is_empty(),
            "bell is deduplicated until new work starts"
        );
        assert!(!pane.needs_attention());
        pane.shutdown().unwrap();
    }

    #[test]
    fn error_notifies_once_and_suppresses_ready_until_the_next_cycle() {
        let (tx, _) = mpsc::channel();
        let (program, args) = shell_command(if cfg!(windows) {
            "ping -n 30 127.0.0.1 > nul"
        } else {
            "sleep 30"
        });
        let mut pane = Pane::spawn(test_spec(35, program, args), 24, 80, tx).unwrap();

        pane.feed_synthetic(b"\x1b]9;4;2;0\x1b\\");
        let (_, error) = pane.refresh_from_callbacks(true);
        assert_eq!(error, vec![PaneNotification::Error]);
        pane.feed_synthetic(b"\x1b]9;4;2;0\x1b\\\x1b]9;4;0;0\x1b\\");
        let (_, duplicate_and_clear) = pane.refresh_from_callbacks(true);
        assert!(duplicate_and_clear.is_empty());

        pane.feed_synthetic(b"\x1b]9;4;3;0\x1b\\\x1b]9;4;0;0\x1b\\");
        let (_, next_cycle) = pane.refresh_from_callbacks(true);
        assert_eq!(next_cycle, vec![PaneNotification::Ready]);
        pane.shutdown().unwrap();
    }

    #[test]
    fn ordered_chunk_preserves_multiple_cycles_and_bell_before_error() {
        let (tx, _) = mpsc::channel();
        let (program, args) = shell_command(if cfg!(windows) {
            "ping -n 30 127.0.0.1 > nul"
        } else {
            "sleep 30"
        });
        let mut pane = Pane::spawn(test_spec(36, program, args), 24, 80, tx).unwrap();

        pane.feed_synthetic(
            b"\x1b]9;4;3;0\x1b\\\x07\x1b]9;4;2;0\x1b\\\x1b]9;4;0;0\x1b\\\
              \x1b]9;4;3;0\x1b\\\x1b]9;4;0;0\x1b\\",
        );
        let (_, notifications) = pane.refresh_from_callbacks(false);

        assert_eq!(
            notifications,
            vec![
                PaneNotification::Ready,
                PaneNotification::Error,
                PaneNotification::Ready,
            ]
        );
        pane.shutdown().unwrap();
    }

    #[test]
    fn keys_sent_to_an_exited_pane_are_ignored() {
        let (tx, rx) = mpsc::channel();
        let (program, args) = shell_command("echo done");
        let mut pane = Pane::spawn(test_spec(2, program, args), 24, 80, tx).unwrap();

        wait_for_exit(&rx, &mut pane);

        // Must not error even though the child is gone.
        pane.send_key(&KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))
            .unwrap();
        pane.send_paste("text").unwrap();
        assert!(pane.send_prompt_snippet("text").is_err());
    }

    #[test]
    fn multiline_snippet_is_rejected_until_bracketed_paste_is_enabled() {
        let (tx, _) = mpsc::channel();
        let (program, args) = shell_command(if cfg!(windows) {
            "ping -n 3 127.0.0.1 > nul"
        } else {
            "sleep 2"
        });
        let mut pane = Pane::spawn(test_spec(3, program, args), 24, 80, tx).unwrap();

        assert!(pane
            .send_prompt_snippet("")
            .unwrap_err()
            .to_string()
            .contains("empty"));
        assert!(pane
            .send_prompt_snippet("safe\x1b[201~unsafe")
            .unwrap_err()
            .to_string()
            .contains("control characters"));
        pane.send_prompt_snippet("single line").unwrap();
        let error = pane
            .send_prompt_snippet("line one\nline two")
            .unwrap_err()
            .to_string();
        assert!(error.contains("not ready for multiline paste"));

        let _ = pane.kill();
    }

    #[test]
    fn resize_updates_the_parser_dimensions() {
        let (tx, rx) = mpsc::channel();
        // Keep the child alive briefly so the resize targets a live PTY.
        let (program, args) = shell_command(if cfg!(windows) {
            "ping -n 3 127.0.0.1 > nul"
        } else {
            "sleep 2"
        });
        let mut pane = Pane::spawn(test_spec(3, program, args), 24, 80, tx).unwrap();

        pane.resize(40, 120).unwrap();
        let size = pane.with_screen(|screen| screen.size()).unwrap();
        assert_eq!(size, (40, 120));

        pane.kill().unwrap();
        wait_for_exit(&rx, &mut pane);
    }

    #[test]
    fn shutdown_waits_until_the_child_is_really_gone() {
        let (tx, _) = mpsc::channel();
        let (program, args) = shell_command(if cfg!(windows) {
            "ping -n 30 127.0.0.1 > nul"
        } else {
            "sleep 30"
        });
        let mut pane = Pane::spawn(test_spec(30, program, args), 24, 80, tx).unwrap();
        assert!(pane.is_running());

        pane.shutdown().unwrap();

        assert!(!pane.is_running());
        assert!(matches!(pane.status, PaneStatus::Exited(_)));
    }

    /// The full loop that makes the multiplexer usable: keystrokes are encoded, written to
    /// the PTY, echoed by the child, parsed, and land on the screen we render.
    #[test]
    fn typed_keys_round_trip_through_the_child() {
        let (tx, rx) = mpsc::channel();
        let (program, args) = if cfg!(windows) {
            (
                "cmd.exe".to_string(),
                vec!["/q".to_string(), "/k".to_string(), "@echo off".to_string()],
            )
        } else {
            ("/bin/sh".to_string(), vec!["-i".to_string()])
        };
        let mut pane = Pane::spawn(test_spec(4, program, args), 24, 80, tx).unwrap();

        // Give the shell time to draw its prompt before typing into it.
        std::thread::sleep(Duration::from_millis(1500));
        for ch in "echo round-trip".chars() {
            pane.send_key(&KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE))
                .unwrap();
        }
        pane.send_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(20);
        let mut contents = String::new();
        while Instant::now() < deadline {
            let _ = rx.recv_timeout(Duration::from_millis(250));
            contents = pane.with_screen(|screen| screen.contents()).unwrap();
            if contents.matches("round-trip").count() >= 2 {
                break;
            }
        }

        // Twice: once as the echoed command line, once as the command's output.
        assert!(
            contents.matches("round-trip").count() >= 2,
            "child never echoed the typed command: {contents:?}"
        );

        pane.kill().unwrap();
        wait_for_exit(&rx, &mut pane);
    }
}
