use crate::app::{App, View};
use crate::mux::{resolve_prefix_command, MuxEvent, PrefixCommand};
use crossterm::event::{Event, KeyEvent, KeyEventKind};

/// Route a terminal event while a pane is focused.
///
/// Everything except the prefix key is forwarded to the child, because Copilot wants
/// nearly every keystroke for itself.
pub fn handle_attached_event(app: &mut App, event: Event) {
    match event {
        Event::Key(key) => {
            if key.kind != KeyEventKind::Press {
                return;
            }
            handle_attached_key(app, key);
        }
        Event::Paste(text) => {
            if let Some(pane) = app.mux.as_mut().and_then(|mux| mux.focused_pane_mut()) {
                let _ = pane.send_paste(&text);
            }
        }
        _ => {}
    }
}

fn handle_attached_key(app: &mut App, key: KeyEvent) {
    let Some(mux) = app.mux.as_mut() else {
        return;
    };

    if mux.prefix_pending {
        mux.prefix_pending = false;
        let prefix = mux.prefix;
        match resolve_prefix_command(&key, &prefix) {
            Some(PrefixCommand::Literal) => {
                // Double prefix: the child gets a real prefix keystroke.
                if let Some(pane) = mux.focused_pane_mut() {
                    let _ = pane.send_key(&key);
                }
            }
            Some(PrefixCommand::Detach) => app.detach(),
            Some(PrefixCommand::NextPane) => mux.cycle(true),
            Some(PrefixCommand::PreviousPane) => mux.cycle(false),
            Some(PrefixCommand::KillPane) => kill_focused(app),
            Some(PrefixCommand::PaneList) => {
                app.open_pane_list();
                return;
            }
            Some(PrefixCommand::SelectIndex(index)) => {
                // Panes are labelled from 1 in the UI.
                mux.select_index(index.saturating_sub(1));
            }
            Some(PrefixCommand::Cancel) | None => {}
        }
        sync_view(app);
        return;
    }

    if mux.prefix.matches(&key) {
        mux.prefix_pending = true;
        return;
    }

    if let Some(pane) = mux.focused_pane_mut() {
        if pane.is_running() {
            let _ = pane.send_key(&key);
        } else if matches!(
            key.code,
            crossterm::event::KeyCode::Enter | crossterm::event::KeyCode::Esc
        ) {
            // A dead pane keeps its final screen until dismissed.
            kill_focused(app);
        }
    }
}

fn kill_focused(app: &mut App) {
    let Some(mux) = app.mux.as_mut() else {
        return;
    };
    if let Some(id) = mux.focused {
        mux.remove(id);
    }
    sync_view(app);
}

/// Keep the visible view consistent with the focused pane, falling back to the list.
pub fn sync_view(app: &mut App) {
    let focused = app.mux.as_ref().and_then(|mux| mux.focused);
    app.view = match (app.view, focused) {
        (View::Attached(_), Some(id)) => View::Attached(id),
        (View::Attached(_), None) => View::List,
        (View::List, _) => View::List,
    };
}

/// Apply a PTY event. Returns true when the UI needs a repaint.
pub fn handle_mux_event(app: &mut App, event: MuxEvent) -> bool {
    match event {
        MuxEvent::Output(id) => {
            // Titles and bells only surface once the child's escape sequences are parsed.
            let bell = app
                .mux
                .as_mut()
                .and_then(|mux| mux.pane_mut(id))
                .map(|pane| pane.refresh_from_callbacks())
                .unwrap_or(false);
            if bell {
                if let Some(title) = app
                    .mux
                    .as_ref()
                    .and_then(|mux| mux.pane(id))
                    .map(|pane| pane.title.clone())
                {
                    app.status_message = Some(format!("🔔 {title}"));
                }
            }
            matches!(app.view, View::Attached(focused) if focused == id) || bell
        }
        MuxEvent::Exited(id, code) => {
            if let Some(mux) = app.mux.as_mut() {
                if let Some(pane) = mux.pane_mut(id) {
                    pane.mark_exited(code);
                }
            }
            let title = app
                .mux
                .as_ref()
                .and_then(|mux| mux.pane(id))
                .map(|pane| pane.title.clone())
                .unwrap_or_default();
            app.status_message = Some(match code {
                Some(0) | None => format!("Session '{title}' finished"),
                Some(code) => format!("Session '{title}' exited with code {code}"),
            });
            true
        }
        MuxEvent::Term(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::UserConfig;
    use crossterm::event::{KeyCode, KeyModifiers};

    fn mux_app() -> App {
        let config = UserConfig {
            mux: true,
            ..UserConfig::default()
        };
        App::new(Vec::new(), config)
    }

    #[test]
    fn prefix_key_is_swallowed_and_arms_the_state_machine() {
        let mut app = mux_app();
        let prefix = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL);

        handle_attached_key(&mut app, prefix);

        assert!(app.mux.as_ref().unwrap().prefix_pending);
    }

    #[test]
    fn prefix_then_d_detaches_to_the_list() {
        let mut app = mux_app();
        app.view = View::Attached(1);
        handle_attached_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
        );

        handle_attached_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
        );

        assert_eq!(app.view, View::List);
        assert!(!app.mux.as_ref().unwrap().prefix_pending);
    }

    #[test]
    fn an_unknown_prefix_command_clears_the_pending_state() {
        let mut app = mux_app();
        handle_attached_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
        );

        handle_attached_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE),
        );

        assert!(
            !app.mux.as_ref().unwrap().prefix_pending,
            "a stray prefix must not swallow every later keystroke"
        );
    }

    #[test]
    fn attached_view_falls_back_to_the_list_when_no_panes_remain() {
        let mut app = mux_app();
        app.view = View::Attached(7);

        sync_view(&mut app);

        assert_eq!(app.view, View::List);
    }

    #[test]
    fn output_for_an_unfocused_pane_does_not_force_a_repaint() {
        let mut app = mux_app();
        app.view = View::Attached(1);

        assert!(handle_mux_event(&mut app, MuxEvent::Output(1)));
        assert!(!handle_mux_event(&mut app, MuxEvent::Output(2)));
    }

    #[test]
    fn exit_events_report_the_status() {
        let mut app = mux_app();

        assert!(handle_mux_event(&mut app, MuxEvent::Exited(1, Some(2))));

        let message = app.status_message.unwrap();
        assert!(message.contains("code 2"), "unexpected message: {message}");
    }
}
