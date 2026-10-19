//! Stands in for the Copilot CLI, so a run never reaches a real model or a real
//! session. The driver copies its own executable into the run's `bin/` under the
//! name `copilot`, and this is what that copy does.

use std::io::Write;
use std::time::{Duration, Instant};

/// CST normally ends its panes, and killing CST takes its pseudo-terminals and
/// so these with it. This is only the backstop for a stub that outlives both.
const LIFETIME: Duration = Duration::from_secs(30 * 60);

pub fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--version") {
        println!("1.0.40");
        return;
    }
    // `copilot plugin ...` is CST installing its hook plugin; succeeding quietly
    // is all it needs.
    if args.first().is_some_and(|arg| arg == "plugin") {
        return;
    }

    print!("\x1b[2J\x1b[H");
    println!("Fake Copilot  args: {}", args.join(" "));
    match std::env::current_dir() {
        Ok(dir) => println!("cwd: {}", dir.display()),
        Err(error) => println!("cwd: <{error}>"),
    }
    println!();
    println!("Fake Copilot is idling.");
    println!();
    print!("> ");
    let _ = std::io::stdout().flush();

    let started = Instant::now();
    while started.elapsed() < LIFETIME {
        std::thread::sleep(Duration::from_secs(5));
    }
}
