//! `cst telegram` — the setup wizard's twin for a shell.
//!
//! Same checks, same sentences, printed one per line as `cst doctor` does: plain text,
//! no colour, no glyphs. For people who would rather do this outside the TUI, and for a
//! machine that is only ever reached over SSH where opening a deep link is a matter of
//! copying it.

use std::io::Write;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use super::bot::UreqBot;
use super::store::{self, Pairing};
use super::wizard::{check_identity, check_rights, check_test, Outcome, StepKind};
use super::{deep_link, pairing_code, Paired, PAIRING_WINDOW};
use crate::config::{self, TelegramChatMode, UserConfig};

/// How long a Bot API call may take from the command line.
const CALL_TIMEOUT: Duration = Duration::from_secs(15);

/// `cst telegram status`: what is configured, and whether the token answers.
pub fn status(config: &UserConfig) -> String {
    let telegram = &config.telegram;
    let mut lines = vec![format!(
        "Telegram notifications are {}.",
        if telegram.enabled { "on" } else { "off" }
    )];
    if config.telegram_bot_token.trim().is_empty() {
        lines.push("No bot token is configured. Run `cst telegram setup`.".to_string());
        return lines.join("\n");
    }
    match config::validate_telegram_bot_token(&config.telegram_bot_token) {
        Ok(()) => {
            let bot = UreqBot::new(&telegram.api_base, &config.telegram_bot_token, CALL_TIMEOUT);
            match check_identity(&bot) {
                Ok(Outcome::Identity { username, .. }) => {
                    lines.push(format!("The bot token answers as @{username}."))
                }
                Ok(_) => {}
                Err((_, reason)) => lines.push(format!("The bot token does not work: {reason}")),
            }
        }
        Err(error) => lines.push(format!("The bot token does not look right: {error}")),
    }
    match (telegram.chat_id, telegram.user_id) {
        (Some(chat_id), Some(_)) => lines.push(format!(
            "Paired with a {} (chat {chat_id}). Content: {}. Replies: {}.",
            telegram.chat_mode.label(),
            telegram.content.settings_label().to_lowercase(),
            if telegram.replies { "on" } else { "off" }
        )),
        _ => lines.push("Not paired yet. Run `cst telegram pair`.".to_string()),
    }
    lines.join("\n")
}

/// `cst telegram pair [--group]`: show a code, wait for it, write who sent it.
///
/// Polls Telegram itself if no running CST holds the poller lock; otherwise the running
/// CST sees the code and this only watches the state file for the result.
pub fn pair(
    root: &Path,
    config_path: &Path,
    config: &mut UserConfig,
    group: bool,
    out: &mut impl Write,
) -> Result<()> {
    config::validate_telegram_bot_token(&config.telegram_bot_token)
        .context("A bot token is needed first; set it with `cst telegram setup --token <TOKEN>` or in Global Settings")?;
    let bot = UreqBot::new(
        &config.telegram.api_base,
        &config.telegram_bot_token,
        CALL_TIMEOUT,
    );
    let username = match check_identity(&bot) {
        Ok(Outcome::Identity { username, .. }) => username,
        Ok(_) => anyhow::bail!("Telegram answered with something unexpected"),
        Err((_, reason)) => anyhow::bail!("{reason}"),
    };
    let mode = if group {
        TelegramChatMode::Forum
    } else {
        TelegramChatMode::Private
    };
    let code = pairing_code();
    let expires_at = chrono::Utc::now() + PAIRING_WINDOW;
    store::update_in(root, |state| {
        state.pairing = Some(Pairing::Armed {
            code: code.clone(),
            expires_at,
        });
    })?;
    writeln!(out, "{}", pairing_instructions(&username, &code, mode))?;
    out.flush()?;

    // A poller of our own, in case no CST is running. It only polls if it gets the lock.
    let (events, _unused) = std::sync::mpsc::channel();
    let _worker = super::worker::TelegramWorker::start(
        root.to_path_buf(),
        Some((
            events,
            super::worker::PollSettings {
                token: config.telegram_bot_token.trim().to_string(),
                api_base: config.telegram.api_base.clone(),
                paired: None,
                replies: config.telegram.replies,
            },
        )),
    );
    let paired = wait_for_pairing(root, expires_at.signed_duration_since(chrono::Utc::now()));
    let _ = store::update_in(root, |state| {
        if matches!(state.pairing, Some(Pairing::Armed { .. })) {
            state.pairing = None;
        }
    });
    let Some(paired) = paired else {
        anyhow::bail!("No message with the code arrived in time. Run `cst telegram pair` again.");
    };
    config.telegram.chat_id = Some(paired.chat_id);
    config.telegram.user_id = Some(paired.user_id);
    config.telegram.chat_mode = paired.mode;
    config.telegram.enabled = true;
    config::save_if_revision(config_path, None, config)?;
    writeln!(out, "{}", render_paired(paired))?;
    Ok(())
}

fn pairing_instructions(username: &str, code: &str, mode: TelegramChatMode) -> String {
    let link = deep_link(username, code, mode);
    match mode {
        TelegramChatMode::Private => format!(
            "Open this link and press Start, which sends the code for you:\n  {link}\n\
             Or send @{username} this code yourself: {code}\n\
             Waiting up to five minutes for it to arrive..."
        ),
        TelegramChatMode::Forum => format!(
            "Open this link, pick the group, and let it make the bot an admin:\n  {link}\n\
             The group needs Topics turned on. Then send this code in the group: {code}\n\
             Waiting up to five minutes for it to arrive..."
        ),
    }
}

fn render_paired(paired: Paired) -> String {
    format!(
        "Paired with a {} (chat {}). Telegram notifications are on.\n\
         Run `cst telegram test` to send a test message.",
        paired.mode.label(),
        paired.chat_id
    )
}

fn wait_for_pairing(root: &Path, window: chrono::Duration) -> Option<Paired> {
    let deadline = Instant::now() + window.to_std().unwrap_or(Duration::from_secs(300));
    while Instant::now() < deadline {
        if let Some(paired) = store::load_in(root).paired() {
            return Some(paired);
        }
        thread::sleep(Duration::from_millis(500));
    }
    None
}

/// `cst telegram test`: one message, and in a group one topic made and closed.
pub fn test(config: &UserConfig) -> Result<String> {
    let telegram = &config.telegram;
    let (Some(chat_id), Some(_)) = (telegram.chat_id, telegram.user_id) else {
        anyhow::bail!("Not paired yet. Run `cst telegram pair` first.");
    };
    let bot = UreqBot::new(&telegram.api_base, &config.telegram_bot_token, CALL_TIMEOUT);
    if telegram.chat_mode == TelegramChatMode::Forum {
        let Ok(Outcome::Identity { id, .. }) = check_identity(&bot) else {
            anyhow::bail!("The bot token does not work; run `cst telegram status`.");
        };
        if let Err((_, reason)) = check_rights(&bot, chat_id, id) {
            anyhow::bail!("{reason}");
        }
    }
    match check_test(&bot, chat_id, telegram.chat_mode) {
        Ok(_) => Ok(format!(
            "Sent a test message to the paired {}.",
            telegram.chat_mode.label()
        )),
        Err((StepKind::Test, reason)) | Err((_, reason)) => anyhow::bail!("{reason}"),
    }
}

/// `cst telegram setup [--token T] [--group]`: the whole checklist, in order.
pub fn setup(
    root: &Path,
    config_path: &Path,
    config: &mut UserConfig,
    token: Option<&str>,
    group: bool,
    out: &mut impl Write,
) -> Result<()> {
    if let Some(token) = token {
        config::validate_telegram_bot_token(token)?;
        config.telegram_bot_token = token.trim().to_string();
        config::save_if_revision(config_path, None, config)?;
        writeln!(out, "Bot token saved.")?;
    }
    if config.telegram_bot_token.trim().is_empty() {
        anyhow::bail!(
            "No bot token yet. In Telegram, open @BotFather, send /newbot, and run\n\
             `cst telegram setup --token <TOKEN>` with what it gives you."
        );
    }
    let bot = UreqBot::new(
        &config.telegram.api_base,
        &config.telegram_bot_token,
        CALL_TIMEOUT,
    );
    match check_identity(&bot) {
        Ok(Outcome::Identity { username, .. }) => writeln!(out, "Bot token: @{username} answers.")?,
        Ok(_) => anyhow::bail!("Telegram answered with something unexpected"),
        Err((_, reason)) => anyhow::bail!("Bot token: {reason}"),
    }
    let _ = bot;
    if !config.telegram.is_paired()
        || group != (config.telegram.chat_mode == TelegramChatMode::Forum)
    {
        pair(root, config_path, config, group, out)?;
    } else {
        writeln!(
            out,
            "Pairing: already paired with a {}.",
            config.telegram.chat_mode.label()
        )?;
    }
    writeln!(out, "Test message: {}", test(config)?)?;
    writeln!(
        out,
        "Content: {}; replies {}. Change either in Global Settings (,).",
        config.telegram.content.settings_label().to_lowercase(),
        if config.telegram.replies { "on" } else { "off" }
    )?;
    Ok(())
}

pub fn emit(out: &mut impl Write, report: &str) -> std::io::Result<()> {
    writeln!(out, "{report}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_says_what_is_missing_without_touching_the_network() {
        let config = UserConfig::default();
        let text = status(&config);
        assert!(text.contains("off"));
        assert!(text.contains("No bot token"));
    }

    #[test]
    fn pairing_instructions_carry_the_link_and_the_code_for_either_chat_type() {
        let private = pairing_instructions("cst_bot", "ABC234", TelegramChatMode::Private);
        assert!(private.contains("https://t.me/cst_bot?start=ABC234"));
        assert!(private.contains("ABC234"));
        let group = pairing_instructions("cst_bot", "ABC234", TelegramChatMode::Forum);
        assert!(group.contains("startgroup=ABC234&admin=manage_topics"));
        assert!(group.contains("Topics turned on"));
    }
}
