//! A stand-in for the Telegram Bot API, served from inside the driver.
//!
//! Enough of the API for CST to pair, send notifications, make topics and read
//! replies, over plain HTTP on a loopback port. The script feeds messages in with the
//! `telegram_*` commands and checks what CST sent against the outbox kept here.
//!
//! No JSON library: the crate has none, and the handful of fields CST sends are read
//! with a small scanner rather than pulling a dependency in for them.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// The token the paired config carries. Its shape passes CST's check.
pub const TOKEN: &str = "123456:STUBTOKENSTUBTOKENSTUBTOKENSTUB";
pub const BOT_ID: i64 = 999;
pub const BOT_USERNAME: &str = "cst_e2e_bot";
/// The paired user and chats, as the config written by `fake_telegram` says.
pub const USER_ID: i64 = 77;
pub const PRIVATE_CHAT_ID: i64 = 1000;
pub const FORUM_CHAT_ID: i64 = -1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Private,
    Forum,
}

impl Mode {
    pub fn chat_id(self) -> i64 {
        match self {
            Self::Private => PRIVATE_CHAT_ID,
            Self::Forum => FORUM_CHAT_ID,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Forum => "forum",
        }
    }
}

/// One message CST sent: what a reply would point at.
#[derive(Debug, Clone)]
pub struct Sent {
    pub message_id: i64,
    pub thread_id: Option<i64>,
}

#[derive(Debug, Default)]
struct State {
    /// Updates not yet handed out, or handed out but not yet confirmed by an offset.
    updates: VecDeque<(i64, String)>,
    next_update_id: i64,
    next_message_id: i64,
    next_thread_id: i64,
    sent: Vec<Sent>,
    /// The last message that was not a reply to the user: a notification, which is what
    /// `telegram_say` answers. Hints and refusals are always replies.
    last_notification: Option<Sent>,
    topics: Vec<(i64, String)>,
    /// Everything that happened, one line each, for `telegram_wait` and the dump.
    outbox: Vec<String>,
    /// Whether `getChatMember` says the bot may manage topics.
    admin: bool,
    /// Every request path seen, for debugging a failing script.
    requests: Vec<String>,
}

pub struct TelegramStub {
    pub port: u16,
    pub mode: Mode,
    state: Arc<(Mutex<State>, Condvar)>,
}

impl TelegramStub {
    pub fn start(mode: Mode) -> Result<Self, String> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
        let port = listener.local_addr().map_err(|error| error.to_string())?.port();
        let state = Arc::new((
            Mutex::new(State {
                next_update_id: 100,
                next_message_id: 1,
                next_thread_id: 500,
                admin: true,
                ..State::default()
            }),
            Condvar::new(),
        ));
        let shared = Arc::clone(&state);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || serve(stream, &shared, mode));
            }
        });
        Ok(Self { port, mode, state })
    }

    pub fn api_base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Put a text message from `from` into the chat, as a reply or inside a topic.
    pub fn say(&self, from: i64, text: &str, reply_to: Option<i64>, thread_id: Option<i64>) {
        let (lock, wake) = &*self.state;
        let mut state = lock.lock().unwrap();
        let update_id = state.next_update_id;
        state.next_update_id += 1;
        let message_id = state.next_message_id;
        state.next_message_id += 1;
        let chat = match self.mode {
            Mode::Private => format!(r#"{{"id":{PRIVATE_CHAT_ID},"type":"private"}}"#),
            Mode::Forum => {
                format!(r#"{{"id":{FORUM_CHAT_ID},"type":"supergroup","is_forum":true}}"#)
            }
        };
        let mut message = format!(
            r#"{{"message_id":{message_id},"chat":{chat},"from":{{"id":{from},"is_bot":false}},"text":{}"#,
            json_string(text)
        );
        if let Some(thread_id) = thread_id {
            message.push_str(&format!(
                r#","message_thread_id":{thread_id},"is_topic_message":true"#
            ));
        }
        if let Some(reply_to) = reply_to {
            message.push_str(&format!(r#","reply_to_message":{{"message_id":{reply_to}}}"#));
            if thread_id.is_none() {
                // Telegram sets the thread id on any reply; CST must not read it as a topic.
                message.push_str(&format!(r#","message_thread_id":{reply_to}"#));
            }
        }
        message.push('}');
        state.updates.push_back((
            update_id,
            format!(r#"{{"update_id":{update_id},"message":{message}}}"#),
        ));
        state
            .outbox
            .push(format!("incoming from {from}: {text}"));
        wake.notify_all();
    }

    /// The last notification CST sent, to reply to, and the topic it went into.
    pub fn last_notification(&self) -> Option<Sent> {
        self.state.0.lock().unwrap().last_notification.clone()
    }

    pub fn set_admin(&self, admin: bool) {
        self.state.0.lock().unwrap().admin = admin;
    }

    pub fn outbox(&self) -> Vec<String> {
        self.state.0.lock().unwrap().outbox.clone()
    }

    pub fn requests(&self) -> Vec<String> {
        self.state.0.lock().unwrap().requests.clone()
    }
}

fn serve(mut stream: TcpStream, shared: &Arc<(Mutex<State>, Condvar)>, mode: Mode) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let Some((path, body)) = read_request(&mut stream) else {
        return;
    };
    let method = path.rsplit('/').next().unwrap_or("").to_string();
    {
        let (lock, _) = &**shared;
        lock.lock().unwrap().requests.push(format!("{method} {body}"));
    }
    let token_ok = path.starts_with(&format!("/bot{TOKEN}/"));
    let (status, reply) = if !token_ok {
        (401, r#"{"ok":false,"error_code":401,"description":"Unauthorized"}"#.to_string())
    } else {
        handle(&method, &body, shared, mode)
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
        reply.len()
    );
    let _ = stream.flush();
}

fn handle(method: &str, body: &str, shared: &Arc<(Mutex<State>, Condvar)>, mode: Mode) -> (u16, String) {
    let (lock, wake) = &**shared;
    let ok = |result: String| (200, format!(r#"{{"ok":true,"result":{result}}}"#));
    match method {
        "getMe" => ok(format!(
            r#"{{"id":{BOT_ID},"is_bot":true,"username":"{BOT_USERNAME}"}}"#
        )),
        "getUpdates" => {
            let offset = number(body, "offset");
            let timeout = number(body, "timeout").unwrap_or(0).max(0) as u64;
            let deadline = Instant::now() + Duration::from_secs(timeout);
            let mut state = lock.lock().unwrap();
            // Confirm what the offset says has been seen.
            if let Some(offset) = offset {
                state.updates.retain(|(id, _)| *id >= offset);
            }
            loop {
                if !state.updates.is_empty() {
                    break;
                }
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                let (next, _) = wake.wait_timeout(state, deadline - now).unwrap();
                state = next;
            }
            let updates: Vec<String> = state.updates.iter().map(|(_, json)| json.clone()).collect();
            ok(format!("[{}]", updates.join(",")))
        }
        "sendMessage" => {
            let chat_id = number(body, "chat_id").unwrap_or(0);
            if chat_id != mode.chat_id() {
                return (
                    400,
                    r#"{"ok":false,"error_code":400,"description":"Bad Request: chat not found"}"#.to_string(),
                );
            }
            let text = string(body, "text").unwrap_or_default();
            let thread_id = number(body, "message_thread_id");
            let is_reply = body.contains("\"reply_parameters\"");
            let mut state = lock.lock().unwrap();
            if mode == Mode::Forum {
                if let Some(thread_id) = thread_id {
                    if !state.topics.iter().any(|(id, _)| *id == thread_id) {
                        return (
                            400,
                            r#"{"ok":false,"error_code":400,"description":"Bad Request: message thread not found"}"#.to_string(),
                        );
                    }
                }
            }
            let message_id = state.next_message_id;
            state.next_message_id += 1;
            let topic = thread_id
                .and_then(|id| state.topics.iter().find(|(t, _)| *t == id).map(|(_, name)| name.clone()));
            state.outbox.push(match topic {
                Some(name) => format!("message in {name}: {text}"),
                None => format!("message: {text}"),
            });
            let sent = Sent {
                message_id,
                thread_id,
            };
            if !is_reply {
                state.last_notification = Some(sent.clone());
            }
            state.sent.push(sent);
            ok(format!(r#"{{"message_id":{message_id}}}"#))
        }
        "createForumTopic" => {
            let mut state = lock.lock().unwrap();
            if mode != Mode::Forum || !state.admin {
                return (
                    400,
                    r#"{"ok":false,"error_code":400,"description":"Bad Request: not enough rights to create a topic"}"#.to_string(),
                );
            }
            let name = string(body, "name").unwrap_or_default();
            let thread_id = state.next_thread_id;
            state.next_thread_id += 1;
            state.topics.push((thread_id, name.clone()));
            state.outbox.push(format!("topic: {name}"));
            ok(format!(
                r#"{{"message_thread_id":{thread_id},"name":{}}}"#,
                json_string(&name)
            ))
        }
        "editForumTopic" => {
            let thread_id = number(body, "message_thread_id").unwrap_or(0);
            let name = string(body, "name").unwrap_or_default();
            let mut state = lock.lock().unwrap();
            if let Some(topic) = state.topics.iter_mut().find(|(id, _)| *id == thread_id) {
                topic.1 = name.clone();
            }
            state.outbox.push(format!("renamed topic: {name}"));
            ok("true".to_string())
        }
        "closeForumTopic" => {
            let thread_id = number(body, "message_thread_id").unwrap_or(0);
            let mut state = lock.lock().unwrap();
            let name = state
                .topics
                .iter()
                .find(|(id, _)| *id == thread_id)
                .map(|(_, name)| name.clone())
                .unwrap_or_else(|| thread_id.to_string());
            state.outbox.push(format!("closed topic: {name}"));
            ok("true".to_string())
        }
        "getChatMember" => {
            let state = lock.lock().unwrap();
            ok(if state.admin {
                r#"{"status":"administrator","can_manage_topics":true}"#.to_string()
            } else {
                r#"{"status":"member"}"#.to_string()
            })
        }
        "setMessageReaction" => {
            let message_id = number(body, "message_id").unwrap_or(0);
            let mut state = lock.lock().unwrap();
            state.outbox.push(format!("reaction on {message_id}"));
            ok("true".to_string())
        }
        other => (
            404,
            format!(r#"{{"ok":false,"error_code":404,"description":"Not Found: {other}"}}"#),
        ),
    }
}

fn read_request(stream: &mut TcpStream) -> Option<(String, String)> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    let header_end;
    loop {
        let read = stream.read(&mut buffer).ok()?;
        if read == 0 {
            return None;
        }
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            header_end = end + 4;
            break;
        }
    }
    let headers = String::from_utf8_lossy(&bytes[..header_end]).to_string();
    let path = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("")
        .to_string();
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    while bytes.len() < header_end + content_length {
        let read = stream.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    let body = String::from_utf8_lossy(&bytes[header_end..]).to_string();
    Some((path, body))
}

/// The number after `"key":` in a JSON object, if it is there.
fn number(json: &str, key: &str) -> Option<i64> {
    let needle = format!("\"{key}\":");
    let at = json.find(&needle)? + needle.len();
    let rest = json[at..].trim_start();
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '-'))
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// The string after `"key":`, with JSON escapes undone.
fn string(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":");
    let at = json.find(&needle)? + needle.len();
    let rest = json[at..].trim_start();
    let mut chars = rest.strip_prefix('"')?.chars();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    let code = u32::from_str_radix(&hex, 16).ok()?;
                    // A surrogate pair, as serde_json writes anything outside the BMP.
                    if (0xD800..0xDC00).contains(&code) {
                        let low: String = chars.by_ref().skip(2).take(4).collect();
                        let low = u32::from_str_radix(&low, 16).ok()?;
                        let combined = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                        out.push(char::from_u32(combined)?);
                    } else {
                        out.push(char::from_u32(code)?);
                    }
                }
                other => out.push(other),
            },
            other => out.push(other),
        }
    }
    None
}

/// A JSON string literal, quotes included.
pub fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scanner_reads_numbers_and_escaped_strings_the_way_serde_writes_them() {
        let body = r#"{"chat_id":-1000,"message_thread_id":7,"text":"a \"quoted\" line\nnext 🚀","parse_mode":"HTML"}"#;
        assert_eq!(number(body, "chat_id"), Some(-1000));
        assert_eq!(number(body, "message_thread_id"), Some(7));
        assert_eq!(number(body, "offset"), None);
        assert_eq!(
            string(body, "text").as_deref(),
            Some("a \"quoted\" line\nnext 🚀")
        );
        assert_eq!(json_string("x\"y\n"), r#""x\"y\n""#);
    }
}
