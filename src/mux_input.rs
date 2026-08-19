use crate::app::{App, View, WorkspaceFocus, WorkspaceHelp};
use crate::mux::{
    resolve_github_command, resolve_help_command, resolve_prefix_command, GithubCommand,
    HelpCommand, MuxEvent, PrefixCommand, PrefixState,
};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::layout::Rect;

/// Route a terminal event while a pane is focused.
///
/// Everything except the prefix key is forwarded to the child, because Copilot wants
/// nearly every keystroke for itself.
pub fn handle_attached_event(app: &mut App, event: Event) {
    if app.github_inspector.is_some() {
        handle_github_inspector_event(app, event);
        return;
    }

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
            .is_some_and(|mux| mux.prefix_state != PrefixState::Idle || mux.prefix.matches(key));
        if is_prefix {
            handle_attached_key(app, *key);
            return;
        }
    }

    if let Event::Mouse(mouse) = &event {
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            let reference = app
                .mux
                .as_ref()
                .and_then(|mux| mux.focused_pane())
                .and_then(|pane| pane.github_reference_at(mouse.column, mouse.row));
            if let Some(number) = reference {
                app.inspect_github_item(number);
                return;
            }
        }
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
    let Some((prefix_state, prefix)) = app.mux.as_ref().map(|mux| (mux.prefix_state, mux.prefix))
    else {
        return;
    };

    if prefix_state == PrefixState::Help {
        if let Some(mux) = app.mux.as_mut() {
            mux.prefix_state = PrefixState::Idle;
        }
        if matches!(resolve_help_command(&key), Some(HelpCommand::Scratchpad)) {
            app.workspace_help = Some(WorkspaceHelp::Scratchpad);
        }
        return;
    }

    if prefix_state == PrefixState::Github {
        if let Some(mux) = app.mux.as_mut() {
            mux.prefix_state = PrefixState::Idle;
        }
        if matches!(resolve_github_command(&key), Some(GithubCommand::Inspect)) {
            app.open_github_inspector();
        }
        return;
    }

    if prefix_state == PrefixState::Root {
        let command = resolve_prefix_command(&key, &prefix);
        if let Some(mux) = app.mux.as_mut() {
            mux.prefix_state = PrefixState::Idle;
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
                    mux.prefix_state = PrefixState::Help;
                }
            }
            Some(PrefixCommand::Github) => {
                if let Some(mux) = app.mux.as_mut() {
                    mux.prefix_state = PrefixState::Github;
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

    if prefix.matches(&key) {
        if let Some(mux) = app.mux.as_mut() {
            mux.prefix_state = PrefixState::Root;
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

fn handle_github_inspector_event(app: &mut App, event: Event) {
    match event {
        Event::Paste(text) => {
            let Some(inspector) = app.github_inspector.as_mut() else {
                return;
            };
            if matches!(
                inspector.screen,
                crate::app::GithubInspectorScreen::NumberPrompt
            ) {
                inspector
                    .input
                    .extend(text.chars().filter(char::is_ascii_digit));
                inspector.prompt_error = None;
            }
        }
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollUp => scroll_github_inspector(app, -3),
            MouseEventKind::ScrollDown => scroll_github_inspector(app, 3),
            _ => {}
        },
        Event::Key(key) if key.kind == KeyEventKind::Press => handle_github_inspector_key(app, key),
        _ => {}
    }
}

fn handle_github_inspector_key(app: &mut App, key: KeyEvent) {
    let Some(inspector) = app.github_inspector.as_ref() else {
        return;
    };
    match inspector.screen {
        crate::app::GithubInspectorScreen::NumberPrompt => match key.code {
            KeyCode::Esc => app.close_github_inspector(),
            KeyCode::Enter => app.submit_github_number(),
            KeyCode::Backspace => {
                if let Some(inspector) = app.github_inspector.as_mut() {
                    inspector.input.pop();
                    inspector.prompt_error = None;
                }
            }
            KeyCode::Char(character)
                if character.is_ascii_digit()
                    && !key.modifiers.intersects(
                        crossterm::event::KeyModifiers::CONTROL
                            | crossterm::event::KeyModifiers::ALT,
                    ) =>
            {
                if let Some(inspector) = app.github_inspector.as_mut() {
                    inspector.input.push(character);
                    inspector.prompt_error = None;
                }
            }
            _ => {}
        },
        crate::app::GithubInspectorScreen::Loading => {
            if key.code == KeyCode::Esc {
                app.close_github_inspector();
            }
        }
        crate::app::GithubInspectorScreen::Error(_) => match key.code {
            KeyCode::Esc => app.close_github_inspector(),
            KeyCode::Char('r') => app.retry_github_request(),
            _ => {}
        },
        crate::app::GithubInspectorScreen::Ready(_) => handle_ready_github_key(app, key),
    }
}

fn handle_ready_github_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Tab
            if key
                .modifiers
                .contains(crossterm::event::KeyModifiers::SHIFT) =>
        {
            if let Some(inspector) = app.github_inspector.as_mut() {
                inspector.cycle_tab(false);
            }
        }
        KeyCode::Tab => {
            if let Some(inspector) = app.github_inspector.as_mut() {
                inspector.cycle_tab(true);
            }
        }
        KeyCode::BackTab => {
            if let Some(inspector) = app.github_inspector.as_mut() {
                inspector.cycle_tab(false);
            }
        }
        KeyCode::Esc => {
            let diff_open = app
                .github_inspector
                .as_ref()
                .is_some_and(|inspector| inspector.diff_open);
            if diff_open {
                if let Some(inspector) = app.github_inspector.as_mut() {
                    inspector.diff_open = false;
                    inspector.diff_scroll = 0;
                    inspector.diff_horizontal = 0;
                }
            } else {
                app.close_github_inspector();
            }
        }
        KeyCode::Up => scroll_github_inspector(app, -1),
        KeyCode::Down => scroll_github_inspector(app, 1),
        KeyCode::PageUp => scroll_github_inspector(app, -10),
        KeyCode::PageDown => scroll_github_inspector(app, 10),
        KeyCode::Home => set_github_scroll_boundary(app, false),
        KeyCode::End => set_github_scroll_boundary(app, true),
        KeyCode::Left => scroll_github_diff_horizontal(app, -4),
        KeyCode::Right => scroll_github_diff_horizontal(app, 4),
        KeyCode::Enter => {
            let can_open = app.github_inspector.as_ref().is_some_and(|inspector| {
                inspector.tab == crate::app::GithubTab::Files
                    && !inspector.diff_open
                    && inspector
                        .ready_item()
                        .is_some_and(|item| !item.files().is_empty())
            });
            if can_open {
                if let Some(inspector) = app.github_inspector.as_mut() {
                    inspector.diff_open = true;
                    inspector.diff_scroll = 0;
                    inspector.diff_horizontal = 0;
                }
            }
        }
        _ => {}
    }
}

fn scroll_github_inspector(app: &mut App, amount: isize) {
    let Some(inspector) = app.github_inspector.as_mut() else {
        return;
    };
    if inspector.diff_open {
        let next = if amount < 0 {
            inspector.diff_scroll.saturating_sub(amount.unsigned_abs())
        } else {
            inspector.diff_scroll.saturating_add(amount as usize)
        };
        inspector.diff_scroll = next.min(inspector.max_diff_scroll);
        return;
    }

    if inspector.tab == crate::app::GithubTab::Files {
        let count = inspector
            .ready_item()
            .map(|item| item.files().len())
            .unwrap_or(0);
        if count == 0 {
            return;
        }
        let next = if amount < 0 {
            inspector
                .selected_file
                .saturating_sub(amount.unsigned_abs())
        } else {
            inspector.selected_file.saturating_add(amount as usize)
        };
        inspector.selected_file = next.min(count - 1);
        if inspector.selected_file < inspector.file_list_offset {
            inspector.file_list_offset = inspector.selected_file;
        } else if inspector.visible_files > 0
            && inspector.selected_file >= inspector.file_list_offset + inspector.visible_files
        {
            inspector.file_list_offset =
                inspector.selected_file - inspector.visible_files.saturating_sub(1);
        }
        return;
    }

    inspector.scroll_active_by(amount);
}

fn set_github_scroll_boundary(app: &mut App, end: bool) {
    let Some(inspector) = app.github_inspector.as_mut() else {
        return;
    };
    if inspector.diff_open {
        inspector.diff_scroll = if end { inspector.max_diff_scroll } else { 0 };
    } else if inspector.tab == crate::app::GithubTab::Files {
        let count = inspector
            .ready_item()
            .map(|item| item.files().len())
            .unwrap_or(0);
        if count > 0 {
            inspector.selected_file = if end { count - 1 } else { 0 };
            inspector.file_list_offset = if end {
                count.saturating_sub(inspector.visible_files.max(1))
            } else {
                0
            };
        }
    } else {
        inspector.set_active_scroll(if end { inspector.max_scroll } else { 0 });
    }
}

fn scroll_github_diff_horizontal(app: &mut App, amount: isize) {
    let Some(inspector) = app.github_inspector.as_mut() else {
        return;
    };
    if !inspector.diff_open {
        return;
    }
    let next = if amount < 0 {
        inspector
            .diff_horizontal
            .saturating_sub(amount.unsigned_abs())
    } else {
        inspector.diff_horizontal.saturating_add(amount as usize)
    };
    inspector.diff_horizontal = next.min(inspector.max_diff_horizontal);
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
    let Some((state, prefix)) = app.mux.as_ref().map(|mux| (mux.prefix_state, mux.prefix)) else {
        return false;
    };

    if state == PrefixState::Help {
        if let Some(mux) = app.mux.as_mut() {
            mux.prefix_state = PrefixState::Idle;
        }
        if matches!(resolve_help_command(&key), Some(HelpCommand::Scratchpad)) {
            app.workspace_help = Some(WorkspaceHelp::Scratchpad);
        }
        return true;
    }

    if state == PrefixState::Github {
        if let Some(mux) = app.mux.as_mut() {
            mux.prefix_state = PrefixState::Idle;
        }
        if matches!(resolve_github_command(&key), Some(GithubCommand::Inspect)) {
            app.status_message =
                Some("Attach to a session before inspecting a GitHub item".to_string());
        }
        return true;
    }

    if state == PrefixState::Idle {
        if prefix.matches(&key) {
            if let Some(mux) = app.mux.as_mut() {
                mux.prefix_state = PrefixState::Root;
            }
            return true;
        }
        return false;
    }

    if let Some(mux) = app.mux.as_mut() {
        mux.prefix_state = PrefixState::Idle;
    }
    let command = resolve_prefix_command(&key, &prefix);
    if matches!(command, Some(PrefixCommand::Help)) {
        if let Some(mux) = app.mux.as_mut() {
            mux.prefix_state = PrefixState::Help;
        }
        return true;
    }
    if matches!(command, Some(PrefixCommand::Github)) {
        if let Some(mux) = app.mux.as_mut() {
            mux.prefix_state = PrefixState::Github;
        }
        return true;
    }
    if app.mux.as_ref().is_none_or(|mux| mux.panes.is_empty()) {
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
        Some(PrefixCommand::Github) => unreachable!("handled before pane availability"),
        Some(PrefixCommand::NextPane) => {
            if let Some(mux) = app.mux.as_mut() {
                mux.cycle(true);
            }
            attach_focused(app);
        }
        Some(PrefixCommand::PreviousPane) => {
            if let Some(mux) = app.mux.as_mut() {
                mux.cycle(false);
            }
            attach_focused(app);
        }
        Some(PrefixCommand::SelectIndex(index)) => {
            // Panes are labelled from 1 in the UI.
            if let Some(mux) = app.mux.as_mut() {
                mux.select_index(index.saturating_sub(1));
            }
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
            app.host_sequences
                .push(crate::host_terminal::CLEAR_PROGRESS.to_vec());
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

        assert_eq!(app.mux.as_ref().unwrap().prefix_state, PrefixState::Root);
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
        assert_eq!(app.mux.as_ref().unwrap().prefix_state, PrefixState::Idle);
    }

    #[test]
    fn github_namespace_opens_the_number_prompt_while_attached() {
        let mut app = attached_mux_app("github-inspector-test");

        for key in [
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE),
        ] {
            handle_attached_key(&mut app, key);
        }

        assert!(matches!(
            app.github_inspector
                .as_ref()
                .map(|inspector| &inspector.screen),
            Some(crate::app::GithubInspectorScreen::NumberPrompt)
        ));
        let _ = app.mux.as_mut().unwrap().shutdown();
    }

    #[test]
    fn github_inspection_from_the_list_explains_that_attachment_is_required() {
        let mut app = mux_app();
        app.view = View::List;

        for key in [
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE),
        ] {
            assert!(handle_list_prefix(&mut app, key));
        }

        assert!(app.github_inspector.is_none());
        assert_eq!(
            app.status_message.as_deref(),
            Some("Attach to a session before inspecting a GitHub item")
        );
    }

    #[test]
    fn github_number_prompt_accepts_only_digits_and_backspace() {
        let mut app = attached_mux_app("github-number-test");
        app.open_github_inspector();

        for key in [
            KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        ] {
            handle_github_inspector_event(&mut app, Event::Key(key));
        }

        assert_eq!(
            app.github_inspector
                .as_ref()
                .map(|inspector| inspector.input.as_str()),
            Some("1")
        );
        handle_github_inspector_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        );
        assert!(app.github_inspector.is_none());
        let _ = app.mux.as_mut().unwrap().shutdown();
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
        assert_eq!(app.mux.as_ref().unwrap().prefix_state, PrefixState::Root);
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
        assert_eq!(app.mux.as_ref().unwrap().prefix_state, PrefixState::Idle);
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
            app.mux.as_ref().unwrap().prefix_state == PrefixState::Idle,
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
        assert_eq!(app.mux.as_ref().unwrap().prefix_state, PrefixState::Help);

        handle_attached_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE),
        );

        assert_eq!(app.workspace_help, Some(WorkspaceHelp::Scratchpad));
        assert_eq!(app.mux.as_ref().unwrap().prefix_state, PrefixState::Idle);

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
        assert_eq!(
            app.host_sequences,
            vec![crate::host_terminal::CLEAR_PROGRESS.to_vec()]
        );
    }
}
