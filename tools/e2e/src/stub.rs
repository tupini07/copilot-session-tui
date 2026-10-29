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
    // `copilot --server --stdio`: the Copilot SDK starting the CLI for CST's side
    // questions, such as a topic's emoji. Spoken over the pipes, never a terminal.
    if args.iter().any(|arg| arg == "--server") {
        super::sdk_server::main();
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
    //   e2e:ask <ms> <text> | <choice> | <choice>...
    //                    start a turn and, after <ms>, ask the user <text> with those
    //                    choices through the ask_user tool, as the real Copilot's
    //                    event log records it; the turn stays open until e2e:answered
    //   e2e:answered <text>  record the answer to the open question and end the turn
    //   e2e:bell         ring the bell, which is how a turn's end is also reported
    //   e2e:shell <id> [async|detached]
    //                    record a shell command left running in the background under
    //                    that id, in the words the real Copilot's log uses: one it
    //                    backgrounded for overrunning (the default), one the agent
    //                    started async, or one started detached
    //   e2e:shell_done <id>  record the completion notice Copilot writes as it wakes
    //                    the agent with that shell's result
    //   e2e:stop <id>    record the agent stopping that shell
    //   e2e:hook <working|ready> [<ms>]
    //                    write what CST's hook plugin would have, beside the event log,
    //                    so a script can drive the regime where hooks decide the state;
    //                    after <ms>, if given, so the script can leave the pane first
    let events_path = events_path(&args);
    enable_virtual_terminal_input();
    std::thread::spawn(move || {
        for line in std::io::stdin().lines() {
            let Ok(line) = line else { break };
            let line = line.trim_end().to_string();
            // Escape on its own, as CST presses it for the phone's /esc. The console is
            // in line mode, so the byte waits for the next Enter and arrives as a line
            // of its own; a script sends that Enter and looks for this.
            if line == "\x1b" {
                println!("Fake Copilot received: <esc>");
                print!("> ");
                let _ = std::io::stdout().flush();
                continue;
            }
            // While a question is open the stub is the form: arrows move a highlight
            // over the choices and the Other row, text typed on Other is the answer,
            // Enter accepts. The e2e driver and CST both send arrows as escape
            // sequences inside the line, which is all the stub needs to count them.
            if let Some(answer) = answer_form(&line) {
                println!("Fake Copilot answered: {answer}");
                act(&format!("answered {answer}"), events_path.as_deref());
                print!("> ");
                let _ = std::io::stdout().flush();
                continue;
            }
            println!("Fake Copilot received: {line}");
            if let Some(direction) = line.strip_prefix("e2e:") {
                act(direction, events_path.as_deref());
            } else if line.starts_with("A new comment arrived on") {
                // A thread wake-up. The real Copilot reads the thread and works for a
                // while; acting that out is what lets a script see how CST treats the
                // turn's end. Long enough for the script to look away first.
                act("work 3000 Handled the comment.", events_path.as_deref());
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

/// The first offered emoji not listed as taken, from CST's prompt: the offered ones
/// follow the line ending in "the title:", the taken ones the line ending in "fits:".
pub(crate) fn choose_emoji(prompt: &str) -> String {
    let list_after = |marker: &str| -> Vec<String> {
        prompt
            .split_inclusive('\n')
            .skip_while(|line| !line.trim_end().ends_with(marker))
            .nth(1)
            .map(|line| line.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default()
    };
    let offered = list_after("the title:");
    let taken = list_after("fits:");
    offered
        .into_iter()
        .find(|emoji| !taken.contains(emoji))
        .unwrap_or_else(|| "🚀".to_string())
}

/// Where the real Copilot would write this session's events: under `COPILOT_HOME`,
/// in the folder named by the `--resume=<id>` CST passes.
fn events_path(args: &[String]) -> Option<std::path::PathBuf> {
    let home = std::env::var_os("COPILOT_HOME")?;
    let id = args.iter().find_map(|arg| {
        arg.strip_prefix("--resume=")
            .or_else(|| arg.strip_prefix("--session-id="))
    })?;
    Some(
        std::path::PathBuf::from(home)
            .join("session-state")
            .join(id)
            .join("events.jsonl"),
    )
}

fn now_stamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn append_event(path: &std::path::Path, record: &str) {
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{record}");
    }
}

fn act(direction: &str, events_path: Option<&std::path::Path>) {
    let (verb, rest) = direction.split_once(' ').unwrap_or((direction, ""));
    match verb {
        "say" => {
            let Some(path) = events_path else { return };
            // A script types one line; a literal `\n` in it is a line break in what the
            // session said, which is how a table or a list gets into a message.
            let said = rest.replace("\\n", "\n");
            let record = format!(
                r#"{{"type":"assistant.message","timestamp":"{}","data":{{"content":{}}}}}"#,
                now_stamp(),
                crate::telegram_stub::json_string(&said)
            );
            append_event(path, &record);
        }
        "work" => {
            let (millis, said) = rest.trim().split_once(' ').unwrap_or((rest.trim(), ""));
            let millis: u64 = millis.parse().unwrap_or(1000);
            // Indeterminate progress, then clear: the shape of a turn as Copilot
            // reports it to the terminal. Anything said is written while the turn is
            // running, which is when the real Copilot writes its messages. The turn's
            // start goes into the log as the real Copilot's does, with a timestamp, which
            // is how CST tells from the folder alone that this session is busy.
            print!("\x1b]9;4;3;0\x1b\\");
            let _ = std::io::stdout().flush();
            if let Some(path) = events_path {
                append_event(
                    path,
                    &format!(
                        r#"{{"type":"assistant.turn_start","timestamp":"{}","data":{{}}}}"#,
                        now_stamp()
                    ),
                );
            }
            std::thread::sleep(Duration::from_millis(millis));
            if !said.is_empty() {
                act(&format!("say {said}"), events_path);
            }
            print!("\x1b]9;4;0;0\x1b\\");
            println!("Fake Copilot finished a turn.");
            let _ = std::io::stdout().flush();
        }
        "ask" => {
            let (millis, spec) = rest.trim().split_once(' ').unwrap_or(("0", rest.trim()));
            let millis: u64 = millis.parse().unwrap_or(0);
            let mut parts = spec.split('|').map(str::trim);
            let text = parts.next().unwrap_or("");
            let choices: Vec<String> = parts
                .filter(|choice| !choice.is_empty())
                .map(crate::telegram_stub::json_string)
                .collect();
            // The turn is open while the question is: the real Copilot asks from
            // inside a tool call, so progress stays on until the answer arrives.
            print!("\x1b]9;4;3;0\x1b\\");
            let _ = std::io::stdout().flush();
            std::thread::sleep(Duration::from_millis(millis));
            let Some(path) = events_path else { return };
            let number = ASKED.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            let id = format!("call_e2e_question_{number}");
            let record = format!(
                r#"{{"type":"tool.execution_start","data":{{"toolCallId":"{id}","toolName":"ask_user","arguments":{{"message":{},"requestedSchema":{{"properties":{{"answer":{{"type":"string","title":"Your answer","enum":[{}]}}}}}}}}}}}}"#,
                crate::telegram_stub::json_string(text),
                choices.join(",")
            );
            if let Ok(mut open) = OPEN_QUESTION.lock() {
                *open = Some((
                    id,
                    spec.split('|')
                        .skip(1)
                        .map(str::trim)
                        .filter(|choice| !choice.is_empty())
                        .map(str::to_string)
                        .collect(),
                ));
            }
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = writeln!(file, "{record}");
            }
            println!("Fake Copilot asked: {text}");
            let _ = std::io::stdout().flush();
        }
        "answered" => {
            if let Some(path) = events_path {
                let id = question_id();
                if let Ok(mut open) = OPEN_QUESTION.lock() {
                    *open = None;
                }
                let record = format!(
                    r#"{{"type":"tool.execution_complete","data":{{"toolCallId":"{id}","result":{{"content":{}}}}}}}"#,
                    crate::telegram_stub::json_string(&format!("User responded: {}", rest.trim()))
                );
                if let Ok(mut file) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                {
                    let _ = writeln!(file, "{record}");
                }
            }
            print!("\x1b]9;4;0;0\x1b\\");
            println!("Fake Copilot finished a turn.");
            let _ = std::io::stdout().flush();
        }
        "bell" => {
            print!("\x07");
            let _ = std::io::stdout().flush();
        }
        // The sentences and the telemetry are copied from a real session's log; they
        // are what CST reads, so the stub has to write them to the letter.
        "shell" => {
            let Some(path) = events_path else { return };
            let (id, mode) = rest.trim().split_once(' ').unwrap_or((rest.trim(), ""));
            let (content, execution_mode, detached) = match mode.trim() {
                "async" => (
                    format!("<command started in background with shellId: {id}>"),
                    "async",
                    "false",
                ),
                "detached" => (
                    format!("<command started in detached background with shellId: {id}>"),
                    "async",
                    "true",
                ),
                _ => (
                    format!("<command with shellId: {id} is still running after 30 seconds. The command is still running but hasn't produced output yet. You will be automatically notified when it completes; if you need the output sooner, read the shell by its id>"),
                    "sync",
                    "false",
                ),
            };
            let call = format!("call_e2e_shell_{id}");
            append_event(
                path,
                &format!(
                    r#"{{"type":"tool.execution_start","timestamp":"{}","data":{{"toolCallId":"{call}","toolName":"powershell","arguments":{{"command":"private command","shellId":"{id}"}}}}}}"#,
                    now_stamp()
                ),
            );
            append_event(
                path,
                &format!(
                    r#"{{"type":"tool.execution_complete","timestamp":"{}","data":{{"toolCallId":"{call}","success":true,"result":{{"content":{}}},"toolTelemetry":{{"properties":{{"executionMode":"{execution_mode}","detached":"{detached}"}}}}}}}}"#,
                    now_stamp(),
                    crate::telegram_stub::json_string(&content)
                ),
            );
            println!("Fake Copilot left shell {id} running");
            let _ = std::io::stdout().flush();
        }
        "shell_done" => {
            let Some(path) = events_path else { return };
            let id = rest.trim();
            let content = format!(
                "<system_notification>\nShell command \"private description\" (shellId: {id}) has completed successfully.\n</system_notification>"
            );
            append_event(
                path,
                &format!(
                    r#"{{"type":"system.notification","timestamp":"{}","data":{{"content":{},"kind":{{"type":"shell_completed","shellId":"{id}","exitCode":0}}}}}}"#,
                    now_stamp(),
                    crate::telegram_stub::json_string(&content)
                ),
            );
            println!("Fake Copilot heard shell {id} finish");
            let _ = std::io::stdout().flush();
        }
        "stop" => {
            let Some(path) = events_path else { return };
            let id = rest.trim();
            let call = format!("call_e2e_stop_{id}");
            append_event(
                path,
                &format!(
                    r#"{{"type":"tool.execution_start","timestamp":"{}","data":{{"toolCallId":"{call}","toolName":"stop_powershell","arguments":{{"shellId":"{id}"}}}}}}"#,
                    now_stamp()
                ),
            );
            append_event(
                path,
                &format!(
                    r#"{{"type":"tool.execution_complete","timestamp":"{}","data":{{"toolCallId":"{call}","success":true,"result":{{"content":"<shellId: {id} stopped>"}}}}}}"#,
                    now_stamp()
                ),
            );
        }
        "hook" => {
            // What `copilot-session-tui hook-event working|ready` appends when the real
            // Copilot runs the plugin's hooks. Milliseconds, like the hook's own clock,
            // and never earlier than the last: CST ignores a hook that goes backwards.
            let Some(path) = events_path else { return };
            let (state, delay) = rest.trim().split_once(' ').unwrap_or((rest.trim(), ""));
            let state = match state {
                "working" => "working",
                "ready" => "ready",
                _ => return,
            };
            // An optional pause first, so a script can leave the pane before the hook
            // lands: whether a turn's end is worth telling the phone about is decided
            // by where the user is when CST confirms it, not when the key was pressed.
            if let Ok(millis) = delay.trim().parse::<u64>() {
                std::thread::sleep(Duration::from_millis(millis));
            }
            let lifecycle = path.with_file_name(".cst-lifecycle.jsonl");
            let millis = chrono::Utc::now().timestamp_millis();
            append_event(
                &lifecycle,
                &format!(r#"{{"state":"{state}","timestamp":{millis}}}"#),
            );
            println!("Fake Copilot ran the {state} hook");
            let _ = std::io::stdout().flush();
        }
        _ => {}
    }
}

/// The question that is open, if one is: its tool call id and its choices. Set by
/// `e2e:ask`, cleared by the answer. Every question gets a new id, because CST's
/// lifecycle monitor ignores a start record whose id it has already seen.
static OPEN_QUESTION: std::sync::Mutex<Option<(String, Vec<String>)>> = std::sync::Mutex::new(None);
static ASKED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The id of the question that is open, or of the last one asked.
fn question_id() -> String {
    if let Some((id, _)) = OPEN_QUESTION.lock().ok().and_then(|open| open.clone()) {
        return id;
    }
    format!(
        "call_e2e_question_{}",
        ASKED.load(std::sync::atomic::Ordering::SeqCst)
    )
}

/// Make arrow keys arrive on stdin as escape sequences.
///
/// Under ConPTY a console reader gets arrows as key events and sees nothing on stdin
/// unless it asks for virtual-terminal input, which Node (and so the real Copilot)
/// does. Without this the form's highlight never moved and every answer was the first
/// choice. Elsewhere the pty delivers the bytes as they are.
#[cfg(windows)]
fn enable_virtual_terminal_input() {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn GetConsoleMode(handle: *mut c_void, mode: *mut u32) -> i32;
        fn SetConsoleMode(handle: *mut c_void, mode: u32) -> i32;
    }
    const STD_INPUT_HANDLE: u32 = -10i32 as u32;
    const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;
    // SAFETY: plain Win32 calls on the process's own standard input handle.
    unsafe {
        let handle = GetStdHandle(STD_INPUT_HANDLE);
        let mut mode = 0u32;
        if GetConsoleMode(handle, &mut mode) != 0 {
            SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_INPUT);
        }
    }
}

#[cfg(not(windows))]
fn enable_virtual_terminal_input() {}

/// Play the form for one line of input: arrows move the highlight from the first row,
/// text typed while the Other row is highlighted is the answer, Enter (the line's end)
/// accepts. Returns what the real Copilot would record, or `None` when no question is
/// open.
fn answer_form(line: &str) -> Option<String> {
    let (_, choices) = OPEN_QUESTION.lock().ok()?.clone()?;
    let mut highlight = 0usize;
    let mut typed = String::new();
    let mut rest = line;
    while !rest.is_empty() {
        if let Some(after) = rest
            .strip_prefix("\x1b[A")
            .or_else(|| rest.strip_prefix("\x1bOA"))
        {
            highlight = highlight.saturating_sub(1);
            rest = after;
        } else if let Some(after) = rest
            .strip_prefix("\x1b[B")
            .or_else(|| rest.strip_prefix("\x1bOB"))
        {
            highlight = (highlight + 1).min(choices.len());
            rest = after;
        } else {
            let mut characters = rest.chars();
            let character = characters.next()?;
            if highlight == choices.len() && !character.is_control() {
                typed.push(character);
            }
            rest = characters.as_str();
        }
    }
    let answer = if highlight == choices.len() {
        typed.trim().to_string()
    } else {
        choices[highlight].clone()
    };
    if answer.is_empty() {
        // Enter on the Other row with nothing typed: the real form refuses it.
        return None;
    }
    *OPEN_QUESTION.lock().ok()? = None;
    Some(answer)
}
