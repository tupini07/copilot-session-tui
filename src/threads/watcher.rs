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

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use chrono::{DateTime, Utc};

use super::doorbell::{self, PollError, Ring, ThreadTransport, UreqTransport};
use super::judge;
use super::store::{self, ThreadState};
use super::{
    classify, CommentSighting, Notice, NoticeStatus, PendingReason, ThreadKind, ThreadRef, Verdict,
};
use crate::mux::MuxEvent;

/// Decide what a batch of comments means for everyone watching a thread.
///
/// Pure apart from the state it is handed, so the security-relevant decisions can be
/// tested without a network, an account, or a running session.
///
/// Comments are marked seen whatever the outcome. A comment that was examined and
/// deliberately not delivered must not be examined again on the next poll, or a
/// throttled thread would raise the same notice again every minute.
///
/// Reports whether this batch produced anything for anybody, which is all the caller
/// needs: the notices themselves are already on the state, and what happens to one next
/// depends on panes the watcher cannot see.
pub fn plan(
    state: &mut ThreadState,
    thread: &ThreadRef,
    comments: &[CommentSighting],
    our_login: &str,
    trusted: &[String],
    wakeups_per_hour: u32,
    now: DateTime<Utc>,
) -> bool {
    let subscriber_ids: Vec<String> = state
        .subscriptions
        .iter()
        .filter(|subscription| &subscription.thread == thread && subscription.is_active())
        .map(|subscription| subscription.session_id.clone())
        .collect();

    let mut recorded: Vec<Notice> = Vec::new();

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

            // Anything already there when this session joined is history, not a message
            // for it. Without this, taking part in a long-running thread would wake you
            // once for every comment anybody had ever left on it.
            if comment.created_at < subscription.subscribed_at {
                continue;
            }

            match classify(comment, our_login, trusted, subscription) {
                Verdict::SkipOwn | Verdict::Ignore => {}
                Verdict::Wake => {
                    if already_woken {
                        continue;
                    }
                    already_woken = true;
                    // Written down as planned before anything is delivered. The comment
                    // is marked seen either way, so a notice that only existed in memory
                    // would be lost by a restart and never mentioned again.
                    let status = if subscription.may_wake(now, wakeups_per_hour) {
                        subscription.record_wakeup(now);
                        NoticeStatus::Planned
                    } else {
                        NoticeStatus::Waiting {
                            reason: PendingReason::Throttled,
                        }
                    };
                    recorded.push(Notice {
                        session_id: session_id.clone(),
                        thread: thread.clone(),
                        comment_url: comment.url.clone(),
                        author: Some(comment.author.clone()),
                        status,
                        planned_at: now,
                    });
                }
            }
        }
    }

    let produced = !recorded.is_empty();
    for notice in recorded {
        state.record_notice(notice);
    }
    produced
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

/// Ask one thread whether it has moved since the stored cursor.
///
/// Issues and pull requests are a conditional HTTP request: free when nothing changed.
/// Discussions have no REST endpoint, so their `updatedAt` is fetched through GraphQL
/// and compared against the stored value — the same idea, one round trip more expensive.
///
/// The new cursor is handed back rather than stored. Storing it here is what used to
/// lose wake-ups: when the comment fetch that follows failed, the next poll already
/// carried the new cursor, was told nothing had changed, and the comments were never
/// looked at again. See [`remember_cursor`].
fn poll_thread(
    transport: &impl ThreadTransport,
    root: &Path,
    thread: &ThreadRef,
    token: &str,
) -> Result<Ring, PollError> {
    let previous = store::load_in(root).cursors.get(&thread.url()).cloned();

    match doorbell::thread_api_url(thread) {
        Some(url) => {
            let response = transport
                .fetch(&url, token, previous.as_deref())
                .map_err(PollError::Unreachable)?;
            doorbell::interpret(&response)
        }
        None => {
            let stamp = crate::github::fetch_discussion_updated_at(
                root.to_path_buf(),
                crate::github::CommentTarget {
                    host: &thread.host,
                    owner: &thread.owner,
                    repo: &thread.repo,
                    number: thread.number,
                    discussion: true,
                },
                Arc::new(AtomicBool::new(false)),
            )
            .map_err(PollError::from_github)?;
            if previous.as_deref() == Some(stamp.as_str()) {
                return Ok(Ring::Quiet);
            }
            Ok(Ring::Moved {
                cursor: Some(stamp),
            })
        }
    }
}

/// Accept a thread's new cursor, once whatever moved it has been accounted for.
///
/// Recorded even on the very first sighting, so a thread that never changes again is
/// asked about for free from here on.
fn remember_cursor(state: &mut ThreadState, thread: &ThreadRef, cursor: Option<String>) {
    match cursor {
        Some(cursor) => {
            state.cursors.insert(thread.url(), cursor);
        }
        None => {
            state.cursors.remove(&thread.url());
        }
    }
}

/// The longest the watcher waits between attempts while GitHub cannot be reached.
///
/// Long enough not to spin through a night offline, short enough that watching visibly
/// resumes within minutes of the network coming back.
const LONGEST_RETRY: Duration = Duration::from_secs(300);

/// What a failed poll does to the rest of the round, and when the next one starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Setback {
    /// Stop asking about the remaining threads this round, because they would fail the
    /// same way.
    abandon_round: bool,
    /// Read the token again before the next request.
    forget_token: bool,
    /// When to try again, if the round was abandoned.
    retry_in: Duration,
}

/// Decide what a failure means for the loop.
///
/// `misses` counts the rounds abandoned in a row before this one, so retries spread out
/// while GitHub stays away and snap back once it answers.
///
/// None of these stop the watcher. A machine left overnight loses its network, and the
/// watcher has to still be there in the morning.
fn setback(error: &PollError, poll_interval: Duration, misses: u32) -> Setback {
    match error {
        PollError::Unreachable(_) => {
            let base = doorbell::poll_delay(poll_interval);
            Setback {
                abandon_round: true,
                // The token is not what failed, and reading it again is a `gh` process.
                forget_token: false,
                retry_in: base
                    .saturating_mul(1 << misses.min(16))
                    .min(LONGEST_RETRY)
                    .max(base),
            }
        }
        PollError::Refused(_) => Setback {
            abandon_round: true,
            // A rejected token will not fix itself; re-read it in case the user has
            // logged in since.
            forget_token: true,
            retry_in: poll_interval.max(LONGEST_RETRY),
        },
        // One deleted or hidden thread must not hold up the others, which is what
        // ending the round on it would do every minute for as long as it is watched.
        PollError::Gone(_) => Setback {
            abandon_round: false,
            forget_token: false,
            retry_in: poll_interval,
        },
    }
}

/// How the watcher is configured, resolved once so a reload is a restart of the loop.
#[derive(Debug, Clone)]
pub struct WatchSettings {
    pub poll_interval: Duration,
    pub wakeups_per_hour: u32,
    pub trusted_authors: Vec<String>,
    pub stall_detection: bool,
}

#[must_use = "dropping the watcher stops its worker"]
pub struct ThreadWatcher {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

/// Unreadable threads already reported, so each is said once rather than every minute.
///
/// The UI only remembers the last failure, and each of these names its own thread, so
/// with two of them watched the UI would see a new message every poll and show it.
#[derive(Debug, Default)]
struct GoneThreads(std::collections::HashSet<String>);

impl GoneThreads {
    /// Whether this is news, recording that it is not news any more.
    fn first_report(&mut self, thread: &ThreadRef) -> bool {
        self.0.insert(thread.url())
    }

    /// A thread that answered again, or is no longer watched, may be reported anew.
    fn keep_only_unreadable(&mut self, still_unreadable: impl Fn(&str) -> bool) {
        self.0.retain(|url| still_unreadable(url));
    }
}

/// What every check in a round shares, so checking one thread can be its own function.
struct Watch {
    events: Sender<MuxEvent>,
    root: PathBuf,
    settings: WatchSettings,
}

impl Watch {
    /// Look at one thread and record whatever it means.
    ///
    /// Any failure leaves the thread's cursor where it was, so the next round asks the
    /// same question again instead of being told nothing has changed.
    fn check_thread(
        &self,
        transport: &impl ThreadTransport,
        state: &ThreadState,
        thread: &ThreadRef,
        token: &str,
        our_login: &str,
    ) -> Result<(), PollError> {
        let Ring::Moved { cursor } = poll_thread(transport, &self.root, thread, token)? else {
            return Ok(());
        };

        // Fetched here and not on the UI thread: this is a `gh` process and a network
        // round trip, and the event loop draws the terminal.
        let since = earliest_interest(state, thread);
        let cancelled = Arc::new(AtomicBool::new(false));
        let comments = fetch_comments(thread, self.root.clone(), since.as_deref(), cancelled)?;

        // The cursor goes in the same write as the notices, so the thread counts as
        // looked at exactly when what it said has been recorded.
        let produced = store::update_in(&self.root, |state| {
            remember_cursor(state, thread, cursor);
            plan(
                state,
                thread,
                &comments,
                our_login,
                &self.settings.trusted_authors,
                self.settings.wakeups_per_hour,
                Utc::now(),
            )
        });
        // The notices are already on disk; this only asks the UI thread to look, since
        // it is the side that knows which panes can take one. Nothing is sent when the
        // batch was all our own comments, which is most of them: a wake CST posted
        // itself moves every thread it is watching.
        match produced {
            Ok(true) => {
                let _ = self.events.send(MuxEvent::ThreadNoticesChanged);
            }
            Ok(false) => {}
            Err(error) => {
                // Nothing was saved, the cursor included, so the next round sees the
                // same comments again — but silence here would look exactly like a
                // quiet thread.
                let _ = self.events.send(MuxEvent::ThreadWatchFailed(format!(
                    "Thread watching could not record notices: {error}"
                )));
            }
        }

        // Only after a burst, and only ever to report. A conversation that settles
        // something takes a handful of turns; one that does not trips this within
        // minutes.
        if self.settings.stall_detection && judge::is_a_burst(&comments, Utc::now()) {
            let checking = Arc::new(AtomicBool::new(false));
            if judge::examine(thread, checking) == Some(judge::Stall::Circling) {
                let _ = self
                    .events
                    .send(MuxEvent::ThreadStalled(judge::notice(thread)));
            }
        }
        Ok(())
    }
}

impl ThreadWatcher {
    /// Start watching. Never touches the network itself: the account is looked up by
    /// the worker, once something is subscribed.
    ///
    /// Asking here, on the UI thread at startup, meant a CST started while GitHub was
    /// out of reach — before the VPN, straight out of sleep — never watched anything
    /// until it was restarted, and said nothing about it.
    pub fn start(events: Sender<MuxEvent>, root: PathBuf, settings: WatchSettings) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            let transport = UreqTransport::new();
            let watch = Watch {
                events,
                root,
                settings,
            };
            let poll_interval = watch.settings.poll_interval;
            let mut delay = poll_interval;
            // Read once and kept: it decides whether a comment came from one of our own
            // agents, and asking GitHub every poll would be a round trip for an answer
            // that does not change.
            let mut our_login: Option<String> = None;
            let mut token: Option<String> = None;
            // Rounds abandoned in a row, which spreads the retries out.
            let mut misses = 0;
            // Whether the user was told watching is held up, so they can be told it is
            // going again. Without that, a notice from the night before would be the
            // last word on the subject.
            let mut held_up = false;
            let mut gone = GoneThreads::default();

            loop {
                thread::park_timeout(delay);
                if worker_stop.load(Ordering::Acquire) {
                    return;
                }

                let state = store::load_in(&watch.root);
                if !state.has_active_subscription() {
                    // Nothing is subscribed, so there is nothing to ask about. An
                    // install that never uses this feature never touches the network.
                    delay = poll_interval;
                    continue;
                }

                let watched = state.active_threads();
                let Some(first) = watched.first() else {
                    continue;
                };

                // Before the first poll: what is said about a failure needs a thread
                // to name, and these two have none of their own.
                let early = if our_login.is_none() {
                    // Only github.com: that is the account CST's agents post as.
                    match doorbell::current_login("github.com") {
                        Ok(login) => {
                            our_login = Some(login);
                            None
                        }
                        // `gh api user` fails the same way whether the network is down
                        // or the login is, and only one of those mends itself, so it is
                        // retried as if it might.
                        Err(error) => Some(PollError::Unreachable(error)),
                    }
                } else {
                    None
                };
                let early = early.or_else(|| {
                    if token.is_some() {
                        return None;
                    }
                    match doorbell::token_for(&first.host) {
                        Ok(value) => {
                            token = Some(value);
                            None
                        }
                        // Without a token every tick would spawn another `gh` for the
                        // same certain failure, so this waits the long way.
                        Err(error) => Some(PollError::Refused(error)),
                    }
                });
                if let Some(error) = early {
                    let setback = setback(&error, poll_interval, misses);
                    let _ = watch
                        .events
                        .send(MuxEvent::ThreadWatchFailed(error.describe(first)));
                    held_up = true;
                    misses += 1;
                    delay = setback.retry_in;
                    continue;
                }
                let (Some(login), Some(active_token)) = (our_login.as_deref(), token.clone())
                else {
                    continue;
                };

                delay = doorbell::poll_delay(poll_interval);
                let mut abandoned = false;
                let mut unreadable: Vec<String> = Vec::new();

                // One conditional request per watched thread. More requests than asking
                // the notifications inbox once, and the only version that works: GitHub
                // does not notify you about your own activity, and every CST agent
                // comments as the same account.
                for thread in &watched {
                    if worker_stop.load(Ordering::Acquire) {
                        return;
                    }
                    let Err(error) =
                        watch.check_thread(&transport, &state, thread, &active_token, login)
                    else {
                        continue;
                    };
                    let setback = setback(&error, poll_interval, misses);
                    let news = match error {
                        PollError::Gone(_) => {
                            unreadable.push(thread.url());
                            gone.first_report(thread)
                        }
                        _ => true,
                    };
                    if news {
                        let _ = watch
                            .events
                            .send(MuxEvent::ThreadWatchFailed(error.describe(thread)));
                    }
                    if setback.forget_token {
                        token = None;
                    }
                    if setback.abandon_round {
                        abandoned = true;
                        delay = setback.retry_in;
                        break;
                    }
                }

                if abandoned {
                    held_up = true;
                    misses += 1;
                } else {
                    // Only a round that reached every thread knows which still fail.
                    gone.keep_only_unreadable(|url| unreadable.iter().any(|u| u == url));
                    misses = 0;
                    if std::mem::take(&mut held_up) {
                        let _ = watch.events.send(MuxEvent::ThreadWatchResumed);
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
) -> Result<Vec<CommentSighting>, PollError> {
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
    .map_err(PollError::from_github)?;

    let now = Utc::now();
    Ok(comments
        .into_iter()
        .map(|comment| sighting(comment, now))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `plan` wrote down, in the shape these assertions are written in.
    ///
    /// `plan` itself only reports how many notices it recorded; the notices left on the
    /// state are the actual product, and reading them back is what proves a decision
    /// would survive the process that made it.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Delivery {
        Wake {
            session_id: String,
            thread: ThreadRef,
        },
        Held {
            session_id: String,
            thread: ThreadRef,
            reason: PendingReason,
        },
    }

    fn recorded(state: &ThreadState) -> Vec<Delivery> {
        state
            .notices
            .iter()
            .map(|notice| match notice.status {
                NoticeStatus::Waiting { reason } => Delivery::Held {
                    session_id: notice.session_id.clone(),
                    thread: notice.thread.clone(),
                    reason,
                },
                _ => Delivery::Wake {
                    session_id: notice.session_id.clone(),
                    thread: notice.thread.clone(),
                },
            })
            .collect()
    }

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

    /// Poll and accept the new cursor straight away, as the loop does once the comments
    /// are recorded. The live tests use it to settle a thread before posting to it.
    fn thread_moved(
        transport: &impl ThreadTransport,
        root: &Path,
        thread: &ThreadRef,
        token: &str,
    ) -> Result<bool, PollError> {
        match poll_thread(transport, root, thread, token)? {
            Ring::Quiet => Ok(false),
            Ring::Moved { cursor } => {
                store::update_in(root, |state| remember_cursor(state, thread, cursor)).unwrap();
                Ok(true)
            }
        }
    }

    /// GitHub as a scripted list of answers, one per request.
    struct ScriptedTransport {
        answers: std::sync::Mutex<Vec<Result<doorbell::DoorbellResponse, String>>>,
        sent_cursors: std::sync::Mutex<Vec<Option<String>>>,
    }

    impl ScriptedTransport {
        fn new(answers: Vec<Result<doorbell::DoorbellResponse, String>>) -> Self {
            Self {
                answers: std::sync::Mutex::new(answers),
                sent_cursors: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl ThreadTransport for ScriptedTransport {
        fn fetch(
            &self,
            _url: &str,
            _token: &str,
            if_none_match: Option<&str>,
        ) -> Result<doorbell::DoorbellResponse, String> {
            self.sent_cursors
                .lock()
                .unwrap()
                .push(if_none_match.map(str::to_string));
            self.answers.lock().unwrap().remove(0)
        }
    }

    fn answer(status: u16, etag: &str) -> Result<doorbell::DoorbellResponse, String> {
        Ok(doorbell::DoorbellResponse {
            status,
            cursor: Some(etag.to_string()),
            body: Vec::new(),
        })
    }

    #[test]
    fn a_thread_that_moved_is_asked_again_until_its_comments_are_recorded() {
        // The doorbell rang, then fetching the comments failed on a network that was
        // going down. Had the poll stored the new cursor itself, the next poll would
        // have been told "not modified" and the reply would never have been seen.
        let root = tempfile::tempdir().unwrap();
        let transport = ScriptedTransport::new(vec![
            answer(200, "\"after-reply\""),
            answer(200, "\"after-reply\""),
        ]);

        let first = poll_thread(&transport, root.path(), &thread(), "token").unwrap();
        assert!(matches!(first, Ring::Moved { .. }));
        // ...and the comment fetch fails here, so nothing is recorded.

        let second = poll_thread(&transport, root.path(), &thread(), "token").unwrap();
        assert!(
            matches!(second, Ring::Moved { .. }),
            "the thread must still read as moved, got: {second:?}"
        );
        assert_eq!(
            *transport.sent_cursors.lock().unwrap(),
            vec![None, None],
            "the second poll must not carry a cursor nobody acted on"
        );
    }

    #[test]
    fn a_thread_whose_comments_were_recorded_is_not_asked_about_twice() {
        let root = tempfile::tempdir().unwrap();
        let transport = ScriptedTransport::new(vec![answer(200, "\"v1\""), answer(304, "\"v1\"")]);

        assert!(thread_moved(&transport, root.path(), &thread(), "token").unwrap());
        assert!(!thread_moved(&transport, root.path(), &thread(), "token").unwrap());
        assert_eq!(
            transport.sent_cursors.lock().unwrap()[1].as_deref(),
            Some("\"v1\"")
        );
    }

    #[test]
    fn a_network_that_is_down_is_reported_as_unreachable_and_not_as_a_bad_login() {
        let root = tempfile::tempdir().unwrap();
        let transport =
            ScriptedTransport::new(vec![Err("Could not reach GitHub: io: timed out".into())]);

        let error = poll_thread(&transport, root.path(), &thread(), "token").unwrap_err();

        assert!(matches!(error, PollError::Unreachable(_)), "got: {error:?}");
    }

    #[test]
    fn an_unreachable_github_is_retried_for_as_long_as_it_takes() {
        // The watcher used to back off to five minutes and say it was paused. Retrying
        // was right; what has to hold is that it never stops, never waits longer than
        // the cap, and starts again at the normal pace.
        let interval = Duration::from_secs(60);
        let error = PollError::Unreachable("Could not reach GitHub".into());

        let first = setback(&error, interval, 0);
        assert!(first.abandon_round);
        assert!(!first.forget_token, "the token is not what failed");
        assert_eq!(
            first.retry_in, interval,
            "the first retry is an ordinary poll"
        );

        let mut previous = first.retry_in;
        for misses in 1..200 {
            let next = setback(&error, interval, misses).retry_in;
            assert!(
                next >= previous,
                "retries only spread out while it stays away"
            );
            assert!(
                next <= LONGEST_RETRY,
                "a night offline must not mean an hour's wait"
            );
            previous = next;
        }
        assert_eq!(previous, LONGEST_RETRY);
    }

    #[test]
    fn one_unreadable_thread_does_not_hold_up_the_others() {
        // Ending the round on it would stall every thread behind it in the list, every
        // minute, for as long as it stayed watched.
        let gone = setback(
            &PollError::Gone("deleted".into()),
            Duration::from_secs(60),
            0,
        );

        assert!(!gone.abandon_round);
        assert!(!gone.forget_token);
    }

    #[test]
    fn two_unreadable_threads_are_each_reported_once_and_not_every_minute() {
        // The UI remembers only the last failure, so two messages taking turns would
        // both be shown again on every poll.
        let mut a = thread();
        a.number = 1;
        let mut b = thread();
        b.number = 2;
        let mut gone = GoneThreads::default();

        assert!(gone.first_report(&a));
        assert!(gone.first_report(&b));
        gone.keep_only_unreadable(|_| true);
        assert!(!gone.first_report(&a), "the next minute is not news");
        assert!(!gone.first_report(&b));

        // `a` answered again; if it disappears later, that is news once more.
        gone.keep_only_unreadable(|url| url == b.url());
        assert!(gone.first_report(&a));
        assert!(!gone.first_report(&b));
    }

    #[test]
    fn a_refused_token_is_read_again_before_the_next_try() {
        let refused = setback(
            &PollError::Refused("401".into()),
            Duration::from_secs(60),
            0,
        );

        assert!(refused.abandon_round);
        assert!(refused.forget_token, "the user may have logged in since");
    }

    #[test]
    fn a_reply_from_another_agent_wakes_the_session_that_was_waiting() {
        let mut state = state_with(&["session-a"]);

        plan(
            &mut state,
            &thread(),
            &[comment("1", "tupini07")],
            "tupini07",
            &[],
            12,
            Utc::now(),
        );
        let deliveries = recorded(&state);

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

        plan(
            &mut state,
            &thread(),
            &[comment("1", "tupini07")],
            "tupini07",
            &[],
            12,
            Utc::now(),
        );
        let deliveries = recorded(&state);

        assert!(deliveries.is_empty(), "got: {deliveries:?}");
    }

    #[test]
    fn an_outsiders_comment_starts_nothing_and_asks_the_user_nothing() {
        // The security property, tested where it actually takes effect rather than only
        // on the rule in isolation: no Wake may appear for a foreign author, ever. And
        // no question either, or a busy public thread fills the inbox with them.
        let mut state = state_with(&["session-a"]);

        let produced = plan(
            &mut state,
            &thread(),
            &[comment("1", "a-stranger")],
            "tupini07",
            &[],
            12,
            Utc::now(),
        );

        assert!(!produced);
        assert!(recorded(&state).is_empty(), "got: {:?}", recorded(&state));
        // Still marked seen, so it is not reconsidered on every poll.
        assert!(state
            .subscription_mut("session-a", &thread())
            .unwrap()
            .has_seen("1"));
    }

    #[test]
    fn an_outsiders_comment_does_not_stand_between_a_session_and_our_own_reply() {
        // Under the old rule the stranger's comment became a held notice, and with one
        // notice per thread every later reply from our own side was folded into it and
        // never delivered.
        let mut state = state_with(&["session-a"]);

        plan(
            &mut state,
            &thread(),
            &[comment("1", "a-stranger")],
            "tupini07",
            &[],
            12,
            Utc::now(),
        );
        plan(
            &mut state,
            &thread(),
            &[comment("2", "tupini07")],
            "tupini07",
            &[],
            12,
            Utc::now(),
        );

        assert_eq!(
            recorded(&state),
            vec![Delivery::Wake {
                session_id: "session-a".to_string(),
                thread: thread(),
            }]
        );
    }

    #[test]
    fn three_replies_arriving_together_are_one_thing_to_go_and_look_at() {
        let mut state = state_with(&["session-a"]);

        plan(
            &mut state,
            &thread(),
            &[
                comment("1", "tupini07"),
                comment("2", "tupini07"),
                comment("3", "tupini07"),
            ],
            "tupini07",
            &[],
            12,
            Utc::now(),
        );
        let deliveries = recorded(&state);

        assert_eq!(deliveries.len(), 1, "got: {deliveries:?}");
    }

    #[test]
    fn a_comment_examined_once_is_not_examined_again_on_the_next_poll() {
        // Without this the same comment would wake the session again on every poll.
        let mut state = state_with(&["session-a"]);
        let comments = [comment("1", "tupini07")];

        let first = plan(
            &mut state,
            &thread(),
            &comments,
            "tupini07",
            &[],
            12,
            Utc::now(),
        );
        let second = plan(
            &mut state,
            &thread(),
            &comments,
            "tupini07",
            &[],
            12,
            Utc::now(),
        );
        assert!(first);
        assert!(!second, "the same comment must not be examined twice");
    }

    #[test]
    fn one_comment_reaches_every_session_watching_the_thread() {
        let mut state = state_with(&["session-a", "session-b"]);

        plan(
            &mut state,
            &thread(),
            &[comment("1", "tupini07")],
            "tupini07",
            &[],
            12,
            Utc::now(),
        );
        let deliveries = recorded(&state);

        let woken: Vec<&str> = deliveries
            .iter()
            .map(|delivery| match delivery {
                Delivery::Wake { session_id, .. } | Delivery::Held { session_id, .. } => {
                    session_id.as_str()
                }
            })
            .collect();
        assert_eq!(woken, vec!["session-a", "session-b"]);
    }

    #[test]
    fn one_message_reaches_every_other_participant_and_never_its_author() {
        // Three agents on one thread. This is the fan-out: a single question wakes both
        // of the others, independently, with nothing telling either that the other is
        // also about to answer.
        let mut state = state_with(&["session-a", "session-b", "session-c"]);
        state
            .subscription_mut("session-c", &thread())
            .unwrap()
            .record_authored("1");

        plan(
            &mut state,
            &thread(),
            &[comment("1", "tupini07")],
            "tupini07",
            &[],
            12,
            Utc::now(),
        );
        let deliveries = recorded(&state);

        assert_eq!(
            deliveries,
            vec![
                Delivery::Wake {
                    session_id: "session-a".to_string(),
                    thread: thread(),
                },
                Delivery::Wake {
                    session_id: "session-b".to_string(),
                    thread: thread(),
                },
            ],
            "the author is not woken by its own question"
        );
    }

    #[test]
    fn two_agents_answering_at_once_is_one_wake_each_not_one_per_message() {
        // The damper that stops a crowded thread amplifying. Both answers land in the
        // same poll, so the third agent is woken once to go and read the thread rather
        // than once per message, and each answerer hears only the other.
        let mut state = state_with(&["session-a", "session-b", "session-c"]);
        state
            .subscription_mut("session-a", &thread())
            .unwrap()
            .record_authored("1");
        state
            .subscription_mut("session-b", &thread())
            .unwrap()
            .record_authored("2");

        plan(
            &mut state,
            &thread(),
            &[comment("1", "tupini07"), comment("2", "tupini07")],
            "tupini07",
            &[],
            12,
            Utc::now(),
        );
        let deliveries = recorded(&state);

        assert_eq!(
            deliveries.len(),
            3,
            "two messages among three agents, got: {deliveries:?}"
        );
        for session in ["session-a", "session-b", "session-c"] {
            assert_eq!(
                deliveries
                    .iter()
                    .filter(|delivery| matches!(
                        delivery,
                        Delivery::Wake { session_id, .. } if session_id == session
                    ))
                    .count(),
                1,
                "{session} should be woken exactly once, got: {deliveries:?}"
            );
        }
    }

    #[test]
    fn a_trusted_colleague_produces_a_real_wake_and_a_stranger_still_does_not() {
        // `classify` is tested on its own, but this is the level that decides whether a
        // session actually runs. Both directions in one place, because the interesting
        // property is that trusting somebody does not quietly trust everybody.
        let mut state = state_with(&["session-a"]);
        let trusted = vec!["a-colleague".to_string()];

        plan(
            &mut state,
            &thread(),
            &[comment("1", "a-colleague")],
            "tupini07",
            &trusted,
            12,
            Utc::now(),
        );
        assert_eq!(
            recorded(&state),
            vec![Delivery::Wake {
                session_id: "session-a".to_string(),
                thread: thread(),
            }]
        );

        // A fresh state for the other direction. Both comments on one thread would leave
        // the first notice in place — it already says "go and read this thread" — and
        // that would test the collapsing rule rather than the trust rule.
        let mut state = state_with(&["session-a"]);
        plan(
            &mut state,
            &thread(),
            &[comment("2", "a-stranger")],
            "tupini07",
            &trusted,
            12,
            Utc::now(),
        );
        assert!(recorded(&state).is_empty(), "got: {:?}", recorded(&state));
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
                &[],
                2,
                now,
            );
            // The session read it and the notice was dropped, which is the only way a
            // third wake can even be asked for: an undelivered notice would absorb the
            // next comment rather than count against the rate.
            state.drop_notice("session-a", &thread());
        }
        plan(
            &mut state,
            &thread(),
            &[comment("3", "tupini07")],
            "tupini07",
            &[],
            2,
            now,
        );

        assert_eq!(
            recorded(&state),
            vec![Delivery::Held {
                session_id: "session-a".to_string(),
                thread: thread(),
                reason: PendingReason::Throttled,
            }]
        );
    }

    #[test]
    fn joining_a_long_thread_does_not_wake_you_for_its_entire_history() {
        // Found by the live round trip: an agent that joined a thread with existing
        // comments was woken once per comment already on it.
        let mut state = ThreadState::default();
        state.subscribe("session-a", thread());

        let mut old = comment("1", "tupini07");
        old.created_at = Utc::now() - chrono::Duration::days(30);

        plan(
            &mut state,
            &thread(),
            &[old],
            "tupini07",
            &[],
            12,
            Utc::now(),
        );
        let deliveries = recorded(&state);

        assert!(deliveries.is_empty(), "got: {deliveries:?}");
    }

    #[test]
    fn a_reply_that_lands_after_you_joined_still_wakes_you() {
        // The other half of the rule above: skipping history must not skip the answer.
        let mut state = ThreadState::default();
        state.subscribe("session-a", thread());

        let mut reply = comment("2", "tupini07");
        reply.created_at = Utc::now() + chrono::Duration::seconds(1);

        plan(
            &mut state,
            &thread(),
            &[reply],
            "tupini07",
            &[],
            12,
            Utc::now(),
        );
        let deliveries = recorded(&state);

        assert_eq!(deliveries.len(), 1, "got: {deliveries:?}");
    }

    #[test]
    fn a_paused_subscription_is_skipped_without_losing_its_place() {
        let mut state = state_with(&["session-a"]);
        state
            .subscription_mut("session-a", &thread())
            .unwrap()
            .state = super::super::SubscriptionState::Paused;

        plan(
            &mut state,
            &thread(),
            &[comment("1", "tupini07")],
            "tupini07",
            &[],
            12,
            Utc::now(),
        );
        let deliveries = recorded(&state);

        assert!(deliveries.is_empty());
        // The comment was not marked seen, so resuming picks it up rather than
        // silently skipping everything that arrived while paused.
        assert!(!state
            .subscription_mut("session-a", &thread())
            .unwrap()
            .has_seen("1"));
    }

    /// Proves the comment fetch against a real thread, read-only.
    ///
    /// `--paginate --slurp` wraps each page in an outer array, `since` filters by
    /// `updated_at`, and a comment from a deleted account has no `user`. None of that is
    /// visible in a hand-written fixture, and all of it decides whether a real thread
    /// produces sightings or an error.
    #[test]
    #[ignore = "reads a real public GitHub thread; run with CST_THREADS_LIVE=1"]
    fn comments_on_a_real_thread_parse_into_sightings() {
        if std::env::var_os("CST_THREADS_LIVE").is_none() {
            return;
        }
        let thread = ThreadRef {
            host: "github.com".to_string(),
            owner: "rust-lang".to_string(),
            repo: "rust".to_string(),
            number: 100_000,
            kind: ThreadKind::Issue,
        };

        let comments = fetch_comments(
            &thread,
            std::env::temp_dir(),
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .expect("the fetch should succeed against a real thread");

        // A thread with more comments than one page, so `--paginate --slurp` is really
        // exercised. Asserting a lower bound rather than an exact count keeps this from
        // failing because somebody replied to a ten-year-old issue.
        assert!(
            comments.len() > 100,
            "expected a multi-page thread, got {}",
            comments.len()
        );
        for comment in &comments {
            assert!(!comment.id.is_empty(), "every comment needs an id");
            assert!(comment.url.contains("github.com"), "got: {}", comment.url);
        }
        // Ids must be distinct, or `seen_comment_ids` would silently swallow replies.
        let mut ids: Vec<&String> = comments.iter().map(|comment| &comment.id).collect();
        ids.sort();
        let total = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), total, "comment ids must be unique");
        println!("fetched {total} comment(s) across pages");
    }

    /// The whole loop against real GitHub: one agent posts, the other is woken.
    ///
    /// This is the test that found the original design was broken. The first doorbell
    /// asked `GET /notifications` once for everything, which is cheaper and does not
    /// work: GitHub never reports your own activity, and every CST agent comments as the
    /// same account, so the inbox stayed silent for exactly the traffic this feature
    /// exists to carry. Nothing offline could have shown that.
    ///
    /// Deliberately transport-agnostic — it takes whatever URL it is given. Run against
    /// an issue and against a discussion, which share no code below `cli::post`: one
    /// posts through REST and polls with `If-None-Match`, the other posts through a
    /// GraphQL mutation and polls by comparing `updatedAt`.
    ///
    /// Posts a real comment on the configured thread each time it runs.
    #[test]
    #[ignore = "posts to a real GitHub issue; run with CST_THREADS_ROUNDTRIP=<issue url>"]
    fn one_agent_posting_wakes_the_other_against_real_github() {
        let Ok(url) = std::env::var("CST_THREADS_ROUNDTRIP") else {
            return;
        };
        let thread = super::super::parse_thread_url(&url).expect("a thread URL");
        let root = tempfile::tempdir().unwrap();
        let login = doorbell::current_login(&thread.host).expect("gh must be logged in");
        let token = doorbell::token_for(&thread.host).expect("gh must have a token");
        let transport = UreqTransport::new();

        // Agent A is waiting on this thread; agent B is about to answer.
        store::update_in(root.path(), |state| {
            state.subscribe("live-a", thread.clone());
        })
        .unwrap();

        // Settle the cursor first, so the poll after B posts is a real change and not
        // just the first sighting of the thread.
        thread_moved(&transport, root.path(), &thread, &token).unwrap();
        assert!(
            !thread_moved(&transport, root.path(), &thread, &token).unwrap(),
            "an unchanged thread must report nothing on the second look"
        );

        super::super::cli::post(
            root.path(),
            "live-b",
            &url,
            &format!(
                "Round-trip check at {}. Agent B is blocked and needs the thing.",
                Utc::now().to_rfc3339()
            ),
        )
        .expect("posting should succeed");

        assert!(
            thread_moved(&transport, root.path(), &thread, &token).unwrap(),
            "a comment from our own account must be visible; this is what the \
             notifications inbox could not see"
        );

        let comments = fetch_comments(
            &thread,
            root.path().to_path_buf(),
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .expect("fetching comments should succeed");

        store::update_in(root.path(), |state| {
            plan(state, &thread, &comments, &login, &[], 12, Utc::now())
        })
        .unwrap();
        let deliveries = recorded(&store::load_in(root.path()));

        assert!(
            deliveries.contains(&Delivery::Wake {
                session_id: "live-a".to_string(),
                thread: thread.clone(),
            }),
            "agent A should have been woken, got: {deliveries:?}"
        );
        assert!(
            !deliveries.iter().any(|delivery| matches!(
                delivery,
                Delivery::Wake { session_id, .. } if session_id == "live-b"
            )),
            "agent B must not be woken by its own comment, got: {deliveries:?}"
        );
        println!("round trip OK: {deliveries:?}");
    }

    /// A real comment, then the process goes away before anything is delivered.
    ///
    /// The offline tests seed a notice and restart around it. This one starts where the
    /// bug started: a comment GitHub actually issued an id for, marked seen by the same
    /// write that planned the notice. That coupling is the whole hazard — once the
    /// comment is seen, a lost notice is a message nobody is ever told about again — and
    /// it only exists on the real path, where the id comes from GitHub rather than a
    /// fixture.
    ///
    /// Posts a real comment on the configured thread each time it runs.
    #[test]
    #[ignore = "posts to a real GitHub issue; run with CST_THREADS_ROUNDTRIP=<issue url>"]
    fn a_notice_from_a_real_comment_survives_the_process_that_planned_it() {
        let Ok(url) = std::env::var("CST_THREADS_ROUNDTRIP") else {
            return;
        };
        let thread = super::super::parse_thread_url(&url).expect("a thread URL");
        let root = tempfile::tempdir().unwrap();
        let login = doorbell::current_login(&thread.host).expect("gh must be logged in");
        let token = doorbell::token_for(&thread.host).expect("gh must have a token");
        let transport = UreqTransport::new();

        store::update_in(root.path(), |state| {
            state.subscribe("live-a", thread.clone());
        })
        .unwrap();
        thread_moved(&transport, root.path(), &thread, &token).unwrap();

        super::super::cli::post(
            root.path(),
            "live-b",
            &url,
            &format!(
                "Restart-durability check at {}. Agent B is asking and then CST dies.",
                Utc::now().to_rfc3339()
            ),
        )
        .expect("posting should succeed");

        assert!(thread_moved(&transport, root.path(), &thread, &token).unwrap());
        let comments = fetch_comments(
            &thread,
            root.path().to_path_buf(),
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .expect("fetching comments should succeed");

        store::update_in(root.path(), |state| {
            plan(state, &thread, &comments, &login, &[], 12, Utc::now())
        })
        .unwrap();

        // The comment is already spent: nothing will ever raise it a second time.
        let after_planning = store::load_in(root.path());
        assert!(
            after_planning
                .subscriptions
                .iter()
                .any(|subscription| subscription.session_id == "live-a"
                    && comments
                        .iter()
                        .all(|comment| subscription.has_seen(&comment.id))),
            "the comments must be marked seen, which is what makes losing a notice fatal"
        );
        assert_eq!(
            after_planning
                .notices
                .iter()
                .find(|notice| notice.session_id == "live-a")
                .map(|notice| notice.status),
            Some(NoticeStatus::Planned),
            "got: {:?}",
            after_planning.notices
        );

        // The notice reaches a composer, which is as far as it gets: Copilot is mid-turn
        // and has not taken it. Then CST dies. Everything above this line — the queue and
        // the fact that one had been written — was in memory before this change.
        drop(after_planning);
        store::update_in(root.path(), |state| {
            state
                .notice_mut("live-a", &thread)
                .expect("the planned notice")
                .status = NoticeStatus::Sent;
        })
        .unwrap();

        store::resume_in(root.path(), |_| false).expect("the next run reads the same file");

        let after_restart = store::load_in(root.path());
        let survivor = after_restart
            .notices
            .iter()
            .find(|notice| notice.session_id == "live-a")
            .expect("the notice must outlive the run that planned it");
        assert_eq!(
            survivor.status,
            NoticeStatus::Planned,
            "a notice left in a composer that no longer exists must be queued again"
        );
        assert!(
            survivor.comment_url.starts_with(&format!(
                "https://{}/{}/{}",
                thread.host, thread.owner, thread.repo
            )),
            "and still point at the real comment, got: {}",
            survivor.comment_url
        );
        println!("survived a restart pointing at {}", survivor.comment_url);
    }

    /// The reported failure, reproduced against real GitHub.
    ///
    /// A notice held days earlier as "its session is closed" sat on the thread, and the
    /// replies that arrived after it were folded into it and never delivered. Offline
    /// tests cover `record_notice`; this one proves the whole path from a comment GitHub
    /// really issued to a notice queued for delivery again, with a clock young enough
    /// that the patience deadline does not hold it straight back.
    ///
    /// Posts a real comment on the configured thread each time it runs.
    #[test]
    #[ignore = "posts to a real GitHub issue; run with CST_THREADS_ROUNDTRIP=<issue url>"]
    fn a_real_reply_puts_a_notice_held_days_ago_back_in_the_queue() {
        let Ok(url) = std::env::var("CST_THREADS_ROUNDTRIP") else {
            return;
        };
        let thread = super::super::parse_thread_url(&url).expect("a thread URL");
        let root = tempfile::tempdir().unwrap();
        let login = doorbell::current_login(&thread.host).expect("gh must be logged in");
        let token = doorbell::token_for(&thread.host).expect("gh must have a token");
        let transport = UreqTransport::new();
        let held_at = Utc::now() - chrono::Duration::days(3);

        store::update_in(root.path(), |state| {
            state.subscribe("live-a", thread.clone());
            state.record_notice(Notice {
                session_id: "live-a".to_string(),
                thread: thread.clone(),
                comment_url: format!("{}#issuecomment-1", thread.url()),
                author: Some(login.clone()),
                status: NoticeStatus::Waiting {
                    reason: PendingReason::SessionClosed,
                },
                planned_at: held_at,
            });
        })
        .unwrap();
        thread_moved(&transport, root.path(), &thread, &token).unwrap();

        let report = super::super::cli::post(
            root.path(),
            "live-b",
            &url,
            &format!(
                "Held-notice check at {}. A reply after a notice was held three days ago.",
                Utc::now().to_rfc3339()
            ),
        )
        .expect("posting should succeed");
        let posted_url = report.lines().nth(1).expect("the report names the comment");

        assert!(thread_moved(&transport, root.path(), &thread, &token).unwrap());
        let comments = fetch_comments(
            &thread,
            root.path().to_path_buf(),
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .expect("fetching comments should succeed");
        store::update_in(root.path(), |state| {
            plan(state, &thread, &comments, &login, &[], 12, Utc::now())
        })
        .unwrap();

        let state = store::load_in(root.path());
        let notice = state
            .notices
            .iter()
            .find(|notice| notice.session_id == "live-a")
            .expect("still one notice for the thread");
        assert_eq!(notice.status, NoticeStatus::Planned, "got: {notice:?}");
        assert!(notice.planned_at > held_at, "the clock must restart");
        assert_eq!(notice.comment_url, posted_url);
        assert!(state.waiting_for_user().is_empty());
        println!("held notice re-queued, pointing at {}", notice.comment_url);
    }

    /// Strangers' comments on a real public thread, run through the rules.
    ///
    /// Read-only. Pretends a session joined the thread long ago, so every comment on it
    /// counts as new, and that we are a login that never commented there. With real
    /// authors, real ids and real timestamps, nothing may come of any of it — and
    /// naming one of those authors as trusted must still wake the session.
    #[test]
    #[ignore = "reads a real public GitHub thread; run with CST_THREADS_LIVE=1"]
    fn strangers_on_a_real_public_thread_wake_nothing_and_ask_nothing() {
        if std::env::var_os("CST_THREADS_LIVE").is_none() {
            return;
        }
        let thread = ThreadRef {
            host: "github.com".to_string(),
            owner: "rust-lang".to_string(),
            repo: "rust".to_string(),
            number: 100_000,
            kind: ThreadKind::Issue,
        };
        let comments = fetch_comments(
            &thread,
            std::env::temp_dir(),
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .expect("the fetch should succeed against a real thread");
        let nobody_here = "cst-live-test-login-that-never-commented";
        assert!(comments
            .iter()
            .all(|comment| !comment.author.eq_ignore_ascii_case(nobody_here)));
        let joined_long_ago = |state: &mut ThreadState| {
            state.subscribe("live-a", thread.clone());
            state
                .subscription_mut("live-a", &thread)
                .unwrap()
                .subscribed_at = DateTime::<Utc>::MIN_UTC;
        };

        let mut state = ThreadState::default();
        joined_long_ago(&mut state);
        let produced = plan(
            &mut state,
            &thread,
            &comments,
            nobody_here,
            &[],
            12,
            Utc::now(),
        );
        assert!(!produced, "got: {:?}", recorded(&state));
        assert!(state.notices.is_empty() && state.waiting_for_user().is_empty());

        let colleague = comments
            .last()
            .expect("a thread with comments")
            .author
            .clone();
        let mut state = ThreadState::default();
        joined_long_ago(&mut state);
        plan(
            &mut state,
            &thread,
            &comments,
            nobody_here,
            std::slice::from_ref(&colleague),
            12,
            Utc::now(),
        );
        assert_eq!(
            recorded(&state),
            vec![Delivery::Wake {
                session_id: "live-a".to_string(),
                thread: thread.clone(),
            }],
            "trusting {colleague} must still let them in"
        );
        println!(
            "{} real comments from strangers ignored; trusting {colleague} woke the session",
            comments.len()
        );
    }

    /// Three agents on one real thread: one posts, the other two hear it.
    ///
    /// The offline test covers the same rule, but not what happens when three
    /// subscriptions share one state file and the ids being compared are the ones GitHub
    /// actually issued. The author is recognised by a comment id recorded through the
    /// real posting path, so a mismatch there would show up as an agent woken by its own
    /// message — the failure that is hardest to spot by reading the code.
    ///
    /// Posts a real comment on the configured thread each time it runs.
    #[test]
    #[ignore = "posts to a real GitHub thread; run with CST_THREADS_MULTIPARTY=<url>"]
    fn a_crowded_thread_wakes_everyone_but_the_author_against_real_github() {
        let Ok(url) = std::env::var("CST_THREADS_MULTIPARTY") else {
            return;
        };
        let thread = super::super::parse_thread_url(&url).expect("a thread URL");
        let root = tempfile::tempdir().unwrap();
        let login = doorbell::current_login(&thread.host).expect("gh must be logged in");
        let token = doorbell::token_for(&thread.host).expect("gh must have a token");
        let transport = UreqTransport::new();

        for session in ["live-a", "live-b", "live-c"] {
            store::update_in(root.path(), |state| {
                state.subscribe(session, thread.clone());
            })
            .unwrap();
        }

        // Settle the cursor so the poll after the post is a real change.
        thread_moved(&transport, root.path(), &thread, &token).unwrap();

        super::super::cli::post(
            root.path(),
            "live-c",
            &url,
            &format!(
                "Crowded-thread check at {}. Agent C is asking the other two.",
                Utc::now().to_rfc3339()
            ),
        )
        .expect("posting should succeed");

        assert!(thread_moved(&transport, root.path(), &thread, &token).unwrap());
        let comments = fetch_comments(
            &thread,
            root.path().to_path_buf(),
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .expect("fetching comments should succeed");

        store::update_in(root.path(), |state| {
            plan(state, &thread, &comments, &login, &[], 12, Utc::now())
        })
        .unwrap();
        let deliveries = recorded(&store::load_in(root.path()));

        let woken: Vec<&str> = deliveries
            .iter()
            .filter_map(|delivery| match delivery {
                Delivery::Wake { session_id, .. } => Some(session_id.as_str()),
                Delivery::Held { .. } => None,
            })
            .collect();

        assert!(woken.contains(&"live-a"), "got: {deliveries:?}");
        assert!(woken.contains(&"live-b"), "got: {deliveries:?}");
        assert!(
            !woken.contains(&"live-c"),
            "the author must not be woken by its own question, got: {deliveries:?}"
        );
        println!("crowded thread woke: {woken:?}");
    }

    /// Discussions against real GitHub, read-only.
    ///
    /// The one transport with no conditional request behind it: GraphQL has no `ETag`,
    /// so a discussion is polled by comparing `updatedAt`. Their comments also carry
    /// opaque node ids rather than numbers, and their replies are nested rather than
    /// flat — which is why comment ids are strings and why `flatten` exists at all.
    /// None of that is visible in a fixture.
    #[test]
    #[ignore = "reads a real public discussion; run with CST_THREADS_LIVE=1"]
    fn a_real_discussion_polls_and_flattens_its_nested_replies() {
        if std::env::var_os("CST_THREADS_LIVE").is_none() {
            return;
        }
        let thread = ThreadRef {
            host: "github.com".to_string(),
            owner: "vercel".to_string(),
            repo: "next.js".to_string(),
            number: 10_640,
            kind: ThreadKind::Discussion,
        };
        let root = tempfile::tempdir().unwrap();
        let token = doorbell::token_for(&thread.host).expect("gh must have a token");
        let transport = UreqTransport::new();

        // First look establishes the cursor; the second must be quiet, which is the
        // whole `updatedAt` comparison working.
        assert!(
            thread_moved(&transport, root.path(), &thread, &token).unwrap(),
            "the first sighting of a thread always counts as movement"
        );
        assert!(
            !thread_moved(&transport, root.path(), &thread, &token).unwrap(),
            "an unchanged discussion must report nothing on the second look"
        );

        let comments = fetch_comments(
            &thread,
            root.path().to_path_buf(),
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .expect("fetching discussion comments should succeed");

        assert!(
            comments.len() > 5,
            "expected a busy discussion, got {}",
            comments.len()
        );
        // Replies are nested under their parent in GraphQL. If `flatten` were dropping
        // them, only top-level comments would arrive and a reply — which is exactly the
        // shape an answer takes — would never wake anybody.
        assert!(
            comments.len() > 30,
            "nested replies should be flattened in, got {}",
            comments.len()
        );
        for comment in &comments {
            assert!(!comment.id.is_empty(), "every comment needs an id");
            // Node ids, not numbers. This is why the id type is a string.
            assert!(
                comment.id.parse::<u64>().is_err(),
                "expected an opaque node id, got {}",
                comment.id
            );
        }
        let mut ids: Vec<&String> = comments.iter().map(|comment| &comment.id).collect();
        ids.sort();
        let total = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), total, "comment ids must be unique");
        println!("discussion: {total} comment(s) including nested replies");
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
