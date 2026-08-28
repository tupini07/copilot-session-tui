use anyhow::{Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A child process running under a pseudo-terminal.
///
/// The reader side runs on its own thread and pushes chunks to `output`; everything
/// else (writes, resizes, kill) happens on the caller's thread.
pub struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer_tx: Sender<Vec<u8>>,
    child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    size: PtySize,
}

pub struct TerminationOutcome {
    pub code: Option<u32>,
    pub terminated: bool,
}

/// Chunks of child output, plus a final exit notification.
pub enum PtyChunk {
    Output(Vec<u8>),
    Exited(Option<u32>),
}

pub fn pty_size(rows: u16, cols: u16) -> PtySize {
    PtySize {
        // A zero-sized pane would make the child compute a degenerate layout.
        rows: rows.max(1),
        cols: cols.max(1),
        pixel_width: 0,
        pixel_height: 0,
    }
}

impl PtySession {
    /// Spawn `command` under a new PTY, streaming its output to `output`.
    pub fn spawn(
        program: &str,
        args: &[String],
        cwd: Option<&Path>,
        size: PtySize,
        output: Sender<PtyChunk>,
    ) -> Result<Self> {
        let pair = native_pty_system()
            .openpty(size)
            .context("Failed to open a pseudo-terminal")?;

        let mut builder = CommandBuilder::new(program);
        for arg in args {
            builder.arg(arg);
        }
        if let Some(cwd) = cwd {
            if cwd.exists() {
                builder.cwd(cwd);
            }
        }
        // Copilot renders a full-screen TUI; advertise a capable terminal so it does
        // not fall back to a degraded mode inside the pane.
        builder.env("TERM", "xterm-256color");

        let child = pair
            .slave
            .spawn_command(builder)
            .context("Failed to launch the session process")?;
        // The slave handle must be dropped or the child never sees EOF on exit.
        drop(pair.slave);

        let writer = pair
            .master
            .take_writer()
            .context("Failed to open the pseudo-terminal writer")?;
        let reader = pair
            .master
            .try_clone_reader()
            .context("Failed to open the pseudo-terminal reader")?;

        let child = Arc::new(Mutex::new(child));
        spawn_reader(reader, output.clone(), Arc::clone(&child));
        spawn_waiter(output, Arc::clone(&child));
        let writer_tx = spawn_writer(writer);

        Ok(Self {
            master: pair.master,
            writer_tx,
            child,
            size,
        })
    }

    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer_tx
            .send(bytes.to_vec())
            .context("The session process is no longer accepting input")?;
        Ok(())
    }

    /// A handle for writing to the child from other threads, used by the emulator to
    /// answer device-status queries. A single writer thread keeps ordering intact.
    pub fn writer_handle(&self) -> Sender<Vec<u8>> {
        self.writer_tx.clone()
    }

    pub fn resize(&mut self, size: PtySize) -> Result<()> {
        if size.rows == self.size.rows && size.cols == self.size.cols {
            return Ok(());
        }
        self.master
            .resize(size)
            .context("Failed to resize the pane")?;
        self.size = size;
        Ok(())
    }

    /// Current PTY dimensions, used to assert resize behaviour.
    #[cfg(test)]
    pub fn size(&self) -> PtySize {
        self.size
    }

    /// Exit code if the child has already finished, without blocking.
    pub fn try_wait(&self) -> Option<u32> {
        let mut child = self.child.lock().ok()?;
        child
            .try_wait()
            .ok()
            .flatten()
            .map(|status| status.exit_code())
    }

    pub fn kill(&self) -> Result<()> {
        if let Ok(mut child) = self.child.lock() {
            if child.try_wait().ok().flatten().is_none() {
                child
                    .kill()
                    .context("Failed to terminate the session process")?;
            }
        }
        Ok(())
    }

    pub fn terminate_and_wait(&self, timeout: Duration) -> Result<TerminationOutcome> {
        let pid = self.child.lock().ok().and_then(|child| child.process_id());
        if let Some(code) = self.try_wait() {
            return Ok(TerminationOutcome {
                code: Some(code),
                terminated: false,
            });
        }
        let terminated = match self.terminate_process() {
            Ok(terminated) => terminated,
            Err(error) => {
                if let Some(code) = self.try_wait() {
                    return Ok(TerminationOutcome {
                        code: Some(code),
                        terminated: false,
                    });
                }
                return Err(error);
            }
        };
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(mut child) = self.child.lock() {
                if let Some(status) = child
                    .try_wait()
                    .context("Failed to check the session process after termination")?
                {
                    return Ok(TerminationOutcome {
                        code: Some(status.exit_code()),
                        terminated,
                    });
                }
            }
            if Instant::now() >= deadline {
                anyhow::bail!(
                    "Session process {} did not exit within {} ms",
                    pid.map_or_else(|| "unknown".to_string(), |pid| pid.to_string()),
                    timeout.as_millis()
                );
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    #[cfg(windows)]
    fn terminate_process(&self) -> Result<bool> {
        use windows_sys::Win32::System::Threading::TerminateProcess;

        let child = self
            .child
            .lock()
            .map_err(|_| anyhow::anyhow!("Session process handle is unavailable"))?;
        let handle = child
            .as_raw_handle()
            .context("Session process has no Windows handle")?;
        // SAFETY: `handle` is owned by the live portable-pty child and remains valid
        // while the child mutex guard above is held.
        let terminated = unsafe { TerminateProcess(handle.cast(), 1) };
        if terminated == 0 {
            return Err(std::io::Error::last_os_error())
                .context("Failed to terminate the session process");
        }
        Ok(true)
    }

    #[cfg(not(windows))]
    fn terminate_process(&self) -> Result<bool> {
        if let Ok(mut child) = self.child.lock() {
            if child.try_wait().ok().flatten().is_some() {
                return Ok(false);
            }
            child
                .kill()
                .context("Failed to terminate the session process")?;
            return Ok(true);
        }
        Ok(false)
    }
}

fn spawn_writer(mut writer: Box<dyn Write + Send>) -> Sender<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        while let Ok(bytes) = rx.recv() {
            if writer.write_all(&bytes).is_err() || writer.flush().is_err() {
                return;
            }
        }
    });
    tx
}

fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    output: Sender<PtyChunk>,
    child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
) {
    std::thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    if output
                        .send(PtyChunk::Output(buffer[..count].to_vec()))
                        .is_err()
                    {
                        return;
                    }
                }
                Err(_) => break,
            }
        }

        // EOF on the master means the child closed its end; reap it for the exit code.
        // On Windows the master often never reaches EOF, which is why the waiter thread
        // below is the authoritative source of exit notifications.
        let code = child
            .lock()
            .ok()
            .and_then(|mut child| child.wait().ok())
            .map(|status| status.exit_code());
        let _ = output.send(PtyChunk::Exited(code));
    });
}

/// Watch for child termination independently of the read side.
///
/// ConPTY keeps the master handle readable for as long as the pseudoconsole exists, so
/// waiting for EOF is not a reliable exit signal on Windows. Polling `try_wait` holds
/// the child lock only briefly, leaving `kill` and `try_wait` responsive.
fn spawn_waiter(output: Sender<PtyChunk>, child: Arc<Mutex<Box<dyn Child + Send + Sync>>>) {
    std::thread::spawn(move || loop {
        let status = match child.lock() {
            Ok(mut child) => child.try_wait().ok().flatten(),
            Err(_) => return,
        };
        if let Some(status) = status {
            // Give the reader a moment to drain output written just before exit.
            std::thread::sleep(std::time::Duration::from_millis(150));
            let _ = output.send(PtyChunk::Exited(Some(status.exit_code())));
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn echo_command() -> (String, Vec<String>) {
        if cfg!(windows) {
            (
                "cmd.exe".to_string(),
                vec!["/c".to_string(), "echo pty-hello".to_string()],
            )
        } else {
            (
                "/bin/sh".to_string(),
                vec!["-c".to_string(), "echo pty-hello".to_string()],
            )
        }
    }

    fn collect_until_exit(
        rx: &mpsc::Receiver<PtyChunk>,
        session: &mut PtySession,
    ) -> (String, bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut text = String::new();
        let mut exited = false;
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(PtyChunk::Output(bytes)) => {
                    let chunk = String::from_utf8_lossy(&bytes).to_string();
                    // ConPTY asks for the cursor position on startup and blocks until
                    // answered; the real emulator does this via vt100 callbacks.
                    if chunk.contains("\x1b[6n") {
                        let _ = session.write(b"\x1b[1;1R");
                    }
                    text.push_str(&chunk);
                }
                Ok(PtyChunk::Exited(_)) => {
                    exited = true;
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        (text, exited)
    }

    #[test]
    fn spawns_a_child_and_streams_its_output() {
        let (tx, rx) = mpsc::channel();
        let (program, args) = echo_command();
        let mut session = PtySession::spawn(&program, &args, None, pty_size(24, 80), tx).unwrap();

        let (text, exited) = collect_until_exit(&rx, &mut session);

        assert!(exited, "expected an exit notification, got: {text:?}");
        assert!(text.contains("pty-hello"), "unexpected output: {text:?}");
    }

    #[test]
    fn resize_is_recorded_and_ignores_no_op_changes() {
        let (tx, rx) = mpsc::channel();
        let (program, args) = echo_command();
        let mut session = PtySession::spawn(&program, &args, None, pty_size(24, 80), tx).unwrap();

        session.resize(pty_size(30, 100)).unwrap();
        assert_eq!(session.size().rows, 30);
        assert_eq!(session.size().cols, 100);

        session.resize(pty_size(30, 100)).unwrap();
        assert_eq!(session.size().cols, 100);

        let _ = collect_until_exit(&rx, &mut session);
    }

    #[test]
    fn reports_the_child_exit_code() {
        let (tx, rx) = mpsc::channel();
        let script = "exit 3";
        let (program, args) = if cfg!(windows) {
            (
                "cmd.exe".to_string(),
                vec!["/c".to_string(), script.to_string()],
            )
        } else {
            (
                "/bin/sh".to_string(),
                vec!["-c".to_string(), script.to_string()],
            )
        };
        let mut session = PtySession::spawn(&program, &args, None, pty_size(24, 80), tx).unwrap();

        let (_, exited) = collect_until_exit(&rx, &mut session);

        assert!(exited);
        assert_eq!(session.try_wait(), Some(3));
    }

    #[test]
    fn zero_dimensions_are_clamped() {
        let size = pty_size(0, 0);
        assert_eq!(size.rows, 1);
        assert_eq!(size.cols, 1);
    }
}
