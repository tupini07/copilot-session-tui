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

use super::bot::{BotApi, BotError, Button, Update, UreqBot};
use super::bot_commands;
use super::follow::{self, EndReason, FollowCommand, FollowView, LogTail};
use super::markdown;
use super::queue::{self, CancelSource, QueuedEntry};
use super::store::{self, Follow, Pairing, Reply, ReplyStatus, TelegramState};
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
    /// Whether session text may go to Telegram at all. A follow shows the session's
    /// words, so under "Status only" it is refused rather than honoured.
    pub content_full: bool,
    /// Where Copilot keeps session logs, which a follow reads.
    pub copilot_home: PathBuf,
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
    /// Words with buttons under them. `queued_for` names the reply a "Queued" notice
    /// is about, so its message id is kept for the "Delivered" edit later.
    Notice {
        session_id: Option<String>,
        thread_id: Option<i64>,
        reply_to: Option<i64>,
        text: String,
        buttons: Vec<Button>,
        queued_for: Option<i64>,
    },
    /// A line about a session, from CST rather than from the session: in a forum it
    /// goes in the session's topic, made first if this is the session's first word.
    /// `Say` cannot do that, since it takes the topic as given.
    Tell {
        session_id: String,
        /// Names the topic if one has to be made.
        title: String,
        text: String,
    },
    /// Replace a message's text; its buttons go with the old text.
    Edit { message_id: i64, html: String },
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
    /// `/follow`s to start or extend. Serviced by the poll loop, never typed anywhere.
    pub follows: Vec<Follow>,
    /// `/unfollow`s: the session, and where to answer if nothing was being followed.
    pub unfollows: Vec<FollowEnd>,
    /// Cancel buttons tapped by the paired user.
    pub cancels: Vec<Cancel>,
    /// Taps to acknowledge and otherwise ignore: a stranger's, or a button CST does not
    /// know. Unanswered, a button spins on the phone.
    pub ignored_taps: Vec<String>,
    /// `/queue`s: which session, or every session when `None`, and where to answer.
    pub queue_requests: Vec<QueueRequest>,
}

/// A tap on a Cancel button, as planned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cancel {
    pub callback_id: String,
    /// The reply to take back.
    pub update_id: i64,
    pub source: CancelSource,
    /// The message the button sat under, which the answer edits.
    pub message_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueRequest {
    pub session_id: Option<String>,
    pub thread_id: Option<i64>,
    pub reply_to: i64,
}

/// A `/unfollow` as planned: which session, and where it was said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowEnd {
    pub session_id: String,
    pub thread_id: Option<i64>,
    pub user_message_id: i64,
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
        // A tap is not a message, and is handled first or the `else` below would
        // swallow it. Only the paired user's taps act; everyone else's are answered
        // with nothing, so the button stops spinning, and dropped — the same rule as
        // for words.
        if let Some(tap) = &update.callback_query {
            let chat_id = tap.message.as_ref().map(|message| message.chat.id);
            let acts = match (paired, chat_id) {
                (Some(paired), Some(chat_id)) => {
                    classify(chat_id, Some(tap.from.id), &paired) == Verdict::Act
                }
                _ => false,
            };
            let cancel = tap
                .data
                .as_deref()
                .and_then(queue::parse_cancel_data)
                .filter(|_| acts);
            match cancel {
                Some((update_id, source)) => batch.cancels.push(Cancel {
                    callback_id: tap.id.clone(),
                    update_id,
                    source,
                    message_id: tap.message.as_ref().map(|message| message.message_id),
                }),
                None => batch.ignored_taps.push(tap.id.clone()),
            }
            continue;
        }
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
        // `/queue` is taken before routing: sent to a session it lists that session's
        // queue, sent anywhere else it lists everything, where a reply would get the
        // "which session?" hint.
        if queue::is_queue_command(text) {
            let session_id = match route(&incoming, paired.mode, &state.sent, &state.topics) {
                Route::Session(session_id) => Some(session_id),
                Route::Unrouted => None,
            };
            batch.queue_requests.push(QueueRequest {
                session_id,
                thread_id: message.topic_id(),
                reply_to: message.message_id,
            });
            continue;
        }
        match route(&incoming, paired.mode, &state.sent, &state.topics) {
            Route::Session(session_id) => {
                // CST's own commands are acted on here and never typed into the
                // session. Copilot has slash commands too, so only these are taken.
                match follow::parse_command(text) {
                    Some(FollowCommand::Follow { minutes }) => {
                        if !settings.content_full {
                            batch.says.push(OutboundJob::Say {
                                session_id: Some(session_id),
                                thread_id: message.topic_id(),
                                reply_to: Some(message.message_id),
                                text: follow::wording::needs_full_content(),
                            });
                            continue;
                        }
                        batch.follows.push(Follow {
                            session_id,
                            chat_id: paired.chat_id,
                            thread_id: message.topic_id(),
                            user_message_id: message.message_id,
                            message_id: None,
                            started_at: now,
                            expires_at: now + chrono::Duration::minutes(i64::from(minutes)),
                            last_hash: None,
                            last_edit_at: None,
                            stop_requested: false,
                        });
                        continue;
                    }
                    Some(FollowCommand::Unfollow) => {
                        batch.unfollows.push(FollowEnd {
                            session_id,
                            thread_id: message.topic_id(),
                            user_message_id: message.message_id,
                        });
                        continue;
                    }
                    None => {}
                }
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
                    queued_message_id: None,
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
    let mut menu_published = false;

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
        // The menu is a property of the bot, so the one CST that polls publishes it,
        // once, rather than every instance on every start. A failure here is not worth
        // a status line: the commands still work untyped, and the next pass retries.
        if !menu_published {
            menu_published = bot.set_my_commands(bot_commands()).is_ok();
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
        // Every return from the long poll, with or without updates, is the cadence
        // the live views are edited at: at most every ten seconds.
        service_follows(&root, &bot, &settings, Utc::now());
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
            let mut refused: Vec<OutboundJob> = Vec::new();
            for reply in &batch.replies {
                recorded |= state.record_reply(reply.clone());
            }
            for follow in &batch.follows {
                if !state.start_follow(follow.clone()) {
                    refused.push(OutboundJob::Say {
                        session_id: Some(follow.session_id.clone()),
                        thread_id: follow.thread_id,
                        reply_to: Some(follow.user_message_id),
                        text: follow::wording::too_many_follows(follow::MAX_FOLLOWS),
                    });
                }
            }
            for end in &batch.unfollows {
                match state.follow_mut(&end.session_id) {
                    Some(running) => running.stop_requested = true,
                    None => refused.push(OutboundJob::Say {
                        session_id: Some(end.session_id.clone()),
                        thread_id: end.thread_id,
                        reply_to: Some(end.user_message_id),
                        text: follow::wording::nothing_to_unfollow(),
                    }),
                }
            }
            // A cancel takes the reply out only while it is still waiting. Delivery
            // claims a reply under this same lock before typing it, so one of the two
            // always sees the other's mark and nothing is both typed and taken back.
            let mut cancelled: Vec<(Cancel, CancelOutcome)> = Vec::new();
            for cancel in &batch.cancels {
                let found = state
                    .reply_mut(cancel.update_id)
                    .map(|reply| (reply.status, reply.session_id.clone()));
                let outcome = match found {
                    Some((ReplyStatus::Planned, session_id)) => {
                        state.drop_reply(cancel.update_id);
                        recorded = true;
                        CancelOutcome::Cancelled { session_id }
                    }
                    Some(_) => CancelOutcome::AlreadyDelivered,
                    None => CancelOutcome::Gone,
                };
                cancelled.push((cancel.clone(), outcome));
            }
            if let Some(paired) = batch.paired {
                state.pairing = Some(Pairing::Paired(paired));
            }
            if let Some(next) = batch.next_offset {
                state.offset = Some(next);
            }
            (recorded, refused, cancelled)
        });
        let refused = match recorded {
            Ok((recorded, refused, cancelled)) => {
                if recorded {
                    let _ = events.send(MuxEvent::TelegramRepliesChanged);
                }
                if batch.paired.is_some() {
                    let _ = events.send(MuxEvent::TelegramPaired);
                }
                answer_taps(&root, &bot, &settings, &cancelled, &batch.ignored_taps);
                refused
            }
            Err(error) => {
                let _ = events.send(MuxEvent::TelegramFailed(format!(
                    "Telegram could not record what arrived: {error}"
                )));
                continue;
            }
        };
        // A follow asked for just now is shown at once, not after the next poll.
        if !batch.follows.is_empty() || !batch.unfollows.is_empty() {
            service_follows(&root, &bot, &settings, Utc::now());
        }
        for request in &batch.queue_requests {
            send_queue_listing(&root, &bot, &settings, request);
        }
        let chat = batch.paired.or(settings.paired);
        for job in batch.says.into_iter().chain(refused) {
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
/// Bring every live view up to date, and end the ones that are over.
///
/// Runs in the poll loop, so only the CST holding the poller lock edits, and a
/// takeover carries on with the same message ids from the state file. Each follow is
/// re-rendered from the end of its session's log; an unchanged rendering is not sent,
/// and a changed one no more often than [`follow::EDIT_EVERY`]. A view that has
/// expired, was told to stop, or whose session is gone gets one last edit that says
/// so, and is forgotten. A failed edit is simply tried again next time: the deadline
/// bounds how long that can go on.
fn service_follows(root: &Path, bot: &impl BotApi, settings: &PollSettings, now: DateTime<Utc>) {
    let follows = store::load_in(root).follows;
    if follows.is_empty() {
        return;
    }
    let forum = settings
        .paired
        .is_some_and(|paired| paired.mode == TelegramChatMode::Forum);
    for current in follows {
        let session_dir = settings
            .copilot_home
            .join("session-state")
            .join(&current.session_id);
        let session_gone = !session_dir.is_dir();
        let lines = read_tail_lines(&session_dir.join("events.jsonl"), follow::TAIL_BYTES);
        let tail = LogTail::from_lines(lines.iter().map(String::as_str));
        let turn_running = turn_in_progress(&session_dir, &tail, now);
        // In a forum the topic already names the session.
        let title = (!forum).then(|| session_title(&session_dir, &current.session_id));
        let ends_in = (current.expires_at - now)
            .to_std()
            .unwrap_or(Duration::ZERO);
        let view = FollowView {
            title: title.as_deref(),
            turn_running,
            ends_in,
            tail: &tail,
        };

        let ending = if current.stop_requested {
            Some(EndReason::Stopped)
        } else if session_gone {
            Some(EndReason::SessionClosed)
        } else if now >= current.expires_at {
            Some(EndReason::Expired)
        } else {
            None
        };
        if let Some(reason) = ending {
            let html = follow::render_ended(&view, reason);
            let sent = match current.message_id {
                Some(id) => bot.edit_message_text(current.chat_id, id, &html),
                None => bot
                    .send_message(
                        current.chat_id,
                        current.thread_id,
                        Some(current.user_message_id),
                        &html,
                    )
                    .map(|_| ()),
            };
            if sent.is_ok() {
                let _ = store::update_in(root, |state| state.drop_follow(&current.session_id));
            }
            continue;
        }

        let recently_edited = current
            .last_edit_at
            .is_some_and(|at| (now - at).to_std().unwrap_or(Duration::ZERO) < follow::EDIT_EVERY);
        if current.message_id.is_some() && recently_edited {
            continue;
        }
        let html = follow::render(&view);
        let hash = follow::content_hash(&html);
        if current.message_id.is_some() && current.last_hash == Some(hash) {
            continue;
        }
        let sent = match current.message_id {
            Some(id) => bot
                .edit_message_text(current.chat_id, id, &html)
                .map(|_| id),
            None => bot.send_message(
                current.chat_id,
                current.thread_id,
                Some(current.user_message_id),
                &html,
            ),
        };
        if let Ok(message_id) = sent {
            let _ = store::update_in(root, |state| {
                if let Some(live) = state.follow_mut(&current.session_id) {
                    live.message_id = Some(message_id);
                    live.last_hash = Some(hash);
                    live.last_edit_at = Some(now);
                }
            });
        }
    }
}

/// What a Cancel tap found when it looked for its reply.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CancelOutcome {
    Cancelled { session_id: String },
    AlreadyDelivered,
    Gone,
}

/// Acknowledge every tap, and turn each Cancel's message into what it now means.
///
/// A tap on a "Queued" notice makes that notice say "Cancelled"; a tap on a listing
/// redraws the listing without the reply, so the other Cancels on it still work. A
/// tap that found nothing to cancel is only told so, in the small notice on the phone,
/// because its message already says what happened.
fn answer_taps(
    root: &Path,
    bot: &impl BotApi,
    settings: &PollSettings,
    cancelled: &[(Cancel, CancelOutcome)],
    ignored: &[String],
) {
    for callback_id in ignored {
        let _ = bot.answer_callback_query(callback_id, None);
    }
    let Some(paired) = settings.paired else {
        return;
    };
    for (cancel, outcome) in cancelled {
        let toast = match outcome {
            CancelOutcome::Cancelled { .. } => queue::wording::toast_cancelled(),
            CancelOutcome::AlreadyDelivered => queue::wording::toast_already_delivered(),
            CancelOutcome::Gone => queue::wording::toast_gone(),
        };
        let _ = bot.answer_callback_query(&cancel.callback_id, Some(toast));
        let Some(message_id) = cancel.message_id else {
            continue;
        };
        match (cancel.source, outcome) {
            (CancelSource::Notice, CancelOutcome::Cancelled { session_id }) => {
                let title = session_title(
                    &settings.copilot_home.join("session-state").join(session_id),
                    session_id,
                );
                let html = escape_html(&queue::wording::cancelled(&title));
                let _ = bot.edit_message_text(paired.chat_id, message_id, &html);
            }
            (CancelSource::Listing, _) => {
                let (html, buttons) = queue_listing(root, settings, None);
                // An edit cannot carry buttons through this API, so a listing with
                // entries left is re-sent whole and the old one closed off.
                if buttons.is_empty() {
                    let _ = bot.edit_message_text(paired.chat_id, message_id, &html);
                } else {
                    let _ = bot.edit_message_text(
                        paired.chat_id,
                        message_id,
                        &escape_html("Updated below."),
                    );
                    let _ =
                        bot.send_message_with_buttons(paired.chat_id, None, None, &html, &buttons);
                }
            }
            _ => {}
        }
    }
}

/// Send the answer to a `/queue`: what is waiting, each line with its own Cancel.
fn send_queue_listing(
    root: &Path,
    bot: &impl BotApi,
    settings: &PollSettings,
    request: &QueueRequest,
) {
    let Some(paired) = settings.paired else {
        return;
    };
    let (html, buttons) = queue_listing(root, settings, request.session_id.as_deref());
    let _ = bot.send_message_with_buttons(
        paired.chat_id,
        request.thread_id,
        Some(request.reply_to),
        &html,
        &buttons,
    );
}

/// The queue as it stands, for one session or for all, rendered with its buttons.
fn queue_listing(
    root: &Path,
    settings: &PollSettings,
    session_id: Option<&str>,
) -> (String, Vec<Button>) {
    let state = store::load_in(root);
    let waiting: Vec<&Reply> = state
        .replies
        .iter()
        .filter(|reply| reply.status == ReplyStatus::Planned)
        .filter(|reply| session_id.is_none_or(|wanted| reply.session_id == wanted))
        .collect();
    let titles: Vec<String> = waiting
        .iter()
        .map(|reply| {
            session_title(
                &settings
                    .copilot_home
                    .join("session-state")
                    .join(&reply.session_id),
                &reply.session_id,
            )
        })
        .collect();
    let entries: Vec<QueuedEntry<'_>> = waiting
        .iter()
        .zip(titles.iter())
        .map(|(reply, title)| QueuedEntry { title, reply })
        .collect();
    queue::render_listing(&entries, Utc::now())
}

/// The last `bytes` of a file as whole lines, oldest first. The first line is dropped
/// when the read began mid-line, so nothing half a record long is parsed.
fn read_tail_lines(path: &Path, bytes: u64) -> Vec<String> {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let Ok(length) = file.metadata().map(|m| m.len()) else {
        return Vec::new();
    };
    let start = length.saturating_sub(bytes);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut text = String::new();
    if file.read_to_string(&mut text).is_err() {
        return Vec::new();
    }
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    lines
}

/// Whether a turn is running: what the lifecycle hook last said, or failing a journal,
/// whether a tool is open or the log grew in the last few seconds.
fn turn_in_progress(session_dir: &Path, tail: &LogTail, now: DateTime<Utc>) -> bool {
    let journal = read_tail_lines(&session_dir.join(".cst-lifecycle.jsonl"), 4096);
    if let Some(last) = journal.last() {
        return last.contains("\"working\"");
    }
    !tail.running.is_empty()
        || tail
            .last_event_at
            .is_some_and(|at| (now - at).num_seconds() < 20)
}

/// The session's name as CST shows it, from its workspace record; the id's start when
/// there is none to read.
fn session_title(session_dir: &Path, session_id: &str) -> String {
    std::fs::read_to_string(session_dir.join("workspace.yaml"))
        .ok()
        .and_then(|yaml| serde_yaml::from_str::<serde_yaml::Value>(&yaml).ok())
        .and_then(|value| value["summary"].as_str().map(str::trim).map(str::to_string))
        .filter(|summary| !summary.is_empty())
        .unwrap_or_else(|| session_id.chars().take(8).collect())
}

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
        OutboundJob::Tell {
            session_id,
            title,
            text,
        } => {
            let thread_id = match outbound.mode {
                TelegramChatMode::Forum => Some(ensure_topic(
                    bot,
                    root,
                    outbound.chat_id,
                    session_id,
                    title,
                )?),
                TelegramChatMode::Private => None,
            };
            let message_id =
                bot.send_message(outbound.chat_id, thread_id, None, &escape_html(text))?;
            let record = SentRecord {
                message_id,
                session_id: session_id.clone(),
                kind: None,
                tool_call_id: None,
                sent_at: Utc::now(),
            };
            let _ = store::update_in(root, |state| state.record_sent(record));
            Ok(())
        }
        OutboundJob::Notice {
            session_id,
            thread_id,
            reply_to,
            text,
            buttons,
            queued_for,
        } => {
            let html = escape_html(text);
            let message_id = bot.send_message_with_buttons(
                outbound.chat_id,
                *thread_id,
                *reply_to,
                &html,
                buttons,
            )?;
            let _ = store::update_in(root, |state| {
                if let Some(session_id) = session_id {
                    state.record_sent(SentRecord {
                        message_id,
                        session_id: session_id.clone(),
                        kind: None,
                        tool_call_id: None,
                        sent_at: Utc::now(),
                    });
                }
                if let Some(update_id) = queued_for {
                    if let Some(reply) = state.reply_mut(*update_id) {
                        reply.queued_message_id = Some(message_id);
                    }
                }
            });
            Ok(())
        }
        OutboundJob::Edit { message_id, html } => {
            bot.edit_message_text(outbound.chat_id, *message_id, html)
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
    use crate::telegram::bot::{CallbackQuery, Chat, Message, RepliedTo, User};

    #[test]
    fn a_line_from_cst_about_a_session_opens_its_topic_if_need_be_and_routes_replies_back() {
        use std::cell::RefCell;

        struct Recorder {
            topics: RefCell<Vec<String>>,
            sent: RefCell<Vec<(Option<i64>, String)>>,
        }
        impl BotApi for Recorder {
            fn set_my_commands(&self, _: &[super::super::bot::BotCommand]) -> Result<(), BotError> {
                Ok(())
            }
            fn create_forum_topic(&self, _: i64, name: &str) -> Result<i64, BotError> {
                self.topics.borrow_mut().push(name.to_string());
                Ok(42)
            }
            fn send_message(
                &self,
                _: i64,
                thread_id: Option<i64>,
                _: Option<i64>,
                html: &str,
            ) -> Result<i64, BotError> {
                self.sent.borrow_mut().push((thread_id, html.to_string()));
                Ok(7)
            }
            fn get_me(&self) -> Result<super::super::bot::BotIdentity, BotError> {
                unreachable!()
            }
            fn get_updates(&self, _: Option<i64>, _: Duration) -> Result<Vec<Update>, BotError> {
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
            fn edit_message_text(&self, _: i64, _: i64, _: &str) -> Result<(), BotError> {
                unreachable!()
            }
            fn send_message_with_buttons(
                &self,
                _: i64,
                _: Option<i64>,
                _: Option<i64>,
                _: &str,
                _: &[Button],
            ) -> Result<i64, BotError> {
                unreachable!()
            }
            fn answer_callback_query(&self, _: &str, _: Option<&str>) -> Result<(), BotError> {
                unreachable!()
            }
        }

        let temp = tempfile::tempdir().unwrap();
        let bot = Recorder {
            topics: RefCell::new(Vec::new()),
            sent: RefCell::new(Vec::new()),
        };
        // A session that has never notified has no topic yet; the line about it is
        // its first word in the group and must not land in General.
        let outbound = Outbound {
            token: "1:x".to_string(),
            api_base: "http://127.0.0.1:9".to_string(),
            chat_id: -100,
            mode: TelegramChatMode::Forum,
            job: OutboundJob::Tell {
                session_id: "s1".to_string(),
                title: "Watched session".to_string(),
                text: wording::thread_wake("Watched session", "https://github.com/o/r/issues/7"),
            },
        };

        perform_with(&bot, temp.path(), &outbound).unwrap();

        assert!(bot.topics.borrow()[0].contains("Watched session"));
        let sent = bot.sent.borrow();
        assert_eq!(sent[0].0, Some(42), "sent into the topic just made");
        assert!(sent[0].1.contains("https://github.com/o/r/issues/7"));
        // Recorded against the session, so a reply to this line reaches it.
        let state = store::load_in(temp.path());
        assert!(state
            .sent
            .iter()
            .any(|record| record.message_id == 7 && record.session_id == "s1"));
    }

    fn settings(paired: Option<Paired>) -> PollSettings {
        PollSettings {
            content_full: true,
            copilot_home: PathBuf::new(),
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
            callback_query: None,
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
            callback_query: None,
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
            fn edit_message_text(&self, _: i64, _: i64, _: &str) -> Result<(), BotError> {
                unreachable!()
            }
            fn send_message_with_buttons(
                &self,
                _: i64,
                _: Option<i64>,
                _: Option<i64>,
                _: &str,
                _: &[super::super::bot::Button],
            ) -> Result<i64, BotError> {
                unreachable!()
            }
            fn answer_callback_query(&self, _: &str, _: Option<&str>) -> Result<(), BotError> {
                unreachable!()
            }
            fn set_my_commands(&self, _: &[super::super::bot::BotCommand]) -> Result<(), BotError> {
                Ok(())
            }
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
            fn edit_message_text(&self, _: i64, _: i64, _: &str) -> Result<(), BotError> {
                unreachable!()
            }
            fn send_message_with_buttons(
                &self,
                _: i64,
                _: Option<i64>,
                _: Option<i64>,
                _: &str,
                _: &[super::super::bot::Button],
            ) -> Result<i64, BotError> {
                unreachable!()
            }
            fn answer_callback_query(&self, _: &str, _: Option<&str>) -> Result<(), BotError> {
                unreachable!()
            }
            fn set_my_commands(&self, _: &[super::super::bot::BotCommand]) -> Result<(), BotError> {
                Ok(())
            }
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
    fn a_follow_is_recorded_as_a_follow_and_never_typed_while_copilots_own_slash_commands_still_are(
    ) {
        use crate::notifications::NotificationKind;

        let now = Utc::now();
        let mut state = TelegramState::default();
        state.record_sent(SentRecord {
            message_id: 50,
            session_id: "s1".to_string(),
            kind: Some(SentKind::from(NotificationKind::Ready)),
            tool_call_id: None,
            sent_at: now,
        });
        let private = Paired {
            chat_id: 7,
            user_id: 7,
            mode: TelegramChatMode::Private,
        };
        let updates = vec![
            replying(update(1, 7, 7, "/follow 10"), 50),
            // Copilot's own command: not CST's to take, so it is a reply like any other.
            replying(update(2, 7, 7, "/compact"), 50),
        ];

        let batch = plan_batch(&updates, &settings(Some(private)), &state, now);
        assert_eq!(batch.follows.len(), 1);
        assert_eq!(batch.follows[0].session_id, "s1");
        assert_eq!(batch.follows[0].user_message_id, 10);
        assert_eq!(
            batch.follows[0].expires_at,
            now + chrono::Duration::minutes(10)
        );
        assert_eq!(batch.replies.len(), 1, "/compact goes to the session");
        assert_eq!(batch.replies[0].text, "/compact");

        // Under "Status only" the user chose to keep session text off Telegram, and a
        // follow would show it: refused with the setting to flip, nothing recorded.
        let mut status_only = settings(Some(private));
        status_only.content_full = false;
        let batch = plan_batch(&updates[..1], &status_only, &state, now);
        assert!(batch.follows.is_empty());
        assert!(batch.says.iter().any(|job| matches!(
            job,
            OutboundJob::Say { text, .. } if text.contains("Full content")
        )));

        let batch = plan_batch(
            &[replying(update(3, 7, 7, "/unfollow"), 50)],
            &settings(Some(private)),
            &state,
            now,
        );
        assert_eq!(batch.unfollows.len(), 1);
        assert_eq!(batch.unfollows[0].session_id, "s1");
        assert!(batch.replies.is_empty());
    }

    #[test]
    fn a_cancel_tap_from_the_paired_user_is_planned_and_anyone_elses_is_only_acknowledged() {
        let now = Utc::now();
        let state = TelegramState::default();
        let private = Paired {
            chat_id: 7,
            user_id: 7,
            mode: TelegramChatMode::Private,
        };
        let tap = |id: i64, from: i64, data: &str| Update {
            update_id: id,
            message: None,
            callback_query: Some(CallbackQuery {
                id: format!("cb{id}"),
                from: User {
                    id: from,
                    is_bot: false,
                },
                message: Some(Message {
                    message_id: 90,
                    chat: Chat {
                        id: 7,
                        kind: "private".to_string(),
                        is_forum: false,
                    },
                    from: None,
                    text: None,
                    message_thread_id: None,
                    is_topic_message: false,
                    reply_to_message: None,
                }),
                data: Some(data.to_string()),
            }),
        };
        let updates = vec![
            tap(1, 7, "c:42"),
            // A stranger's tap on the same button, and a button CST does not know.
            tap(2, 999, "c:42"),
            tap(3, 7, "nonsense"),
        ];

        let batch = plan_batch(&updates, &settings(Some(private)), &state, now);

        assert_eq!(batch.cancels.len(), 1);
        assert_eq!(batch.cancels[0].update_id, 42);
        assert_eq!(batch.cancels[0].source, CancelSource::Notice);
        assert_eq!(batch.cancels[0].message_id, Some(90));
        // Acknowledged so the button stops spinning on the phone, and nothing more:
        // the "only the paired user acts" rule holds for taps as for words.
        assert_eq!(batch.ignored_taps, vec!["cb2", "cb3"]);
        assert!(batch.replies.is_empty());
        assert_eq!(batch.next_offset, Some(4));
    }

    #[test]
    fn slash_queue_lists_one_session_when_sent_to_it_and_everything_when_sent_bare() {
        use crate::notifications::NotificationKind;

        let now = Utc::now();
        let mut state = TelegramState::default();
        state.record_sent(SentRecord {
            message_id: 50,
            session_id: "s1".to_string(),
            kind: Some(SentKind::from(NotificationKind::Ready)),
            tool_call_id: None,
            sent_at: now,
        });
        let private = Paired {
            chat_id: 7,
            user_id: 7,
            mode: TelegramChatMode::Private,
        };
        let updates = vec![
            replying(update(1, 7, 7, "/queue"), 50),
            update(2, 7, 7, "/Queue@cst_bot"),
        ];

        let batch = plan_batch(&updates, &settings(Some(private)), &state, now);

        assert_eq!(batch.queue_requests.len(), 2);
        assert_eq!(batch.queue_requests[0].session_id.as_deref(), Some("s1"));
        assert_eq!(batch.queue_requests[0].reply_to, 10);
        assert_eq!(batch.queue_requests[1].session_id, None);
        // Neither is typed into a session, and the bare one gets no "which session?"
        // hint: listing everything is what it asked for.
        assert!(batch.replies.is_empty());
        assert!(batch.says.is_empty());
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
