//! Commands Copilot left running in the background, read out of a session's event log.
//!
//! A session that started a long command with `mode: async` goes idle: the turn ends,
//! the terminal's progress clears, and nothing on a tab says a two-day run is still
//! going. The log does say so. Copilot records the command being put in the background
//! under a shell id, and later records that shell finishing, being stopped, or being
//! read to its end. This follows those records and reports the starts and the ends.
//!
//! It only reports. Whether a command counts as work is the user's call (a dev server
//! is left running on purpose and is not something to wait for), so nothing here marks
//! a session busy.
//!
//! The one piece of a record that is kept is the command's `description`, the short
//! label Copilot itself shows for it. The command text and its output are not.

use serde::Deserialize;
use std::collections::{HashMap, HashSet};

use super::lifecycle::LifecycleEvent;

const TOOL_START: &[u8] = b"tool.execution_start";
const TOOL_COMPLETE: &[u8] = b"tool.execution_complete";
const NOTIFICATION: &[u8] = b"system.notification";

/// Shell calls remembered between their start and completion records, for the
/// description. They complete within the same turn, so more than a handful in flight
/// means records were missed, and the oldest knowledge is not worth keeping.
const MAX_CALLS_IN_FLIGHT: usize = 64;

/// What the result of a shell call reads when the command was left running.
const STARTED: &[u8] = br#""content":"<command started in "#;
/// How a result begins whenever it speaks of a shell that is still going: a command
/// that outlived the wait it was given ("is still running after 180 seconds"), one the
/// user sent to the background by hand ("moved to background by the user"), and a
/// later look at either that found it not done. Every form of it means the same thing
/// here, so only the id after it is read.
///
/// It closes the result and does not always open it: whatever the command printed
/// before its wait ran out comes first. So it is looked for at the start of the result
/// or of a line of it, and has to go on in one of the ways Copilot words it, which
/// keeps printed text that merely mentions a shell from passing for the notice.
const STILL_RUNNING: &[u8] = b"<command with shellId: ";
const STILL_RUNNING_WORDINGS: [&[u8]; 3] = [
    b" is still running",
    b" moved to background",
    b" is running in the background",
];
/// What the result of stopping a shell reads, for one that was running or already gone.
const STOPPED: &[u8] = br#""content":"<command with id: "#;

#[derive(Default)]
pub(super) struct BackgroundTracker {
    /// Descriptions of async shell calls whose completion has not been read yet.
    descriptions: HashMap<String, String>,
    /// Shell ids reported as started and not yet as ended.
    open: HashSet<String>,
}

impl BackgroundTracker {
    /// Forget everything, for a log that was replaced under the reader.
    ///
    /// Returns whether anything was being followed, so the caller knows to say so.
    pub(super) fn clear(&mut self) -> bool {
        self.descriptions.clear();
        let had_open = !self.open.is_empty();
        self.open.clear();
        had_open
    }

    /// Read one record, or the leading part of an oversized one.
    ///
    /// Written against the bytes first and parsed only when a record is one of the few
    /// that matter: the log runs to six figures of records, and nearly all are not.
    pub(super) fn observe(
        &mut self,
        line: &[u8],
        event_type: Option<&[u8]>,
    ) -> Option<LifecycleEvent> {
        match event_type? {
            TOOL_START => {
                self.remember_description(line);
                None
            }
            TOOL_COMPLETE => self.completed(line),
            NOTIFICATION => self.notified(line),
            // The shells belong to one run of the CLI. A log that starts over, or says
            // the CLI went away, describes none that are still ours to follow.
            b"session.start" | b"session.resume" | b"session.shutdown" => {
                self.clear().then_some(LifecycleEvent::BackgroundCleared)
            }
            _ => None,
        }
    }

    /// Keep the description of a shell call until its result says how it went.
    ///
    /// Every shell call, not only the ones asked to run in the background: a plain
    /// command that outlives its wait is moved there too, and which will is not known
    /// when it starts. Read from the bytes, since this is every command a session runs.
    fn remember_description(&mut self, line: &[u8]) {
        if !SHELL_TOOLS.iter().any(|tool| contains(line, tool)) {
            return;
        }
        let Some(call) = tool_call_id(line) else {
            return;
        };
        // The command text comes first and may itself hold this key, but quoted inside
        // a JSON string its quotes are escaped, so the first bare match is the real one.
        // An oversized record is cut before it; the command is then labelled by its
        // shell id.
        let Some(description) = after(line, br#","description":"#)
            .and_then(json_string)
            .filter(|text| !text.trim().is_empty())
        else {
            return;
        };
        if self.descriptions.len() >= MAX_CALLS_IN_FLIGHT {
            self.descriptions.clear();
        }
        self.descriptions.insert(call.to_string(), description);
    }

    fn completed(&mut self, line: &[u8]) -> Option<LifecycleEvent> {
        let description = tool_call_id(line).and_then(|call| self.descriptions.remove(call));
        if let Some(rest) = after(line, STARTED) {
            let detached = rest.starts_with(b"detached background with shellId: ");
            let id = between(rest, b"background with shellId: ", b">")?;
            // A shell id is the model's to choose and can be used twice. The second
            // start replaces the first on the receiving side, so it is reported again.
            self.open.insert(id.to_string());
            return Some(LifecycleEvent::BackgroundStarted {
                shell_id: id.to_string(),
                description: description.unwrap_or_else(|| id.to_string()),
                detached,
            });
        }
        if let Some((id, rest)) = still_running(line) {
            // Already followed: this is the model looking in on it, not a start.
            if !self.open.insert(id.to_string()) {
                return None;
            }
            let said = &rest[..rest.len().min(80)];
            return Some(LifecycleEvent::BackgroundStarted {
                shell_id: id.to_string(),
                description: description.unwrap_or_else(|| id.to_string()),
                detached: contains(said, b" in detached background "),
            });
        }
        if self.open.is_empty() {
            return None;
        }
        if let Some(rest) = after(line, STOPPED) {
            let end = rest.iter().position(|byte| *byte == b'>')?;
            let text = std::str::from_utf8(&rest[..end]).ok()?;
            let id = text
                .strip_suffix(" had already stopped")
                .or_else(|| text.strip_suffix(" stopped"))?;
            return self.ended(id, None);
        }
        // Reading a shell that has finished ends its output with this. The id is one we
        // are following, so the search is for exactly that text and not for a pattern.
        if contains(line, b" completed with exit code ") {
            let finished = self.open.iter().find_map(|id| {
                let marker = format!("<shellId: {id} completed with exit code ");
                let rest = after(line, marker.as_bytes())?;
                let end = rest.iter().position(|byte| *byte == b'>')?;
                let code = std::str::from_utf8(&rest[..end]).ok()?.parse::<i32>().ok();
                Some((id.clone(), code))
            });
            if let Some((id, code)) = finished {
                return self.ended(&id, code);
            }
        }
        None
    }

    fn notified(&mut self, line: &[u8]) -> Option<LifecycleEvent> {
        if self.open.is_empty()
            || !(contains(line, br#""shell_completed""#)
                || contains(line, br#""shell_detached_completed""#))
        {
            return None;
        }
        let record = serde_json::from_slice::<NotificationRecord>(line).ok()?;
        let kind = record.data.kind?;
        if !matches!(
            kind.kind_type.as_deref(),
            Some("shell_completed" | "shell_detached_completed")
        ) {
            return None;
        }
        self.ended(&kind.shell_id?, kind.exit_code)
    }

    /// An end counts only for a shell that was seen starting: the log is joined
    /// partway through, and ends for shells from before that are not news.
    fn ended(&mut self, id: &str, exit_code: Option<i32>) -> Option<LifecycleEvent> {
        self.open
            .remove(id)
            .then(|| LifecycleEvent::BackgroundEnded {
                shell_id: id.to_string(),
                exit_code,
            })
    }
}

/// The shell a result says is still going, and what follows its id, if it says so.
fn still_running(line: &[u8]) -> Option<(&str, &[u8])> {
    // The last one: the notice ends the result, and anything like it earlier is output.
    let at = line
        .windows(STILL_RUNNING.len())
        .rposition(|window| window == STILL_RUNNING)?;
    let before = &line[..at];
    // A result carries its text twice, as `content` and again as `detailedContent`,
    // and the last notice is in whichever comes second.
    if !(before.ends_with(br#"ontent":""#) || before.ends_with(br"\n")) {
        return None;
    }
    let rest = &line[at + STILL_RUNNING.len()..];
    let id = between(rest, b"", b" ")?;
    let rest = &rest[id.len()..];
    STILL_RUNNING_WORDINGS
        .iter()
        .any(|wording| rest.starts_with(wording))
        .then_some((id, rest))
}

/// The tools that run a command in a shell, as their start records name them.
const SHELL_TOOLS: [&[u8]; 3] = [
    br#""toolName":"powershell""#,
    br#""toolName":"bash""#,
    br#""toolName":"local_shell""#,
];

/// The JSON string starting at `bytes`, decoded. `None` if it is not one, or is cut.
fn json_string(bytes: &[u8]) -> Option<String> {
    if bytes.first() != Some(&b'"') {
        return None;
    }
    let mut end = 1;
    while end < bytes.len() {
        match bytes[end] {
            b'\\' => end += 2,
            b'"' => return serde_json::from_slice(&bytes[..=end]).ok(),
            _ => end += 1,
        }
    }
    None
}

#[derive(Deserialize)]
struct NotificationRecord {
    data: NotificationData,
}

#[derive(Deserialize)]
struct NotificationData {
    kind: Option<NotificationKind>,
}

#[derive(Deserialize)]
struct NotificationKind {
    #[serde(rename = "type")]
    kind_type: Option<String>,
    #[serde(rename = "shellId")]
    shell_id: Option<String>,
    #[serde(rename = "exitCode")]
    exit_code: Option<i32>,
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    after(haystack, needle).is_some()
}

/// The bytes following the first `needle`.
fn after<'a>(haystack: &'a [u8], needle: &[u8]) -> Option<&'a [u8]> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|start| &haystack[start + needle.len()..])
}

/// The text between `open` and the next `close`. Shell ids are plain words, so an id
/// with a JSON escape in it is not one of ours and is passed over.
fn between<'a>(haystack: &'a [u8], open: &[u8], close: &[u8]) -> Option<&'a str> {
    let rest = if open.is_empty() {
        haystack
    } else {
        after(haystack, open)?
    };
    let end = rest
        .windows(close.len())
        .position(|window| window == close)?;
    let text = std::str::from_utf8(&rest[..end]).ok()?;
    (!text.is_empty() && !text.contains(['\\', '"'])).then_some(text)
}

fn tool_call_id(line: &[u8]) -> Option<&str> {
    between(line, br#""toolCallId":""#, b"\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(tracker: &mut BackgroundTracker, line: &str) -> Option<LifecycleEvent> {
        let event_type = line
            .strip_prefix(r#"{"type":""#)
            .and_then(|rest| rest.split('"').next());
        tracker.observe(line.as_bytes(), event_type.map(str::as_bytes))
    }

    // The shape of Copilot's records, with invented text. `description` really does
    // come after `command`, which is why an oversized record loses it.
    fn start(call: &str, description: &str, shell: &str, detach: bool) -> String {
        format!(
            r#"{{"type":"tool.execution_start","data":{{"toolCallId":"{call}","toolName":"powershell","arguments":{{"command":"python train.py","description":"{description}","shellId":"{shell}","mode":"async","detach":{detach}}},"turnId":"1"}},"id":"a","timestamp":"2026-10-29T05:44:03.000Z"}}"#
        )
    }

    fn started(call: &str, shell: &str, detached: bool) -> String {
        let how = if detached {
            "detached background"
        } else {
            "background"
        };
        format!(
            r#"{{"type":"tool.execution_complete","data":{{"toolCallId":"{call}","success":true,"result":{{"content":"<command started in {how} with shellId: {shell}>"}}}},"id":"b"}}"#
        )
    }

    fn notification(kind: &str, shell: &str, exit: Option<i32>) -> String {
        let exit = exit
            .map(|code| format!(r#","exitCode":{code}"#))
            .unwrap_or_default();
        format!(
            r#"{{"type":"system.notification","data":{{"content":"<system_notification>\nShell command \"Train\" (shellId: {shell}) has completed.\n</system_notification>","kind":{{"type":"{kind}","shellId":"{shell}"{exit},"description":"Train"}}}},"id":"c"}}"#
        )
    }

    fn result(call: &str, content: &str) -> String {
        format!(
            r#"{{"type":"tool.execution_complete","data":{{"toolCallId":"{call}","success":true,"result":{{"content":"{content}"}}}},"id":"d"}}"#
        )
    }

    #[test]
    fn a_command_left_running_is_reported_with_copilots_own_label_for_it() {
        let mut tracker = BackgroundTracker::default();

        assert_eq!(
            feed(
                &mut tracker,
                &start("call-1", "Train the model", "train-1", true)
            ),
            None
        );
        assert_eq!(
            feed(&mut tracker, &started("call-1", "train-1", true)),
            Some(LifecycleEvent::BackgroundStarted {
                shell_id: "train-1".into(),
                description: "Train the model".into(),
                detached: true,
            })
        );

        assert_eq!(
            feed(
                &mut tracker,
                &start("call-2", "Watch the build", "7", false)
            ),
            None
        );
        assert_eq!(
            feed(&mut tracker, &started("call-2", "7", false)),
            Some(LifecycleEvent::BackgroundStarted {
                shell_id: "7".into(),
                description: "Watch the build".into(),
                detached: false,
            })
        );
    }

    #[test]
    fn a_command_whose_start_record_was_too_long_to_read_is_still_found_under_its_shell_id() {
        let mut tracker = BackgroundTracker::default();
        // What is left of a start record cut at the size limit: no description, no end.
        let cut = r#"{"type":"tool.execution_start","data":{"toolCallId":"call-1","toolName":"powershell","arguments":{"command":"a very long script"#;
        assert_eq!(feed(&mut tracker, cut), None);

        assert_eq!(
            feed(&mut tracker, &started("call-1", "campaign", true)),
            Some(LifecycleEvent::BackgroundStarted {
                shell_id: "campaign".into(),
                description: "campaign".into(),
                detached: true,
            })
        );
    }

    #[test]
    fn a_plain_command_that_outlives_its_wait_is_reported_because_it_carries_on_in_the_background()
    {
        let mut tracker = BackgroundTracker::default();
        // Not asked to run in the background: there is no `mode`, and the description
        // has an escaped quote in it, as descriptions do. The command text holds a
        // decoy of the key, which is only ever a quoted string inside a string.
        let plain = r#"{"type":"tool.execution_start","data":{"toolCallId":"call-1","toolName":"powershell","arguments":{"command":"echo \",\\\"description\\\":\\\"decoy\\\"\"","description":"Run the \"slow\" audit","initial_wait":180},"turnId":"1"}}"#;
        assert_eq!(feed(&mut tracker, plain), None);

        let outlived = result(
            "call-1",
            "<command with shellId: audit-1 is still running after 180 seconds. You will be automatically notified when it completes.>",
        );
        assert_eq!(
            feed(&mut tracker, &outlived),
            Some(LifecycleEvent::BackgroundStarted {
                shell_id: "audit-1".into(),
                description: r#"Run the "slow" audit"#.into(),
                detached: false,
            })
        );
        // Looked in on later and still going: not a second start.
        assert_eq!(
            feed(
                &mut tracker,
                &result(
                    "read-1",
                    "<command with shellId: audit-1 is still running after 30 seconds.>"
                )
            ),
            None
        );
        assert_eq!(
            feed(
                &mut tracker,
                &notification("shell_completed", "audit-1", Some(0))
            ),
            Some(LifecycleEvent::BackgroundEnded {
                shell_id: "audit-1".into(),
                exit_code: Some(0),
            })
        );
    }

    #[test]
    fn the_notice_is_found_after_whatever_the_command_printed_before_its_wait_ran_out() {
        let mut tracker = BackgroundTracker::default();

        assert_eq!(
            feed(
                &mut tracker,
                &result(
                    "call-1",
                    r"wrote catalog (1768 bytes)\n<command with shellId: verify is still running after 120 seconds. The command is still running.>"
                )
            ),
            Some(LifecycleEvent::BackgroundStarted {
                shell_id: "verify".into(),
                description: "verify".into(),
                detached: false,
            })
        );

        // Output that only mentions a shell, mid-line or with other words after the
        // id, is not the notice.
        for printed in [
            r"see <command with shellId: fake is still running after 5 seconds>",
            r"log\n<command with shellId: fake was mentioned here>",
        ] {
            assert_eq!(
                feed(&mut tracker, &result("cat", printed)),
                None,
                "{printed}"
            );
        }
    }

    #[test]
    fn a_command_the_user_sent_to_the_background_by_hand_is_reported() {
        let mut tracker = BackgroundTracker::default();

        assert_eq!(
            feed(
                &mut tracker,
                &result(
                    "call-1",
                    "<command with shellId: 149 moved to background by the user. You will be automatically notified when it completes.>"
                )
            ),
            Some(LifecycleEvent::BackgroundStarted {
                shell_id: "149".into(),
                description: "149".into(),
                detached: false,
            })
        );
    }

    #[test]
    fn a_shell_from_before_the_log_was_joined_is_picked_up_when_the_model_looks_in_on_it() {
        let mut tracker = BackgroundTracker::default();

        assert_eq!(
            feed(
                &mut tracker,
                &result(
                    "read-1",
                    "<command with shellId: campaign is still running in detached background after 30s. You will be automatically notified when it completes.>"
                )
            ),
            Some(LifecycleEvent::BackgroundStarted {
                shell_id: "campaign".into(),
                description: "campaign".into(),
                detached: true,
            })
        );
    }

    #[test]
    fn every_way_copilot_records_a_command_ending_is_reported_once() {
        let ended = |shell: &str, exit_code| {
            Some(LifecycleEvent::BackgroundEnded {
                shell_id: shell.into(),
                exit_code,
            })
        };
        let mut tracker = BackgroundTracker::default();
        for shell in ["a", "b", "c", "d", "e"] {
            feed(&mut tracker, &started("call", shell, false));
        }

        assert_eq!(
            feed(&mut tracker, &notification("shell_completed", "a", Some(3))),
            ended("a", Some(3))
        );
        assert_eq!(
            feed(
                &mut tracker,
                &notification("shell_detached_completed", "b", None)
            ),
            ended("b", None)
        );
        assert_eq!(
            feed(
                &mut tracker,
                &result("stop-1", "<command with id: c stopped>")
            ),
            ended("c", None)
        );
        assert_eq!(
            feed(
                &mut tracker,
                &result("stop-2", "<command with id: d had already stopped>")
            ),
            ended("d", None)
        );
        assert_eq!(
            feed(
                &mut tracker,
                &result(
                    "read-1",
                    r"last line of output\n<shellId: e completed with exit code 0>"
                )
            ),
            ended("e", Some(0))
        );

        // The notification that follows a read to the end must not end it twice.
        assert_eq!(
            feed(&mut tracker, &notification("shell_completed", "e", Some(0))),
            None
        );
    }

    #[test]
    fn an_ending_for_a_shell_never_seen_starting_is_not_reported() {
        let mut tracker = BackgroundTracker::default();

        assert_eq!(
            feed(
                &mut tracker,
                &notification("shell_completed", "old", Some(0))
            ),
            None
        );
        assert_eq!(
            feed(
                &mut tracker,
                &result("stop", "<command with id: old stopped>")
            ),
            None
        );

        // Nor is one shell's end taken for another's whose id it merely starts with.
        feed(&mut tracker, &started("call", "train-10", false));
        assert_eq!(
            feed(
                &mut tracker,
                &result("read", "<shellId: train-1 completed with exit code 0>")
            ),
            None
        );
    }

    #[test]
    fn a_shell_still_running_when_read_is_neither_a_start_nor_an_end() {
        let mut tracker = BackgroundTracker::default();
        feed(&mut tracker, &started("call", "watch", true));

        assert_eq!(
            feed(
                &mut tracker,
                &result(
                    "read",
                    "<command with shellId: watch is still running in detached background after 30s. You will be automatically notified when it completes.>"
                )
            ),
            None
        );
        assert_eq!(
            feed(
                &mut tracker,
                &notification("shell_detached_completed", "watch", None)
            ),
            Some(LifecycleEvent::BackgroundEnded {
                shell_id: "watch".into(),
                exit_code: None,
            })
        );
    }

    #[test]
    fn the_cli_restarting_or_shutting_down_clears_what_was_running_because_its_shells_died_with_it()
    {
        for record in ["session.shutdown", "session.resume", "session.start"] {
            let mut tracker = BackgroundTracker::default();
            let line = format!(r#"{{"type":"{record}","data":{{}}}}"#);
            assert_eq!(feed(&mut tracker, &line), None, "nothing to clear yet");

            feed(&mut tracker, &started("call", "watch", false));
            assert_eq!(
                feed(&mut tracker, &line),
                Some(LifecycleEvent::BackgroundCleared),
                "{record}"
            );
            assert_eq!(
                feed(
                    &mut tracker,
                    &notification("shell_completed", "watch", Some(0))
                ),
                None
            );
        }
    }

    /// Replay a real event log and count what the tracker makes of it:
    /// `CST_REPLAY_LOG=<events.jsonl> cargo test replay_a_real_log -- --ignored --nocapture`.
    /// Prints counts only, never the log's text.
    #[test]
    #[ignore = "needs a real event log named by CST_REPLAY_LOG"]
    fn replay_a_real_log() {
        let Some(path) = std::env::var_os("CST_REPLAY_LOG") else {
            return;
        };
        let text = std::fs::read(path).expect("the log can be read");
        let mut tracker = BackgroundTracker::default();
        let (mut starts, mut ends, mut cleared, mut unmatched, mut never) = (0, 0, 0, 0, 0);
        let mut ever_started = HashSet::new();
        for line in text.split(|byte| *byte == b'\n') {
            let event_type = line
                .strip_prefix(br#"{"type":""#)
                .and_then(|rest| rest.split(|byte| *byte == b'"').next());
            let was_open = tracker.open.len();
            match tracker.observe(line, event_type) {
                Some(LifecycleEvent::BackgroundStarted { shell_id, .. }) => {
                    starts += 1;
                    ever_started.insert(shell_id);
                }
                Some(LifecycleEvent::BackgroundEnded { .. }) => ends += 1,
                Some(LifecycleEvent::BackgroundCleared) => cleared += was_open,
                _ => {
                    if event_type == Some(NOTIFICATION)
                        && (contains(line, br#""shell_completed""#)
                            || contains(line, br#""shell_detached_completed""#))
                    {
                        unmatched += 1;
                        let id = serde_json::from_slice::<NotificationRecord>(line)
                            .ok()
                            .and_then(|record| record.data.kind?.shell_id);
                        if id.is_some_and(|id| !ever_started.contains(&id)) {
                            never += 1;
                        }
                    }
                }
            }
        }
        println!(
            "starts {starts}, ends {ends}, dropped at a restart {cleared}, still open {}, \
             completions with nothing open under that id {unmatched} \
             (of those, for an id never seen starting: {never})",
            tracker.open.len()
        );
    }

    #[test]
    fn output_that_quotes_a_background_notice_does_not_invent_a_command() {
        let mut tracker = BackgroundTracker::default();
        // A file being printed that happens to contain the notice, mid-output.
        assert_eq!(
            feed(
                &mut tracker,
                &result(
                    "cat",
                    r"line one\n<command started in background with shellId: fake>"
                )
            ),
            None
        );
    }
}
