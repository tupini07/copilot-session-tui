//! Work a session is waiting on while Copilot itself sits idle.
//!
//! A session that started a two-day run in the background reads as idle on its tab,
//! because the turn that started it is over. That is true of the model and misleading
//! about the session. This holds what is running behind a session and which of it the
//! user said to wait for.
//!
//! The user decides, never CST. A background command is as often a dev server left up
//! on purpose as it is a job somebody is waiting on, and nothing in the log tells the
//! two apart. So commands are listed as they start and count for nothing until marked.
//!
//! Watched work is not a working turn. It never touches the pane's own working state,
//! because that state is what holds back thread wake-ups and Telegram replies, and a
//! session with a long job running is still free to be talked to.

use std::time::{Duration, Instant};

use crate::events::lifecycle::LifecycleEvent;

/// The tab marker for a session with watched work and nothing more pressing to show.
pub const MARKER: &str = "◷ ";

/// A command Copilot left running, as its event log reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub shell_id: String,
    pub description: String,
    pub detached: bool,
    pub started: Instant,
    pub watched: bool,
}

/// What tells one process from every other, including a later one given the same pid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub pid: u32,
    /// When it started, in whatever unit the system reports. Only ever compared with
    /// another reading from the same system.
    pub created: u64,
}

/// A process the user marked, watched until it exits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedProcess {
    pub identity: ProcessIdentity,
    pub label: String,
    pub marked: Instant,
}

#[derive(Debug, Default)]
pub struct BackgroundWork {
    commands: Vec<Command>,
    processes: Vec<WatchedProcess>,
}

impl BackgroundWork {
    /// Take in what the event log said. Returns whether anything changed.
    pub fn apply(&mut self, event: &LifecycleEvent) -> bool {
        match event {
            LifecycleEvent::BackgroundStarted {
                shell_id,
                description,
                detached,
            } => {
                // A shell id used again is a new command; a mark made on the old one
                // was about the old one.
                self.commands
                    .retain(|command| &command.shell_id != shell_id);
                self.commands.push(Command {
                    shell_id: shell_id.clone(),
                    description: description.clone(),
                    detached: *detached,
                    started: Instant::now(),
                    watched: false,
                });
                true
            }
            LifecycleEvent::BackgroundEnded { shell_id, .. } => {
                let before = self.commands.len();
                self.commands
                    .retain(|command| &command.shell_id != shell_id);
                before != self.commands.len()
            }
            // The shells died with the CLI. Marked processes are another matter: they
            // are watched by what the system says, and can outlive it.
            LifecycleEvent::BackgroundCleared | LifecycleEvent::Reset => {
                let had = !self.commands.is_empty();
                self.commands.clear();
                had
            }
            _ => false,
        }
    }

    pub fn commands(&self) -> &[Command] {
        &self.commands
    }

    pub fn processes(&self) -> &[WatchedProcess] {
        &self.processes
    }

    /// How many things the user is waiting on here.
    pub fn watched(&self) -> usize {
        self.commands
            .iter()
            .filter(|command| command.watched)
            .count()
            + self.processes.len()
    }

    /// Flip whether a command counts as work. `None` if it has gone.
    pub fn toggle_command(&mut self, shell_id: &str) -> Option<bool> {
        let command = self
            .commands
            .iter_mut()
            .find(|command| command.shell_id == shell_id)?;
        command.watched = !command.watched;
        Some(command.watched)
    }

    pub fn is_watching_process(&self, identity: ProcessIdentity) -> bool {
        self.processes
            .iter()
            .any(|process| process.identity == identity)
    }

    /// Flip whether a process is watched. Returns whether it now is.
    pub fn toggle_process(&mut self, identity: ProcessIdentity, label: &str) -> bool {
        if self.is_watching_process(identity) {
            self.processes
                .retain(|process| process.identity != identity);
            false
        } else {
            self.processes.push(WatchedProcess {
                identity,
                label: label.to_string(),
                marked: Instant::now(),
            });
            true
        }
    }

    /// Drop the watched processes that have exited, and hand them back.
    pub fn reap(&mut self, still_running: impl Fn(ProcessIdentity) -> bool) -> Vec<WatchedProcess> {
        let (running, finished) = std::mem::take(&mut self.processes)
            .into_iter()
            .partition(|process| still_running(process.identity));
        self.processes = running;
        finished
    }

    /// One line for the status bar: the newest watched thing, and how many more.
    ///
    /// The newest because it is the one just set going, and so the one recognised at a
    /// glance. The rest are a key away.
    pub fn summary(&self, now: Instant) -> Option<String> {
        let newest = self
            .commands
            .iter()
            .filter(|command| command.watched)
            .map(|command| (command.description.as_str(), command.started))
            .chain(
                self.processes
                    .iter()
                    .map(|process| (process.label.as_str(), process.marked)),
            )
            .max_by_key(|(_, since)| *since)?;
        let more = match self.watched() - 1 {
            0 => String::new(),
            more => format!(" +{more} more"),
        };
        Some(format!(
            "{} ({}){more}",
            newest.0,
            age(now.saturating_duration_since(newest.1))
        ))
    }
}

/// A duration as the popup and the status bar show it: one unit, no seconds past a minute.
pub fn age(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m", seconds / 60),
        3600..=86_399 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86_400),
    }
}

/// A running process, as listed for marking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    pub identity: ProcessIdentity,
    pub parent: u32,
    pub name: String,
    /// The whole command line where the system gives it; the name otherwise.
    pub command: String,
    pub age: Option<Duration>,
}

impl ProcessInfo {
    /// What a row and a notification call this process.
    pub fn label(&self) -> &str {
        if self.command.trim().is_empty() {
            &self.name
        } else {
            &self.command
        }
    }
}

/// The processes worth offering under a session's Copilot process.
///
/// Everything descended from `root`, less the plumbing: console hosts, and whatever
/// Copilot started that is not a shell (its MCP servers and helpers), with all that
/// those started. A command the model ran is always under one of its shells.
///
/// A child must not be older than its parent. Pids are reused, and without this a
/// process whose long-dead parent's pid now belongs to one of ours would be adopted.
pub fn descendants(all: &[ProcessInfo], root: u32) -> Vec<ProcessInfo> {
    let Some(root_process) = all.iter().find(|process| process.identity.pid == root) else {
        return Vec::new();
    };
    let mut found: Vec<ProcessInfo> = Vec::new();
    let mut frontier = vec![(root_process.identity, true)];
    while let Some((parent, parent_is_root)) = frontier.pop() {
        for process in all {
            if process.parent != parent.pid
                || process.identity.pid == parent.pid
                || process.identity.created < parent.created
                || found.iter().any(|seen| seen.identity == process.identity)
            {
                continue;
            }
            if is_console_host(&process.name) || (parent_is_root && !is_shell(&process.name)) {
                continue;
            }
            found.push(process.clone());
            frontier.push((process.identity, false));
        }
    }
    // Oldest first: a list that reorders itself between two looks is hard to mark in.
    found.sort_by_key(|process| (process.identity.created, process.identity.pid));
    found
}

fn bare_name(name: &str) -> String {
    let name = name.rsplit(['/', '\\']).next().unwrap_or(name);
    name.strip_suffix(".exe")
        .or_else(|| name.strip_suffix(".EXE"))
        .unwrap_or(name)
        .to_ascii_lowercase()
}

fn is_console_host(name: &str) -> bool {
    bare_name(name) == "conhost"
}

fn is_shell(name: &str) -> bool {
    matches!(
        bare_name(name).as_str(),
        "pwsh" | "powershell" | "cmd" | "bash" | "sh" | "zsh" | "fish" | "dash"
    )
}

/// Every process on the machine. Slow enough on Windows to belong off the UI thread.
pub fn list_processes() -> Result<Vec<ProcessInfo>, String> {
    system::list()
}

/// Whether the process that was `identity` is still running, and still that process.
pub fn process_is_running(identity: ProcessIdentity) -> bool {
    match system::created(identity.pid) {
        // The two readings come from different interfaces on Windows and can differ in
        // their last digits, so near enough is the same process. A reused pid starts
        // seconds to days later, never within this.
        Some(created) => created.abs_diff(identity.created) <= system::CREATED_TOLERANCE,
        None => false,
    }
}

#[cfg(windows)]
mod system {
    use super::{ProcessIdentity, ProcessInfo};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    /// A second, in the 100 ns ticks process creation times are counted in.
    pub const CREATED_TOLERANCE: u64 = 10_000_000;
    /// 1601 to 1970, in the same ticks.
    const UNIX_EPOCH_TICKS: u64 = 116_444_736_000_000_000;

    /// One line per process, tab separated, from the only interface that gives command
    /// lines without reading another process's memory. Whitespace in a command line is
    /// flattened so a tab or a newline in one cannot pass for a field or a row.
    const QUERY: &str = "Get-CimInstance Win32_Process | ForEach-Object { \
        $created = if ($_.CreationDate) { $_.CreationDate.ToFileTimeUtc() } else { 0 }; \
        [string]::Join(\"`t\", @($_.ProcessId, $_.ParentProcessId, $created, $_.Name, \
        ([string]$_.CommandLine -replace '\\s+', ' '))) }";

    pub fn list() -> Result<Vec<ProcessInfo>, String> {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let output = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", QUERY])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|error| format!("Could not list processes: {error}"))?;
        if !output.status.success() {
            return Err("Could not list processes: PowerShell reported an error".to_string());
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|since| since.as_nanos() as u64 / 100 + UNIX_EPOCH_TICKS)
            .unwrap_or(0);
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| parse_row(line, now))
            .collect())
    }

    pub(super) fn parse_row(line: &str, now: u64) -> Option<ProcessInfo> {
        let mut fields = line.trim_end_matches('\r').splitn(5, '\t');
        let pid = fields.next()?.trim().parse().ok()?;
        let parent = fields.next()?.trim().parse().ok()?;
        let created: u64 = fields.next()?.trim().parse().ok()?;
        let name = fields.next()?.trim().to_string();
        let command = fields.next().unwrap_or_default().trim().to_string();
        Some(ProcessInfo {
            identity: ProcessIdentity { pid, created },
            parent,
            name,
            command,
            age: (created > 0 && now >= created)
                .then(|| Duration::from_nanos((now - created).saturating_mul(100))),
        })
    }

    /// When `pid` started, or `None` if no such process is running.
    pub fn created(pid: u32) -> Option<u64> {
        use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut creation, mut exit, mut kernel, mut user) = (zero, zero, zero, zero);
        // SAFETY: OpenProcess takes no pointers. The two queries write only the
        // initialized values passed to them. The handle is checked and closed here.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return None;
            }
            let mut exit_code = 0;
            let alive =
                GetExitCodeProcess(handle, &mut exit_code) != 0 && exit_code == STILL_ACTIVE as u32;
            let timed =
                GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) != 0;
            CloseHandle(handle);
            (alive && timed).then(|| {
                (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime)
            })
        }
    }
}

#[cfg(target_os = "linux")]
mod system {
    use super::{ProcessIdentity, ProcessInfo};
    use std::time::Duration;

    /// Start times are read from the same file both times, so they match exactly.
    pub const CREATED_TOLERANCE: u64 = 0;

    pub fn list() -> Result<Vec<ProcessInfo>, String> {
        let entries = std::fs::read_dir("/proc")
            .map_err(|error| format!("Could not list processes: {error}"))?;
        // SAFETY: sysconf reads a constant and takes no pointers.
        let ticks_per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64;
        let uptime = std::fs::read_to_string("/proc/uptime")
            .ok()
            .and_then(|text| text.split_whitespace().next()?.parse::<f64>().ok());
        Ok(entries
            .flatten()
            .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
            .filter_map(|pid| {
                let (name, parent, created) = stat(pid)?;
                let command = std::fs::read(format!("/proc/{pid}/cmdline"))
                    .map(|bytes| {
                        String::from_utf8_lossy(&bytes)
                            .split('\0')
                            .filter(|part| !part.is_empty())
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();
                let started = created as f64 / ticks_per_second as f64;
                Some(ProcessInfo {
                    identity: ProcessIdentity { pid, created },
                    parent,
                    name,
                    command,
                    age: uptime
                        .filter(|uptime| *uptime >= started)
                        .map(|uptime| Duration::from_secs_f64(uptime - started)),
                })
            })
            .collect())
    }

    /// Name, parent and start time from `/proc/<pid>/stat`. The name is in brackets
    /// and may itself hold spaces and brackets, so the fields are counted from the
    /// last closing one.
    fn stat(pid: u32) -> Option<(String, u32, u64)> {
        let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let open = text.find('(')?;
        let close = text.rfind(')')?;
        let name = text.get(open + 1..close)?.to_string();
        let mut rest = text.get(close + 1..)?.split_whitespace();
        // After the name: state, then the parent; the start time is the 20th.
        let parent = rest.nth(1)?.parse().ok()?;
        let created = rest.nth(17)?.parse().ok()?;
        Some((name, parent, created))
    }

    pub fn created(pid: u32) -> Option<u64> {
        stat(pid).map(|(_, _, created)| created)
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod system {
    use super::ProcessInfo;

    pub const CREATED_TOLERANCE: u64 = 0;

    pub fn list() -> Result<Vec<ProcessInfo>, String> {
        Err("Listing a session's processes is not supported on this system".to_string())
    }

    pub fn created(_pid: u32) -> Option<u64> {
        None
    }
}

/// What a row of the popup is about, so the selection stays on it while rows come
/// and go underneath: commands end, and the process list arrives late.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKey {
    Command(String),
    Process(ProcessIdentity),
}

/// One row of the popup, ready to draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub key: RowKey,
    pub label: String,
    pub age: Option<Duration>,
    pub watched: bool,
    /// "command", "detached", or "process".
    pub kind: &'static str,
}

#[derive(Debug)]
pub enum ProcessList {
    Loading,
    Loaded(Vec<ProcessInfo>),
    /// Why there is no list, as a sentence.
    Unavailable(String),
}

/// The list of what is running behind one session, open over the workspace.
#[derive(Debug)]
pub struct Popup {
    pub pane: crate::mux::PaneId,
    pub selected: Option<RowKey>,
    pub processes: ProcessList,
    receiver: Option<std::sync::mpsc::Receiver<Result<Vec<ProcessInfo>, String>>>,
}

impl Popup {
    /// Open on `pane`, listing what runs under `root` on another thread: asking
    /// Windows for every command line takes most of a second.
    pub fn open(pane: crate::mux::PaneId, root: Result<u32, String>) -> Self {
        let (processes, receiver) = match root {
            Ok(root) => {
                let (sender, receiver) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let _ = sender.send(list_processes().map(|all| descendants(&all, root)));
                });
                (ProcessList::Loading, Some(receiver))
            }
            Err(reason) => (ProcessList::Unavailable(reason), None),
        };
        Self {
            pane,
            selected: None,
            processes,
            receiver,
        }
    }

    pub fn is_loading(&self) -> bool {
        self.receiver.is_some()
    }

    /// Take the process list if it has arrived. Returns whether it did.
    pub fn poll(&mut self) -> bool {
        let Some(receiver) = self.receiver.as_ref() else {
            return false;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err("Could not list processes".to_string())
            }
        };
        self.receiver = None;
        self.processes = match result {
            Ok(processes) => ProcessList::Loaded(processes),
            Err(reason) => ProcessList::Unavailable(reason),
        };
        true
    }

    /// Commands first, as the log reported them; then the processes being watched;
    /// then the rest of what is running.
    pub fn rows(&self, work: &BackgroundWork, now: Instant) -> Vec<Row> {
        let mut rows: Vec<Row> = work
            .commands()
            .iter()
            .map(|command| Row {
                key: RowKey::Command(command.shell_id.clone()),
                label: command.description.clone(),
                age: Some(now.saturating_duration_since(command.started)),
                watched: command.watched,
                kind: if command.detached {
                    "detached"
                } else {
                    "command"
                },
            })
            .collect();
        let listed: &[ProcessInfo] = match &self.processes {
            ProcessList::Loaded(processes) => processes,
            _ => &[],
        };
        rows.extend(work.processes().iter().map(|process| {
            Row {
                key: RowKey::Process(process.identity),
                label: process.label.clone(),
                age: listed
                    .iter()
                    .find(|info| info.identity == process.identity)
                    .and_then(|info| info.age),
                watched: true,
                kind: "process",
            }
        }));
        rows.extend(
            listed
                .iter()
                .filter(|info| !work.is_watching_process(info.identity))
                .map(|info| Row {
                    key: RowKey::Process(info.identity),
                    label: info.label().to_string(),
                    age: info.age,
                    watched: false,
                    kind: "process",
                }),
        );
        rows
    }

    /// Where the selection is among `rows`: on its row, or on the first if that row
    /// has gone.
    pub fn selected_index(&self, rows: &[Row]) -> usize {
        self.selected
            .as_ref()
            .and_then(|key| rows.iter().position(|row| &row.key == key))
            .unwrap_or(0)
    }

    pub fn move_selection(&mut self, rows: &[Row], delta: isize) {
        if rows.is_empty() {
            self.selected = None;
            return;
        }
        let index = self
            .selected_index(rows)
            .saturating_add_signed(delta)
            .min(rows.len() - 1);
        self.selected = Some(rows[index].key.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started(shell: &str, description: &str) -> LifecycleEvent {
        LifecycleEvent::BackgroundStarted {
            shell_id: shell.to_string(),
            description: description.to_string(),
            detached: true,
        }
    }

    fn process(pid: u32, parent: u32, created: u64, name: &str) -> ProcessInfo {
        ProcessInfo {
            identity: ProcessIdentity { pid, created },
            parent,
            name: name.to_string(),
            command: format!("{name} --flag"),
            age: None,
        }
    }

    #[test]
    fn a_background_command_counts_for_nothing_until_the_user_marks_it_because_many_are_just_servers(
    ) {
        let mut work = BackgroundWork::default();
        assert!(work.apply(&started("server", "Run the dev server")));
        assert!(work.apply(&started("train", "Train the model")));

        assert_eq!(work.watched(), 0);
        assert_eq!(work.summary(Instant::now()), None);

        assert_eq!(work.toggle_command("train"), Some(true));
        assert_eq!(work.watched(), 1);
        assert_eq!(work.toggle_command("train"), Some(false));
        assert_eq!(work.watched(), 0);
        assert_eq!(work.toggle_command("gone"), None);
    }

    #[test]
    fn a_watched_command_stops_counting_when_the_log_says_it_ended() {
        let mut work = BackgroundWork::default();
        work.apply(&started("train", "Train the model"));
        work.toggle_command("train");

        assert!(work.apply(&LifecycleEvent::BackgroundEnded {
            shell_id: "train".to_string(),
            exit_code: Some(0),
        }));
        assert_eq!(work.watched(), 0);
        assert!(work.commands().is_empty());
        // Said twice, the second changes nothing.
        assert!(!work.apply(&LifecycleEvent::BackgroundEnded {
            shell_id: "train".to_string(),
            exit_code: Some(0),
        }));
    }

    #[test]
    fn a_shell_id_started_again_is_a_new_command_and_does_not_inherit_the_old_mark() {
        let mut work = BackgroundWork::default();
        work.apply(&started("job", "First run"));
        work.toggle_command("job");

        work.apply(&started("job", "Second run"));

        assert_eq!(work.commands().len(), 1);
        assert_eq!(work.commands()[0].description, "Second run");
        assert_eq!(work.watched(), 0);
    }

    #[test]
    fn the_cli_going_away_drops_its_commands_but_not_a_process_the_user_marked() {
        let mut work = BackgroundWork::default();
        work.apply(&started("job", "A job"));
        work.toggle_command("job");
        let identity = ProcessIdentity { pid: 7, created: 1 };
        assert!(work.toggle_process(identity, "python train.py"));

        assert!(work.apply(&LifecycleEvent::BackgroundCleared));

        assert!(work.commands().is_empty());
        assert_eq!(work.watched(), 1, "the process is still running");
    }

    #[test]
    fn the_summary_names_the_newest_watched_thing_and_counts_the_rest() {
        let mut work = BackgroundWork::default();
        work.apply(&started("first", "Build the index"));
        work.toggle_command("first");
        let now = Instant::now() + Duration::from_secs(7 * 60);
        assert_eq!(work.summary(now).as_deref(), Some("Build the index (7m)"));

        std::thread::sleep(Duration::from_millis(5));
        work.apply(&started("second", "Train the model"));
        work.toggle_command("second");
        work.toggle_process(ProcessIdentity { pid: 7, created: 1 }, "python eval.py");

        let summary = work.summary(Instant::now()).unwrap();
        assert!(summary.starts_with("python eval.py ("), "got {summary}");
        assert!(summary.ends_with(" +2 more"), "got {summary}");
    }

    #[test]
    fn a_marked_process_is_handed_back_once_it_has_exited() {
        let mut work = BackgroundWork::default();
        let running = ProcessIdentity { pid: 7, created: 1 };
        let exited = ProcessIdentity { pid: 8, created: 2 };
        work.toggle_process(running, "server");
        work.toggle_process(exited, "python train.py");

        let finished = work.reap(|identity| identity == running);

        assert_eq!(finished.len(), 1);
        assert_eq!(finished[0].label, "python train.py");
        assert_eq!(work.watched(), 1);
        assert!(work.reap(|_| true).is_empty());
    }

    #[test]
    fn the_popup_keeps_its_selection_on_the_same_thing_when_rows_above_it_go() {
        let mut work = BackgroundWork::default();
        work.apply(&started("first", "Build the index"));
        work.apply(&started("second", "Train the model"));
        let mut popup = Popup::open(1, Err("no process list in this test".to_string()));
        let now = Instant::now();

        let rows = popup.rows(&work, now);
        assert_eq!(popup.selected_index(&rows), 0, "starts on the first row");
        popup.move_selection(&rows, 1);
        assert_eq!(popup.selected, Some(RowKey::Command("second".into())));

        work.apply(&LifecycleEvent::BackgroundEnded {
            shell_id: "first".to_string(),
            exit_code: None,
        });
        let rows = popup.rows(&work, now);
        assert_eq!(rows[popup.selected_index(&rows)].label, "Train the model");

        // The selected thing itself ending leaves the selection on what is left.
        work.apply(&LifecycleEvent::BackgroundEnded {
            shell_id: "second".to_string(),
            exit_code: None,
        });
        let rows = popup.rows(&work, now);
        assert!(rows.is_empty());
        popup.move_selection(&rows, 1);
        assert_eq!(popup.selected, None);
    }

    #[test]
    fn the_popup_lists_commands_then_watched_processes_then_the_rest() {
        let mut work = BackgroundWork::default();
        work.apply(&started("job", "Train the model"));
        let watched = process(8, 1, 2, "python.exe");
        let other = process(9, 1, 3, "node.exe");
        work.toggle_process(watched.identity, watched.label());
        let mut popup = Popup::open(1, Err(String::new()));
        popup.processes = ProcessList::Loaded(vec![other.clone(), watched.clone()]);

        let rows = popup.rows(&work, Instant::now());

        let shown: Vec<(&str, bool, &str)> = rows
            .iter()
            .map(|row| (row.label.as_str(), row.watched, row.kind))
            .collect();
        assert_eq!(
            shown,
            vec![
                ("Train the model", false, "detached"),
                ("python.exe --flag", true, "process"),
                ("node.exe --flag", false, "process"),
            ]
        );
    }

    #[test]
    fn ages_read_in_one_unit() {
        for (seconds, expected) in [(5, "5s"), (59, "59s"), (60, "1m"), (7 * 60 + 4, "7m")] {
            assert_eq!(age(Duration::from_secs(seconds)), expected);
        }
        assert_eq!(age(Duration::from_secs(2 * 3600 + 5)), "2h");
        assert_eq!(age(Duration::from_secs(3 * 86_400)), "3d");
    }

    #[test]
    fn the_processes_offered_are_what_the_models_shells_started_not_copilots_own_plumbing() {
        let all = vec![
            process(100, 1, 10, "copilot.exe"),
            process(101, 100, 11, "conhost.exe"),
            process(110, 100, 12, "computer-use-mcp.exe"),
            process(111, 110, 13, "helper.exe"),
            process(120, 100, 14, "pwsh.exe"),
            process(121, 120, 15, "conhost.exe"),
            process(122, 120, 16, "python.exe"),
            process(123, 122, 17, "python.exe"),
            // Another session's, and unrelated.
            process(200, 1, 10, "copilot.exe"),
            process(201, 200, 20, "pwsh.exe"),
        ];

        let pids: Vec<u32> = descendants(&all, 100)
            .iter()
            .map(|process| process.identity.pid)
            .collect();

        assert_eq!(pids, vec![120, 122, 123]);
        assert!(descendants(&all, 999).is_empty());
    }

    #[test]
    fn a_process_older_than_its_supposed_parent_is_not_adopted_because_pids_are_reused() {
        let all = vec![
            process(100, 1, 50, "copilot.exe"),
            process(120, 100, 60, "pwsh.exe"),
            // Started before 120 existed: its real parent was an earlier pid 120.
            process(300, 120, 5, "python.exe"),
            process(301, 120, 70, "python.exe"),
        ];

        let pids: Vec<u32> = descendants(&all, 100)
            .iter()
            .map(|process| process.identity.pid)
            .collect();

        assert_eq!(pids, vec![120, 301]);
    }

    #[cfg(windows)]
    #[test]
    fn a_row_from_the_process_query_keeps_a_command_line_with_tabs_flattened_out_of_it() {
        let now = 116_444_736_000_000_000 + 600 * 10_000_000;
        let created = now - 90 * 10_000_000;
        let row = format!("4242\t17\t{created}\tpython.exe\tpython train.py --gpus 0");

        let parsed = system::parse_row(&row, now).unwrap();

        assert_eq!(parsed.identity, ProcessIdentity { pid: 4242, created });
        assert_eq!(parsed.parent, 17);
        assert_eq!(parsed.label(), "python train.py --gpus 0");
        assert_eq!(parsed.age, Some(Duration::from_secs(90)));

        // A system process gives no command line; its name stands in.
        let bare = system::parse_row("4\t0\t0\tSystem\t", now).unwrap();
        assert_eq!(bare.label(), "System");
        assert_eq!(bare.age, None);
        assert!(system::parse_row("not a row", now).is_none());
    }

    #[test]
    fn this_very_process_is_listed_and_recognised_as_still_running() {
        let Ok(all) = list_processes() else {
            // Nothing to check where the system has no way to list them.
            return;
        };
        let me = all
            .iter()
            .find(|process| process.identity.pid == std::process::id())
            .expect("the test process is in the list");

        assert!(process_is_running(me.identity));
        // The same pid with another start time is some other process.
        assert!(!process_is_running(ProcessIdentity {
            pid: me.identity.pid,
            created: me.identity.created / 2,
        }));
    }
}
