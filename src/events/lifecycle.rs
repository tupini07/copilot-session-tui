use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{File, Metadata};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

const POLL_INTERVAL: Duration = Duration::from_millis(150);
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
    Reset,
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
    /// are neither retained nor included in [`LifecycleEvent`].
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
            while !worker_stop.load(Ordering::Acquire) {
                match tail.poll(&mut callback) {
                    Ok(true) => {}
                    Ok(false) => break,
                    // A missing, concurrently replaced, or temporarily unreadable
                    // file is retried on the next poll.
                    Err(_) => {}
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
        }
    }

    fn poll<F>(&mut self, callback: &mut F) -> io::Result<bool>
    where
        F: FnMut(LifecycleEvent) -> bool,
    {
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
            self.reset_to_end(&mut file, len, identity)?;
            return Ok(callback(LifecycleEvent::Reset));
        }
        self.identity = Some(identity);

        if len > self.offset {
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
    callback: &mut F,
) -> bool
where
    F: FnMut(LifecycleEvent) -> bool,
{
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
    callback: &mut F,
) -> bool
where
    F: FnMut(LifecycleEvent) -> bool,
{
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
