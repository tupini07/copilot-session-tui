//! Reconstructs paste events that Windows delivers as ordinary keystrokes.
//!
//! Crossterm only understands bracketed paste on Unix — the parser lives in
//! `event/sys/unix/parse.rs`, and the Windows backend reads console input records
//! instead. Conhost strips the `ESC [ 200~` markers before the records reach us, so a
//! paste arrives as a burst of plain key events in which every newline is
//! indistinguishable from the user pressing Enter.
//!
//! Forwarding that burst keystroke-by-keystroke is what makes a pasted message submit
//! itself part-way through: each key becomes its own PTY write, and the child sees a
//! lone `\r` whenever the stream happens to be chunked next to one.
//!
//! A human cannot put several keystrokes into the console buffer at the same instant,
//! so a run of text keys that is *already queued* when the previous one is read can
//! only have come from a paste. Recognising it lets the rest of CST take the same
//! `Event::Paste` path Unix already takes, which wraps the text in bracketed-paste
//! markers and hands it to the child in a single write.

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::io;
use std::time::Duration;

/// Text characters a burst must contain before it is treated as a paste.
///
/// Key auto-repeat cannot reach this — repeats arrive tens of milliseconds apart and
/// the drain below only collects what is already waiting — so the threshold exists to
/// protect a genuine Enter that was typed while the reader was briefly descheduled.
const MIN_PASTE_CHARS: usize = 8;

/// Give the console pipe time to deliver the records after the first character.
///
/// A zero-time poll here let the first character escape as typing, then folded the
/// remainder into a paste on the next read (`A` followed by Copilot's paste card).
/// Five milliseconds is well below human key-repeat/typing intervals but comfortably
/// above the small scheduling gap Windows Terminal can put after the first record.
const BURST_START_GRACE: Duration = Duration::from_millis(5);

/// How long to keep waiting for more of a burst once one is clearly under way.
///
/// A large paste crosses the console pipe in several chunks with sub-millisecond gaps.
/// Pausing briefly keeps it as one paste instead of several; it never delays an
/// ordinary keystroke because a run of one character does not qualify.
const BURST_GRACE: Duration = Duration::from_millis(2);

/// Read the next terminal event, folding Windows paste bursts into `Event::Paste`.
///
/// Returns at least one event. Unix already delivers real paste events, so the burst
/// detection is skipped there and the stream is passed through untouched.
pub fn read_events() -> io::Result<Vec<Event>> {
    let first = event::read()?;
    if !cfg!(windows) || paste_char(&first).is_none() {
        return Ok(vec![first]);
    }
    collect_run(first, event::poll, event::read)
}

fn collect_run<P, R>(first: Event, mut poll: P, mut read: R) -> io::Result<Vec<Event>>
where
    P: FnMut(Duration) -> io::Result<bool>,
    R: FnMut() -> io::Result<Event>,
{
    let mut run = vec![first];
    let mut trailing = None;
    loop {
        let grace = if text_len(&run) >= 2 {
            BURST_GRACE
        } else {
            BURST_START_GRACE
        };
        if !poll(grace)? {
            break;
        }
        let next = read()?;
        if paste_char(&next).is_some() || is_key_release(&next) {
            run.push(next);
        } else {
            // Anything else ends the burst but still belongs to the caller.
            trailing = Some(next);
            break;
        }
    }

    let mut events = fold(run);
    events.extend(trailing);
    Ok(events)
}

/// Collapse a burst into a single paste, or hand back the original keys unchanged.
fn fold(run: Vec<Event>) -> Vec<Event> {
    if text_len(&run) < MIN_PASTE_CHARS {
        return run;
    }
    vec![Event::Paste(run.iter().filter_map(paste_char).collect())]
}

fn text_len(run: &[Event]) -> usize {
    run.iter().filter_map(paste_char).count()
}

/// The character a key event contributes to a paste, if it can belong to one.
///
/// Enter maps to `\n` rather than `\r`: the text ends up inside bracketed-paste
/// markers, and `\n` is what the same child already receives from a Unix paste.
fn paste_char(event: &Event) -> Option<char> {
    let Event::Key(key) = event else {
        return None;
    };
    if key.kind != KeyEventKind::Press {
        return None;
    }
    // Shift is expected on capitals and symbols; the others mean a real chord.
    if key.modifiers.intersects(
        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER | KeyModifiers::HYPER,
    ) {
        return None;
    }
    match key.code {
        KeyCode::Char(c) => Some(c),
        KeyCode::Enter => Some('\n'),
        KeyCode::Tab => Some('\t'),
        _ => None,
    }
}

/// Key-up records are interleaved with key-down records inside a Windows burst.
///
/// They carry no text and every CST handler discards them, but they must not be taken
/// for the end of a run or no paste would ever be long enough to detect.
fn is_key_release(event: &Event) -> bool {
    matches!(event, Event::Key(KeyEvent { kind, .. }) if *kind != KeyEventKind::Press)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn release(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new_with_kind(
            code,
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ))
    }

    fn typed(text: &str) -> Vec<Event> {
        text.chars()
            .map(|c| match c {
                '\n' => press(KeyCode::Enter),
                '\t' => press(KeyCode::Tab),
                c => press(KeyCode::Char(c)),
            })
            .collect()
    }

    #[test]
    fn folds_a_long_burst_into_one_paste() {
        let folded = fold(typed("hello world"));
        assert_eq!(folded, vec![Event::Paste("hello world".to_string())]);
    }

    #[test]
    fn delayed_remainder_does_not_leave_the_first_character_outside_the_paste() {
        use std::cell::{Cell, RefCell};
        use std::collections::VecDeque;

        let queue = RefCell::new(VecDeque::from(typed("fter the first character")));
        let polls = Cell::new(0);
        let events = collect_run(
            press(KeyCode::Char('A')),
            |timeout| {
                // Models Windows Terminal delivering the first record immediately and
                // the remaining console records a few milliseconds later.
                let call = polls.get();
                polls.set(call + 1);
                Ok(!queue.borrow().is_empty() && (call > 0 || timeout >= Duration::from_millis(3)))
            },
            || {
                queue
                    .borrow_mut()
                    .pop_front()
                    .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "empty queue"))
            },
        )
        .unwrap();

        assert_eq!(
            events,
            vec![Event::Paste("After the first character".to_string())]
        );
    }

    #[test]
    fn keeps_newlines_as_text_instead_of_enter_presses() {
        let folded = fold(typed("first line\nsecond line"));
        assert_eq!(
            folded,
            vec![Event::Paste("first line\nsecond line".to_string())]
        );
    }

    #[test]
    fn blank_lines_and_tabs_survive_the_fold() {
        let folded = fold(typed("para one\n\n\tindented"));
        assert_eq!(
            folded,
            vec![Event::Paste("para one\n\n\tindented".to_string())]
        );
    }

    #[test]
    fn a_short_run_is_left_as_individual_keys() {
        // Typing "ok" then Enter must still submit, never turn into pasted text.
        let run = typed("ok\n");
        assert_eq!(fold(run.clone()), run);
    }

    #[test]
    fn interleaved_key_releases_do_not_break_a_burst() {
        let mut run = Vec::new();
        for c in "pasted!".chars() {
            run.push(press(KeyCode::Char(c)));
            run.push(release(KeyCode::Char(c)));
        }
        run.push(press(KeyCode::Enter));
        run.push(release(KeyCode::Enter));

        assert_eq!(fold(run), vec![Event::Paste("pasted!\n".to_string())]);
    }

    #[test]
    fn chords_and_navigation_keys_are_never_paste_text() {
        assert_eq!(paste_char(&press(KeyCode::Char('a'))), Some('a'));
        assert_eq!(paste_char(&press(KeyCode::Enter)), Some('\n'));
        assert_eq!(paste_char(&press(KeyCode::Left)), None);
        assert_eq!(paste_char(&press(KeyCode::Backspace)), None);
        assert_eq!(paste_char(&press(KeyCode::Esc)), None);
        assert_eq!(paste_char(&release(KeyCode::Char('a'))), None);
        assert_eq!(
            paste_char(&Event::Key(KeyEvent::new(
                KeyCode::Char('b'),
                KeyModifiers::CONTROL
            ))),
            None,
            "the mux prefix must survive a burst instead of being pasted"
        );
    }

    #[test]
    fn shifted_characters_still_count_as_paste_text() {
        let shifted = Event::Key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT));
        assert_eq!(paste_char(&shifted), Some('A'));
    }

    #[test]
    fn a_non_text_event_is_not_swallowed_by_the_fold() {
        let mut run = typed("some pasted text");
        let folded = fold(std::mem::take(&mut run));
        assert_eq!(folded.len(), 1);
        assert!(matches!(folded[0], Event::Paste(_)));
    }

    #[test]
    fn a_pasted_burst_reaches_the_child_as_one_bracketed_write() {
        // The bug this module exists for: every Enter in the burst used to become its
        // own `\r`, so the child submitted the message part-way through the paste.
        let folded = fold(typed("first line\nsecond line\n\nthird line"));
        let [Event::Paste(text)] = folded.as_slice() else {
            panic!("expected the burst to fold into a single paste: {folded:?}");
        };

        let bytes = crate::mux::keys::encode_paste(text, true);

        assert_eq!(
            bytes,
            b"\x1b[200~first line\nsecond line\n\nthird line\x1b[201~"
        );
        assert!(
            !bytes.contains(&b'\r'),
            "a carriage return would submit the message mid-paste"
        );
    }

    /// Text a paste has to survive: several lines, a blank line, and no trailing break.
    const PASTED_TEXT: &str = "first pasted line\nsecond pasted line\n\nfourth pasted line";

    /// The child half of the console round-trip below.
    ///
    /// Ignored so it never runs as part of an ordinary suite, and it exits immediately
    /// unless the parent asked for it, so `--ignored` runs stay fast.
    #[test]
    #[ignore = "spawned by paste_bursts_survive_a_real_windows_console"]
    fn console_paste_probe_child() {
        use std::io::Write;

        if std::env::var("CST_PASTE_PROBE").is_err() {
            return;
        }

        let mut stdout = std::io::stdout();
        crossterm::terminal::enable_raw_mode().expect("raw mode");
        // Advertise bracketed paste exactly as the real UI does, so the console layer
        // makes the same decisions about the incoming paste that it makes for CST.
        write!(stdout, "\x1b[?2004h").unwrap();
        write!(stdout, "PROBE-READY\r\n").unwrap();
        stdout.flush().unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match event::poll(Duration::from_millis(100)) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(_) => break,
            }
            let Ok(events) = read_events() else { break };
            let mut saw_paste = false;
            for event in events {
                saw_paste |= matches!(event, Event::Paste(_));
                write!(stdout, "PROBE-EVENT {event:?}\r\n").unwrap();
            }
            stdout.flush().unwrap();
            // The parent sends one paste and nothing else, so there is nothing left to
            // wait for; failures still burn the full window and report what did arrive.
            if saw_paste {
                break;
            }
        }

        write!(stdout, "PROBE-DONE\r\n").unwrap();
        stdout.flush().unwrap();
        let _ = crossterm::terminal::disable_raw_mode();
    }

    /// Drive a paste through a genuine console instead of trusting a hand-built burst.
    ///
    /// The child runs under a pseudoconsole, so the bytes written here take the same
    /// route a real paste takes: the console host turns them into input records and
    /// crossterm reports those as ordinary keys. That is the step this module exists to
    /// undo, and the only way to confirm it is to watch it happen.
    #[test]
    #[cfg_attr(not(windows), ignore = "reconstruction only runs on Windows")]
    fn paste_bursts_survive_a_real_windows_console() {
        use portable_pty::{native_pty_system, CommandBuilder, PtySize};
        use std::io::{Read, Write};

        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 30,
                cols: 100,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();

        let mut command = CommandBuilder::new(std::env::current_exe().unwrap());
        command.arg("--exact");
        command.arg("paste::tests::console_paste_probe_child");
        command.arg("--ignored");
        command.arg("--nocapture");
        command.env("CST_PASTE_PROBE", "1");

        let mut child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);

        let mut writer = pair.master.take_writer().unwrap();
        let mut reader = pair.master.try_clone_reader().unwrap();
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            while let Ok(count) = reader.read(&mut buffer) {
                if count == 0 || tx.send(buffer[..count].to_vec()).is_err() {
                    return;
                }
            }
        });

        let mut transcript = String::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let mut pasted = false;
        while std::time::Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(chunk) => {
                    // ConPTY asks for the cursor position on startup and blocks until
                    // it is answered; the real pane does this via vt100 callbacks.
                    if chunk.windows(4).any(|window| window == b"\x1b[6n") {
                        writer.write_all(b"\x1b[1;1R").unwrap();
                        writer.flush().unwrap();
                    }
                    transcript.push_str(&String::from_utf8_lossy(&chunk));
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
            if !pasted && transcript.contains("PROBE-READY") {
                pasted = true;
                // Byte-for-byte what a terminal sends when bracketed paste is on.
                let mut payload = b"\x1b[200~".to_vec();
                payload.extend_from_slice(PASTED_TEXT.replace('\n', "\r").as_bytes());
                payload.extend_from_slice(b"\x1b[201~");
                writer.write_all(&payload).unwrap();
                writer.flush().unwrap();
            }
            if transcript.contains("PROBE-DONE") {
                break;
            }
        }

        let status = child.try_wait().ok().flatten().map(|s| s.exit_code());
        let _ = child.kill();
        let _ = child.wait();

        assert!(
            pasted,
            "probe never signalled readiness (child status {status:?}, \
             {} bytes read), transcript:\n{transcript}",
            transcript.len()
        );

        let reported: Vec<&str> = transcript
            .lines()
            .filter_map(|line| line.split_once("PROBE-EVENT ").map(|(_, rest)| rest.trim()))
            .collect();
        assert!(
            !reported.is_empty(),
            "the console delivered nothing, transcript:\n{transcript}"
        );

        let pastes: Vec<&&str> = reported
            .iter()
            .filter(|line| line.starts_with("Paste("))
            .collect();
        assert_eq!(
            pastes.len(),
            1,
            "expected one reconstructed paste, got {reported:#?}"
        );

        // Every line has to arrive as text. A stray Enter here is the original bug:
        // the child would have submitted the message part-way through the paste.
        for line in PASTED_TEXT.split('\n').filter(|line| !line.is_empty()) {
            assert!(
                pastes[0].contains(line),
                "{line:?} missing from {:?}",
                pastes[0]
            );
        }
        assert!(
            !reported.iter().any(|line| line.starts_with("Key(")),
            "pasted keys leaked out of the burst: {reported:#?}"
        );
    }
}
