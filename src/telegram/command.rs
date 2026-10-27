//! Telling CST's own slash commands from Copilot's, and from plain text.
//!
//! Telegram shows anything that starts with a slash as a command, and so does Copilot:
//! `/ask`, `/compact`, `/model` open menus in the session's terminal. A message such as
//! `/ask hello` typed on the phone used to be forwarded as text, which opened that menu
//! in a session nobody was sitting at. There is no key on the phone to close it, so the
//! session sat stuck until somebody pressed Escape at the PC.
//!
//! So a slash command is never forwarded by accident. CST's own are acted on here;
//! anything else that looks like a command is refused with a sentence naming the way
//! round it: `/say` sends the rest of the line exactly as written, for the user who
//! does mean `/compact`, and `/esc` presses Escape in the session for the one that is
//! already stuck.
//!
//! Pure, so every rule can be tested without a bot.

use super::follow::{self, FollowCommand};

/// What a message from the paired user turns out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    /// Ordinary text, to be typed into the session.
    Text,
    /// One of CST's own commands.
    Cst(Command),
    /// A slash command that is not CST's. The name as typed, without `@bot`.
    Foreign(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Follow {
        minutes: u32,
    },
    Unfollow,
    /// `/queue` alone lists what is waiting; with text, that text waits for the turn to
    /// end instead of steering it.
    Queue(Option<String>),
    /// The rest of the line, forwarded as it is.
    Say(String),
    /// Press Escape in the session.
    Esc,
}

/// Read a message. Case does not matter, and `/follow@cst_bot` is `/follow`: Telegram
/// adds the bot's name when a command is picked from the menu in a group.
pub fn parse(text: &str) -> Parsed {
    let trimmed = text.trim_start();
    let Some(first) = trimmed.split_whitespace().next() else {
        return Parsed::Text;
    };
    if !first.starts_with('/') {
        return Parsed::Text;
    }
    let rest = trimmed[first.len()..].trim();
    let name = first
        .split('@')
        .next()
        .unwrap_or(first)
        .to_ascii_lowercase();
    match follow::parse_command(text) {
        Some(FollowCommand::Follow { minutes }) => return Parsed::Cst(Command::Follow { minutes }),
        Some(FollowCommand::Unfollow) => return Parsed::Cst(Command::Unfollow),
        None => {}
    }
    match name.as_str() {
        "/queue" => Parsed::Cst(Command::Queue((!rest.is_empty()).then(|| rest.to_string()))),
        "/say" => {
            if rest.is_empty() {
                // `/say` with nothing after it is a question about itself.
                Parsed::Foreign(name)
            } else {
                Parsed::Cst(Command::Say(rest.to_string()))
            }
        }
        "/esc" | "/escape" => Parsed::Cst(Command::Esc),
        _ => Parsed::Foreign(name),
    }
}

pub mod wording {
    /// A slash command that is not CST's.
    pub fn not_forwarded(command: &str) -> String {
        format!(
            "Not delivered: {command} looks like a Copilot slash command. Those open menus in \
             the session that cannot be closed from here, so none is forwarded. CST's own are \
             /follow, /unfollow, /queue, /say and /esc. To send the line anyway, write \
             /say {command} …"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copilots_commands_are_told_apart_from_csts_and_from_text() {
        assert_eq!(parse("hello there"), Parsed::Text);
        assert_eq!(parse("  a path like /tmp/x"), Parsed::Text);
        assert_eq!(parse("/ask hello"), Parsed::Foreign("/ask".to_string()));
        assert_eq!(parse("/COMPACT"), Parsed::Foreign("/compact".to_string()));
        assert_eq!(
            parse("/model@cst_bot"),
            Parsed::Foreign("/model".to_string())
        );
        assert_eq!(
            parse("/follow 10"),
            Parsed::Cst(Command::Follow { minutes: 10 })
        );
        assert_eq!(parse("/Unfollow@cst_bot"), Parsed::Cst(Command::Unfollow));
        assert_eq!(parse("/esc"), Parsed::Cst(Command::Esc));
    }

    #[test]
    fn queue_alone_lists_and_queue_with_words_holds_those_words() {
        assert_eq!(parse("/queue"), Parsed::Cst(Command::Queue(None)));
        assert_eq!(parse("/queue@cst_bot  "), Parsed::Cst(Command::Queue(None)));
        assert_eq!(
            parse("/queue then write the summary"),
            Parsed::Cst(Command::Queue(Some("then write the summary".to_string())))
        );
    }

    #[test]
    fn say_forwards_the_rest_of_the_line_slash_and_all_and_says_nothing_with_nothing() {
        assert_eq!(
            parse("/say /compact"),
            Parsed::Cst(Command::Say("/compact".to_string()))
        );
        assert_eq!(
            parse("/say@cst_bot /ask how do I\nmultiline"),
            Parsed::Cst(Command::Say("/ask how do I\nmultiline".to_string()))
        );
        assert_eq!(parse("/say"), Parsed::Foreign("/say".to_string()));
    }

    #[test]
    fn the_refusal_names_the_command_and_the_way_round_it() {
        let text = wording::not_forwarded("/ask");
        assert!(text.starts_with("Not delivered: /ask"));
        assert!(text.contains("/say /ask"));
        assert!(text.contains("/esc"));
    }
}
