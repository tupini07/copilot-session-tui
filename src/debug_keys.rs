use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement};
use std::io::Write;

/// Interactive probe that reports exactly which key events this terminal delivers.
///
/// The multiplexer needs a prefix key that is reliably captured. Plain control
/// characters (`Ctrl-b` = 0x02) travel as single bytes and work almost everywhere,
/// while chords like `Ctrl-Shift-*` or `Ctrl-Enter` require the Kitty keyboard
/// protocol or win32-input-mode and are silently dropped by many terminals.
pub fn run() -> Result<()> {
    println!("cst key probe");
    println!();
    match supports_keyboard_enhancement() {
        Ok(true) => println!("Keyboard enhancement protocol: SUPPORTED"),
        Ok(false) => println!("Keyboard enhancement protocol: not supported (plain mode)"),
        Err(error) => println!("Keyboard enhancement protocol: unknown ({error})"),
    }
    println!();
    println!("Press keys to see how this terminal reports them.");
    println!("Candidate prefixes to try: Ctrl-b, Ctrl-g, Ctrl-Space, Ctrl-a");
    println!("Press Esc twice to quit.");
    println!();

    enable_raw_mode()?;
    let result = probe_loop();
    disable_raw_mode()?;
    result
}

fn probe_loop() -> Result<()> {
    let mut last_was_esc = false;
    let mut stdout = std::io::stdout();

    loop {
        let event = event::read()?;
        match event {
            Event::Key(key) => {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                write!(stdout, "{}\r\n", describe_key(&key))?;
                stdout.flush()?;

                if key.code == KeyCode::Esc && key.modifiers.is_empty() {
                    if last_was_esc {
                        return Ok(());
                    }
                    last_was_esc = true;
                } else {
                    last_was_esc = false;
                }
            }
            Event::Mouse(mouse) => {
                if !matches!(mouse.kind, MouseEventKind::Moved) {
                    write!(stdout, "mouse    {:?}\r\n", mouse.kind)?;
                    stdout.flush()?;
                }
            }
            Event::Resize(cols, rows) => {
                write!(stdout, "resize   {cols}x{rows}\r\n")?;
                stdout.flush()?;
            }
            Event::Paste(text) => {
                write!(stdout, "paste    {} bytes\r\n", text.len())?;
                stdout.flush()?;
            }
            Event::FocusGained | Event::FocusLost => {}
        }
    }
}

fn describe_key(key: &KeyEvent) -> String {
    let modifiers = if key.modifiers.is_empty() {
        "none".to_string()
    } else {
        let mut parts = Vec::new();
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            parts.push("CTRL");
        }
        if key.modifiers.contains(KeyModifiers::ALT) {
            parts.push("ALT");
        }
        if key.modifiers.contains(KeyModifiers::SHIFT) {
            parts.push("SHIFT");
        }
        if key.modifiers.contains(KeyModifiers::SUPER) {
            parts.push("SUPER");
        }
        parts.join("+")
    };

    format!(
        "key      code={:<12} modifiers={:<16} chord={}",
        format!("{:?}", key.code),
        modifiers,
        chord_name(key)
    )
}

/// Render a key event in the `C-b` / `M-x` notation used by the `mux_prefix` config.
pub fn chord_name(key: &KeyEvent) -> String {
    let mut name = String::new();
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        name.push_str("C-");
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        name.push_str("M-");
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) && !matches!(key.code, KeyCode::Char(_)) {
        name.push_str("S-");
    }
    match key.code {
        KeyCode::Char(' ') => name.push_str("Space"),
        KeyCode::Char(c) => name.push(c),
        other => name.push_str(&format!("{other:?}")),
    }
    name
}
