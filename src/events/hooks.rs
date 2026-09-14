use anyhow::{Context, Result};
use fs4::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookCommand {
    SessionStart,
    Working,
    Ready,
    Error,
    SessionEnd,
    Notification,
}

impl HookCommand {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "session-start" => Some(Self::SessionStart),
            "working" => Some(Self::Working),
            "ready" => Some(Self::Ready),
            "error" => Some(Self::Error),
            "session-end" => Some(Self::SessionEnd),
            "notification" => Some(Self::Notification),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookAttention {
    Elicitation,
    Permission,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum HookLifecycleEvent {
    SessionStarted {
        timestamp: u64,
    },
    Working {
        timestamp: u64,
    },
    Ready {
        timestamp: u64,
    },
    Error {
        timestamp: u64,
    },
    Awaiting {
        timestamp: u64,
        attention: HookAttention,
    },
    SessionEnded {
        timestamp: u64,
    },
}

#[derive(Deserialize)]
struct HookPayload {
    #[serde(rename = "sessionId", alias = "session_id")]
    session_id: String,
    timestamp: Option<u64>,
    #[serde(rename = "notification_type", alias = "notificationType")]
    notification_type: Option<String>,
    recoverable: Option<bool>,
}

impl HookLifecycleEvent {
    pub fn timestamp(self) -> u64 {
        match self {
            Self::SessionStarted { timestamp }
            | Self::Working { timestamp }
            | Self::Ready { timestamp }
            | Self::Error { timestamp }
            | Self::Awaiting { timestamp, .. }
            | Self::SessionEnded { timestamp } => timestamp,
        }
    }
}

pub fn record_stdin(command: HookCommand, copilot_home: &Path) -> Result<()> {
    let payload = parse_payload(std::io::stdin().lock())?;
    validate_session_id(&payload.session_id)?;
    let Some(event) = hook_event(command, &payload)? else {
        return Ok(());
    };
    append_event(&hook_events_path(copilot_home, &payload.session_id), event)
}

fn parse_payload(input: impl Read) -> Result<HookPayload> {
    serde_json::from_reader(input).context("Failed to parse Copilot hook payload")
}

fn hook_event(command: HookCommand, payload: &HookPayload) -> Result<Option<HookLifecycleEvent>> {
    let timestamp = payload.timestamp.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64
    });
    Ok(Some(match command {
        HookCommand::SessionStart => HookLifecycleEvent::SessionStarted { timestamp },
        HookCommand::Working => HookLifecycleEvent::Working { timestamp },
        HookCommand::Ready => HookLifecycleEvent::Ready { timestamp },
        HookCommand::Error if payload.recoverable == Some(true) => {
            return Ok(None);
        }
        HookCommand::Error => HookLifecycleEvent::Error { timestamp },
        HookCommand::SessionEnd => HookLifecycleEvent::SessionEnded { timestamp },
        HookCommand::Notification => {
            let notification = payload
                .notification_type
                .as_deref()
                .context("Copilot notification hook has no notification_type")?;
            let attention = match notification {
                "elicitation_dialog" => HookAttention::Elicitation,
                "permission_prompt" => HookAttention::Permission,
                other => anyhow::bail!("Unsupported Copilot notification type: {other}"),
            };
            HookLifecycleEvent::Awaiting {
                timestamp,
                attention,
            }
        }
    }))
}

fn validate_session_id(session_id: &str) -> Result<()> {
    let mut components = Path::new(session_id).components();
    if !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
    {
        anyhow::bail!("Invalid Copilot session id");
    }
    Ok(())
}

pub fn hook_events_path(copilot_home: &Path, session_id: &str) -> PathBuf {
    copilot_home
        .join("session-state")
        .join(session_id)
        .join(".cst-lifecycle.jsonl")
}

fn append_event(path: &Path, event: HookLifecycleEvent) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(path)
        .with_context(|| format!("Failed to open {}", path.display()))?;
    FileExt::lock(&file).with_context(|| format!("Failed to lock {}", path.display()))?;
    serde_json::to_writer(&mut file, &event)?;
    file.write_all(b"\n")?;
    file.flush()?;
    FileExt::unlock(&file)?;
    Ok(())
}

pub fn capture_file_len(path: &Path) -> u64 {
    std::fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or_default()
}

#[must_use = "dropping the monitor stops its worker"]
pub struct HookMonitor {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl HookMonitor {
    pub fn start<F>(path: PathBuf, offset: u64, mut callback: F) -> Self
    where
        F: FnMut(HookLifecycleEvent) -> bool + Send + 'static,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            let mut tail = HookTail {
                path,
                offset,
                partial: Vec::new(),
            };
            while !worker_stop.load(Ordering::Acquire) {
                if tail
                    .poll(&mut callback)
                    .is_ok_and(|keep_running| !keep_running)
                {
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

impl Drop for HookMonitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}

struct HookTail {
    path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
}

impl HookTail {
    fn poll<F>(&mut self, callback: &mut F) -> std::io::Result<bool>
    where
        F: FnMut(HookLifecycleEvent) -> bool,
    {
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
            Err(error) => return Err(error),
        };
        let length = file.metadata()?.len();
        if length < self.offset {
            self.offset = 0;
            self.partial.clear();
        }
        if length == self.offset {
            return Ok(true);
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let mut bytes = Vec::with_capacity((length - self.offset).min(64 * 1024) as usize);
        file.read_to_end(&mut bytes)?;
        self.offset += bytes.len() as u64;
        self.partial.extend_from_slice(&bytes);

        let mut consumed = 0;
        while let Some(end) = self.partial[consumed..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|end| consumed + end)
        {
            let line = &self.partial[consumed..end];
            if let Ok(event) = serde_json::from_slice::<HookLifecycleEvent>(line) {
                if !callback(event) {
                    return Ok(false);
                }
            }
            consumed = end + 1;
        }
        if consumed > 0 {
            self.partial.copy_within(consumed.., 0);
            self.partial.truncate(self.partial.len() - consumed);
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::mpsc;

    fn payload(json: &[u8]) -> HookPayload {
        parse_payload(Cursor::new(json)).unwrap()
    }

    #[test]
    fn streaming_payload_parser_reaches_scalars_after_large_private_values() {
        let json = format!(
            r#"{{"sessionId":"abc","message":"{}","notification_type":"permission_prompt","timestamp":42}}"#,
            "private".repeat(100_000)
        );
        let parsed = parse_payload(Cursor::new(json.as_bytes())).unwrap();
        assert_eq!(parsed.session_id, "abc");
        assert_eq!(
            parsed.notification_type.as_deref(),
            Some("permission_prompt")
        );
        assert_eq!(parsed.timestamp, Some(42));
    }

    #[test]
    fn notification_payload_maps_only_attention_types() {
        let event = hook_event(
            HookCommand::Notification,
            &payload(
                br#"{"sessionId":"abc","notification_type":"elicitation_dialog","message":"private"}"#,
            ),
        )
        .unwrap()
        .unwrap();
        assert!(matches!(
            event,
            HookLifecycleEvent::Awaiting {
                attention: HookAttention::Elicitation,
                ..
            }
        ));
        assert!(hook_event(
            HookCommand::Notification,
            &payload(br#"{"sessionId":"abc","notification_type":"shell_completed"}"#),
        )
        .is_err());
    }

    #[test]
    fn recoverable_errors_do_not_override_continuing_work() {
        assert!(hook_event(
            HookCommand::Error,
            &payload(br#"{"sessionId":"abc","recoverable":true}"#),
        )
        .unwrap()
        .is_none());
        assert!(matches!(
            hook_event(
                HookCommand::Error,
                &payload(br#"{"sessionId":"abc","recoverable":false}"#),
            )
            .unwrap(),
            Some(HookLifecycleEvent::Error { .. })
        ));
    }

    #[test]
    fn source_timestamp_is_preserved_for_cross_hook_ordering() {
        assert_eq!(
            hook_event(
                HookCommand::Working,
                &payload(br#"{"sessionId":"abc","timestamp":12345}"#),
            )
            .unwrap(),
            Some(HookLifecycleEvent::Working { timestamp: 12345 })
        );
    }

    #[test]
    fn writer_persists_no_prompt_or_notification_message() {
        let temp = tempfile::tempdir().unwrap();
        let path = hook_events_path(temp.path(), "session-1");
        append_event(&path, HookLifecycleEvent::Working { timestamp: 42 }).unwrap();
        let text = std::fs::read_to_string(path).unwrap();
        assert_eq!(text, "{\"state\":\"working\",\"timestamp\":42}\n");
        assert!(!text.contains("prompt"));
    }

    #[test]
    fn monitor_delivers_only_new_complete_records() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("events.jsonl");
        append_event(&path, HookLifecycleEvent::Working { timestamp: 1 }).unwrap();
        let baseline = capture_file_len(&path);
        let (sender, receiver) = mpsc::channel();
        let monitor = HookMonitor::start(path.clone(), baseline, move |event| {
            sender.send(event).is_ok()
        });
        append_event(&path, HookLifecycleEvent::Ready { timestamp: 2 }).unwrap();

        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(2)).unwrap(),
            HookLifecycleEvent::Ready { timestamp: 2 }
        );
        drop(monitor);
    }
}
