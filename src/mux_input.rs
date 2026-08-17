use crate::app::{App, View, WorkspaceFocus, WorkspaceHelp};
use crate::mux::{
    resolve_help_command, resolve_prefix_command, HelpCommand, MuxEvent, PrefixCommand,
};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, MouseEventKind};
use ratatui::layout::Rect;

/// Route a terminal event while a pane is focused.
///
/// Everything except the prefix key is forwarded to the child, because Copilot wants
/// nearly every keystroke for itself.
pub fn handle_attached_event(app: &mut App, event: Event) {
    if app.workspace_help.is_some() {
        if matches!(
            event,
            Event::Key(KeyEvent {
                code: KeyCode::Esc,
                kind: KeyEventKind::Press,
                ..
            })
        ) {
            app.workspace_help = None;
        }
        return;
    }

    if let Event::Key(key) = &event {
        if key.kind != KeyEventKind::Press {
            return;
        }
        let is_prefix = app
            .mux
            .as_ref()
            .is_some_and(|mux| mux.prefix_pending || mux.help_pending || mux.prefix.matches(key));
        if is_prefix {
            handle_attached_key(app, *key);
            return;
        }
    }

    if let Event::Mouse(mouse) = &event {
        if matches!(mouse.kind, MouseEventKind::Down(_)) {
            focus_clicked_workspace(app, mouse.column, mouse.row);
        }
    }

    match app.workspace_focus {
        WorkspaceFocus::Chat => handle_chat_event(app, event),
        WorkspaceFocus::Scratchpad => handle_scratchpad_event(app, event),
        WorkspaceFocus::Terminal => handle_terminal_event(app, event),
    }
}

fn handle_chat_event(app: &mut App, event: Event) {
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
        Event::Mouse(mouse) => {
            if let Some(pane) = app.mux.as_mut().and_then(|mux| mux.focused_pane_mut()) {
                let _ = pane.handle_mouse(mouse);
            }
        }
        _ => {}
    }
}

fn handle_attached_key(app: &mut App, key: KeyEvent) {
    let Some(mux) = app.mux.as_ref() else {
        return;
    };

    if mux.help_pending {
        if let Some(mux) = app.mux.as_mut() {
            mux.help_pending = false;
        }
        if matches!(resolve_help_command(&key), Some(HelpCommand::Scratchpad)) {
            app.workspace_help = Some(WorkspaceHelp::Scratchpad);
        }
        return;
    }

    if mux.prefix_pending {
        let prefix = mux.prefix;
        let command = resolve_prefix_command(&key, &prefix);
        if let Some(mux) = app.mux.as_mut() {
            mux.prefix_pending = false;
        }
        match command {
            Some(PrefixCommand::Literal) => {
                // Double prefix: the child gets a real prefix keystroke.
                if let Some(pane) = app.mux.as_mut().and_then(|mux| mux.focused_pane_mut()) {
                    let _ = pane.send_key(&key);
                }
            }
            Some(PrefixCommand::Detach) => app.detach(),
            Some(PrefixCommand::NextPane) => {
                if let Some(mux) = app.mux.as_mut() {
                    mux.cycle(true);
                }
                sync_workspace_panels(app);
            }
            Some(PrefixCommand::PreviousPane) => {
                if let Some(mux) = app.mux.as_mut() {
                    mux.cycle(false);
                }
                sync_workspace_panels(app);
            }
            Some(PrefixCommand::KillPane) => kill_focused(app),
            Some(PrefixCommand::PaneList) => {
                app.open_pane_list();
                return;
            }
            Some(PrefixCommand::Chat) => focus_chat(app),
            Some(PrefixCommand::Scratchpad) => toggle_attached_scratchpad(app),
            Some(PrefixCommand::Terminal) => toggle_attached_terminal(app),
            Some(PrefixCommand::Help) => {
                if let Some(mux) = app.mux.as_mut() {
                    mux.help_pending = true;
                }
            }
            Some(PrefixCommand::SelectIndex(index)) => {
                // Panes are labelled from 1 in the UI.
                if let Some(mux) = app.mux.as_mut() {
                    mux.select_index(index.saturating_sub(1));
                }
                sync_workspace_panels(app);
            }
            Some(PrefixCommand::Cancel) | None => {}
        }
        sync_view(app);
        return;
    }

    if mux.prefix.matches(&key) {
        if let Some(mux) = app.mux.as_mut() {
            mux.prefix_pending = true;
        }
        return;
    }

    if let Some(pane) = app.mux.as_mut().and_then(|mux| mux.focused_pane_mut()) {
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

fn handle_scratchpad_event(app: &mut App, event: Event) {
    let Some(scratchpad) = app.scratchpad.as_mut() else {
        app.workspace_focus = WorkspaceFocus::Chat;
        return;
    };
    let outcome = scratchpad.handle_event(event);
    match outcome {
        Ok(crate::scratchpad::InputOutcome::Continue) => {}
        Ok(crate::scratchpad::InputOutcome::Close) => {
            let context = focused_workspace_context(app);
            app.scratchpad = None;
            app.scratchpad_owner = None;
            if let Some((pane_id, session_id, _, _)) = context {
                app.remember_scratchpad_panel(pane_id, &session_id, false);
            }
            app.workspace_focus = WorkspaceFocus::Chat;
        }
        Err(error) => {
            scratchpad.status_message = Some(format!("Scratchpad save failed: {error}"));
        }
    }
}

fn handle_terminal_event(app: &mut App, event: Event) {
    if let Event::Key(key) = &event {
        if key.kind == KeyEventKind::Press
            && key.code == KeyCode::Enter
            && app
                .terminal
                .active()
                .is_some_and(|terminal| !terminal.is_running())
        {
            if let Err(error) = app.terminal.restart_active(&app.config.terminal) {
                app.status_message = Some(format!("Cannot restart terminal: {error}"));
            }
            return;
        }
    }
    if let Some(terminal) = app.terminal.active_mut() {
        if let Err(error) = terminal.handle_event(event) {
            app.status_message = Some(format!("Terminal input failed: {error}"));
        }
    } else {
        app.workspace_focus = WorkspaceFocus::Chat;
    }
}

fn focus_chat(app: &mut App) {
    app.workspace_focus = WorkspaceFocus::Chat;
    app.terminal.unfocus();
}

fn toggle_attached_scratchpad(app: &mut App) {
    let Some((pane_id, session_id, _, _)) = focused_workspace_context(app) else {
        return;
    };
    if app.scratchpad_owner == Some(pane_id) && app.scratchpad.is_some() {
        if app.workspace_focus == WorkspaceFocus::Scratchpad {
            if let Some(scratchpad) = app.scratchpad.as_mut() {
                if let Err(error) = scratchpad.save() {
                    scratchpad.status_message = Some(format!("Scratchpad save failed: {error}"));
                    return;
                }
            }
            app.scratchpad = None;
            app.scratchpad_owner = None;
            app.remember_scratchpad_panel(pane_id, &session_id, false);
            app.workspace_focus = WorkspaceFocus::Chat;
        } else {
            app.workspace_focus = WorkspaceFocus::Scratchpad;
            app.terminal.unfocus();
        }
        return;
    }

    if !close_scratchpad(app) {
        return;
    }
    match crate::scratchpad::Scratchpad::open(&session_id) {
        Ok(scratchpad) => {
            app.scratchpad = Some(scratchpad);
            app.scratchpad_owner = Some(pane_id);
            app.remember_scratchpad_panel(pane_id, &session_id, true);
            app.workspace_focus = WorkspaceFocus::Scratchpad;
            app.terminal.unfocus();
        }
        Err(error) => {
            app.status_message = Some(format!("Cannot open scratchpad: {error}"));
        }
    }
}

fn toggle_attached_terminal(app: &mut App) {
    let Some((pane_id, session_id, title, cwd)) = focused_workspace_context(app) else {
        return;
    };
    if app.attached_terminal_visible()
        && app.terminal.active_session_id() == Some(session_id.as_str())
        && app.workspace_focus == WorkspaceFocus::Terminal
    {
        app.terminal.hide();
        app.terminal_owner = None;
        app.remember_terminal_panel(pane_id, &session_id, false);
        app.workspace_focus = WorkspaceFocus::Chat;
        return;
    }

    match app
        .terminal
        .activate(session_id.clone(), title, cwd, &app.config.terminal)
    {
        Ok(_) => {
            app.terminal_owner = Some(pane_id);
            app.remember_terminal_panel(pane_id, &session_id, true);
            app.workspace_focus = WorkspaceFocus::Terminal;
        }
        Err(error) => app.status_message = Some(format!("Cannot open terminal: {error}")),
    }
}

pub fn sync_workspace_panels(app: &mut App) {
    let Some((pane_id, session_id, title, cwd)) = focused_workspace_context(app) else {
        let _ = close_scratchpad(app);
        app.terminal.hide();
        app.terminal_owner = None;
        app.scratchpad_open.clear();
        app.terminal_open.clear();
        app.workspace_focus = WorkspaceFocus::Chat;
        return;
    };

    if app.scratchpad_owner != Some(pane_id) || app.scratchpad.is_none() {
        if !close_scratchpad(app) {
            return;
        }
        if app.scratchpad_open.contains(&pane_id) {
            match crate::scratchpad::Scratchpad::open(&session_id) {
                Ok(scratchpad) => {
                    app.scratchpad = Some(scratchpad);
                    app.scratchpad_owner = Some(pane_id);
                }
                Err(error) => {
                    app.scratchpad_open.remove(&pane_id);
                    app.status_message = Some(format!("Cannot switch scratchpad: {error}"));
                }
            }
        }
    }

    if app.terminal_open.contains(&pane_id) {
        let activated = match app
            .terminal
            .activate(session_id, title, cwd, &app.config.terminal)
        {
            Ok(_) => {
                if app.workspace_focus != WorkspaceFocus::Terminal {
                    app.terminal.unfocus();
                }
                true
            }
            Err(error) => {
                app.terminal.hide();
                app.terminal_owner = None;
                app.terminal_open.remove(&pane_id);
                app.status_message = Some(format!("Cannot switch terminal: {error}"));
                false
            }
        };
        if activated {
            app.terminal_owner = Some(pane_id);
        }
    } else {
        app.terminal.hide();
        app.terminal_owner = None;
    }

    if (!app.attached_scratchpad_visible() && app.workspace_focus == WorkspaceFocus::Scratchpad)
        || (!app.attached_terminal_visible() && app.workspace_focus == WorkspaceFocus::Terminal)
    {
        app.workspace_focus = WorkspaceFocus::Chat;
    }
}

fn close_scratchpad(app: &mut App) -> bool {
    if let Some(scratchpad) = app.scratchpad.as_mut() {
        if let Err(error) = scratchpad.save() {
            app.status_message = Some(format!("Scratchpad save failed: {error}"));
            return false;
        }
    }
    app.scratchpad = None;
    app.scratchpad_owner = None;
    true
}

fn focused_workspace_context(app: &App) -> Option<(u64, String, String, String)> {
    let pane = app.mux.as_ref()?.focused_pane()?;
    let session_id = pane
        .session_id
        .clone()
        .unwrap_or_else(|| format!("mux-pane-{}", pane.id));
    Some((
        pane.id,
        session_id,
        pane.title.clone(),
        pane.cwd.to_string_lossy().to_string(),
    ))
}

fn focus_clicked_workspace(app: &mut App, column: u16, row: u16) {
    let areas = app.workspace_areas;
    if areas
        .scratchpad
        .is_some_and(|area| contains(area, column, row))
    {
        app.workspace_focus = WorkspaceFocus::Scratchpad;
        app.terminal.unfocus();
    } else if areas
        .terminal
        .is_some_and(|area| contains(area, column, row))
    {
        app.workspace_focus = WorkspaceFocus::Terminal;
        app.terminal.focus();
    } else if contains(areas.chat, column, row) {
        app.workspace_focus = WorkspaceFocus::Chat;
        app.terminal.unfocus();
    }
}

fn contains(area: Rect, column: u16, row: u16) -> bool {
    column >= area.x && column < area.right() && row >= area.y && row < area.bottom()
}

/// Route a key from the session list through the prefix state machine.
///
/// Returns true when the key was consumed, so panes remain reachable from the list
/// instead of only from inside an attached pane.
pub fn handle_list_prefix(app: &mut App, key: KeyEvent) -> bool {
    let Some(mux) = app.mux.as_mut() else {
        return false;
    };

    if mux.help_pending {
        mux.help_pending = false;
        if matches!(resolve_help_command(&key), Some(HelpCommand::Scratchpad)) {
            app.workspace_help = Some(WorkspaceHelp::Scratchpad);
        }
        return true;
    }

    if !mux.prefix_pending {
        if mux.prefix.matches(&key) {
            mux.prefix_pending = true;
            return true;
        }
        return false;
    }

    mux.prefix_pending = false;
    let prefix = mux.prefix;
    let command = resolve_prefix_command(&key, &prefix);
    if matches!(command, Some(PrefixCommand::Help)) {
        mux.help_pending = true;
        return true;
    }
    if mux.panes.is_empty() {
        app.status_message = Some("No sessions are running".to_string());
        return true;
    }

    match command {
        // There is no child to re-attach to and nothing to detach from, so the only
        // sensible reading of these from the list is "show me what's running".
        Some(PrefixCommand::Detach) | Some(PrefixCommand::Literal) => app.open_pane_list(),
        Some(PrefixCommand::PaneList) => app.open_pane_list(),
        Some(PrefixCommand::Chat) => {
            attach_focused(app);
            focus_chat(app);
        }
        Some(PrefixCommand::Scratchpad) => {
            attach_focused(app);
            toggle_attached_scratchpad(app);
        }
        Some(PrefixCommand::Terminal) => {
            attach_focused(app);
            toggle_attached_terminal(app);
        }
        Some(PrefixCommand::Help) => unreachable!("handled before pane availability"),
        Some(PrefixCommand::NextPane) => {
            mux.cycle(true);
            attach_focused(app);
        }
        Some(PrefixCommand::PreviousPane) => {
            mux.cycle(false);
            attach_focused(app);
        }
        Some(PrefixCommand::SelectIndex(index)) => {
            // Panes are labelled from 1 in the UI.
            mux.select_index(index.saturating_sub(1));
            attach_focused(app);
        }
        Some(PrefixCommand::KillPane) => kill_focused(app),
        Some(PrefixCommand::Cancel) | None => {}
    }
    true
}

/// Bring the focused pane back on screen after the list changed it.
fn attach_focused(app: &mut App) {
    if let Some(id) = app.mux.as_ref().and_then(|mux| mux.focused) {
        app.view = View::Attached(id);
        sync_workspace_panels(app);
    }
}

fn kill_focused(app: &mut App) {
    let Some(id) = app.mux.as_ref().and_then(|mux| mux.focused) else {
        return;
    };
    if !app.forget_workspace_panels(id) {
        return;
    }
    if let Some(mux) = app.mux.as_mut() {
        mux.remove(id);
    }
    sync_workspace_panels(app);
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
        MuxEvent::HostSequence(sequence) => {
            app.host_sequences.push(sequence);
            true
        }
        MuxEvent::Term(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::UserConfig;
    use crate::mux::{Pane, PaneSpec};
    use crossterm::event::{KeyCode, KeyModifiers};

    fn mux_app() -> App {
        let config = UserConfig {
            mux: true,
            ..UserConfig::default()
        };
        let mut app = App::new(Vec::new(), config);
        app.disable_workspace_state_persistence();
        app
    }

    fn attached_mux_app(session_id: &str) -> App {
        let mut app = mux_app();
        push_test_pane(&mut app, 1, session_id);
        app.view = View::Attached(1);
        app
    }

    fn push_test_pane(app: &mut App, id: u64, session_id: &str) {
        let events = app.mux.as_ref().unwrap().events.clone();
        let (program, args) = if cfg!(windows) {
            (
                "cmd.exe".to_string(),
                vec!["/c".to_string(), "ping -n 30 127.0.0.1 >nul".to_string()],
            )
        } else {
            (
                "/bin/sh".to_string(),
                vec!["-c".to_string(), "sleep 30".to_string()],
            )
        };
        let pane = Pane::spawn(
            PaneSpec {
                id,
                title: format!("Test session {id}"),
                cwd: std::env::temp_dir(),
                session_id: Some(session_id.to_string()),
                program,
                args,
            },
            24,
            80,
            events,
        )
        .unwrap();
        app.mux.as_mut().unwrap().push(pane);
    }

    fn send_prefix_command(app: &mut App, command: char) {
        handle_attached_key(
            app,
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
        );
        handle_attached_key(
            app,
            KeyEvent::new(KeyCode::Char(command), KeyModifiers::NONE),
        );
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
    fn the_prefix_is_recognised_from_the_session_list() {
        let mut app = mux_app();
        app.view = View::List;

        let consumed = handle_list_prefix(
            &mut app,
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
        );

        assert!(
            consumed,
            "the prefix must not fall through to list bindings"
        );
        assert!(app.mux.as_ref().unwrap().prefix_pending);
    }

    #[test]
    fn ordinary_list_keys_are_left_alone_when_no_prefix_is_pending() {
        let mut app = mux_app();
        app.view = View::List;

        let consumed = handle_list_prefix(
            &mut app,
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
        );

        assert!(
            !consumed,
            "'n' must still create a new session from the list"
        );
    }

    #[test]
    fn a_prefix_command_from_the_list_reports_when_nothing_is_running() {
        let mut app = mux_app();
        app.view = View::List;
        handle_list_prefix(
            &mut app,
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
        );

        let consumed = handle_list_prefix(
            &mut app,
            KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE),
        );

        assert!(consumed);
        assert!(!app.mux.as_ref().unwrap().prefix_pending);
        assert_eq!(app.mode, crate::app::Mode::Normal, "no panes, no switcher");
        assert!(app.status_message.is_some());
    }

    #[test]
    fn scratchpad_help_is_available_from_the_list_without_running_sessions() {
        let mut app = mux_app();
        app.view = View::List;

        for key in [
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE),
        ] {
            assert!(handle_list_prefix(&mut app, key));
        }

        assert_eq!(app.workspace_help, Some(WorkspaceHelp::Scratchpad));
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
    fn prefix_e_opens_the_attached_scratchpad() {
        let mut app = attached_mux_app("prefix-scratchpad-test");

        send_prefix_command(&mut app, 'e');

        assert!(app.scratchpad.is_some());
        assert_eq!(app.scratchpad_owner, Some(1));
        assert_eq!(app.workspace_focus, WorkspaceFocus::Scratchpad);
        app.scratchpad = None;
        let _ = app.mux.as_mut().unwrap().shutdown();
    }

    #[test]
    fn prefix_ctrl_h_e_opens_scratchpad_help() {
        let mut app = attached_mux_app("prefix-scratchpad-help-test");

        handle_attached_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
        );
        handle_attached_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL),
        );
        assert!(app.mux.as_ref().unwrap().help_pending);

        handle_attached_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE),
        );

        assert_eq!(app.workspace_help, Some(WorkspaceHelp::Scratchpad));
        assert!(!app.mux.as_ref().unwrap().help_pending);

        handle_attached_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        );
        assert_eq!(app.workspace_help, None);
        let _ = app.mux.as_mut().unwrap().shutdown();
    }

    #[test]
    fn prefix_t_opens_and_hides_the_attached_terminal() {
        let mut app = attached_mux_app("prefix-terminal-test");

        send_prefix_command(&mut app, 't');
        assert!(app.terminal.is_visible());
        assert_eq!(app.terminal_owner, Some(1));
        assert_eq!(app.workspace_focus, WorkspaceFocus::Terminal);

        send_prefix_command(&mut app, 't');
        assert!(!app.terminal.is_visible());
        assert_eq!(app.workspace_focus, WorkspaceFocus::Chat);
        app.terminal.shutdown();
        let _ = app.mux.as_mut().unwrap().shutdown();
    }

    #[test]
    fn attached_panels_are_hidden_behind_the_pane_list_and_restored_on_reattach() {
        let mut app = attached_mux_app("panel-reattach-test");
        send_prefix_command(&mut app, 'e');
        send_prefix_command(&mut app, 't');

        app.open_pane_list();

        assert_eq!(app.mode, crate::app::Mode::PaneList);
        assert!(!app.attached_scratchpad_visible());
        assert!(!app.attached_terminal_visible());
        assert!(!app.list_terminal_visible());
        assert!(app.scratchpad_open.contains(&1));
        assert!(app.terminal_open.contains(&1));

        app.view = View::Attached(1);
        sync_workspace_panels(&mut app);

        assert!(app.attached_scratchpad_visible());
        assert!(app.attached_terminal_visible());
        app.scratchpad = None;
        app.terminal.shutdown();
        let _ = app.mux.as_mut().unwrap().shutdown();
    }

    #[test]
    fn attached_panels_are_not_inherited_by_another_session() {
        let mut app = attached_mux_app("panel-owner-a");
        push_test_pane(&mut app, 2, "panel-owner-b");
        app.mux.as_mut().unwrap().select_index(0);
        send_prefix_command(&mut app, 'e');
        send_prefix_command(&mut app, 't');

        app.mux.as_mut().unwrap().select_index(1);
        app.view = View::Attached(2);
        sync_workspace_panels(&mut app);

        assert!(!app.attached_scratchpad_visible());
        assert!(!app.attached_terminal_visible());

        app.mux.as_mut().unwrap().select_index(0);
        app.view = View::Attached(1);
        sync_workspace_panels(&mut app);

        assert!(app.attached_scratchpad_visible());
        assert!(app.attached_terminal_visible());
        app.scratchpad = None;
        app.terminal.shutdown();
        let _ = app.mux.as_mut().unwrap().shutdown();
    }

    #[test]
    fn prefix_c_returns_focus_to_chat_without_hiding_panels() {
        let mut app = attached_mux_app("prefix-chat-test");
        send_prefix_command(&mut app, 'e');
        assert!(app.scratchpad.is_some());

        send_prefix_command(&mut app, 'c');

        assert_eq!(app.workspace_focus, WorkspaceFocus::Chat);
        assert!(app.scratchpad.is_some());
        app.scratchpad = None;
        let _ = app.mux.as_mut().unwrap().shutdown();
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
