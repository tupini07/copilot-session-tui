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
    Question,
    PlanApproval,
    Error,
}

impl NotificationKind {
    fn message(self) -> &'static str {
        match self {
            Self::Ready => "Ready for attention",
            Self::Question => "Question waiting for your response",
            Self::PlanApproval => "Plan waiting for approval",
            Self::Error => "Copilot reported an error",
        }
    }

    fn priority(self) -> u8 {
        match self {
            Self::Ready => 3,
            Self::Question | Self::PlanApproval => 4,
            Self::Error => 4,
        }
    }

    fn tags(self) -> [&'static str; 2] {
        match self {
            Self::Ready => ["robot", "white_check_mark"],
            Self::Question => ["robot", "question"],
            Self::PlanApproval => ["robot", "clipboard"],
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
        (
            NotificationKind::Ready | NotificationKind::Question | NotificationKind::PlanApproval,
            Some(path),
            Some(start),
            Some(end),
        ) if end > start => latest_assistant_message(path, start, end),
        _ => None,
    };
    match context {
        Some(context) => truncate_utf8(&format!("{status}\n\n{context}"), MAX_MESSAGE_BYTES),
        None => format!("{status}\n\nNo assistant response was persisted for this work cycle."),
    }
}

/// The event log's lines for one work cycle, oldest first, bounded to the last two
/// megabytes so a long-running session's log is never read whole.
fn cycle_lines(path: &Path, cycle_start: u64, cycle_end: u64) -> Option<Vec<String>> {
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
    Some(lines.map(str::to_string).collect())
}

fn latest_assistant_message(path: &Path, cycle_start: u64, cycle_end: u64) -> Option<String> {
    cycle_lines(path, cycle_start, cycle_end)?
        .iter()
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

/// Longest question, choice, or plan summary the phone is shown. The last message is
/// bounded separately by the Telegram chunker.
const MAX_PROMPT_CHARS: usize = 2_000;
const MAX_CHOICES: usize = 20;

/// The tool call of `tool_name` that this cycle started and has not completed.
///
/// Read on demand at send time, like [`latest_assistant_message`]; the lifecycle monitor
/// deliberately keeps no payload text, and this does not change that.
fn pending_tool_call(
    path: &Path,
    cycle_start: u64,
    cycle_end: u64,
    tool_name: &str,
) -> Option<(String, Value)> {
    let lines = cycle_lines(path, cycle_start, cycle_end)?;
    let events: Vec<Value> = lines
        .iter()
        .filter(|line| line.contains("tool.execution_"))
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect();
    let completed: std::collections::HashSet<&str> = events
        .iter()
        .filter(|event| event["type"].as_str() == Some("tool.execution_complete"))
        .filter_map(|event| event["data"]["toolCallId"].as_str())
        .collect();
    events
        .iter()
        .rev()
        .filter(|event| event["type"].as_str() == Some("tool.execution_start"))
        .filter(|event| event["data"]["toolName"].as_str() == Some(tool_name))
        .find_map(|event| {
            let id = event["data"]["toolCallId"].as_str()?;
            (!completed.contains(id)).then(|| (id.to_string(), event["data"]["arguments"].clone()))
        })
}

fn bounded(text: &str) -> String {
    truncate_utf8(&sanitize_message(text), MAX_PROMPT_CHARS)
}

/// What Copilot recorded as the answer to a question, once the call completed.
///
/// The form is answered with keys played blind after its first field, so Copilot's
/// own record — `User responded: Medium`, or `User responded: colour=Red,
/// finish_time=…` — is the only trustworthy account of what was chosen, and what the
/// phone is shown as confirmation.
pub(crate) fn recorded_answer(
    path: &Path,
    cycle_start: u64,
    cycle_end: u64,
    tool_call_id: &str,
) -> Option<String> {
    let lines = cycle_lines(path, cycle_start, cycle_end)?;
    lines
        .iter()
        .rev()
        .filter(|line| line.contains("tool.execution_complete") && line.contains(tool_call_id))
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|event| event["data"]["toolCallId"].as_str() == Some(tool_call_id))
        .and_then(|event| {
            event["data"]["result"]["content"]
                .as_str()
                .map(|content| bounded(content.trim()))
        })
}

/// The question a session is asking, with every field and its choices.
pub(crate) fn pending_question(
    path: &Path,
    cycle_start: u64,
    cycle_end: u64,
) -> Option<crate::telegram::Question> {
    use crate::telegram::QuestionField;

    let (tool_call_id, arguments) = pending_tool_call(path, cycle_start, cycle_end, "ask_user")?;
    let text = arguments["question"]
        .as_str()
        .or_else(|| arguments["message"].as_str())
        .map(bounded)
        .unwrap_or_default();
    let strings = |values: Option<&Vec<Value>>| -> Vec<String> {
        values
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(bounded)
                    .filter(|choice| !choice.is_empty())
                    .take(MAX_CHOICES)
                    .collect()
            })
            .unwrap_or_default()
    };
    // Three shapes, all seen in real logs. Older Copilots: `question` and `choices`.
    // Current ones (1.0.104): an elicitation with `message` and a `requestedSchema`
    // whose properties are the fields, in order; a field's choices are an `enum` of
    // strings or a `oneOf` of `{const, title}` where the title is what the user sees,
    // and a field with neither is free text, with a `description` and a `default`.
    let mut fields: Vec<QuestionField> = arguments["requestedSchema"]["properties"]
        .as_object()
        .map(|properties| {
            properties
                .values()
                .map(|field| {
                    let mut choices = strings(field["enum"].as_array());
                    if choices.is_empty() {
                        if let Some(options) = field["oneOf"].as_array() {
                            choices = options
                                .iter()
                                .filter_map(|option| {
                                    option["title"]
                                        .as_str()
                                        .or_else(|| option["const"].as_str())
                                })
                                .map(bounded)
                                .filter(|choice| !choice.is_empty())
                                .take(MAX_CHOICES)
                                .collect();
                        }
                    }
                    QuestionField {
                        title: field["title"].as_str().map(bounded).unwrap_or_default(),
                        description: field["description"]
                            .as_str()
                            .map(bounded)
                            .filter(|text| !text.is_empty()),
                        choices,
                        default: field["default"]
                            .as_str()
                            .map(bounded)
                            .filter(|text| !text.is_empty()),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    if fields.is_empty() {
        let choices = strings(arguments["choices"].as_array());
        if !choices.is_empty() {
            fields.push(QuestionField {
                choices,
                ..QuestionField::default()
            });
        }
    }
    Some(crate::telegram::Question {
        tool_call_id,
        text,
        fields,
    })
}

/// The plan a session wants approved, as the summary it gave.
pub(crate) fn pending_plan_summary(
    path: &Path,
    cycle_start: u64,
    cycle_end: u64,
) -> Option<String> {
    let (_, arguments) = pending_tool_call(path, cycle_start, cycle_end, "exit_plan_mode")?;
    arguments["summary"]
        .as_str()
        .map(bounded)
        .filter(|summary| !summary.is_empty())
}

/// Everything a Telegram notification says about a session, read from its event log.
///
/// Unlike ntfy's detailed mode this reads the last message for every kind, an error
/// included: the user chose full content so they can act on what the session said, and
/// a checkpoint report is plain text rather than a question.
pub(crate) fn telegram_content(
    kind: NotificationKind,
    session_title: &str,
    events_path: Option<&Path>,
    cycle_start: Option<u64>,
    cycle_end: Option<u64>,
) -> crate::telegram::Content {
    let window = match (events_path, cycle_start, cycle_end) {
        (Some(path), Some(start), Some(end)) if end > start => Some((path, start, end)),
        _ => None,
    };
    let body = window.and_then(|(path, start, end)| latest_assistant_message(path, start, end));
    let question = match (kind, window) {
        (NotificationKind::Question, Some((path, start, end))) => {
            pending_question(path, start, end)
        }
        _ => None,
    };
    let plan_summary = match (kind, window) {
        (NotificationKind::PlanApproval, Some((path, start, end))) => {
            pending_plan_summary(path, start, end)
        }
        _ => None,
    };
    crate::telegram::Content {
        kind,
        title: session_title.to_string(),
        body,
        question,
        plan_summary,
        // The caller knows the session's folder and what `gh` said about it.
        repository: None,
    }
}

pub(crate) fn sanitize_message(message: &str) -> String {
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
        assert_eq!(
            payload["tags"],
            serde_json::json!(["robot", "white_check_mark"])
        );
        assert!(!body.contains("project"));
        assert!(!body.contains("session_id"));
    }

    #[test]
    fn question_and_plan_approval_payloads_use_high_priority_distinct_tag_pairs() {
        let (url, request_receiver) = mock_server_responses(vec![200, 200]);
        for (kind, message, tag) in [
            (
                NotificationKind::Question,
                "Question waiting for your response",
                "question",
            ),
            (
                NotificationKind::PlanApproval,
                "Plan waiting for approval",
                "clipboard",
            ),
        ] {
            let request = NotificationRequest {
                config: NotificationConfig {
                    enabled: true,
                    server: url.clone(),
                    topic: "private_topic".to_string(),
                    ..NotificationConfig::default()
                },
                access_token: String::new(),
                verbose: false,
                session_title: "Agent".to_string(),
                kind,
                events_path: None,
                events_start: None,
                events_end: None,
            };

            publish_once(&request).unwrap();
            let request = request_receiver
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
            let payload = request_payload(&request);
            assert_eq!(payload["message"], message);
            assert_eq!(
                payload["priority"], 4,
                "{kind:?} should use ntfy high priority"
            );
            assert_eq!(payload["tags"], serde_json::json!(["robot", tag]));
            assert_eq!(payload["tags"].as_array().unwrap().len(), 2);
        }
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
        assert_eq!(payload["message"], "Copilot reported an error");
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
    fn verbose_context_is_available_for_attention_kinds_without_tool_arguments() {
        let temp = tempfile::tempdir().unwrap();
        let events = temp.path().join("events.jsonl");
        std::fs::write(
            &events,
            format!(
                "{}\n{}\n",
                serde_json::json!({
                    "type": "assistant.message",
                    "data": {"content": "Persisted assistant response"}
                }),
                serde_json::json!({
                    "type": "tool.execution_start",
                    "data": {
                        "arguments": {
                            "prompt": "private tool argument mentioning assistant.message"
                        }
                    }
                })
            ),
        )
        .unwrap();
        let events_end = std::fs::metadata(&events).unwrap().len();

        for (kind, status) in [
            (NotificationKind::Ready, "Ready for attention"),
            (
                NotificationKind::Question,
                "Question waiting for your response",
            ),
            (NotificationKind::PlanApproval, "Plan waiting for approval"),
        ] {
            let request = NotificationRequest {
                config: NotificationConfig::default(),
                access_token: String::new(),
                verbose: true,
                session_title: "Verbose".to_string(),
                kind,
                events_path: Some(events.clone()),
                events_start: Some(0),
                events_end: Some(events_end),
            };

            let message = notification_message(&request);
            assert_eq!(message, format!("{status}\n\nPersisted assistant response"));
            assert!(!message.contains("private tool argument"));
        }
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
    fn telegram_content_carries_the_open_question_with_its_choices_and_not_an_answered_one() {
        let temp = tempfile::tempdir().unwrap();
        let events = temp.path().join("events.jsonl");
        std::fs::write(
            &events,
            format!(
                "{}\n{}\n{}\n{}\n{}\n",
                serde_json::json!({"type": "assistant.message", "data": {"content": "I need a decision."}}),
                serde_json::json!({"type": "tool.execution_start", "data": {"toolName": "ask_user", "toolCallId": "old", "arguments": {"question": "Old question?", "choices": ["x"]}}}),
                serde_json::json!({"type": "tool.execution_complete", "data": {"toolCallId": "old", "result": {"content": "User responded: x"}}}),
                serde_json::json!({"type": "tool.execution_start", "data": {"toolName": "bash", "toolCallId": "cmd", "arguments": {"command": "rm -rf /tmp/x"}}}),
                serde_json::json!({"type": "tool.execution_start", "data": {"toolName": "ask_user", "toolCallId": "q2", "arguments": {"question": "Which <colour>?", "choices": ["Red", "Green", ""]}}}),
            ),
        )
        .unwrap();
        let end = std::fs::metadata(&events).unwrap().len();

        let content = telegram_content(
            NotificationKind::Question,
            "Palette",
            Some(&events),
            Some(0),
            Some(end),
        );

        assert_eq!(content.body.as_deref(), Some("I need a decision."));
        let question = content.question.expect("the open question");
        assert_eq!(question.tool_call_id, "q2");
        assert_eq!(question.text, "Which <colour>?");
        assert_eq!(question.choices().collect::<Vec<_>>(), vec!["Red", "Green"]);
        assert_eq!(question.fields.len(), 1);
        assert!(question.fields[0].title.is_empty());
        assert!(content.plan_summary.is_none());

        // A ready event never carries a question, even with one in the log.
        let ready = telegram_content(
            NotificationKind::Ready,
            "Palette",
            Some(&events),
            Some(0),
            Some(end),
        );
        assert!(ready.question.is_none());
        assert_eq!(ready.body.as_deref(), Some("I need a decision."));
    }

    #[test]
    fn telegram_content_reads_the_current_copilots_elicitation_shaped_question() {
        // Seen live with Copilot 1.0.104: the question is `message`, and the choices
        // live in the first property's `enum` of a requested schema. The first version
        // read `question`/`choices` only and sent "Question:" with nothing after it.
        let temp = tempfile::tempdir().unwrap();
        let events = temp.path().join("events.jsonl");
        std::fs::write(
            &events,
            format!(
                "{}\n",
                serde_json::json!({"type": "tool.execution_start", "data": {"toolName": "ask_user", "toolCallId": "call_1", "arguments": {"message": "Which colour do you prefer?", "requestedSchema": {"properties": {"colour": {"type": "string", "title": "Preferred colour", "enum": ["Red", "Green", "Blue"]}}}}}}),
            ),
        )
        .unwrap();
        let end = std::fs::metadata(&events).unwrap().len();

        let content = telegram_content(
            NotificationKind::Question,
            "Colours",
            Some(&events),
            Some(0),
            Some(end),
        );

        let question = content.question.expect("the open question");
        assert_eq!(question.tool_call_id, "call_1");
        assert_eq!(question.text, "Which colour do you prefer?");
        assert_eq!(question.fields.len(), 1);
        assert_eq!(question.fields[0].title, "Preferred colour");
        assert_eq!(question.fields[0].choices, vec!["Red", "Green", "Blue"]);
    }

    #[test]
    fn a_two_field_form_with_one_of_choices_and_a_free_text_default_is_read_whole() {
        // The record a real session wrote on 2026-10-24, trimmed: two fields, the
        // first with `oneOf` options whose `title` is what the user sees, the second
        // free text with a description and a default. The first version read the
        // first field's `enum` only and sent its title with nothing under it.
        let temp = tempfile::tempdir().unwrap();
        let events = temp.path().join("events.jsonl");
        std::fs::write(
            &events,
            format!(
                "{}\n",
                serde_json::json!({"type": "tool.execution_start", "data": {"toolName": "ask_user", "toolCallId": "call_yZ6", "arguments": {"message": "What should your approval mean for dataset splits?", "requestedSchema": {"properties": {"allocation_policy": {"type": "string", "title": "Building-level allocation policy", "oneOf": [{"const": "preserve", "title": "Preserve existing assignments (recommended)"}, {"const": "propose", "title": "Authorize a new allocation"}], "default": "preserve"}, "finish_time": {"type": "string", "title": "Weekend campaign finish time", "description": "Local time.", "default": "Monday, October 26, 2026 at 09:00 PDT"}}}}}}),
            ),
        )
        .unwrap();
        let end = std::fs::metadata(&events).unwrap().len();

        let question = telegram_content(
            NotificationKind::Question,
            "Floor plans",
            Some(&events),
            Some(0),
            Some(end),
        )
        .question
        .expect("the open question");

        assert_eq!(question.tool_call_id, "call_yZ6");
        assert_eq!(question.fields.len(), 2);
        assert_eq!(question.fields[0].title, "Building-level allocation policy");
        assert_eq!(
            question.fields[0].choices,
            vec![
                "Preserve existing assignments (recommended)",
                "Authorize a new allocation"
            ]
        );
        assert_eq!(question.fields[1].title, "Weekend campaign finish time");
        assert!(question.fields[1].choices.is_empty());
        assert_eq!(
            question.fields[1].description.as_deref(),
            Some("Local time.")
        );
        assert_eq!(
            question.fields[1].default.as_deref(),
            Some("Monday, October 26, 2026 at 09:00 PDT")
        );
        assert_eq!(question.choices().count(), 2);
    }

    #[test]
    fn telegram_content_reads_a_plan_summary_and_nothing_without_a_cycle() {
        let temp = tempfile::tempdir().unwrap();
        let events = temp.path().join("events.jsonl");
        std::fs::write(
            &events,
            format!(
                "{}\n",
                serde_json::json!({"type": "tool.execution_start", "data": {"toolName": "exit_plan_mode", "toolCallId": "p1", "arguments": {"summary": "- do this\n- then that"}}}),
            ),
        )
        .unwrap();
        let end = std::fs::metadata(&events).unwrap().len();

        let plan = telegram_content(
            NotificationKind::PlanApproval,
            "P",
            Some(&events),
            Some(0),
            Some(end),
        );
        assert_eq!(plan.plan_summary.as_deref(), Some("- do this\n- then that"));
        assert!(plan.body.is_none());

        let none = telegram_content(
            NotificationKind::PlanApproval,
            "P",
            Some(&events),
            Some(end),
            Some(end),
        );
        assert!(none.plan_summary.is_none() && none.body.is_none());
        assert_eq!(none.title, "P");
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
