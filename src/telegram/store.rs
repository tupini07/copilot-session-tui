//! Where the Telegram channel keeps its state on disk.
//!
//! Several CSTs write this file: whichever one is polling records what arrived, and
//! whichever one has a session open records that it delivered. So every change goes
//! through [`update_in`] under an `fs4` lock held for the whole read-modify-write, the
//! same discipline as `threads::store` — a copy of it rather than a shared helper, because
//! that module's shape is held up by security tests and this one should not reach into it.
//!
//! The lock is a separate `telegram.lock` because saving replaces the state file by
//! rename, and a lock on the old inode would protect nothing.

use anyhow::{Context, Result};
use fs4::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use super::{Paired, SentRecord, TopicRecord};

/// How many sent messages are remembered for reply routing.
///
/// Small on purpose: the file is pretty-printed, fsynced on every write, and re-read by
/// every CST whenever its modification time changes. A few hundred covers a day of
/// notifications; a reply to anything older gets the "reply to a session's message" hint.
const SENT_HISTORY: usize = 300;

/// A pairing in progress or complete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Pairing {
    /// A code was shown; the poller is waiting for someone to send it.
    Armed {
        code: String,
        expires_at: DateTime<Utc>,
    },
    /// Somebody sent the code. The poller recorded who and where.
    Paired(Paired),
}

/// A message from the phone that CST still has to do something about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    pub update_id: i64,
    /// The user's own message, so delivery can be confirmed with a reaction on it.
    pub user_message_id: i64,
    pub session_id: String,
    pub text: String,
    /// The question this answers, when it was a reply to one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    pub received_at: DateTime<Utc>,
    /// Restarted while the pane works, so a long turn is not aged out by an instance
    /// that is not the host.
    pub planned_at: DateTime<Utc>,
    /// Whether the phone has been told this is waiting on a turn, so it is told once.
    #[serde(default)]
    pub queued_told: bool,
    #[serde(default)]
    pub status: ReplyStatus,
    /// A `Sent` reply that was keyed into a question's form rather than a composer.
    /// Dropped once the question is no longer pending, which is the evidence the form
    /// took it. A flag beside `Sent` rather than a third status, because this file is
    /// shared with every CST on the machine and an older one reading a status it does
    /// not know would throw the whole state away; a flag it does not know is ignored,
    /// and it treats the reply as any other sent one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub answering: bool,
}

impl Reply {
    /// How long this has been waiting since it was last (re)planned.
    pub fn age_seconds(&self) -> i64 {
        (Utc::now() - self.planned_at).num_seconds()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplyStatus {
    #[default]
    Planned,
    /// Written into a composer. Dropped once the composer empties.
    Sent,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelegramState {
    /// The next `getUpdates` offset. Read from here before every poll, so a CST that
    /// takes over polling carries on where the last one stopped.
    #[serde(default)]
    pub offset: Option<i64>,
    #[serde(default)]
    pub pairing: Option<Pairing>,
    #[serde(default)]
    pub sent: Vec<SentRecord>,
    /// Forum topic per session, keyed by session id.
    #[serde(default)]
    pub topics: BTreeMap<String, TopicRecord>,
    #[serde(default)]
    pub replies: Vec<Reply>,
    /// Anything a newer CST wrote that this one does not understand.
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

impl TelegramState {
    pub fn paired(&self) -> Option<Paired> {
        match self.pairing {
            Some(Pairing::Paired(paired)) => Some(paired),
            _ => None,
        }
    }

    /// The code the poller is waiting for, if one is still valid.
    pub fn armed_code(&self, now: DateTime<Utc>) -> Option<&str> {
        match &self.pairing {
            Some(Pairing::Armed { code, expires_at }) if *expires_at > now => Some(code),
            _ => None,
        }
    }

    /// Remember a message CST sent, forgetting the oldest beyond the history bound.
    pub fn record_sent(&mut self, record: SentRecord) {
        self.sent.push(record);
        if self.sent.len() > SENT_HISTORY {
            let excess = self.sent.len() - SENT_HISTORY;
            self.sent.drain(..excess);
        }
    }

    /// Record a reply once. The same update seen twice (a crash between the write and
    /// the next poll) must not become two prompts.
    pub fn record_reply(&mut self, reply: Reply) -> bool {
        if self
            .replies
            .iter()
            .any(|existing| existing.update_id == reply.update_id)
        {
            return false;
        }
        self.replies.push(reply);
        true
    }

    pub fn reply_mut(&mut self, update_id: i64) -> Option<&mut Reply> {
        self.replies
            .iter_mut()
            .find(|reply| reply.update_id == update_id)
    }

    pub fn drop_reply(&mut self, update_id: i64) {
        self.replies.retain(|reply| reply.update_id != update_id);
    }

    /// Forget a session's topic, for when the session is gone.
    pub fn forget_topic(&mut self, session_id: &str) -> Option<TopicRecord> {
        self.topics.remove(session_id)
    }

    /// What a restart does to replies mid-flight.
    ///
    /// Opposite of `threads::store`: a thread notice that was in a composer is queued
    /// again because a link twice is harmless. A reply typed again could be submitted
    /// twice, so one that reached a composer is dropped — the user's message is still
    /// in Telegram, and nothing was lost that they cannot see.
    fn resume_after_restart(&mut self, still_open: impl Fn(&str) -> bool) {
        self.replies
            .retain(|reply| reply.status != ReplyStatus::Sent || still_open(&reply.session_id));
    }
}

/// Read the state, treating every failure as "nothing yet".
///
/// Infallible like `threads::store::load_in`: a damaged file must not stop CST, and the
/// worst it costs is forgetting which messages were sent, which the hint covers.
pub fn load_in(root: &Path) -> TelegramState {
    fs::read_to_string(state_path(root))
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default()
}

/// Load once at startup and settle what a previous run left in flight.
pub fn resume_in(root: &Path, still_open: impl Fn(&str) -> bool) -> Result<()> {
    update_in(root, |state| state.resume_after_restart(still_open))
}

/// Read, change and save under one lock held for the whole cycle.
pub fn update_in<T>(root: &Path, change: impl FnOnce(&mut TelegramState) -> T) -> Result<T> {
    let _guard = StateLock::acquire(root)?;
    let mut state = load_in(root);
    let outcome = change(&mut state);
    write_in(root, &state)?;
    Ok(outcome)
}

fn write_in(root: &Path, state: &TelegramState) -> Result<()> {
    let path = state_path(root);
    fs::create_dir_all(root)
        .with_context(|| format!("Failed to create state directory: {}", root.display()))?;
    let content = serde_json::to_vec_pretty(state)?;
    let mut temp = tempfile::NamedTempFile::new_in(root)?;
    temp.as_file_mut().write_all(&content)?;
    temp.as_file_mut().sync_all()?;
    temp.persist(&path)
        .map_err(|error| error.error)
        .with_context(|| format!("Failed to replace Telegram state: {}", path.display()))?;
    Ok(())
}

/// Blocking: the contention window is one small file write, and every writer here is a
/// background thread that can afford to wait for it.
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
            .with_context(|| format!("Failed to open Telegram lock {}", path.display()))?;
        FileExt::lock(&file).with_context(|| format!("Failed to lock {}", path.display()))?;
        Ok(Self { file })
    }
}

impl Drop for StateLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

/// When the state was last written, by anyone. How a CST that is not polling learns
/// that a reply arrived for a session only it has open.
pub fn modified_in(root: &Path) -> Option<std::time::SystemTime> {
    fs::metadata(state_path(root))
        .and_then(|metadata| metadata.modified())
        .ok()
}

fn state_path(root: &Path) -> PathBuf {
    root.join("telegram.json")
}

fn lock_path(root: &Path) -> PathBuf {
    root.join("telegram.lock")
}

/// The file whose exclusive lock says "I am the one polling Telegram".
///
/// Held open for as long as a worker polls; released by the OS when the process ends,
/// however it ends, so a crashed poller never blocks the next one.
pub fn poller_lock_path(root: &Path) -> PathBuf {
    root.join("telegram-poller.lock")
}

/// Beside `threads.json`, under the same root, so `CST_HOME` isolates both.
pub fn state_root() -> PathBuf {
    crate::app_state::state_root()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TelegramChatMode;
    use crate::telegram::SentKind;

    fn sent(message_id: i64) -> SentRecord {
        SentRecord {
            message_id,
            session_id: "s".to_string(),
            kind: Some(SentKind::Ready),
            tool_call_id: None,
            sent_at: Utc::now(),
        }
    }

    fn reply(update_id: i64, status: ReplyStatus) -> Reply {
        Reply {
            update_id,
            user_message_id: update_id * 10,
            session_id: "s".to_string(),
            text: "hello".to_string(),
            tool_call_id: None,
            received_at: Utc::now(),
            planned_at: Utc::now(),
            queued_told: false,
            answering: false,
            status,
        }
    }

    #[test]
    fn a_missing_or_corrupt_file_reads_as_nothing_and_the_next_write_repairs_it() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(load_in(temp.path()), TelegramState::default());

        fs::write(state_path(temp.path()), "{not json").unwrap();
        assert_eq!(load_in(temp.path()), TelegramState::default());

        update_in(temp.path(), |state| state.offset = Some(5)).unwrap();
        assert_eq!(load_in(temp.path()).offset, Some(5));
    }

    #[test]
    fn sent_history_is_bounded_and_keeps_the_newest() {
        let mut state = TelegramState::default();
        for id in 0..(SENT_HISTORY as i64 + 50) {
            state.record_sent(sent(id));
        }
        assert_eq!(state.sent.len(), SENT_HISTORY);
        assert_eq!(state.sent.first().unwrap().message_id, 50);
    }

    #[test]
    fn the_same_update_recorded_twice_is_one_reply() {
        // A crash between writing the reply and the next poll re-fetches the update.
        let mut state = TelegramState::default();
        assert!(state.record_reply(reply(1, ReplyStatus::Planned)));
        assert!(!state.record_reply(reply(1, ReplyStatus::Planned)));
        assert_eq!(state.replies.len(), 1);
    }

    #[test]
    fn a_restart_drops_replies_that_reached_a_composer_unless_another_cst_still_has_it() {
        // The opposite of thread notices: a reply typed again could be submitted twice.
        let temp = tempfile::tempdir().unwrap();
        update_in(temp.path(), |state| {
            state.record_reply(reply(1, ReplyStatus::Sent));
            state.record_reply(reply(2, ReplyStatus::Planned));
            let mut elsewhere = reply(3, ReplyStatus::Sent);
            elsewhere.session_id = "open-elsewhere".to_string();
            state.record_reply(elsewhere);
        })
        .unwrap();

        resume_in(temp.path(), |session| session == "open-elsewhere").unwrap();

        let ids: Vec<i64> = load_in(temp.path())
            .replies
            .iter()
            .map(|reply| reply.update_id)
            .collect();
        assert_eq!(ids, vec![2, 3]);
    }

    #[test]
    fn an_armed_code_expires_and_a_pairing_survives_a_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        let now = Utc::now();
        update_in(temp.path(), |state| {
            state.pairing = Some(Pairing::Armed {
                code: "ABC234".to_string(),
                expires_at: now + chrono::Duration::minutes(5),
            });
        })
        .unwrap();
        let state = load_in(temp.path());
        assert_eq!(state.armed_code(now), Some("ABC234"));
        assert_eq!(state.armed_code(now + chrono::Duration::minutes(6)), None);
        assert_eq!(state.paired(), None);

        let paired = Paired {
            chat_id: -100,
            user_id: 7,
            mode: TelegramChatMode::Forum,
        };
        update_in(temp.path(), |state| {
            state.pairing = Some(Pairing::Paired(paired));
        })
        .unwrap();
        assert_eq!(load_in(temp.path()).paired(), Some(paired));
    }

    #[test]
    fn fields_from_a_newer_cst_survive_a_rewrite_by_this_one() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path()).unwrap();
        fs::write(
            state_path(temp.path()),
            r#"{"offset": 3, "future_thing": {"a": 1}}"#,
        )
        .unwrap();

        update_in(temp.path(), |state| state.offset = Some(4)).unwrap();

        let raw: Value =
            serde_json::from_str(&fs::read_to_string(state_path(temp.path())).unwrap()).unwrap();
        assert_eq!(raw["offset"], 4);
        assert_eq!(raw["future_thing"]["a"], 1);
    }
}
