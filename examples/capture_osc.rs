//! Diagnostic: spawn Copilot under a PTY the way CST does and report which
//! progress signals it actually emits.
//!
//! Run with:  cargo run --example capture_osc -- "<prompt>"
//!
//! Copilot blocks on a Device Status Report at startup, so this answers DSR the way
//! `PaneCallbacks` does. Without that the child never draws anything.

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn main() -> anyhow::Result<()> {
    let prompt = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "say hi in one word".to_string());

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

    let collector = std::thread::spawn(move || {
        let mut all: Vec<u8> = Vec::new();
        let mut buf = [0u8; 8192];
        while !flag.load(Ordering::Relaxed) {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = &buf[..n];
                    // Answer cursor-position requests, or ConPTY stalls on startup.
                    if chunk.windows(4).any(|w| w == b"\x1b[6n") {
                        if let Ok(mut writer) = responder.lock() {
                            let _ = writer.write_all(b"\x1b[1;1R");
                            let _ = writer.flush();
                        }
                    }
                    all.extend_from_slice(chunk);
                }
                Err(_) => break,
            }
        }
        all
    });

    std::thread::sleep(Duration::from_secs(15));
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

    let deadline = Instant::now() + Duration::from_secs(70);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(500));
    }

    done.store(true, Ordering::Relaxed);
    let _ = child.kill();
    let output = collector.join().unwrap_or_default();

    println!("=== captured {} bytes ===", output.len());

    let mut osc_kinds: std::collections::BTreeMap<String, usize> = Default::default();
    let mut index = 0;
    while index + 1 < output.len() {
        if output[index] == 0x1b && output[index + 1] == b']' {
            let start = index + 2;
            let mut end = start;
            while end < output.len() {
                if output[end] == 0x07 {
                    break;
                }
                if output[end] == 0x1b && end + 1 < output.len() && output[end + 1] == 0x5c {
                    break;
                }
                end += 1;
            }
            let body = String::from_utf8_lossy(&output[start..end.min(output.len())]);
            let key: String = body.split(';').take(2).collect::<Vec<_>>().join(";");
            *osc_kinds.entry(key.chars().take(24).collect()).or_default() += 1;
            index = end + 1;
        } else {
            index += 1;
        }
    }

    println!("=== OSC sequences emitted (command;param -> count) ===");
    if osc_kinds.is_empty() {
        println!("  (none)");
    }
    for (kind, count) in &osc_kinds {
        let note = if kind.starts_with("9;4") {
            "   <-- PROGRESS (drives the tab spinner)"
        } else {
            ""
        };
        println!("  {kind:<26} x{count}{note}");
    }

    let text = String::from_utf8_lossy(&output);
    println!(
        "=== child produced visible text: {} ===",
        text.chars().filter(|c| c.is_alphanumeric()).count() > 50
    );
    println!(
        "\n=== VERDICT: Copilot {} emit OSC 9;4 progress ===",
        if osc_kinds.keys().any(|k| k.starts_with("9;4")) {
            "DOES"
        } else {
            "does NOT"
        }
    );
    Ok(())
}
