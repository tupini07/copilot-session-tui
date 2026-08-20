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

    let mut run = vec![first];
    let mut trailing = None;
    loop {
        let grace = if text_len(&run) >= 2 {
            BURST_GRACE
        } else {
            Duration::ZERO
        };
        if !event::poll(grace)? {
            break;
        }
        let next = event::read()?;
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
}
