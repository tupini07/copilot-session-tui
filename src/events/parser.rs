use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct SessionDetails {
    pub edited_files: Vec<String>,
    pub last_user_message: Option<String>,
    pub turn_count: usize,
    pub tool_call_count: usize,
    /// Bytes of `events.jsonl` already folded into the fields above. Only whole
    /// lines are counted, so a session still being written resumes cleanly.
    pub parsed_len: u64,
}

/// The two event types the session list summarizes.
///
/// Everything else — notably `tool.execution_complete`, which carries whole file
/// contents and command output — is skipped without being parsed.
const USER_MESSAGE: &str = "user.message";
const TOOL_START: &str = "tool.execution_start";

/// Fold events appended since `details.parsed_len` into `details`.
///
/// `events.jsonl` is append-only and routinely reaches hundreds of megabytes, so
/// re-reading it on every selection change is what made the session list feel
/// sluggish. Resuming from the previous offset makes revisiting a session nearly
/// free while keeping a live session's counts current for the cost of its new bytes.
pub fn parse_events_into(path: &Path, details: &mut SessionDetails) -> Result<()> {
    let file_len = std::fs::metadata(path)
        .with_context(|| format!("Failed to stat {}", path.display()))?
        .len();

    // A shorter file is a different file — truncated, replaced, or rotated — so the
    // accumulated counts no longer describe it.
    if file_len < details.parsed_len {
        *details = SessionDetails::default();
    }
    if file_len == details.parsed_len {
        return Ok(());
    }

    let mut file =
        File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    if details.parsed_len > 0 {
        file.seek(SeekFrom::Start(details.parsed_len))
            .with_context(|| format!("Failed to seek {}", path.display()))?;
    }

    let mut seen: BTreeSet<String> = details.edited_files.iter().cloned().collect();
    let mut reader = BufReader::with_capacity(1 << 20, file);
    // Reused across lines: allocating a String per line cost more than the file read.
    let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
    let mut consumed = details.parsed_len;

    loop {
        let read = match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(read) => read,
            Err(_) => break,
        };

        // A line with no terminator is still being written. Leave it for the next
        // pass instead of counting a half-written event.
        if !buf.ends_with(b"\n") {
            break;
        }
        consumed += read as u64;

        let line = trim_line_ending(&buf);
        if !line.is_empty() && is_relevant(line) {
            if let Ok(event) = serde_json::from_slice::<Value>(line) {
                record_event(details, &event, &mut seen);
            }
        }
        buf.clear();
    }

    details.parsed_len = consumed;
    Ok(())
}

fn record_event(details: &mut SessionDetails, event: &Value, seen: &mut BTreeSet<String>) {
    match event["type"].as_str().unwrap_or("") {
        USER_MESSAGE => {
            details.turn_count += 1;
            if let Some(content) = event["data"]["content"].as_str() {
                // Truncate long messages for preview. Messages routinely contain
                // emoji, so this must not slice raw bytes.
                let preview = crate::text::truncate_to_width(content, 200);
                details.last_user_message = Some(preview);
            }
        }
        TOOL_START => {
            details.tool_call_count += 1;
            let tool_name = event["data"]["toolName"].as_str().unwrap_or("");
            if matches!(tool_name, "edit" | "create") {
                if let Some(path) = event["data"]["arguments"]["path"].as_str() {
                    let normalized = path.replace("\\\\", "\\");
                    if seen.insert(normalized.clone()) {
                        details.edited_files.push(normalized);
                    }
                }
            }
        }
        _ => {}
    }
}

/// Whether a raw line needs full JSON parsing.
///
/// Every event the CLI writes leads with its `type`, so the interesting ones can be
/// picked out from a few bytes instead of building a `Value` for payloads that
/// individually exceed a megabyte. A line that does not match the expected layout is
/// treated as relevant, so an unrecognized format degrades to full parsing rather
/// than silently dropping events.
fn is_relevant(line: &[u8]) -> bool {
    match leading_event_type(line) {
        Some(kind) => kind == USER_MESSAGE || kind == TOOL_START,
        None => true,
    }
}

/// Read a leading `"type"` field's value without parsing the rest of the line.
fn leading_event_type(line: &[u8]) -> Option<&str> {
    const PREFIX: &[u8] = br#"{"type":""#;
    let rest = line.strip_prefix(PREFIX)?;
    // Type names are short; bound the scan so a malformed line cannot drag this
    // across a multi-megabyte payload.
    let end = rest.iter().take(64).position(|byte| *byte == b'"')?;
    std::str::from_utf8(&rest[..end]).ok()
}

fn trim_line_ending(buf: &[u8]) -> &[u8] {
    let line = buf.strip_suffix(b"\n").unwrap_or(buf);
    line.strip_suffix(b"\r").unwrap_or(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Summarize a whole file from scratch, as the first look at a session does.
    fn parse_events(path: &Path) -> Result<SessionDetails> {
        let mut details = SessionDetails::default();
        parse_events_into(path, &mut details)?;
        Ok(details)
    }

    fn user_message(content: &str) -> String {
        format!(
            r#"{{"type":"user.message","data":{{"content":{}}}}}"#,
            serde_json::to_string(content).unwrap()
        )
    }

    fn tool_start(name: &str, path: &str) -> String {
        format!(
            r#"{{"type":"tool.execution_start","data":{{"toolName":"{name}","arguments":{{"path":{}}}}}}}"#,
            serde_json::to_string(path).unwrap()
        )
    }

    fn write_events(lines: &[String]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let mut file = File::create(&path).unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
        (dir, path)
    }

    fn append(path: &Path, lines: &[String]) {
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
    }

    #[test]
    fn counts_turns_tool_calls_and_edited_files() {
        let (_dir, path) = write_events(&[
            user_message("first"),
            tool_start("edit", "src/main.rs"),
            tool_start("view", "src/other.rs"),
            tool_start("create", "src/new.rs"),
            user_message("second"),
        ]);

        let details = parse_events(&path).unwrap();

        assert_eq!(details.turn_count, 2);
        assert_eq!(details.tool_call_count, 3);
        assert_eq!(details.edited_files, vec!["src/main.rs", "src/new.rs"]);
        assert_eq!(details.last_user_message.as_deref(), Some("second"));
    }

    #[test]
    fn skips_large_unrelated_events() {
        // A completion payload big enough that parsing it would dominate the run.
        let huge = "x".repeat(2 * 1024 * 1024);
        let noise = format!(
            r#"{{"type":"tool.execution_complete","data":{{"result":{}}}}}"#,
            serde_json::to_string(&huge).unwrap()
        );
        let (_dir, path) =
            write_events(&[user_message("hello"), noise, tool_start("edit", "a.rs")]);

        let details = parse_events(&path).unwrap();

        assert_eq!(details.turn_count, 1);
        assert_eq!(details.tool_call_count, 1);
        assert_eq!(details.edited_files, vec!["a.rs"]);
    }

    #[test]
    fn falls_back_to_full_parse_when_type_is_not_leading() {
        let reordered = r#"{"data":{"content":"hi"},"type":"user.message"}"#.to_string();
        let (_dir, path) = write_events(&[reordered]);

        let details = parse_events(&path).unwrap();

        assert_eq!(details.turn_count, 1);
        assert_eq!(details.last_user_message.as_deref(), Some("hi"));
    }

    #[test]
    fn incremental_parse_matches_a_full_parse() {
        let first = vec![user_message("one"), tool_start("edit", "a.rs")];
        let second = vec![
            tool_start("edit", "a.rs"),
            tool_start("create", "b.rs"),
            user_message("two"),
        ];
        let (_dir, path) = write_events(&first);

        let mut incremental = SessionDetails::default();
        parse_events_into(&path, &mut incremental).unwrap();
        assert_eq!(incremental.turn_count, 1);

        append(&path, &second);
        parse_events_into(&path, &mut incremental).unwrap();

        let full = parse_events(&path).unwrap();
        assert_eq!(incremental, full);
        assert_eq!(full.turn_count, 2);
        assert_eq!(full.tool_call_count, 3);
        // The repeated edit is recorded once even across separate passes.
        assert_eq!(full.edited_files, vec!["a.rs", "b.rs"]);
    }

    #[test]
    fn unchanged_file_is_not_reparsed() {
        let (_dir, path) = write_events(&[user_message("one")]);

        let mut details = SessionDetails::default();
        parse_events_into(&path, &mut details).unwrap();
        let after_first = details.clone();

        parse_events_into(&path, &mut details).unwrap();

        assert_eq!(details, after_first);
        assert_eq!(details.turn_count, 1);
    }

    #[test]
    fn partial_trailing_line_is_left_for_the_next_pass() {
        let (_dir, path) = write_events(&[user_message("one")]);
        // A half-written event, as a live session would leave behind.
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        write!(file, r#"{{"type":"user.message","data":{{"cont"#).unwrap();
        drop(file);

        let mut details = SessionDetails::default();
        parse_events_into(&path, &mut details).unwrap();
        assert_eq!(details.turn_count, 1);

        // Completing the line counts it exactly once.
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(file, r#"ent":"two"}}}}"#).unwrap();
        drop(file);

        parse_events_into(&path, &mut details).unwrap();
        assert_eq!(details.turn_count, 2);
        assert_eq!(details.last_user_message.as_deref(), Some("two"));
    }

    #[test]
    fn truncated_file_restarts_from_scratch() {
        let (_dir, path) = write_events(&[user_message("one"), user_message("two")]);
        let mut details = SessionDetails::default();
        parse_events_into(&path, &mut details).unwrap();
        assert_eq!(details.turn_count, 2);

        let (_dir2, replacement) = write_events(&[user_message("only")]);
        std::fs::copy(&replacement, &path).unwrap();
        parse_events_into(&path, &mut details).unwrap();

        assert_eq!(details.turn_count, 1);
        assert_eq!(details.last_user_message.as_deref(), Some("only"));
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let (_dir, path) = write_events(&[
            user_message("one"),
            "not json at all".to_string(),
            String::new(),
            tool_start("edit", "a.rs"),
        ]);

        let details = parse_events(&path).unwrap();

        assert_eq!(details.turn_count, 1);
        assert_eq!(details.tool_call_count, 1);
    }
}
