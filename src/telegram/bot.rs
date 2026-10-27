//! The Bot API over HTTP, behind a trait so everything above it can be tested with a
//! stub.
//!
//! Only the methods and fields CST uses are modelled. Every error is reduced to a
//! sentence CST wrote: a ureq error can name the URL it was calling, and the URL has
//! the bot token in it.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A bot as Telegram describes it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BotIdentity {
    pub id: i64,
    pub username: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct User {
    pub id: i64,
    #[serde(default)]
    pub is_bot: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub is_forum: bool,
}

/// Just enough of the message being replied to.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RepliedTo {
    pub message_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Message {
    pub message_id: i64,
    pub chat: Chat,
    #[serde(default)]
    pub from: Option<User>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub message_thread_id: Option<i64>,
    #[serde(default)]
    pub is_topic_message: bool,
    #[serde(default)]
    pub reply_to_message: Option<RepliedTo>,
}

impl Message {
    /// The topic a message was typed in, if it was typed in one.
    ///
    /// Telegram sets `message_thread_id` on any reply as well, pointing at the message
    /// being replied to, so without `is_topic_message` a reply in a private chat would
    /// look like a topic.
    pub fn topic_id(&self) -> Option<i64> {
        self.is_topic_message
            .then_some(self.message_thread_id)
            .flatten()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Update {
    pub update_id: i64,
    #[serde(default)]
    pub message: Option<Message>,
    /// A tap on one of this bot's inline buttons.
    #[serde(default)]
    pub callback_query: Option<CallbackQuery>,
}

/// A button tapped. `data` is what the button was made with; `message` is the one it
/// sat under, which is where any edit in answer goes.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CallbackQuery {
    pub id: String,
    pub from: User,
    #[serde(default)]
    pub message: Option<Message>,
    #[serde(default)]
    pub data: Option<String>,
}

/// One inline button under a message. `callback_data` comes back verbatim in the
/// tap and may be at most 64 bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Button {
    pub text: String,
    pub callback_data: String,
}

/// One of the emoji a forum topic may wear, as Telegram offers them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopicIcon {
    pub emoji: String,
    pub custom_emoji_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ChatMember {
    pub status: String,
    #[serde(default)]
    pub can_manage_topics: bool,
}

impl ChatMember {
    pub fn may_manage_topics(&self) -> bool {
        self.status == "creator" || (self.status == "administrator" && self.can_manage_topics)
    }
}

/// One entry of the bot's command menu, the list Telegram offers when the user types `/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BotCommand {
    /// Without the slash: 1 to 32 lowercase letters, digits and underscores.
    pub command: &'static str,
    /// 3 to 256 characters, shown beside the name in the menu.
    pub description: &'static str,
}

/// Why a call failed, in words that never include the token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotError {
    /// Telegram does not recognise the token.
    Unauthorized,
    /// Another `getUpdates` consumer, or a webhook, owns this bot.
    Conflict,
    /// Telegram answered and said no, with its own description.
    Rejected(String),
    /// Nothing answered, or not in time.
    Unreachable(String),
    /// The answer was not what the Bot API documents.
    Malformed,
}

impl std::fmt::Display for BotError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthorized => formatter.write_str("Telegram does not recognise this bot token"),
            Self::Conflict => formatter.write_str(
                "this bot is already used by another program; create a separate bot for CST \
                 with BotFather",
            ),
            Self::Rejected(description) => write!(formatter, "Telegram refused: {description}"),
            Self::Unreachable(detail) => write!(formatter, "could not reach Telegram: {detail}"),
            Self::Malformed => formatter.write_str("Telegram answered with something unexpected"),
        }
    }
}

/// The calls CST makes. A stub implements this in tests and in the e2e driver's server.
pub trait BotApi {
    fn get_me(&self) -> Result<BotIdentity, BotError>;
    fn get_updates(&self, offset: Option<i64>, timeout: Duration) -> Result<Vec<Update>, BotError>;
    /// Returns the sent message's id. `reply_to` quotes one of the user's messages.
    fn send_message(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        reply_to: Option<i64>,
        html: &str,
    ) -> Result<i64, BotError>;
    /// Returns the new topic's thread id.
    fn create_forum_topic(&self, chat_id: i64, name: &str) -> Result<i64, BotError>;
    fn edit_forum_topic(&self, chat_id: i64, thread_id: i64, name: &str) -> Result<(), BotError>;
    fn close_forum_topic(&self, chat_id: i64, thread_id: i64) -> Result<(), BotError>;
    fn get_chat_member(&self, chat_id: i64, user_id: i64) -> Result<ChatMember, BotError>;
    fn set_message_reaction(
        &self,
        chat_id: i64,
        message_id: i64,
        emoji: &str,
    ) -> Result<(), BotError>;
    /// Replace the text of a message this bot sent. An edit to the same text is not an
    /// error here, whatever Telegram says about it.
    fn edit_message_text(&self, chat_id: i64, message_id: i64, html: &str) -> Result<(), BotError>;
    /// As `send_message`, with inline buttons under the text, one per row.
    fn send_message_with_buttons(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        reply_to: Option<i64>,
        html: &str,
        buttons: &[Button],
    ) -> Result<i64, BotError>;
    /// Acknowledge a tap so the button stops spinning on the phone; `text` shows as a
    /// small notice there. Every tap is answered, including ones that are ignored.
    fn answer_callback_query(&self, callback_id: &str, text: Option<&str>) -> Result<(), BotError>;
    /// The emoji Telegram allows as topic icons, each with the id a topic is made
    /// with. Provided as empty, for stubs: no icon is the state before this existed.
    fn get_forum_topic_icon_stickers(&self) -> Result<Vec<TopicIcon>, BotError> {
        Ok(Vec::new())
    }
    /// [`BotApi::create_forum_topic`] with an icon from
    /// [`BotApi::get_forum_topic_icon_stickers`]. Provided as the plain call, so a
    /// stub that knows nothing of icons still makes the topic.
    fn create_forum_topic_with_icon(
        &self,
        chat_id: i64,
        name: &str,
        _icon_custom_emoji_id: Option<&str>,
    ) -> Result<i64, BotError> {
        self.create_forum_topic(chat_id, name)
    }
    /// A PNG with an HTML caption, as a reply when there is something to reply to.
    /// Returns the message id. Provided, so the test stubs that never send one need
    /// not say so.
    fn send_photo(
        &self,
        _chat_id: i64,
        _thread_id: Option<i64>,
        _reply_to: Option<i64>,
        _png: &[u8],
        _caption_html: &str,
    ) -> Result<i64, BotError> {
        Err(BotError::Rejected(
            "this bot does not send photos".to_string(),
        ))
    }
    /// Publish the command menu. Per bot, not per chat: one call with the default scope
    /// reaches the private chat and every group the bot is in.
    fn set_my_commands(&self, commands: &[BotCommand]) -> Result<(), BotError>;
}

/// The real thing.
pub struct UreqBot {
    api_base: String,
    token: String,
    agent: ureq::Agent,
}

impl UreqBot {
    /// `timeout` bounds every call. A long poll needs this to exceed its own wait.
    pub fn new(api_base: &str, token: &str, timeout: Duration) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            // Telegram puts the real answer in the JSON body of a 4xx as well, so the
            // status alone must not end the call.
            .http_status_as_error(false)
            .build()
            .into();
        Self {
            api_base: api_base.trim_end_matches('/').to_string(),
            token: token.trim().to_string(),
            agent,
        }
    }

    fn call<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        body: &impl Serialize,
    ) -> Result<T, BotError> {
        let url = format!("{}/bot{}/{method}", self.api_base, self.token);
        let response = self
            .agent
            .post(&url)
            .header(
                "User-Agent",
                concat!("copilot-session-tui/", env!("CARGO_PKG_VERSION")),
            )
            .send_json(body)
            .map_err(sanitize)?;
        Self::unwrap_envelope(response)
    }

    /// A call whose body is a file: `multipart/form-data`, built by hand because the
    /// one method that needs it does not justify a crate.
    fn call_multipart<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        fields: &[(&str, &str)],
        file: (&str, &str, &[u8]),
    ) -> Result<T, BotError> {
        let boundary = format!("cst{}", uuid::Uuid::new_v4().simple());
        let mut body: Vec<u8> = Vec::new();
        for (name, value) in fields {
            body.extend_from_slice(
                format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
                    .as_bytes(),
            );
        }
        let (name, filename, bytes) = file;
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\nContent-Type: image/png\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let url = format!("{}/bot{}/{method}", self.api_base, self.token);
        let response = self
            .agent
            .post(&url)
            .header(
                "User-Agent",
                concat!("copilot-session-tui/", env!("CARGO_PKG_VERSION")),
            )
            .header(
                "Content-Type",
                &format!("multipart/form-data; boundary={boundary}"),
            )
            .send(&body[..])
            .map_err(sanitize)?;
        Self::unwrap_envelope(response)
    }

    fn unwrap_envelope<T: for<'de> Deserialize<'de>>(
        response: ureq::http::Response<ureq::Body>,
    ) -> Result<T, BotError> {
        let status = response.status().as_u16();
        let bytes = response
            .into_body()
            .read_to_vec()
            .map_err(|_| BotError::Unreachable("the reply was cut short".to_string()))?;
        let envelope: Envelope = serde_json::from_slice(&bytes).map_err(|_| match status {
            401 => BotError::Unauthorized,
            409 => BotError::Conflict,
            _ => BotError::Malformed,
        })?;
        if !envelope.ok {
            return Err(match (status, envelope.error_code) {
                (401, _) | (_, Some(401)) => BotError::Unauthorized,
                (409, _) | (_, Some(409)) => BotError::Conflict,
                _ => BotError::Rejected(
                    envelope
                        .description
                        .unwrap_or_else(|| format!("HTTP {status}")),
                ),
            });
        }
        serde_json::from_value(envelope.result.unwrap_or(Value::Null))
            .map_err(|_| BotError::Malformed)
    }
}

#[derive(Deserialize)]
struct Envelope {
    ok: bool,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error_code: Option<u16>,
    #[serde(default)]
    description: Option<String>,
}

/// Fixed sentences: a transport error's text can carry the URL, and the URL carries the
/// token.
fn sanitize(error: ureq::Error) -> BotError {
    match error {
        ureq::Error::Timeout(_) => BotError::Unreachable("no answer in time".to_string()),
        ureq::Error::BadUri(_) | ureq::Error::Http(_) => {
            BotError::Unreachable("the Telegram API address is not usable".to_string())
        }
        _ => BotError::Unreachable("no connection".to_string()),
    }
}

#[derive(Serialize)]
struct Empty {}

impl BotApi for UreqBot {
    fn send_message_with_buttons(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        reply_to: Option<i64>,
        html: &str,
        buttons: &[Button],
    ) -> Result<i64, BotError> {
        #[derive(Serialize)]
        struct ReplyParameters {
            message_id: i64,
            allow_sending_without_reply: bool,
        }
        #[derive(Serialize)]
        struct Keyboard<'a> {
            inline_keyboard: Vec<[&'a Button; 1]>,
        }
        #[derive(Serialize)]
        struct NoPreview {
            is_disabled: bool,
        }
        #[derive(Serialize)]
        struct Body<'a> {
            chat_id: i64,
            #[serde(skip_serializing_if = "Option::is_none")]
            message_thread_id: Option<i64>,
            #[serde(skip_serializing_if = "Option::is_none")]
            reply_parameters: Option<ReplyParameters>,
            text: &'a str,
            parse_mode: &'static str,
            link_preview_options: NoPreview,
            #[serde(skip_serializing_if = "Option::is_none")]
            reply_markup: Option<Keyboard<'a>>,
        }
        #[derive(Deserialize)]
        struct Sent {
            message_id: i64,
        }
        let body = Body {
            chat_id,
            message_thread_id: thread_id,
            reply_parameters: reply_to.map(|message_id| ReplyParameters {
                message_id,
                allow_sending_without_reply: true,
            }),
            text: html,
            parse_mode: "HTML",
            link_preview_options: NoPreview { is_disabled: true },
            reply_markup: (!buttons.is_empty()).then(|| Keyboard {
                inline_keyboard: buttons.iter().map(|button| [button]).collect(),
            }),
        };
        self.call::<Sent>("sendMessage", &body)
            .map(|sent| sent.message_id)
    }

    fn answer_callback_query(&self, callback_id: &str, text: Option<&str>) -> Result<(), BotError> {
        #[derive(Serialize)]
        struct Body<'a> {
            callback_query_id: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            text: Option<&'a str>,
        }
        self.call::<Value>(
            "answerCallbackQuery",
            &Body {
                callback_query_id: callback_id,
                text,
            },
        )
        .map(|_| ())
    }

    fn send_photo(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        reply_to: Option<i64>,
        png: &[u8],
        caption_html: &str,
    ) -> Result<i64, BotError> {
        #[derive(Deserialize)]
        struct Sent {
            message_id: i64,
        }
        let chat = chat_id.to_string();
        let thread = thread_id.map(|id| id.to_string());
        let reply = reply_to
            .map(|id| format!(r#"{{"message_id":{id},"allow_sending_without_reply":true}}"#));
        let mut fields: Vec<(&str, &str)> = vec![("chat_id", &chat), ("parse_mode", "HTML")];
        if !caption_html.is_empty() {
            fields.push(("caption", caption_html));
        }
        if let Some(thread) = &thread {
            fields.push(("message_thread_id", thread));
        }
        if let Some(reply) = &reply {
            fields.push(("reply_parameters", reply));
        }
        self.call_multipart::<Sent>("sendPhoto", &fields, ("photo", "screen.png", png))
            .map(|sent| sent.message_id)
    }

    fn set_my_commands(&self, commands: &[BotCommand]) -> Result<(), BotError> {
        #[derive(Serialize)]
        struct Body<'a> {
            commands: &'a [BotCommand],
        }
        self.call::<Value>("setMyCommands", &Body { commands })
            .map(|_| ())
    }

    fn edit_message_text(&self, chat_id: i64, message_id: i64, html: &str) -> Result<(), BotError> {
        #[derive(Serialize)]
        struct Body<'a> {
            chat_id: i64,
            message_id: i64,
            text: &'a str,
            parse_mode: &'static str,
            link_preview_options: LinkPreviewOff,
        }
        #[derive(Serialize)]
        struct LinkPreviewOff {
            is_disabled: bool,
        }
        let body = Body {
            chat_id,
            message_id,
            text: html,
            parse_mode: "HTML",
            link_preview_options: LinkPreviewOff { is_disabled: true },
        };
        match self.call::<Value>("editMessageText", &body) {
            Ok(_) => Ok(()),
            // The view is only edited when it changed, but a race can still send the
            // same text twice; Telegram calling that a bad request is not a failure.
            Err(BotError::Rejected(description))
                if description.to_lowercase().contains("not modified") =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    fn get_me(&self) -> Result<BotIdentity, BotError> {
        self.call("getMe", &Empty {})
    }

    fn get_updates(&self, offset: Option<i64>, timeout: Duration) -> Result<Vec<Update>, BotError> {
        #[derive(Serialize)]
        struct Body {
            #[serde(skip_serializing_if = "Option::is_none")]
            offset: Option<i64>,
            timeout: u64,
            allowed_updates: [&'static str; 2],
        }
        self.call(
            "getUpdates",
            &Body {
                offset,
                timeout: timeout.as_secs(),
                // Telegram remembers this per bot: an older CST taking over the poll
                // during an upgrade narrows it back to messages, and taps go nowhere
                // until a newer one polls again. A bounded gap, not a loss.
                allowed_updates: ["message", "callback_query"],
            },
        )
    }

    fn send_message(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        reply_to: Option<i64>,
        html: &str,
    ) -> Result<i64, BotError> {
        #[derive(Serialize)]
        struct ReplyParameters {
            message_id: i64,
            // The message may be gone by the time the answer goes out; a missing quote
            // must not swallow the answer.
            allow_sending_without_reply: bool,
        }
        #[derive(Serialize)]
        struct Body<'a> {
            chat_id: i64,
            #[serde(skip_serializing_if = "Option::is_none")]
            message_thread_id: Option<i64>,
            #[serde(skip_serializing_if = "Option::is_none")]
            reply_parameters: Option<ReplyParameters>,
            text: &'a str,
            parse_mode: &'static str,
            link_preview_options: LinkPreview,
        }
        #[derive(Serialize)]
        struct LinkPreview {
            is_disabled: bool,
        }
        #[derive(Deserialize)]
        struct Sent {
            message_id: i64,
        }
        let sent: Sent = self.call(
            "sendMessage",
            &Body {
                chat_id,
                message_thread_id: thread_id,
                reply_parameters: reply_to.map(|message_id| ReplyParameters {
                    message_id,
                    allow_sending_without_reply: true,
                }),
                text: html,
                parse_mode: "HTML",
                // An agent's message is full of links; previews would bury the text.
                link_preview_options: LinkPreview { is_disabled: true },
            },
        )?;
        Ok(sent.message_id)
    }

    fn create_forum_topic(&self, chat_id: i64, name: &str) -> Result<i64, BotError> {
        self.create_forum_topic_with_icon(chat_id, name, None)
    }

    fn create_forum_topic_with_icon(
        &self,
        chat_id: i64,
        name: &str,
        icon_custom_emoji_id: Option<&str>,
    ) -> Result<i64, BotError> {
        #[derive(Serialize)]
        struct Body<'a> {
            chat_id: i64,
            name: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            icon_custom_emoji_id: Option<&'a str>,
        }
        #[derive(Deserialize)]
        struct Topic {
            message_thread_id: i64,
        }
        let topic: Topic = self.call(
            "createForumTopic",
            &Body {
                chat_id,
                name: &topic_name(name),
                icon_custom_emoji_id,
            },
        )?;
        Ok(topic.message_thread_id)
    }

    fn get_forum_topic_icon_stickers(&self) -> Result<Vec<TopicIcon>, BotError> {
        #[derive(Deserialize)]
        struct Sticker {
            #[serde(default)]
            emoji: Option<String>,
            #[serde(default)]
            custom_emoji_id: Option<String>,
        }
        let stickers: Vec<Sticker> = self.call("getForumTopicIconStickers", &Empty {})?;
        Ok(stickers
            .into_iter()
            .filter_map(|sticker| {
                Some(TopicIcon {
                    emoji: sticker.emoji?,
                    custom_emoji_id: sticker.custom_emoji_id?,
                })
            })
            .collect())
    }

    fn edit_forum_topic(&self, chat_id: i64, thread_id: i64, name: &str) -> Result<(), BotError> {
        #[derive(Serialize)]
        struct Body<'a> {
            chat_id: i64,
            message_thread_id: i64,
            name: &'a str,
        }
        let _: Value = self.call(
            "editForumTopic",
            &Body {
                chat_id,
                message_thread_id: thread_id,
                name: &topic_name(name),
            },
        )?;
        Ok(())
    }

    fn close_forum_topic(&self, chat_id: i64, thread_id: i64) -> Result<(), BotError> {
        #[derive(Serialize)]
        struct Body {
            chat_id: i64,
            message_thread_id: i64,
        }
        let _: Value = self.call(
            "closeForumTopic",
            &Body {
                chat_id,
                message_thread_id: thread_id,
            },
        )?;
        Ok(())
    }

    fn get_chat_member(&self, chat_id: i64, user_id: i64) -> Result<ChatMember, BotError> {
        #[derive(Serialize)]
        struct Body {
            chat_id: i64,
            user_id: i64,
        }
        self.call("getChatMember", &Body { chat_id, user_id })
    }

    fn set_message_reaction(
        &self,
        chat_id: i64,
        message_id: i64,
        emoji: &str,
    ) -> Result<(), BotError> {
        #[derive(Serialize)]
        struct Reaction<'a> {
            #[serde(rename = "type")]
            kind: &'static str,
            emoji: &'a str,
        }
        #[derive(Serialize)]
        struct Body<'a> {
            chat_id: i64,
            message_id: i64,
            reaction: [Reaction<'a>; 1],
        }
        let _: Value = self.call(
            "setMessageReaction",
            &Body {
                chat_id,
                message_id,
                reaction: [Reaction {
                    kind: "emoji",
                    emoji,
                }],
            },
        )?;
        Ok(())
    }
}

/// A topic name Telegram accepts: 1 to 128 characters, no control characters.
pub fn topic_name(title: &str) -> String {
    let clean: String = title
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .chars()
        .take(128)
        .collect();
    if clean.is_empty() {
        "Session".to_string()
    } else {
        clean
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    /// A one-shot HTTP server that records the request and answers with a fixed body.
    pub(crate) fn mock_server(responses: Vec<(u16, String)>) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0u8; 4096];
                let header_end;
                loop {
                    let read = stream.read(&mut buffer).unwrap();
                    if read == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&buffer[..read]);
                    if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                        header_end = end + 4;
                        break;
                    }
                }
                let headers = String::from_utf8_lossy(&bytes[..header_end]).to_string();
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                while bytes.len() < header_end + content_length {
                    let read = stream.read(&mut buffer).unwrap();
                    if read == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buffer[..read]);
                }
                let _ = sender.send(String::from_utf8_lossy(&bytes).to_string());
                write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        (format!("http://{address}"), receiver)
    }

    fn request_body(request: &str) -> Value {
        serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap()
    }

    #[test]
    fn calls_go_to_the_token_path_as_json_and_the_result_is_unwrapped() {
        let (base, requests) = mock_server(vec![(
            200,
            r#"{"ok":true,"result":{"id":5,"is_bot":true,"username":"cst_bot"}}"#.to_string(),
        )]);
        let bot = UreqBot::new(&base, "123:SECRET", Duration::from_secs(2));

        let me = bot.get_me().unwrap();
        assert_eq!(me.username, "cst_bot");
        let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(request.starts_with("POST /bot123:SECRET/getMe HTTP/1.1"));
    }

    #[test]
    fn the_command_menu_goes_up_in_one_call_without_a_scope_so_groups_see_it_too() {
        let (base, requests) = mock_server(vec![(200, r#"{"ok":true,"result":true}"#.to_string())]);
        let bot = UreqBot::new(&base, "123:SECRET", Duration::from_secs(2));

        bot.set_my_commands(&[BotCommand {
            command: "follow",
            description: "Watch a session live",
        }])
        .unwrap();

        let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(request.starts_with("POST /bot123:SECRET/setMyCommands HTTP/1.1"));
        let body = request_body(&request);
        assert_eq!(body["commands"][0]["command"], "follow");
        assert_eq!(body["commands"][0]["description"], "Watch a session live");
        assert!(
            body.get("scope").is_none(),
            "a scoped list would hide the menu from chats of the other kind"
        );
    }

    #[test]
    fn a_send_uses_html_mode_in_the_topic_and_turns_previews_off() {
        let (base, requests) = mock_server(vec![(
            200,
            r#"{"ok":true,"result":{"message_id":42}}"#.to_string(),
        )]);
        let bot = UreqBot::new(&base, "123:SECRET", Duration::from_secs(2));

        let id = bot
            .send_message(-100, Some(7), Some(3), "<b>hi</b>")
            .unwrap();
        assert_eq!(id, 42);
        let body = request_body(&requests.recv_timeout(Duration::from_secs(2)).unwrap());
        assert_eq!(body["chat_id"], -100);
        assert_eq!(body["message_thread_id"], 7);
        assert_eq!(body["parse_mode"], "HTML");
        assert_eq!(body["link_preview_options"]["is_disabled"], true);
    }

    #[test]
    fn telegrams_own_refusals_are_named_and_the_token_never_appears_in_an_error() {
        let (base, _) = mock_server(vec![
            (401, r#"{"ok":false,"error_code":401,"description":"Unauthorized"}"#.to_string()),
            (
                409,
                r#"{"ok":false,"error_code":409,"description":"Conflict: terminated by other getUpdates request"}"#.to_string(),
            ),
            (
                400,
                r#"{"ok":false,"error_code":400,"description":"Bad Request: chat not found"}"#.to_string(),
            ),
        ]);
        let bot = UreqBot::new(&base, "123:SECRET", Duration::from_secs(2));

        assert_eq!(bot.get_me().unwrap_err(), BotError::Unauthorized);
        assert_eq!(
            bot.get_updates(None, Duration::ZERO).unwrap_err(),
            BotError::Conflict
        );
        let error = bot.send_message(1, None, None, "x").unwrap_err();
        assert_eq!(
            error,
            BotError::Rejected("Bad Request: chat not found".to_string())
        );

        let unreachable = UreqBot::new("http://127.0.0.1:9", "123:SECRET", Duration::from_secs(1));
        let error = unreachable.get_me().unwrap_err();
        assert!(matches!(error, BotError::Unreachable(_)));
        assert!(!error.to_string().contains("SECRET"));
    }

    #[test]
    fn a_reply_in_a_private_chat_is_not_mistaken_for_a_topic() {
        // Telegram sets message_thread_id on replies too; only is_topic_message says
        // the message was typed inside a forum topic.
        let message: Message = serde_json::from_str(
            r#"{"message_id":3,"chat":{"id":1,"type":"private"},"from":{"id":7},"text":"hi","message_thread_id":2,"reply_to_message":{"message_id":2}}"#,
        )
        .unwrap();
        assert_eq!(message.topic_id(), None);
        assert_eq!(message.reply_to_message.unwrap().message_id, 2);

        let topic: Message = serde_json::from_str(
            r#"{"message_id":3,"chat":{"id":-100,"type":"supergroup","is_forum":true},"from":{"id":7},"text":"hi","message_thread_id":55,"is_topic_message":true}"#,
        )
        .unwrap();
        assert_eq!(topic.topic_id(), Some(55));
    }

    #[test]
    fn a_topic_name_fits_telegrams_bounds() {
        assert_eq!(topic_name("  \u{7}  "), "Session");
        assert_eq!(topic_name("a\nb").chars().count(), 3);
        assert_eq!(topic_name(&"x".repeat(300)).chars().count(), 128);
    }

    #[test]
    fn only_an_admin_who_may_manage_topics_or_the_creator_can_run_a_forum() {
        let admin: ChatMember =
            serde_json::from_str(r#"{"status":"administrator","can_manage_topics":true}"#).unwrap();
        let limited: ChatMember =
            serde_json::from_str(r#"{"status":"administrator","can_manage_topics":false}"#)
                .unwrap();
        let member: ChatMember = serde_json::from_str(r#"{"status":"member"}"#).unwrap();
        let creator: ChatMember = serde_json::from_str(r#"{"status":"creator"}"#).unwrap();
        assert!(admin.may_manage_topics());
        assert!(!limited.may_manage_topics());
        assert!(!member.may_manage_topics());
        assert!(creator.may_manage_topics());
    }
}
