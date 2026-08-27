#![cfg_attr(test, allow(dead_code))]

use crate::config::{normalize_ntfy_server, NotificationConfig};
use serde::Serialize;
use serde_json::Value;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const RETRY_DELAY: Duration = Duration::from_millis(250);
const ATTEMPTS: usize = 2;
const MAX_TITLE_CHARS: usize = 120;
const MAX_MESSAGE_BYTES: usize = 3_500;
const EVENT_TAIL_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationKind {
    Ready,
    Error,
}

impl NotificationKind {
    fn message(self) -> &'static str {
        match self {
            Self::Ready => "Ready for attention",
            Self::Error => "Copilot reported an error",
        }
    }

    fn priority(self) -> u8 {
        match self {
            Self::Ready => 3,
            Self::Error => 4,
        }
    }

    fn tags(self) -> [&'static str; 2] {
        match self {
            Self::Ready => ["robot", "question"],
            Self::Error => ["robot", "warning"],
        }
    }
}

#[derive(Debug, Clone)]
pub struct NotificationRequest {
    pub config: NotificationConfig,
    pub access_token: String,
    pub verbose: bool,
    pub session_title: String,
    pub kind: NotificationKind,
    pub events_path: Option<PathBuf>,
    pub events_start: Option<u64>,
    pub events_end: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationResult {
    pub result: Result<(), String>,
}

pub struct NotificationWorker {
    sender: mpsc::Sender<NotificationRequest>,
    receiver: mpsc::Receiver<NotificationResult>,
}

impl NotificationWorker {
    pub fn start() -> Self {
        let (request_sender, request_receiver) = mpsc::channel::<NotificationRequest>();
        let (result_sender, result_receiver) = mpsc::channel::<NotificationResult>();
        std::thread::spawn(move || {
            while let Ok(request) = request_receiver.recv() {
                let result = publish_with_retry(&request).map_err(|error| error.to_string());
                if result_sender.send(NotificationResult { result }).is_err() {
                    return;
                }
            }
        });
        Self {
            sender: request_sender,
            receiver: result_receiver,
        }
    }

    pub fn enqueue(&self, request: NotificationRequest) -> Result<(), String> {
        self.sender
            .send(request)
            .map_err(|_| "notification worker stopped".to_string())
    }

    pub fn try_result(&self) -> Option<NotificationResult> {
        self.receiver.try_recv().ok()
    }
}

#[derive(Serialize)]
struct NtfyPayload<'a> {
    topic: &'a str,
    message: &'a str,
    title: &'a str,
    priority: u8,
    tags: [&'a str; 2],
}

fn publish_with_retry(request: &NotificationRequest) -> Result<(), PublishError> {
    let mut last = None;
    for attempt in 0..ATTEMPTS {
        match publish_once(request) {
            Ok(()) => return Ok(()),
            Err(error) if error.retryable && attempt + 1 < ATTEMPTS => {
                last = Some(error);
                std::thread::sleep(RETRY_DELAY);
            }
            Err(error) => return Err(error),
        }
    }
    Err(last.unwrap_or_else(|| PublishError::permanent("notification delivery failed")))
}

fn publish_once(request: &NotificationRequest) -> Result<(), PublishError> {
    let server = normalize_ntfy_server(&request.config.server)
        .map_err(|_| PublishError::permanent("invalid ntfy server configuration"))?;
    crate::config::validate_ntfy_topic(request.config.topic.trim())
        .map_err(|_| PublishError::permanent("invalid ntfy topic configuration"))?;
    crate::config::validate_ntfy_access_token(request.access_token.trim())
        .map_err(|_| PublishError::permanent("invalid ntfy access token configuration"))?;
    let title = notification_title(&request.session_title);
    let message = notification_message(request);
    let payload = NtfyPayload {
        topic: request.config.topic.trim(),
        message: &message,
        title: &title,
        priority: request.kind.priority(),
        tags: request.kind.tags(),
    };
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        .build()
        .into();
    let mut publish = agent
        .post(&server)
        .header(
            "User-Agent",
            concat!("copilot-session-tui/", env!("CARGO_PKG_VERSION")),
        )
        // Keep this exact: ntfy otherwise treats the body as an ordinary text publish.
        .header("Content-Type", "application/json");
    let token = request.access_token.trim();
    if !token.is_empty() {
        publish = publish.header("Authorization", &format!("Bearer {token}"));
    }
    publish
        .send_json(&payload)
        .map(|_| ())
        .map_err(sanitize_ureq_error)
}

fn notification_message(request: &NotificationRequest) -> String {
    let status = request.kind.message();
    if !request.verbose {
        return status.to_string();
    }
    let context = match (
        request.kind,
        request.events_path.as_deref(),
        request.events_start,
        request.events_end,
    ) {
        (NotificationKind::Ready, Some(path), Some(start), Some(end)) if end > start => {
            latest_assistant_message(path, start, end)
        }
        _ => None,
    };
    match context {
        Some(context) => truncate_utf8(&format!("{status}\n\n{context}"), MAX_MESSAGE_BYTES),
        None => format!("{status}\n\nNo assistant response was persisted for this work cycle."),
    }
}

fn latest_assistant_message(path: &Path, cycle_start: u64, cycle_end: u64) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let length = file.metadata().ok()?.len().min(cycle_end);
    let start = cycle_start.max(length.saturating_sub(EVENT_TAIL_BYTES));
    if start >= length {
        return None;
    }
    let skip_partial = if start > 0 {
        file.seek(SeekFrom::Start(start - 1)).ok()?;
        let mut previous = [0u8; 1];
        file.read_exact(&mut previous).ok()?;
        previous[0] != b'\n'
    } else {
        false
    };
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = vec![0; (length - start) as usize];
    file.read_exact(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = text.lines();
    if skip_partial {
        lines.next();
    }
    lines
        .rev()
        .filter(|line| line.contains("assistant.message"))
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find_map(|event| {
            (event["type"].as_str() == Some("assistant.message"))
                .then(|| event["data"]["content"].as_str())
                .flatten()
                .map(sanitize_message)
                .filter(|message| !message.is_empty())
        })
}

fn sanitize_message(message: &str) -> String {
    message
        .chars()
        .filter_map(|character| match character {
            '\r' => None,
            '\n' | '\t' => Some(character),
            character if character.is_control() => Some(' '),
            character => Some(character),
        })
        .collect::<String>()
        .trim()
        .to_string()
}

fn truncate_utf8(message: &str, max_bytes: usize) -> String {
    if message.len() <= max_bytes {
        return message.to_string();
    }
    let mut end = max_bytes.saturating_sub('…'.len_utf8()).min(message.len());
    while !message.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}…", &message[..end])
}

fn notification_title(title: &str) -> String {
    let clean: String = title
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .take(MAX_TITLE_CHARS)
        .collect();
    let clean = clean.trim();
    if clean.is_empty() {
        "CST · Session".to_string()
    } else {
        format!("CST · {clean}")
    }
}

#[derive(Debug)]
struct PublishError {
    message: &'static str,
    retryable: bool,
}

impl PublishError {
    const fn retryable(message: &'static str) -> Self {
        Self {
            message,
            retryable: true,
        }
    }

    const fn permanent(message: &'static str) -> Self {
        Self {
            message,
            retryable: false,
        }
    }
}

impl std::fmt::Display for PublishError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message)
    }
}

fn sanitize_ureq_error(error: ureq::Error) -> PublishError {
    match error {
        ureq::Error::StatusCode(code) if code >= 500 => {
            PublishError::retryable("ntfy server returned an error")
        }
        ureq::Error::StatusCode(_) => PublishError::permanent("ntfy rejected the notification"),
        ureq::Error::BadUri(_) | ureq::Error::Http(_) => {
            PublishError::permanent("invalid ntfy request")
        }
        _ => PublishError::retryable("could not reach the ntfy server"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn title_removes_controls_and_has_a_length_bound() {
        let title = notification_title(&format!("hello\r\n{}\u{7}", "x".repeat(300)));

        assert!(title.starts_with("CST · hello  "));
        assert!(!title.contains('\r'));
        assert!(!title.contains('\n'));
        assert!(title.chars().count() <= MAX_TITLE_CHARS + 6);
    }

    #[test]
    fn direct_http_publish_sends_title_only_json() {
        let (url, request_receiver) = mock_server(200);
        let request = NotificationRequest {
            config: NotificationConfig {
                enabled: true,
                server: url,
                topic: "private_topic".to_string(),
                ..NotificationConfig::default()
            },
            access_token: String::new(),
            verbose: false,
            session_title: "Plan review".to_string(),
            kind: NotificationKind::Ready,
            events_path: None,
            events_start: None,
            events_end: None,
        };

        publish_once(&request).unwrap();
        let request = request_receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap();

        assert!(request.starts_with("POST / HTTP/1.1"));
        let content_types: Vec<&str> = request
            .split("\r\n\r\n")
            .next()
            .unwrap()
            .lines()
            .filter_map(|line| line.split_once(':'))
            .filter(|(name, _)| name.eq_ignore_ascii_case("content-type"))
            .map(|(_, value)| value.trim())
            .collect();
        assert_eq!(
            content_types,
            ["application/json"],
            "ntfy requires one exact JSON media type:\n{request}"
        );
        let body = request.split("\r\n\r\n").nth(1).unwrap();
        let payload: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(payload["topic"], "private_topic");
        assert_eq!(payload["title"], "CST · Plan review");
        assert_eq!(payload["message"], "Ready for attention");
        assert_eq!(payload["priority"], 3);
        assert_eq!(payload["tags"], serde_json::json!(["robot", "question"]));
        assert!(!body.contains("project"));
        assert!(!body.contains("session_id"));
    }

    #[test]
    fn error_notification_uses_high_priority_warning_tag() {
        let (url, request_receiver) = mock_server(200);
        let request = NotificationRequest {
            config: NotificationConfig {
                enabled: true,
                server: url,
                topic: "private_topic".to_string(),
                ..NotificationConfig::default()
            },
            access_token: String::new(),
            verbose: false,
            session_title: "Agent".to_string(),
            kind: NotificationKind::Error,
            events_path: None,
            events_start: None,
            events_end: None,
        };

        publish_once(&request).unwrap();
        let request = request_receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        let payload: serde_json::Value =
            serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(payload["priority"], 4);
        assert_eq!(payload["tags"], serde_json::json!(["robot", "warning"]));
    }

    #[test]
    fn worker_serializes_requests_and_reports_results() {
        let (url, request_receiver) = mock_server_responses(vec![200, 200]);
        let worker = NotificationWorker::start();
        for title in ["First", "Second"] {
            worker
                .enqueue(NotificationRequest {
                    config: NotificationConfig {
                        enabled: true,
                        server: url.clone(),
                        topic: "private_topic".to_string(),
                        ..NotificationConfig::default()
                    },
                    access_token: String::new(),
                    verbose: false,
                    session_title: title.to_string(),
                    kind: NotificationKind::Ready,
                    events_path: None,
                    events_start: None,
                    events_end: None,
                })
                .unwrap();
        }

        assert!(worker
            .receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .result
            .is_ok());
        assert!(worker
            .receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .result
            .is_ok());
        let first = request_receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        let second = request_receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert_eq!(request_payload(&first)["title"], "CST · First");
        assert_eq!(request_payload(&second)["title"], "CST · Second");
    }

    #[test]
    fn transient_server_error_retries_once() {
        let (url, request_receiver) = mock_server_responses(vec![500, 200]);
        let request = NotificationRequest {
            config: NotificationConfig {
                enabled: true,
                server: url,
                topic: "private_topic".to_string(),
                ..NotificationConfig::default()
            },
            access_token: String::new(),
            verbose: false,
            session_title: "Retry".to_string(),
            kind: NotificationKind::Ready,
            events_path: None,
            events_start: None,
            events_end: None,
        };

        publish_with_retry(&request).unwrap();

        request_receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        request_receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
    }

    #[test]
    fn access_token_uses_bearer_auth_without_entering_the_payload() {
        let (url, request_receiver) = mock_server(200);
        let request = NotificationRequest {
            config: NotificationConfig {
                enabled: true,
                server: url,
                topic: "private_topic".to_string(),
                ..NotificationConfig::default()
            },
            access_token: "tk_example_private_token".to_string(),
            verbose: false,
            session_title: "Authenticated".to_string(),
            kind: NotificationKind::Ready,
            events_path: None,
            events_start: None,
            events_end: None,
        };

        publish_once(&request).unwrap();
        let request = request_receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert!(request
            .to_ascii_lowercase()
            .contains("\r\nauthorization: bearer tk_example_private_token\r\n"));
        assert!(!request_payload(&request)
            .to_string()
            .contains("tk_example_private_token"));
    }

    #[test]
    fn verbose_mode_uses_the_latest_assistant_message_with_a_size_bound() {
        let temp = tempfile::tempdir().unwrap();
        let events = temp.path().join("events.jsonl");
        std::fs::write(
            &events,
            format!(
                "{}\n{}\n{}\n",
                serde_json::json!({
                    "type": "assistant.turn_start",
                    "data": {"turnId": "current-turn"}
                }),
                serde_json::json!({
                    "type": "assistant.message",
                    "data": {"turnId": "current-turn", "content": "Older response"}
                }),
                serde_json::json!({
                    "type": "assistant.message",
                    "data": {
                        "turnId": "current-turn",
                        "content": format!("Useful final context {}", "🚀".repeat(2_000))
                    }
                })
            ),
        )
        .unwrap();
        let request = NotificationRequest {
            config: NotificationConfig::default(),
            access_token: String::new(),
            verbose: true,
            session_title: "Verbose".to_string(),
            kind: NotificationKind::Ready,
            events_path: Some(events),
            events_start: Some(0),
            events_end: Some(
                std::fs::metadata(temp.path().join("events.jsonl"))
                    .unwrap()
                    .len(),
            ),
        };

        let message = notification_message(&request);

        assert!(message.starts_with("Ready for attention\n\nUseful final context"));
        assert!(!message.contains("Older response"));
        assert!(message.len() <= MAX_MESSAGE_BYTES);
        assert!(std::str::from_utf8(message.as_bytes()).is_ok());
    }

    #[test]
    fn verbose_mode_never_reuses_a_previous_work_cycles_response() {
        let temp = tempfile::tempdir().unwrap();
        let events = temp.path().join("events.jsonl");
        let previous = format!(
            "{}\n{}\n",
            serde_json::json!({
                "type": "assistant.turn_start",
                "data": {"turnId": "previous-turn"}
            }),
            serde_json::json!({
                "type": "assistant.message",
                "data": {"turnId": "previous-turn", "content": "Stale previous answer"}
            })
        );
        std::fs::write(&events, &previous).unwrap();
        let cycle_start = previous.len() as u64;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&events)
            .unwrap()
            .write_all(
                format!(
                    "{}\n",
                    serde_json::json!({
                        "type": "assistant.turn_start",
                        "data": {"turnId": "current-turn"}
                    })
                )
                .as_bytes(),
            )
            .unwrap();
        let mut request = NotificationRequest {
            config: NotificationConfig::default(),
            access_token: String::new(),
            verbose: true,
            session_title: "Verbose".to_string(),
            kind: NotificationKind::Ready,
            events_path: Some(events.clone()),
            events_start: Some(cycle_start),
            events_end: Some(std::fs::metadata(&events).unwrap().len()),
        };

        let message = notification_message(&request);
        assert!(message.contains("No assistant response was persisted"));
        assert!(!message.contains("Stale previous answer"));

        std::fs::OpenOptions::new()
            .append(true)
            .open(&events)
            .unwrap()
            .write_all(
                format!(
                    "{}\n",
                    serde_json::json!({
                        "type": "assistant.message",
                        "data": {"turnId": "current-turn", "content": "Current answer"}
                    })
                )
                .as_bytes(),
            )
            .unwrap();
        request.events_end = Some(std::fs::metadata(&events).unwrap().len());
        assert!(notification_message(&request).contains("Current answer"));

        request.kind = NotificationKind::Error;
        let error = notification_message(&request);
        assert!(error.contains("No assistant response was persisted"));
        assert!(!error.contains("Current answer"));
    }

    #[test]
    #[ignore = "requires an explicitly approved CST_NTFY_TEST_TOPIC"]
    fn live_ntfy_parses_the_structured_json_payload() {
        let topic = std::env::var("CST_NTFY_TEST_TOPIC")
            .expect("set CST_NTFY_TEST_TOPIC only after the user approves the exact topic");
        crate::config::validate_ntfy_topic(&topic).unwrap();
        let request = NotificationRequest {
            config: NotificationConfig {
                enabled: true,
                server: "https://ntfy.sh".to_string(),
                topic: topic.clone(),
                ..NotificationConfig::default()
            },
            access_token: String::new(),
            verbose: false,
            session_title: "Structured JSON E2E".to_string(),
            kind: NotificationKind::Ready,
            events_path: None,
            events_start: None,
            events_end: None,
        };

        if std::env::var_os("CST_NTFY_VERIFY_EXISTING").is_none() {
            publish_once(&request).unwrap();
        }

        let mut response = ureq::get(format!("https://ntfy.sh/{topic}/json?poll=1&since=all"))
            .call()
            .unwrap();
        let body = response.body_mut().read_to_string().unwrap();
        let event = body
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|event| event["event"] == "message")
            .expect("published message event");
        assert_eq!(event["title"], "CST · Structured JSON E2E");
        assert_eq!(event["message"], "Ready for attention");
    }

    fn mock_server(status: u16) -> (String, mpsc::Receiver<String>) {
        mock_server_responses(vec![status])
    }

    fn request_payload(request: &str) -> serde_json::Value {
        serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap()
    }

    fn mock_server_responses(statuses: Vec<u16>) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            for status in statuses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0u8; 4096];
                let header_end;
                loop {
                    let read = stream.read(&mut buffer).unwrap();
                    if read == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&buffer[..read]);
                    if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                        header_end = end + 4;
                        break;
                    }
                }
                let headers = String::from_utf8_lossy(&bytes[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("content-length:")
                            .or_else(|| line.strip_prefix("Content-Length:"))
                    })
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                while bytes.len() < header_end + content_length {
                    let read = stream.read(&mut buffer).unwrap();
                    if read == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buffer[..read]);
                }
                let _ = sender.send(String::from_utf8_lossy(&bytes).to_string());
                write!(
                    stream,
                    "HTTP/1.1 {status} OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
            }
        });
        (format!("http://{address}"), receiver)
    }
}
