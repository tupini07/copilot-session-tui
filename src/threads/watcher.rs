//! The loop that turns a doorbell ring into a woken session.
//!
//! Shaped like `ConfigWatcher`: a worker thread parked on a timeout, a stop flag, and a
//! `Drop` that unparks it. It sends on the mux channel rather than touching `App`,
//! because everything from `spawn_pane` down to `send_prompt_snippet` needs `&mut App`
//! on the UI thread — and because sending is what wakes an event loop that would
//! otherwise idle for five seconds.
//!
//! The watcher decides *whether* a comment should reach a session. It deliberately does
//! not decide *how*: whether a session has a running pane, and whether that pane is
//! mid-turn, is only knowable on the UI thread, so that judgement lives there.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use chrono::{DateTime, Utc};

use super::doorbell::{self, NotificationTransport, Ring, UreqTransport};
use super::store::{self, ThreadState};
use super::{
    classify, CommentSighting, PendingDelivery, PendingReason, ThreadKind, ThreadRef, Verdict,
};
use crate::mux::MuxEvent;

/// What the watcher concluded about one comment for one subscriber.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// Hand this session a pointer, if the UI thread finds somewhere to put it.
    Wake {
        session_id: String,
        thread: ThreadRef,
    },
    /// Do not run anything; show it to the user instead.
    Held {
        session_id: String,
        thread: ThreadRef,
        reason: PendingReason,
    },
}

impl Delivery {
    pub fn session_id(&self) -> &str {
        match self {
            Self::Wake { session_id, .. } | Self::Held { session_id, .. } => session_id,
        }
    }
}

/// Decide what a batch of comments means for everyone watching a thread.
///
/// Pure apart from the state it is handed, so the security-relevant decisions can be
/// tested without a network, an account, or a running session.
///
/// Comments are marked seen whatever the outcome. A comment that was examined and
/// deliberately not delivered must not be examined again on the next poll, or a thread
/// with one foreign comment would re-raise the same notice every minute.
pub fn plan(
    state: &mut ThreadState,
    thread: &ThreadRef,
    comments: &[CommentSighting],
    our_login: &str,
    wakeups_per_hour: u32,
    now: DateTime<Utc>,
) -> Vec<Delivery> {
    let subscriber_ids: Vec<String> = state
        .subscriptions
        .iter()
        .filter(|subscription| &subscription.thread == thread && subscription.is_active())
        .map(|subscription| subscription.session_id.clone())
        .collect();

    let mut deliveries: Vec<Delivery> = Vec::new();
    let mut held: Vec<PendingDelivery> = Vec::new();

    for session_id in subscriber_ids {
        let Some(subscription) = state.subscription_mut(&session_id, thread) else {
            continue;
        };
        // One wake per thread per poll however many comments landed. Three replies
        // arriving together are one thing to go and look at, not three turns.
        let mut already_woken = false;

        for comment in comments {
            if subscription.has_seen(&comment.id) {
                continue;
            }
            subscription.mark_seen(&comment.id);

            match classify(comment, our_login, subscription) {
                Verdict::SkipOwn => {}
                Verdict::HoldForUser(reason) => {
                    held.push(PendingDelivery {
                        session_id: session_id.clone(),
                        thread: thread.clone(),
                        comment_url: comment.url.clone(),
                        reason,
                        arrived_at: now,
                    });
                    deliveries.push(Delivery::Held {
                        session_id: session_id.clone(),
                        thread: thread.clone(),
                        reason,
                    });
                }
                Verdict::Wake => {
                    if already_woken {
                        continue;
                    }
                    if subscription.may_wake(now, wakeups_per_hour) {
                        subscription.record_wakeup(now);
                        already_woken = true;
                        deliveries.push(Delivery::Wake {
                            session_id: session_id.clone(),
                            thread: thread.clone(),
                        });
                    } else {
                        held.push(PendingDelivery {
                            session_id: session_id.clone(),
                            thread: thread.clone(),
                            comment_url: comment.url.clone(),
                            reason: PendingReason::Throttled,
                            arrived_at: now,
                        });
                        deliveries.push(Delivery::Held {
                            session_id: session_id.clone(),
                            thread: thread.clone(),
                            reason: PendingReason::Throttled,
                        });
                        already_woken = true;
                    }
                }
            }
        }
    }

    for delivery in held {
        state.hold_for_user(delivery);
    }
    deliveries
}

/// Turn a fetched comment into what the rules operate on.
///
/// A comment whose timestamp GitHub sends in a form chrono cannot read still counts —
/// the time is only used for ageing, and dropping a real message over a formatting
/// detail would be far worse than showing the wrong age.
pub fn sighting(comment: crate::github::ThreadComment, fallback: DateTime<Utc>) -> CommentSighting {
    let created_at = DateTime::parse_from_rfc3339(&comment.created_at)
        .map(|moment| moment.with_timezone(&Utc))
        .unwrap_or(fallback);
    CommentSighting {
        id: comment.id,
        author: comment.author,
        url: comment.url,
        created_at,
    }
}

/// The oldest moment any active subscriber started caring about a thread.
///
/// Bounds the comment fetch: nobody is waiting on a comment posted before they joined,
/// and without a bound a thread with hundreds of comments would be pulled down in full
/// every time it moved.
fn earliest_interest(state: &ThreadState, thread: &ThreadRef) -> Option<String> {
    state
        .subscriptions
        .iter()
        .filter(|subscription| &subscription.thread == thread && subscription.is_active())
        .map(|subscription| subscription.subscribed_at)
        .min()
        .map(|moment| moment.to_rfc3339())
}

/// How the watcher is configured, resolved once so a reload is a restart of the loop.
#[derive(Debug, Clone)]
pub struct WatchSettings {
    pub poll_interval: Duration,
    pub wakeups_per_hour: u32,
}

#[must_use = "dropping the watcher stops its worker"]
pub struct ThreadWatcher {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl ThreadWatcher {
    pub fn start(
        events: Sender<MuxEvent>,
        root: PathBuf,
        our_login: String,
        settings: WatchSettings,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            let transport = UreqTransport::new();
            let mut delay = settings.poll_interval;
            let mut token: Option<String> = None;

            loop {
                thread::park_timeout(delay);
                if worker_stop.load(Ordering::Acquire) {
                    return;
                }

                let state = store::load_in(&root);
                if !state.has_active_subscription() {
                    // Nothing is subscribed, so there is nothing to ask about. An
                    // install that never uses this feature never touches the network.
                    delay = settings.poll_interval;
                    continue;
                }

                let watched = state.active_threads();
                let host = watched
                    .first()
                    .map(|thread| thread.host.clone())
                    .unwrap_or_else(|| "github.com".to_string());

                if token.is_none() {
                    match doorbell::token_for(&host) {
                        Ok(value) => token = Some(value),
                        Err(error) => {
                            let _ = events.send(MuxEvent::ThreadWatchFailed(error));
                            // Backing off hard: without a token every tick would spawn
                            // another `gh` for the same certain failure.
                            delay = settings.poll_interval.max(Duration::from_secs(300));
                            continue;
                        }
                    }
                }
                let Some(active_token) = token.clone() else {
                    continue;
                };

                let cursor = state.notifications_cursor.clone();
                let url = doorbell::notifications_url(&host);
                let response = match transport.fetch(&url, &active_token, cursor.as_deref()) {
                    Ok(response) => response,
                    Err(error) => {
                        let _ = events.send(MuxEvent::ThreadWatchFailed(error));
                        delay = (delay * 2).min(Duration::from_secs(600));
                        continue;
                    }
                };

                match doorbell::interpret(&response, &watched) {
                    Ok((ring, server_interval)) => {
                        delay = doorbell::poll_delay(settings.poll_interval, server_interval);
                        if let Ring::Moved { threads, cursor } = ring {
                            // The cursor advances even when nothing we watch moved, so
                            // an inbox full of other people's noise is paid for once.
                            let _ = store::update_in(&root, |state| {
                                state.notifications_cursor = cursor;
                            });
                            for thread in threads {
                                // Fetched here and not on the UI thread: this is a `gh`
                                // process and a network round trip, and the event loop
                                // draws the terminal.
                                let since = earliest_interest(&state, &thread);
                                let cancelled = Arc::new(AtomicBool::new(false));
                                match fetch_comments(
                                    &thread,
                                    root.clone(),
                                    since.as_deref(),
                                    cancelled,
                                ) {
                                    Ok(comments) => {
                                        let deliveries = store::update_in(&root, |state| {
                                            plan(
                                                state,
                                                &thread,
                                                &comments,
                                                &our_login,
                                                settings.wakeups_per_hour,
                                                Utc::now(),
                                            )
                                        });
                                        for delivery in deliveries.unwrap_or_default() {
                                            let _ = events
                                                .send(MuxEvent::ThreadDelivery(Box::new(delivery)));
                                        }
                                    }
                                    Err(error) => {
                                        let _ = events.send(MuxEvent::ThreadWatchFailed(error));
                                    }
                                }
                            }
                        }
                    }
                    Err(error) => {
                        // A rejected token will not fix itself; make the next attempt
                        // re-read it in case the user has just logged in.
                        token = None;
                        let _ = events.send(MuxEvent::ThreadWatchFailed(error));
                        delay = settings.poll_interval.max(Duration::from_secs(300));
                    }
                }
            }
        });

        Self {
            stop,
            worker: Some(worker),
        }
    }
}

impl Drop for ThreadWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}

/// Fetch the comments on a thread that this machine has not accounted for yet.
///
/// Runs on whatever thread asks; the caller owns the cancellation flag.
pub fn fetch_comments(
    thread: &ThreadRef,
    cwd: PathBuf,
    since: Option<&str>,
    cancelled: Arc<AtomicBool>,
) -> Result<Vec<CommentSighting>, String> {
    let comments = crate::github::fetch_thread_comments(
        cwd,
        crate::github::CommentTarget {
            host: &thread.host,
            owner: &thread.owner,
            repo: &thread.repo,
            number: thread.number,
            discussion: thread.kind == ThreadKind::Discussion,
        },
        since,
        cancelled,
    )
    .map_err(|error| error.to_string())?;

    let now = Utc::now();
    Ok(comments
        .into_iter()
        .map(|comment| sighting(comment, now))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread() -> ThreadRef {
        ThreadRef {
            host: "github.com".to_string(),
            owner: "o".to_string(),
            repo: "r".to_string(),
            number: 12,
            kind: ThreadKind::Issue,
        }
    }

    fn comment(id: &str, author: &str) -> CommentSighting {
        CommentSighting {
            id: id.to_string(),
            author: author.to_string(),
            url: format!("https://github.com/o/r/issues/12#issuecomment-{id}"),
            created_at: Utc::now(),
        }
    }

    fn state_with(sessions: &[&str]) -> ThreadState {
        let mut state = ThreadState::default();
        for session in sessions {
            state.subscribe(session, thread());
        }
        state
    }

    #[test]
    fn a_reply_from_another_agent_wakes_the_session_that_was_waiting() {
        let mut state = state_with(&["session-a"]);

        let deliveries = plan(
            &mut state,
            &thread(),
            &[comment("1", "tupini07")],
            "tupini07",
            12,
            Utc::now(),
        );

        assert_eq!(
            deliveries,
            vec![Delivery::Wake {
                session_id: "session-a".to_string(),
                thread: thread(),
            }]
        );
    }

    #[test]
    fn a_session_is_never_woken_by_the_comment_it_just_posted() {
        let mut state = state_with(&["session-a"]);
        state
            .subscription_mut("session-a", &thread())
            .unwrap()
            .record_authored("1");

        let deliveries = plan(
            &mut state,
            &thread(),
            &[comment("1", "tupini07")],
            "tupini07",
            12,
            Utc::now(),
        );

        assert!(deliveries.is_empty(), "got: {deliveries:?}");
    }

    #[test]
    fn an_outsiders_comment_is_held_for_the_user_and_starts_nothing() {
        // The security property, tested where it actually takes effect rather than only
        // on the rule in isolation: no Wake may appear for a foreign author, ever.
        let mut state = state_with(&["session-a"]);

        let deliveries = plan(
            &mut state,
            &thread(),
            &[comment("1", "a-stranger")],
            "tupini07",
            12,
            Utc::now(),
        );

        assert_eq!(
            deliveries,
            vec![Delivery::Held {
                session_id: "session-a".to_string(),
                thread: thread(),
                reason: PendingReason::ForeignAuthor,
            }]
        );
        assert!(!deliveries
            .iter()
            .any(|delivery| matches!(delivery, Delivery::Wake { .. })));
        assert_eq!(state.pending_for("session-a").len(), 1);
    }

    #[test]
    fn three_replies_arriving_together_are_one_thing_to_go_and_look_at() {
        let mut state = state_with(&["session-a"]);

        let deliveries = plan(
            &mut state,
            &thread(),
            &[
                comment("1", "tupini07"),
                comment("2", "tupini07"),
                comment("3", "tupini07"),
            ],
            "tupini07",
            12,
            Utc::now(),
        );

        assert_eq!(deliveries.len(), 1, "got: {deliveries:?}");
    }

    #[test]
    fn a_comment_examined_once_is_not_examined_again_on_the_next_poll() {
        // Without this a single foreign comment would re-raise its notice every minute.
        let mut state = state_with(&["session-a"]);
        let comments = [comment("1", "tupini07")];

        let first = plan(&mut state, &thread(), &comments, "tupini07", 12, Utc::now());
        let second = plan(&mut state, &thread(), &comments, "tupini07", 12, Utc::now());

        assert_eq!(first.len(), 1);
        assert!(second.is_empty(), "got: {second:?}");
    }

    #[test]
    fn one_comment_reaches_every_session_watching_the_thread() {
        let mut state = state_with(&["session-a", "session-b"]);

        let deliveries = plan(
            &mut state,
            &thread(),
            &[comment("1", "tupini07")],
            "tupini07",
            12,
            Utc::now(),
        );

        let woken: Vec<&str> = deliveries.iter().map(Delivery::session_id).collect();
        assert_eq!(woken, vec!["session-a", "session-b"]);
    }

    #[test]
    fn a_thread_that_keeps_waking_one_session_is_held_rather_than_left_to_run_away() {
        let mut state = state_with(&["session-a"]);
        let now = Utc::now();

        for id in 1..=2 {
            plan(
                &mut state,
                &thread(),
                &[comment(&id.to_string(), "tupini07")],
                "tupini07",
                2,
                now,
            );
        }
        let over = plan(
            &mut state,
            &thread(),
            &[comment("3", "tupini07")],
            "tupini07",
            2,
            now,
        );

        assert_eq!(
            over,
            vec![Delivery::Held {
                session_id: "session-a".to_string(),
                thread: thread(),
                reason: PendingReason::Throttled,
            }]
        );
    }

    #[test]
    fn a_paused_subscription_is_skipped_without_losing_its_place() {
        let mut state = state_with(&["session-a"]);
        state
            .subscription_mut("session-a", &thread())
            .unwrap()
            .state = super::super::SubscriptionState::Paused;

        let deliveries = plan(
            &mut state,
            &thread(),
            &[comment("1", "tupini07")],
            "tupini07",
            12,
            Utc::now(),
        );

        assert!(deliveries.is_empty());
        // The comment was not marked seen, so resuming picks it up rather than
        // silently skipping everything that arrived while paused.
        assert!(!state
            .subscription_mut("session-a", &thread())
            .unwrap()
            .has_seen("1"));
    }

    #[test]
    fn an_unreadable_timestamp_still_delivers_the_message() {
        let fallback = Utc::now();
        let sighting = sighting(
            crate::github::ThreadComment {
                id: "1".to_string(),
                author: "tupini07".to_string(),
                url: "https://github.com/o/r/issues/12#issuecomment-1".to_string(),
                created_at: "not a timestamp".to_string(),
            },
            fallback,
        );

        assert_eq!(sighting.created_at, fallback);
        assert_eq!(sighting.id, "1");
    }
}
