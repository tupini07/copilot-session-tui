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
/// The topic icons the stub offers, as (emoji, sticker id). The stub Copilot, asked to
/// choose one, takes the first it is offered that is not already in use, so a script
/// knows which to expect.
pub const TOPIC_ICONS: &[(&str, &str)] = &[("🚀", "5300"), ("🐛", "5301"), ("📦", "5302")];

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
    /// Inline buttons under it, as (label, callback data), for `telegram_tap`.
    pub buttons: Vec<(String, String)>,
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
    /// Where pictures are written.
    photos: std::path::PathBuf,
}

pub struct TelegramStub {
    pub port: u16,
    pub mode: Mode,
    state: Arc<(Mutex<State>, Condvar)>,
}

impl TelegramStub {
    /// `photos` is where every picture CST sends is written, as `photo-<id>.png`, so a
    /// script's author can look at it.
    pub fn start(mode: Mode, photos: std::path::PathBuf) -> Result<Self, String> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
        let port = listener.local_addr().map_err(|error| error.to_string())?.port();
        let state = Arc::new((
            Mutex::new(State {
                next_update_id: 100,
                next_message_id: 1,
                next_thread_id: 500,
                admin: true,
                photos,
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
    /// Tap the button with this label under the latest message that has one, as the
    /// paired user would. Fails if no message carries such a button.
    pub fn tap(&self, label: &str) -> Result<(), String> {
        let (lock, wake) = &*self.state;
        let mut state = lock.lock().unwrap();
        let Some((message_id, thread_id, data)) = state.sent.iter().rev().find_map(|sent| {
            sent.buttons
                .iter()
                .find(|(text, _)| text == label)
                .map(|(_, data)| (sent.message_id, sent.thread_id, data.clone()))
        }) else {
            return Err(format!("no message CST sent has a button labelled {label:?}"));
        };
        let update_id = state.next_update_id;
        state.next_update_id += 1;
        let chat = match self.mode {
            Mode::Private => format!(r#"{{"id":{PRIVATE_CHAT_ID},"type":"private"}}"#),
            Mode::Forum => {
                format!(r#"{{"id":{FORUM_CHAT_ID},"type":"supergroup","is_forum":true}}"#)
            }
        };
        let mut message = format!(r#"{{"message_id":{message_id},"chat":{chat}"#);
        if let Some(thread_id) = thread_id {
            message.push_str(&format!(
                r#","message_thread_id":{thread_id},"is_topic_message":true"#
            ));
        }
        message.push('}');
        state.updates.push_back((
            update_id,
            format!(
                r#"{{"update_id":{update_id},"callback_query":{{"id":"tap{update_id}","from":{{"id":{USER_ID},"is_bot":false}},"message":{message},"data":{}}}}}"#,
                json_string(&data)
            ),
        ));
        state.outbox.push(format!("tap: {label}"));
        wake.notify_all();
        Ok(())
    }

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
    let Some((path, content_type, bytes)) = read_request(&mut stream) else {
        return;
    };
    let method = path.rsplit('/').next().unwrap_or("").to_string();
    let multipart = content_type.contains("multipart/form-data");
    // A picture's bytes are not text; the request log keeps the fields and the size.
    let body = if multipart {
        format!("<multipart, {} bytes>", bytes.len())
    } else {
        String::from_utf8_lossy(&bytes).to_string()
    };
    {
        let (lock, _) = &**shared;
        lock.lock().unwrap().requests.push(format!("{method} {body}"));
    }
    let token_ok = path.starts_with(&format!("/bot{TOKEN}/"));
    let (status, reply) = if !token_ok {
        (401, r#"{"ok":false,"error_code":401,"description":"Unauthorized"}"#.to_string())
    } else if multipart {
        handle_multipart(&method, &content_type, &bytes, shared, mode)
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
        "getForumTopicIconStickers" => ok(format!(
            "[{}]",
            TOPIC_ICONS
                .iter()
                .map(|(emoji, id)| format!(
                    r#"{{"emoji":{},"custom_emoji_id":"{id}"}}"#,
                    json_string(emoji)
                ))
                .collect::<Vec<_>>()
                .join(",")
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
            let buttons = buttons_in(body);
            for (label, _) in &buttons {
                state.outbox.push(format!("button on {message_id}: {label}"));
            }
            let sent = Sent {
                message_id,
                thread_id,
                buttons,
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
            // The icon is named by the sticker id CST was offered; the outbox shows
            // the emoji it stands for, which is what a script can read.
            let icon = string(body, "icon_custom_emoji_id").and_then(|id| {
                TOPIC_ICONS
                    .iter()
                    .find(|(_, offered)| *offered == id)
                    .map(|(emoji, _)| *emoji)
            });
            state.outbox.push(match icon {
                Some(emoji) => format!("topic: {name} icon={emoji}"),
                None => format!("topic: {name}"),
            });
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
        "answerCallbackQuery" => {
            let text = string(body, "text").unwrap_or_default();
            let mut state = lock.lock().unwrap();
            state.outbox.push(format!("answered tap: {text}"));
            ok("true".to_string())
        }
        "editMessageText" => {
            let message_id = number(body, "message_id").unwrap_or(0);
            let text = string(body, "text").unwrap_or_default();
            let mut state = lock.lock().unwrap();
            state.outbox.push(format!("edited {message_id}: {text}"));
            ok(format!(r#"{{"message_id":{message_id}}}"#))
        }
        "setMessageReaction" => {
            let message_id = number(body, "message_id").unwrap_or(0);
            let mut state = lock.lock().unwrap();
            state.outbox.push(format!("reaction on {message_id}"));
            ok("true".to_string())
        }
        "setMyCommands" => {
            let names = all_strings(body, "command");
            let mut state = lock.lock().unwrap();
            state.outbox.push(format!("commands: {}", names.join(" ")));
            ok("true".to_string())
        }
        other => (
            404,
            format!(r#"{{"ok":false,"error_code":404,"description":"Not Found: {other}"}}"#),
        ),
    }
}

/// `sendPhoto`: the one method that arrives as a form with a file in it. The parts are
/// split on the boundary the header names; each has its own headers, then its bytes.
fn handle_multipart(
    method: &str,
    content_type: &str,
    bytes: &[u8],
    shared: &Arc<(Mutex<State>, Condvar)>,
    mode: Mode,
) -> (u16, String) {
    let (lock, _) = &**shared;
    let Some(boundary) = content_type
        .split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix("boundary="))
    else {
        return (400, r#"{"ok":false,"error_code":400,"description":"Bad Request: no boundary"}"#.to_string());
    };
    let marker = format!("--{boundary}").into_bytes();
    let mut fields: Vec<(String, Vec<u8>)> = Vec::new();
    let mut at = 0;
    while let Some(start) = find(&bytes[at..], &marker).map(|i| at + i + marker.len()) {
        if bytes[start..].starts_with(b"--") {
            break;
        }
        let Some(header_end) = find(&bytes[start..], b"\r\n\r\n").map(|i| start + i) else {
            break;
        };
        let headers = String::from_utf8_lossy(&bytes[start..header_end]).to_string();
        let name = headers
            .split(';')
            .map(str::trim)
            .find_map(|part| part.strip_prefix("name=\""))
            .and_then(|rest| rest.split('"').next())
            .unwrap_or("")
            .to_string();
        let content_start = header_end + 4;
        let Some(next) = find(&bytes[content_start..], &marker).map(|i| content_start + i) else {
            break;
        };
        // The CRLF before the next boundary belongs to the boundary, not the content.
        let content_end = next.saturating_sub(2).max(content_start);
        fields.push((name, bytes[content_start..content_end].to_vec()));
        at = next;
    }
    let text_of = |wanted: &str| -> Option<String> {
        fields
            .iter()
            .find(|(name, _)| name == wanted)
            .map(|(_, value)| String::from_utf8_lossy(value).to_string())
    };
    if method != "sendPhoto" {
        return (404, format!(r#"{{"ok":false,"error_code":404,"description":"Not Found: {method}"}}"#));
    }
    let chat_id: i64 = text_of("chat_id").and_then(|c| c.parse().ok()).unwrap_or(0);
    if chat_id != mode.chat_id() {
        return (400, r#"{"ok":false,"error_code":400,"description":"Bad Request: chat not found"}"#.to_string());
    }
    let caption = text_of("caption").unwrap_or_default();
    let thread_id: Option<i64> = text_of("message_thread_id").and_then(|t| t.parse().ok());
    let is_reply = text_of("reply_parameters").is_some();
    let photo = fields
        .iter()
        .find(|(name, _)| name == "photo")
        .map(|(_, value)| value.clone())
        .unwrap_or_default();
    let size = png_size(&photo);
    let mut state = lock.lock().unwrap();
    let message_id = state.next_message_id;
    state.next_message_id += 1;
    let path = state.photos.join(format!("photo-{message_id}.png"));
    let _ = std::fs::write(&path, &photo);
    let topic = thread_id
        .and_then(|id| state.topics.iter().find(|(t, _)| *t == id).map(|(_, name)| name.clone()));
    let dims = match size {
        Some((w, h)) => format!("{w}x{h}"),
        None => "not a PNG".to_string(),
    };
    state.outbox.push(match topic {
        Some(name) => format!("photo {dims} in {name}: {caption}"),
        None => format!("photo {dims}: {caption}"),
    });
    let sent = Sent {
        message_id,
        thread_id,
        buttons: Vec::new(),
    };
    if !is_reply {
        state.last_notification = Some(sent.clone());
    }
    state.sent.push(sent);
    (200, format!(r#"{{"ok":true,"result":{{"message_id":{message_id}}}}}"#))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// The width and height a PNG declares.
fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[1..4] != b"PNG" {
        return None;
    }
    let read = |at: usize| u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    Some((read(16), read(20)))
}

/// The request's path, its content type, and its body bytes.
fn read_request(stream: &mut TcpStream) -> Option<(String, String, Vec<u8>)> {
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
    let content_type = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-type")
                .then(|| value.trim().to_string())
        })
        .unwrap_or_default();
    Some((path, content_type, bytes[header_end..].to_vec()))
}

/// The number after `"key":` in a JSON object, if it is there.
/// The inline buttons in a `sendMessage` body, as (label, callback data). Each button
/// is `{"text":…,"callback_data":…}`, so the label is the nearest `"text"` before its
/// data; the message's own `"text"` comes earlier and is never the nearest.
fn buttons_in(body: &str) -> Vec<(String, String)> {
    // CST writes its bodies pretty-printed, so a key is followed by a colon, maybe
    // whitespace, then the opening quote; `string` tolerates the same.
    fn value_after(json: &str, key_at: usize, key: &str) -> Option<(String, usize)> {
        let rest = json[key_at + key.len()..].trim_start().strip_prefix(':')?;
        let rest = rest.trim_start().strip_prefix('"')?;
        let len = rest.find('"')?;
        let start = json.len() - rest.len();
        Some((rest[..len].to_string(), start + len))
    }
    const DATA: &str = "\"callback_data\"";
    const TEXT: &str = "\"text\"";
    let mut buttons = Vec::new();
    let mut from = 0;
    while let Some(at) = body[from..].find(DATA) {
        let key_at = from + at;
        let Some((data, after)) = value_after(body, key_at, DATA) else {
            break;
        };
        let label = body[..key_at]
            .rfind(TEXT)
            .and_then(|text_at| value_after(body, text_at, TEXT))
            .map(|(label, _)| label)
            .unwrap_or_default();
        buttons.push((label, data));
        from = after;
    }
    buttons
}

fn number(json: &str, key: &str) -> Option<i64> {
    let needle = format!("\"{key}\":");
    let at = json.find(&needle)? + needle.len();
    let rest = json[at..].trim_start();
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '-'))
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// Every value of `key` in the body, in order: a list of objects sharing one key, such
/// as the command menu. The same pretty-printing tolerance as `string`.
fn all_strings(json: &str, key: &str) -> Vec<String> {
    let needle = format!("\"{key}\"");
    let mut values = Vec::new();
    let mut from = 0;
    while let Some(at) = json[from..].find(&needle) {
        let after_key = from + at + needle.len();
        if let Some(value) = string(&json[from + at..], key) {
            values.push(value);
        }
        from = after_key;
    }
    values
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
    fn buttons_are_read_from_a_send_body_with_each_label_taken_from_its_own_button() {
        // The body as CST really sends it — pretty-printed, a space after each colon —
        // and the message's own "text" comes first and must not be taken for a label.
        let pretty = "{\n  \"chat_id\": 1000,\n  \"text\": \"Queued: a reply\",\n  \"reply_markup\": {\n    \"inline_keyboard\": [\n      [\n        {\n          \"text\": \"Cancel\",\n          \"callback_data\": \"c:5\"\n        }\n      ],\n      [\n        {\n          \"text\": \"Cancel 2\",\n          \"callback_data\": \"q:6\"\n        }\n      ]\n    ]\n  }\n}";
        let expected = vec![
            ("Cancel".to_string(), "c:5".to_string()),
            ("Cancel 2".to_string(), "q:6".to_string()),
        ];
        assert_eq!(buttons_in(pretty), expected);
        // The compact form, should a body ever arrive that way.
        let compact = r#"{"chat_id":1000,"text":"Queued: a reply","reply_markup":{"inline_keyboard":[[{"text":"Cancel","callback_data":"c:5"}],[{"text":"Cancel 2","callback_data":"q:6"}]]}}"#;
        assert_eq!(buttons_in(compact), expected);
        assert!(buttons_in("{\n  \"chat_id\": 1000,\n  \"text\": \"plain\"\n}").is_empty());
    }

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
