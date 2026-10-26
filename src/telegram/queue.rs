//! Seeing, and taking back, the replies waiting for a turn to end.
//!
//! A reply sent while its session is working is queued and the phone is told so once.
//! Until now that queue was a black box: nothing showed what was waiting and nothing
//! could withdraw a message. Now the "Queued" notice carries a Cancel button, and
//! `/queue` lists what is waiting — for one session when sent to it, for every
//! session when sent bare — one Cancel per line. A tap is answered on the spot: the
//! reply leaves the queue if it is still there, or the phone hears it already went in.
//!
//! Editing a queued reply is not offered. On a phone, cancel-and-resend is the same
//! gesture with fewer ways to go wrong.
//!
//! A button's `callback_data` names the reply and where the button sat: a tap on the
//! notice turns that notice into "Cancelled", a tap on a listing redraws the listing.
//! Everything here is pure; the poller acts on taps and the UI thread sends notices.

use chrono::{DateTime, Utc};

use super::bot::Button;
use super::escape_html;
use super::store::Reply;

/// What the Cancel button says.
pub const CANCEL_LABEL: &str = "Cancel";
/// How much of a queued reply the listing quotes.
const QUOTE_WIDTH: usize = 80;

/// `/queue` or `/queue@bot`, in any case; anything else is not CST's.
pub fn is_queue_command(text: &str) -> bool {
    text.split_whitespace()
        .next()
        .and_then(|word| word.split('@').next())
        .is_some_and(|command| command.eq_ignore_ascii_case("/queue"))
}

/// Where a Cancel button sat, which decides what its message becomes when tapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelSource {
    /// The "Queued …" notice for one reply.
    Notice,
    /// A `/queue` listing, possibly with other replies on it.
    Listing,
}

/// The data a Cancel button carries. Short, because Telegram allows 64 bytes.
pub fn cancel_data(update_id: i64, source: CancelSource) -> String {
    match source {
        CancelSource::Notice => format!("c:{update_id}"),
        CancelSource::Listing => format!("q:{update_id}"),
    }
}

/// Read a Cancel button's data back. `None` for anything not made by [`cancel_data`].
pub fn parse_cancel_data(data: &str) -> Option<(i64, CancelSource)> {
    let (prefix, id) = data.split_once(':')?;
    let source = match prefix {
        "c" => CancelSource::Notice,
        "q" => CancelSource::Listing,
        _ => return None,
    };
    id.parse().ok().map(|update_id| (update_id, source))
}

pub fn cancel_button(update_id: i64, source: CancelSource) -> Button {
    Button {
        text: CANCEL_LABEL.to_string(),
        callback_data: cancel_data(update_id, source),
    }
}

/// One line of a listing: the session's name and the reply, as the caller resolved
/// them.
pub struct QueuedEntry<'a> {
    pub title: &'a str,
    pub reply: &'a Reply,
}

/// The `/queue` listing and its buttons. An empty queue is a sentence, no buttons.
pub fn render_listing(entries: &[QueuedEntry<'_>], now: DateTime<Utc>) -> (String, Vec<Button>) {
    if entries.is_empty() {
        return (wording::nothing_queued(), Vec::new());
    }
    let mut lines = vec!["<b>Waiting to be delivered</b>".to_string()];
    let mut buttons = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let number = index + 1;
        let waited = now
            .signed_duration_since(entry.reply.received_at)
            .num_minutes()
            .max(0);
        lines.push(format!(
            "{number}. <b>{}</b> — “{}” <i>({waited} min)</i>",
            escape_html(entry.title),
            escape_html(&quote(&entry.reply.text))
        ));
        buttons.push(Button {
            text: format!("{CANCEL_LABEL} {number}"),
            callback_data: cancel_data(entry.reply.update_id, CancelSource::Listing),
        });
    }
    (lines.join("\n"), buttons)
}

/// The start of a reply, on one line, with an ellipsis when cut.
fn quote(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= QUOTE_WIDTH {
        return flat;
    }
    let mut shown: String = flat.chars().take(QUOTE_WIDTH).collect();
    shown.push('…');
    shown
}

pub mod wording {
    pub fn nothing_queued() -> String {
        "Nothing is waiting to be delivered.".to_string()
    }

    pub fn cancelled(title: &str) -> String {
        format!("Cancelled: your reply to {title} was taken out of the queue and not delivered.")
    }

    pub fn delivered(title: &str) -> String {
        format!("Delivered to {title}.")
    }

    /// The small notice on the phone after a tap.
    pub fn toast_cancelled() -> &'static str {
        "Cancelled"
    }

    pub fn toast_already_delivered() -> &'static str {
        "Already delivered"
    }

    pub fn toast_gone() -> &'static str {
        "That reply is no longer queued"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telegram::store::ReplyStatus;

    fn reply(update_id: i64, text: &str, minutes_ago: i64) -> Reply {
        let received_at = Utc::now() - chrono::Duration::minutes(minutes_ago);
        Reply {
            update_id,
            user_message_id: update_id * 10,
            session_id: "s".to_string(),
            text: text.to_string(),
            tool_call_id: None,
            received_at,
            planned_at: received_at,
            queued_told: true,
            answering: false,
            queued_message_id: None,
            status: ReplyStatus::Planned,
        }
    }

    #[test]
    fn only_queue_is_the_queue_command() {
        assert!(is_queue_command("/queue"));
        assert!(is_queue_command("  /Queue@cst_bot "));
        assert!(!is_queue_command("/queued up"));
        assert!(!is_queue_command("queue"));
        assert!(!is_queue_command("/compact"));
    }

    #[test]
    fn a_cancel_buttons_data_names_the_reply_and_where_it_sat_and_reads_back() {
        for source in [CancelSource::Notice, CancelSource::Listing] {
            let data = cancel_data(4242, source);
            assert!(data.len() <= 64);
            assert_eq!(parse_cancel_data(&data), Some((4242, source)));
        }
        assert_eq!(parse_cancel_data("x:1"), None);
        assert_eq!(parse_cancel_data("c:notanumber"), None);
        assert_eq!(parse_cancel_data("nonsense"), None);
        assert_eq!(cancel_button(7, CancelSource::Notice).text, "Cancel");
    }

    #[test]
    fn the_listing_numbers_each_waiting_reply_with_its_own_cancel_and_quotes_it_short() {
        let long = "word ".repeat(40);
        let replies = [reply(1, "Stop and write tests", 3), reply(2, &long, 0)];
        let entries = [
            QueuedEntry {
                title: "Plan <A>",
                reply: &replies[0],
            },
            QueuedEntry {
                title: "B",
                reply: &replies[1],
            },
        ];
        let (html, buttons) = render_listing(&entries, Utc::now());
        assert!(html.starts_with("<b>Waiting to be delivered</b>\n"));
        assert!(html.contains("1. <b>Plan &lt;A&gt;</b> — “Stop and write tests” <i>(3 min)</i>"));
        assert!(html.contains("2. <b>B</b> — “word word"));
        assert!(html.contains("…” <i>(0 min)</i>"));
        assert_eq!(buttons.len(), 2);
        assert_eq!(buttons[0].text, "Cancel 1");
        assert_eq!(buttons[0].callback_data, "q:1");
        assert_eq!(buttons[1].callback_data, "q:2");

        let (empty, none) = render_listing(&[], Utc::now());
        assert_eq!(empty, "Nothing is waiting to be delivered.");
        assert!(none.is_empty());
    }
}
