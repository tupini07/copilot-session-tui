use anyhow::Result;
use crossterm::event::{KeyEvent, MouseEvent, MouseEventKind};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::callbacks::PaneCallbacks;
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
    /// Copilot session id, once known. New sessions only learn theirs after Copilot starts.
    pub session_id: Option<String>,
    pub title: String,
    pub cwd: PathBuf,
    pub status: PaneStatus,
    /// When the child was spawned, so the UI can say how long a slow start has taken.
    pub started_at: Instant,
    parser: PaneParser,
    pty: PtySession,
    mouse_captured: bool,
    viewport: Viewport,
}

/// Everything needed to start a pane, grouped so callers don't juggle a long argument list.
pub struct PaneSpec {
    pub id: PaneId,
    pub title: String,
    pub cwd: PathBuf,
    pub session_id: Option<String>,
    pub program: String,
    pub args: Vec<String>,
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
        } = spec;
        let size = pty_size(rows, cols);

        let (chunk_tx, chunk_rx) = std::sync::mpsc::channel();
        let pty = PtySession::spawn(&program, &args, Some(&cwd), size, chunk_tx)?;

        // Device-status replies must reach the child, or ConPTY stalls on startup.
        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            size.rows,
            size.cols,
            // Scrollback keeps output above the viewport reachable once copy-mode lands.
            5000,
            PaneCallbacks::new(pty.writer_handle(), events.clone()),
        )));

        let pump_parser = Arc::clone(&parser);
        std::thread::spawn(move || {
            while let Ok(chunk) = chunk_rx.recv() {
                match chunk {
                    PtyChunk::Output(bytes) => {
                        if let Ok(mut parser) = pump_parser.lock() {
                            parser.process(&bytes);
                        }
                        // The UI thread coalesces these; one wakeup per chunk is fine.
                        if events.send(MuxEvent::Output(id)).is_err() {
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

        Ok(Self {
            id,
            session_id,
            title,
            cwd,
            status: PaneStatus::Running,
            started_at: Instant::now(),
            parser,
            pty,
            mouse_captured: false,
            viewport: Viewport {
                x: 0,
                y: 0,
                rows: size.rows,
                cols: size.cols,
            },
        })
    }

    pub fn is_running(&self) -> bool {
        self.status == PaneStatus::Running
    }

    /// True while the child has produced nothing visible yet.
    ///
    /// Copilot takes a few seconds to draw its first frame, and ConPTY emits control
    /// sequences long before any text, so "has the screen got anything on it" is a more
    /// honest readiness signal than "have we received bytes".
    pub fn is_blank(&self) -> bool {
        self.with_screen(|screen| screen.contents().trim().is_empty())
            .unwrap_or(true)
    }

    /// Feed bytes straight into the parser, standing in for child output in tests
    /// and in the generated documentation screenshots.
    #[cfg(any(test, feature = "screenshots"))]
    pub fn feed_synthetic(&mut self, bytes: &[u8]) {
        if let Ok(mut parser) = self.parser.lock() {
            parser.process(bytes);
        }
    }

    pub fn mark_exited(&mut self, code: Option<u32>) {
        self.status = PaneStatus::Exited(code);
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

    /// Adopt any window title the child set via OSC, and report a pending bell.
    pub fn refresh_from_callbacks(&mut self) -> bool {
        let Ok(mut parser) = self.parser.lock() else {
            return false;
        };
        let callbacks = parser.callbacks_mut();
        if let Some(title) = callbacks.take_title() {
            let title = title.trim().to_string();
            if !title.is_empty() {
                self.title = title;
            }
        }

        callbacks.take_bell()
    }

    pub fn send_key(&mut self, key: &KeyEvent) -> Result<()> {
        if !self.is_running() {
            return Ok(());
        }
        if let Some(bytes) = keys::encode(key, self.application_cursor()) {
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

    /// Pick up an exit that happened without the reader thread noticing yet.
    pub fn poll_exit(&mut self) {
        if self.is_running() {
            if let Some(code) = self.pty.try_wait() {
                self.mark_exited(Some(code));
            }
        }
    }
}

fn github_reference_at(screen: &vt100::Screen, row: u16, column: u16) -> Option<u64> {
    let (_, cols) = screen.size();
    let characters: Vec<char> = (0..cols)
        .map(|col| {
            screen
                .cell(row, col)
                .and_then(|cell| cell.contents().chars().next())
                .unwrap_or(' ')
        })
        .collect();
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
            session_id: None,
            program,
            args,
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
        pane.feed_synthetic(b"\x1b[?2004l");
        assert!(!pane.bracketed_paste());

        wait_for_exit(&rx, &mut pane);
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
