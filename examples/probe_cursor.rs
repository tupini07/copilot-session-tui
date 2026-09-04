//! Diagnostic: spawn Copilot under a PTY the way CST does and report what it does to
//! the cursor, which CST mirrors into the host terminal.
//!
//! Run with:  cargo run --example probe_cursor -- [seconds] ["prompt"]
//!
//! Written to answer whether a flickering cursor in the chat pane comes from the child.
//! It does not: Copilot repaints its composer around ten times a second and brackets
//! every repaint with `ESC[?25l` … `ESC[?25h`, but those brackets almost always land
//! inside a single PTY read, so what CST samples per frame barely moves. The flicker
//! is in how CST writes its frames — see `synchronized_frame` in `main.rs`.

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn main() -> anyhow::Result<()> {
    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(20);

    let pty = native_pty_system();
    let pair = pty.openpty(PtySize {
        rows: 40,
        cols: 120,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let mut cmd = CommandBuilder::new("copilot");
    cmd.cwd(std::env::current_dir()?);
    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let writer = Arc::new(Mutex::new(pair.master.take_writer()?));

    let done = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&done);
    let responder = Arc::clone(&writer);
    let start = Instant::now();

    let collector = std::thread::spawn(move || {
        let mut chunks: Vec<(Duration, Vec<u8>)> = Vec::new();
        let mut buf = [0u8; 8192];
        while !flag.load(Ordering::Relaxed) {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = &buf[..n];
                    if chunk.windows(4).any(|w| w == b"\x1b[6n") {
                        if let Ok(mut writer) = responder.lock() {
                            let _ = writer.write_all(b"\x1b[1;1R");
                            let _ = writer.flush();
                        }
                    }
                    chunks.push((start.elapsed(), chunk.to_vec()));
                }
                Err(_) => break,
            }
        }
        chunks
    });

    // With a prompt, the capture covers a working session: bigger repaints, more of
    // them, and a far better chance of one being split across PTY reads.
    if let Some(prompt) = std::env::args().nth(2) {
        std::thread::sleep(Duration::from_secs(8));
        {
            let mut writer = writer.lock().unwrap();
            writer.write_all(prompt.as_bytes())?;
            writer.flush()?;
        }
        std::thread::sleep(Duration::from_millis(800));
        {
            let mut writer = writer.lock().unwrap();
            writer.write_all(b"\r")?;
            writer.flush()?;
        }
        std::thread::sleep(Duration::from_secs(seconds.saturating_sub(9)));
    } else {
        std::thread::sleep(Duration::from_secs(seconds));
    }
    done.store(true, Ordering::Relaxed);
    let _ = child.kill();
    let chunks = collector.join().unwrap_or_default();

    let total: usize = chunks.iter().map(|(_, bytes)| bytes.len()).sum();
    println!("=== captured {total} bytes in {} chunks ===", chunks.len());

    let mut hide_per_second: std::collections::BTreeMap<u64, usize> = Default::default();
    let mut show_per_second: std::collections::BTreeMap<u64, usize> = Default::default();
    let mut move_per_second: std::collections::BTreeMap<u64, usize> = Default::default();
    for (at, bytes) in &chunks {
        let second = at.as_secs();
        *hide_per_second.entry(second).or_default() += count(bytes, b"\x1b[?25l");
        *show_per_second.entry(second).or_default() += count(bytes, b"\x1b[?25h");
        *move_per_second.entry(second).or_default() += cursor_moves(bytes);
    }

    println!("=== per second: hide(?25l) / show(?25h) / cursor moves ===");
    for second in 0..seconds {
        let hide = hide_per_second.get(&second).copied().unwrap_or(0);
        let show = show_per_second.get(&second).copied().unwrap_or(0);
        let moves = move_per_second.get(&second).copied().unwrap_or(0);
        if hide + show + moves > 0 {
            println!("  t={second:>3}s  hide={hide:<4} show={show:<4} moves={moves}");
        }
    }
    let hide: usize = hide_per_second.values().sum();
    let show: usize = show_per_second.values().sum();
    println!("=== totals: hide={hide} show={show} ===");

    // The flicker only reaches the screen if CST can observe a hidden cursor, that is,
    // if a read ends while the child has it hidden. Replaying the stream chunk by
    // chunk, the way the mux feeds the parser, answers that directly.
    let mut hidden_after_chunk = 0usize;
    let mut both_in_one_chunk = 0usize;
    let mut hidden_for = Duration::ZERO;
    let mut hidden_since: Option<Duration> = None;
    let mut visible = true;
    let mut positions: Vec<(Duration, usize)> = Vec::new();
    for (at, bytes) in &chunks {
        if count(bytes, b"\x1b[?25l") > 0 && count(bytes, b"\x1b[?25h") > 0 {
            both_in_one_chunk += 1;
        }
        if let Some(last) = last_toggle(bytes) {
            visible = last;
        }
        if visible {
            if let Some(since) = hidden_since.take() {
                hidden_for += at.saturating_sub(since);
            }
        } else {
            hidden_after_chunk += 1;
            hidden_since.get_or_insert(*at);
        }
        if let Some(last) = last_absolute_move(bytes) {
            positions.push((*at, last));
        }
    }
    println!(
        "=== chunks ending with the cursor hidden: {hidden_after_chunk} of {} ({:.1}%) ===",
        chunks.len(),
        100.0 * hidden_after_chunk as f64 / chunks.len().max(1) as f64
    );
    println!("=== chunks carrying both a hide and a show: {both_in_one_chunk} ===");
    println!("=== time spent hidden across read boundaries: {hidden_for:?} of {seconds}s ===");

    let distinct: std::collections::BTreeSet<usize> =
        positions.iter().map(|(_, row)| *row).collect();
    println!(
        "=== distinct cursor rows left at chunk boundaries: {} ({:?}) ===",
        distinct.len(),
        distinct.iter().take(12).collect::<Vec<_>>()
    );

    // What CST would mirror into the host terminal, replayed through a real parser.
    // Near-zero blink and jump counts mean the child holds its cursor still, and so any
    // flicker on screen comes from how CST writes its frames rather than from Copilot.
    let (paints, blinks, jumps) = simulate(&chunks);
    println!("=== simulated paints={paints} cursor blinks={blinks} cursor jumps={jumps} ===");
    Ok(())
}

/// Replay the capture through a real parser, the way the UI loop consumes it, and count
/// what the host terminal's mirrored cursor would do.
///
/// A "jump" is a paint that moves the cursor somewhere else and a later paint moves it
/// back: exactly the darting that reads as flicker. A "blink" is a paint that shows or
/// hides it.
fn simulate(chunks: &[(Duration, Vec<u8>)]) -> (usize, usize, usize) {
    let mut parser = vt100::Parser::new(40, 120, 0);
    let mut paints = 0usize;
    let mut blinks = 0usize;
    let mut jumps = 0usize;
    let mut shown: Option<(u16, u16)> = None;

    // CST paints once per batch of child output, so one chunk is one frame.
    for (_, bytes) in chunks {
        parser.process(bytes);
        let screen = parser.screen();
        let mirrored = (!screen.hide_cursor()).then(|| screen.cursor_position());

        paints += 1;
        match (shown, mirrored) {
            (Some(before), Some(after)) if before != after => jumps += 1,
            (Some(_), None) | (None, Some(_)) => blinks += 1,
            _ => {}
        }
        shown = mirrored;
    }

    (paints, blinks, jumps)
}

/// Visibility left behind by the last DECTCEM toggle in a chunk, if any.
fn last_toggle(bytes: &[u8]) -> Option<bool> {
    let mut result = None;
    for index in 0..bytes.len() {
        if bytes[index..].starts_with(b"\x1b[?25l") {
            result = Some(false);
        } else if bytes[index..].starts_with(b"\x1b[?25h") {
            result = Some(true);
        }
    }
    result
}

/// Row of the last absolute cursor move (CSI row;col H) in a chunk.
fn last_absolute_move(bytes: &[u8]) -> Option<usize> {
    let mut result = None;
    let mut index = 0;
    while index + 1 < bytes.len() {
        if bytes[index] == 0x1b && bytes[index + 1] == b'[' {
            let mut end = index + 2;
            while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b';') {
                end += 1;
            }
            if end < bytes.len() && matches!(bytes[end], b'H' | b'f') {
                let params = String::from_utf8_lossy(&bytes[index + 2..end]);
                result = params
                    .split(';')
                    .next()
                    .and_then(|row| row.parse().ok())
                    .or(Some(1));
            }
            index = end + 1;
        } else {
            index += 1;
        }
    }
    result
}

fn count(bytes: &[u8], needle: &[u8]) -> usize {
    bytes.windows(needle.len()).filter(|w| *w == needle).count()
}

/// Rough count of cursor-positioning sequences: CSI ... H/f/A/B/C/D/G.
fn cursor_moves(bytes: &[u8]) -> usize {
    let mut moves = 0;
    let mut index = 0;
    while index + 1 < bytes.len() {
        if bytes[index] == 0x1b && bytes[index + 1] == b'[' {
            let mut end = index + 2;
            while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b';') {
                end += 1;
            }
            if end < bytes.len()
                && matches!(bytes[end], b'H' | b'f' | b'A' | b'B' | b'C' | b'D' | b'G')
            {
                moves += 1;
            }
            index = end + 1;
        } else {
            index += 1;
        }
    }
    moves
}
