//! The end of a session's conversation, as text a model can read.
//!
//! A side question is normally asked in a copy of the whole conversation, which Copilot
//! makes by copying the session's event log. That log can run to gigabytes for a
//! session that has lived for weeks, and copying it takes minutes the phone would spend
//! looking at "Asking…". For such a session the question is asked in a fresh session
//! instead, with the last stretch of the conversation quoted into the prompt: the user
//! and assistant messages from the tail of the log, oldest first, each cut to a length
//! a model can take in. Less than the whole, but seconds rather than minutes, and the
//! recent part is what a question from the phone is usually about.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::Value;

/// How much of the log's tail is read. Messages are a small part of a log — tool calls
/// and their output are most of it — so this holds a good many turns.
pub const TAIL_BYTES: u64 = 512 * 1024;

/// Longest a single quoted message may be. Agents write long reports; the model needs
/// the gist of each, not every line.
pub const MESSAGE_CHARS: usize = 3_000;

/// The recent exchange from `events.jsonl`, or `None` when the log is unreadable or
/// holds no messages in its tail.
pub fn recent_exchange(events_path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(events_path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::with_capacity((len - start) as usize);
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    // A read that starts mid-file starts mid-line; the first line is a fragment.
    let text = if start > 0 {
        text.split_once('\n').map(|(_, rest)| rest).unwrap_or("")
    } else {
        &text
    };
    let exchange = exchange_from_lines(text);
    (!exchange.is_empty()).then_some(exchange)
}

/// The user and assistant messages in these log lines, oldest first, as
/// `User:` / `Copilot:` paragraphs. Pure, for the test.
pub fn exchange_from_lines(lines: &str) -> String {
    let mut out = String::new();
    for line in lines.lines() {
        if !line.contains("user.message") && !line.contains("assistant.message") {
            continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let speaker = match event["type"].as_str() {
            Some("user.message") => "User",
            Some("assistant.message") => "Copilot",
            _ => continue,
        };
        let Some(content) = event["data"]["content"].as_str() else {
            continue;
        };
        let content = crate::notifications::sanitize_message(content);
        if content.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(speaker);
        out.push_str(": ");
        out.push_str(&cut(&content, MESSAGE_CHARS));
    }
    out
}

fn cut(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let kept: String = text.chars().take(max_chars).collect();
    format!("{kept} […]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_exchange_keeps_what_was_said_in_order_and_leaves_tool_traffic_out() {
        // Tool calls are most of a log and none of a conversation; a model asked what
        // was going on should read the words, oldest first.
        let lines = concat!(
            r#"{"type":"user.message","data":{"content":"Fix the login test."}}"#,
            "\n",
            r#"{"type":"tool.execution_start","data":{"toolName":"bash","arguments":{"command":"cargo test"}}}"#,
            "\n",
            r#"{"type":"assistant.message","data":{"content":"It fails on CI only.\nLooking at the clock."}}"#,
            "\n",
            r#"{"type":"assistant.message","data":{"content":""}}"#,
            "\n",
            "not json at all\n",
        );

        let exchange = exchange_from_lines(lines);

        assert_eq!(
            exchange,
            "User: Fix the login test.\n\nCopilot: It fails on CI only.\nLooking at the clock."
        );
    }

    #[test]
    fn a_long_message_is_cut_with_a_mark_so_the_model_knows_it_was() {
        let long = "x".repeat(MESSAGE_CHARS + 10);
        let line = format!(r#"{{"type":"user.message","data":{{"content":"{long}"}}}}"#);

        let exchange = exchange_from_lines(&line);

        assert!(exchange.ends_with(" […]"));
        assert_eq!(
            exchange.chars().count(),
            "User: ".len() + MESSAGE_CHARS + " […]".chars().count()
        );
    }

    #[test]
    fn reading_a_tail_drops_the_line_it_starts_in_the_middle_of() {
        // The first line of a mid-file read is a fragment that would fail to parse at
        // best and parse as a different message at worst.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("events.jsonl");
        let mut content = String::new();
        for i in 0..8000 {
            content.push_str(&format!(
                r#"{{"type":"user.message","data":{{"content":"message number {i} with some padding to make it long enough"}}}}"#
            ));
            content.push('\n');
        }
        std::fs::write(&path, &content).unwrap();
        assert!(
            content.len() as u64 > TAIL_BYTES,
            "the test needs a file longer than the tail"
        );

        let exchange = recent_exchange(&path).unwrap();

        assert!(exchange.ends_with("message number 7999 with some padding to make it long enough"));
        assert!(
            !exchange.contains("message number 0 "),
            "the head is not read"
        );
        let first = exchange.lines().next().unwrap();
        assert!(
            first.starts_with("User: message number "),
            "a whole message, not a fragment: {first}"
        );
    }
}
