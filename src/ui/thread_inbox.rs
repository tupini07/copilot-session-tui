//! What is waiting on you, and what happens if you accept it.
//!
//! This modal is the only place a GitHub comment can cause a closed session to start
//! running again. That is deliberate: a comment from outside, or one that arrived while
//! a session was closed, must be something the user chose to act on rather than
//! something that happened while they were looking elsewhere.
//!
//! It opens when asked for, never on arrival. An unsolicited popup appearing while
//! somebody is deep in another pane teaches them to dismiss it without reading.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::App;
use crate::theme::Theme;
use crate::threads::Notice;

pub fn draw(f: &mut Frame, app: &App) {
    let Some(selected) = app.thread_inbox else {
        return;
    };
    let theme = app.theme();

    let height = (app.thread_pending.len() * 2 + 10).min(20) as u16;
    let percent_y = ((height as f32 / f.area().height as f32) * 100.0).min(75.0) as u16;
    let area = super::popups::centered_rect(72, percent_y.max(40), f.area());
    super::popups::prepare_popup(f, area, theme);

    let mut lines = vec![Line::from("")];
    if app.thread_pending.is_empty() {
        lines.push(Line::from(Span::styled(
            "  Nothing is waiting.",
            Style::default().fg(theme.muted),
        )));
    } else {
        for (index, pending) in app.thread_pending.iter().enumerate() {
            lines.extend(entry_lines(app, pending, index == selected, theme));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            "Enter",
            Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" open the session and hand it the link    "),
        Span::styled(
            "d",
            Style::default()
                .fg(theme.success)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" dismiss    "),
        Span::styled(
            "Esc",
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" close"),
    ]));

    let block = Block::default()
        .title(" Waiting for you ")
        .borders(Borders::ALL)
        .style(super::popups::surface_style(theme))
        .border_style(Style::default().fg(theme.warning));

    f.render_widget(
        Paragraph::new(lines)
            .style(super::popups::surface_style(theme))
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// One waiting message: which session, which thread, and why it is here.
///
/// The reason is spelled out because the three are not interchangeable — a comment from
/// a stranger is a very different thing to accept than a reply to your own agent, and
/// the user is about to decide whether to start a process because of it.
fn entry_lines<'a>(app: &App, pending: &Notice, selected: bool, theme: Theme) -> Vec<Line<'a>> {
    let marker = if selected { "> " } else { "  " };
    let name = app
        .sessions
        .iter()
        .find(|session| session.id == pending.session_id)
        .map(|session| session.display_name().to_string())
        .unwrap_or_else(|| format!("session {}", short(&pending.session_id)));

    let title_style = if selected {
        Style::default().fg(theme.text).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.text)
    };

    // Naming who wrote it, because "someone else" cannot answer the question being
    // asked: whether to let this person's words start work on your machine. It is also
    // the name to add to the trusted list if the answer is yes.
    let reason = match (&pending.author, pending.reason()) {
        (Some(author), Some(crate::threads::PendingReason::ForeignAuthor)) => {
            format!("  (written by {author})")
        }
        (_, Some(reason)) => format!("  ({})", reason.describe()),
        // Only notices waiting on the user reach this list, so this is unreachable in
        // practice; saying nothing beats asserting in a draw call.
        (_, None) => String::new(),
    };

    vec![
        Line::from(vec![
            Span::styled(format!("{marker}{name}"), title_style),
            Span::styled(reason, Style::default().fg(theme.warning)),
        ]),
        Line::from(Span::styled(
            format!("    {}", pending.thread.url()),
            Style::default().fg(theme.directory),
        )),
    ]
}

fn short(session_id: &str) -> &str {
    session_id.get(..8).unwrap_or(session_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::UserConfig;
    use crate::threads::{PendingReason, ThreadKind, ThreadRef};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn pending(reason: PendingReason) -> Notice {
        Notice {
            session_id: "abcdef123456".to_string(),
            thread: ThreadRef {
                host: "github.com".to_string(),
                owner: "microsoft".to_string(),
                repo: "maps".to_string(),
                number: 2366,
                kind: ThreadKind::Issue,
            },
            comment_url: "https://github.com/microsoft/maps/issues/2366".to_string(),
            status: crate::threads::NoticeStatus::Waiting { reason },
            author: None,
            planned_at: chrono::Utc::now(),
        }
    }

    fn rendered(reason: PendingReason) -> String {
        let mut app = App::new(Vec::new(), UserConfig::default());
        app.thread_pending = vec![pending(reason)];
        app.thread_inbox = Some(0);

        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(f, &app)).expect("draw succeeds");
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn the_thread_is_named_so_the_decision_can_be_made_without_guessing() {
        let screen = rendered(PendingReason::SessionClosed);

        assert!(screen.contains("2366"), "got: {screen}");
        assert!(screen.contains("microsoft/maps"), "got: {screen}");
    }

    #[test]
    fn a_comment_from_an_outsider_says_so_before_anything_is_started() {
        // Accepting this starts a process because of something a stranger wrote. The
        // user has to be told that is what they are agreeing to.
        let screen = rendered(PendingReason::ForeignAuthor);

        assert!(screen.contains("written by someone else"), "got: {screen}");
    }

    #[test]
    fn a_known_author_is_named_so_the_decision_can_be_made() {
        // "Someone else" cannot answer the question being asked. The name is both what
        // makes the decision possible and what you would add to the trusted list.
        let mut app = App::new(Vec::new(), UserConfig::default());
        let mut held = pending(PendingReason::ForeignAuthor);
        held.author = Some("a-colleague".to_string());
        app.thread_pending = vec![held];
        app.thread_inbox = Some(0);

        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(f, &app)).expect("draw succeeds");
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();

        assert!(screen.contains("a-colleague"), "got: {screen}");
    }

    #[test]
    fn the_way_out_is_always_on_screen() {
        let screen = rendered(PendingReason::Throttled);

        assert!(screen.contains("Esc"), "got: {screen}");
        assert!(screen.contains("dismiss"), "got: {screen}");
    }
}
