#![cfg_attr(test, allow(dead_code))]

//! The threads that talk to Telegram, so the UI thread never does.
//!
//! Two of them, because `ureq` is synchronous and a long poll would otherwise make every
//! outgoing message wait behind it:
//!
//! - the **poll loop**, which runs only in the CST holding the poller lock, because
//!   Telegram allows one `getUpdates` consumer per bot and answers a second one with
//!   `409 Conflict`. It writes what arrived into the state file and never touches a
//!   pane: which session can take text is only knowable on the UI thread;
//! - the **outbound drain**, which runs in every CST and sends whatever the UI thread
//!   queued — notifications, refusals, the pairing confirmation — in one place, in order.
//!
//! What the poll loop decides about a batch is a pure function, [`plan_batch`], so the
//! security-relevant decisions can be tested without a network.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use chrono::{DateTime, Utc};
use fs4::{FileExt, TryLockError};

use super::bot::{BotApi, BotError, Update, UreqBot};
use super::markdown;
use super::store::{self, Pairing, Reply, ReplyStatus, TelegramState};
use super::{
    chat_mode_of, classify, compose, escape_html, message_carries_code, route, wording, Content,
    Incoming, Paired, Route, SentKind, SentRecord, TopicRecord, Verdict, POLL_TIMEOUT,
};
use crate::config::TelegramChatMode;
use crate::mux::MuxEvent;

/// How often a CST that is not the poller asks whether it may become the poller.
const LOCK_RETRY: Duration = Duration::from_secs(5);

/// How long to wait after Telegram refused or did not answer. Long enough not to hammer
/// a dead network, short enough that a VPN coming up is noticed within the minute.
const FAILURE_DELAY: Duration = Duration::from_secs(30);

/// What the poll loop needs. Resolved once; a change is a restart of the worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollSettings {
    pub token: String,
    pub api_base: String,
    /// Who may act, from the config. `None` until the wizard has copied a pairing in.
    pub paired: Option<Paired>,
    pub replies: bool,
}

/// One thing to send. Carries its own credentials, resolved by the UI thread from the
/// config in force at that moment, so a settings change never has to restart sending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outbound {
    pub token: String,
    pub api_base: String,
    pub chat_id: i64,
    pub mode: TelegramChatMode,
    pub job: OutboundJob,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutboundJob {
    /// An attention event: find or make the session's topic, send the content in
    /// chunks, remember every message id for reply routing.
    Notify {
        session_id: String,
        content: Content,
    },
    /// Plain words to the user, as a reply when there is something to reply to.
    Say {
        /// Recorded against this session so a reply to it routes there too.
        session_id: Option<String>,
        thread_id: Option<i64>,
        reply_to: Option<i64>,
        text: String,
    },
    /// A thumbs-up on the user's own message: delivery confirmed without a bubble.
    React { message_id: i64 },
    /// The session is gone; its topic is closed, not deleted, so the history stays.
    CloseTopic { session_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundResult {
    pub result: Result<(), String>,
}

#[must_use = "dropping the worker stops its threads"]
pub struct TelegramWorker {
    stop: Arc<AtomicBool>,
    outbound: Sender<Outbound>,
    results: Receiver<OutboundResult>,
    poller: Option<JoinHandle<()>>,
}

impl TelegramWorker {
    /// Start sending, and polling too when `poll` is given.
    pub fn start(root: PathBuf, poll: Option<(Sender<MuxEvent>, PollSettings)>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let (outbound_sender, outbound_receiver) = mpsc::channel::<Outbound>();
        let (result_sender, result_receiver) = mpsc::channel::<OutboundResult>();

        let drain_root = root.clone();
        thread::spawn(move || {
            while let Ok(outbound) = outbound_receiver.recv() {
                let result = perform(&drain_root, &outbound).map_err(|error| error.to_string());
                if result_sender.send(OutboundResult { result }).is_err() {
                    return;
                }
            }
        });

        let poller = poll.map(|(events, settings)| {
            let stop = Arc::clone(&stop);
            let say = outbound_sender.clone();
            thread::spawn(move || poll_loop(events, root, settings, say, stop))
        });

        Self {
            stop,
            outbound: outbound_sender,
            results: result_receiver,
            poller,
        }
    }

    pub fn enqueue(&self, outbound: Outbound) -> Result<(), String> {
        self.outbound
            .send(outbound)
            .map_err(|_| "Telegram worker stopped".to_string())
    }

    pub fn try_result(&self) -> Option<OutboundResult> {
        self.results.try_recv().ok()
    }
}

impl Drop for TelegramWorker {
    /// Asks the poll loop to stop and does not wait for it: a `getUpdates` in flight
    /// cannot be interrupted, and joining would hang quit or a restart for the length of
    /// a poll. The loop checks the flag when the call returns and exits without writing;
    /// its lock file closes with it. The drain thread ends when the last sender is gone.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(poller) = self.poller.take() {
            poller.thread().unpark();
        }
    }
}

/// Everything a batch of updates means, decided before anything is written or sent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Batch {
    pub replies: Vec<Reply>,
    pub says: Vec<OutboundJob>,
    /// A pairing completed by this batch.
    pub paired: Option<Paired>,
    pub next_offset: Option<i64>,
}

/// Decide what a batch of updates means.
///
/// Pure, so the rules can be tested: who may act, what gets a hint, and that an
/// unpaired bot acts on nothing but its pairing code.
pub fn plan_batch(
    updates: &[Update],
    settings: &PollSettings,
    state: &TelegramState,
    now: DateTime<Utc>,
) -> Batch {
    let mut batch = Batch::default();
    let armed = state.armed_code(now).map(str::to_string);
    let mut paired = settings.paired;

    for update in updates {
        batch.next_offset = Some(
            batch
                .next_offset
                .map_or(update.update_id + 1, |next| next.max(update.update_id + 1)),
        );
        let Some(message) = &update.message else {
            continue;
        };
        let text = message.text.as_deref().unwrap_or("").trim();
        let from_id = message.from.as_ref().map(|user| user.id);

        // Pairing is an explicit act and wins over everything else in the batch,
        // including an existing pairing: sending a fresh code is how the user moves
        // the bot from a private chat to a group.
        if let (Some(code), Some(from)) = (armed.as_deref(), from_id) {
            if batch.paired.is_none() && message_carries_code(text, code) {
                match chat_mode_of(&message.chat.kind, message.chat.is_forum) {
                    Some(mode) => {
                        let fresh = Paired {
                            chat_id: message.chat.id,
                            user_id: from,
                            mode,
                        };
                        batch.paired = Some(fresh);
                        paired = Some(fresh);
                        batch.says.push(OutboundJob::Say {
                            session_id: None,
                            thread_id: message.topic_id(),
                            reply_to: Some(message.message_id),
                            text: wording::paired(mode),
                        });
                    }
                    None => batch.says.push(OutboundJob::Say {
                        session_id: None,
                        thread_id: None,
                        reply_to: Some(message.message_id),
                        text: wording::group_without_topics(),
                    }),
                }
                continue;
            }
        }

        let Some(paired) = paired else {
            // Unpaired: nothing anyone sends does anything.
            continue;
        };
        if classify(message.chat.id, from_id, &paired) == Verdict::Ignore {
            continue;
        }
        if text.is_empty() {
            // A sticker, a photo, a voice note. Nothing CST can type into a session.
            batch.says.push(OutboundJob::Say {
                session_id: None,
                thread_id: message.topic_id(),
                reply_to: Some(message.message_id),
                text: "Only text can be delivered to a session.".to_string(),
            });
            continue;
        }
        let incoming = Incoming {
            message_thread_id: message.topic_id(),
            reply_to_message_id: message.reply_to_message.as_ref().map(|r| r.message_id),
        };
        match route(&incoming, paired.mode, &state.sent, &state.topics) {
            Route::Session(session_id) => {
                if !settings.replies {
                    batch.says.push(OutboundJob::Say {
                        session_id: Some(session_id),
                        thread_id: message.topic_id(),
                        reply_to: Some(message.message_id),
                        text: wording::replies_off(),
                    });
                    continue;
                }
                let tool_call_id = incoming.reply_to_message_id.and_then(|reply_to| {
                    state
                        .sent
                        .iter()
                        .find(|record| record.message_id == reply_to)
                        .and_then(|record| record.tool_call_id.clone())
                });
                batch.replies.push(Reply {
                    update_id: update.update_id,
                    user_message_id: message.message_id,
                    session_id,
                    text: text.to_string(),
                    tool_call_id,
                    received_at: now,
                    planned_at: now,
                    queued_told: false,
                    answering: false,
                    status: ReplyStatus::Planned,
                });
            }
            Route::Unrouted => batch.says.push(OutboundJob::Say {
                session_id: None,
                thread_id: message.topic_id(),
                reply_to: Some(message.message_id),
                text: wording::unrouted_hint(paired.mode),
            }),
        }
    }
    batch
}

fn poll_loop(
    events: Sender<MuxEvent>,
    root: PathBuf,
    settings: PollSettings,
    say: Sender<Outbound>,
    stop: Arc<AtomicBool>,
) {
    let bot = UreqBot::new(
        &settings.api_base,
        &settings.token,
        POLL_TIMEOUT + Duration::from_secs(10),
    );
    let mut lock: Option<std::fs::File> = None;
    let mut held_up: Option<String> = None;

    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        if lock.is_none() {
            lock = acquire_poller_lock(&root);
            if lock.is_none() {
                thread::park_timeout(LOCK_RETRY);
                continue;
            }
        }

        let offset = store::load_in(&root).offset;
        let updates = match bot.get_updates(offset, POLL_TIMEOUT) {
            Ok(updates) => updates,
            Err(error) => {
                let reason = describe(&error);
                if held_up.as_deref() != Some(&reason) {
                    let _ = events.send(MuxEvent::TelegramFailed(reason.clone()));
                    held_up = Some(reason);
                }
                thread::park_timeout(FAILURE_DELAY);
                continue;
            }
        };
        // A stopped worker must not write: another worker may already be polling with
        // the settings that replaced ours.
        if stop.load(Ordering::Acquire) {
            return;
        }
        if held_up.take().is_some() {
            let _ = events.send(MuxEvent::TelegramResumed);
        }
        if updates.is_empty() {
            continue;
        }

        // Read again now, not before the poll: while it waited, this or another CST may
        // have sent a notification, and a reply to that message has to find its record.
        let state = store::load_in(&root);
        let now = Utc::now();
        let batch = plan_batch(&updates, &settings, &state, now);
        // Replies and the offset in one write: a crash before it re-fetches the batch
        // with nothing recorded, a crash after it has the offset. The confirmations go
        // out only after, so none is ever sent twice.
        let recorded = store::update_in(&root, |state| {
            let mut recorded = false;
            for reply in &batch.replies {
                recorded |= state.record_reply(reply.clone());
            }
            if let Some(paired) = batch.paired {
                state.pairing = Some(Pairing::Paired(paired));
            }
            if let Some(next) = batch.next_offset {
                state.offset = Some(next);
            }
            recorded
        });
        match recorded {
            Ok(recorded) => {
                if recorded {
                    let _ = events.send(MuxEvent::TelegramRepliesChanged);
                }
                if batch.paired.is_some() {
                    let _ = events.send(MuxEvent::TelegramPaired);
                }
            }
            Err(error) => {
                let _ = events.send(MuxEvent::TelegramFailed(format!(
                    "Telegram could not record what arrived: {error}"
                )));
                continue;
            }
        }
        let chat = batch.paired.or(settings.paired);
        for job in batch.says {
            // A hint for an unpaired chat (the code sent from a group without topics)
            // goes back where it came from; everything else goes to the paired chat.
            let Some(paired) = chat else {
                continue;
            };
            let _ = say.send(Outbound {
                token: settings.token.clone(),
                api_base: settings.api_base.clone(),
                chat_id: paired.chat_id,
                mode: paired.mode,
                job,
            });
        }
    }
}

/// Try to become the poller. The file stays open, and locked, for as long as it is held.
fn acquire_poller_lock(root: &Path) -> Option<std::fs::File> {
    std::fs::create_dir_all(root).ok()?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(store::poller_lock_path(root))
        .ok()?;
    match FileExt::try_lock(&file) {
        Ok(()) => Some(file),
        Err(TryLockError::WouldBlock) => None,
        Err(TryLockError::Error(_)) => None,
    }
}

fn describe(error: &BotError) -> String {
    format!("Telegram: {error}")
}

/// Do one outbound job. Runs on the drain thread.
fn perform(root: &Path, outbound: &Outbound) -> Result<(), BotError> {
    let bot = UreqBot::new(&outbound.api_base, &outbound.token, Duration::from_secs(15));
    perform_with(&bot, root, outbound)
}

/// Send one part of a notification, and send it plain if Telegram refuses its markup.
///
/// The HTML comes from the agent's Markdown through rules checked against Telegram's
/// own, but Telegram's parser is the final word and refuses the whole message when it
/// disagrees. The text, with its tags removed, is what the user came to read; losing
/// it over formatting is the one failure that is not acceptable. Returns the message
/// id and whether the plain copy is what went out.
pub(crate) fn send_part(
    bot: &impl BotApi,
    chat_id: i64,
    thread_id: Option<i64>,
    html: &str,
) -> Result<(i64, bool), BotError> {
    match bot.send_message(chat_id, thread_id, None, html) {
        Err(BotError::Rejected(description))
            if description.to_lowercase().contains("parse entities") =>
        {
            bot.send_message(chat_id, thread_id, None, &markdown::strip_tags(html))
                .map(|id| (id, true))
        }
        other => other.map(|id| (id, false)),
    }
}

fn perform_with(bot: &impl BotApi, root: &Path, outbound: &Outbound) -> Result<(), BotError> {
    match &outbound.job {
        OutboundJob::Notify {
            session_id,
            content,
        } => {
            let thread_id = match outbound.mode {
                TelegramChatMode::Forum => Some(ensure_topic(
                    bot,
                    root,
                    outbound.chat_id,
                    session_id,
                    &content.title,
                )?),
                TelegramChatMode::Private => None,
            };
            let tool_call_id = content
                .question
                .as_ref()
                .map(|question| question.tool_call_id.clone());
            let mut records = Vec::new();
            for chunk in compose(content, outbound.mode) {
                let (message_id, _) = send_part(bot, outbound.chat_id, thread_id, &chunk)?;
                records.push(SentRecord {
                    message_id,
                    session_id: session_id.clone(),
                    kind: Some(SentKind::from(content.kind)),
                    tool_call_id: tool_call_id.clone(),
                    sent_at: Utc::now(),
                });
            }
            let _ = store::update_in(root, |state| {
                for record in records {
                    state.record_sent(record);
                }
            });
            Ok(())
        }
        OutboundJob::Say {
            session_id,
            thread_id,
            reply_to,
            text,
        } => {
            let html = escape_html(text);
            let message_id = bot.send_message(outbound.chat_id, *thread_id, *reply_to, &html)?;
            if let Some(session_id) = session_id {
                let record = SentRecord {
                    message_id,
                    session_id: session_id.clone(),
                    kind: None,
                    tool_call_id: None,
                    sent_at: Utc::now(),
                };
                let _ = store::update_in(root, |state| state.record_sent(record));
            }
            Ok(())
        }
        OutboundJob::React { message_id } => {
            bot.set_message_reaction(outbound.chat_id, *message_id, "👍")
        }
        OutboundJob::CloseTopic { session_id } => {
            let topic = store::update_in(root, |state| state.forget_topic(session_id))
                .ok()
                .flatten();
            if let Some(topic) = topic {
                bot.close_forum_topic(outbound.chat_id, topic.thread_id)?;
            }
            Ok(())
        }
    }
}

/// The topic that stands for a session, made or renamed as needed.
fn ensure_topic(
    bot: &impl BotApi,
    root: &Path,
    chat_id: i64,
    session_id: &str,
    title: &str,
) -> Result<i64, BotError> {
    let existing = store::load_in(root).topics.get(session_id).cloned();
    let name = super::bot::topic_name(title);
    match existing {
        Some(topic) if topic.title == name => Ok(topic.thread_id),
        Some(topic) => {
            // A failed rename is not worth losing the message over.
            let _ = bot.edit_forum_topic(chat_id, topic.thread_id, &name);
            let _ = store::update_in(root, |state| {
                if let Some(record) = state.topics.get_mut(session_id) {
                    record.title = name.clone();
                }
            });
            Ok(topic.thread_id)
        }
        None => {
            let thread_id = bot.create_forum_topic(chat_id, &name)?;
            let _ = store::update_in(root, |state| {
                state.topics.insert(
                    session_id.to_string(),
                    TopicRecord {
                        thread_id,
                        title: name.clone(),
                    },
                );
            });
            Ok(thread_id)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telegram::bot::{Chat, Message, RepliedTo, User};

    fn settings(paired: Option<Paired>) -> PollSettings {
        PollSettings {
            token: "1:x".to_string(),
            api_base: "http://127.0.0.1:9".to_string(),
            paired,
            replies: true,
        }
    }

    fn paired() -> Paired {
        Paired {
            chat_id: -100,
            user_id: 7,
            mode: TelegramChatMode::Forum,
        }
    }

    fn update(id: i64, chat_id: i64, from: i64, text: &str) -> Update {
        Update {
            update_id: id,
            message: Some(Message {
                message_id: id * 10,
                chat: Chat {
                    id: chat_id,
                    kind: if chat_id < 0 { "supergroup" } else { "private" }.to_string(),
                    is_forum: chat_id < 0,
                },
                from: Some(User {
                    id: from,
                    is_bot: false,
                }),
                text: Some(text.to_string()),
                message_thread_id: None,
                is_topic_message: false,
                reply_to_message: None,
            }),
        }
    }

    fn in_topic(mut update: Update, thread_id: i64) -> Update {
        if let Some(message) = update.message.as_mut() {
            message.message_thread_id = Some(thread_id);
            message.is_topic_message = true;
        }
        update
    }

    fn replying(mut update: Update, to: i64) -> Update {
        if let Some(message) = update.message.as_mut() {
            message.reply_to_message = Some(RepliedTo { message_id: to });
        }
        update
    }

    #[test]
    fn an_unpaired_bot_acts_on_nothing_but_its_pairing_code() {
        let now = Utc::now();
        let mut state = TelegramState::default();
        state.pairing = Some(Pairing::Armed {
            code: "ABC234".to_string(),
            expires_at: now + chrono::Duration::minutes(5),
        });
        let updates = vec![
            update(1, 5, 7, "hello there"),
            update(2, 5, 7, "/start ABC234"),
            update(3, 5, 7, "a message after pairing"),
        ];

        let batch = plan_batch(&updates, &settings(None), &state, now);

        assert_eq!(
            batch.paired,
            Some(Paired {
                chat_id: 5,
                user_id: 7,
                mode: TelegramChatMode::Private
            })
        );
        assert_eq!(batch.next_offset, Some(4));
        // The first message predates pairing and is nothing; the third is from the
        // newly paired user but replies to nothing, so it gets the hint.
        assert!(batch.replies.is_empty());
        assert_eq!(batch.says.len(), 2);
        assert!(matches!(&batch.says[0], OutboundJob::Say { text, .. } if text.contains("paired")));
        assert!(
            matches!(&batch.says[1], OutboundJob::Say { text, .. } if text.contains("Reply to a session"))
        );
    }

    #[test]
    fn the_code_sent_from_a_group_without_topics_is_refused_with_what_to_turn_on() {
        let now = Utc::now();
        let mut state = TelegramState::default();
        state.pairing = Some(Pairing::Armed {
            code: "ABC234".to_string(),
            expires_at: now + chrono::Duration::minutes(5),
        });
        let mut plain_group = update(1, -200, 7, "ABC234");
        plain_group.message.as_mut().unwrap().chat.is_forum = false;

        let batch = plan_batch(&[plain_group], &settings(None), &state, now);

        assert_eq!(batch.paired, None);
        assert!(
            matches!(&batch.says[0], OutboundJob::Say { text, .. } if text.contains("Topics turned on"))
        );
    }

    #[test]
    fn an_expired_code_pairs_nobody() {
        let now = Utc::now();
        let mut state = TelegramState::default();
        state.pairing = Some(Pairing::Armed {
            code: "ABC234".to_string(),
            expires_at: now - chrono::Duration::seconds(1),
        });
        let batch = plan_batch(&[update(1, 5, 7, "ABC234")], &settings(None), &state, now);
        assert_eq!(batch.paired, None);
        assert!(batch.says.is_empty());
    }

    #[test]
    fn only_the_paired_user_in_the_paired_chat_produces_a_reply_and_strangers_get_silence() {
        let now = Utc::now();
        let mut state = TelegramState::default();
        state.topics.insert(
            "session-a".to_string(),
            TopicRecord {
                thread_id: 300,
                title: "A".to_string(),
            },
        );
        let updates = vec![
            in_topic(update(1, -100, 7, "go ahead"), 300),
            in_topic(update(2, -100, 8, "stranger in the group"), 300),
            in_topic(update(3, -999, 7, "paired user, other chat"), 300),
        ];

        let batch = plan_batch(&updates, &settings(Some(paired())), &state, now);

        assert_eq!(batch.replies.len(), 1);
        assert_eq!(batch.replies[0].session_id, "session-a");
        assert_eq!(batch.replies[0].text, "go ahead");
        assert_eq!(batch.replies[0].user_message_id, 10);
        assert!(batch.says.is_empty(), "a stranger is not even answered");
        assert_eq!(batch.next_offset, Some(4));
    }

    #[test]
    fn a_reply_to_a_question_carries_that_questions_id_and_general_gets_a_hint() {
        let now = Utc::now();
        let mut state = TelegramState::default();
        state.record_sent(SentRecord {
            message_id: 55,
            session_id: "session-q".to_string(),
            kind: Some(SentKind::Question),
            tool_call_id: Some("call-1".to_string()),
            sent_at: now,
        });
        let updates = vec![
            replying(update(1, -100, 7, "the second one"), 55),
            update(2, -100, 7, "typed into General"),
        ];

        let batch = plan_batch(&updates, &settings(Some(paired())), &state, now);

        assert_eq!(batch.replies.len(), 1);
        assert_eq!(batch.replies[0].tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(batch.says.len(), 1);
        assert!(
            matches!(&batch.says[0], OutboundJob::Say { text, reply_to: Some(20), .. } if text.contains("topic"))
        );
    }

    #[test]
    fn with_replies_off_a_routed_message_is_told_so_instead_of_being_recorded() {
        let now = Utc::now();
        let mut state = TelegramState::default();
        state.record_sent(SentRecord {
            message_id: 55,
            session_id: "s".to_string(),
            kind: Some(SentKind::Ready),
            tool_call_id: None,
            sent_at: now,
        });
        let mut settings = settings(Some(paired()));
        settings.replies = false;

        let batch = plan_batch(
            &[replying(update(1, -100, 7, "hi"), 55)],
            &settings,
            &state,
            now,
        );

        assert!(batch.replies.is_empty());
        assert!(
            matches!(&batch.says[0], OutboundJob::Say { text, .. } if text.contains("turned off"))
        );
    }

    #[test]
    fn a_batch_with_no_messages_still_advances_the_offset() {
        let updates = vec![Update {
            update_id: 9,
            message: None,
        }];
        let batch = plan_batch(
            &updates,
            &settings(Some(paired())),
            &TelegramState::default(),
            Utc::now(),
        );
        assert_eq!(batch.next_offset, Some(10));
        assert!(batch.replies.is_empty() && batch.says.is_empty());
    }

    #[test]
    fn a_part_telegram_refuses_to_parse_is_sent_again_as_plain_text() {
        use std::cell::RefCell;

        // Telegram's parser is the final word on markup; the text must arrive even
        // when it disagrees with ours, so the second attempt carries no tags at all.
        struct Picky {
            sent: RefCell<Vec<String>>,
        }
        impl BotApi for Picky {
            fn get_me(&self) -> Result<super::super::bot::BotIdentity, BotError> {
                unreachable!()
            }
            fn get_updates(&self, _: Option<i64>, _: Duration) -> Result<Vec<Update>, BotError> {
                unreachable!()
            }
            fn send_message(
                &self,
                _: i64,
                _: Option<i64>,
                _: Option<i64>,
                html: &str,
            ) -> Result<i64, BotError> {
                self.sent.borrow_mut().push(html.to_string());
                if html.contains('<') {
                    return Err(BotError::Rejected(
                        "Bad Request: can't parse entities: Unsupported start tag".to_string(),
                    ));
                }
                Ok(self.sent.borrow().len() as i64)
            }
            fn create_forum_topic(&self, _: i64, _: &str) -> Result<i64, BotError> {
                unreachable!()
            }
            fn edit_forum_topic(&self, _: i64, _: i64, _: &str) -> Result<(), BotError> {
                unreachable!()
            }
            fn close_forum_topic(&self, _: i64, _: i64) -> Result<(), BotError> {
                unreachable!()
            }
            fn get_chat_member(
                &self,
                _: i64,
                _: i64,
            ) -> Result<super::super::bot::ChatMember, BotError> {
                unreachable!()
            }
            fn set_message_reaction(&self, _: i64, _: i64, _: &str) -> Result<(), BotError> {
                unreachable!()
            }
        }

        let bot = Picky {
            sent: RefCell::new(Vec::new()),
        };
        let sent = send_part(
            &bot,
            -100,
            None,
            "<b>Done</b> &amp; tested\n• <code>x</code>",
        )
        .expect("the plain retry is accepted");
        assert_eq!(sent, (2, true));
        assert_eq!(
            bot.sent.borrow().as_slice(),
            [
                "<b>Done</b> &amp; tested\n• <code>x</code>",
                "Done &amp; tested\n• x"
            ]
        );

        // Any other refusal is reported as it is, not retried.
        struct Down;
        impl BotApi for Down {
            fn get_me(&self) -> Result<super::super::bot::BotIdentity, BotError> {
                unreachable!()
            }
            fn get_updates(&self, _: Option<i64>, _: Duration) -> Result<Vec<Update>, BotError> {
                unreachable!()
            }
            fn send_message(
                &self,
                _: i64,
                _: Option<i64>,
                _: Option<i64>,
                _: &str,
            ) -> Result<i64, BotError> {
                Err(BotError::Rejected(
                    "Bad Request: chat not found".to_string(),
                ))
            }
            fn create_forum_topic(&self, _: i64, _: &str) -> Result<i64, BotError> {
                unreachable!()
            }
            fn edit_forum_topic(&self, _: i64, _: i64, _: &str) -> Result<(), BotError> {
                unreachable!()
            }
            fn close_forum_topic(&self, _: i64, _: i64) -> Result<(), BotError> {
                unreachable!()
            }
            fn get_chat_member(
                &self,
                _: i64,
                _: i64,
            ) -> Result<super::super::bot::ChatMember, BotError> {
                unreachable!()
            }
            fn set_message_reaction(&self, _: i64, _: i64, _: &str) -> Result<(), BotError> {
                unreachable!()
            }
        }
        assert_eq!(
            send_part(&Down, -100, None, "<b>x</b>"),
            Err(BotError::Rejected(
                "Bad Request: chat not found".to_string()
            ))
        );
    }

    #[test]
    fn only_one_process_holds_the_poller_lock_at_a_time() {
        let temp = tempfile::tempdir().unwrap();
        let first = acquire_poller_lock(temp.path());
        assert!(first.is_some());
        // Same process, second handle: the OS lock is advisory per open file on Unix
        // and mandatory on Windows; fs4 reports both as busy.
        let second = acquire_poller_lock(temp.path());
        assert!(
            second.is_none(),
            "a second poller would get 409 from Telegram"
        );
        drop(first);
        assert!(acquire_poller_lock(temp.path()).is_some());
    }
}
