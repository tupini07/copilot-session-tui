//! Where thread subscriptions live on disk.
//!
//! Unlike every other piece of CST state, this file has **two writers**: the running TUI
//! and the short-lived `cst thread post` an agent runs in its own shell. Every
//! read-modify-write therefore goes through [`update_in`], which holds an `fs4` advisory
//! lock across the whole cycle — the same approach `events::hooks::append_event` uses for
//! the lifecycle journal. Without it, an agent posting while the doorbell is recording a
//! watermark would silently drop one of the two changes.
//!
//! The lock is a separate `threads.lock` file rather than the state file itself, because
//! saving replaces the state file by rename and a lock held on the old inode would
//! protect nothing.
//!
//! Kept beside `app-state.json` and out of `config.json` for the reasons spelled out in
//! [`crate::app_state`]: the config file is hand-edited and polled every second, so
//! machine-managed bookkeeping does not belong there.

use anyhow::{Context, Result};
use fs4::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::{PendingDelivery, Subscription, ThreadRef};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadState {
    #[serde(default)]
    pub subscriptions: Vec<Subscription>,

    /// Messages that arrived but were not delivered, waiting on the user.
    #[serde(default)]
    pub pending: Vec<PendingDelivery>,

    /// The `Last-Modified` last returned by `GET /notifications`.
    ///
    /// Our private cursor into the notifications inbox. CST deliberately never marks a
    /// notification read: that inbox is the user's own, shared with their browser, and
    /// marking read here would make their real notifications disappear there.
    #[serde(default)]
    pub notifications_cursor: Option<String>,

    /// Anything a newer CST wrote that this one does not understand, so downgrading does
    /// not silently discard it. Same approach as `UserConfig::extra`.
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

impl ThreadState {
    /// Every distinct thread that some session is actively subscribed to.
    ///
    /// The doorbell asks for this to decide whether it has any reason to poll at all.
    pub fn active_threads(&self) -> Vec<ThreadRef> {
        let mut threads: Vec<ThreadRef> = Vec::new();
        for subscription in self.subscriptions.iter().filter(|s| s.is_active()) {
            if !threads.contains(&subscription.thread) {
                threads.push(subscription.thread.clone());
            }
        }
        threads
    }

    /// Whether anything at all is subscribed, so an unused install never polls GitHub.
    pub fn has_active_subscription(&self) -> bool {
        self.subscriptions.iter().any(Subscription::is_active)
    }

    pub fn subscription_mut(
        &mut self,
        session_id: &str,
        thread: &ThreadRef,
    ) -> Option<&mut Subscription> {
        self.subscriptions
            .iter_mut()
            .find(|s| s.session_id == session_id && &s.thread == thread)
    }

    /// Subscribe a session, returning whether this was new.
    ///
    /// Idempotent: taking part in a thread twice is the normal case, and it must not
    /// create a second row that would then wake the session twice per comment.
    pub fn subscribe(&mut self, session_id: &str, thread: ThreadRef) -> bool {
        if let Some(existing) = self.subscription_mut(session_id, &thread) {
            // Re-subscribing is how a paused thread is resumed.
            existing.state = super::SubscriptionState::Active;
            return false;
        }
        self.subscriptions
            .push(Subscription::new(session_id, thread));
        true
    }

    /// Drop one session's interest, leaving every other subscriber untouched.
    ///
    /// This is `leave`, not `close`: an agent deciding a thread no longer concerns it
    /// must not silence the thread for the agent still working on it.
    pub fn leave(&mut self, session_id: &str, thread: &ThreadRef) -> bool {
        let before = self.subscriptions.len();
        self.subscriptions
            .retain(|s| !(s.session_id == session_id && &s.thread == thread));
        self.pending
            .retain(|p| !(p.session_id == session_id && &p.thread == thread));
        self.subscriptions.len() != before
    }

    /// Drop every local subscription to a thread, for when the correspondence is over.
    pub fn close(&mut self, thread: &ThreadRef) -> usize {
        let before = self.subscriptions.len();
        self.subscriptions.retain(|s| &s.thread != thread);
        self.pending.retain(|p| &p.thread != thread);
        before - self.subscriptions.len()
    }

    pub fn subscriptions_for(&self, session_id: &str) -> Vec<&Subscription> {
        self.subscriptions
            .iter()
            .filter(|s| s.session_id == session_id)
            .collect()
    }

    /// Queue a message for the user, collapsing repeats of the same thread.
    ///
    /// A thread that keeps moving while its session is closed should read as one item
    /// that is getting older, not as a pile that buries everything else in the list.
    pub fn hold_for_user(&mut self, delivery: PendingDelivery) {
        if let Some(existing) = self
            .pending
            .iter_mut()
            .find(|p| p.session_id == delivery.session_id && p.thread == delivery.thread)
        {
            existing.comment_url = delivery.comment_url;
            existing.reason = delivery.reason;
            return;
        }
        self.pending.push(delivery);
    }

    pub fn pending_for(&self, session_id: &str) -> Vec<&PendingDelivery> {
        self.pending
            .iter()
            .filter(|p| p.session_id == session_id)
            .collect()
    }

    pub fn clear_pending(&mut self, session_id: &str, thread: &ThreadRef) {
        self.pending
            .retain(|p| !(p.session_id == session_id && &p.thread == thread));
    }
}

/// Read the state, treating every failure as "nothing subscribed yet".
///
/// Deliberately infallible, for the same reason as [`crate::app_state::load_in`]: a
/// corrupt file must never stop CST from starting. The cost is that a damaged file loses
/// subscriptions, which the agents can recreate by posting again.
pub fn load_in(root: &Path) -> ThreadState {
    fs::read_to_string(state_path(root))
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default()
}

/// Read, change and save under one lock held for the whole cycle.
///
/// The only sanctioned way to modify the state. Taking the lock around just the write
/// would still lose changes: the losing writer would have read a copy that predates the
/// winner's change and then written it back wholesale.
pub fn update_in<T>(root: &Path, change: impl FnOnce(&mut ThreadState) -> T) -> Result<T> {
    let _guard = StateLock::acquire(root)?;
    let mut state = load_in(root);
    let outcome = change(&mut state);
    write_in(root, &state)?;
    Ok(outcome)
}

fn write_in(root: &Path, state: &ThreadState) -> Result<()> {
    let path = state_path(root);
    fs::create_dir_all(root)
        .with_context(|| format!("Failed to create state directory: {}", root.display()))?;
    let content = serde_json::to_vec_pretty(state)?;
    let mut temp = tempfile::NamedTempFile::new_in(root)?;
    temp.as_file_mut().write_all(&content)?;
    temp.as_file_mut().sync_all()?;
    temp.persist(&path)
        .map_err(|error| error.error)
        .with_context(|| format!("Failed to replace thread state: {}", path.display()))?;
    Ok(())
}

/// Blocking, unlike `ConfigLock`.
///
/// Refusing with "another process is saving" would be wrong here: `cst thread post` is
/// run by an agent mid-turn with no way to retry sensibly, and the contention window is
/// a single small file write.
struct StateLock {
    file: fs::File,
}

impl StateLock {
    fn acquire(root: &Path) -> Result<Self> {
        fs::create_dir_all(root)
            .with_context(|| format!("Failed to create state directory: {}", root.display()))?;
        let path = lock_path(root);
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("Failed to open thread lock {}", path.display()))?;
        FileExt::lock(&file).with_context(|| format!("Failed to lock {}", path.display()))?;
        Ok(Self { file })
    }
}

impl Drop for StateLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

fn state_path(root: &Path) -> PathBuf {
    root.join("threads.json")
}

fn lock_path(root: &Path) -> PathBuf {
    root.join("threads.lock")
}

/// Beside `app-state.json`, and reusing its root so the two never drift apart.
pub fn state_root() -> PathBuf {
    crate::app_state::state_root()
}

#[cfg(test)]
mod tests {
    use super::super::{PendingReason, ThreadKind};
    use super::*;

    fn thread(number: u64) -> ThreadRef {
        ThreadRef {
            host: "github.com".to_string(),
            owner: "o".to_string(),
            repo: "r".to_string(),
            number,
            kind: ThreadKind::Issue,
        }
    }

    #[test]
    fn a_machine_with_no_subscriptions_never_gives_the_doorbell_a_reason_to_poll() {
        let temp = tempfile::tempdir().unwrap();
        let state = load_in(temp.path());

        assert!(!state.has_active_subscription());
        assert!(state.active_threads().is_empty());
    }

    #[test]
    fn taking_part_in_a_thread_twice_does_not_wake_the_session_twice_per_comment() {
        let mut state = ThreadState::default();

        assert!(state.subscribe("session-a", thread(1)));
        assert!(!state.subscribe("session-a", thread(1)));
        assert_eq!(state.subscriptions.len(), 1);
    }

    #[test]
    fn leaving_a_thread_silences_it_for_one_session_and_not_for_the_other() {
        // An agent deciding a thread no longer concerns it must not cut off the agent
        // still working on it.
        let mut state = ThreadState::default();
        state.subscribe("session-a", thread(1));
        state.subscribe("session-b", thread(1));

        assert!(state.leave("session-a", &thread(1)));

        assert_eq!(state.subscriptions_for("session-a").len(), 0);
        assert_eq!(state.subscriptions_for("session-b").len(), 1);
        assert_eq!(
            state.active_threads().len(),
            1,
            "the thread is still watched"
        );
    }

    #[test]
    fn closing_a_thread_ends_it_for_everyone_unlike_leaving_it() {
        let mut state = ThreadState::default();
        state.subscribe("session-a", thread(1));
        state.subscribe("session-b", thread(1));
        state.subscribe("session-a", thread(2));

        assert_eq!(state.close(&thread(1)), 2);

        assert!(state.active_threads().contains(&thread(2)));
        assert!(!state.active_threads().contains(&thread(1)));
    }

    #[test]
    fn one_thread_that_keeps_moving_stays_one_ageing_item_rather_than_a_pile() {
        let mut state = ThreadState::default();
        let arrived = chrono::Utc::now() - chrono::Duration::days(3);
        for id in 1..=4 {
            state.hold_for_user(PendingDelivery {
                session_id: "session-a".to_string(),
                thread: thread(1),
                comment_url: format!("https://github.com/o/r/issues/1#issuecomment-{id}"),
                reason: PendingReason::SessionClosed,
                arrived_at: arrived,
            });
        }

        let pending = state.pending_for("session-a");
        assert_eq!(pending.len(), 1);
        // The age must survive the collapse, or a long wait would look brand new.
        assert_eq!(pending[0].arrived_at, arrived);
        assert!(
            pending[0].comment_url.ends_with("-4"),
            "newest comment wins"
        );
    }

    #[test]
    fn a_change_is_readable_by_the_next_process_to_open_the_file() {
        let temp = tempfile::tempdir().unwrap();
        update_in(temp.path(), |state| {
            state.subscribe("session-a", thread(7));
        })
        .unwrap();

        let reloaded = load_in(temp.path());
        assert_eq!(reloaded.subscriptions.len(), 1);
        assert_eq!(reloaded.subscriptions[0].thread, thread(7));
    }

    #[test]
    fn a_corrupt_file_is_treated_as_a_fresh_start_rather_than_failing_startup() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(state_path(temp.path()), "{not json").unwrap();

        assert_eq!(load_in(temp.path()), ThreadState::default());

        // And it repairs itself on the next write rather than staying broken.
        update_in(temp.path(), |state| state.subscribe("session-a", thread(1))).unwrap();
        assert_eq!(load_in(temp.path()).subscriptions.len(), 1);
    }

    #[test]
    fn a_field_written_by_a_newer_cst_survives_a_save_by_an_older_one() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path()).unwrap();
        fs::write(
            state_path(temp.path()),
            r#"{"subscriptions":[],"something_newer":{"kept":true}}"#,
        )
        .unwrap();

        update_in(temp.path(), |state| state.subscribe("session-a", thread(1))).unwrap();

        let written = fs::read_to_string(state_path(temp.path())).unwrap();
        assert!(written.contains("something_newer"), "got: {written}");
    }

    #[test]
    fn the_notifications_cursor_survives_a_restart_so_the_inbox_is_not_replayed() {
        let temp = tempfile::tempdir().unwrap();
        update_in(temp.path(), |state| {
            state.notifications_cursor = Some("Mon, 14 Sep 2026 16:04:34 GMT".to_string());
        })
        .unwrap();

        assert_eq!(
            load_in(temp.path()).notifications_cursor.as_deref(),
            Some("Mon, 14 Sep 2026 16:04:34 GMT")
        );
    }
}
