//! Drives the real CST binary in a pseudo-terminal from a script, and captures
//! what it shows as text, HTML and PNG. See `tools/e2e/README.md`.

mod screen;
mod stub;

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
    Session { project: String, summary: String },
    Project { name: String, git: bool },
    Env(String, String),
    Arg(String),
    Wait(Duration),
    WaitFor(String),
    WaitGone(String),
    Expect(String),
    ExpectNot(String),
    Key(Vec<u8>),
    Type(String),
    Press(Vec<String>),
    Paste(String),
    Click { button: u8, at: Target },
    Scroll { down: bool, at: Target },
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
        "session" => {
            let text = need("a project name and a summary")?;
            let (project, summary) = text
                .split_once(' ')
                .ok_or("session wants a project name, then the summary")?;
            Step::Session {
                project: project.to_string(),
                summary: summary.trim().to_string(),
            }
        }
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
            _ => unreachable!("setup steps are handled before launching"),
        }
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
        for (index, (project, summary)) in self.setup.sessions.iter().enumerate() {
            let number = index + 1;
            let id = format!("{number:08}-0000-4000-8000-{number:012}");
            let cwd = projects.join(project);
            std::fs::create_dir_all(&cwd).map_err(|error| error.to_string())?;
            let dir = home.join("session-state").join(&id);
            std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
            // The first session declared is the most recently used.
            let day = 28usize.saturating_sub(number).max(1);
            let yaml = format!(
                "id: {id}\ncwd: {}\nsummary: {}\ncreated_at: 2026-01-01T10:00:00Z\nupdated_at: 2026-01-{day:02}T10:00:00Z\n",
                yaml_quote(&cwd.display().to_string()),
                yaml_quote(summary),
            );
            std::fs::write(dir.join("workspace.yaml"), yaml).map_err(|error| error.to_string())?;
        }

        let me = std::env::current_exe().map_err(|error| error.to_string())?;
        let stub = bin.join(format!("copilot{}", std::env::consts::EXE_SUFFIX));
        std::fs::copy(&me, &stub)
            .map_err(|error| format!("cannot install the copilot stub: {error}"))?;

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

        // Every CST writes a recovery record named after its pid; left behind,
        // the user's next CST offers to recover these fake sessions.
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

        // CST reads its config from the user's real config directory whatever the
        // environment says, so a run that saves anything changes the user's own.
        if let Some(before) = &self.config_before {
            let after = std::fs::read(config_path()).ok();
            if &after != before {
                clean = false;
                println!(
                    "WARNING: this run changed your real config: {}",
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
        assert!(key_bytes("hyper+x").is_err());
    }
}
