//! Drives the real CST binary in a pseudo-terminal from a script, and captures
//! what it shows as text, HTML and PNG. See `tools/e2e/README.md`.

mod gh_stub;
mod screen;
mod sdk_server;
mod stub;
mod telegram_stub;

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const USAGE: &str = "\
usage: cargo run --manifest-path tools/e2e/Cargo.toml -- <script> [options]

options:
  --no-build     use target/debug as it is instead of running `cargo build` first
  --exe <path>   drive this CST executable (implies --no-build)
  --out <dir>    write the run here instead of <temp>/cst-e2e/<script name>
  --no-png       skip the PNG screenshots (the text and HTML are still written)";

/// Enough for ConPTY to deliver one write before the next, which it otherwise
/// merges: an Enter sent in the same write as typed text is lost.
const DEFAULT_PACE: Duration = Duration::from_millis(250);
/// A lone Esc is held back until the terminal is sure it does not start a
/// sequence, so whatever follows too soon is read as Alt+key.
const ESC_SETTLE: Duration = Duration::from_millis(600);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

fn main() {
    let invoked_as = std::env::current_exe().ok().and_then(|exe| {
        exe.file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
    });
    if invoked_as.as_deref() == Some("gh") {
        gh_stub::main();
        return;
    }
    if invoked_as.as_deref() == Some("copilot") {
        stub::main();
        return;
    }

    let code = match run() {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(error) => {
            eprintln!("error: {error}");
            2
        }
    };
    std::process::exit(code);
}

struct Options {
    script: PathBuf,
    build: bool,
    png: bool,
    exe: Option<PathBuf>,
    out: Option<PathBuf>,
}

fn parse_options() -> Result<Options, String> {
    let mut args = std::env::args().skip(1);
    let mut options = Options {
        script: PathBuf::new(),
        build: true,
        png: true,
        exe: None,
        out: None,
    };
    let mut script = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--no-build" => options.build = false,
            "--no-png" => options.png = false,
            "--exe" => {
                options.exe = Some(args.next().ok_or("--exe needs a path")?.into());
                options.build = false;
            }
            "--out" => options.out = Some(args.next().ok_or("--out needs a path")?.into()),
            "-h" | "--help" => return Err(USAGE.to_string()),
            _ if arg.starts_with('-') => return Err(format!("unknown option {arg}\n\n{USAGE}")),
            _ if script.is_none() => script = Some(PathBuf::from(arg)),
            _ => return Err(format!("unexpected argument {arg}\n\n{USAGE}")),
        }
    }
    options.script = script.ok_or(USAGE)?;
    Ok(options)
}

fn repo_root() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    canonical(root)
}

fn run() -> Result<bool, String> {
    let options = parse_options()?;
    let source = std::fs::read_to_string(&options.script)
        .map_err(|error| format!("cannot read {}: {error}", options.script.display()))?;
    // Every line is checked before anything starts, so a typo near the end of a
    // script does not cost a whole run to find.
    let steps = parse_script(&source)?;

    let repo = repo_root();
    if options.build {
        build_cst(&repo)?;
    }
    let exe = options.exe.clone().unwrap_or_else(|| {
        repo.join("target").join("debug").join(format!(
            "copilot-session-tui{}",
            std::env::consts::EXE_SUFFIX
        ))
    });
    if !exe.is_file() {
        return Err(format!("{} does not exist; build CST first", exe.display()));
    }

    let name = options
        .script
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "run".to_string());
    let run_dir = options
        .out
        .clone()
        // Outside the repository: CST started inside one filters the list to it,
        // which is not what a user starting it from anywhere else would see.
        .unwrap_or_else(|| std::env::temp_dir().join("cst-e2e").join(&name));
    if run_dir.exists() {
        std::fs::remove_dir_all(&run_dir)
            .map_err(|error| format!("cannot clear {}: {error}", run_dir.display()))?;
    }
    std::fs::create_dir_all(&run_dir).map_err(|error| error.to_string())?;
    let run_dir = canonical(run_dir);
    println!("run directory: {}", run_dir.display());

    let browser = if options.png {
        let found = screen::find_browser();
        if found.is_none() {
            println!(
                "note: no Chromium-based browser found, so no PNGs; set CST_E2E_BROWSER to one"
            );
        }
        found
    } else {
        None
    };

    let mut driver = Driver {
        run_dir,
        exe,
        setup: Setup::default(),
        cst: None,
        pace: DEFAULT_PACE,
        timeout: DEFAULT_TIMEOUT,
        browser,
        dumps: Vec::new(),
        config_before: None,
        telegram: None,
    };

    let mut failure = None;
    for (line, step) in &steps {
        if let Err(message) = driver.step(step) {
            failure = Some((*line, message));
            break;
        }
    }
    if failure.is_none() && driver.cst.is_none() {
        // A script of nothing but setup still means "start it and look".
        if let Err(message) = driver.launch() {
            failure = Some((0, message));
        }
    }
    if let Some((line, message)) = &failure {
        println!("FAIL at line {line}: {message}");
        if driver.cst.is_some() {
            let _ = driver.dump("failed");
        }
    }

    let clean = driver.finish();
    println!();
    for dump in &driver.dumps {
        println!("screen: {}", dump.display());
    }
    match (&failure, clean) {
        (None, true) => {
            println!("RESULT: pass");
            Ok(true)
        }
        (None, false) => {
            println!("RESULT: FAIL (the script passed, but cleaning up did not; see above)");
            Ok(false)
        }
        (Some((line, message)), _) => {
            println!("RESULT: FAIL at line {line}: {message}");
            Ok(false)
        }
    }
}

fn build_cst(repo: &Path) -> Result<(), String> {
    println!("building CST (cargo build) ...");
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command.arg("build").current_dir(repo);
    // `cargo run` described this crate to it in these; they are not CST's.
    for (key, _) in std::env::vars_os() {
        let key = key.to_string_lossy();
        if key.starts_with("CARGO_PKG_")
            || key.starts_with("CARGO_MANIFEST_")
            || key.starts_with("CARGO_BIN_")
            || key.starts_with("CARGO_CRATE_")
        {
            command.env_remove(key.as_ref());
        }
    }
    let status = command
        .status()
        .map_err(|error| format!("cannot run cargo build: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("cargo build failed".to_string())
    }
}

#[derive(Debug)]
enum Step {
    Size(u16, u16),
    Session {
        project: String,
        summary: String,
    },
    AddSession {
        project: String,
        summary: String,
    },
    DeleteSession(String),
    Project {
        name: String,
        git: bool,
    },
    Env(String, String),
    Arg(String),
    Config(String),
    FakeGh,
    WatchThread(String),
    HeldReply(String),
    /// The fake `gh` marker to leave in place, or `None` for GitHub working normally.
    Github(Option<&'static str>),
    /// A stub Bot API, with CST configured against it: paired, or with a token only.
    FakeTelegram {
        mode: telegram_stub::Mode,
        paired: bool,
    },
    /// A message into the chat: from the paired user unless `from` says otherwise, as
    /// a reply to CST's last message (or in its topic) unless `bare`.
    TelegramSay {
        from: i64,
        text: String,
        bare: bool,
    },
    /// Tap the button with this label under the latest message that has one.
    TelegramTap(String),
    /// Send the pairing code CST armed, as pressing Start would.
    TelegramPairCode,
    TelegramWait(String),
    TelegramExpect(String),
    TelegramExpectNot(String),
    TelegramOrder(Vec<String>),
    TelegramDump(String),
    /// Whether the stub says the bot may manage topics.
    TelegramRights(bool),
    Wait(Duration),
    WaitFor(String),
    WaitGone(String),
    Expect(String),
    ExpectNot(String),
    Key(Vec<u8>),
    Type(String),
    Press(Vec<String>),
    Paste(String),
    Click {
        button: u8,
        at: Target,
    },
    Scroll {
        down: bool,
        at: Target,
    },
    Dump(String),
    Pace(Duration),
    Timeout(Duration),
}

#[derive(Debug)]
enum Target {
    Cell(u16, u16),
    Text(String),
}

impl Step {
    fn is_setup(&self) -> bool {
        matches!(
            self,
            Step::Size(..)
                | Step::Session { .. }
                | Step::Project { .. }
                | Step::Env(..)
                | Step::Arg(_)
                | Step::Config(_)
                | Step::FakeGh
                | Step::WatchThread(_)
                | Step::HeldReply(_)
                | Step::FakeTelegram { .. }
        )
    }
}

fn parse_script(source: &str) -> Result<Vec<(usize, Step)>, String> {
    let mut steps = Vec::new();
    let mut launched = false;
    for (index, raw) in source.lines().enumerate() {
        let line = index + 1;
        let text = raw.trim_start().trim_end_matches('\r');
        if text.trim().is_empty() || text.starts_with('#') {
            continue;
        }
        let (command, rest) = text.split_once(' ').unwrap_or((text, ""));
        let step = parse_step(command, rest).map_err(|error| format!("line {line}: {error}"))?;
        if step.is_setup() && launched {
            return Err(format!(
                "line {line}: `{command}` sets up the run, so it has to come before the first step that drives CST"
            ));
        }
        // `pace` and `timeout` can go anywhere, so they neither start CST nor
        // close the setup.
        launched |= !step.is_setup() && !matches!(step, Step::Pace(_) | Step::Timeout(_));
        steps.push((line, step));
    }
    Ok(steps)
}

fn parse_step(command: &str, rest: &str) -> Result<Step, String> {
    let need = |what: &str| -> Result<String, String> {
        if rest.trim().is_empty() {
            Err(format!("`{command}` needs {what}"))
        } else {
            Ok(rest.trim().to_string())
        }
    };
    let millis = |text: String| -> Result<Duration, String> {
        text.parse::<u64>()
            .map(Duration::from_millis)
            .map_err(|_| format!("`{command}` wants milliseconds, got {text:?}"))
    };
    Ok(match command {
        "size" => {
            let size = need("COLSxROWS")?;
            let (columns, rows) = size
                .split_once('x')
                .ok_or_else(|| format!("size wants COLSxROWS, got {size:?}"))?;
            Step::Size(
                columns.parse().map_err(|_| "bad column count")?,
                rows.parse().map_err(|_| "bad row count")?,
            )
        }
        "session" | "add_session" => {
            let text = need("a project name and a summary")?;
            let (project, summary) = text
                .split_once(' ')
                .ok_or_else(|| format!("{command} wants a project name, then the summary"))?;
            let (project, summary) = (project.to_string(), summary.trim().to_string());
            if command == "session" {
                Step::Session { project, summary }
            } else {
                Step::AddSession { project, summary }
            }
        }
        "delete_session" => Step::DeleteSession(need("the session's summary")?),
        "project" => Step::Project {
            name: need("a name")?,
            git: false,
        },
        "git_project" => Step::Project {
            name: need("a name")?,
            git: true,
        },
        "env" => {
            let text = need("KEY=VALUE")?;
            let (key, value) = text.split_once('=').ok_or("env wants KEY=VALUE")?;
            Step::Env(key.to_string(), value.to_string())
        }
        "arg" => Step::Arg(need("an argument")?),
        "config" => Step::Config(need("the settings as JSON")?),
        "fake_gh" => Step::FakeGh,
        "watch_thread" => {
            let url = need("a thread URL")?;
            thread_json(&url, "")?;
            Step::WatchThread(url)
        }
        "held_reply" => {
            let spec = need("a thread URL")?;
            held_reply_json(&spec, "")?;
            Step::HeldReply(spec)
        }
        "github" => match need("down, logged-out or up")?.as_str() {
            "down" => Step::Github(Some(gh_stub::DOWN_MARKER)),
            "logged-out" => Step::Github(Some(gh_stub::LOGGED_OUT_MARKER)),
            "up" => Step::Github(None),
            other => return Err(format!("github goes down, logged-out or up, not {other:?}")),
        },
        "fake_telegram" => {
            let mut mode = telegram_stub::Mode::Private;
            let mut paired = true;
            for word in rest.split_whitespace() {
                match word {
                    "private" => mode = telegram_stub::Mode::Private,
                    "forum" => mode = telegram_stub::Mode::Forum,
                    "unpaired" => paired = false,
                    other => {
                        return Err(format!(
                            "fake_telegram takes `private` or `forum`, and `unpaired`, not {other:?}"
                        ))
                    }
                }
            }
            Step::FakeTelegram { mode, paired }
        }
        "telegram_say" => Step::TelegramSay {
            from: telegram_stub::USER_ID,
            text: need("text")?,
            bare: false,
        },
        "telegram_say_bare" => Step::TelegramSay {
            from: telegram_stub::USER_ID,
            text: need("text")?,
            bare: true,
        },
        "telegram_say_from" => {
            let text = need("a user id, then text")?;
            let (from, text) = text
                .split_once(' ')
                .ok_or("telegram_say_from wants a user id, then the text")?;
            Step::TelegramSay {
                from: from
                    .parse()
                    .map_err(|_| format!("telegram_say_from wants a numeric user id, got {from:?}"))?,
                text: text.trim().to_string(),
                bare: false,
            }
        }
        "telegram_tap" => Step::TelegramTap(need("a button label")?),
        "telegram_pair_code" => Step::TelegramPairCode,
        "telegram_wait" => Step::TelegramWait(need("text")?),
        "telegram_expect" => Step::TelegramExpect(need("text")?),
        "telegram_expect_not" => Step::TelegramExpectNot(need("text")?),
        "telegram_order" => {
            let texts: Vec<String> = need("texts separated by ||")?
                .split("||")
                .map(|text| text.trim().to_string())
                .filter(|text| !text.is_empty())
                .collect();
            if texts.len() < 2 {
                return Err("telegram_order needs at least two texts, separated by ||".to_string());
            }
            Step::TelegramOrder(texts)
        }
        "telegram_dump" => Step::TelegramDump(need("a name")?),
        "telegram_rights" => match need("on or off")?.as_str() {
            "on" => Step::TelegramRights(true),
            "off" => Step::TelegramRights(false),
            other => return Err(format!("telegram_rights goes on or off, not {other:?}")),
        },
        "wait" => Step::Wait(millis(need("milliseconds")?)?),
        "wait_for" => Step::WaitFor(need("text")?),
        "wait_gone" => Step::WaitGone(need("text")?),
        "expect" => Step::Expect(need("text")?),
        "expect_not" => Step::ExpectNot(need("text")?),
        "key" => Step::Key(unescape(&need("bytes")?)?),
        // Not trimmed: a trailing space may be what is being typed.
        "type" => Step::Type(rest.to_string()),
        "press" => {
            let names: Vec<String> = need("key names")?
                .split_whitespace()
                .map(str::to_string)
                .collect();
            for name in &names {
                key_bytes(name)?;
            }
            Step::Press(names)
        }
        "paste" => Step::Paste(rest.to_string()),
        "click" | "click_on" => {
            let text = need("a place")?;
            let (button, place) = match text.split_once(' ') {
                Some(("left", place)) => (0, place),
                Some(("middle", place)) => (1, place),
                Some(("right", place)) => (2, place),
                _ => (0, text.as_str()),
            };
            Step::Click {
                button,
                at: target(command == "click_on", place)?,
            }
        }
        "scroll" | "scroll_on" => {
            let text = need("up or down, then a place")?;
            let (direction, place) = text
                .split_once(' ')
                .ok_or("scroll wants up or down, then a place")?;
            let down = match direction {
                "up" => false,
                "down" => true,
                other => return Err(format!("scroll goes up or down, not {other:?}")),
            };
            Step::Scroll {
                down,
                at: target(command == "scroll_on", place)?,
            }
        }
        "dump" => Step::Dump(need("a name")?),
        "pace" => Step::Pace(millis(need("milliseconds")?)?),
        "timeout" => Step::Timeout(millis(need("milliseconds")?)?),
        other => {
            return Err(format!(
                "unknown command `{other}`; see tools/e2e/README.md"
            ))
        }
    })
}

fn target(by_text: bool, place: &str) -> Result<Target, String> {
    if by_text {
        return Ok(Target::Text(place.trim().to_string()));
    }
    let mut parts = place.split_whitespace();
    let (Some(column), Some(row), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(format!("wants a zero-based COLUMN ROW, got {place:?}"));
    };
    Ok(Target::Cell(
        column.parse().map_err(|_| "bad column")?,
        row.parse().map_err(|_| "bad row")?,
    ))
}

/// `\e` Esc, `\r`, `\n`, `\t`, `\s` space, `\\`, and `\xHH` for any byte.
fn unescape(text: &str) -> Result<Vec<u8>, String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'\\' || index + 1 == bytes.len() {
            out.push(bytes[index]);
            index += 1;
            continue;
        }
        match bytes[index + 1] {
            b'e' => out.push(0x1b),
            b'r' => out.push(b'\r'),
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b's' => out.push(b' '),
            b'\\' => out.push(b'\\'),
            b'x' => {
                let hex = text
                    .get(index + 2..index + 4)
                    .ok_or("\\x wants two hex digits")?;
                out.push(u8::from_str_radix(hex, 16).map_err(|_| format!("bad \\x{hex}"))?);
                index += 4;
                continue;
            }
            other => return Err(format!("unknown escape \\{}", other as char)),
        }
        index += 2;
    }
    Ok(out)
}

/// What a terminal sends for a named key, as xterm and ConPTY spell it.
fn key_bytes(name: &str) -> Result<Vec<u8>, String> {
    let lower = name.to_ascii_lowercase();
    if let Some(letter) = lower.strip_prefix("ctrl+") {
        let [byte] = letter.as_bytes() else {
            return Err(format!("{name}: ctrl+ takes one letter"));
        };
        if !byte.is_ascii_lowercase() {
            return Err(format!("{name}: ctrl+ takes a letter"));
        }
        return Ok(vec![byte & 0x1f]);
    }
    if let Some(rest) = name.strip_prefix("alt+") {
        let mut bytes = vec![0x1b];
        bytes.extend(rest.as_bytes());
        return Ok(bytes);
    }
    Ok(match lower.as_str() {
        "enter" => b"\r".to_vec(),
        "esc" => b"\x1b".to_vec(),
        "tab" => b"\t".to_vec(),
        "backtab" | "shift+tab" => b"\x1b[Z".to_vec(),
        "backspace" => b"\x7f".to_vec(),
        "space" => b" ".to_vec(),
        "up" => b"\x1b[A".to_vec(),
        "down" => b"\x1b[B".to_vec(),
        "right" => b"\x1b[C".to_vec(),
        "left" => b"\x1b[D".to_vec(),
        "home" => b"\x1b[H".to_vec(),
        "end" => b"\x1b[F".to_vec(),
        "pageup" => b"\x1b[5~".to_vec(),
        "pagedown" => b"\x1b[6~".to_vec(),
        "delete" => b"\x1b[3~".to_vec(),
        "f1" => b"\x1bOP".to_vec(),
        "f2" => b"\x1bOQ".to_vec(),
        "f3" => b"\x1bOR".to_vec(),
        "f4" => b"\x1bOS".to_vec(),
        // The gaps in the numbering (16, 22) are xterm's, not a mistake.
        "f5" => b"\x1b[15~".to_vec(),
        "f6" => b"\x1b[17~".to_vec(),
        "f7" => b"\x1b[18~".to_vec(),
        "f8" => b"\x1b[19~".to_vec(),
        "f9" => b"\x1b[20~".to_vec(),
        "f10" => b"\x1b[21~".to_vec(),
        "f11" => b"\x1b[23~".to_vec(),
        "f12" => b"\x1b[24~".to_vec(),
        _ if name.chars().count() == 1 => name.as_bytes().to_vec(),
        _ => {
            return Err(format!(
                "unknown key name {name:?}; see tools/e2e/README.md"
            ))
        }
    })
}

#[derive(Default)]
struct Setup {
    size: Option<(u16, u16)>,
    sessions: Vec<(String, String)>,
    projects: Vec<(String, bool)>,
    env: Vec<(String, String)>,
    args: Vec<String>,
    config: Option<String>,
    fake_gh: bool,
    watched: Vec<String>,
    held: Vec<String>,
    telegram: Option<(telegram_stub::Mode, bool)>,
}

struct Running {
    parser: Arc<Mutex<vt100::Parser>>,
    raw: Arc<Mutex<Vec<u8>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Box<dyn Child + Send + Sync>,
    // Dropping the master closes the terminal under CST, so it is kept.
    _master: Box<dyn MasterPty + Send>,
    size: (u16, u16),
}

struct Driver {
    run_dir: PathBuf,
    exe: PathBuf,
    setup: Setup,
    cst: Option<Running>,
    pace: Duration,
    timeout: Duration,
    browser: Option<PathBuf>,
    dumps: Vec<PathBuf>,
    config_before: Option<Option<Vec<u8>>>,
    telegram: Option<telegram_stub::TelegramStub>,
}

impl Driver {
    fn step(&mut self, step: &Step) -> Result<(), String> {
        match step {
            Step::Size(columns, rows) => self.setup.size = Some((*columns, *rows)),
            Step::Session { project, summary } => {
                self.setup.sessions.push((project.clone(), summary.clone()))
            }
            Step::Project { name, git } => self.setup.projects.push((name.clone(), *git)),
            Step::Env(key, value) => self.setup.env.push((key.clone(), value.clone())),
            Step::Arg(arg) => self.setup.args.push(arg.clone()),
            Step::Config(json) => self.setup.config = Some(json.clone()),
            Step::FakeGh => self.setup.fake_gh = true,
            Step::WatchThread(url) => self.setup.watched.push(url.clone()),
            Step::HeldReply(url) => self.setup.held.push(url.clone()),
            Step::FakeTelegram { mode, paired } => self.setup.telegram = Some((*mode, *paired)),
            Step::Pace(pace) => self.pace = *pace,
            Step::Timeout(timeout) => self.timeout = *timeout,
            _ => {
                if self.cst.is_none() {
                    self.launch()?;
                }
                return self.drive(step);
            }
        }
        Ok(())
    }

    fn drive(&mut self, step: &Step) -> Result<(), String> {
        match step {
            Step::WaitFor(text) => self.wait_until(text, true),
            Step::WaitGone(text) => self.wait_until(text, false),
            Step::Expect(text) => match self.find(text) {
                Some(_) => Ok(()),
                None => Err(format!("expected {text:?} on screen")),
            },
            Step::ExpectNot(text) => match self.find(text) {
                None => Ok(()),
                Some((column, row)) => Err(format!(
                    "did not expect {text:?} on screen, found at column {column} row {row}"
                )),
            },
            Step::Key(bytes) => self.send(bytes),
            Step::Type(text) => self.send(text.as_bytes()),
            Step::Press(names) => {
                for name in names {
                    self.send(&key_bytes(name)?)?;
                }
                Ok(())
            }
            Step::Paste(text) => self.send(format!("\x1b[200~{text}\x1b[201~").as_bytes()),
            Step::Click { button, at } => {
                let (column, row) = self.locate(at)?;
                // SGR mouse reports are one-based.
                let (x, y) = (column + 1, row + 1);
                self.send(format!("\x1b[<{button};{x};{y}M\x1b[<{button};{x};{y}m").as_bytes())
            }
            Step::Scroll { down, at } => {
                let (column, row) = self.locate(at)?;
                let button = if *down { 65 } else { 64 };
                self.send(format!("\x1b[<{button};{};{}M", column + 1, row + 1).as_bytes())
            }
            Step::Wait(duration) => {
                std::thread::sleep(*duration);
                Ok(())
            }
            Step::Dump(name) => self.dump(name),
            // As if another CST, or `copilot` in another terminal, had done it while
            // this one was running.
            Step::AddSession { project, summary } => {
                self.setup.sessions.push((project.clone(), summary.clone()));
                self.write_session(self.setup.sessions.len() - 1)
            }
            Step::DeleteSession(summary) => {
                let index = self
                    .setup
                    .sessions
                    .iter()
                    .position(|(_, existing)| existing == summary)
                    .ok_or_else(|| format!("no session with the summary {summary:?}"))?;
                let dir = self
                    .run_dir
                    .join("home")
                    .join("session-state")
                    .join(session_id(index));
                std::fs::remove_dir_all(&dir)
                    .map_err(|error| format!("cannot delete {}: {error}", dir.display()))
            }
            Step::Github(state) => {
                if !self.setup.fake_gh {
                    return Err("`github` only affects the fake `gh`; add `fake_gh`".to_string());
                }
                // One state at a time, so each command replaces the last.
                for marker in [gh_stub::DOWN_MARKER, gh_stub::LOGGED_OUT_MARKER] {
                    let path = self.run_dir.join(marker);
                    let result = if *state == Some(marker) {
                        std::fs::write(&path, b"")
                    } else {
                        match std::fs::remove_file(&path) {
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                            other => other,
                        }
                    };
                    result.map_err(|error| format!("cannot flip {}: {error}", path.display()))?;
                }
                Ok(())
            }
            Step::TelegramSay { from, text, bare } => {
                let stub = self.telegram_stub()?;
                let (reply_to, thread_id) = if *bare {
                    (None, None)
                } else {
                    match stub.last_notification() {
                        Some(sent) => match stub.mode {
                            // In a forum the topic is the address; a reply inside it
                            // would say the same thing twice.
                            telegram_stub::Mode::Forum if sent.thread_id.is_some() => {
                                (None, sent.thread_id)
                            }
                            _ => (Some(sent.message_id), None),
                        },
                        None => {
                            return Err(
                                "telegram_say replies to CST's last message, and CST has sent nothing yet; use telegram_say_bare"
                                    .to_string(),
                            )
                        }
                    }
                };
                stub.say(*from, text, reply_to, thread_id);
                std::thread::sleep(self.pace);
                Ok(())
            }
            Step::TelegramTap(label) => {
                self.telegram_stub()?.tap(label)?;
                std::thread::sleep(self.pace);
                Ok(())
            }
            Step::TelegramPairCode => {
                let code = self.armed_pairing_code()?;
                let stub = self.telegram_stub()?;
                stub.say(telegram_stub::USER_ID, &format!("/start {code}"), None, None);
                std::thread::sleep(self.pace);
                Ok(())
            }
            Step::TelegramWait(text) => {
                let started = Instant::now();
                loop {
                    if self.telegram_stub()?.outbox().iter().any(|line| line.contains(text.as_str())) {
                        return Ok(());
                    }
                    if started.elapsed() > self.timeout {
                        return Err(format!(
                            "waited {}ms for Telegram to receive {text:?}; it has:\n{}",
                            self.timeout.as_millis(),
                            self.telegram_stub()?.outbox().join("\n")
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            Step::TelegramExpect(text) => {
                let outbox = self.telegram_stub()?.outbox();
                if outbox.iter().any(|line| line.contains(text.as_str())) {
                    Ok(())
                } else {
                    Err(format!(
                        "expected Telegram to have received {text:?}; it has:\n{}",
                        outbox.join("\n")
                    ))
                }
            }
            Step::TelegramExpectNot(text) => {
                let outbox = self.telegram_stub()?.outbox();
                match outbox.iter().find(|line| line.contains(text.as_str())) {
                    None => Ok(()),
                    Some(line) => Err(format!(
                        "did not expect Telegram to receive {text:?}, found {line:?}"
                    )),
                }
            }
            Step::TelegramOrder(texts) => {
                // Presence is what telegram_wait checks; this is for when the order
                // of what was received is the point, as with a table sent between the
                // paragraphs around it.
                let outbox = self.telegram_stub()?.outbox();
                let mut last = None;
                for text in texts {
                    let found = outbox
                        .iter()
                        .enumerate()
                        .skip(last.map_or(0, |at| at + 1))
                        .find(|(_, line)| line.contains(text.as_str()))
                        .map(|(at, _)| at);
                    match found {
                        Some(at) => last = Some(at),
                        None => {
                            return Err(format!(
                                "expected Telegram to receive {text:?} after what came before it; it has:\n{}",
                                outbox.join("\n")
                            ))
                        }
                    }
                }
                Ok(())
            }
            Step::TelegramDump(name) => {
                let stub = self.telegram_stub()?;
                let mut text = stub.outbox().join("\n");
                text.push_str("\n\n--- requests ---\n");
                text.push_str(&stub.requests().join("\n"));
                let path = self.run_dir.join(format!("{name}.telegram.txt"));
                std::fs::write(&path, text).map_err(|error| error.to_string())?;
                println!("dumped {name}.telegram.txt");
                Ok(())
            }
            Step::TelegramRights(admin) => {
                self.telegram_stub()?.set_admin(*admin);
                Ok(())
            }
            _ => unreachable!("setup steps are handled before launching"),
        }
    }

    fn telegram_stub(&self) -> Result<&telegram_stub::TelegramStub, String> {
        self.telegram
            .as_ref()
            .ok_or_else(|| "this needs `fake_telegram` in the setup".to_string())
    }

    /// The code CST wrote for the poller to look for, read from its state file.
    fn armed_pairing_code(&self) -> Result<String, String> {
        let path = self
            .run_dir
            .join("cst-home")
            .join("data")
            .join("copilot-session-tui")
            .join("telegram.json");
        let started = Instant::now();
        loop {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Some(at) = text.find("\"code\":") {
                    let rest = &text[at + 7..];
                    if let Some(start) = rest.find('"') {
                        let rest = &rest[start + 1..];
                        if let Some(end) = rest.find('"') {
                            return Ok(rest[..end].to_string());
                        }
                    }
                }
            }
            if started.elapsed() > self.timeout {
                return Err("CST has not armed a pairing code; open Set up… and press Enter on the Pair row first".to_string());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Write the fake Copilot session declared `index`th, by `session` or `add_session`.
    fn write_session(&self, index: usize) -> Result<(), String> {
        let (project, summary) = &self.setup.sessions[index];
        let number = index + 1;
        let id = session_id(index);
        let cwd = self.run_dir.join("projects").join(project);
        std::fs::create_dir_all(&cwd).map_err(|error| error.to_string())?;
        let dir = self.run_dir.join("home").join("session-state").join(&id);
        std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        // The first session declared is the most recently used.
        let day = 28usize.saturating_sub(number).max(1);
        let yaml = format!(
            "id: {id}\ncwd: {}\nsummary: {}\ncreated_at: 2026-01-01T10:00:00Z\nupdated_at: 2026-01-{day:02}T10:00:00Z\n",
            yaml_quote(&cwd.display().to_string()),
            yaml_quote(summary),
        );
        std::fs::write(dir.join("workspace.yaml"), yaml).map_err(|error| error.to_string())
    }

    fn launch(&mut self) -> Result<(), String> {
        let (columns, rows) = self.setup.size.unwrap_or((110, 30));
        let home = self.run_dir.join("home");
        let projects = self.run_dir.join("projects");
        let bin = self.run_dir.join("bin");
        for dir in [&home, &projects, &bin] {
            std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
        }
        std::fs::create_dir_all(home.join("session-state")).map_err(|error| error.to_string())?;

        for (name, git) in &self.setup.projects {
            let dir = projects.join(name);
            std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
            if *git {
                let status = Command::new("git")
                    .args(["init", "-q"])
                    .current_dir(&dir)
                    .status()
                    .map_err(|error| format!("git init: {error}"))?;
                if !status.success() {
                    return Err(format!("git init failed in {}", dir.display()));
                }
            }
        }
        for index in 0..self.setup.sessions.len() {
            self.write_session(index)?;
        }

        let me = std::env::current_exe().map_err(|error| error.to_string())?;
        let stub = bin.join(format!("copilot{}", std::env::consts::EXE_SUFFIX));
        std::fs::copy(&me, &stub)
            .map_err(|error| format!("cannot install the copilot stub: {error}"))?;
        if self.setup.fake_gh {
            let gh = bin.join(format!("gh{}", std::env::consts::EXE_SUFFIX));
            std::fs::copy(&me, &gh)
                .map_err(|error| format!("cannot install the gh stub: {error}"))?;
        }

        // Watched by the first session, and subscribed as CST starts, so nothing
        // posted to the thread before then counts as news.
        if !self.setup.watched.is_empty() || !self.setup.held.is_empty() {
            let session = session_id(0);
            let subscriptions = self
                .setup
                .watched
                .iter()
                .map(|url| thread_json(url, &session))
                .collect::<Result<Vec<_>, _>>()?;
            let notices = self
                .setup
                .held
                .iter()
                .map(|url| held_reply_json(url, &session))
                .collect::<Result<Vec<_>, _>>()?;
            let dir = self
                .run_dir
                .join("cst-home")
                .join("data")
                .join("copilot-session-tui");
            std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
            std::fs::write(
                dir.join("threads.json"),
                format!(
                    r#"{{"subscriptions":[{}],"notices":[{}]}}"#,
                    subscriptions.join(","),
                    notices.join(",")
                ),
            )
            .map_err(|error| error.to_string())?;
        }

        // The stub Bot API, and CST pointed at it. Merged into the script's own settings
        // by text, since the crate has no JSON library: the script's object loses its
        // closing brace and gains the Telegram keys.
        if let Some((mode, paired)) = self.setup.telegram {
            let stub = telegram_stub::TelegramStub::start(mode, self.run_dir.clone())?;
            let pairing = if paired {
                format!(
                    r#""chat_id": {}, "user_id": {}, "chat_mode": "{}", "#,
                    mode.chat_id(),
                    telegram_stub::USER_ID,
                    mode.label()
                )
            } else {
                String::new()
            };
            let telegram = format!(
                r#""telegram": {{"enabled": true, {pairing}"api_base": "{}"}}, "telegram_bot_token": "{}""#,
                stub.api_base(),
                telegram_stub::TOKEN
            );
            let merged = match self.setup.config.as_deref().map(str::trim) {
                Some(json) if json != "{}" => {
                    let body = json
                        .strip_suffix('}')
                        .ok_or("the `config` JSON must be one object")?;
                    format!("{body}, {telegram}}}")
                }
                _ => format!("{{{telegram}}}"),
            };
            self.setup.config = Some(merged);
            self.telegram = Some(stub);
        }

        // Where CST looks for its settings under CST_HOME; without a `config` line
        // there is no file and CST runs on its defaults.
        if let Some(json) = &self.setup.config {
            let dir = self
                .run_dir
                .join("cst-home")
                .join("config")
                .join("copilot-session-tui");
            std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
            std::fs::write(dir.join("config.json"), json).map_err(|error| error.to_string())?;
        }

        self.config_before = Some(std::fs::read(config_path()).ok());
        if let Some(Some(bytes)) = &self.config_before {
            let _ = std::fs::write(self.run_dir.join("config.before.json"), bytes);
        }

        let pty = native_pty_system()
            .openpty(PtySize {
                rows,
                cols: columns,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| format!("cannot open a pseudo-terminal: {error}"))?;
        let mut command = CommandBuilder::new(&self.exe);
        for arg in &self.setup.args {
            command.arg(arg);
        }
        command.cwd(&self.run_dir);
        command.env("COPILOT_HOME", &home);
        // CST keeps its own config and state here rather than in the user's.
        command.env("CST_HOME", self.run_dir.join("cst-home"));
        let path = std::env::var_os("PATH").unwrap_or_default();
        let path =
            std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(&path)))
                .map_err(|error| error.to_string())?;
        command.env("PATH", path);
        for (key, value) in &self.setup.env {
            command.env(key, value);
        }
        let child = pty
            .slave
            .spawn_command(command)
            .map_err(|error| format!("cannot start {}: {error}", self.exe.display()))?;
        println!("started CST, pid {:?}", child.process_id());
        drop(pty.slave);

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, columns, 0)));
        let raw = Arc::new(Mutex::new(Vec::new()));
        let writer: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(
            pty.master
                .take_writer()
                .map_err(|error| error.to_string())?,
        ));
        let mut reader = pty
            .master
            .try_clone_reader()
            .map_err(|error| error.to_string())?;
        {
            let parser = parser.clone();
            let raw = raw.clone();
            let answer = writer.clone();
            std::thread::spawn(move || {
                let mut buffer = [0u8; 8192];
                while let Ok(read) = reader.read(&mut buffer) {
                    if read == 0 {
                        break;
                    }
                    let chunk = &buffer[..read];
                    parser.lock().unwrap().process(chunk);
                    raw.lock().unwrap().extend_from_slice(chunk);
                    // ConPTY asks where the cursor is before it shows anything,
                    // and stalls until a terminal answers.
                    if chunk.windows(4).any(|window| window == b"\x1b[6n") {
                        let mut answer = answer.lock().unwrap();
                        let _ = answer.write_all(b"\x1b[1;1R");
                        let _ = answer.flush();
                    }
                }
            });
        }

        self.cst = Some(Running {
            parser,
            raw,
            writer,
            child,
            _master: pty.master,
            size: (columns, rows),
        });
        Ok(())
    }

    fn running(&self) -> &Running {
        self.cst.as_ref().expect("CST is running")
    }

    fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        let running = self.cst.as_mut().expect("CST is running");
        if let Ok(Some(status)) = running.child.try_wait() {
            return Err(format!("CST has exited ({status:?})"));
        }
        {
            let mut writer = running.writer.lock().unwrap();
            writer
                .write_all(bytes)
                .and_then(|()| writer.flush())
                .map_err(|error| format!("cannot write to CST: {error}"))?;
        }
        // However it was sent, `key \e` as much as `press esc`.
        if bytes.last() == Some(&0x1b) {
            std::thread::sleep(self.pace.max(ESC_SETTLE));
        } else {
            std::thread::sleep(self.pace);
        }
        Ok(())
    }

    fn find(&self, text: &str) -> Option<(u16, u16)> {
        let parser = self.running().parser.lock().unwrap();
        screen::find(parser.screen(), text)
    }

    fn locate(&self, target: &Target) -> Result<(u16, u16), String> {
        match target {
            Target::Cell(column, row) => Ok((*column, *row)),
            Target::Text(text) => self
                .find(text)
                .ok_or_else(|| format!("{text:?} is not on screen to point at")),
        }
    }

    fn wait_until(&self, text: &str, present: bool) -> Result<(), String> {
        let started = Instant::now();
        loop {
            if self.find(text).is_some() == present {
                return Ok(());
            }
            if started.elapsed() > self.timeout {
                let what = if present { "to appear" } else { "to go away" };
                return Err(format!(
                    "waited {}ms for {text:?} {what}",
                    self.timeout.as_millis()
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn dump(&mut self, name: &str) -> Result<(), String> {
        let (text, html, size) = {
            let running = self.running();
            let parser = running.parser.lock().unwrap();
            let screen = parser.screen();
            (screen::text(screen), screen::html(screen), running.size)
        };
        let base = self.run_dir.join(name);
        let text_path = base.with_extension("txt");
        let html_path = base.with_extension("html");
        std::fs::write(&text_path, text).map_err(|error| error.to_string())?;
        std::fs::write(&html_path, html).map_err(|error| error.to_string())?;
        let mut shown = text_path;
        if let Some(browser) = &self.browser {
            let png = base.with_extension("png");
            match screen::png(
                browser,
                &self.run_dir.join("browser"),
                &html_path,
                &png,
                size,
            ) {
                Ok(()) => shown = png,
                Err(error) => println!("note: no PNG for {name}: {error}"),
            }
        }
        println!("dumped {name}");
        self.dumps.push(shown);
        Ok(())
    }

    /// Stops CST and undoes what it left outside the run directory. Returns
    /// whether that all went cleanly.
    fn finish(&mut self) -> bool {
        let Some(mut running) = self.cst.take() else {
            return true;
        };
        let pid = running.child.process_id();
        let _ = running.child.kill();
        let _ = running.child.wait();
        let _ = std::fs::write(self.run_dir.join("raw.bin"), &*running.raw.lock().unwrap());
        let mut clean = true;

        // With CST_HOME set, CST writes nothing to the user's own folders. These
        // checks catch a binary that does anyway, such as one built before
        // CST_HOME existed and passed with --exe. A recovery record left in the
        // real folder would make the user's next CST offer to recover fake sessions.
        if let Some(pid) = pid {
            let records = recovery_dir();
            let prefix = format!("{pid}-");
            let ours = |records: &Path| -> Vec<PathBuf> {
                std::fs::read_dir(records)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| {
                        path.file_name()
                            .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
                    })
                    .collect()
            };
            for record in ours(&records) {
                clean = false;
                println!(
                    "this CST ignored CST_HOME and wrote a recovery record to your real data folder: {}",
                    record.display()
                );
                if let Err(error) = std::fs::remove_file(&record) {
                    println!("could not delete {}: {error}", record.display());
                }
            }
            let left = ours(&records);
            if !left.is_empty() {
                clean = false;
                println!("recovery records of this run are still there: {left:?}");
            }
        }

        if let Some(before) = &self.config_before {
            let after = std::fs::read(config_path()).ok();
            if &after != before {
                clean = false;
                println!(
                    "WARNING: your real config changed during this run: {}\n  \
                     Either another CST saved it meanwhile, or this CST ignored CST_HOME.",
                    config_path().display()
                );
                match before {
                    Some(_) => println!(
                        "  the contents from before the run are in {}; compare before restoring,\n  since another CST may have saved its own change meanwhile",
                        self.run_dir.join("config.before.json").display()
                    ),
                    None => println!("  there was no config file before the run"),
                }
            }
        }
        clean
    }
}

/// Where CST keeps its config. Mirrors `config_path()` in CST.
fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".config")
        })
        .join("copilot-session-tui")
        .join("config.json")
}

/// Where CST writes its recovery records. Mirrors `recovery::root()` in CST.
fn recovery_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local")
                .join("share")
        })
        .join("copilot-session-tui")
        .join("workspaces")
}

/// Single-quoted, where a backslash in a Windows path is just a backslash.
/// `00000001-0000-4000-8000-000000000001` for the first session declared, and so on.
/// One subscription in CST's `threads.json`, for a thread URL such as
/// `https://github.com/o/r/discussions/7`. The host can be anything, including one
/// that answers nothing, which is how a script makes polling an issue fail.
fn thread_json(url: &str, session: &str) -> Result<String, String> {
    Ok(format!(
        r#"{{"session_id":"{}","thread":{},"state":"active"}}"#,
        json_quote(session),
        thread_ref_json(url)?,
    ))
}

/// A reply CST is holding for the first session, as CST writes it in `threads.json`.
///
/// `spec` is the script's `<url> [throttled]`: held because the session was closed
/// when it arrived, or because the thread had woken it too often.
fn held_reply_json(spec: &str, session: &str) -> Result<String, String> {
    let mut words = spec.split_whitespace();
    let url = words.next().unwrap_or_default();
    let reason = match words.next() {
        None => "session_closed",
        Some("throttled") => "throttled",
        Some(other) => {
            return Err(format!(
                "held_reply takes `throttled` or nothing after the URL, not {other:?}"
            ))
        }
    };
    Ok(format!(
        r#"{{"session_id":"{}","thread":{},"comment_url":"{}#issuecomment-1","state":"waiting","reason":"{reason}"}}"#,
        json_quote(session),
        thread_ref_json(url)?,
        json_quote(url),
    ))
}

fn json_quote(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

fn thread_ref_json(url: &str) -> Result<String, String> {
    let bad =
        || format!("a thread is https://HOST/OWNER/REPO/(issues|pull|discussions)/N, got {url:?}");
    let path = url.strip_prefix("https://").ok_or_else(bad)?;
    let parts: Vec<&str> = path.split('/').collect();
    let [host, owner, repo, kind, number] = parts[..] else {
        return Err(bad());
    };
    let kind = match kind {
        "issues" => "issue",
        "pull" => "pull_request",
        "discussions" => "discussion",
        _ => return Err(bad()),
    };
    let number: u64 = number.parse().map_err(|_| bad())?;
    Ok(format!(
        r#"{{"host":"{}","owner":"{}","repo":"{}","number":{number},"kind":"{kind}"}}"#,
        json_quote(host),
        json_quote(owner),
        json_quote(repo),
    ))
}

fn session_id(index: usize) -> String {
    let number = index + 1;
    format!("{number:08}-0000-4000-8000-{number:012}")
}

fn yaml_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// An absolute path without the `\\?\` prefix Windows adds, which browsers and
/// several other programs given the path do not understand.
fn canonical(path: PathBuf) -> PathBuf {
    let Ok(resolved) = path.canonicalize() else {
        return path;
    };
    match resolved
        .to_str()
        .and_then(|text| text.strip_prefix(r"\\?\"))
    {
        Some(plain) if !plain.starts_with("UNC\\") => PathBuf::from(plain),
        _ => resolved,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_setup_line_after_driving_has_started_is_refused_before_anything_runs() {
        let error = parse_script("session a A\npress enter\nsize 80x24\n").unwrap_err();

        assert!(error.starts_with("line 3:"), "got {error}");
    }

    #[test]
    fn pace_and_timeout_may_come_before_setup_without_closing_it() {
        assert!(parse_script(
            "pace 100
timeout 500
size 80x24
press enter
"
        )
        .is_ok());
    }

    #[test]
    fn a_typo_anywhere_in_a_script_is_reported_with_its_line() {
        let error = parse_script("press enter\nwait 100\npresss esc\n").unwrap_err();

        assert!(
            error.contains("line 3") && error.contains("presss"),
            "got {error}"
        );
    }

    #[test]
    fn escapes_spell_the_bytes_a_terminal_sends() {
        assert_eq!(unescape(r"\e[A\r\x02\s\\").unwrap(), b"\x1b[A\r\x02 \\");
        assert!(unescape(r"\q").is_err());
    }

    #[test]
    fn named_keys_cover_control_letters_and_reject_what_they_do_not_know() {
        assert_eq!(key_bytes("ctrl+b").unwrap(), vec![0x02]);
        assert_eq!(key_bytes("esc").unwrap(), vec![0x1b]);
        assert_eq!(key_bytes("?").unwrap(), b"?".to_vec());
        assert_eq!(key_bytes("F5").unwrap(), b"\x1b[15~".to_vec());
        assert!(key_bytes("hyper+x").is_err());
    }

    /// Unlike `session`, these change the fake history under a running CST, so they
    /// are steps rather than setup and may come after it has started.
    #[test]
    fn sessions_can_be_added_and_deleted_after_cst_has_started() {
        let steps = parse_script(
            "session app First
press enter
add_session app Started elsewhere
delete_session First
",
        )
        .unwrap();

        assert!(matches!(&steps[2].1, Step::AddSession { project, summary }
            if project == "app" && summary == "Started elsewhere"));
        assert!(matches!(&steps[3].1, Step::DeleteSession(summary) if summary == "First"));
        assert_eq!(session_id(1), "00000002-0000-4000-8000-000000000002");
    }

    #[test]
    fn a_watched_thread_is_written_the_way_cst_reads_its_subscriptions() {
        let json = thread_json(
            "https://127.0.0.1:9/o/r/issues/12",
            "00000001-0000-4000-8000-000000000001",
        )
        .unwrap();

        assert_eq!(
            json,
            r#"{"session_id":"00000001-0000-4000-8000-000000000001","thread":{"host":"127.0.0.1:9","owner":"o","repo":"r","number":12,"kind":"issue"},"state":"active"}"#
        );
    }

    #[test]
    fn a_held_reply_is_written_the_way_cst_reads_a_notice_for_a_closed_session() {
        let json = held_reply_json(
            "https://github.com/o/r/issues/12",
            "00000001-0000-4000-8000-000000000001",
        )
        .unwrap();

        assert_eq!(
            json,
            r#"{"session_id":"00000001-0000-4000-8000-000000000001","thread":{"host":"github.com","owner":"o","repo":"r","number":12,"kind":"issue"},"comment_url":"https://github.com/o/r/issues/12#issuecomment-1","state":"waiting","reason":"session_closed"}"#
        );
    }

    #[test]
    fn a_held_reply_can_be_one_held_for_waking_its_session_too_often() {
        let json = held_reply_json("https://github.com/o/r/issues/13 throttled", "s").unwrap();

        assert!(json.ends_with(r#""reason":"throttled"}"#), "got {json}");
        assert!(parse_script("held_reply https://github.com/o/r/issues/13 soon\n").is_err());
    }

    #[test]
    fn a_thread_url_cst_could_not_watch_is_refused_before_anything_runs() {
        let error = parse_script("watch_thread https://github.com/o/r/wiki/7\n").unwrap_err();

        assert!(error.starts_with("line 1:"), "got {error}");
    }

    #[test]
    fn taking_github_down_is_driving_so_it_may_come_after_cst_has_started() {
        let steps =
            parse_script("fake_gh\npress enter\ngithub down\ngithub logged-out\ngithub up\n")
                .unwrap();

        assert!(steps[0].1.is_setup());
        assert!(matches!(
            steps[2].1,
            Step::Github(Some(gh_stub::DOWN_MARKER))
        ));
        assert!(matches!(
            steps[3].1,
            Step::Github(Some(gh_stub::LOGGED_OUT_MARKER))
        ));
        assert!(matches!(steps[4].1, Step::Github(None)));
    }
}
