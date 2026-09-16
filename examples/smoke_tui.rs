//! Diagnostic: drive the real CST binary under a PTY and assert on what it draws.
//!
//! Run with:  cargo run --example smoke_tui
//!
//! Unit tests render through ratatui's `TestBackend`, which exercises the widgets but
//! not the binary: raw mode, the event loop, terminal queries, and the startup path
//! that decides what to show are all outside them. This spawns the actual executable,
//! answers the queries a real terminal would, parses its output with the same `vt100`
//! CST uses for child panes, and reads the screen back as text.
//!
//! Isolation matters more than it looks. `--copilot-home` is pointed at a temporary
//! directory for two reasons: the session list then has nothing machine-specific in it,
//! and — the important one — `hook_plugin::refresh_if_needed` hashes the *executable
//! path* into its receipt, so running a debug build against the real Copilot home would
//! silently repoint the user's installed lifecycle hooks at `target/debug`.
//!
//! The What's New marker has no such override, so it is saved and restored around the
//! run. Its end state is whatever a normal launch would have left anyway.
//!
//! Two things to know before trusting a failure here:
//!
//! - **It drives the last binary you built, not the current source.** `cargo run
//!   --example` builds the example, not the `copilot-session-tui` target, so a stale
//!   executable silently reports stale behaviour. There is a guard below; heed it.
//! - **Non-ASCII glyphs arrive mangled on Windows.** `•`, `·` and `—` come back as
//!   mojibake even though `vt100` round-trips them perfectly in isolation and the
//!   console is already UTF-8 — the loss is in ConPTY transit. Box-drawing survives.
//!   Every assertion here is therefore ASCII, and a garbled bullet in the dump below
//!   is the transport, not CST.

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const ROWS: u16 = 40;
const COLS: u16 = 140;
const TIMEOUT: Duration = Duration::from_secs(15);

struct Session {
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    done: Arc<AtomicBool>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    _master: Box<dyn MasterPty + Send>,
}

impl Session {
    fn start(exe: &PathBuf, copilot_home: &PathBuf) -> anyhow::Result<Self> {
        let pty = native_pty_system();
        let pair = pty.openpty(PtySize {
            rows: ROWS,
            cols: COLS,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut cmd = CommandBuilder::new(exe);
        cmd.arg("--copilot-home");
        cmd.arg(copilot_home);
        // Keep the run deterministic: no project auto-filter from wherever this is run.
        cmd.arg("--auto-filter");
        cmd.arg("false");
        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader()?;
        // A pty master hands out its writer once, so the reply path and the input
        // path share it.
        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(pair.master.take_writer()?));
        let responder = Arc::clone(&writer);
        let parser = Arc::new(Mutex::new(vt100::Parser::new(ROWS, COLS, 0)));
        let done = Arc::new(AtomicBool::new(false));

        let sink = Arc::clone(&parser);
        let flag = Arc::clone(&done);
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            // A pty read can end in the middle of a multi-byte character, and the
            // parser has no memory between calls — feeding it a split sequence turns a
            // bullet into mojibake. Hold the incomplete tail back until its rest
            // arrives.
            let mut pending: Vec<u8> = Vec::new();
            while !flag.load(Ordering::Relaxed) {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let chunk = &buf[..n];
                        // Answer the startup queries a real terminal would, or CST
                        // blocks waiting for a reply that never arrives.
                        let mut reply: Vec<u8> = Vec::new();
                        if chunk.windows(4).any(|w| w == b"\x1b[6n") {
                            reply.extend_from_slice(b"\x1b[1;1R");
                        }
                        if chunk.windows(3).any(|w| w == b"\x1b[c") {
                            reply.extend_from_slice(b"\x1b[?1;2c");
                        }
                        if !reply.is_empty() {
                            if let Ok(mut writer) = responder.lock() {
                                let _ = writer.write_all(&reply);
                                let _ = writer.flush();
                            }
                        }
                        pending.extend_from_slice(chunk);
                        let complete = match std::str::from_utf8(&pending) {
                            Ok(_) => pending.len(),
                            // A genuinely invalid byte is not going to be completed by
                            // waiting, so pass it through rather than stalling.
                            Err(error) => match error.error_len() {
                                Some(bad) => error.valid_up_to() + bad,
                                None => error.valid_up_to(),
                            },
                        };
                        let ready: Vec<u8> = pending.drain(..complete).collect();
                        if !ready.is_empty() {
                            if let Ok(mut parser) = sink.lock() {
                                parser.process(&ready);
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            parser,
            writer,
            done,
            child,
            _master: pair.master,
        })
    }

    fn screen(&self) -> String {
        let parser = self.parser.lock().expect("parser lock");
        let screen = parser.screen();
        (0..ROWS)
            .map(|row| {
                (0..COLS)
                    .map(|col| {
                        let contents = screen
                            .cell(row, col)
                            .map(|cell| cell.contents())
                            .unwrap_or_default();
                        if contents.is_empty() {
                            " ".to_string()
                        } else {
                            contents.to_string()
                        }
                    })
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn send(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
        let mut writer = self.writer.lock().expect("writer lock");
        writer.write_all(bytes)?;
        writer.flush()?;
        Ok(())
    }

    /// Poll until the screen contains `needle`, or give up.
    fn wait_for(&self, needle: &str) -> anyhow::Result<()> {
        let deadline = Instant::now() + TIMEOUT;
        while Instant::now() < deadline {
            if self.screen().contains(needle) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        anyhow::bail!(
            "timed out waiting for {needle:?}. Screen was:\n{}",
            self.screen()
        )
    }

    /// Poll until the screen stops containing `needle`.
    fn wait_until_gone(&self, needle: &str) -> anyhow::Result<()> {
        let deadline = Instant::now() + TIMEOUT;
        while Instant::now() < deadline {
            if !self.screen().contains(needle) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        anyhow::bail!("{needle:?} never went away. Screen was:\n{}", self.screen())
    }

    /// Give the app a moment to settle, then assert `needle` is still absent.
    fn assert_absent(&self, needle: &str) -> anyhow::Result<()> {
        std::thread::sleep(Duration::from_millis(1200));
        if self.screen().contains(needle) {
            anyhow::bail!("did not expect {needle:?} on screen:\n{}", self.screen());
        }
        Ok(())
    }

    fn quit(mut self) -> anyhow::Result<()> {
        let _ = self.send(b"q");
        std::thread::sleep(Duration::from_millis(400));
        self.done.store(true, Ordering::Relaxed);
        let _ = self.child.kill();
        let _ = self.child.wait();
        Ok(())
    }
}

/// Refuse to report on a binary older than the source it is supposed to represent.
///
/// `cargo run --example` does not rebuild the `copilot-session-tui` target, so without
/// this the harness happily asserts against whatever was last built and reports a
/// difference that exists only in your `target/` directory.
fn warn_if_stale(exe: &PathBuf) {
    let built = match std::fs::metadata(exe).and_then(|meta| meta.modified()) {
        Ok(time) => time,
        Err(_) => return,
    };
    let newest = newest_source(&PathBuf::from("src"));
    if let Some(newest) = newest {
        if newest > built {
            eprintln!(
                "\nWARNING: {} is older than src/. Run `cargo build` first, or you are \
                 testing code you have already changed.\n",
                exe.display()
            );
        }
    }
}

fn newest_source(dir: &PathBuf) -> Option<std::time::SystemTime> {
    let mut newest: Option<std::time::SystemTime> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        let candidate = if path.is_dir() {
            newest_source(&path)
        } else {
            entry.metadata().ok().and_then(|meta| meta.modified().ok())
        };
        if let Some(candidate) = candidate {
            newest = Some(newest.map_or(candidate, |current| current.max(candidate)));
        }
    }
    newest
}

fn marker_path() -> PathBuf {
    state_root().join("app-state.json")
}

/// Thread subscriptions and undelivered messages, seeded to test the inbox.
///
/// Like the What's New marker, this has no `--copilot-home` style override, so the real
/// file is saved and put back rather than isolated.
fn threads_path() -> PathBuf {
    state_root().join("threads.json")
}

fn state_root() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("copilot-session-tui")
}

fn main() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?
        .parent()
        .and_then(|dir| dir.parent())
        .map(|dir| dir.join("copilot-session-tui.exe"))
        .filter(|path| path.exists())
        .unwrap_or_else(|| "copilot-session-tui".into());
    println!("driving {}", exe.display());
    warn_if_stale(&exe);

    let copilot_home = tempfile::tempdir()?;
    let home = copilot_home.path().to_path_buf();

    let marker = marker_path();
    let threads = threads_path();
    let saved_marker = std::fs::read(&marker).ok();
    let saved_threads = std::fs::read(&threads).ok();
    let restore = |path: &PathBuf, saved: &Option<Vec<u8>>| match saved {
        Some(bytes) => {
            let _ = std::fs::write(path, bytes);
        }
        None => {
            let _ = std::fs::remove_file(path);
        }
    };

    let result = run(&exe, &home, &marker, &threads);
    restore(&marker, &saved_marker);
    restore(&threads, &saved_threads);
    println!("\nrestored the What's New marker and thread state to how they were found");
    result
}

fn run(exe: &PathBuf, home: &PathBuf, marker: &PathBuf, threads: &PathBuf) -> anyhow::Result<()> {
    let mut failures = Vec::new();

    // 1. A version behind: the notes for every release since should appear.
    if let Some(parent) = marker.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(marker, br#"{"last_ran_version":"0.25.0"}"#)?;

    let mut cst = Session::start(exe, home)?;
    match cst.wait_for("What's new") {
        Ok(()) => {
            let screen = cst.screen();
            println!("--- What's New on first launch after an update ---\n{screen}\n");
            // Only what is above the fold, and only what stays true as releases pile
            // up. Naming a specific old version dates the test twice over: it drops
            // below the fold, and then out of the capped span entirely. This one asks
            // where the user was, and that the newest release is what they are shown.
            let newest = format!("v{}", env!("CARGO_PKG_VERSION"));
            for expected in ["You were on v0.25.0", newest.as_str()] {
                if !screen.contains(expected) {
                    failures.push(format!("the span is missing {expected}"));
                }
            }
            if screen.contains("v0.25.0 —") {
                failures.push("the version already seen was shown again".to_string());
            }

            // End reaches the bottom, where a span this long is truncated. The notice
            // is the thing worth asserting: the bound has to be stated rather than the
            // older releases silently vanishing. Naming a version here would pass by
            // luck, since the one it named is the one the notice happens to mention.
            cst.send(b"\x1b[F")?;
            if let Err(error) = cst.wait_for("earlier releases") {
                failures.push(format!(
                    "scrolling did not reach the truncation notice: {error}"
                ));
            }

            // 2. Esc closes it and reveals the session list underneath.
            cst.send(b"\x1b")?;
            if let Err(error) = cst.wait_until_gone("What's new in CST") {
                failures.push(format!("Esc did not close it: {error}"));
            }
            if let Err(error) = cst.wait_for("Copilot Session Manager") {
                failures.push(format!("the list did not come back: {error}"));
            }
        }
        Err(error) => failures.push(format!("What's New never appeared: {error}")),
    }
    cst.quit()?;

    // 3. Relaunching the same version must not show it again.
    let cst = Session::start(exe, home)?;
    cst.wait_for("Copilot Session Manager")?;
    match cst.assert_absent("What's new in CST") {
        Ok(()) => println!("second launch: correctly silent"),
        Err(error) => failures.push(format!("it repeated itself on relaunch: {error}")),
    }
    cst.quit()?;

    // 4. The command palette can bring it back on demand.
    let mut cst = Session::start(exe, home)?;
    cst.wait_for("Copilot Session Manager")?;
    cst.send(b"\x02\x02")?; // C-b C-b
    match cst.wait_for("Command") {
        Ok(()) => {
            cst.send(b"whats new")?;
            std::thread::sleep(Duration::from_millis(300));
            cst.send(b"\r")?;
            match cst.wait_for("What's new in CST") {
                Ok(()) => println!(
                    "--- reopened from the command palette ---\n{}\n",
                    cst.screen()
                ),
                Err(error) => failures.push(format!("the palette entry did not open it: {error}")),
            }
        }
        Err(error) => failures.push(format!("the command palette did not open: {error}")),
    }
    cst.quit()?;

    // 5. A message that could not be delivered is visible and can be dismissed.
    //
    // This is the only path by which a GitHub comment can start a closed session, so it
    // is worth proving against the real binary rather than only a TestBackend: raw mode,
    // the event loop, and the modal being reachable at all.
    //
    // Seeded in the **old** `pending` shape on purpose. It is what a user upgrading from
    // 0.32 has on disk, and reading it is the one migration that cannot be checked by
    // unit tests alone — nobody would notice a silently empty inbox after an upgrade.
    std::fs::write(
        threads,
        br#"{"subscriptions":[],"pending":[{
            "session_id":"smoke-session","comment_url":"https://github.com/o/r/issues/12",
            "reason":"foreign_author","arrived_at":"2026-09-10T09:00:00Z",
            "thread":{"host":"github.com","owner":"o","repo":"r","number":12,"kind":"issue"}
        }],"notifications_cursor":null}"#,
    )?;

    let mut cst = Session::start(exe, home)?;
    cst.wait_for("Copilot Session Manager")?;
    cst.send(b"\x02\x02")?; // C-b C-b
    match cst.wait_for("Command") {
        Ok(()) => {
            cst.send(b"waiting")?;
            std::thread::sleep(Duration::from_millis(300));
            cst.send(b"\r")?;
            // Waited for on a string only the modal renders. "Waiting for you" is also
            // the palette row's own title, so it matches before Enter is even processed.
            match cst.wait_for("written by someone else") {
                Ok(()) => {
                    let screen = cst.screen();
                    println!("--- a message held for the user ---\n{screen}\n");
                    // ASCII only: the harness documents that other glyphs do not
                    // survive ConPTY, so the reason and URL are what is asserted.
                    for expected in ["issues/12", "dismiss"] {
                        if !screen.contains(expected) {
                            failures.push(format!("the inbox is missing {expected}"));
                        }
                    }
                    cst.send(b"\x1b")?;
                    if let Err(error) = cst.wait_until_gone("written by someone else") {
                        failures.push(format!("Esc did not close the inbox: {error}"));
                    }
                }
                Err(error) => failures.push(format!("the inbox did not open: {error}")),
            }
        }
        Err(error) => failures.push(format!("the command palette did not open: {error}")),
    }
    cst.quit()?;

    // And the file it left behind is in the new shape, so the next run does not have to
    // migrate it again — and so a downgrade is the only way back, not an accident.
    match std::fs::read_to_string(threads) {
        Ok(saved) => {
            if !saved.contains("\"notices\"") {
                failures.push("the upgraded state file has no notices".to_string());
            }
            if saved.contains("\"pending\"") {
                failures.push("the old pending list was written back out".to_string());
            }
        }
        Err(error) => failures.push(format!("the thread state was not saved: {error}")),
    }

    // 6. A notice the previous run planned but never delivered is not lost.
    //
    // This is the whole reason the status is on disk. CST marks the comment behind a
    // notice as seen the moment it plans one, so a notice that only existed in memory
    // meant the message was never mentioned again. Written here in the `planned` state
    // a crashed run would leave behind, and the session it names is not running — so a
    // correct build must surface it rather than silently start anything or forget it.
    std::fs::write(
        threads,
        br#"{"subscriptions":[],"notices":[{
            "session_id":"smoke-session","comment_url":"https://github.com/o/r/issues/77",
            "state":"planned","planned_at":"2026-09-10T09:00:00Z",
            "thread":{"host":"github.com","owner":"o","repo":"r","number":77,"kind":"issue"}
        }],"cursors":{}}"#,
    )?;

    let mut cst = Session::start(exe, home)?;
    cst.wait_for("Copilot Session Manager")?;
    cst.send(b"\x02\x02")?; // C-b C-b
    match cst.wait_for("Command") {
        Ok(()) => {
            cst.send(b"waiting")?;
            std::thread::sleep(Duration::from_millis(300));
            cst.send(b"\r")?;
            match cst.wait_for("its session is closed") {
                Ok(()) => {
                    let screen = cst.screen();
                    println!("--- a notice that outlived the run that planned it ---\n{screen}\n");
                    if !screen.contains("issues/77") {
                        failures.push("the resumed notice names the wrong thread".to_string());
                    }
                    cst.send(b"\x1b")?;
                }
                Err(error) => failures.push(format!(
                    "a planned notice did not survive a restart: {error}"
                )),
            }
        }
        Err(error) => failures.push(format!("the command palette did not open: {error}")),
    }
    cst.quit()?;

    if failures.is_empty() {
        println!("\nAll checks passed.");
        Ok(())
    } else {
        for failure in &failures {
            eprintln!("FAIL: {failure}");
        }
        anyhow::bail!("{} check(s) failed", failures.len())
    }
}
