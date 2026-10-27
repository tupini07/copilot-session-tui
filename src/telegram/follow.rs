//! A live view of what a session is doing, as one Telegram message edited in place.
//!
//! A notification says a turn ended. While a turn runs, the phone sees nothing, and
//! the user sometimes wants to know what the agent is up to before deciding whether to
//! wait. `/follow` answers that: CST sends one message with the session's last words
//! and whatever tool is running, and for a few minutes keeps editing that message as
//! the log grows. Telegram lets a bot edit its own messages, so the chat does not fill
//! with copies.
//!
//! The view is the pane's screen, verbatim, in a monospace block (see `screen`): the
//! person asking wants to see what the terminal shows, a menu or a prompt included,
//! not a summary of it. A screen is only readable by the CST hosting the pane, so that
//! CST renders and edits, stamping the follow as hosted. The view built from the
//! event log — the session's last words and the tool running — is what the poller
//! falls back to for a session no CST has open, since the log is readable by any CST
//! and a takeover carries on with the same message.
//!
//! A follow expires on its own, because an edit every ten seconds forever is not a
//! fair use of Telegram, and the user who asked has usually looked by then. When it
//! ends — by time, by `/unfollow`, or because the session closed — the message says
//! so in a way that cannot be mistaken for a live one.
//!
//! Everything here is pure; the poller does the reading and sending.

use std::collections::BTreeSet;
use std::hash::{Hash, Hasher};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::Value;

use super::markdown::to_telegram_html;
use super::{escape_html, MESSAGE_LIMIT};

/// How long a follow runs when no length is given.
pub const DEFAULT_MINUTES: u32 = 5;
/// The most a follow may be asked to run.
pub const MAX_MINUTES: u32 = 30;
/// How often the message is edited at most. Six edits a minute is well under what
/// Telegram allows a bot in one chat, and nobody reads faster than that on a phone.
pub const EDIT_EVERY: Duration = Duration::from_secs(10);
/// How many follows may run at once, across sessions. Each is an edit every ten
/// seconds; three is eighteen a minute, under Telegram's ceiling for a chat.
pub const MAX_FOLLOWS: usize = 3;
/// How much of the end of the event log is read for a view. A long turn writes
/// thousands of records; the last few messages are within this.
pub const TAIL_BYTES: u64 = 512 * 1024;
/// How many of the session's latest messages the view shows at most.
const SHOWN_MESSAGES: usize = 3;
/// The longest a running tool's argument is shown.
const ARGUMENT_WIDTH: usize = 72;

/// A `/follow` or `/unfollow`, from a message that is one. Anything else — Copilot's
/// own slash commands included — is not CST's to intercept and is `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowCommand {
    Follow { minutes: u32 },
    Unfollow,
}

/// Read a command out of a message, if the message is one.
///
/// `/follow`, `/follow 10`, `/follow@cst_bot`, in any case. A bad or missing number
/// means the default; more than the cap means the cap, because asking for an hour and
/// getting half of it is friendlier than being refused.
pub fn parse_command(text: &str) -> Option<FollowCommand> {
    let mut words = text.split_whitespace();
    let command = words.next()?;
    let command = command.split('@').next()?.to_ascii_lowercase();
    match command.as_str() {
        "/follow" | "/tail" => {
            let minutes = words
                .next()
                .and_then(|word| word.trim_end_matches(['m', 'M']).parse::<u32>().ok())
                .unwrap_or(DEFAULT_MINUTES)
                .clamp(1, MAX_MINUTES);
            Some(FollowCommand::Follow { minutes })
        }
        "/unfollow" | "/untail" => Some(FollowCommand::Unfollow),
        _ => None,
    }
}

/// What the end of a session's event log says it is doing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogTail {
    /// The session's latest messages, oldest first, at most [`SHOWN_MESSAGES`].
    pub messages: Vec<String>,
    /// Tools started and not yet finished, as "name: argument".
    pub running: Vec<String>,
    /// An `ask_user` is open.
    pub question_pending: bool,
    /// When the log last grew, if a record said.
    pub last_event_at: Option<DateTime<Utc>>,
}

impl LogTail {
    /// Build the view from the last lines of `events.jsonl`, in file order.
    pub fn from_lines<'a>(lines: impl IntoIterator<Item = &'a str>) -> Self {
        let mut messages: Vec<String> = Vec::new();
        let mut started: Vec<(String, String)> = Vec::new();
        let mut completed: BTreeSet<String> = BTreeSet::new();
        let mut question_open: Option<String> = None;
        let mut last_event_at = None;
        for line in lines {
            let Ok(event) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if let Some(stamp) = event["timestamp"]
                .as_str()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            {
                last_event_at = Some(stamp.with_timezone(&Utc));
            }
            let data = &event["data"];
            match event["type"].as_str() {
                Some("assistant.message") => {
                    if let Some(content) = data["content"].as_str() {
                        let content = content.trim();
                        if !content.is_empty() {
                            messages.push(content.to_string());
                            if messages.len() > SHOWN_MESSAGES {
                                messages.remove(0);
                            }
                        }
                    }
                }
                Some("tool.execution_start") => {
                    let (Some(id), Some(name)) =
                        (data["toolCallId"].as_str(), data["toolName"].as_str())
                    else {
                        continue;
                    };
                    if name == "ask_user" {
                        question_open = Some(id.to_string());
                        continue;
                    }
                    started.push((id.to_string(), describe_tool(name, &data["arguments"])));
                }
                Some("tool.execution_complete") => {
                    if let Some(id) = data["toolCallId"].as_str() {
                        completed.insert(id.to_string());
                        if question_open.as_deref() == Some(id) {
                            question_open = None;
                        }
                    }
                }
                _ => {}
            }
        }
        let running = started
            .into_iter()
            .filter(|(id, _)| !completed.contains(id))
            .map(|(_, description)| description)
            .collect();
        Self {
            messages,
            running,
            question_pending: question_open.is_some(),
            last_event_at,
        }
    }
}

/// "bash: cargo test", from a tool's name and the one argument worth showing.
fn describe_tool(name: &str, arguments: &Value) -> String {
    let argument = [
        "command",
        "path",
        "file_path",
        "pattern",
        "query",
        "url",
        "prompt",
    ]
    .iter()
    .find_map(|key| arguments[*key].as_str())
    .map(|value| value.lines().next().unwrap_or("").trim())
    .filter(|value| !value.is_empty());
    match argument {
        Some(value) => {
            let shown: String = value.chars().take(ARGUMENT_WIDTH).collect();
            let ellipsis = if shown.chars().count() < value.chars().count() {
                "…"
            } else {
                ""
            };
            format!("{name}: {shown}{ellipsis}")
        }
        None => name.to_string(),
    }
}

/// Everything one rendering of the view needs.
#[derive(Debug, Clone)]
pub struct FollowView<'a> {
    /// The session's name, or `None` in a forum where the topic already carries it.
    pub title: Option<&'a str>,
    /// Whether a turn is in progress, as the lifecycle hook last reported. The poller
    /// cannot see panes, but it can read the journal; an idle session's log is still
    /// worth looking at, and the header says which it is.
    pub turn_running: bool,
    pub ends_in: Duration,
    pub tail: &'a LogTail,
}

/// Why a follow ended, for its last edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    Expired,
    Stopped,
    SessionClosed,
}

/// Everything one rendering of the screen view needs: the host CST's counterpart of
/// [`FollowView`], built from the pane rather than the log.
#[derive(Debug, Clone)]
pub struct ScreenView<'a> {
    pub title: Option<&'a str>,
    pub turn_running: bool,
    pub ends_in: Duration,
    /// The screen's rows, as [`screen::rows`] cuts them.
    pub rows: &'a [String],
}

/// What the live view's footer says. A slash command in a message is tappable in
/// Telegram, so this is also the button that stops the view.
const STOP_HINT: &str = "Send /unfollow to stop.";

/// The message as it stands while the follow is live.
pub fn render(view: &FollowView<'_>) -> String {
    let header = live_header(view.title, view.turn_running, view.ends_in);
    render_with_footer(view, &header, Some(STOP_HINT))
}

/// The message's final form, marked so it cannot be read as current.
pub fn render_ended(view: &FollowView<'_>, reason: EndReason) -> String {
    render_with_footer(view, &ended_header(view.title), Some(&ended_footer(reason)))
}

/// The live view from the screen: header, the rows in a monospace block cut from the
/// top to fit, and the way to stop it.
pub fn render_screen(view: &ScreenView<'_>) -> String {
    let header = live_header(view.title, view.turn_running, view.ends_in);
    let footer = format!("<i>{}</i>", escape_html(STOP_HINT));
    with_screen_block(&header, view.rows, &footer)
}

/// The screen view's final form.
pub fn render_screen_ended(view: &ScreenView<'_>, reason: EndReason) -> String {
    let footer = format!("<i>{}</i>", escape_html(&ended_footer(reason)));
    with_screen_block(&ended_header(view.title), view.rows, &footer)
}

/// One picture of the screen, asked for with `/peek`.
pub fn render_peek(title: Option<&str>, rows: &[String]) -> String {
    let header = match title {
        Some(title) => format!("<b>Peek: {}</b>", escape_html(title)),
        None => "<b>Peek</b>".to_string(),
    };
    with_screen_block(&header, rows, "")
}

fn with_screen_block(header: &str, rows: &[String], footer: &str) -> String {
    let fixed = header.chars().count() + footer.chars().count() + 2;
    let block = super::screen::as_pre(rows, MESSAGE_LIMIT.saturating_sub(fixed));
    let mut text = format!("{header}\n{block}");
    if !footer.is_empty() {
        text.push('\n');
        text.push_str(footer);
    }
    text
}

fn ended_header(title: Option<&str>) -> String {
    match title {
        Some(title) => format!("<b>Followed {}</b>", escape_html(title)),
        None => "<b>Followed</b>".to_string(),
    }
}

fn ended_footer(reason: EndReason) -> String {
    match reason {
        EndReason::Expired => {
            "⏹ Follow ended: time is up. Send /follow to watch again.".to_string()
        }
        EndReason::Stopped => "⏹ Follow stopped.".to_string(),
        EndReason::SessionClosed => "⏹ Follow ended: the session closed.".to_string(),
    }
}

fn live_header(title: Option<&str>, turn_running: bool, ends_in: Duration) -> String {
    let minutes = ends_in.as_secs().div_ceil(60).max(1);
    let name = match title {
        Some(title) => format!("<b>Following {}</b>", escape_html(title)),
        None => "<b>Following</b>".to_string(),
    };
    let state = if turn_running { "" } else { " · idle" };
    format!(
        "{name}{state} · <i>updates every {} s · ends in {minutes} min</i>",
        EDIT_EVERY.as_secs()
    )
}

/// Header, then the body made to fit under the header and footer.
///
/// A follow is one message edited in place, so unlike a notification it cannot run
/// to several parts; what does not fit is dropped from the top, oldest words first,
/// because the newest are what a live view is for.
fn render_with_footer(view: &FollowView<'_>, header: &str, footer: Option<&str>) -> String {
    let status = status_lines(view.tail);
    let footer_html = footer
        .map(|text| format!("\n\n<i>{}</i>", escape_html(text)))
        .unwrap_or_default();
    let fixed = header.chars().count() + status.chars().count() + footer_html.chars().count() + 4;
    let budget = MESSAGE_LIMIT.saturating_sub(fixed);

    let mut body = body_html(&view.tail.messages);
    let mut messages: Vec<String> = view.tail.messages.clone();
    while body.chars().count() > budget && messages.len() > 1 {
        messages.remove(0);
        body = body_html(&messages);
    }
    if body.chars().count() > budget {
        if let Some(last) = messages.last() {
            body = body_html(&[tail_of(last, budget)]);
        }
    }
    let mut text = header.to_string();
    if !body.is_empty() {
        text.push_str("\n\n");
        text.push_str(&body);
    }
    if !status.is_empty() {
        text.push_str("\n\n");
        text.push_str(&status);
    }
    text.push_str(&footer_html);
    text
}

fn body_html(messages: &[String]) -> String {
    messages
        .iter()
        .map(|message| to_telegram_html(message))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// What the session is doing right now, under its words.
fn status_lines(tail: &LogTail) -> String {
    let mut lines: Vec<String> = tail
        .running
        .iter()
        .map(|tool| format!("▶ <code>{}</code>", escape_html(tool)))
        .collect();
    if tail.question_pending {
        lines.push("❓ waiting on a question".to_string());
    }
    lines.join("\n")
}

/// The end of a message, cut at a paragraph or line so what is shown starts whole.
///
/// `budget` is in rendered characters; Markdown rarely grows much in rendering, and
/// the few tags it adds are allowed for in the caller's fixed reserve.
fn tail_of(message: &str, budget: usize) -> String {
    let total = message.chars().count();
    if total <= budget {
        return message.to_string();
    }
    let skip = total - budget;
    let start = message
        .char_indices()
        .nth(skip)
        .map(|(index, _)| index)
        .unwrap_or(0);
    let rest = &message[start..];
    let cut = rest
        .find("\n\n")
        .map(|at| at + 2)
        .or_else(|| rest.find('\n').map(|at| at + 1))
        .unwrap_or(0);
    format!("…{}", &rest[cut..])
}

/// A fingerprint of a rendering, so an unchanged view is not re-sent: Telegram answers
/// an identical edit with an error, and the call is wasted anyway.
pub fn content_hash(html: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    html.hash(&mut hasher);
    hasher.finish()
}

pub mod wording {
    pub fn needs_full_content() -> String {
        "Following a session shows its words, which Content: Status only keeps off \
         Telegram. Turn on Full content in CST's Telegram settings to follow."
            .to_string()
    }

    pub fn nothing_to_unfollow() -> String {
        "No follow is running for this session.".to_string()
    }

    /// `/follow image`: a picture re-uploaded every ten seconds is not a fair use of
    /// anyone's data plan, so a live view is text.
    pub fn no_image_follow() -> String {
        "A live view is text, because an image would be uploaded again every ten seconds. \
         Send /follow for the screen as text, or /peek image for one picture."
            .to_string()
    }

    /// `/peek image` before pictures are drawn: said rather than silently sent as text.
    pub fn no_images_yet() -> String {
        "Pictures of the screen are not available in this version of CST; here is the \
         screen as text."
            .to_string()
    }

    pub fn too_many_follows(limit: usize) -> String {
        format!(
            "{limit} sessions are already being followed, which is as many as Telegram \
             comfortably allows. Send /unfollow in one of them first."
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tail(messages: &[&str]) -> LogTail {
        LogTail {
            messages: messages.iter().map(|m| m.to_string()).collect(),
            ..LogTail::default()
        }
    }

    #[test]
    fn only_cst_s_own_commands_are_commands_and_the_minutes_are_clamped() {
        assert_eq!(
            parse_command("/follow"),
            Some(FollowCommand::Follow { minutes: 5 })
        );
        assert_eq!(
            parse_command("  /Follow 10 "),
            Some(FollowCommand::Follow { minutes: 10 })
        );
        assert_eq!(
            parse_command("/follow@cst_bot 12m"),
            Some(FollowCommand::Follow { minutes: 12 })
        );
        // Asking for too much gets the cap, not a refusal; nonsense gets the default.
        assert_eq!(
            parse_command("/follow 500"),
            Some(FollowCommand::Follow { minutes: 30 })
        );
        assert_eq!(
            parse_command("/follow soon"),
            Some(FollowCommand::Follow { minutes: 5 })
        );
        assert_eq!(parse_command("/unfollow"), Some(FollowCommand::Unfollow));
        // Copilot's own slash commands, and plain text, must reach the session.
        assert_eq!(parse_command("/compact"), None);
        assert_eq!(parse_command("follow the plan please"), None);
        assert_eq!(parse_command(""), None);
    }

    #[test]
    fn the_tail_keeps_the_last_messages_and_the_tools_still_running() {
        let lines = [
            r#"{"type":"assistant.message","data":{"content":"First thing."},"timestamp":"2026-10-26T10:00:00Z"}"#,
            r#"{"type":"assistant.message","data":{"content":"Second."}}"#,
            r#"{"type":"tool.execution_start","data":{"toolCallId":"a","toolName":"bash","arguments":{"command":"cargo test\n--all"}}}"#,
            r#"{"type":"tool.execution_complete","data":{"toolCallId":"a"}}"#,
            r#"{"type":"tool.execution_start","data":{"toolCallId":"b","toolName":"edit","arguments":{"path":"src/main.rs"}}}"#,
            r#"{"type":"assistant.message","data":{"content":"   "}}"#,
            r#"{"type":"assistant.message","data":{"content":"Third."}}"#,
            r#"{"type":"assistant.message","data":{"content":"Fourth."},"timestamp":"2026-10-26T10:05:00Z"}"#,
            "not json",
        ];
        let tail = LogTail::from_lines(lines.iter().copied());
        // Oldest first, blanks skipped, only the last three.
        assert_eq!(tail.messages, vec!["Second.", "Third.", "Fourth."]);
        // The finished bash is gone; the edit is still running, with its first line.
        assert_eq!(tail.running, vec!["edit: src/main.rs"]);
        assert!(!tail.question_pending);
        assert_eq!(
            tail.last_event_at.map(|t| t.to_rfc3339()),
            Some("2026-10-26T10:05:00+00:00".to_string())
        );
    }

    #[test]
    fn an_open_question_is_shown_and_an_answered_one_is_not() {
        let open = [
            r#"{"type":"tool.execution_start","data":{"toolCallId":"q","toolName":"ask_user","arguments":{"message":"Which?"}}}"#,
        ];
        assert!(LogTail::from_lines(open.iter().copied()).question_pending);
        let answered = [
            open[0],
            r#"{"type":"tool.execution_complete","data":{"toolCallId":"q"}}"#,
        ];
        assert!(!LogTail::from_lines(answered.iter().copied()).question_pending);
    }

    #[test]
    fn the_live_view_has_a_header_the_words_as_markdown_and_what_is_running() {
        let mut tail = tail(&["## Done\n\nChanged `a.rs`."]);
        tail.running = vec!["bash: cargo test".to_string()];
        tail.question_pending = true;
        let view = FollowView {
            title: Some("Plan <review>"),
            turn_running: true,
            ends_in: Duration::from_secs(290),
            tail: &tail,
        };
        assert_eq!(
            render(&view),
            "<b>Following Plan &lt;review&gt;</b> · <i>updates every 10 s · ends in 5 min</i>\n\n\
             <b>Done</b>\n\nChanged <code>a.rs</code>.\n\n\
             ▶ <code>bash: cargo test</code>\n❓ waiting on a question\n\n\
             <i>Send /unfollow to stop.</i>"
        );
        // In a forum the topic names the session; an idle session says so.
        let idle = FollowView {
            title: None,
            turn_running: false,
            ends_in: Duration::from_secs(61),
            tail: &tail,
        };
        assert!(render(&idle)
            .starts_with("<b>Following</b> · idle · <i>updates every 10 s · ends in 2 min</i>"));
    }

    #[test]
    fn an_ended_follow_cannot_be_read_as_live() {
        let tail = tail(&["Last words."]);
        let view = FollowView {
            title: Some("S"),
            turn_running: true,
            ends_in: Duration::ZERO,
            tail: &tail,
        };
        let expired = render_ended(&view, EndReason::Expired);
        assert!(expired.starts_with("<b>Followed S</b>\n\nLast words."));
        assert!(
            expired.ends_with("<i>⏹ Follow ended: time is up. Send /follow to watch again.</i>")
        );
        assert!(!expired.contains("updates every"));
        assert!(render_ended(&view, EndReason::Stopped).ends_with("<i>⏹ Follow stopped.</i>"));
        assert!(render_ended(&view, EndReason::SessionClosed)
            .ends_with("<i>⏹ Follow ended: the session closed.</i>"));
    }

    #[test]
    fn what_does_not_fit_is_dropped_from_the_top_oldest_first() {
        // Three messages, the first two huge: both go, the newest stays whole.
        let big = "word ".repeat(1500);
        let tail = tail(&[&big, &big, "The newest, kept whole."]);
        let view = FollowView {
            title: None,
            turn_running: true,
            ends_in: Duration::from_secs(60),
            tail: &tail,
        };
        let html = render(&view);
        assert!(html.chars().count() <= MESSAGE_LIMIT);
        assert!(html.contains(
            "The newest, kept whole.

<i>Send /unfollow to stop.</i>"
        ));
        assert!(!html.contains("word word"));

        // One message too long on its own: its end is shown, from a whole line.
        let lines = (0..400)
            .map(|i| format!("line {i} of a very long single message"))
            .collect::<Vec<_>>()
            .join("\n");
        let tail = self::tail(&[&lines]);
        let view = FollowView {
            tail: &tail,
            ..view
        };
        let html = render(&view);
        assert!(html.chars().count() <= MESSAGE_LIMIT);
        assert!(html.contains("…line "), "{html:.120}");
        assert!(html
            .contains("line 399 of a very long single message\n\n<i>Send /unfollow to stop.</i>"));
    }

    #[test]
    fn the_same_view_hashes_the_same_and_a_changed_one_does_not() {
        let a = content_hash("<b>x</b>");
        assert_eq!(a, content_hash("<b>x</b>"));
        assert_ne!(a, content_hash("<b>y</b>"));
    }
}
