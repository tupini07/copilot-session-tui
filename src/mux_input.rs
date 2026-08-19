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
            MouseEventKind::ScrollUp => scroll_github_at(app, mouse.column, mouse.row, -3),
            MouseEventKind::ScrollDown => scroll_github_at(app, mouse.column, mouse.row, 3),
            MouseEventKind::Down(MouseButton::Left) => {
                click_github_inspector(app, mouse.column, mouse.row)
            }
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
    let files_tab = app.github_inspector.as_ref().is_some_and(|inspector| {
        inspector.tab == crate::app::GithubTab::Files
            && inspector
                .ready_item()
                .is_some_and(|item| item.is_pull_request())
    });
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
            let in_diff = app
                .github_inspector
                .as_ref()
                .is_some_and(|inspector| inspector.files_pane == crate::app::FilesPane::Diff);
            if files_tab && in_diff {
                if let Some(inspector) = app.github_inspector.as_mut() {
                    inspector.files_pane = crate::app::FilesPane::Tree;
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
        KeyCode::Left if files_tab => collapse_or_ascend_github_tree(app),
        KeyCode::Right if files_tab => expand_or_enter_github_tree(app),
        KeyCode::Left => scroll_github_diff_horizontal(app, -4),
        KeyCode::Right => scroll_github_diff_horizontal(app, 4),
        KeyCode::Enter if files_tab => activate_github_tree_row(app),
        _ => {}
    }
}

/// Scroll whichever pane the pointer is over, so the wheel works without
/// changing focus first.
fn scroll_github_at(app: &mut App, column: u16, row: u16, amount: isize) {
    let target = app.github_inspector.as_ref().and_then(|inspector| {
        if inspector.tab != crate::app::GithubTab::Files {
            return None;
        }
        if contains(inspector.diff_area, column, row) {
            Some(crate::app::FilesPane::Diff)
        } else if contains(inspector.tree_area, column, row) {
            Some(crate::app::FilesPane::Tree)
        } else {
            None
        }
    });

    match target {
        Some(crate::app::FilesPane::Diff) => scroll_github_diff(app, amount),
        // The tree scrolls under the pointer without dragging the selection along.
        Some(crate::app::FilesPane::Tree) => scroll_github_tree_view(app, amount),
        None => scroll_github_inspector(app, amount),
    }
}

fn scroll_github_tree_view(app: &mut App, amount: isize) {
    let row_count = github_tree_rows(app).len();
    let Some(inspector) = app.github_inspector.as_mut() else {
        return;
    };
    let max_offset = row_count.saturating_sub(inspector.visible_tree_rows.max(1));
    let next = if amount < 0 {
        inspector.tree_offset.saturating_sub(amount.unsigned_abs())
    } else {
        inspector.tree_offset.saturating_add(amount as usize)
    };
    inspector.tree_offset = next.min(max_offset);
}

/// Clicking a pane focuses it, and clicking a tree row selects that row.
fn click_github_inspector(app: &mut App, column: u16, row: u16) {
    let Some(inspector) = app.github_inspector.as_ref() else {
        return;
    };
    if inspector.tab != crate::app::GithubTab::Files {
        return;
    }
    if contains(inspector.diff_area, column, row) {
        if let Some(inspector) = app.github_inspector.as_mut() {
            inspector.files_pane = crate::app::FilesPane::Diff;
        }
        return;
    }
    if !contains(inspector.tree_area, column, row) {
        return;
    }

    let clicked = inspector.tree_offset + (row - inspector.tree_area.y) as usize;
    if let Some(inspector) = app.github_inspector.as_mut() {
        inspector.files_pane = crate::app::FilesPane::Tree;
    }
    select_github_tree_row(app, clicked);
}

/// Rows of the changed-file tree as currently displayed.
fn github_tree_rows(app: &App) -> Vec<crate::ui::file_tree::TreeRow> {
    app.github_inspector
        .as_ref()
        .and_then(|inspector| {
            inspector.ready_item().map(|item| {
                crate::ui::file_tree::build_rows(item.files(), &inspector.collapsed_dirs)
            })
        })
        .unwrap_or_default()
}

/// Move the tree cursor, keeping the diff pane showing the selected file.
fn select_github_tree_row(app: &mut App, row: usize) {
    let rows = github_tree_rows(app);
    let Some(inspector) = app.github_inspector.as_mut() else {
        return;
    };
    if rows.is_empty() {
        return;
    }
    let row = row.min(rows.len() - 1);
    inspector.tree_selected = row;
    if let Some(index) = rows[row].file_index() {
        if inspector.selected_file != index {
            inspector.selected_file = index;
            inspector.diff_scroll = 0;
            inspector.diff_horizontal = 0;
        }
    }
}

fn collapse_or_ascend_github_tree(app: &mut App) {
    let in_diff = app
        .github_inspector
        .as_ref()
        .is_some_and(|inspector| inspector.files_pane == crate::app::FilesPane::Diff);
    if in_diff {
        scroll_github_diff_horizontal(app, -4);
        return;
    }

    let rows = github_tree_rows(app);
    let Some(inspector) = app.github_inspector.as_ref() else {
        return;
    };
    let selected = inspector.tree_selected;
    let Some(row) = rows.get(selected) else {
        return;
    };

    // An open directory folds; anything else steps out to its parent.
    if let crate::ui::file_tree::RowKind::Directory { path, expanded, .. } = &row.kind {
        if *expanded {
            let path = path.clone();
            if let Some(inspector) = app.github_inspector.as_mut() {
                inspector.collapsed_dirs.insert(path);
            }
            return;
        }
    }
    if let Some(parent) = crate::ui::file_tree::parent_row(&rows, selected) {
        select_github_tree_row(app, parent);
    }
}

fn expand_or_enter_github_tree(app: &mut App) {
    let in_diff = app
        .github_inspector
        .as_ref()
        .is_some_and(|inspector| inspector.files_pane == crate::app::FilesPane::Diff);
    if in_diff {
        scroll_github_diff_horizontal(app, 4);
        return;
    }

    let rows = github_tree_rows(app);
    let Some(inspector) = app.github_inspector.as_ref() else {
        return;
    };
    let selected = inspector.tree_selected;
    let Some(row) = rows.get(selected) else {
        return;
    };

    match &row.kind {
        crate::ui::file_tree::RowKind::Directory { path, expanded, .. } => {
            if *expanded {
                select_github_tree_row(app, selected + 1);
            } else {
                let path = path.clone();
                if let Some(inspector) = app.github_inspector.as_mut() {
                    inspector.collapsed_dirs.remove(&path);
                }
            }
        }
        crate::ui::file_tree::RowKind::File { .. } => {
            if let Some(inspector) = app.github_inspector.as_mut() {
                inspector.files_pane = crate::app::FilesPane::Diff;
            }
        }
    }
}

/// Enter folds a directory or moves focus into the diff.
fn activate_github_tree_row(app: &mut App) {
    let in_diff = app
        .github_inspector
        .as_ref()
        .is_some_and(|inspector| inspector.files_pane == crate::app::FilesPane::Diff);
    if in_diff {
        return;
    }

    let rows = github_tree_rows(app);
    let Some(inspector) = app.github_inspector.as_ref() else {
        return;
    };
    let Some(row) = rows.get(inspector.tree_selected) else {
        return;
    };
    match &row.kind {
        crate::ui::file_tree::RowKind::Directory { path, expanded, .. } => {
            let (path, expanded) = (path.clone(), *expanded);
            if let Some(inspector) = app.github_inspector.as_mut() {
                if expanded {
                    inspector.collapsed_dirs.insert(path);
                } else {
                    inspector.collapsed_dirs.remove(&path);
                }
            }
        }
        crate::ui::file_tree::RowKind::File { .. } => {
            if let Some(inspector) = app.github_inspector.as_mut() {
                inspector.files_pane = crate::app::FilesPane::Diff;
            }
        }
    }
}

fn scroll_github_inspector(app: &mut App, amount: isize) {
    let Some(inspector) = app.github_inspector.as_ref() else {
        return;
    };
    let files_tab = inspector.tab == crate::app::GithubTab::Files
        && inspector
            .ready_item()
            .is_some_and(|item| item.is_pull_request());

    if files_tab {
        if inspector.files_pane == crate::app::FilesPane::Diff {
            scroll_github_diff(app, amount);
        } else {
            let selected = inspector.tree_selected;
            let next = if amount < 0 {
                selected.saturating_sub(amount.unsigned_abs())
            } else {
                selected.saturating_add(amount as usize)
            };
            select_github_tree_row(app, next);
        }
        return;
    }

    if let Some(inspector) = app.github_inspector.as_mut() {
        inspector.scroll_active_by(amount);
    }
}

fn scroll_github_diff(app: &mut App, amount: isize) {
    let Some(inspector) = app.github_inspector.as_mut() else {
        return;
    };
    let next = if amount < 0 {
        inspector.diff_scroll.saturating_sub(amount.unsigned_abs())
    } else {
        inspector.diff_scroll.saturating_add(amount as usize)
    };
    inspector.diff_scroll = next.min(inspector.max_diff_scroll);
}

fn set_github_scroll_boundary(app: &mut App, end: bool) {
    let Some(inspector) = app.github_inspector.as_ref() else {
        return;
    };
    let files_tab = inspector.tab == crate::app::GithubTab::Files
        && inspector
            .ready_item()
            .is_some_and(|item| item.is_pull_request());

    if files_tab {
        if inspector.files_pane == crate::app::FilesPane::Diff {
            if let Some(inspector) = app.github_inspector.as_mut() {
                inspector.diff_scroll = if end { inspector.max_diff_scroll } else { 0 };
            }
        } else {
            let rows = github_tree_rows(app);
            if !rows.is_empty() {
                select_github_tree_row(app, if end { rows.len() - 1 } else { 0 });
            }
        }
        return;
    }

    if let Some(inspector) = app.github_inspector.as_mut() {
        inspector.set_active_scroll(if end { inspector.max_scroll } else { 0 });
    }
}

fn scroll_github_diff_horizontal(app: &mut App, amount: isize) {
    let Some(inspector) = app.github_inspector.as_mut() else {
        return;
    };
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

    /// A pull request whose tree is `src/{ui/pane.rs, lib.rs}`, so there is a
    /// nested directory to fold and two files to move between.
    fn pull_request_app() -> App {
        use crate::github::{Author, ChangedFile, ItemCommon, Label, PullRequest, RepositoryRef};

        let changed = |path: &str| ChangedFile {
            path: path.to_string(),
            status: "modified".to_string(),
            additions: 1,
            deletions: 1,
            changes: 2,
            patch: Some(format!("@@ -1 +1 @@\n-old {path}\n+new {path}")),
        };
        let item = crate::github::GithubItem::PullRequest(PullRequest {
            common: ItemCommon {
                repository: RepositoryRef {
                    host: "github.com".to_string(),
                    owner: "octo".to_string(),
                    name: "widgets".to_string(),
                },
                number: 7,
                title: "Tree navigation".to_string(),
                state: "open".to_string(),
                author: Author {
                    login: "monalisa".to_string(),
                },
                labels: Vec::<Label>::new(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-02T00:00:00Z".to_string(),
                url: "https://github.com/octo/widgets/pull/7".to_string(),
                body: String::new(),
            },
            draft: false,
            merged: false,
            mergeable_state: None,
            base_ref: "main".to_string(),
            head_ref: "feature".to_string(),
            additions: 2,
            deletions: 2,
            changed_files: 2,
            discussion: Vec::new(),
            files: vec![changed("src/ui/pane.rs"), changed("src/lib.rs")],
        });

        let mut app = App::new(Vec::new(), UserConfig::default());
        let mut inspector = crate::app::GithubInspector::number_prompt();
        inspector.screen = crate::app::GithubInspectorScreen::Ready(item);
        inspector.tab = crate::app::GithubTab::Files;
        inspector.select_first_tree_file();
        app.github_inspector = Some(inspector);
        app
    }

    fn press(app: &mut App, code: KeyCode) {
        handle_github_inspector_event(app, Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    fn tree_labels(app: &App) -> Vec<String> {
        github_tree_rows(app)
            .iter()
            .map(|row| row.label.clone())
            .collect()
    }

    #[test]
    fn the_files_tree_opens_on_the_first_file_not_a_directory() {
        let app = pull_request_app();
        let inspector = app.github_inspector.as_ref().unwrap();

        // rows: [src, ui, pane.rs, lib.rs] — the cursor skips to `pane.rs`.
        assert_eq!(tree_labels(&app), vec!["src", "ui", "pane.rs", "lib.rs"]);
        assert_eq!(inspector.tree_selected, 2);
        assert_eq!(inspector.selected_file, 0);
    }

    #[test]
    fn moving_through_the_tree_updates_the_diff_without_pressing_enter() {
        let mut app = pull_request_app();

        press(&mut app, KeyCode::Down);

        let inspector = app.github_inspector.as_ref().unwrap();
        assert_eq!(inspector.tree_selected, 3);
        assert_eq!(inspector.selected_file, 1, "the diff follows the cursor");
        assert_eq!(inspector.files_pane, crate::app::FilesPane::Tree);
    }

    #[test]
    fn left_folds_a_directory_and_right_unfolds_it() {
        let mut app = pull_request_app();

        // From `pane.rs`, Left steps out to `ui`, then Left folds it.
        press(&mut app, KeyCode::Left);
        assert_eq!(app.github_inspector.as_ref().unwrap().tree_selected, 1);
        press(&mut app, KeyCode::Left);
        assert_eq!(tree_labels(&app), vec!["src", "ui", "lib.rs"]);

        press(&mut app, KeyCode::Right);
        assert_eq!(tree_labels(&app), vec!["src", "ui", "pane.rs", "lib.rs"]);
    }

    #[test]
    fn enter_moves_focus_to_the_diff_and_esc_brings_it_back() {
        let mut app = pull_request_app();

        press(&mut app, KeyCode::Enter);
        assert_eq!(
            app.github_inspector.as_ref().unwrap().files_pane,
            crate::app::FilesPane::Diff
        );

        // Arrows now scroll the patch instead of moving the tree cursor.
        press(&mut app, KeyCode::Down);
        let inspector = app.github_inspector.as_ref().unwrap();
        assert_eq!(inspector.tree_selected, 2, "the tree cursor stays put");

        press(&mut app, KeyCode::Esc);
        assert_eq!(
            app.github_inspector.as_ref().unwrap().files_pane,
            crate::app::FilesPane::Tree,
            "Esc returns to the tree before closing the inspector"
        );

        press(&mut app, KeyCode::Esc);
        assert!(app.github_inspector.is_none());
    }

    #[test]
    fn enter_on_a_directory_folds_it_instead_of_focusing_the_diff() {
        let mut app = pull_request_app();
        app.github_inspector.as_mut().unwrap().tree_selected = 0;

        press(&mut app, KeyCode::Enter);

        assert_eq!(tree_labels(&app), vec!["src"]);
        assert_eq!(
            app.github_inspector.as_ref().unwrap().files_pane,
            crate::app::FilesPane::Tree
        );
    }

    #[test]
    fn the_wheel_scrolls_whichever_pane_it_is_over() {
        let mut app = pull_request_app();
        {
            let inspector = app.github_inspector.as_mut().unwrap();
            inspector.tree_area = Rect::new(0, 4, 30, 10);
            inspector.diff_area = Rect::new(31, 4, 60, 10);
            inspector.visible_tree_rows = 2;
            // Normally set while drawing; the wheel clamps against it.
            inspector.max_diff_scroll = 10;
        }

        // Over the diff: the patch scrolls and the tree cursor is untouched.
        scroll_github_at(&mut app, 40, 6, 3);
        let inspector = app.github_inspector.as_ref().unwrap();
        assert_eq!(inspector.diff_scroll, 3);
        assert_eq!(inspector.tree_selected, 2);

        // Over the tree: the view scrolls without dragging the selection.
        scroll_github_at(&mut app, 5, 6, 3);
        let inspector = app.github_inspector.as_ref().unwrap();
        assert_eq!(inspector.tree_offset, 2, "clamped to the last full page");
        assert_eq!(inspector.tree_selected, 2);
    }

    #[test]
    fn clicking_a_tree_row_selects_it_and_focuses_the_tree() {
        let mut app = pull_request_app();
        {
            let inspector = app.github_inspector.as_mut().unwrap();
            inspector.tree_area = Rect::new(0, 4, 30, 10);
            inspector.diff_area = Rect::new(31, 4, 60, 10);
            inspector.files_pane = crate::app::FilesPane::Diff;
        }

        click_github_inspector(&mut app, 5, 7);

        let inspector = app.github_inspector.as_ref().unwrap();
        assert_eq!(inspector.files_pane, crate::app::FilesPane::Tree);
        assert_eq!(inspector.tree_selected, 3, "row 3 of the tree");
        assert_eq!(inspector.selected_file, 1);
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
