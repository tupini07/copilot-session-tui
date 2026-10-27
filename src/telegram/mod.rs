//! Two-way notifications over a Telegram bot.
//!
//! ntfy tells a phone that a session wants attention and stops there. This channel
//! sends what the session actually said, and carries an answer back: a reply typed on
//! the phone lands in that session's composer and is submitted. The user is often away
//! from the PC with many sessions running, and Copilot's own remote steering is not
//! available to them, so this has to be CST's.
//!
//! The design copies `crate::threads` where it can — a worker that polls, a locked shared
//! state file, an apply pass on the UI thread that knows which panes can take input — and
//! differs in the one place that matters: a reply **carries text**. Threads refuse that
//! because anyone can comment on an issue. Here the sender is one Telegram user, written
//! into the config by pairing and verified by Telegram on every update; a message from
//! anyone else is dropped without a word. That is the same trust the user exercises by
//! typing at the keyboard, through a different keyboard.
//!
//! Only the lock holder polls, because Telegram allows one `getUpdates` consumer per
//! bot. Every CST sends. Whichever CST has the session open delivers, found through the
//! state file's modification time exactly as thread notices are.
//!
//! This module is the model and the wording. Everything here is pure, so the security
//! decisions and every sentence the phone sees can be tested without a network.

pub mod answer;
pub mod bot;
pub mod cli;
pub mod follow;
pub mod markdown;
pub mod queue;
pub mod store;
pub mod wizard;
pub mod worker;

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::TelegramChatMode;
use crate::notifications::NotificationKind;

/// How long one `getUpdates` waits for something to arrive.
///
/// A long poll still, so idle costs a request every ten seconds and nothing more. Not
/// longer, because a ureq call cannot be interrupted: quitting CST or pasting a new
/// token waits for the poll in flight to return, and ten seconds is the most that
/// should cost.
pub const POLL_TIMEOUT: Duration = Duration::from_secs(10);

/// Telegram's limit on one message's text.
pub const MESSAGE_LIMIT: usize = 4_096;

/// The most messages one notification may become. A safety valve against a runaway,
/// not a length anyone should meet: Telegram lets a bot send about twenty messages a
/// minute to one group, and a hundred thousand characters is not a message to read on
/// a phone.
pub const MAX_MESSAGES: usize = 25;

/// Room kept in each part for the tags that close a run cut by the split and reopen it
/// in the next part: a quote holding a code block with its language, with bold and
/// italic inside, and the closers the next part hands back.
const TAG_RESERVE: usize = 200;

/// How long a pairing code stays valid. Long enough to open Telegram and find the bot.
pub const PAIRING_WINDOW: chrono::Duration = chrono::Duration::minutes(5);

/// The one chat and user whose messages are acted on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Paired {
    pub chat_id: i64,
    pub user_id: i64,
    pub mode: TelegramChatMode,
}

/// What an incoming message means for CST.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// From the paired user in the paired chat.
    Act,
    /// Anyone or anywhere else. Nothing runs, nothing is recorded, nobody is asked.
    Ignore,
}

/// Decide whether a message may do anything at all.
///
/// Both ids have to match. The chat alone is not enough: the bot may have been added to
/// a group, and a group has other people in it. The user alone is not enough either,
/// because the same person writing to the bot from a chat that was never paired is not
/// what they set up — and in a group, "which chat" is "which topic belongs to which
/// session", so a stray chat would route nowhere sensible.
pub fn classify(chat_id: i64, from_id: Option<i64>, paired: &Paired) -> Verdict {
    match from_id {
        Some(from) if chat_id == paired.chat_id && from == paired.user_id => Verdict::Act,
        _ => Verdict::Ignore,
    }
}

/// A message CST sent, kept so a reply to it can be routed to its session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentRecord {
    pub message_id: i64,
    pub session_id: String,
    /// Which kind of attention event this was part of, when it was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<SentKind>,
    /// The question this message put to the user, so an answer can be matched to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    pub sent_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SentKind {
    Ready,
    Question,
    PlanApproval,
    Error,
}

impl From<NotificationKind> for SentKind {
    fn from(kind: NotificationKind) -> Self {
        match kind {
            NotificationKind::Ready => Self::Ready,
            NotificationKind::Question => Self::Question,
            NotificationKind::PlanApproval => Self::PlanApproval,
            NotificationKind::Error => Self::Error,
        }
    }
}

/// A forum topic standing in for a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopicRecord {
    pub thread_id: i64,
    /// The name the topic was given, so a renamed session renames its topic once.
    pub title: String,
}

/// The parts of an incoming message that routing looks at.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Incoming {
    pub message_thread_id: Option<i64>,
    pub reply_to_message_id: Option<i64>,
}

/// Which session a message is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Session(String),
    /// Nothing says which session. Answered with a hint, never guessed.
    Unrouted,
}

/// Work out which session a message addresses.
///
/// In a forum the topic is the address, so a bare message typed into a topic goes to
/// that session. A reply to one of CST's messages works in either mode, and inside a
/// topic it says the same thing twice. A message with neither — in a private chat, or
/// in the group's General topic — is unrouted, because guessing "the last session that
/// spoke" would send text to the wrong agent exactly when several are busy.
pub fn route(
    incoming: &Incoming,
    mode: TelegramChatMode,
    sent: &[SentRecord],
    topics: &BTreeMap<String, TopicRecord>,
) -> Route {
    if mode == TelegramChatMode::Forum {
        if let Some(thread_id) = incoming.message_thread_id {
            if let Some((session_id, _)) = topics
                .iter()
                .find(|(_, topic)| topic.thread_id == thread_id)
            {
                return Route::Session(session_id.clone());
            }
        }
    }
    if let Some(reply_to) = incoming.reply_to_message_id {
        if let Some(record) = sent.iter().find(|record| record.message_id == reply_to) {
            return Route::Session(record.session_id.clone());
        }
    }
    Route::Unrouted
}

/// A question a session is asking, read from its event log at send time.
///
/// Copilot's `ask_user` is a form: one message, then one or more fields, each a list
/// of choices or a free-text entry with a default. A single-choice question is a form
/// with one field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub tool_call_id: String,
    pub text: String,
    pub fields: Vec<QuestionField>,
}

impl Question {
    /// Every choice offered, across fields, in the order the phone numbers them.
    #[cfg(test)]
    pub fn choices(&self) -> impl Iterator<Item = &str> {
        self.fields
            .iter()
            .flat_map(|field| field.choices.iter().map(String::as_str))
    }
}

/// One field of a question form.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QuestionField {
    /// Empty for the older single-question shape, which has no field titles.
    pub title: String,
    pub description: Option<String>,
    /// Empty for a free-text field.
    pub choices: Vec<String>,
    pub default: Option<String>,
}

/// Everything one notification says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Content {
    pub kind: NotificationKind,
    pub title: String,
    /// The session's last assistant message of this work cycle, when content is on.
    pub body: Option<String>,
    pub question: Option<Question>,
    pub plan_summary: Option<String>,
    /// The repository the session's folder belongs to, once known, so a `#1234` in the
    /// body opens that issue or pull request from the phone.
    pub repository: Option<crate::github::RepositoryRef>,
}

/// Turn a notification into the HTML messages Telegram is sent, in order.
///
/// Every message starts with the session title in bold. Messages from several sessions
/// arrive interleaved in one chat, and the title is what tells them apart, including in
/// the quote Telegram shows above a reply. HTML rather than MarkdownV2: its escaping is
/// three characters, and what the agent wrote is Markdown, which [`markdown`] turns into
/// the tags Telegram knows and shows everything else as text.
pub fn compose(content: &Content, mode: TelegramChatMode) -> Vec<String> {
    // In a forum the topic already carries the name, so repeating it would read as
    // part of what the session said. In a private chat it is the only thing telling
    // interleaved sessions apart, kept in brackets so it reads as a label.
    let title = match mode {
        TelegramChatMode::Private => format!("<b>({})</b>", escape_html(&content.title)),
        TelegramChatMode::Forum => String::new(),
    };
    let body = content
        .body
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty());
    let mut text = String::new();
    // A finished turn with its text needs no caption: what the session said is the
    // news. The line stays where it carries something the text does not — a question,
    // a plan, an error — or when there is no text to show.
    if content.kind != NotificationKind::Ready || body.is_none() {
        text.push_str(&format!("<i>{}</i>", escape_html(kind_line(content.kind))));
    }
    if let Some(body) = body {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&markdown::to_telegram_html_in(
            body,
            content.repository.as_ref(),
        ));
    }
    if let Some(question) = &content.question {
        text.push_str("\n\n");
        text.push_str(&escape_html("Question: "));
        text.push_str(&escape_html(question.text.trim()));
        // Choices are numbered straight through the form, so "2" means one thing
        // whichever field it belongs to. A free-text field shows what it is for and
        // what it takes when nothing is said.
        let mut number = 0;
        for field in &question.fields {
            if !field.title.trim().is_empty() {
                text.push_str(&format!("\n<b>{}</b>", escape_html(field.title.trim())));
                if field.choices.is_empty() {
                    if let Some(description) = field.description.as_deref() {
                        text.push_str(&escape_html(&format!(" — {}", description.trim())));
                    }
                }
            }
            for choice in &field.choices {
                number += 1;
                text.push_str(&escape_html(&format!("\n  {number}. {}", choice.trim())));
            }
            if field.choices.is_empty() {
                let default = field.default.as_deref().map(str::trim).unwrap_or("");
                text.push_str(&escape_html(&if default.is_empty() {
                    "\n  (free text)".to_string()
                } else {
                    format!("\n  (free text; default: {default})")
                }));
            }
        }
        if !question.fields.is_empty() {
            text.push_str(&format!(
                "\n<i>{}</i>",
                escape_html(&wording::how_to_answer(question))
            ));
        }
    }
    if let Some(summary) = content
        .plan_summary
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        // A plan is Markdown too, usually several paragraphs; a one-line summary stays
        // on the label's line.
        let html = markdown::to_telegram_html(summary);
        text.push_str("\n\n");
        text.push_str(if html.contains('\n') {
            "Plan:\n"
        } else {
            "Plan: "
        });
        text.push_str(&html);
    }
    chunk(&title, &text)
}

fn kind_line(kind: NotificationKind) -> &'static str {
    match kind {
        NotificationKind::Ready => "Ready for attention",
        NotificationKind::Question => "Question waiting for your response",
        NotificationKind::PlanApproval => "Plan waiting for approval",
        NotificationKind::Error => "Copilot reported an error",
    }
}

/// Split escaped text into messages under Telegram's limit, each headed by the label.
///
/// The whole message goes, in as many parts as it takes: the text is what the user
/// opened Telegram to read, and a note saying "the rest is on the PC" is what they
/// came to avoid. Cuts fall on a paragraph break where one is near, then a line break,
/// then the end of a sentence, then a word boundary, so each part reads whole; never
/// inside an HTML entity, a tag or a multi-byte character. A cut inside formatting — a
/// code block, a bold run, a quote — closes it at the end of one part and opens it
/// again at the start of the next, because Telegram rejects a message whose tags do not
/// balance, and rejects the whole of it. When there is more than one part, each says
/// which it is. [`MAX_MESSAGES`] is a safety valve against a runaway, not a length
/// anyone should meet.
fn chunk(title_html: &str, text_html: &str) -> Vec<String> {
    let header = |index: usize, total: usize| -> String {
        let label = if title_html.is_empty() {
            String::new()
        } else {
            format!("{title_html} ")
        };
        match total {
            0 | 1 if title_html.is_empty() => String::new(),
            0 | 1 => format!("{title_html}\n"),
            total => format!("{label}<i>(part {} of {total})</i>\n", index + 1),
        }
    };
    // Room for the longest header, the note a truncation would add, and the tags that
    // close a cut-open run and reopen it.
    let reserve = header(MAX_MESSAGES, MAX_MESSAGES).chars().count() + 80 + TAG_RESERVE;
    let budget = MESSAGE_LIMIT.saturating_sub(reserve).max(256);

    let mut pieces: Vec<String> = Vec::new();
    let mut rest = text_html.to_string();
    while !rest.is_empty() && pieces.len() < MAX_MESSAGES {
        let (piece, remainder) = split_at_budget(&rest, budget);
        let open = open_tags(piece);
        let mut part = piece.trim().to_string();
        for (_, tag) in open.iter().rev() {
            part.push_str(&closing_tag(tag));
        }
        let mut next = String::new();
        if !remainder.trim().is_empty() {
            for (_, tag) in &open {
                next.push_str(tag);
            }
            next.push_str(remainder.trim_start_matches('\n'));
        }
        pieces.push(part);
        rest = next;
    }
    if pieces.is_empty() {
        pieces.push(String::new());
    }
    let total = pieces.len();
    let mut messages: Vec<String> = pieces
        .iter()
        .enumerate()
        .map(|(index, piece)| {
            let mut message = header(index, total);
            message.push_str(piece);
            message
        })
        .collect();
    if !rest.is_empty() {
        let cut = rest.chars().count();
        if let Some(last) = messages.last_mut() {
            last.push_str(&format!(
                "\n\n… {cut} more characters not shown; open the session to read the rest."
            ));
        }
    }
    messages
}

/// Cut `text` after at most `budget` characters, at the best boundary available.
///
/// Preference, each looked for in the tail of the budget so a part is never much
/// shorter than it could be: a blank line, a line break, the end of a sentence, a
/// space. Never between `&` and its `;`, and never inside a character.
fn split_at_budget(text: &str, budget: usize) -> (&str, &str) {
    if text.chars().count() <= budget {
        return (text, "");
    }
    let byte_at = |characters: usize| {
        text.char_indices()
            .nth(characters)
            .map(|(index, _)| index)
            .unwrap_or(text.len())
    };
    let limit = byte_at(budget);
    let window = |fraction_kept: usize| &text[byte_at(budget * fraction_kept / 10)..limit];

    // A paragraph break anywhere in the last half; a line break in the last third;
    // a sentence end in the last third; a space in the last fifth.
    let mut cut = None;
    if let Some(at) = window(5).rfind("\n\n") {
        cut = Some(byte_at(budget * 5 / 10) + at + 2);
    }
    if cut.is_none() {
        if let Some(at) = window(7).rfind('\n') {
            cut = Some(byte_at(budget * 7 / 10) + at + 1);
        }
    }
    if cut.is_none() {
        let tail = window(7);
        if let Some(at) = sentence_end(tail) {
            cut = Some(byte_at(budget * 7 / 10) + at);
        }
    }
    if cut.is_none() {
        if let Some(at) = window(8).rfind(' ') {
            cut = Some(byte_at(budget * 8 / 10) + at + 1);
        }
    }
    let mut cut = cut.unwrap_or(limit).min(limit);
    // Inside an entity: back up to the `&` that opened it.
    if let Some(amp) = text[..cut].rfind('&') {
        if !text[amp..cut].contains(';') {
            cut = amp;
        }
    }
    cut = clear_of_tags(text, cut);
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    if cut == 0 {
        cut = clear_of_tags(text, limit);
    }
    if cut == 0 {
        cut = limit;
    }
    (&text[..cut], &text[cut..])
}

/// Move a cut so neither side is left with markup it cannot show.
///
/// Never inside a tag. Not inside a link: its address would be repeated in the next
/// part, and a link is short enough to move whole. Not right after an opening tag,
/// which would close again with nothing inside. And when the next part would begin
/// with closing tags, they belong to this one, rather than closing a run here to
/// reopen it there for nothing.
fn clear_of_tags(text: &str, mut cut: usize) -> usize {
    if let Some(open) = text[..cut].rfind('<') {
        if !text[open..cut].contains('>') {
            cut = open;
        }
    }
    if let Some((at, _)) = open_tags(&text[..cut])
        .into_iter()
        .find(|(_, tag)| tag_name(tag) == "a")
    {
        cut = at;
    }
    loop {
        let head = text[..cut].trim_end();
        let Some(open) = head.rfind('<') else { break };
        if head.ends_with('>') && !head[open..].starts_with("</") {
            cut = open;
        } else {
            break;
        }
    }
    while text[cut..].starts_with("</") {
        match text[cut..].find('>') {
            Some(end) => cut += end + 1,
            None => break,
        }
    }
    cut
}

/// The tags still open at the end of `html`, with where each starts.
fn open_tags(html: &str) -> Vec<(usize, &str)> {
    let mut open: Vec<(usize, &str)> = Vec::new();
    let mut from = 0;
    while let Some(start) = html[from..].find('<').map(|at| from + at) {
        let Some(end) = html[start..].find('>').map(|at| start + at + 1) else {
            break;
        };
        let tag = &html[start..end];
        if let Some(name) = tag.strip_prefix("</") {
            let name = name.trim_end_matches('>');
            if open.last().is_some_and(|(_, last)| tag_name(last) == name) {
                open.pop();
            }
        } else {
            open.push((start, tag));
        }
        from = end;
    }
    open
}

fn tag_name(tag: &str) -> &str {
    tag.trim_start_matches('<')
        .split([' ', '>'])
        .next()
        .unwrap_or("")
}

fn closing_tag(tag: &str) -> String {
    format!("</{}>", tag_name(tag))
}

/// The byte just after the last sentence end in `text`: `.`, `!` or `?` followed by
/// whitespace, or one of them closing the text.
fn sentence_end(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    (1..bytes.len())
        .rev()
        .find(|&i| {
            matches!(bytes[i - 1], b'.' | b'!' | b'?') && (bytes[i] == b' ' || bytes[i] == b'\n')
        })
        .map(|i| i + 1)
}

/// The commands CST answers itself, for Telegram's `/` menu.
///
/// Only CST's own: Copilot's slash commands pass through untouched and are not CST's to
/// advertise. The aliases (`/tail`, `/untail`) still work but stay out of the menu,
/// which is meant to be read, not completed.
pub fn bot_commands() -> &'static [bot::BotCommand] {
    &[
        bot::BotCommand {
            command: "follow",
            description: "Watch a session live for a few minutes (add a number of minutes)",
        },
        bot::BotCommand {
            command: "unfollow",
            description: "Stop the live view",
        },
        bot::BotCommand {
            command: "queue",
            description: "List replies waiting for a turn to end, each with a Cancel",
        },
    ]
}

/// The three characters Telegram's HTML mode reads as markup.
pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            other => out.push(other),
        }
    }
    out
}

/// A short code the user sends the bot to prove the chat is theirs.
///
/// Letters and digits that survive a deep link's parameter and are hard to misread;
/// six of them from a UUID is more than enough for a five-minute window.
pub fn pairing_code() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    uuid::Uuid::new_v4()
        .as_bytes()
        .iter()
        .take(6)
        .map(|byte| ALPHABET[(*byte as usize) % ALPHABET.len()] as char)
        .collect()
}

/// Whether a message is the pairing code, allowing for Telegram's `/start <code>`.
pub fn message_carries_code(text: &str, code: &str) -> bool {
    text.split_whitespace()
        .map(|word| word.trim_matches(|c: char| !c.is_ascii_alphanumeric()))
        .any(|word| word.eq_ignore_ascii_case(code))
}

/// The link that opens Telegram on the bot with the code already filled in.
///
/// Pressing Start on a private chat sends `/start <code>` for the user, so following
/// the link *is* the pairing. For a group the link asks which group to add the bot to
/// and requests the Manage Topics right on the way in, which is what creating a topic
/// per session needs.
pub fn deep_link(bot_username: &str, code: &str, mode: TelegramChatMode) -> String {
    let username = bot_username.trim_start_matches('@');
    match mode {
        TelegramChatMode::Private => format!("https://t.me/{username}?start={code}"),
        TelegramChatMode::Forum => {
            format!("https://t.me/{username}?startgroup={code}&admin=manage_topics")
        }
    }
}

/// Read the chat mode off the chat Telegram described.
pub fn chat_mode_of(chat_type: &str, is_forum: bool) -> Option<TelegramChatMode> {
    match chat_type {
        "private" => Some(TelegramChatMode::Private),
        "supergroup" if is_forum => Some(TelegramChatMode::Forum),
        _ => None,
    }
}

/// Everything the phone is told, in one place so the sentences can be read together.
///
/// Plain sentences that name the consequence. Sent as text, not HTML, so nothing here
/// needs escaping.
pub mod wording {
    use super::Question;
    use crate::config::TelegramChatMode;

    /// CST has just typed a thread pointer into the session. The link and nothing from
    /// the comment: that is the rule for what reaches a session, and it holds for what
    /// reaches the phone about it.
    pub fn thread_wake(title: &str, url: &str) -> String {
        format!("Woke {title} for a new comment on {url}. Its turn ends without a notification.")
    }

    pub fn paired(mode: TelegramChatMode) -> String {
        match mode {
            TelegramChatMode::Private => {
                "CST is paired with this chat. Each session's messages will \
                                          arrive here; reply to one to answer that session."
                    .to_string()
            }
            TelegramChatMode::Forum => "CST is paired with this group. Each session gets its own \
                                        topic; type in a topic to answer that session."
                .to_string(),
        }
    }

    pub fn group_without_topics() -> String {
        "This group does not have Topics turned on. Turn them on in the group's settings, \
         then send the code again, or pair a private chat instead."
            .to_string()
    }

    pub fn test_message() -> String {
        "Test message from CST. If you can read this, notifications will arrive here.".to_string()
    }

    pub fn not_open(title: &str) -> String {
        format!("Not delivered: {title} is not open in CST. Open it on the PC and reply again.")
    }

    pub fn deleted() -> String {
        "Not delivered: that session no longer exists.".to_string()
    }

    pub fn has_draft(title: &str) -> String {
        format!(
            "Not delivered: {title} has unsent text in its composer. Finish or clear it at the \
             PC, then reply again."
        )
    }

    pub fn queued(title: &str) -> String {
        format!("Queued: {title} is working. Your reply goes in when this turn ends.")
    }

    pub fn permission_wait(title: &str) -> String {
        format!(
            "Not delivered: {title} is waiting for a tool permission, which cannot be answered \
             from Telegram."
        )
    }

    /// A question is open but its record could not be read, so the keys that would
    /// answer it cannot be planned.
    pub fn question_not_supported(title: &str) -> String {
        format!(
            "Not delivered: {title} is waiting on a question CST could not read; answer it \
             at the PC."
        )
    }

    /// Under a question: how a reply is read. Short, because it is sent with every
    /// question.
    pub fn how_to_answer(question: &Question) -> String {
        let has_choices = question
            .fields
            .iter()
            .any(|field| !field.choices.is_empty());
        match (question.fields.len(), has_choices) {
            (1, true) => "Reply with a number, or type another answer.".to_string(),
            (1, false) => "Reply with your answer.".to_string(),
            (_, _) => "Reply with one line per field, in order: a number or text; \
                       a dash keeps the default."
                .to_string(),
        }
    }

    pub fn answered(title: &str, recorded: &str) -> String {
        format!("{title} took the answer: {recorded}")
    }

    pub fn answer_not_taken(title: &str) -> String {
        format!(
            "Not delivered: the answer did not take in {title}. The question is still open; \
             answer it at the PC."
        )
    }

    pub fn plan_not_supported(title: &str) -> String {
        format!(
            "Not delivered: {title} is waiting for a plan to be approved, which is done at the PC."
        )
    }

    pub fn replies_off() -> String {
        "Replies are turned off in CST's Telegram settings.".to_string()
    }

    pub fn unrouted_hint(mode: TelegramChatMode) -> String {
        match mode {
            TelegramChatMode::Private => {
                "Reply to a session's message to answer that session.".to_string()
            }
            TelegramChatMode::Forum => {
                "Type in a session's topic to answer that session.".to_string()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_menu_names_only_what_cst_answers_and_fits_telegrams_rules() {
        let commands = bot_commands();
        for command in commands {
            assert!(
                follow::parse_command(&format!("/{}", command.command)).is_some()
                    || queue::is_queue_command(&format!("/{}", command.command)),
                "/{} is in the menu but nothing answers it",
                command.command
            );
            assert!(
                (1..=32).contains(&command.command.len())
                    && command
                        .command
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                "Telegram rejects the whole list over one bad name: {}",
                command.command
            );
            assert!(
                (3..=256).contains(&command.description.chars().count()),
                "description out of Telegram's bounds for /{}",
                command.command
            );
        }
        assert!(
            !commands.iter().any(|c| c.command == "tail"),
            "aliases exist for typing, not for the menu"
        );
    }

    fn paired(mode: TelegramChatMode) -> Paired {
        Paired {
            chat_id: 1000,
            user_id: 77,
            mode,
        }
    }

    #[test]
    fn a_message_from_any_other_chat_or_user_is_ignored_and_never_surfaces() {
        // The security property of the whole channel: only the paired person in the
        // paired chat can put text into a session. A stranger in the group, the paired
        // user writing from somewhere else, and a message with no sender all do nothing.
        let paired = paired(TelegramChatMode::Forum);

        assert_eq!(classify(1000, Some(77), &paired), Verdict::Act);
        assert_eq!(classify(1000, Some(78), &paired), Verdict::Ignore);
        assert_eq!(classify(1001, Some(77), &paired), Verdict::Ignore);
        assert_eq!(classify(1000, None, &paired), Verdict::Ignore);
    }

    fn sent(message_id: i64, session: &str) -> SentRecord {
        SentRecord {
            message_id,
            session_id: session.to_string(),
            kind: Some(SentKind::Ready),
            tool_call_id: None,
            sent_at: Utc::now(),
        }
    }

    #[test]
    fn a_reply_to_a_sent_message_routes_to_that_session_and_a_bare_message_is_not_guessed() {
        let sent = vec![sent(5, "session-a"), sent(9, "session-b")];
        let topics = BTreeMap::new();

        let reply = Incoming {
            reply_to_message_id: Some(9),
            ..Default::default()
        };
        assert_eq!(
            route(&reply, TelegramChatMode::Private, &sent, &topics),
            Route::Session("session-b".to_string())
        );
        // Replying to a confirmation CST sent must work too; it is recorded like any other.
        let stale = Incoming {
            reply_to_message_id: Some(4),
            ..Default::default()
        };
        assert_eq!(
            route(&stale, TelegramChatMode::Private, &sent, &topics),
            Route::Unrouted
        );
        assert_eq!(
            route(
                &Incoming::default(),
                TelegramChatMode::Private,
                &sent,
                &topics
            ),
            Route::Unrouted,
            "guessing the last session would hit the wrong agent exactly when several are busy"
        );
    }

    #[test]
    fn in_a_forum_the_topic_is_the_address_and_general_is_unrouted() {
        let sent = vec![sent(5, "session-a")];
        let mut topics = BTreeMap::new();
        topics.insert(
            "session-c".to_string(),
            TopicRecord {
                thread_id: 300,
                title: "C".to_string(),
            },
        );

        let in_topic = Incoming {
            message_thread_id: Some(300),
            reply_to_message_id: None,
        };
        assert_eq!(
            route(&in_topic, TelegramChatMode::Forum, &sent, &topics),
            Route::Session("session-c".to_string())
        );
        // A reply inside a topic says the same thing twice; the topic wins and agrees.
        let reply_in_topic = Incoming {
            message_thread_id: Some(300),
            reply_to_message_id: Some(5),
        };
        assert_eq!(
            route(&reply_in_topic, TelegramChatMode::Forum, &sent, &topics),
            Route::Session("session-c".to_string())
        );
        // General has no topic id of its own that CST recorded, so it is unrouted
        // unless the message replies to something CST sent.
        let general = Incoming {
            message_thread_id: None,
            reply_to_message_id: None,
        };
        assert_eq!(
            route(&general, TelegramChatMode::Forum, &sent, &topics),
            Route::Unrouted
        );
        let reply_in_general = Incoming {
            message_thread_id: None,
            reply_to_message_id: Some(5),
        };
        assert_eq!(
            route(&reply_in_general, TelegramChatMode::Forum, &sent, &topics),
            Route::Session("session-a".to_string())
        );
    }

    fn content(body: Option<&str>) -> Content {
        Content {
            kind: NotificationKind::Ready,
            title: "Plan <review> & more".to_string(),
            body: body.map(str::to_string),
            question: None,
            plan_summary: None,
            repository: None,
        }
    }

    #[test]
    fn a_private_chat_message_is_labelled_with_the_session_and_agent_text_can_never_become_markup()
    {
        let messages = compose(
            &content(Some("Use <b>bold</b> & co.")),
            TelegramChatMode::Private,
        );

        assert_eq!(messages.len(), 1);
        // Brackets, so the name reads as a label rather than as something the session said.
        assert!(messages[0].starts_with("<b>(Plan &lt;review&gt; &amp; more)</b>\n"));
        // A finished turn with its text is captioned by the text itself.
        assert!(!messages[0].contains("Ready for attention"));
        assert!(messages[0].contains("Use &lt;b&gt;bold&lt;/b&gt; &amp; co."));
        assert!(!messages[0].contains("<b>bold</b>"));
    }

    #[test]
    fn a_ready_message_with_nothing_to_show_keeps_its_caption_and_an_error_always_has_one() {
        let messages = compose(&content(None), TelegramChatMode::Forum);
        assert_eq!(messages[0], "<i>Ready for attention</i>");

        let mut error = content(Some("Traceback: boom"));
        error.kind = NotificationKind::Error;
        let messages = compose(&error, TelegramChatMode::Forum);
        assert_eq!(
            messages[0],
            "<i>Copilot reported an error</i>\n\nTraceback: boom"
        );
    }

    #[test]
    fn a_forum_message_carries_no_name_because_the_topic_already_does() {
        let messages = compose(&content(Some("Done.")), TelegramChatMode::Forum);
        assert_eq!(messages[0], "Done.");
    }

    #[test]
    fn a_question_lists_its_choices_and_a_plan_carries_its_summary() {
        let mut content = content(Some("Context"));
        content.kind = NotificationKind::Question;
        content.question = Some(Question {
            tool_call_id: "q1".to_string(),
            text: "Which colour?".to_string(),
            fields: vec![QuestionField {
                choices: vec!["Red".to_string(), "Green".to_string()],
                ..QuestionField::default()
            }],
        });
        let messages = compose(&content, TelegramChatMode::Forum);
        assert!(messages[0].contains("Question waiting for your response"));
        assert!(messages[0].contains("Question: Which colour?\n  1. Red\n  2. Green"));

        // A form: each field titled, choices numbered straight through, a free-text
        // field saying what it is for and what it takes by default.
        content.question = Some(Question {
            tool_call_id: "q2".to_string(),
            text: "Approve the weekend plan?".to_string(),
            fields: vec![
                QuestionField {
                    title: "Policy".to_string(),
                    choices: vec!["Preserve <all>".to_string(), "Propose".to_string()],
                    ..QuestionField::default()
                },
                QuestionField {
                    title: "Finish time".to_string(),
                    description: Some("Local time.".to_string()),
                    default: Some("Monday 09:00".to_string()),
                    ..QuestionField::default()
                },
            ],
        });
        let messages = compose(&content, TelegramChatMode::Forum);
        assert!(
            messages[0].contains(
                "Question: Approve the weekend plan?\n<b>Policy</b>\n  1. Preserve &lt;all&gt;\n  2. Propose\n\
                 <b>Finish time</b> — Local time.\n  (free text; default: Monday 09:00)"
            ),
            "{}",
            messages[0]
        );

        // A plan is Markdown like the body, and a multi-line one starts under its label.
        let mut plan = self::content(None);
        plan.kind = NotificationKind::PlanApproval;
        plan.plan_summary = Some("- do this\n- then **that**".to_string());
        let messages = compose(&plan, TelegramChatMode::Forum);
        assert!(
            messages[0]
                .contains("Plan waiting for approval</i>\n\nPlan:\n• do this\n• then <b>that</b>"),
            "{}",
            messages[0]
        );
        plan.plan_summary = Some("Rename the module.".to_string());
        let messages = compose(&plan, TelegramChatMode::Forum);
        assert!(messages[0].ends_with("Plan: Rename the module."));
    }

    #[test]
    fn markdown_in_the_body_arrives_as_telegram_formatting() {
        let body = "## Done\n\nChanged `src/main.rs`:\n\n- **bold** item\n- [link](https://e.com)\n\n```rust\nlet a = 1 < 2;\n```";
        let messages = compose(&content(Some(body)), TelegramChatMode::Forum);
        assert_eq!(
            messages[0],
            "<b>Done</b>\n\nChanged <code>src/main.rs</code>:\n\n• <b>bold</b> item\n\
             • <a href=\"https://e.com\">link</a>\n\n\
             <pre><code class=\"language-rust\">let a = 1 &lt; 2;</code></pre>"
        );
    }

    /// What Telegram would accept: only its tags, every one closed in order, a quote
    /// never inside a quote, nothing that is not an entity left bare. Checked against
    /// the real API once: code inside bold or a link is accepted despite the
    /// documentation, so that is not refused here. The e2e stub does not check any of
    /// this, so this is the only oracle short of Telegram itself.
    fn assert_valid_telegram_html(html: &str) {
        const KNOWN: [&str; 8] = ["b", "i", "s", "u", "code", "pre", "a", "blockquote"];
        let mut open: Vec<&str> = Vec::new();
        let mut rest = html;
        while let Some(start) = rest.find('<') {
            let text = &rest[..start];
            assert!(!text.contains('>'), "bare > in {text:?}");
            let end = rest[start..].find('>').expect("unclosed tag") + start;
            let tag = &rest[start + 1..end];
            if let Some(name) = tag.strip_prefix('/') {
                assert_eq!(
                    open.pop(),
                    Some(name),
                    "closing {name} out of order in {html}"
                );
            } else {
                let name = tag.split(' ').next().unwrap();
                assert!(KNOWN.contains(&name), "unknown tag {name} in {html}");
                if name == "blockquote" {
                    assert!(!open.contains(&"blockquote"), "nested quote in {html}");
                }
                open.push(name);
            }
            rest = &rest[end + 1..];
        }
        assert!(open.is_empty(), "left open: {open:?} in {html}");
        for (index, _) in html.match_indices('&') {
            let entity = &html[index..html.len().min(index + 6)];
            assert!(
                ["&amp;", "&lt;", "&gt;", "&quot;"]
                    .iter()
                    .any(|known| entity.starts_with(known)),
                "bare & in {entity:?}"
            );
        }
    }

    #[test]
    fn a_split_inside_a_code_block_or_a_bold_run_closes_it_and_reopens_it_in_the_next_part() {
        // Telegram rejects the whole message when a tag is left open, so the cut must
        // leave each part balanced and the next one picking up where the first stopped.
        let code = (0..300)
            .map(|i| {
                format!("    let value_{i} = compute(&input[{i}]) < limit; // bold **not** here")
            })
            .collect::<Vec<_>>()
            .join("\n");
        let prose = "This **bold sentence runs on and on** with words. ".repeat(120);
        let body = format!("## Report\n\n> quoted **{prose}**\n\n```rust\n{code}\n```\n\n{prose}");
        let messages = compose(&content(Some(&body)), TelegramChatMode::Private);

        assert!(messages.len() >= 4, "{} parts", messages.len());
        for (index, message) in messages.iter().enumerate() {
            assert!(
                message.chars().count() <= MESSAGE_LIMIT,
                "part {index} is {} chars",
                message.chars().count()
            );
            assert_valid_telegram_html(message);
        }
        // The code block was long enough to need more than one part, each of them a
        // code block of its own, and no line of code was lost.
        let in_code: Vec<&String> = messages
            .iter()
            .filter(|m| m.contains("<pre><code class=\"language-rust\">"))
            .collect();
        assert!(
            in_code.len() >= 2,
            "code block was not split: {}",
            in_code.len()
        );
        let joined = messages.join("\n");
        for i in [0, 150, 299] {
            assert!(
                joined.contains(&format!("let value_{i} = ")),
                "line {i} lost"
            );
        }
        assert!(!joined.contains("more characters not shown"));
    }

    #[test]
    fn every_message_the_channel_composes_is_html_telegram_accepts() {
        let bodies = [
            "plain & simple < text >",
            "- a\n- b\n  - c\n\n1. one\n2. two",
            "| a | b |\n|---|---|\n| 1 | **2** |",
            "> quote with `code` and [x](https://e.com/?a=1&b=2)",
            "Broken **bold and *italic\n\nnew paragraph* end**",
            "<div>raw</div> and <br> and </b> stray",
            "```\nunterminated fence",
            "![](https://e.com/i.png) ![alt](https://e.com/j.png)",
        ];
        for body in bodies {
            for mode in [TelegramChatMode::Private, TelegramChatMode::Forum] {
                for message in compose(&content(Some(body)), mode) {
                    assert_valid_telegram_html(&message);
                }
            }
        }
    }

    #[test]
    fn long_content_is_sent_whole_in_numbered_parts_cut_at_line_breaks() {
        // Nothing is thrown away: the user opened Telegram to read what the session
        // said, and "the rest is on the PC" is what they came to avoid.
        let long = (0..600)
            .map(|i| format!("line {i} with an entity & some text to fill the message"))
            .collect::<Vec<_>>()
            .join("\n");
        let messages = compose(&content(Some(&long)), TelegramChatMode::Private);

        assert!(messages.len() > 3, "{} parts", messages.len());
        let total = messages.len();
        let mut rebuilt = String::new();
        for (index, message) in messages.iter().enumerate() {
            assert!(
                message.chars().count() <= MESSAGE_LIMIT,
                "part {index} is {} chars",
                message.chars().count()
            );
            let header = format!(
                "<b>(Plan &lt;review&gt; &amp; more)</b> <i>(part {} of {total})</i>\n",
                index + 1
            );
            assert!(message.starts_with(&header), "part {index}: {message:.80}");
            // No entity is ever cut in half across a boundary.
            let tail = &message[message.rfind('&').unwrap_or(0)..];
            assert!(tail.contains(';'), "entity split at a part edge: {tail:?}");
            let body = &message[header.len()..];
            // Each part ends where a line ended, not mid-sentence.
            assert!(
                body.ends_with("fill the message"),
                "part {index} ends mid-line: {:?}",
                &body[body.len().saturating_sub(40)..]
            );
            rebuilt.push_str(body);
            rebuilt.push('\n');
        }
        assert!(!messages
            .last()
            .unwrap()
            .contains("more characters not shown"));
        assert_eq!(
            rebuilt.trim_end(),
            escape_html(&long),
            "every line arrived once"
        );
    }

    #[test]
    fn prose_without_line_breaks_is_cut_at_a_sentence_end_and_a_paragraph_break_is_preferred() {
        let sentence = "This sentence fills some of the message with words. ";
        let paragraph = sentence.repeat(30);
        let prose = format!("{paragraph}\n\n{paragraph}\n\n{paragraph}");
        let messages = compose(&content(Some(&prose)), TelegramChatMode::Forum);

        assert!(messages.len() >= 2);
        for message in &messages[..messages.len() - 1] {
            assert!(message.chars().count() <= MESSAGE_LIMIT);
            assert!(
                message.ends_with("words."),
                "a part ends mid-sentence: {:?}",
                &message[message.len().saturating_sub(40)..]
            );
        }
        // One long paragraph with no break at all still cuts between sentences.
        let unbroken = sentence.repeat(120);
        let messages = compose(&content(Some(&unbroken)), TelegramChatMode::Forum);
        assert!(messages.len() >= 2);
        assert!(
            messages[0].ends_with("words."),
            "{:?}",
            &messages[0][messages[0].len() - 40..]
        );
        assert!(messages[0].contains("<i>(part 1 of"));
    }

    #[test]
    fn multibyte_text_is_never_cut_inside_a_character() {
        let long = "🚀é".repeat(5_000);
        let messages = compose(&content(Some(&long)), TelegramChatMode::Forum);
        for message in &messages {
            assert!(std::str::from_utf8(message.as_bytes()).is_ok());
            assert!(message.chars().count() <= MESSAGE_LIMIT);
        }
    }

    #[test]
    fn the_pairing_code_is_found_inside_a_start_command_and_survives_a_deep_link() {
        let code = pairing_code();
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|c| c.is_ascii_alphanumeric()));
        assert!(message_carries_code(&format!("/start {code}"), &code));
        assert!(message_carries_code(&code.to_lowercase(), &code));
        assert!(!message_carries_code("/start OTHER1", &code));

        let link = deep_link("@cst_bot", &code, TelegramChatMode::Forum);
        assert_eq!(
            link,
            format!("https://t.me/cst_bot?startgroup={code}&admin=manage_topics")
        );
        assert_eq!(
            deep_link("cst_bot", &code, TelegramChatMode::Private),
            format!("https://t.me/cst_bot?start={code}")
        );
    }

    #[test]
    fn only_a_private_chat_or_a_forum_supergroup_can_be_paired() {
        assert_eq!(
            chat_mode_of("private", false),
            Some(TelegramChatMode::Private)
        );
        assert_eq!(
            chat_mode_of("supergroup", true),
            Some(TelegramChatMode::Forum)
        );
        // A group without topics has no address per session, so it is refused with a
        // sentence saying what to turn on.
        assert_eq!(chat_mode_of("supergroup", false), None);
        assert_eq!(chat_mode_of("group", false), None);
        assert_eq!(chat_mode_of("channel", false), None);
    }
}
