//! Setting Telegram up, one checked step at a time.
//!
//! Getting a bot talking takes several things done by hand in two apps — a token from
//! BotFather, a chat or a group, admin rights for topics — and each can go wrong on its
//! own. So this is a checklist rather than a form: every row is verified against
//! Telegram, shows why it failed, and can be retried alone. Re-opening it on a working
//! setup shows every row done, and fixing one broken thing means redoing one row.
//!
//! The state machine here is pure. Each check runs on a background thread through
//! [`Runner`], which the UI thread polls, so a slow network never freezes the screen. The
//! checks themselves are the ones `cst doctor` and `cst telegram setup` run, so there is
//! one definition of "configured".

use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use chrono::{DateTime, Utc};

use super::bot::{BotApi, BotError, UreqBot};
use super::{deep_link, wording, Paired};
use crate::config::{TelegramChatMode, TelegramContent, UserConfig};

/// The rows, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    /// The BotFather token, checked with `getMe`.
    Token,
    /// Private chat or group with topics.
    Mode,
    /// Sending the code: a deep link, and the wait for it to arrive.
    Pair,
    /// Group only: the bot is an admin who may manage topics.
    Rights,
    /// One real message, and in a group one topic made and closed.
    Test,
    /// Content level and replies.
    Content,
}

impl StepKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Token => "Bot token",
            Self::Mode => "Chat type",
            Self::Pair => "Pair the chat",
            Self::Rights => "Topic rights",
            Self::Test => "Test message",
            Self::Content => "What is sent",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepState {
    Pending,
    Running,
    Done(String),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub kind: StepKind,
    pub state: StepState,
}

/// A code the user is being asked to send, and the link that sends it for them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingPrompt {
    pub code: String,
    pub deep_link: String,
    pub expires_at: DateTime<Utc>,
}

/// What a background check came back with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Identity {
        id: i64,
        username: String,
    },
    /// Topics may be managed; `may_pin` says whether a new topic's first message can
    /// be unpinned too, which is cosmetic.
    RightsOk {
        may_pin: bool,
    },
    TestOk,
    Failed(StepKind, String),
}

pub struct Wizard {
    pub steps: Vec<Step>,
    pub cursor: usize,
    pub mode: TelegramChatMode,
    /// Typed or pasted here rather than in the settings row: the wizard swallows keys
    /// first, and the settings paste arm only feeds a row being edited.
    pub token_input: String,
    pub editing_token: bool,
    pub bot_id: Option<i64>,
    pub bot_username: Option<String>,
    pub pairing: Option<PairingPrompt>,
    pub paired: Option<Paired>,
    /// Whether pairing completed in this wizard session, as opposed to being read from
    /// the settings. Only a fresh pairing turns the channel on: opening the wizard on a
    /// deliberately disabled setup and closing it must not switch it back on.
    pub paired_now: bool,
    pub content: TelegramContent,
    pub replies: bool,
    runner: Option<Runner>,
}

impl Wizard {
    /// Start from what is already configured, so a working setup reads as done.
    pub fn from_config(config: &UserConfig) -> Self {
        let telegram = &config.telegram;
        let paired = match (telegram.chat_id, telegram.user_id) {
            (Some(chat_id), Some(user_id)) => Some(Paired {
                chat_id,
                user_id,
                mode: telegram.chat_mode,
            }),
            _ => None,
        };
        let mode = telegram.chat_mode;
        let mut wizard = Self {
            steps: Vec::new(),
            cursor: 0,
            mode,
            token_input: config.telegram_bot_token.trim().to_string(),
            editing_token: false,
            bot_id: None,
            bot_username: None,
            pairing: None,
            paired,
            paired_now: false,
            content: telegram.content,
            replies: telegram.replies,
            runner: None,
        };
        wizard.rebuild_steps();
        if paired.is_some() {
            wizard.set_state(StepKind::Pair, StepState::Done(Self::paired_detail(paired)));
        }
        wizard.set_state(StepKind::Mode, StepState::Done(mode.label().to_string()));
        wizard
    }

    /// The rows for the chosen mode. Rights exists only for a group.
    fn rebuild_steps(&mut self) {
        let kinds: Vec<StepKind> = match self.mode {
            TelegramChatMode::Private => vec![
                StepKind::Token,
                StepKind::Mode,
                StepKind::Pair,
                StepKind::Test,
                StepKind::Content,
            ],
            TelegramChatMode::Forum => vec![
                StepKind::Token,
                StepKind::Mode,
                StepKind::Pair,
                StepKind::Rights,
                StepKind::Test,
                StepKind::Content,
            ],
        };
        let previous = std::mem::take(&mut self.steps);
        self.steps = kinds
            .into_iter()
            .map(|kind| Step {
                kind,
                state: previous
                    .iter()
                    .find(|step| step.kind == kind)
                    .map(|step| step.state.clone())
                    .unwrap_or(StepState::Pending),
            })
            .collect();
        self.cursor = self.cursor.min(self.steps.len() - 1);
    }

    pub fn current(&self) -> &Step {
        &self.steps[self.cursor.min(self.steps.len() - 1)]
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let next = (self.cursor as isize + delta).clamp(0, self.steps.len() as isize - 1);
        self.cursor = next as usize;
    }

    pub fn state_of(&self, kind: StepKind) -> Option<&StepState> {
        self.steps
            .iter()
            .find(|step| step.kind == kind)
            .map(|step| &step.state)
    }

    fn set_state(&mut self, kind: StepKind, state: StepState) {
        if let Some(step) = self.steps.iter_mut().find(|step| step.kind == kind) {
            step.state = state;
        }
    }

    pub fn is_busy(&self) -> bool {
        self.runner.is_some() || self.pairing.is_some()
    }

    fn paired_detail(paired: Option<Paired>) -> String {
        match paired {
            Some(paired) => format!("{} (chat {})", paired.mode.label(), paired.chat_id),
            None => String::new(),
        }
    }

    /// Choose the chat type. Changing it after pairing means pairing again: the chat
    /// id that was paired belongs to the other kind.
    pub fn choose_mode(&mut self, mode: TelegramChatMode) {
        if mode != self.mode {
            self.mode = mode;
            self.paired = None;
            self.pairing = None;
            self.set_state(StepKind::Pair, StepState::Pending);
            self.set_state(StepKind::Rights, StepState::Pending);
            self.set_state(StepKind::Test, StepState::Pending);
        }
        self.set_state(StepKind::Mode, StepState::Done(mode.label().to_string()));
        self.rebuild_steps();
    }

    pub fn cycle_content(&mut self) {
        self.content = self.content.next();
        self.set_state(StepKind::Content, StepState::Done(self.content_detail()));
    }

    pub fn toggle_replies(&mut self) {
        self.replies = !self.replies;
        self.set_state(StepKind::Content, StepState::Done(self.content_detail()));
    }

    fn content_detail(&self) -> String {
        format!(
            "{}, replies {}",
            self.content.settings_label().to_lowercase(),
            if self.replies { "on" } else { "off" }
        )
    }

    /// Note that a code has been armed and the user is being asked to send it.
    pub fn await_pairing(&mut self, code: String, expires_at: DateTime<Utc>) {
        let link = self
            .bot_username
            .as_deref()
            .map(|username| deep_link(username, &code, self.mode))
            .unwrap_or_default();
        self.pairing = Some(PairingPrompt {
            code,
            deep_link: link,
            expires_at,
        });
        self.set_state(StepKind::Pair, StepState::Running);
    }

    /// The poller recorded who sent the code.
    pub fn pairing_arrived(&mut self, paired: Paired) {
        self.pairing = None;
        if paired.mode != self.mode {
            // The user chose "group" and then pressed Start on a private chat, say.
            // What they paired is what counts; the rows follow it.
            self.mode = paired.mode;
            self.set_state(
                StepKind::Mode,
                StepState::Done(paired.mode.label().to_string()),
            );
            self.rebuild_steps();
        }
        self.paired = Some(paired);
        self.paired_now = true;
        self.set_state(
            StepKind::Pair,
            StepState::Done(Self::paired_detail(Some(paired))),
        );
    }

    /// The code was never sent in time.
    pub fn pairing_expired(&mut self) {
        self.pairing = None;
        self.set_state(
            StepKind::Pair,
            StepState::Failed(
                "No message with the code arrived in time. Press Enter to try again.".to_string(),
            ),
        );
    }

    /// Fold a background check's result in.
    pub fn apply(&mut self, outcome: Outcome) {
        self.runner = None;
        match outcome {
            Outcome::Identity { id, username } => {
                self.bot_id = Some(id);
                self.bot_username = Some(username.clone());
                self.set_state(StepKind::Token, StepState::Done(format!("@{username}")));
            }
            Outcome::RightsOk { may_pin: true } => self.set_state(
                StepKind::Rights,
                StepState::Done("admin, may manage topics".to_string()),
            ),
            Outcome::RightsOk { may_pin: false } => self.set_state(
                StepKind::Rights,
                // The hint under the rows says what the missing right costs; the row
                // has room for a verdict, not a paragraph.
                StepState::Done("admin, may manage topics, not pin".to_string()),
            ),
            Outcome::TestOk => {
                self.set_state(StepKind::Test, StepState::Done("delivered".to_string()))
            }
            Outcome::Failed(kind, reason) => self.set_state(kind, StepState::Failed(reason)),
        }
    }

    /// What the current step needs before its check can run, as a sentence for the
    /// user, or `None` when it can run now.
    pub fn blocker(&self, kind: StepKind) -> Option<String> {
        let token_done = matches!(self.state_of(StepKind::Token), Some(StepState::Done(_)));
        match kind {
            StepKind::Token | StepKind::Mode | StepKind::Content => None,
            StepKind::Pair if !token_done => Some("Check the bot token first.".to_string()),
            StepKind::Pair => None,
            StepKind::Rights | StepKind::Test if !token_done => {
                Some("Check the bot token first.".to_string())
            }
            StepKind::Rights | StepKind::Test if self.paired.is_none() => {
                Some("Pair the chat first.".to_string())
            }
            StepKind::Rights | StepKind::Test => None,
        }
    }

    /// Whether the setup is complete enough to use.
    pub fn can_finish(&self) -> bool {
        let done = |kind: StepKind| matches!(self.state_of(kind), Some(StepState::Done(_)));
        done(StepKind::Token)
            && self.paired.is_some()
            && (self.mode == TelegramChatMode::Private || done(StepKind::Rights))
    }

    /// What the user is told to do for the current step.
    pub fn instructions(&self) -> Vec<String> {
        match self.current().kind {
            StepKind::Token => vec![
                "In Telegram, open @BotFather and send /newbot. Give the bot a name and a".to_string(),
                "username ending in \"bot\", then paste the token it sends you here.".to_string(),
                "A bot used by another program cannot be shared; make a new one for CST.".to_string(),
                String::new(),
                if self.token_input.trim().is_empty() {
                    "Enter: type or paste the token, then Enter again to check it".to_string()
                } else {
                    "Enter: check the token  ·  e: change it".to_string()
                },
            ],
            StepKind::Mode => vec![
                "p: private chat — every session in one conversation; reply to a message to answer it.".to_string(),
                "g: group with topics — one topic per session; type in the topic to answer it.".to_string(),
                "Running many sessions at once? Choose the group.".to_string(),
            ],
            StepKind::Pair => match (&self.pairing, self.mode) {
                (Some(prompt), TelegramChatMode::Private) => vec![
                    "Open this link and press Start; that sends the code for you:".to_string(),
                    format!("  {}", prompt.deep_link),
                    format!("Or send the bot this code yourself: {}", prompt.code),
                    "Waiting for it to arrive…".to_string(),
                ],
                (Some(prompt), TelegramChatMode::Forum) => vec![
                    "Open this link, pick the group, and let it make the bot an admin:".to_string(),
                    format!("  {}", prompt.deep_link),
                    "The group needs Topics turned on (group settings). Then send this code".to_string(),
                    format!("in the group, in any topic: {}", prompt.code),
                    "Waiting for it to arrive…".to_string(),
                ],
                (None, _) => vec![
                    "Enter: show a code to send the bot. Whoever sends it, from wherever".to_string(),
                    "they send it, becomes the one person and chat CST listens to.".to_string(),
                ],
            },
            StepKind::Rights => vec![
                "The bot must be an admin of the group with the Manage Topics right, so it".to_string(),
                "can open a topic per session; Pin Messages lets it unpin each topic's".to_string(),
                "first message, which Telegram pins by itself. Enter: check.".to_string(),
            ],
            StepKind::Test => vec![
                "Enter: send one message to the paired chat. In a group this also makes".to_string(),
                "and closes a topic, which proves the rights really work.".to_string(),
            ],
            StepKind::Content => vec![
                "c: content — full sends the session's last message, questions and plans;".to_string(),
                "   status only sends the title and what kind of attention it wants.".to_string(),
                "r: replies on or off.".to_string(),
                "Full content goes to Telegram's servers, which can read bot chats.".to_string(),
            ],
        }
    }

    // ---- I/O below: starting checks and collecting their results ----

    /// Start the current step's check, if it has one and nothing blocks it.
    ///
    /// Returns the sentence to show when it cannot start.
    pub fn run_step(&mut self, kind: StepKind, api_base: &str) -> Option<String> {
        if self.runner.is_some() {
            return Some("A check is already running.".to_string());
        }
        if let Some(blocker) = self.blocker(kind) {
            return Some(blocker);
        }
        let token = self.token_input.trim().to_string();
        let job = match kind {
            StepKind::Token => {
                if let Err(error) = crate::config::validate_telegram_bot_token(&token) {
                    self.set_state(StepKind::Token, StepState::Failed(error.to_string()));
                    return None;
                }
                CheckJob::Identity
            }
            StepKind::Rights => CheckJob::Rights {
                chat_id: self.paired?.chat_id,
                bot_id: self.bot_id?,
            },
            StepKind::Test => CheckJob::Test {
                chat_id: self.paired?.chat_id,
                mode: self.mode,
            },
            StepKind::Mode | StepKind::Pair | StepKind::Content => return None,
        };
        self.set_state(kind, StepState::Running);
        self.runner = Some(Runner::start(api_base.to_string(), token, kind, job));
        None
    }

    /// Take a finished check's result, if one has arrived.
    pub fn poll(&mut self) -> bool {
        let Some(runner) = self.runner.as_ref() else {
            return false;
        };
        match runner.receiver.try_recv() {
            Ok(outcome) => {
                self.apply(outcome);
                true
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                let kind = runner.kind;
                self.apply(Outcome::Failed(
                    kind,
                    "the check stopped unexpectedly".to_string(),
                ));
                true
            }
            Err(mpsc::TryRecvError::Empty) => false,
        }
    }
}

enum CheckJob {
    Identity,
    Rights {
        chat_id: i64,
        bot_id: i64,
    },
    Test {
        chat_id: i64,
        mode: TelegramChatMode,
    },
}

/// One check on its own thread, reporting once.
struct Runner {
    kind: StepKind,
    receiver: Receiver<Outcome>,
}

impl Runner {
    fn start(api_base: String, token: String, kind: StepKind, job: CheckJob) -> Self {
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let bot = UreqBot::new(&api_base, &token, Duration::from_secs(15));
            let outcome = match job {
                CheckJob::Identity => check_identity(&bot),
                CheckJob::Rights { chat_id, bot_id } => check_rights(&bot, chat_id, bot_id),
                CheckJob::Test { chat_id, mode } => check_test(&bot, chat_id, mode),
            };
            let _ =
                sender.send(outcome.unwrap_or_else(|(kind, reason)| Outcome::Failed(kind, reason)));
        });
        Self { kind, receiver }
    }
}

/// The checks, shared with `cst doctor` and `cst telegram setup`.
pub fn check_identity(bot: &impl BotApi) -> Result<Outcome, (StepKind, String)> {
    match bot.get_me() {
        Ok(identity) => Ok(Outcome::Identity {
            id: identity.id,
            username: identity.username,
        }),
        Err(error) => Err((StepKind::Token, failure(StepKind::Token, &error))),
    }
}

pub fn check_rights(
    bot: &impl BotApi,
    chat_id: i64,
    bot_id: i64,
) -> Result<Outcome, (StepKind, String)> {
    match bot.get_chat_member(chat_id, bot_id) {
        Ok(member) if member.may_manage_topics() => Ok(Outcome::RightsOk {
            may_pin: member.may_pin_messages(),
        }),
        Ok(member) if member.status == "administrator" => Err((
            StepKind::Rights,
            "The bot is an admin but may not manage topics. Edit its admin rights in the \
             group and turn on Manage Topics."
                .to_string(),
        )),
        Ok(_) => Err((
            StepKind::Rights,
            "The bot is not an admin of the group. Promote it, with the Manage Topics right."
                .to_string(),
        )),
        Err(error) => Err((StepKind::Rights, failure(StepKind::Rights, &error))),
    }
}

pub fn check_test(
    bot: &impl BotApi,
    chat_id: i64,
    mode: TelegramChatMode,
) -> Result<Outcome, (StepKind, String)> {
    let send = |thread_id| {
        bot.send_message(
            chat_id,
            thread_id,
            None,
            &super::escape_html(&wording::test_message()),
        )
    };
    let result = match mode {
        TelegramChatMode::Private => send(None).map(|_| ()),
        TelegramChatMode::Forum => {
            bot.create_forum_topic(chat_id, "CST test")
                .and_then(|thread_id| {
                    send(Some(thread_id))?;
                    bot.close_forum_topic(chat_id, thread_id)
                })
        }
    };
    result
        .map(|_| Outcome::TestOk)
        .map_err(|error| (StepKind::Test, failure(StepKind::Test, &error)))
}

/// Every failure sentence in one place, per step, so they read as a set.
fn failure(kind: StepKind, error: &BotError) -> String {
    match (kind, error) {
        (StepKind::Token, BotError::Unauthorized) => {
            "Telegram does not recognise this token. Paste the whole line BotFather sent."
                .to_string()
        }
        (_, BotError::Conflict) => error.to_string(),
        (StepKind::Rights, BotError::Rejected(description))
            if description.contains("not found") =>
        {
            "The bot is not in that group any more. Add it back, then pair again.".to_string()
        }
        (StepKind::Test, BotError::Rejected(description))
            if description.contains("TOPIC") || description.contains("topic") =>
        {
            format!("Telegram refused to make a topic: {description}. Check Topics are turned on and the bot may manage them.")
        }
        (_, BotError::Unreachable(detail)) => {
            format!("Could not reach Telegram ({detail}). Check the network and try again.")
        }
        (_, other) => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TelegramConfig;

    #[test]
    fn a_fresh_wizard_has_everything_pending_except_the_default_mode() {
        let wizard = Wizard::from_config(&UserConfig::default());
        let kinds: Vec<StepKind> = wizard.steps.iter().map(|step| step.kind).collect();
        assert_eq!(
            kinds,
            vec![
                StepKind::Token,
                StepKind::Mode,
                StepKind::Pair,
                StepKind::Test,
                StepKind::Content
            ]
        );
        assert_eq!(wizard.state_of(StepKind::Token), Some(&StepState::Pending));
        assert!(!wizard.can_finish());
        assert_eq!(
            wizard.blocker(StepKind::Pair).as_deref(),
            Some("Check the bot token first.")
        );
    }

    #[test]
    fn a_configured_setup_opens_with_pairing_done_so_one_broken_row_is_one_fix() {
        let config = UserConfig {
            telegram: TelegramConfig {
                enabled: true,
                chat_id: Some(-100),
                user_id: Some(7),
                chat_mode: TelegramChatMode::Forum,
                ..TelegramConfig::default()
            },
            telegram_bot_token: "1:x".to_string(),
            ..UserConfig::default()
        };
        let wizard = Wizard::from_config(&config);
        assert!(matches!(
            wizard.state_of(StepKind::Pair),
            Some(StepState::Done(_))
        ));
        assert!(wizard
            .steps
            .iter()
            .any(|step| step.kind == StepKind::Rights));
        // Not finished until the token has been checked and the rights confirmed.
        assert!(!wizard.can_finish());
    }

    #[test]
    fn choosing_the_other_chat_type_after_pairing_means_pairing_again() {
        let config = UserConfig {
            telegram: TelegramConfig {
                chat_id: Some(5),
                user_id: Some(7),
                chat_mode: TelegramChatMode::Private,
                ..TelegramConfig::default()
            },
            ..UserConfig::default()
        };
        let mut wizard = Wizard::from_config(&config);
        wizard.choose_mode(TelegramChatMode::Forum);
        assert_eq!(wizard.paired, None);
        assert_eq!(wizard.state_of(StepKind::Pair), Some(&StepState::Pending));
        assert!(wizard
            .steps
            .iter()
            .any(|step| step.kind == StepKind::Rights));
        // Choosing the same mode again changes nothing.
        wizard.pairing_arrived(Paired {
            chat_id: -1,
            user_id: 7,
            mode: TelegramChatMode::Forum,
        });
        wizard.choose_mode(TelegramChatMode::Forum);
        assert!(wizard.paired.is_some());
    }

    #[test]
    fn what_was_actually_paired_wins_over_what_was_chosen() {
        // The user picks "group", then presses Start on the private chat anyway.
        let mut wizard = Wizard::from_config(&UserConfig::default());
        wizard.choose_mode(TelegramChatMode::Forum);
        wizard.apply(Outcome::Identity {
            id: 1,
            username: "cst_bot".to_string(),
        });
        wizard.await_pairing(
            "ABC234".to_string(),
            Utc::now() + chrono::Duration::minutes(5),
        );
        assert!(wizard
            .pairing
            .as_ref()
            .unwrap()
            .deep_link
            .contains("startgroup=ABC234"));
        assert!(wizard.is_busy());

        wizard.pairing_arrived(Paired {
            chat_id: 5,
            user_id: 7,
            mode: TelegramChatMode::Private,
        });

        assert_eq!(wizard.mode, TelegramChatMode::Private);
        assert!(!wizard
            .steps
            .iter()
            .any(|step| step.kind == StepKind::Rights));
        assert!(wizard.can_finish());
        assert!(!wizard.is_busy());
    }

    #[test]
    fn a_failed_check_names_its_row_and_the_pairing_timeout_says_what_to_do() {
        let mut wizard = Wizard::from_config(&UserConfig::default());
        wizard.apply(Outcome::Failed(StepKind::Token, "nope".to_string()));
        assert_eq!(
            wizard.state_of(StepKind::Token),
            Some(&StepState::Failed("nope".to_string()))
        );
        wizard.pairing_expired();
        assert!(
            matches!(wizard.state_of(StepKind::Pair), Some(StepState::Failed(reason)) if reason.contains("Enter"))
        );
    }

    #[test]
    fn rights_failures_tell_the_user_which_switch_to_flip() {
        struct Stub(&'static str, bool, bool);
        impl BotApi for Stub {
            fn edit_message_text(&self, _: i64, _: i64, _: &str) -> Result<(), BotError> {
                Ok(())
            }
            fn send_message_with_buttons(
                &self,
                _: i64,
                _: Option<i64>,
                _: Option<i64>,
                _: &str,
                _: &[super::super::bot::Button],
            ) -> Result<i64, BotError> {
                Ok(1)
            }
            fn answer_callback_query(&self, _: &str, _: Option<&str>) -> Result<(), BotError> {
                Ok(())
            }
            fn set_my_commands(&self, _: &[super::super::bot::BotCommand]) -> Result<(), BotError> {
                Ok(())
            }
            fn get_me(&self) -> Result<super::super::bot::BotIdentity, BotError> {
                unreachable!()
            }
            fn get_updates(
                &self,
                _: Option<i64>,
                _: Duration,
            ) -> Result<Vec<super::super::bot::Update>, BotError> {
                unreachable!()
            }
            fn send_message(
                &self,
                _: i64,
                _: Option<i64>,
                _: Option<i64>,
                _: &str,
            ) -> Result<i64, BotError> {
                Ok(1)
            }
            fn create_forum_topic(&self, _: i64, _: &str) -> Result<i64, BotError> {
                Ok(2)
            }
            fn edit_forum_topic(&self, _: i64, _: i64, _: &str) -> Result<(), BotError> {
                Ok(())
            }
            fn close_forum_topic(&self, _: i64, _: i64) -> Result<(), BotError> {
                Ok(())
            }
            fn get_chat_member(
                &self,
                _: i64,
                _: i64,
            ) -> Result<super::super::bot::ChatMember, BotError> {
                Ok(super::super::bot::ChatMember {
                    status: self.0.to_string(),
                    can_manage_topics: self.1,
                    can_pin_messages: self.2,
                })
            }
            fn set_message_reaction(&self, _: i64, _: i64, _: &str) -> Result<(), BotError> {
                Ok(())
            }
        }

        assert_eq!(
            check_rights(&Stub("administrator", true, true), 1, 2),
            Ok(Outcome::RightsOk { may_pin: true })
        );
        // Pinning is cosmetic: an admin who may manage topics but not pin passes, and
        // the row says what that costs.
        assert_eq!(
            check_rights(&Stub("administrator", true, false), 1, 2),
            Ok(Outcome::RightsOk { may_pin: false })
        );
        assert_eq!(
            check_rights(&Stub("creator", false, false), 1, 2),
            Ok(Outcome::RightsOk { may_pin: true })
        );
        let (_, limited) = check_rights(&Stub("administrator", false, false), 1, 2).unwrap_err();
        assert!(limited.contains("Manage Topics"));
        let (_, member) = check_rights(&Stub("member", false, false), 1, 2).unwrap_err();
        assert!(member.contains("not an admin"));
        assert_eq!(
            check_test(&Stub("creator", true, true), 1, TelegramChatMode::Forum),
            Ok(Outcome::TestOk)
        );
    }

    #[test]
    fn a_wrong_token_is_caught_before_asking_telegram() {
        let mut wizard = Wizard::from_config(&UserConfig::default());
        wizard.token_input = "not a token".to_string();
        assert_eq!(wizard.run_step(StepKind::Token, "http://127.0.0.1:9"), None);
        assert!(matches!(
            wizard.state_of(StepKind::Token),
            Some(StepState::Failed(_))
        ));
        assert!(!wizard.is_busy());
    }
}
