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

    // Shown back on screen, so a script can see what CST wrote into the session —
    // a thread wake-up, say — and not only that the session is open.
    //
    // Lines starting with `e2e:` are stage directions for the stub itself, typed by a
    // script, so it can act out what the real Copilot does without a model:
    //   e2e:say <text>          write an assistant message to the session's event log
    //   e2e:work <ms> [<text>]  report a turn over OSC progress that ends after <ms>,
    //                           saying <text> while it runs
    //   e2e:bell         ring the bell, which is how a turn's end is also reported
    let events_path = events_path(&args);
    std::thread::spawn(move || {
        for line in std::io::stdin().lines() {
            let Ok(line) = line else { break };
            let line = line.trim_end().to_string();
            println!("Fake Copilot received: {line}");
            if let Some(direction) = line.strip_prefix("e2e:") {
                act(direction, events_path.as_deref());
            }
            print!("> ");
            let _ = std::io::stdout().flush();
        }
    });

    let started = Instant::now();
    while started.elapsed() < LIFETIME {
        std::thread::sleep(Duration::from_secs(5));
    }
}

/// Where the real Copilot would write this session's events: under `COPILOT_HOME`,
/// in the folder named by the `--resume=<id>` CST passes.
fn events_path(args: &[String]) -> Option<std::path::PathBuf> {
    let home = std::env::var_os("COPILOT_HOME")?;
    let id = args
        .iter()
        .find_map(|arg| arg.strip_prefix("--resume=").or_else(|| arg.strip_prefix("--session-id=")))?;
    Some(
        std::path::PathBuf::from(home)
            .join("session-state")
            .join(id)
            .join("events.jsonl"),
    )
}

fn act(direction: &str, events_path: Option<&std::path::Path>) {
    let (verb, rest) = direction.split_once(' ').unwrap_or((direction, ""));
    match verb {
        "say" => {
            let Some(path) = events_path else { return };
            let record = format!(
                r#"{{"type":"assistant.message","data":{{"content":{}}}}}"#,
                crate::telegram_stub::json_string(rest)
            );
            if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path)
            {
                let _ = writeln!(file, "{record}");
            }
        }
        "work" => {
            let (millis, said) = rest.trim().split_once(' ').unwrap_or((rest.trim(), ""));
            let millis: u64 = millis.parse().unwrap_or(1000);
            // Indeterminate progress, then clear: the shape of a turn as Copilot
            // reports it to the terminal. Anything said is written while the turn is
            // running, which is when the real Copilot writes its messages.
            print!("\x1b]9;4;3;0\x1b\\");
            let _ = std::io::stdout().flush();
            std::thread::sleep(Duration::from_millis(millis));
            if !said.is_empty() {
                act(&format!("say {said}"), events_path);
            }
            print!("\x1b]9;4;0;0\x1b\\");
            println!("Fake Copilot finished a turn.");
            let _ = std::io::stdout().flush();
        }
        "bell" => {
            print!("\x07");
            let _ = std::io::stdout().flush();
        }
        _ => {}
    }
}
