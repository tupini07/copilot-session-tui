use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{File, Metadata};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(150);

/// How long the event log must stay silent before the session counts as idle.
///
/// Measured rather than guessed, and the measurement was not what I first assumed. A
/// working turn usually writes constantly — consecutive records land hundredths of a
/// second apart — but not always: replaying 363 real turns from one session found 261
/// silences of 15 seconds or more with no tool call in flight, the longest over ten
/// minutes. Whatever those are, they are not idleness.
///
/// Sixty seconds cuts that to 40, and the cost of waiting is small: the bug this exists
/// to correct held a spinner for 72 minutes, so a minute is still two orders of magnitude
/// better while being far harder to trip by accident.
const QUIET_AFTER: Duration = Duration::from_secs(60);
const ANCHOR_LEN: u64 = 64;
const MAX_RECORD_PREFIX: usize = 64 * 1024;
const TOOL_START: &[u8] = b"tool.execution_start";
const TOOL_COMPLETE: &[u8] = b"tool.execution_complete";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    Question,
    PlanApproval,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleEvent {
    InputRequested {
        tool_call_id: String,
        kind: InputKind,
    },
    InputResolved {
        tool_call_id: String,
        kind: InputKind,
    },
    /// Copilot has written nothing for a while and has no tool call in flight.
    ///
    /// The evidence that a session is genuinely idle, as opposed to merely looking
    /// idle. A working session writes to this log constantly — consecutive records are
    /// hundredths of a second apart — so silence with nothing outstanding means no turn
    /// is running, whatever the terminal's progress sequences claim.
    ///
    /// Repeated while the quiet lasts rather than sent once, because whoever needs it
    /// may not have needed it yet when the quiet began.
    Quiet,
    /// The set of background shells the agent is still waiting on has changed.
    ///
    /// A shell command that outlives its time limit, or one the agent starts in async
    /// mode, keeps running after the tool call returns; the agent is told it will be
    /// notified when it finishes, says what it has to say, and stops. Its `agentStop`
    /// hook fires, so to the hooks the session is idle, while Copilot's own status row
    /// reads "Waiting for background shells" and the work the user cares about is still
    /// running. The whole current set is sent each time, so a listener never has to
    /// reconcile.
    ///
    /// `completed` is true when a shell left the set because Copilot wrote its
    /// completion notice into the log. That notice is written at the start of the
    /// turn it wakes, milliseconds after `assistant.turn_start`, so it is also the
    /// evidence that a follow-up turn has begun — one no `userPromptSubmitted` hook
    /// announces.
    BackgroundShells {
        shells: Vec<BackgroundShell>,
        completed: bool,
    },
    Reset,
}

/// How a shell came to run in the background, which is how much it says about work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    /// Copilot backgrounded it when it overran its time limit. Nobody starts a server
    /// that way, so this is a job the agent is waiting on.
    Backgrounded,
    /// The agent asked for async mode. Usually a long job, sometimes a server left up
    /// for the user, which the log cannot tell apart.
    Async,
}

/// One background shell the agent has not yet heard back from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundShell {
    /// The agent's own name for it, such as `fits-inference`. Shown to the user, who
    /// can tell a job from a server by its name far better than any rule here.
    pub id: String,
    pub kind: ShellKind,
    /// When this monitor saw it start, not when Copilot did: the monitor begins at
    /// the session's launch, so the two agree for anything it will ever report.
    pub since: Instant,
}

#[cfg(test)]
fn capture_file_len(path: &Path) -> u64 {
    std::fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

/// File state captured immediately before spawning the Copilot child.
///
/// The trailing bytes are represented only by a digest, so lifecycle monitoring
/// never retains conversation or tool payload text.
#[derive(Default)]
pub struct LifecycleBaseline {
    offset: u64,
    identity: Option<FileIdentity>,
    anchor: Option<FileAnchor>,
}

/// Capture enough state to validate the event file on the monitor's first poll.
///
/// A session's event file may not exist yet. In that case the monitor starts at
/// zero and follows the file when the child creates it.
pub fn capture_file_baseline(path: &Path) -> io::Result<LifecycleBaseline> {
    for _ in 0..3 {
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(LifecycleBaseline::default());
            }
            Err(error) => return Err(error),
        };
        let before = file.metadata()?;
        let offset = before.len();
        let identity = file_identity(&before);
        let anchor = file_anchor(&mut file, offset)?;
        let after = file.metadata()?;
        if after.len() == offset && file_identity(&after) == identity {
            return Ok(LifecycleBaseline {
                offset,
                identity: Some(identity),
                anchor,
            });
        }
        thread::yield_now();
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "events file changed repeatedly while capturing lifecycle baseline",
    ))
}

/// Owns the stop signal for one lifecycle watcher.
///
/// Dropping this value wakes and joins the worker, so removing a pane cannot
/// leave a watcher behind.
#[must_use = "dropping the monitor stops its worker"]
pub struct LifecycleMonitor {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl LifecycleMonitor {
    /// Follow complete records appended after a captured pre-spawn baseline.
    ///
    /// Events are delivered in file order. Returning `false` from `callback`
    /// stops the worker. Payload fields such as arguments, result, and content
    /// are neither retained nor included in [`LifecycleEvent`], with one exception:
    /// the ids of background shells still running, which Copilot writes into tool
    /// results and which the user needs to see to know what their session is
    /// waiting on. An id is a name the agent gave a command, never the command or
    /// its output.
    pub fn start_from_baseline<F>(
        path: impl Into<PathBuf>,
        baseline: LifecycleBaseline,
        mut callback: F,
    ) -> Self
    where
        F: FnMut(LifecycleEvent) -> bool + Send + 'static,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let mut tail = LifecycleTail::from_baseline(path.into(), baseline);
        let worker = thread::spawn(move || {
            let mut last_growth = std::time::Instant::now();
            let mut last_quiet = None::<std::time::Instant>;
            while !worker_stop.load(Ordering::Acquire) {
                match tail.poll(&mut callback) {
                    Ok(true) => {}
                    Ok(false) => break,
                    // A missing, concurrently replaced, or temporarily unreadable
                    // file is retried on the next poll.
                    Err(_) => {}
                }

                if tail.advanced {
                    last_growth = std::time::Instant::now();
                    last_quiet = None;
                } else if should_report_quiet(
                    tail.outstanding_tools,
                    last_growth.elapsed(),
                    last_quiet.map(|sent: std::time::Instant| sent.elapsed()),
                ) {
                    last_quiet = Some(std::time::Instant::now());
                    if !callback(LifecycleEvent::Quiet) {
                        break;
                    }
                }

                if worker_stop.load(Ordering::Acquire) {
                    break;
                }
                thread::park_timeout(POLL_INTERVAL);
            }
        });

        Self {
            stop,
            worker: Some(worker),
        }
    }
}

impl Drop for LifecycleMonitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity(u64, u64);

struct FileAnchor {
    start: u64,
    len: usize,
    digest: [u8; 32],
}

#[cfg(unix)]
fn file_identity(metadata: &Metadata) -> FileIdentity {
    use std::os::unix::fs::MetadataExt;
    FileIdentity(metadata.dev(), metadata.ino())
}

#[cfg(windows)]
fn file_identity(metadata: &Metadata) -> FileIdentity {
    use std::os::windows::fs::MetadataExt;
    // Stable Rust does not expose the Windows file index. Creation time catches
    // ordinary replacement, while the anchor below catches in-place truncation
    // and replacement that preserves metadata.
    FileIdentity(metadata.creation_time(), 0)
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_metadata: &Metadata) -> FileIdentity {
    FileIdentity(0, 0)
}

fn file_anchor(file: &mut File, offset: u64) -> io::Result<Option<FileAnchor>> {
    let len = offset.min(ANCHOR_LEN) as usize;
    if len == 0 {
        return Ok(None);
    }
    let start = offset - len as u64;
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = vec![0; len];
    file.read_exact(&mut bytes)?;
    let digest = Sha256::digest(&bytes).into();
    Ok(Some(FileAnchor { start, len, digest }))
}

struct LifecycleTail {
    path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
    discarding_oversized: bool,
    identity: Option<FileIdentity>,
    anchor: Option<FileAnchor>,
    pending: HashMap<String, InputKind>,
    seen_starts: HashSet<String>,
    seen_completions: HashSet<String>,
    /// Bytes arrived on the last poll, which is what "Copilot is doing something" means.
    advanced: bool,
    /// Tool calls started but not yet finished.
    ///
    /// A count rather than a set of ids: extracting every id would mean JSON-parsing
    /// every record in a log that runs to six figures, which is exactly what
    /// `candidate_kind` exists to avoid. Counting needs only the leading `type`, which
    /// is already read for every line.
    ///
    /// Signed, because a log CST started reading partway through can complete a tool it
    /// never saw start. Going negative is harmless — it means the same thing as zero.
    outstanding_tools: i64,
    /// Background shells the agent has been told it will hear back from. Keyed by the
    /// agent's id for the shell, because Copilot reuses ids: a second launch under the
    /// same name owns the next completion.
    shells: HashMap<String, BackgroundShell>,
}

impl LifecycleTail {
    #[cfg(test)]
    fn new(path: PathBuf, offset: u64) -> Self {
        Self::from_baseline(
            path,
            LifecycleBaseline {
                offset,
                ..LifecycleBaseline::default()
            },
        )
    }

    fn from_baseline(path: PathBuf, baseline: LifecycleBaseline) -> Self {
        Self {
            path,
            offset: baseline.offset,
            partial: Vec::new(),
            discarding_oversized: false,
            identity: baseline.identity,
            anchor: baseline.anchor,
            pending: HashMap::new(),
            seen_starts: HashSet::new(),
            seen_completions: HashSet::new(),
            advanced: false,
            outstanding_tools: 0,
            shells: HashMap::new(),
        }
    }

    fn poll<F>(&mut self, callback: &mut F) -> io::Result<bool>
    where
        F: FnMut(LifecycleEvent) -> bool,
    {
        // Cleared here rather than by the caller, so the flag always describes this poll
        // and cannot be left set by whoever looked last.
        self.advanced = false;
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if self.identity.take().is_some() || !self.pending.is_empty() {
                    self.offset = 0;
                    self.anchor = None;
                    self.partial.clear();
                    self.discarding_oversized = false;
                    self.pending.clear();
                    self.seen_starts.clear();
                    self.seen_completions.clear();
                    self.outstanding_tools = 0;
                    self.shells.clear();
                    return Ok(callback(LifecycleEvent::Reset));
                }
                return Ok(true);
            }
            Err(error) => return Err(error),
        };
        let metadata = file.metadata()?;
        let len = metadata.len();
        let identity = file_identity(&metadata);

        let replaced = self.identity.is_some_and(|previous| previous != identity);
        if replaced || len < self.offset || !self.anchor_matches(&mut file)? {
            self.pending.clear();
            self.seen_starts.clear();
            self.seen_completions.clear();
            self.outstanding_tools = 0;
            self.shells.clear();
            self.reset_to_end(&mut file, len, identity)?;
            return Ok(callback(LifecycleEvent::Reset));
        }
        self.identity = Some(identity);

        if len > self.offset {
            self.advanced = true;
            file.seek(SeekFrom::Start(self.offset))?;
            let mut remaining = len - self.offset;
            let mut chunk = [0_u8; 64 * 1024];
            while remaining > 0 {
                let wanted = usize::try_from(remaining.min(chunk.len() as u64)).unwrap();
                let read = file.read(&mut chunk[..wanted])?;
                if read == 0 {
                    break;
                }
                self.offset += read as u64;
                remaining -= read as u64;
                if !self.process_bytes(&chunk[..read], callback) {
                    return Ok(false);
                }
            }
        }

        self.refresh_anchor(&mut file)?;
        Ok(true)
    }

    fn process_bytes<F>(&mut self, mut bytes: &[u8], callback: &mut F) -> bool
    where
        F: FnMut(LifecycleEvent) -> bool,
    {
        while !bytes.is_empty() {
            if self.discarding_oversized {
                let Some(line_end) = bytes.iter().position(|byte| *byte == b'\n') else {
                    return true;
                };
                if !record_oversized_prefix(
                    &self.partial,
                    &mut self.pending,
                    &mut self.seen_starts,
                    &mut self.seen_completions,
                    &mut self.outstanding_tools,
                    &mut self.shells,
                    callback,
                ) {
                    return false;
                }
                self.partial = Vec::new();
                self.discarding_oversized = false;
                bytes = &bytes[line_end + 1..];
                continue;
            }

            if let Some(line_end) = bytes.iter().position(|byte| *byte == b'\n') {
                let segment = &bytes[..line_end];
                let available = MAX_RECORD_PREFIX.saturating_sub(self.partial.len());
                if segment.len() <= available {
                    self.partial.extend_from_slice(segment);
                    let line = self.partial.strip_suffix(b"\r").unwrap_or(&self.partial);
                    if !record_line(
                        line,
                        &mut self.pending,
                        &mut self.seen_starts,
                        &mut self.seen_completions,
                        &mut self.outstanding_tools,
                        &mut self.shells,
                        callback,
                    ) {
                        return false;
                    }
                } else {
                    self.partial.extend_from_slice(&segment[..available]);
                    if !record_oversized_prefix(
                        &self.partial,
                        &mut self.pending,
                        &mut self.seen_starts,
                        &mut self.seen_completions,
                        &mut self.outstanding_tools,
                        &mut self.shells,
                        callback,
                    ) {
                        return false;
                    }
                    self.partial = Vec::new();
                }
                self.partial.clear();
                bytes = &bytes[line_end + 1..];
            } else {
                let available = MAX_RECORD_PREFIX.saturating_sub(self.partial.len());
                let take = available.min(bytes.len());
                self.partial.extend_from_slice(&bytes[..take]);
                self.discarding_oversized = take < bytes.len();
                return true;
            }
        }
        true
    }

    fn anchor_matches(&self, file: &mut File) -> io::Result<bool> {
        let Some(anchor) = self.anchor.as_ref() else {
            return Ok(true);
        };

        file.seek(SeekFrom::Start(anchor.start))?;
        let mut current = vec![0; anchor.len];
        file.read_exact(&mut current)?;
        let digest: [u8; 32] = Sha256::digest(&current).into();
        Ok(digest == anchor.digest)
    }

    fn reset_to_end(
        &mut self,
        file: &mut File,
        len: u64,
        identity: FileIdentity,
    ) -> io::Result<()> {
        self.offset = len;
        self.partial.clear();
        self.discarding_oversized = false;
        self.identity = Some(identity);
        self.refresh_anchor(file)
    }

    fn refresh_anchor(&mut self, file: &mut File) -> io::Result<()> {
        self.anchor = file_anchor(file, self.offset)?;
        Ok(())
    }
}

#[derive(Deserialize)]
struct RawEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    data: RawData,
}

#[derive(Default, Deserialize)]
struct RawData {
    #[serde(rename = "toolName")]
    tool_name: Option<String>,
    #[serde(rename = "toolCallId")]
    tool_call_id: Option<String>,
}

fn record_line<F>(
    line: &[u8],
    pending: &mut HashMap<String, InputKind>,
    seen_starts: &mut HashSet<String>,
    seen_completions: &mut HashSet<String>,
    outstanding_tools: &mut i64,
    shells: &mut HashMap<String, BackgroundShell>,
    callback: &mut F,
) -> bool
where
    F: FnMut(LifecycleEvent) -> bool,
{
    count_tool(line, outstanding_tools);
    if !track_shells(line, shells, callback) {
        return false;
    }
    if candidate_kind(line, pending).is_none() {
        return true;
    }
    let Ok(event) = serde_json::from_slice::<RawEvent>(line) else {
        return true;
    };
    dispatch_lifecycle(
        event.event_type.as_bytes(),
        event.data.tool_name.as_deref(),
        event.data.tool_call_id,
        pending,
        seen_starts,
        seen_completions,
        callback,
    )
}

fn record_oversized_prefix<F>(
    prefix: &[u8],
    pending: &mut HashMap<String, InputKind>,
    seen_starts: &mut HashSet<String>,
    seen_completions: &mut HashSet<String>,
    outstanding_tools: &mut i64,
    shells: &mut HashMap<String, BackgroundShell>,
    callback: &mut F,
) -> bool
where
    F: FnMut(LifecycleEvent) -> bool,
{
    count_tool(prefix, outstanding_tools);
    // A shell's result opens with the sentence that matters, long before any output,
    // so even a prefix of an oversized record carries it.
    if !track_shells(prefix, shells, callback) {
        return false;
    }
    if candidate_kind(prefix, pending).is_none() {
        return true;
    }
    let Some(event_type) = leading_event_type(prefix) else {
        return true;
    };
    dispatch_lifecycle(
        event_type,
        json_string_field(prefix, br#""toolName""#),
        json_string_field(prefix, br#""toolCallId""#).map(str::to_string),
        pending,
        seen_starts,
        seen_completions,
        callback,
    )
}

fn dispatch_lifecycle<F>(
    event_type: &[u8],
    tool_name: Option<&str>,
    tool_call_id: Option<String>,
    pending: &mut HashMap<String, InputKind>,
    seen_starts: &mut HashSet<String>,
    seen_completions: &mut HashSet<String>,
    callback: &mut F,
) -> bool
where
    F: FnMut(LifecycleEvent) -> bool,
{
    match event_type {
        TOOL_START => {
            let kind = match tool_name {
                Some("ask_user") => InputKind::Question,
                Some("exit_plan_mode") => InputKind::PlanApproval,
                _ => return true,
            };
            let Some(tool_call_id) = tool_call_id.filter(|id| !id.is_empty()) else {
                return true;
            };
            if !seen_starts.insert(tool_call_id.clone()) {
                return true;
            }
            pending.insert(tool_call_id.clone(), kind);
            callback(LifecycleEvent::InputRequested { tool_call_id, kind })
        }
        TOOL_COMPLETE => {
            let Some(tool_call_id) = tool_call_id.filter(|id| !id.is_empty()) else {
                return true;
            };
            if seen_completions.contains(&tool_call_id) {
                return true;
            }
            let Some(kind) = pending.remove(&tool_call_id) else {
                return true;
            };
            seen_completions.insert(tool_call_id.clone());
            callback(LifecycleEvent::InputResolved { tool_call_id, kind })
        }
        _ => true,
    }
}

#[derive(Clone, Copy)]
enum CandidateKind {
    Start,
    Complete,
}

/// Whether the session has been quiet long enough to say so, and is due another reminder.
///
/// Split out from the worker loop so the rule can be tested without waiting real seconds
/// for it. Re-reports rather than firing once, because the state this corrects may only
/// be entered *after* the quiet began: a progress sequence arriving a minute into the
/// silence would otherwise pin the pane to working with no further quiet ever announced.
fn should_report_quiet(
    outstanding_tools: i64,
    since_growth: Duration,
    since_last_report: Option<Duration>,
) -> bool {
    // A tool call in flight explains any amount of silence. Builds and test runs write
    // nothing for minutes at a time, and that is a working session, not an idle one.
    if outstanding_tools > 0 {
        return false;
    }
    since_growth >= QUIET_AFTER && since_last_report.is_none_or(|sent| sent >= QUIET_AFTER)
}

/// Keep a running count of tool calls in flight, reading only the leading `type`.
///
/// Deliberately blind to which tool it is. The question being answered is "could this
/// session be busy in a way that writes nothing?", and a long build is exactly as silent
/// as a long test run.
fn count_tool(line: &[u8], outstanding_tools: &mut i64) {
    match leading_event_type(line) {
        Some(TOOL_START) => *outstanding_tools += 1,
        Some(TOOL_COMPLETE) => *outstanding_tools -= 1,
        _ => {}
    }
}

const SYSTEM_NOTIFICATION: &[u8] = b"system.notification";
/// How Copilot's shell tool reports a command it started in async mode. The detached
/// variant reads "started in detached background", which this never matches: a
/// detached shell is meant to outlive the turn and holds nothing up.
const ASYNC_STARTED: &[u8] = b"started in background with shellId: ";
/// How the shell tool reports a command Copilot backgrounded for overrunning its limit.
const STILL_RUNNING: &[u8] = b" is still running after";
const SHELL_ID_IN_RESULT: &[u8] = b"shellId: ";
/// How reading a finished shell's output reports it, when no notice was ever written.
const READ_COMPLETED: &[u8] = b" completed with exit code";
const SHELL_COMPLETED: &[u8] = br#""shell_completed""#;
const DETACHED_TELEMETRY: &[u8] = br#""detached":"true""#;

enum ShellChange {
    Started(String, ShellKind),
    /// `bool`: by the completion notice Copilot writes as it wakes the agent.
    Gone(String, bool),
    Clear,
}

/// Follow the agent's background shells through the sentences Copilot writes about
/// them, reading no more of a record than the words that name them.
///
/// Nothing here parses JSON: a shell's result record can be enormous, and the sentences
/// that matter sit at its front. An id is taken from the text after its marker and runs
/// to the first character an id cannot contain.
fn track_shells<F>(
    line: &[u8],
    shells: &mut HashMap<String, BackgroundShell>,
    callback: &mut F,
) -> bool
where
    F: FnMut(LifecycleEvent) -> bool,
{
    let Some(event_type) = leading_event_type(line) else {
        return true;
    };
    let change = match event_type {
        TOOL_COMPLETE if contains_bytes(line, SHELL_ID_IN_RESULT) => shell_change_in_result(line),
        TOOL_START
            if json_string_field(line, br#""toolName""#)
                .is_some_and(|name| name.starts_with("stop_")) =>
        {
            json_string_field(line, br#""shellId""#)
                .map(|id| ShellChange::Gone(id.to_string(), false))
        }
        SYSTEM_NOTIFICATION if contains_bytes(line, SHELL_COMPLETED) => {
            json_string_field(line, br#""shellId""#)
                .map(|id| ShellChange::Gone(id.to_string(), true))
        }
        // An interrupt ends the wait whatever the shells do next; a resume starts the
        // session over, and the monitor begins at launch anyway.
        b"abort" | b"session.resume" => Some(ShellChange::Clear),
        _ => None,
    };
    let Some(change) = change else {
        return true;
    };
    let completed = match change {
        ShellChange::Started(id, kind) => {
            shells.insert(
                id.clone(),
                BackgroundShell {
                    id,
                    kind,
                    since: Instant::now(),
                },
            );
            false
        }
        ShellChange::Gone(id, by_notice) => {
            if shells.remove(&id).is_none() {
                return true;
            }
            by_notice
        }
        ShellChange::Clear => {
            if shells.is_empty() {
                return true;
            }
            shells.clear();
            false
        }
    };
    let mut current: Vec<BackgroundShell> = shells.values().cloned().collect();
    current.sort_by_key(|shell| shell.since);
    callback(LifecycleEvent::BackgroundShells {
        shells: current,
        completed,
    })
}

fn shell_change_in_result(line: &[u8]) -> Option<ShellChange> {
    if let Some(id) =
        shell_id_after(line, ASYNC_STARTED).filter(|_| !contains_bytes(line, DETACHED_TELEMETRY))
    {
        return Some(ShellChange::Started(id.to_string(), ShellKind::Async));
    }
    // Every "shellId: " in the record is tried, because a listing of active shells
    // names several and only the sentence after the name says which this is.
    let mut from = 0;
    while let Some(id) = shell_id_after(&line[from..], SHELL_ID_IN_RESULT) {
        let start = position(&line[from..], SHELL_ID_IN_RESULT)? + from + SHELL_ID_IN_RESULT.len();
        let after = &line[start + id.len()..];
        if after.starts_with(STILL_RUNNING) {
            return Some(ShellChange::Started(
                id.to_string(),
                ShellKind::Backgrounded,
            ));
        }
        if after.starts_with(READ_COMPLETED) {
            return Some(ShellChange::Gone(id.to_string(), false));
        }
        from = start + id.len();
    }
    None
}

/// The shell id written right after `marker`: letters, digits, `_`, `-` and `.`, which
/// is what Copilot accepts as one.
fn shell_id_after<'a>(line: &'a [u8], marker: &[u8]) -> Option<&'a str> {
    let start = position(line, marker)? + marker.len();
    let len = line[start..]
        .iter()
        .take_while(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        .count();
    if len == 0 {
        return None;
    }
    std::str::from_utf8(&line[start..start + len]).ok()
}

fn position(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn candidate_kind(line: &[u8], pending: &HashMap<String, InputKind>) -> Option<CandidateKind> {
    match leading_event_type(line)? {
        TOOL_START
            if contains_bytes(line, br#""ask_user""#)
                || contains_bytes(line, br#""exit_plan_mode""#) =>
        {
            Some(CandidateKind::Start)
        }
        TOOL_COMPLETE
            if !pending.is_empty()
                && pending
                    .keys()
                    .any(|tool_call_id| contains_bytes(line, tool_call_id.as_bytes())) =>
        {
            Some(CandidateKind::Complete)
        }
        _ => None,
    }
}

/// Inspect the leading `type` field without parsing the record or its payload.
fn leading_event_type(line: &[u8]) -> Option<&[u8]> {
    let mut cursor = 0;
    skip_ascii_whitespace(line, &mut cursor);
    if line.get(cursor)? != &b'{' {
        return None;
    }
    cursor += 1;
    skip_ascii_whitespace(line, &mut cursor);
    if !line.get(cursor..)?.starts_with(br#""type""#) {
        return None;
    }
    cursor += br#""type""#.len();
    skip_ascii_whitespace(line, &mut cursor);
    if line.get(cursor)? != &b':' {
        return None;
    }
    cursor += 1;
    skip_ascii_whitespace(line, &mut cursor);
    if line.get(cursor)? != &b'"' {
        return None;
    }
    cursor += 1;
    let end = line[cursor..]
        .iter()
        .position(|byte| *byte == b'"')
        .map(|end| cursor + end)?;
    Some(&line[cursor..end])
}

fn skip_ascii_whitespace(line: &[u8], cursor: &mut usize) {
    while line.get(*cursor).is_some_and(u8::is_ascii_whitespace) {
        *cursor += 1;
    }
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn json_string_field<'a>(line: &'a [u8], field: &[u8]) -> Option<&'a str> {
    let field_start = line
        .windows(field.len())
        .position(|window| window == field)?;
    let mut cursor = field_start + field.len();
    skip_ascii_whitespace(line, &mut cursor);
    if line.get(cursor)? != &b':' {
        return None;
    }
    cursor += 1;
    skip_ascii_whitespace(line, &mut cursor);
    if line.get(cursor)? != &b'"' {
        return None;
    }
    cursor += 1;
    let start = cursor;
    while let Some(byte) = line.get(cursor) {
        match byte {
            b'"' => return std::str::from_utf8(&line[start..cursor]).ok(),
            b'\\' => return None,
            _ => cursor += 1,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::AtomicUsize;

    fn start(id: &str, tool_name: &str) -> String {
        format!(
            r#"{{"type":"tool.execution_start","data":{{"toolName":"{tool_name}","toolCallId":"{id}","arguments":{{"question":"private question text"}}}}}}"#
        )
    }

    fn complete(id: &str) -> String {
        format!(
            r#"{{"type":"tool.execution_complete","data":{{"toolCallId":"{id}","result":"private result"}}}}"#
        )
    }

    fn append(path: &Path, text: &str) {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        file.write_all(text.as_bytes()).unwrap();
        file.flush().unwrap();
    }

    fn poll(tail: &mut LifecycleTail) -> Vec<LifecycleEvent> {
        let mut events = Vec::new();
        assert!(tail
            .poll(&mut |event| {
                events.push(event);
                true
            })
            .unwrap());
        events
    }

    /// Tool calls are counted whatever tool they are, not just the ones that block.
    ///
    /// The count is what stops a long build being mistaken for an idle session, so it has
    /// to include the tools nothing else here cares about — `candidate_kind` filters those
    /// out before any JSON is parsed, and the count has to survive that filter.
    #[test]
    fn every_tool_counts_towards_being_busy_not_only_the_blocking_ones() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        append(&path, "{\"type\":\"session.start\"}\n");
        let mut tail = LifecycleTail::new(path.clone(), 0);
        poll(&mut tail);
        assert_eq!(tail.outstanding_tools, 0);

        append(&path, &(start("build-1", "bash") + "\n"));
        poll(&mut tail);
        assert_eq!(
            tail.outstanding_tools, 1,
            "a shell command is not a question, and still means the session is busy"
        );
        assert!(tail.advanced, "bytes arrived");

        append(&path, &(complete("build-1") + "\n"));
        poll(&mut tail);
        assert_eq!(tail.outstanding_tools, 0);

        poll(&mut tail);
        assert!(!tail.advanced, "nothing new was written");
    }

    /// The sentences below are copied from a real session's log, because they are the
    /// whole interface: Copilot writes them, nothing else says a shell is still running.
    fn backgrounded(id: &str) -> String {
        format!(
            r#"{{"type":"tool.execution_complete","data":{{"toolCallId":"call_{id}","success":true,"result":{{"content":"<command with shellId: {id} is still running after 30 seconds. The command is still running but hasn't produced output yet. You will be automatically notified when it completes>"}},"toolTelemetry":{{"properties":{{"executionMode":"sync","detached":"false"}}}}}}}}"#
        )
    }

    fn async_started(id: &str, detached: bool) -> String {
        let (phrase, flag) = if detached {
            ("started in detached background", "true")
        } else {
            ("started in background", "false")
        };
        format!(
            r#"{{"type":"tool.execution_complete","data":{{"toolCallId":"call_{id}","success":true,"result":{{"content":"<command {phrase} with shellId: {id}>"}},"toolTelemetry":{{"properties":{{"executionMode":"async","detached":"{flag}"}}}}}}}}"#
        )
    }

    fn completion_notice(id: &str) -> String {
        format!(
            r#"{{"type":"system.notification","data":{{"content":"<system_notification>\nShell command \"Run it\" (shellId: {id}) has completed successfully.\n</system_notification>","kind":{{"type":"shell_completed","shellId":"{id}","exitCode":0}}}}}}"#
        )
    }

    fn shell_ids(events: &[LifecycleEvent]) -> Vec<Vec<(String, ShellKind, bool)>> {
        events
            .iter()
            .filter_map(|event| match event {
                LifecycleEvent::BackgroundShells { shells, completed } => Some(
                    shells
                        .iter()
                        .map(|shell| (shell.id.clone(), shell.kind, *completed))
                        .collect(),
                ),
                _ => None,
            })
            .collect()
    }

    /// A shell Copilot backgrounded for overrunning is a job the agent waits on, and
    /// the wait ends with the completion notice; a detached shell is a server left for
    /// the user and is never a wait at all.
    #[test]
    fn a_backgrounded_shell_is_waited_on_until_its_notice_and_a_detached_one_never_is() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let mut tail = LifecycleTail::new(path.clone(), 0);

        append(&path, &(async_started("report-server", true) + "\n"));
        assert!(
            shell_ids(&poll(&mut tail)).is_empty(),
            "a detached server holds nothing up"
        );

        append(&path, &(backgrounded("fits-inference") + "\n"));
        assert_eq!(
            shell_ids(&poll(&mut tail)),
            vec![vec![(
                "fits-inference".to_string(),
                ShellKind::Backgrounded,
                false
            )]]
        );

        append(&path, &(completion_notice("fits-inference") + "\n"));
        assert_eq!(
            shell_ids(&poll(&mut tail)),
            vec![vec![]],
            "the notice empties the set, and says so was by notice"
        );
        let events = {
            append(&path, &(completion_notice("fits-inference") + "\n"));
            poll(&mut tail)
        };
        assert!(
            shell_ids(&events).is_empty(),
            "a notice for a shell not waited on changes nothing"
        );
    }

    /// Only one in twelve stopped shells ever gets a completion notice, and a shell
    /// whose output the agent read after it finished gets none either: both have to end
    /// the wait, or the tab spins for a shell that ended an hour ago.
    #[test]
    fn a_stop_or_a_read_of_the_finished_output_ends_the_wait_as_the_notice_does() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let mut tail = LifecycleTail::new(path.clone(), 0);

        append(&path, &(async_started("worker", false) + "\n"));
        append(&path, &(backgrounded("fits") + "\n"));
        let _ = poll(&mut tail);
        append(
            &path,
            r#"{"type":"tool.execution_start","data":{"toolCallId":"call_s","toolName":"stop_powershell","arguments":{"shellId":"worker"}}}
"#,
        );
        let after_stop = shell_ids(&poll(&mut tail));
        assert_eq!(
            after_stop,
            vec![vec![("fits".to_string(), ShellKind::Backgrounded, false)]],
            "the stop removed the worker and left the fits"
        );

        append(
            &path,
            r#"{"type":"tool.execution_complete","data":{"toolCallId":"call_r","result":{"content":"<shellId: fits completed with exit code 0>"}}}
"#,
        );
        assert_eq!(shell_ids(&poll(&mut tail)), vec![vec![]]);
    }

    /// Copilot reuses shell ids, so a second launch under a name owns the next
    /// completion; and a listing of active shells, which names several, starts nothing.
    #[test]
    fn a_reused_id_is_one_shell_and_a_listing_of_shells_starts_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let mut tail = LifecycleTail::new(path.clone(), 0);

        append(&path, &(async_started("worker", false) + "\n"));
        append(&path, &(completion_notice("worker") + "\n"));
        append(&path, &(async_started("worker", false) + "\n"));
        let events = shell_ids(&poll(&mut tail));
        assert_eq!(events.len(), 3);
        assert_eq!(
            events[2],
            vec![("worker".to_string(), ShellKind::Async, false)],
            "the second launch is waited on again"
        );

        append(
            &path,
            r#"{"type":"tool.execution_complete","data":{"toolCallId":"call_l","result":{"content":"Active shell sessions:\n  shellId: report-server, pid: 4704, mode: async, status: running, elapsed: 64748s\n  shellId: worker, pid: 11, mode: async, status: running"}}}
"#,
        );
        assert!(
            shell_ids(&poll(&mut tail)).is_empty(),
            "a listing is neither a start nor an end"
        );
        assert_eq!(tail.shells.len(), 1);
    }

    /// An interrupt ends the wait whatever the shells go on doing.
    #[test]
    fn an_abort_clears_every_waited_shell() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let mut tail = LifecycleTail::new(path.clone(), 0);
        append(&path, &(backgrounded("fits") + "\n"));
        let _ = poll(&mut tail);
        append(&path, "{\"type\":\"abort\",\"data\":{}}\n");
        assert_eq!(shell_ids(&poll(&mut tail)), vec![vec![]]);
    }

    /// A shell's result can run past the prefix the monitor keeps of a huge record; the
    /// sentence that matters is at its front, so the wait is still seen.
    #[test]
    fn a_backgrounded_shell_in_an_oversized_record_is_still_seen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let mut tail = LifecycleTail::new(path.clone(), 0);
        let padding = "x".repeat(MAX_RECORD_PREFIX + 10);
        let record = format!(
            r#"{{"type":"tool.execution_complete","data":{{"toolCallId":"call_big","result":{{"content":"<command with shellId: big-job is still running after 30 seconds. {padding}>"}}}}}}"#
        );
        append(&path, &(record + "\n"));
        assert_eq!(
            shell_ids(&poll(&mut tail)),
            vec![vec![(
                "big-job".to_string(),
                ShellKind::Backgrounded,
                false
            )]]
        );
    }

    /// The rule that decides a session is idle, without waiting real seconds for it.
    #[test]
    fn quiet_needs_silence_and_nothing_in_flight_and_then_repeats() {
        let long = QUIET_AFTER + Duration::from_secs(1);
        let short = QUIET_AFTER / 2;

        assert!(!should_report_quiet(0, short, None), "barely quiet yet");
        assert!(should_report_quiet(0, long, None), "silent and idle");
        assert!(
            !should_report_quiet(1, long, None),
            "a tool in flight explains any silence; builds write nothing for minutes"
        );
        assert!(
            should_report_quiet(-1, long, None),
            "a log joined mid-run can complete a tool it never saw start"
        );
        assert!(
            !should_report_quiet(0, long, Some(short)),
            "already said so recently"
        );
        assert!(
            should_report_quiet(0, long, Some(long)),
            "said again, because whoever needs it may have only just started needing it"
        );
    }

    #[test]
    fn sees_records_appended_between_capture_and_monitor_start() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        append(&path, "{\"type\":\"session.start\"}\n");
        let start_len = capture_file_len(&path);

        append(&path, &(start("question-1", "ask_user") + "\n"));
        let mut tail = LifecycleTail::new(path, start_len);

        assert_eq!(
            poll(&mut tail),
            vec![LifecycleEvent::InputRequested {
                tool_call_id: "question-1".into(),
                kind: InputKind::Question,
            }]
        );
        assert!(tail.partial.len() <= MAX_RECORD_PREFIX);
        assert!(tail.partial.capacity() <= MAX_RECORD_PREFIX);
    }

    #[test]
    fn oversized_question_and_completion_use_only_the_structured_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let huge = "x".repeat(MAX_RECORD_PREFIX * 2);
        append(
            &path,
            &format!(
                r#"{{"type":"tool.execution_start","data":{{"toolName":"ask_user","toolCallId":"question-1","arguments":{{"question":"{huge}"}}}}}}"#
            ),
        );
        append(&path, "\n");
        let mut tail = LifecycleTail::new(path.clone(), 0);
        assert_eq!(
            poll(&mut tail),
            vec![LifecycleEvent::InputRequested {
                tool_call_id: "question-1".into(),
                kind: InputKind::Question,
            }]
        );
        assert!(tail.partial.capacity() <= MAX_RECORD_PREFIX);

        append(
            &path,
            &format!(
                r#"{{"type":"tool.execution_complete","data":{{"toolCallId":"question-1","success":true,"result":{{"content":"{huge}"}}}}}}"#
            ),
        );
        append(&path, "\n");
        assert_eq!(
            poll(&mut tail),
            vec![LifecycleEvent::InputResolved {
                tool_call_id: "question-1".into(),
                kind: InputKind::Question,
            }]
        );
        assert!(tail.partial.capacity() <= MAX_RECORD_PREFIX);
    }

    #[test]
    fn baseline_detects_replacement_before_the_first_poll() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        append(
            &path,
            &format!(
                "{{\"type\":\"assistant.message\",\"data\":{{\"content\":\"{}\"}}}}\n",
                "old".repeat(100)
            ),
        );
        let baseline = capture_file_baseline(&path).unwrap();

        std::fs::write(
            &path,
            format!(
                "{{\"type\":\"assistant.message\",\"data\":{{\"content\":\"{}\"}}}}\n{}\n",
                "replacement".repeat(100),
                start("replacement-history", "ask_user")
            ),
        )
        .unwrap();
        let mut tail = LifecycleTail::from_baseline(path.clone(), baseline);
        assert_eq!(poll(&mut tail), vec![LifecycleEvent::Reset]);

        append(&path, &(start("question-2", "ask_user") + "\n"));
        assert_eq!(
            poll(&mut tail),
            vec![LifecycleEvent::InputRequested {
                tool_call_id: "question-2".into(),
                kind: InputKind::Question,
            }]
        );
    }

    #[test]
    fn retains_partial_lines_until_the_newline_arrives() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let record = start("question-1", "ask_user");
        let split = record.len() / 2;
        let mut tail = LifecycleTail::new(path.clone(), 0);

        append(&path, &record[..split]);
        assert!(poll(&mut tail).is_empty());
        assert!(!tail.partial.is_empty());

        append(&path, &format!("{}\n", &record[split..]));
        assert_eq!(
            poll(&mut tail),
            vec![LifecycleEvent::InputRequested {
                tool_call_id: "question-1".into(),
                kind: InputKind::Question,
            }]
        );
        assert!(tail.partial.is_empty());
    }

    #[test]
    fn ignores_nonmatching_completions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        append(
            &path,
            &format!(
                "{}\n{}\n{}\n",
                start("question-1", "ask_user"),
                complete("someone-else"),
                complete("question-1")
            ),
        );
        let mut tail = LifecycleTail::new(path, 0);

        assert_eq!(
            poll(&mut tail),
            vec![
                LifecycleEvent::InputRequested {
                    tool_call_id: "question-1".into(),
                    kind: InputKind::Question,
                },
                LifecycleEvent::InputResolved {
                    tool_call_id: "question-1".into(),
                    kind: InputKind::Question,
                },
            ]
        );
    }

    #[test]
    fn reports_both_input_kinds_in_file_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        append(
            &path,
            &format!(
                "{}\n{}\n{}\n{}\n",
                start("question-1", "ask_user"),
                start("plan-1", "exit_plan_mode"),
                complete("plan-1"),
                complete("question-1")
            ),
        );
        let mut tail = LifecycleTail::new(path, 0);

        assert_eq!(
            poll(&mut tail),
            vec![
                LifecycleEvent::InputRequested {
                    tool_call_id: "question-1".into(),
                    kind: InputKind::Question,
                },
                LifecycleEvent::InputRequested {
                    tool_call_id: "plan-1".into(),
                    kind: InputKind::PlanApproval,
                },
                LifecycleEvent::InputResolved {
                    tool_call_id: "plan-1".into(),
                    kind: InputKind::PlanApproval,
                },
                LifecycleEvent::InputResolved {
                    tool_call_id: "question-1".into(),
                    kind: InputKind::Question,
                },
            ]
        );
    }

    #[test]
    fn callback_can_stop_ordered_delivery() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        append(
            &path,
            &format!(
                "{}\n{}\n",
                start("question-1", "ask_user"),
                start("question-2", "ask_user")
            ),
        );
        let mut tail = LifecycleTail::new(path, 0);
        let mut events = Vec::new();

        assert!(!tail
            .poll(&mut |event| {
                events.push(event);
                false
            })
            .unwrap());
        assert_eq!(
            events,
            vec![LifecycleEvent::InputRequested {
                tool_call_id: "question-1".into(),
                kind: InputKind::Question,
            }]
        );
    }

    #[test]
    fn deduplicates_starts_and_completions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        append(
            &path,
            &format!(
                "{0}\n{0}\n{1}\n{1}\n",
                start("question-1", "ask_user"),
                complete("question-1")
            ),
        );
        let mut tail = LifecycleTail::new(path, 0);

        assert_eq!(
            poll(&mut tail),
            vec![
                LifecycleEvent::InputRequested {
                    tool_call_id: "question-1".into(),
                    kind: InputKind::Question,
                },
                LifecycleEvent::InputResolved {
                    tool_call_id: "question-1".into(),
                    kind: InputKind::Question,
                },
            ]
        );
    }

    #[test]
    fn truncation_skips_replacement_history_without_replay() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        append(&path, "{\"type\":\"session.start\"}\n");
        let mut tail = LifecycleTail::new(path.clone(), capture_file_len(&path));

        append(&path, &(start("question-1", "ask_user") + "\n"));
        assert_eq!(poll(&mut tail).len(), 1);

        let mut replacement = File::create(&path).unwrap();
        let replacement_noise = "x".repeat(1024);
        writeln!(
            replacement,
            "{{\"type\":\"assistant.message\",\"data\":{{\"content\":\"{replacement_noise}\"}}}}"
        )
        .unwrap();
        writeln!(replacement, "{}", start("replacement-history", "ask_user")).unwrap();
        replacement.flush().unwrap();
        assert!(capture_file_len(&path) > tail.offset);
        assert_eq!(poll(&mut tail), vec![LifecycleEvent::Reset]);
        assert_eq!(poll(&mut tail), Vec::<LifecycleEvent>::new());

        append(&path, &(start("question-2", "ask_user") + "\n"));
        assert_eq!(
            poll(&mut tail),
            vec![LifecycleEvent::InputRequested {
                tool_call_id: "question-2".into(),
                kind: InputKind::Question,
            }]
        );
    }

    #[test]
    fn deletion_clears_pending_input_before_a_recreated_file_is_tailed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        append(&path, &(start("question-1", "ask_user") + "\n"));
        let mut tail = LifecycleTail::new(path.clone(), 0);
        assert_eq!(poll(&mut tail).len(), 1);

        std::fs::remove_file(&path).unwrap();
        assert_eq!(poll(&mut tail), vec![LifecycleEvent::Reset]);
        assert!(poll(&mut tail).is_empty());

        append(&path, &(start("question-2", "ask_user") + "\n"));
        assert_eq!(
            poll(&mut tail),
            vec![LifecycleEvent::InputRequested {
                tool_call_id: "question-2".into(),
                kind: InputKind::Question,
            }]
        );
    }

    #[test]
    fn skips_a_huge_unrelated_line_before_json_parsing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let huge = "x".repeat(2 * 1024 * 1024);
        append(
            &path,
            &format!(
                "{{\"type\":\"assistant.message\",\"data\":{{\"content\":\"{huge}\"}}}}\n{}\n",
                start("question-1", "ask_user")
            ),
        );
        let mut tail = LifecycleTail::new(path, 0);

        assert_eq!(
            poll(&mut tail),
            vec![LifecycleEvent::InputRequested {
                tool_call_id: "question-1".into(),
                kind: InputKind::Question,
            }]
        );
        assert!(tail.partial.len() <= MAX_RECORD_PREFIX);
        assert!(tail.partial.capacity() <= MAX_RECORD_PREFIX);
    }

    #[test]
    fn follows_a_file_created_after_monitor_initialization() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let mut tail = LifecycleTail::new(path.clone(), capture_file_len(&path));

        assert!(poll(&mut tail).is_empty());
        append(&path, &(start("question-1", "ask_user") + "\n"));

        assert_eq!(
            poll(&mut tail),
            vec![LifecycleEvent::InputRequested {
                tool_call_id: "question-1".into(),
                kind: InputKind::Question,
            }]
        );
    }

    #[test]
    fn drop_sets_stop_and_joins_the_worker() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let calls = Arc::new(AtomicUsize::new(0));
        let callback_calls = Arc::clone(&calls);
        let monitor = LifecycleMonitor::start_from_baseline(
            path.clone(),
            LifecycleBaseline::default(),
            move |_| {
                callback_calls.fetch_add(1, Ordering::Relaxed);
                true
            },
        );
        let stopped = Arc::clone(&monitor.stop);

        drop(monitor);

        assert!(stopped.load(Ordering::Acquire));
        append(&path, &(start("question-1", "ask_user") + "\n"));
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }
}
