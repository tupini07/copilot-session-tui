//! The Copilot CLI's server mode, as the stub plays it.
//!
//! CST asks Copilot side questions through the Copilot SDK, which starts the CLI as
//! `copilot --server --stdio` and speaks JSON-RPC to it over pipes, framed with
//! `Content-Length` headers. This is the stub's end of that conversation: just enough
//! of the protocol for CST to open a session, ask one thing, and delete the session,
//! with the shapes copied from a real exchange so the SDK's typed decoding is content.
//!
//! The SDK's `sendAndWait` does not return when the response arrives: it waits until
//! the `session.event` whose id the response names as `completionEventId` has been
//! seen. The real CLI sends `session.idle` with that id; so does this.
//!
//! Answers are canned, so scripts can predict them. A prompt that asks for an emoji
//! gets the first offered one not already taken, as the `-p` stub did; anything else
//! gets a line naming the session the question was about, when it was about one.
//! Stage directions in a prompt steer the rest: `e2e:slow` delays the answer,
//! `e2e:fail` refuses it, `e2e:table` answers with prose, a table and more prose.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;

/// What the stub remembers of a session: the session it was copied from, if any.
#[derive(Default)]
struct Sessions {
    forked_from: HashMap<String, Option<String>>,
    next: u32,
}

pub fn main() {
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    let mut sessions = Sessions::default();
    while let Some(message) = read_message(&mut input) {
        let id = number(&message, "id");
        let method = string(&message, "method").unwrap_or_default();
        let (result, notifications) = match handle(&method, &message, &mut sessions) {
            Ok((result, notifications)) => (format!(r#""result":{result}"#), notifications),
            Err((code, text)) => (
                format!(r#""error":{{"code":{code},"message":{}}}"#, json_string(&text)),
                Vec::new(),
            ),
        };
        if let Some(id) = id {
            write_message(&mut output, &format!(r#"{{"jsonrpc":"2.0","id":{id},{result}}}"#));
        }
        for notification in notifications {
            write_message(&mut output, &notification);
        }
        if method == "runtime.shutdown" {
            return;
        }
    }
}

type Outcome = Result<(String, Vec<String>), (i64, String)>;

fn handle(method: &str, message: &str, sessions: &mut Sessions) -> Outcome {
    let session_id = string(message, "sessionId");
    match method {
        "connect" => Ok((
            r#"{"ok":true,"protocolVersion":3,"version":"stub","taskKinds":["agent"]}"#.to_string(),
            Vec::new(),
        )),
        "ping" => Ok((r#"{"message":null,"protocolVersion":3}"#.to_string(), Vec::new())),
        "status.get" => Ok((r#"{"version":"stub","protocolVersion":3}"#.to_string(), Vec::new())),
        "session.create" => {
            let id = session_id.unwrap_or_else(|| sessions.fresh_id());
            sessions.forked_from.insert(id.clone(), None);
            make_folder(&id, string(message, "name").as_deref());
            Ok((session_result(&id), Vec::new()))
        }
        "session.resume" => {
            let id = session_id.ok_or((-32602, "sessionId is required".to_string()))?;
            Ok((session_result(&id), Vec::new()))
        }
        "sessions.fork" => {
            let source = session_id.ok_or((-32602, "sessionId is required".to_string()))?;
            let known = sessions.forked_from.contains_key(&source)
                || folder(&source).is_some_and(|dir| dir.join("workspace.yaml").is_file());
            if !known {
                return Err((
                    -32603,
                    format!(
                        "Request sessions.fork failed with message: Cannot fork session {source}: \
                         no persisted or in-memory events found"
                    ),
                ));
            }
            let id = sessions.fresh_id();
            sessions.forked_from.insert(id.clone(), Some(source));
            make_folder(&id, string(message, "name").as_deref());
            let name = string(message, "name").map(|name| json_string(&name)).unwrap_or_else(|| "null".to_string());
            Ok((format!(r#"{{"sessionId":"{id}","name":{name}}}"#), Vec::new()))
        }
        "session.skills.reload" => Ok((r#"{"warnings":[],"errors":[]}"#.to_string(), Vec::new())),
        "session.sendAndWait" => {
            let id = session_id.ok_or((-32602, "sessionId is required".to_string()))?;
            let prompt = string(message, "prompt").unwrap_or_default();
            if prompt.contains("e2e:fail") {
                return Err((-32603, "Request session.sendAndWait failed with message: model unavailable".to_string()));
            }
            if prompt.contains("e2e:slow") {
                std::thread::sleep(std::time::Duration::from_secs(3));
            }
            let about = sessions.forked_from.get(&id).cloned().flatten();
            let answer = if prompt.contains("Choose the one emoji") {
                super::stub::choose_emoji(&prompt)
            } else if prompt.contains("e2e:table") {
                "Here is the **state**:\n\n| Step | Status |\n|---|---|\n| build | done |\n| tests | running |\n\nThat is all for now.".to_string()
            } else {
                let first_line = prompt.lines().last().unwrap_or("").trim();
                match about {
                    Some(source) => format!("Fake Copilot aside about {source}: {first_line}"),
                    None => format!("Fake Copilot aside: {first_line}"),
                }
            };
            sessions.next += 1;
            let n = sessions.next;
            let completion = format!("idle-{n}");
            let result = format!(
                r#"{{"messageId":"m{n}","aborted":false,"response":{{"type":"assistant.message","data":{{"messageId":"a{n}","originatingMessageId":"m{n}","model":"stub","content":{},"toolRequests":[]}},"id":"e{n}","timestamp":"2026-01-01T00:00:00.000Z"}},"completionEventId":"{completion}"}}"#,
                json_string(&answer)
            );
            let idle = format!(
                r#"{{"jsonrpc":"2.0","method":"session.event","params":{{"sessionId":"{id}","event":{{"type":"session.idle","data":{{"mode":"interactive"}},"ephemeral":true,"id":"{completion}","timestamp":"2026-01-01T00:00:00.000Z"}}}}}}"#
            );
            Ok((result, vec![idle]))
        }
        "session.send" => {
            sessions.next += 1;
            Ok((format!(r#"{{"messageId":"m{}"}}"#, sessions.next), Vec::new()))
        }
        "session.abort" => Ok(("{}".to_string(), Vec::new())),
        "session.detach" => Ok((r#"{"success":true}"#.to_string(), Vec::new())),
        "session.delete" => {
            if let Some(id) = session_id {
                sessions.forked_from.remove(&id);
                if let Some(dir) = folder(&id) {
                    let _ = std::fs::remove_dir_all(dir);
                }
            }
            Ok((r#"{"success":true}"#.to_string(), Vec::new()))
        }
        "runtime.shutdown" => Ok(("{}".to_string(), Vec::new())),
        other => Err((-32601, format!("Method not found: {other}"))),
    }
}

impl Sessions {
    fn fresh_id(&mut self) -> String {
        self.next += 1;
        format!("aside-{}", self.next)
    }
}

/// The shape a real `session.create` answers with, which the SDK decodes as typed.
fn session_result(id: &str) -> String {
    let workspace = folder(id).map(|dir| dir.display().to_string()).unwrap_or_default();
    format!(
        r#"{{"sessionId":"{id}","nativeSessionId":"{id}","startTime":"2026-01-01T00:00:00.000Z","isRemote":false,"mode":"interactive","workspacePath":{},"capabilities":{{"ui":{{"elicitation":false,"mcpApps":false,"canvases":false}},"extensions":false}},"modelState":{{}}}}"#,
        json_string(&workspace)
    )
}

/// Where the real CLI keeps this session: under `COPILOT_HOME`, which the driver sets.
fn folder(id: &str) -> Option<PathBuf> {
    let home = std::env::var_os("COPILOT_HOME")?;
    Some(PathBuf::from(home).join("session-state").join(id))
}

/// A session the real CLI would save, so a script can see it appear and disappear.
fn make_folder(id: &str, name: Option<&str>) {
    if let Some(dir) = folder(id) {
        let _ = std::fs::create_dir_all(&dir);
        let yaml = format!(
            "id: {id}\ncwd: .\nname: {}\ncreated_at: 2026-01-01T00:00:00Z\nupdated_at: 2026-01-01T00:00:00Z\n",
            name.unwrap_or("aside")
        );
        let _ = std::fs::write(dir.join("workspace.yaml"), yaml);
        let _ = std::fs::write(dir.join("events.jsonl"), "{\"type\":\"session.start\"}\n".repeat(4));
    }
}

fn read_message(input: &mut impl BufRead) -> Option<String> {
    let mut length: Option<usize> = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if length.is_some() {
                break;
            }
            continue;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            length = value.trim().parse().ok();
        }
    }
    let mut body = vec![0u8; length?];
    input.read_exact(&mut body).ok()?;
    Some(String::from_utf8_lossy(&body).into_owned())
}

fn write_message(output: &mut impl Write, json: &str) {
    let _ = write!(output, "Content-Length: {}\r\n\r\n{json}", json.len());
    let _ = output.flush();
}

/// A JSON string value for `key` at any depth, unescaped. Good enough for messages
/// the SDK builds, whose keys never collide across nesting in ways that matter here.
fn string(json: &str, key: &str) -> Option<String> {
    let marker = format!("\"{key}\":");
    let start = json.find(&marker)? + marker.len();
    let rest = json[start..].trim_start();
    let rest = rest.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'u' => {
                    let code: String = chars.by_ref().take(4).collect();
                    if let Some(ch) = u32::from_str_radix(&code, 16).ok().and_then(char::from_u32) {
                        out.push(ch);
                    }
                }
                other => out.push(other),
            },
            other => out.push(other),
        }
    }
    None
}

fn number(json: &str, key: &str) -> Option<i64> {
    let marker = format!("\"{key}\":");
    let start = json.find(&marker)? + marker.len();
    let digits: String = json[start..]
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    digits.parse().ok()
}

fn json_string(text: &str) -> String {
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
    fn a_question_in_a_copied_session_is_answered_about_its_source_and_the_copy_is_deleted() {
        // The whole point of the fork path: the answer must show which conversation it
        // came from, and nothing must be left behind.
        let mut sessions = Sessions::default();
        sessions.forked_from.insert("existing-app".to_string(), None);
        let (forked, _) = handle(
            "sessions.fork",
            r#"{"id":2,"method":"sessions.fork","params":{"sessionId":"existing-app","name":"CST aside"}}"#,
            &mut sessions,
        )
        .unwrap();
        let id = string(&forked, "sessionId").unwrap();
        let (answer, notifications) = handle(
            "session.sendAndWait",
            &format!(r#"{{"id":3,"method":"session.sendAndWait","params":{{"sessionId":"{id}","prompt":"what now?"}}}}"#),
            &mut sessions,
        )
        .unwrap();
        assert!(answer.contains("Fake Copilot aside about existing-app: what now?"), "{answer}");
        let completion = string(&answer, "completionEventId").unwrap();
        assert!(notifications[0].contains(&format!(r#""id":"{completion}""#)), "{}", notifications[0]);
        handle("session.delete", &format!(r#"{{"id":4,"method":"session.delete","params":{{"sessionId":"{id}"}}}}"#), &mut sessions).unwrap();
        assert!(!sessions.forked_from.contains_key(&id));
    }

    #[test]
    fn forking_a_session_nobody_has_heard_of_fails_the_way_the_real_cli_does() {
        let mut sessions = Sessions::default();
        let error = handle(
            "sessions.fork",
            r#"{"id":2,"method":"sessions.fork","params":{"sessionId":"ghost"}}"#,
            &mut sessions,
        )
        .unwrap_err();
        assert!(error.1.contains("no persisted or in-memory events found"), "{}", error.1);
    }

    #[test]
    fn an_emoji_question_is_answered_with_an_offered_emoji_as_the_old_stub_did() {
        let mut sessions = Sessions::default();
        let prompt = "Title: \\\"Deploy\\\"\\n\\nChoose the one emoji from this list that best stands for the title:\\n🚀 🐛 📦\\n\\nThese are already in use by other topics nearby; avoid them unless nothing else fits:\\n🚀\\n\\nAnswer with that one emoji and nothing else.";
        let (answer, _) = handle(
            "session.sendAndWait",
            &format!(r#"{{"id":3,"method":"session.sendAndWait","params":{{"sessionId":"s","prompt":"{prompt}"}}}}"#),
            &mut sessions,
        )
        .unwrap();
        assert!(answer.contains(r#""content":"🐛""#), "{answer}");
    }

    #[test]
    fn framing_round_trips_a_message_with_a_multibyte_body() {
        let mut buffer = Vec::new();
        write_message(&mut buffer, r#"{"a":"🚀"}"#);
        let mut reader = std::io::Cursor::new(buffer);
        assert_eq!(read_message(&mut reader).as_deref(), Some(r#"{"a":"🚀"}"#));
    }
}
