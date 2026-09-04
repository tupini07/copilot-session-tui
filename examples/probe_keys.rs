//! Diagnostic: feed raw key bytes into a ConPTY and report how they reach a crossterm
//! application on the other side.
//!
//! This is the same path a terminal like Alacritty uses — it writes bytes to the pty
//! input and conhost synthesises console input records from them — so whatever this
//! prints is what CST actually receives.
//!
//! Run with:  cargo run --example probe_keys

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn main() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?
        .parent()
        .and_then(|dir| dir.parent())
        .map(|dir| dir.join("copilot-session-tui.exe"))
        .filter(|path| path.exists())
        .unwrap_or_else(|| "copilot-session-tui".into());

    let probes: &[(&str, &[u8])] = &[
        ("Ctrl+J", b"\x0a"),
        ("Enter", b"\x0d"),
        ("Alt+Enter", b"\x1b\x0d"),
        ("Ctrl+Enter (kitty)", b"\x1b[13;5u"),
        ("Shift+Enter (kitty)", b"\x1b[13;2u"),
    ];

    let pty = native_pty_system();
    let pair = pty.openpty(PtySize {
        rows: 40,
        cols: 120,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let mut cmd = CommandBuilder::new(exe);
    cmd.arg("debug-keys");
    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let writer = Arc::new(Mutex::new(pair.master.take_writer()?));
    let done = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&done);

    let responder = Arc::clone(&writer);
    let collector = std::thread::spawn(move || {
        let mut all = Vec::new();
        let mut buf = [0u8; 4096];
        while !flag.load(Ordering::Relaxed) {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = &buf[..n];
                    // Answer the startup queries a real terminal would, or the probe
                    // blocks forever waiting for a reply that never comes.
                    let mut reply: Vec<u8> = Vec::new();
                    if chunk.windows(4).any(|w| w == b"\x1b[6n") {
                        reply.extend_from_slice(b"\x1b[1;1R");
                    }
                    if chunk.windows(4).any(|w| w == b"\x1b[?u") {
                        // No Kitty keyboard protocol, which is what Alacritty-via-ConPTY
                        // effectively looks like from here.
                        reply.extend_from_slice(b"\x1b[?1;2c");
                    } else if chunk.windows(3).any(|w| w == b"\x1b[c") {
                        reply.extend_from_slice(b"\x1b[?1;2c");
                    }
                    if !reply.is_empty() {
                        if let Ok(mut writer) = responder.lock() {
                            let _ = writer.write_all(&reply);
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

    std::thread::sleep(Duration::from_millis(1500));

    for (name, bytes) in probes {
        {
            let mut writer = writer.lock().unwrap();
            // A marker keypress delimits each probe in the transcript.
            writer.write_all(b"#")?;
            writer.flush()?;
            std::thread::sleep(Duration::from_millis(150));
            writer.write_all(bytes)?;
            writer.flush()?;
        }
        std::thread::sleep(Duration::from_millis(350));
        println!("sent {name:<22} {bytes:02x?}");
    }

    std::thread::sleep(Duration::from_millis(400));
    done.store(true, Ordering::Relaxed);
    let _ = child.kill();
    let output = collector.join().unwrap_or_default();

    let text = String::from_utf8_lossy(&output);
    println!("\n=== RAW ({} bytes) ===", output.len());
    println!("{text}");
    println!("=== END RAW ===");
    println!("\n=== what the crossterm app on the other side saw ===");
    let mut probe = 0usize;
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        // Each probe was prefixed with '#', so a line reporting it starts a new group.
        if line.contains("Char('#')") {
            if probe < probes.len() {
                println!("\n-- {} --", probes[probe].0);
            }
            probe += 1;
            continue;
        }
        if probe > 0 && probe <= probes.len() {
            println!("   {line}");
        }
    }
    Ok(())
}
