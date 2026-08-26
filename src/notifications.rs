#![cfg_attr(test, allow(dead_code))]

use crate::config::{normalize_ntfy_server, NotificationConfig};
use serde::Serialize;
use std::sync::mpsc;
use std::time::Duration;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const RETRY_DELAY: Duration = Duration::from_millis(250);
const ATTEMPTS: usize = 2;
const MAX_TITLE_CHARS: usize = 120;

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
    pub session_title: String,
    pub kind: NotificationKind,
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
    let url = format!("{server}/{}", request.config.topic.trim());
    let title = notification_title(&request.session_title);
    let payload = NtfyPayload {
        message: request.kind.message(),
        title: &title,
        priority: request.kind.priority(),
        tags: request.kind.tags(),
    };
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        .build()
        .into();
    agent
        .post(&url)
        .header(
            "User-Agent",
            concat!("copilot-session-tui/", env!("CARGO_PKG_VERSION")),
        )
        .send_json(&payload)
        .map(|_| ())
        .map_err(sanitize_ureq_error)
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
            session_title: "Plan review".to_string(),
            kind: NotificationKind::Ready,
        };

        publish_once(&request).unwrap();
        let request = request_receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap();

        assert!(request.starts_with("POST /private_topic HTTP/1.1"));
        let body = request.split("\r\n\r\n").nth(1).unwrap();
        let payload: serde_json::Value = serde_json::from_str(body).unwrap();
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
            session_title: "Agent".to_string(),
            kind: NotificationKind::Error,
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
                    session_title: title.to_string(),
                    kind: NotificationKind::Ready,
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
            session_title: "Retry".to_string(),
            kind: NotificationKind::Ready,
        };

        publish_with_retry(&request).unwrap();

        request_receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        request_receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
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
