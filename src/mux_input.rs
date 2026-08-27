use crate::app::{App, View, WorkspaceFocus, WorkspaceHelp};
use crate::input::{handle_quit_confirm, request_quit};
use crate::mux::pane::PaneNotification;
use crate::mux::{
    resolve_github_command, resolve_help_command, resolve_prefix_command, GithubCommand,
    HelpCommand, MuxEvent, PrefixCommand, PrefixState,
};
use crate::notifications::NotificationKind;
use crate::snippets::{SnippetEditorField, SnippetScope, SnippetScreen};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::layout::Rect;

/// Route a terminal event while a pane is focused.
///
/// Everything except the prefix key is forwarded to the child, because Copilot wants
/// nearly every keystroke for itself.
pub fn handle_attached_event(app: &mut App, event: Event) {
    let attended = matches!(
        &event,
        Event::Key(KeyEvent {
            kind: KeyEventKind::Press,
            ..
        }) | Event::Paste(_)
            | Event::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(_),
                ..
            })
    );
    if attended {
        app.terminal_focused = true;
        app.acknowledge_focused_pane();
    }

    if app.confirm_quit {
        if let Event::Key(key) = &event {
            if key.kind == KeyEventKind::Press {
                handle_quit_confirm(app, key.code);
                // The attached status bar has nowhere to show the shared "Quit
                // cancelled" notice, and leaving it set would surface it stale on a
                // later detach.
                if !app.should_quit {
                    app.status_message = None;
                }
            }
        }
        return;
    }

    if app.github_inspector.is_some() {
        handle_github_inspector_event(app, event);
        return;
    }

    if app.snippet_modal.is_some() {
        handle_snippet_event(app, event);
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
        app.clear_update_notice();
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

fn handle_snippet_event(app: &mut App, event: Event) {
    let screen = match app.snippet_modal.as_ref() {
        Some(modal) => modal.screen,
        None => return,
    };
    match (screen, event) {
        (SnippetScreen::List, Event::Key(key)) if key.kind == KeyEventKind::Press => {
            handle_snippet_list_key(app, key.code);
        }
        (SnippetScreen::Editor, Event::Key(key)) if key.kind == KeyEventKind::Press => {
            handle_snippet_editor_key(app, key);
        }
        (SnippetScreen::Editor, Event::Paste(text)) => {
            let Some(modal) = app.snippet_modal.as_mut() else {
                return;
            };
            match modal.editor_field {
                SnippetEditorField::Name => {
                    modal.insert_editor_text(&text.replace(['\r', '\n'], " "));
                }
                SnippetEditorField::Prompt => modal.insert_editor_text(&text),
                SnippetEditorField::Scope => {}
            }
        }
        (SnippetScreen::ConfirmDelete, Event::Key(key)) if key.kind == KeyEventKind::Press => {
            match key.code {
                KeyCode::Char('y') | KeyCode::Enter => delete_selected_snippet(app),
                KeyCode::Char('n') | KeyCode::Esc => {
                    if let Some(modal) = app.snippet_modal.as_mut() {
                        modal.cancel_subscreen();
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

fn handle_snippet_list_key(app: &mut App, key: KeyCode) {
    match key {
        KeyCode::Up => {
            if let Some(modal) = app.snippet_modal.as_mut() {
                modal.select_previous();
            }
        }
        KeyCode::Down => {
            if let Some(modal) = app.snippet_modal.as_mut() {
                modal.select_next();
            }
        }
        KeyCode::Char('a') => {
            if let Some(modal) = app.snippet_modal.as_mut() {
                modal.begin_add();
            }
        }
        KeyCode::Char('e') => {
            if let Some(modal) = app.snippet_modal.as_mut() {
                modal.begin_edit();
            }
        }
        KeyCode::Char('d') => {
            if let Some(modal) = app.snippet_modal.as_mut() {
                modal.begin_delete();
            }
        }
        KeyCode::Enter => use_selected_snippet(app),
        KeyCode::Char('q') | KeyCode::Esc => app.snippet_modal = None,
        _ => {}
    }
}

fn handle_snippet_editor_key(app: &mut App, key: KeyEvent) {
    let control = key
        .modifiers
        .contains(crossterm::event::KeyModifiers::CONTROL);
    if control && matches!(key.code, KeyCode::Char('s')) {
        save_snippet_editor(app);
        return;
    }
    if control && matches!(key.code, KeyCode::Char('g')) {
        toggle_snippet_scope(app);
        return;
    }

    let Some(modal) = app.snippet_modal.as_mut() else {
        return;
    };
    match key.code {
        KeyCode::Esc => modal.cancel_subscreen(),
        KeyCode::Tab => modal.editor_field = modal.editor_field.next(true),
        KeyCode::BackTab => modal.editor_field = modal.editor_field.next(false),
        KeyCode::Backspace => modal.backspace_editor(),
        KeyCode::Delete => modal.delete_editor(),
        KeyCode::Left => modal.move_editor_cursor(-1),
        KeyCode::Right => modal.move_editor_cursor(1),
        KeyCode::Up => modal.move_prompt_cursor_vertical(false),
        KeyCode::Down => modal.move_prompt_cursor_vertical(true),
        KeyCode::Home => modal.move_editor_line_boundary(false),
        KeyCode::End => modal.move_editor_line_boundary(true),
        KeyCode::Enter => match modal.editor_field {
            SnippetEditorField::Name => modal.editor_field = SnippetEditorField::Scope,
            SnippetEditorField::Prompt => modal.insert_editor_text("\n"),
            SnippetEditorField::Scope => toggle_snippet_scope(app),
        },
        KeyCode::Char(' ') if modal.editor_field == SnippetEditorField::Scope => {
            toggle_snippet_scope(app);
        }
        KeyCode::Char(character)
            if !key.modifiers.intersects(
                crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::ALT,
            ) =>
        {
            match modal.editor_field {
                SnippetEditorField::Name | SnippetEditorField::Prompt => {
                    modal.insert_editor_text(&character.to_string());
                }
                SnippetEditorField::Scope => {}
            }
        }
        _ => {}
    }
}

fn toggle_snippet_scope(app: &mut App) {
    let Some(modal) = app.snippet_modal.as_mut() else {
        return;
    };
    modal.error = None;
    modal.editor_scope = match modal.editor_scope {
        SnippetScope::Global if modal.project_root.is_some() => SnippetScope::Project,
        SnippetScope::Global => {
            modal.error = Some("No Git project detected for this session".to_string());
            SnippetScope::Global
        }
        SnippetScope::Project => SnippetScope::Global,
    };
}

fn save_snippet_editor(app: &mut App) {
    let Some(modal) = app.snippet_modal.as_ref() else {
        return;
    };
    let name = modal.editor_name.trim().to_string();
    let prompt = modal
        .editor_prompt
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    if name.is_empty() || prompt.trim().is_empty() {
        if let Some(modal) = app.snippet_modal.as_mut() {
            modal.error = Some("Name and prompt are both required".to_string());
        }
        return;
    }
    if name.chars().any(char::is_control)
        || prompt
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
    {
        if let Some(modal) = app.snippet_modal.as_mut() {
            modal.error = Some("Snippets cannot contain terminal control characters".to_string());
        }
        return;
    }
    if modal.editor_scope == SnippetScope::Project && modal.project_root.is_none() {
        if let Some(modal) = app.snippet_modal.as_mut() {
            modal.error = Some("No Git project detected for project scope".to_string());
        }
        return;
    }

    let editing = modal.editing;
    let target_scope = modal.editor_scope;
    let project_root = modal.project_root.clone();
    let original_global = modal.original_global.clone();
    let original_project = modal.original_project.clone();
    let mut global = modal.global.clone();
    let mut project = modal.project.clone();
    let snippet = crate::config::PromptSnippet { name, prompt };
    let selected = match editing {
        Some((old_scope, index)) if old_scope == target_scope => match target_scope {
            SnippetScope::Global => {
                global[index] = snippet;
                index
            }
            SnippetScope::Project => {
                project[index] = snippet;
                global.len() + index
            }
        },
        Some((old_scope, index)) => {
            match old_scope {
                SnippetScope::Global => {
                    global.remove(index);
                }
                SnippetScope::Project => {
                    project.remove(index);
                }
            }
            match target_scope {
                SnippetScope::Global => {
                    global.push(snippet);
                    global.len() - 1
                }
                SnippetScope::Project => {
                    project.push(snippet);
                    global.len() + project.len() - 1
                }
            }
        }
        None => match target_scope {
            SnippetScope::Global => {
                global.push(snippet);
                global.len() - 1
            }
            SnippetScope::Project => {
                project.push(snippet);
                global.len() + project.len() - 1
            }
        },
    };

    let (global_dirty, project_dirty) = match editing {
        Some((old_scope, _)) if old_scope != target_scope => (true, true),
        _ => match target_scope {
            SnippetScope::Global => (true, false),
            SnippetScope::Project => (false, true),
        },
    };
    let update = crate::snippets::SnippetUpdate {
        global: global.clone(),
        project: project.clone(),
        original_global,
        original_project,
        project_root,
        global_dirty,
        project_dirty,
    };
    match app.persist_snippets(&update) {
        Ok(()) => {
            let modal = app.snippet_modal.as_mut().expect("modal remains open");
            modal.global = global;
            modal.project = project;
            modal.original_global = modal.global.clone();
            modal.original_project = modal.project.clone();
            modal.selected = selected;
            modal.cancel_subscreen();
        }
        Err(error) => {
            if let Some(modal) = app.snippet_modal.as_mut() {
                modal.error = Some(format!("Cannot save snippet: {error}"));
            }
        }
    }
}

fn delete_selected_snippet(app: &mut App) {
    let Some(modal) = app.snippet_modal.as_ref() else {
        return;
    };
    let Some((scope, index, _)) = modal.selected_entry() else {
        return;
    };
    let project_root = modal.project_root.clone();
    let original_global = modal.original_global.clone();
    let original_project = modal.original_project.clone();
    let mut global = modal.global.clone();
    let mut project = modal.project.clone();
    match scope {
        SnippetScope::Global => {
            global.remove(index);
        }
        SnippetScope::Project => {
            project.remove(index);
        }
    }
    let update = crate::snippets::SnippetUpdate {
        global: global.clone(),
        project: project.clone(),
        original_global,
        original_project,
        project_root,
        global_dirty: scope == SnippetScope::Global,
        project_dirty: scope == SnippetScope::Project,
    };
    match app.persist_snippets(&update) {
        Ok(()) => {
            let modal = app.snippet_modal.as_mut().expect("modal remains open");
            modal.global = global;
            modal.project = project;
            modal.original_global = modal.global.clone();
            modal.original_project = modal.project.clone();
            modal.selected = modal.selected.min(modal.len().saturating_sub(1));
            modal.cancel_subscreen();
        }
        Err(error) => {
            if let Some(modal) = app.snippet_modal.as_mut() {
                modal.screen = SnippetScreen::List;
                modal.error = Some(format!("Cannot delete snippet: {error}"));
            }
        }
    }
}

fn use_selected_snippet(app: &mut App) {
    let prompt = app
        .snippet_modal
        .as_ref()
        .and_then(|modal| modal.selected_entry())
        .map(|(_, _, snippet)| snippet.prompt.clone());
    let Some(prompt) = prompt else {
        return;
    };
    if let Some(pane) = app.mux.as_mut().and_then(|mux| mux.focused_pane_mut()) {
        if let Err(error) = pane.send_prompt_snippet(&prompt) {
            if let Some(modal) = app.snippet_modal.as_mut() {
                modal.error = Some(format!("Cannot paste snippet: {error}"));
            }
            return;
        }
    }
    app.snippet_modal = None;
    focus_chat(app);
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
            Some(PrefixCommand::Quit) => {
                app.request_quit_from_pane();
                return;
            }
            Some(PrefixCommand::PaneList) => {
                app.open_pane_list();
                return;
            }
            Some(PrefixCommand::Chat) => focus_chat(app),
            Some(PrefixCommand::Scratchpad) => toggle_attached_scratchpad(app),
            Some(PrefixCommand::Terminal) => toggle_attached_terminal(app),
            Some(PrefixCommand::Snippets) => app.open_snippets(),
            Some(PrefixCommand::Update) => app.request_update(),
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
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
                app.close_github_inspector();
            }
        }
        crate::app::GithubInspectorScreen::Error(_) => match key.code {
            KeyCode::Esc | KeyCode::Char('q') => app.close_github_inspector(),
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
        // `q` leaves the inspector outright, wherever you are in it; Esc only
        // steps back out of the diff.
        KeyCode::Char('q') => app.close_github_inspector(),
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

fn sync_outer_progress(app: &mut App) {
    let state = app
        .mux
        .as_ref()
        .and_then(|mux| mux.focused_pane())
        .map(|pane| pane.effective_progress_state())
        .unwrap_or(crate::host_terminal::ProgressState::Clear);
    app.host_sequences
        .push(crate::host_terminal::progress_sequence_for_state(state));
}

pub fn sync_workspace_panels(app: &mut App) {
    app.acknowledge_focused_pane();
    sync_outer_progress(app);
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
    Some((
        pane.id,
        pane.session_id.clone(),
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
    // Quitting is meaningful with or without panes, so it is resolved before the
    // "nothing is running" guard below.
    if matches!(command, Some(PrefixCommand::Quit)) {
        request_quit(app);
        return true;
    }
    if matches!(command, Some(PrefixCommand::Update)) {
        app.request_update();
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
        Some(PrefixCommand::Snippets) => {
            attach_focused(app);
            app.open_snippets();
        }
        Some(PrefixCommand::Update) => unreachable!("handled before pane availability"),
        Some(PrefixCommand::Help) => unreachable!("handled before pane availability"),
        Some(PrefixCommand::Github) => unreachable!("handled before pane availability"),
        Some(PrefixCommand::Quit) => unreachable!("handled before pane availability"),
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
        MuxEvent::Output(id, signals) => {
            let attended = app.terminal_focused
                && matches!(app.view, View::Attached(focused) if focused == id);
            let (
                outcome,
                notifications,
                attention_changed,
                title_changed,
                cycle_started,
                title,
                session_id,
            ) = app
                .mux
                .as_mut()
                .and_then(|mux| mux.pane_mut(id))
                .map(|pane| {
                    let before = pane.needs_attention();
                    let title_before = pane.title.clone();
                    let cycle_started = signals_start_cycle(pane.is_working(), &signals.events);
                    let (outcome, notifications) = pane.apply_signals(signals, attended);
                    (
                        outcome,
                        notifications,
                        before != pane.needs_attention(),
                        title_before != pane.title,
                        cycle_started,
                        pane.title.clone(),
                        pane.session_id.clone(),
                    )
                })
                .unwrap_or_default();
            if cycle_started {
                app.begin_notification_cycle(&session_id);
            }
            let notification_changed = !notifications.is_empty();
            for notification in notifications {
                let kind = match notification {
                    PaneNotification::Ready => NotificationKind::Ready,
                    PaneNotification::Question => NotificationKind::Question,
                    PaneNotification::PlanApproval => NotificationKind::PlanApproval,
                    PaneNotification::Error => NotificationKind::Error,
                };
                app.enqueue_notification(kind, title.clone(), Some(&session_id));
            }
            if outcome.bell {
                if let Some(title) = app
                    .mux
                    .as_ref()
                    .and_then(|mux| mux.pane(id))
                    .map(|pane| pane.title.clone())
                {
                    app.status_message = Some(format!("🔔 {title}"));
                }
            }
            matches!(app.view, View::Attached(focused) if focused == id)
                || outcome.bell
                || attention_changed
                || title_changed
                || notification_changed
        }
        MuxEvent::Exited(id, code) => {
            let was_focused = app.mux.as_ref().is_some_and(|mux| mux.focused == Some(id));
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
            if was_focused {
                app.host_sequences
                    .push(crate::host_terminal::CLEAR_PROGRESS.to_vec());
            }
            true
        }
        MuxEvent::SessionLifecycle(id, event) => {
            let focused = app.mux.as_ref().and_then(|mux| mux.focused) == Some(id);
            let (notification, attention_changed, action_changed, title, session_id) = app
                .mux
                .as_mut()
                .and_then(|mux| mux.pane_mut(id))
                .and_then(|pane| {
                    if !pane.is_running() {
                        return None;
                    }
                    let attention_before = pane.needs_attention();
                    let action_before = pane.requires_user_action();
                    let notification = pane.apply_lifecycle(event);
                    Some((
                        notification,
                        attention_before != pane.needs_attention(),
                        action_before != pane.requires_user_action(),
                        pane.title.clone(),
                        pane.session_id.clone(),
                    ))
                })
                .unwrap_or_default();
            if let Some(notification) = notification {
                let kind = match notification {
                    PaneNotification::Question => NotificationKind::Question,
                    PaneNotification::PlanApproval => NotificationKind::PlanApproval,
                    PaneNotification::Ready => NotificationKind::Ready,
                    PaneNotification::Error => NotificationKind::Error,
                };
                app.enqueue_notification(kind, title, Some(&session_id));
            }
            if focused && action_changed {
                sync_outer_progress(app);
            }
            focused || attention_changed || action_changed || notification.is_some()
        }
        MuxEvent::HostSequence(id, sequence) => {
            let progress = crate::host_terminal::progress_state_from_sequence(&sequence);
            if let Some(progress) = progress {
                if let Some(pane) = app.mux.as_mut().and_then(|mux| mux.pane_mut(id)) {
                    if pane.is_running() {
                        pane.record_progress_state(progress);
                    }
                }
            }
            let focused = app.mux.as_ref().and_then(|mux| mux.focused);
            let waiting = app
                .mux
                .as_ref()
                .and_then(|mux| mux.pane(id))
                .is_some_and(|pane| pane.requires_user_action());
            if progress.is_none()
                || (focused == Some(id)
                    && !progress.is_some_and(|state| state.is_working() && waiting))
            {
                app.host_sequences.push(sequence);
                true
            } else {
                false
            }
        }
        MuxEvent::ConfigChanged => app.request_config_reload(),
        MuxEvent::Term(_) => true,
    }
}

fn signals_start_cycle(
    initially_working: bool,
    events: &[crate::mux::callbacks::PaneSignalEvent],
) -> bool {
    let mut working = initially_working;
    let mut started = false;
    for event in events {
        if let crate::mux::callbacks::PaneSignalEvent::Progress(progress) = event {
            if progress.is_working() {
                started |= !working;
                working = true;
            } else {
                working = false;
            }
        }
    }
    started
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
        app.disable_config_persistence();
        app
    }

    #[test]
    fn a_clear_then_working_batch_starts_a_fresh_notification_cycle() {
        use crate::host_terminal::ProgressState::{Clear, Indeterminate};
        use crate::mux::callbacks::PaneSignalEvent::Progress;

        assert!(!signals_start_cycle(true, &[Progress(Clear)]));
        assert!(signals_start_cycle(
            true,
            &[Progress(Clear), Progress(Indeterminate)]
        ));
        assert!(signals_start_cycle(
            false,
            &[Progress(Indeterminate), Progress(Clear)]
        ));
    }

    fn attached_mux_app(session_id: &str) -> App {
        let mut app = mux_app();
        push_test_pane(&mut app, 1, session_id);
        app.view = View::Attached(1);
        app
    }

    fn push_test_pane(app: &mut App, id: u64, session_id: &str) {
        push_test_pane_at(app, id, session_id, std::env::temp_dir());
    }

    fn push_test_pane_at(app: &mut App, id: u64, session_id: &str, cwd: std::path::PathBuf) {
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
                cwd,
                session_id: session_id.to_string(),
                program,
                args,
                events_path: None,
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
    fn prefix_q_confirms_before_ending_a_lone_active_session() {
        let mut app = attached_mux_app("quit-lone-session");

        send_prefix_command(&mut app, 'q');

        assert!(!app.should_quit);
        assert!(app.confirm_quit);
    }

    #[test]
    fn prefix_q_dismisses_an_exited_session_without_confirming() {
        let mut app = attached_mux_app("quit-exited-session");
        app.mux
            .as_mut()
            .unwrap()
            .focused_pane_mut()
            .unwrap()
            .mark_exited(Some(0));

        send_prefix_command(&mut app, 'q');

        assert!(app.should_quit);
        assert!(!app.confirm_quit);
    }

    #[test]
    fn prefix_q_confirms_when_other_sessions_would_die_unseen() {
        let mut app = attached_mux_app("quit-visible-session");
        // Pushing focuses the new pane, so the first one becomes the background
        // session the user cannot see.
        push_test_pane(&mut app, 2, "quit-background-session");
        app.view = View::Attached(2);

        send_prefix_command(&mut app, 'q');

        assert!(app.confirm_quit);
        assert!(!app.should_quit);
    }

    #[test]
    fn confirming_the_quit_prompt_from_a_pane_quits() {
        let mut app = mux_app();
        app.view = View::Attached(1);
        app.confirm_quit = true;

        handle_attached_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)),
        );

        assert!(app.should_quit);
        assert!(!app.confirm_quit);
    }

    #[test]
    fn declining_the_quit_prompt_returns_to_the_pane_without_a_stale_notice() {
        let mut app = mux_app();
        app.view = View::Attached(1);
        app.confirm_quit = true;

        handle_attached_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE)),
        );

        assert!(!app.should_quit);
        assert!(!app.confirm_quit);
        assert_eq!(app.view, View::Attached(1));
        assert_eq!(app.status_message, None);
    }

    #[test]
    fn the_quit_prompt_swallows_keys_instead_of_forwarding_them_to_the_child() {
        let mut app = mux_app();
        app.view = View::Attached(1);
        app.confirm_quit = true;

        handle_attached_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL)),
        );

        assert_eq!(app.mux.as_ref().unwrap().prefix_state, PrefixState::Idle);
        assert!(!app.confirm_quit);
        assert!(!app.should_quit);
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
            patches_loaded: true,
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
    fn q_leaves_the_inspector_from_anywhere_in_it() {
        let mut app = pull_request_app();

        press(&mut app, KeyCode::Char('q'));
        assert!(app.github_inspector.is_none(), "q closes from a tab");

        // From inside a diff, where Esc only steps back to the tree.
        let mut app = pull_request_app();
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            app.github_inspector.as_ref().unwrap().files_pane,
            crate::app::FilesPane::Diff
        );

        press(&mut app, KeyCode::Char('q'));
        assert!(
            app.github_inspector.is_none(),
            "q closes outright rather than returning to the tree"
        );
    }

    #[test]
    fn q_closes_the_loading_and_error_screens() {
        let mut app = pull_request_app();
        app.github_inspector.as_mut().unwrap().screen = crate::app::GithubInspectorScreen::Loading;
        press(&mut app, KeyCode::Char('q'));
        assert!(app.github_inspector.is_none());

        let mut app = pull_request_app();
        app.github_inspector.as_mut().unwrap().screen =
            crate::app::GithubInspectorScreen::Error("nope".to_string());
        press(&mut app, KeyCode::Char('q'));
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
    fn per_session_state_is_keyed_on_the_session_not_the_pane_number() {
        // Pane numbering restarts at 1 every run, so deriving a scratchpad or
        // terminal key from it made two unrelated new sessions share one file.
        let first_run = attached_mux_app("session-alpha");
        let (first_pane, alpha, _, _) = focused_workspace_context(&first_run).unwrap();

        let second_run = attached_mux_app("session-beta");
        let (second_pane, beta, _, _) = focused_workspace_context(&second_run).unwrap();

        assert_eq!(first_pane, second_pane, "the same pane number, as in life");
        assert_eq!(alpha, "session-alpha");
        assert_eq!(beta, "session-beta");
        assert_ne!(alpha, beta, "but state must not be shared between them");
    }

    fn snippet_key(app: &mut App, code: KeyCode) {
        handle_snippet_event(app, Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    #[test]
    fn prefix_s_opens_snippets_and_new_snippets_default_to_global() {
        let mut app = attached_mux_app("snippet-session");
        app.disable_config_persistence();

        send_prefix_command(&mut app, 's');
        assert!(app.snippet_modal.is_some());

        snippet_key(&mut app, KeyCode::Char('a'));
        {
            let modal = app.snippet_modal.as_mut().unwrap();
            assert_eq!(modal.editor_scope, SnippetScope::Global);
            modal.editor_name = "Review".to_string();
            modal.editor_prompt = "Review this carefully.".to_string();
        }

        handle_snippet_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
        );

        assert_eq!(app.config.snippets.len(), 1);
        assert_eq!(app.config.snippets[0].name, "Review");
        assert_eq!(
            app.snippet_modal.as_ref().unwrap().screen,
            SnippetScreen::List
        );
    }

    #[test]
    fn prefix_u_installs_without_detaching_or_ending_the_pane() {
        let mut app = attached_mux_app("update-session");
        app.update_info = Some(crate::updater::UpdateInfo {
            current_version: "0.18.0".to_string(),
            latest_version: "0.19.0".to_string(),
        });

        send_prefix_command(&mut app, 'u');

        assert_eq!(app.update_install_requested_for.as_deref(), Some("0.19.0"));
        assert!(matches!(app.view, View::Attached(1)));
        assert!(app.mux.as_ref().unwrap().pane(1).unwrap().is_running());
        assert!(!app.should_quit);

        handle_attached_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
        );
        assert!(app.update_notice.is_none());
    }

    #[test]
    fn snippet_editor_navigation_follows_name_scope_prompt() {
        let mut app = attached_mux_app("snippet-navigation-session");
        app.open_snippets();
        snippet_key(&mut app, KeyCode::Char('a'));
        assert_eq!(
            app.snippet_modal.as_ref().unwrap().editor_field,
            SnippetEditorField::Name
        );

        snippet_key(&mut app, KeyCode::Tab);
        assert_eq!(
            app.snippet_modal.as_ref().unwrap().editor_field,
            SnippetEditorField::Scope
        );
        snippet_key(&mut app, KeyCode::Tab);
        assert_eq!(
            app.snippet_modal.as_ref().unwrap().editor_field,
            SnippetEditorField::Prompt
        );
        snippet_key(&mut app, KeyCode::BackTab);
        assert_eq!(
            app.snippet_modal.as_ref().unwrap().editor_field,
            SnippetEditorField::Scope
        );

        app.snippet_modal.as_mut().unwrap().editor_field = SnippetEditorField::Name;
        snippet_key(&mut app, KeyCode::Enter);
        assert_eq!(
            app.snippet_modal.as_ref().unwrap().editor_field,
            SnippetEditorField::Scope
        );
    }

    #[test]
    fn opening_snippets_loads_the_focused_sessions_project_collection() {
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir(project.path().join(".git")).unwrap();
        let project_snippet = crate::config::PromptSnippet {
            name: "Local".to_string(),
            prompt: "Only in this repository".to_string(),
        };
        let mut settings =
            crate::config::ProjectSettings::load(project.path(), &UserConfig::default()).unwrap();
        settings.set_snippets(vec![project_snippet.clone()]);
        settings.save().unwrap();

        let mut app = mux_app();
        push_test_pane_at(
            &mut app,
            1,
            "project-snippet-session",
            project.path().to_path_buf(),
        );
        app.view = View::Attached(1);

        send_prefix_command(&mut app, 's');

        let modal = app.snippet_modal.as_ref().unwrap();
        assert_eq!(modal.project, vec![project_snippet]);
        assert_eq!(
            modal.project_root.as_deref(),
            Some(project.path()),
            "scope follows the focused pane, not CST's launch directory"
        );
    }

    #[test]
    fn using_a_snippet_closes_the_modal_and_focuses_chat_without_submitting() {
        let mut app = attached_mux_app("snippet-use-session");
        app.config.snippets = vec![crate::config::PromptSnippet {
            name: "Plan".to_string(),
            prompt: "Make a plan first; do not execute it yet.".to_string(),
        }];
        app.workspace_focus = WorkspaceFocus::Scratchpad;
        app.open_snippets();

        snippet_key(&mut app, KeyCode::Enter);

        assert!(app.snippet_modal.is_none());
        assert_eq!(app.workspace_focus, WorkspaceFocus::Chat);
        // Pane::send_prompt_snippet owns paste-safety checks; the next test covers
        // multiline text before Copilot has enabled bracketed paste.
    }

    #[test]
    fn multiline_snippet_waits_for_bracketed_paste_instead_of_pressing_enter() {
        let mut app = attached_mux_app("snippet-safe-paste-session");
        app.config.snippets = vec![crate::config::PromptSnippet {
            name: "Multiline".to_string(),
            prompt: "line one\nline two".to_string(),
        }];
        app.open_snippets();

        snippet_key(&mut app, KeyCode::Enter);

        let modal = app
            .snippet_modal
            .as_ref()
            .expect("unsafe paste keeps the modal open");
        assert!(modal
            .error
            .as_deref()
            .unwrap()
            .contains("not ready for multiline paste"));
    }

    #[test]
    fn deleting_a_snippet_requires_confirmation() {
        let mut app = attached_mux_app("snippet-delete-session");
        app.disable_config_persistence();
        app.config.snippets = vec![crate::config::PromptSnippet {
            name: "Disposable".to_string(),
            prompt: "Delete me".to_string(),
        }];
        app.open_snippets();

        snippet_key(&mut app, KeyCode::Char('d'));
        assert_eq!(
            app.snippet_modal.as_ref().unwrap().screen,
            SnippetScreen::ConfirmDelete
        );
        snippet_key(&mut app, KeyCode::Esc);
        assert_eq!(app.config.snippets.len(), 1, "cancel keeps the snippet");

        snippet_key(&mut app, KeyCode::Char('d'));
        snippet_key(&mut app, KeyCode::Char('y'));
        assert!(app.config.snippets.is_empty());
        assert_eq!(
            app.snippet_modal.as_ref().unwrap().screen,
            SnippetScreen::List
        );
    }

    #[test]
    fn editor_supports_multiline_prompt_input_and_project_scope_guard() {
        let mut app = attached_mux_app("snippet-editor-session");
        app.disable_config_persistence();
        app.open_snippets();
        snippet_key(&mut app, KeyCode::Char('a'));
        {
            let modal = app.snippet_modal.as_mut().unwrap();
            modal.editor_field = SnippetEditorField::Prompt;
        }

        snippet_key(&mut app, KeyCode::Char('o'));
        snippet_key(&mut app, KeyCode::Char('n'));
        snippet_key(&mut app, KeyCode::Char('e'));
        snippet_key(&mut app, KeyCode::Enter);
        handle_snippet_event(&mut app, Event::Paste("two\nthree".to_string()));
        assert_eq!(
            app.snippet_modal.as_ref().unwrap().editor_prompt,
            "one\ntwo\nthree"
        );

        handle_snippet_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL)),
        );
        let modal = app.snippet_modal.as_ref().unwrap();
        assert_eq!(modal.editor_scope, SnippetScope::Global);
        assert!(modal.error.as_deref().unwrap().contains("No Git project"));
    }

    #[test]
    fn editor_rejects_terminal_controls_before_persisting() {
        let mut app = attached_mux_app("snippet-control-session");
        app.open_snippets();
        snippet_key(&mut app, KeyCode::Char('a'));
        {
            let modal = app.snippet_modal.as_mut().unwrap();
            modal.editor_name = "Unsafe".to_string();
            modal.editor_prompt = "text\u{1b}[201~".to_string();
        }

        handle_snippet_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
        );

        assert!(app.config.snippets.is_empty());
        assert!(app
            .snippet_modal
            .as_ref()
            .unwrap()
            .error
            .as_deref()
            .unwrap()
            .contains("control characters"));
    }

    #[test]
    fn editing_can_move_a_global_snippet_into_project_scope() {
        let mut app = attached_mux_app("snippet-move-session");
        app.disable_config_persistence();
        app.config.snippets = vec![crate::config::PromptSnippet {
            name: "Move me".to_string(),
            prompt: "Project-specific prompt".to_string(),
        }];
        app.open_snippets();
        app.snippet_modal.as_mut().unwrap().project_root =
            Some(std::env::temp_dir().join("snippet-project"));

        snippet_key(&mut app, KeyCode::Char('e'));
        handle_snippet_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL)),
        );
        handle_snippet_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
        );

        assert!(app.config.snippets.is_empty());
        let modal = app.snippet_modal.as_ref().unwrap();
        assert_eq!(modal.project.len(), 1);
        assert_eq!(modal.project[0].name, "Move me");
        assert_eq!(modal.screen, SnippetScreen::List);
    }

    #[test]
    fn editing_in_place_does_not_reorder_the_list() {
        let mut app = attached_mux_app("snippet-order-session");
        app.disable_config_persistence();
        app.config.snippets = vec![
            crate::config::PromptSnippet {
                name: "First".to_string(),
                prompt: "one".to_string(),
            },
            crate::config::PromptSnippet {
                name: "Second".to_string(),
                prompt: "two".to_string(),
            },
        ];
        app.open_snippets();
        snippet_key(&mut app, KeyCode::Char('e'));
        app.snippet_modal.as_mut().unwrap().editor_prompt = "updated".to_string();
        handle_snippet_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
        );

        assert_eq!(app.config.snippets[0].name, "First");
        assert_eq!(app.config.snippets[0].prompt, "updated");
        assert_eq!(app.config.snippets[1].name, "Second");
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

        assert!(handle_mux_event(
            &mut app,
            MuxEvent::Output(1, Default::default())
        ));
        assert!(!handle_mux_event(
            &mut app,
            MuxEvent::Output(2, Default::default())
        ));
    }

    #[test]
    fn host_progress_sequences_still_request_the_outer_terminal_flush() {
        let mut app = attached_mux_app("foreground");
        let sequence = b"\x1b]9;4;3;0\x1b\\".to_vec();

        assert!(handle_mux_event(
            &mut app,
            MuxEvent::HostSequence(1, sequence.clone())
        ));
        assert_eq!(app.host_sequences, vec![sequence]);
    }

    #[test]
    fn background_pane_progress_cannot_override_the_focused_tab_spinner() {
        let mut app = attached_mux_app("foreground");
        push_test_pane(&mut app, 2, "background");
        app.mux.as_mut().unwrap().focused = Some(1);
        let sequence = b"\x1b]9;4;0;0\x1b\\".to_vec();

        assert!(!handle_mux_event(
            &mut app,
            MuxEvent::HostSequence(2, sequence)
        ));
        assert!(app.host_sequences.is_empty());
    }

    #[test]
    fn progress_sequence_updates_background_state_before_a_focus_switch() {
        let mut app = attached_mux_app("foreground");
        push_test_pane(&mut app, 2, "background");
        app.mux.as_mut().unwrap().focused = Some(1);

        assert!(!handle_mux_event(
            &mut app,
            MuxEvent::HostSequence(2, b"\x1b]9;4;3;0\x1b\\".to_vec())
        ));
        handle_mux_event(
            &mut app,
            MuxEvent::Output(
                2,
                crate::mux::callbacks::PaneSignals {
                    events: vec![crate::mux::callbacks::PaneSignalEvent::Progress(
                        crate::host_terminal::ProgressState::Indeterminate,
                    )],
                    ..Default::default()
                },
            ),
        );
        assert!(!handle_mux_event(
            &mut app,
            MuxEvent::HostSequence(2, b"\x1b]9;4;0;0\x1b\\".to_vec())
        ));

        app.mux.as_mut().unwrap().focused = Some(2);
        app.view = View::Attached(2);
        sync_workspace_panels(&mut app);
        assert_eq!(
            app.host_sequences.last(),
            Some(&crate::host_terminal::CLEAR_PROGRESS.to_vec()),
            "the clear sequence must be recorded before its later Output event"
        );
    }

    #[test]
    fn focused_question_clears_and_suppresses_progress_until_resolved() {
        use crate::events::lifecycle::{InputKind, LifecycleEvent};

        let mut app = attached_mux_app("foreground-question");
        app.config.notifications.enabled = true;
        app.config.notifications.topic = "private_topic".to_string();
        app.terminal_focused = true;

        let working = crate::mux::callbacks::PaneSignals {
            events: vec![crate::mux::callbacks::PaneSignalEvent::Progress(
                crate::host_terminal::ProgressState::Indeterminate,
            )],
            ..Default::default()
        };
        handle_mux_event(&mut app, MuxEvent::Output(1, working.clone()));
        app.host_sequences.clear();

        assert!(handle_mux_event(
            &mut app,
            MuxEvent::SessionLifecycle(
                1,
                LifecycleEvent::InputRequested {
                    tool_call_id: "question-1".into(),
                    kind: InputKind::Question,
                },
            )
        ));
        assert_eq!(
            app.host_sequences,
            vec![crate::host_terminal::CLEAR_PROGRESS.to_vec()]
        );
        assert_eq!(app.notification_requests.len(), 1);
        assert_eq!(
            app.notification_requests[0].kind,
            NotificationKind::Question
        );
        assert!(app.mux.as_ref().unwrap().pane(1).unwrap().needs_attention());
        app.acknowledge_focused_pane();
        assert!(
            app.mux.as_ref().unwrap().pane(1).unwrap().needs_attention(),
            "a focused question remains visible until it is answered"
        );

        app.host_sequences.clear();
        assert!(!handle_mux_event(
            &mut app,
            MuxEvent::HostSequence(1, b"\x1b]9;4;3;0\x1b\\".to_vec())
        ));
        assert!(app.host_sequences.is_empty());
        handle_mux_event(&mut app, MuxEvent::Output(1, working));

        handle_mux_event(
            &mut app,
            MuxEvent::SessionLifecycle(
                1,
                LifecycleEvent::InputResolved {
                    tool_call_id: "not-the-question".into(),
                    kind: InputKind::Question,
                },
            ),
        );
        assert!(app.mux.as_ref().unwrap().pane(1).unwrap().needs_attention());
        assert!(app.host_sequences.is_empty());

        handle_mux_event(
            &mut app,
            MuxEvent::SessionLifecycle(
                1,
                LifecycleEvent::InputResolved {
                    tool_call_id: "question-1".into(),
                    kind: InputKind::Question,
                },
            ),
        );
        assert!(!app.mux.as_ref().unwrap().pane(1).unwrap().needs_attention());
        assert_eq!(
            app.host_sequences,
            vec![crate::host_terminal::progress_sequence_for_state(
                crate::host_terminal::ProgressState::Indeterminate
            )]
        );

        let complete = crate::mux::callbacks::PaneSignals {
            events: vec![crate::mux::callbacks::PaneSignalEvent::Progress(
                crate::host_terminal::ProgressState::Clear,
            )],
            ..Default::default()
        };
        handle_mux_event(&mut app, MuxEvent::Output(1, complete));
        assert_eq!(app.notification_requests.len(), 2);
        assert_eq!(app.notification_requests[1].kind, NotificationKind::Ready);
    }

    #[test]
    fn background_plan_approval_marks_attention_without_changing_outer_progress() {
        use crate::events::lifecycle::{InputKind, LifecycleEvent};

        let mut app = attached_mux_app("foreground");
        app.config.notifications.enabled = true;
        app.config.notifications.topic = "private_topic".to_string();
        push_test_pane(&mut app, 2, "background-plan");
        app.mux.as_mut().unwrap().focused = Some(1);
        app.host_sequences.clear();

        assert!(handle_mux_event(
            &mut app,
            MuxEvent::SessionLifecycle(
                2,
                LifecycleEvent::InputRequested {
                    tool_call_id: "plan-1".into(),
                    kind: InputKind::PlanApproval,
                },
            )
        ));
        assert!(app.host_sequences.is_empty());
        assert!(app.mux.as_ref().unwrap().pane(2).unwrap().needs_attention());
        assert_eq!(
            app.notification_requests[0].kind,
            NotificationKind::PlanApproval
        );
    }

    #[test]
    fn title_change_for_an_unfocused_pane_repaints_the_tab_strip() {
        let mut app = attached_mux_app("foreground");
        push_test_pane(&mut app, 2, "background");
        app.mux.as_mut().unwrap().focused = Some(1);
        app.view = View::Attached(1);
        let signals = crate::mux::callbacks::PaneSignals {
            title: Some("Renamed background pane".to_string()),
            events: Vec::new(),
        };

        assert!(handle_mux_event(&mut app, MuxEvent::Output(2, signals)));
        assert_eq!(
            app.mux.as_ref().unwrap().pane(2).unwrap().title,
            "Renamed background pane"
        );
    }

    #[test]
    fn background_completion_requests_attention_and_switching_clears_it() {
        let mut app = attached_mux_app("foreground");
        app.config.notifications.enabled = true;
        app.config.notifications.topic = "private_topic".to_string();
        push_test_pane(&mut app, 2, "background");
        app.mux.as_mut().unwrap().focused = Some(1);
        app.view = View::Attached(1);
        app.terminal_focused = true;

        app.mux
            .as_mut()
            .unwrap()
            .pane_mut(2)
            .unwrap()
            .feed_synthetic(b"\x1b]9;4;3;0\x1b\\");
        let working = crate::mux::callbacks::PaneSignals {
            events: vec![crate::mux::callbacks::PaneSignalEvent::Progress(
                crate::host_terminal::ProgressState::Indeterminate,
            )],
            ..Default::default()
        };
        handle_mux_event(&mut app, MuxEvent::Output(2, working));
        app.mux
            .as_mut()
            .unwrap()
            .pane_mut(2)
            .unwrap()
            .feed_synthetic(b"\x1b]9;4;0;0\x1b\\");

        let complete = crate::mux::callbacks::PaneSignals {
            events: vec![crate::mux::callbacks::PaneSignalEvent::Progress(
                crate::host_terminal::ProgressState::Clear,
            )],
            ..Default::default()
        };
        assert!(
            handle_mux_event(&mut app, MuxEvent::Output(2, complete)),
            "attention transition must repaint the internal tab strip"
        );
        assert!(app.mux.as_ref().unwrap().pane(2).unwrap().needs_attention());
        assert_eq!(app.notification_requests.len(), 1);
        assert_eq!(app.notification_requests[0].kind, NotificationKind::Ready);
        assert_eq!(app.notification_requests[0].session_title, "Test session 2");
        assert!(app
            .mux
            .as_ref()
            .unwrap()
            .pane(2)
            .unwrap()
            .display_title()
            .starts_with("? "));

        app.mux.as_mut().unwrap().focused = Some(2);
        app.view = View::Attached(2);
        sync_workspace_panels(&mut app);

        assert!(!app.mux.as_ref().unwrap().pane(2).unwrap().needs_attention());
        let _ = app.mux.as_mut().unwrap().shutdown();
    }

    #[test]
    fn completion_in_the_focused_terminal_needs_attention_only_when_unfocused() {
        let mut app = attached_mux_app("focus-test");
        app.config.notifications.enabled = true;
        app.config.notifications.topic = "private_topic".to_string();
        app.terminal_focused = false;
        app.mux
            .as_mut()
            .unwrap()
            .pane_mut(1)
            .unwrap()
            .feed_synthetic(b"\x1b]9;4;3;0\x1b\\\x1b]9;4;0;0\x1b\\");

        let signals = crate::mux::callbacks::PaneSignals {
            events: vec![
                crate::mux::callbacks::PaneSignalEvent::Progress(
                    crate::host_terminal::ProgressState::Indeterminate,
                ),
                crate::mux::callbacks::PaneSignalEvent::Progress(
                    crate::host_terminal::ProgressState::Clear,
                ),
            ],
            ..Default::default()
        };
        handle_mux_event(&mut app, MuxEvent::Output(1, signals));
        assert!(app.mux.as_ref().unwrap().pane(1).unwrap().needs_attention());
        assert_eq!(app.notification_requests.len(), 1);
        assert_eq!(app.notification_requests[0].kind, NotificationKind::Ready);

        app.terminal_focused = true;
        app.acknowledge_focused_pane();
        assert!(app.terminal_focused);
        assert!(!app.mux.as_ref().unwrap().pane(1).unwrap().needs_attention());
        let _ = app.mux.as_mut().unwrap().shutdown();
    }

    #[test]
    fn focused_ready_and_error_events_still_publish_for_history() {
        let mut app = attached_mux_app("notification-history");
        app.config.notifications.enabled = true;
        app.config.notifications.topic = "private_topic".to_string();
        app.config.ntfy_verbose = true;
        let copilot_home = tempfile::tempdir().unwrap();
        app.copilot_home = copilot_home.path().to_path_buf();
        app.terminal_focused = true;

        let ready = crate::mux::callbacks::PaneSignals {
            events: vec![
                crate::mux::callbacks::PaneSignalEvent::Progress(
                    crate::host_terminal::ProgressState::Indeterminate,
                ),
                crate::mux::callbacks::PaneSignalEvent::Progress(
                    crate::host_terminal::ProgressState::Clear,
                ),
            ],
            ..Default::default()
        };
        handle_mux_event(&mut app, MuxEvent::Output(1, ready));
        assert!(!app.mux.as_ref().unwrap().pane(1).unwrap().needs_attention());
        assert_eq!(app.notification_requests[0].kind, NotificationKind::Ready);
        assert_eq!(
            app.notification_requests[0].events_path.as_deref(),
            Some(
                copilot_home
                    .path()
                    .join("session-state")
                    .join("notification-history")
                    .join("events.jsonl")
                    .as_path()
            )
        );

        let error = crate::mux::callbacks::PaneSignals {
            events: vec![crate::mux::callbacks::PaneSignalEvent::Progress(
                crate::host_terminal::ProgressState::Error,
            )],
            ..Default::default()
        };
        handle_mux_event(&mut app, MuxEvent::Output(1, error));
        assert_eq!(app.notification_requests[1].kind, NotificationKind::Error);
        let clear = crate::mux::callbacks::PaneSignals {
            events: vec![crate::mux::callbacks::PaneSignalEvent::Progress(
                crate::host_terminal::ProgressState::Clear,
            )],
            ..Default::default()
        };
        handle_mux_event(&mut app, MuxEvent::Output(1, clear));
        assert_eq!(
            app.notification_requests.len(),
            2,
            "error suppresses duplicate ready in the same work cycle"
        );
        let _ = app.mux.as_mut().unwrap().shutdown();
    }

    #[test]
    fn exit_events_report_the_status() {
        use crate::events::lifecycle::{InputKind, LifecycleEvent};

        let mut app = attached_mux_app("exiting");
        app.config.notifications.enabled = true;
        app.config.notifications.topic = "private_topic".to_string();
        handle_mux_event(
            &mut app,
            MuxEvent::HostSequence(1, b"\x1b]9;4;3;0\x1b\\".to_vec()),
        );
        app.host_sequences.clear();

        assert!(handle_mux_event(&mut app, MuxEvent::Exited(1, Some(2))));

        let message = app.status_message.as_deref().unwrap();
        assert!(message.contains("code 2"), "unexpected message: {message}");
        assert_eq!(
            app.host_sequences,
            vec![crate::host_terminal::CLEAR_PROGRESS.to_vec()]
        );
        let pane = app.mux.as_ref().unwrap().pane(1).unwrap();
        assert_eq!(
            pane.effective_progress_state(),
            crate::host_terminal::ProgressState::Clear
        );
        assert!(!pane.is_working());

        app.host_sequences.clear();
        assert!(handle_mux_event(
            &mut app,
            MuxEvent::SessionLifecycle(
                1,
                LifecycleEvent::InputRequested {
                    tool_call_id: "late-question".into(),
                    kind: InputKind::Question,
                }
            )
        ));
        assert!(!app.mux.as_ref().unwrap().pane(1).unwrap().needs_attention());
        assert!(app.notification_requests.is_empty());
        assert!(app.host_sequences.is_empty());
    }

    #[test]
    fn background_exit_does_not_clear_the_focused_pane_progress() {
        let mut app = attached_mux_app("foreground");
        push_test_pane(&mut app, 2, "background");
        app.mux.as_mut().unwrap().focused = Some(1);

        assert!(handle_mux_event(&mut app, MuxEvent::Exited(2, Some(0))));
        assert!(app.host_sequences.is_empty());
    }
}
