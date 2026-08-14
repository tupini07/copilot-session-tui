use crate::app::{
    App, DeleteTarget, Mode, NewSessionRequest, PendingWorktree, SettingsEditField, View,
};
use crate::config;
use crate::session::loader;
use crate::session::manager;
use crate::session::worktree;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use std::path::{Path, PathBuf};

pub fn handle_input(app: &mut App) -> anyhow::Result<bool> {
    if !event::poll(std::time::Duration::from_millis(100))? {
        return Ok(false);
    }

    let event = event::read()?;
    handle_terminal_event(app, event)?;
    Ok(true)
}

/// Apply an already-read terminal event to the session list UI.
///
/// Split out from `handle_input` so the multiplexer's event thread — which owns the
/// only reader of the terminal — can route events here instead of polling separately.
pub fn handle_terminal_event(app: &mut App, event: Event) -> anyhow::Result<()> {
    if app.mode == Mode::Scratchpad {
        handle_scratchpad(app, event);
        return Ok(());
    }

    if app.terminal.is_focused() {
        handle_terminal(app, event);
        return Ok(());
    }

    let Event::Key(key) = event else {
        return Ok(());
    };

    if key.kind != KeyEventKind::Press {
        return Ok(());
    }

    if app.confirm_quit {
        handle_quit_confirm(app, key.code);
        return Ok(());
    }

    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        request_quit(app);
        return Ok(());
    }

    // The prefix works from the session list too, so panes stay reachable after
    // detaching without having to re-resume a session from the list.
    if matches!(app.mode, Mode::Normal) && crate::mux_input::handle_list_prefix(app, key) {
        return Ok(());
    }

    match app.mode {
        Mode::Normal => handle_normal(app, key.code),
        Mode::Search => handle_search(app, key.code),
        Mode::Rename => handle_rename(app, key.code),
        Mode::ConfirmDelete => handle_confirm_delete(app, key.code),
        Mode::ConfirmForceDelete => handle_confirm_force_delete(app, key.code),
        Mode::FilterProject => handle_filter_project(app, key.code),
        Mode::Help => handle_help(app, key.code),
        Mode::Settings => handle_settings(app, key.code),
        Mode::ProjectSettings => handle_project_settings(app, key.code),
        Mode::BranchName => handle_branch_name(app, key.code),
        Mode::PaneList => handle_pane_list(app, key.code),
        Mode::Scratchpad => unreachable!(),
    }

    Ok(())
}

/// Quitting kills every pane, so confirm while sessions are still running.
fn request_quit(app: &mut App) {
    let running = app
        .mux
        .as_mut()
        .map(|mux| {
            mux.reap();
            mux.running_count()
        })
        .unwrap_or(0);
    if running > 0 {
        app.confirm_quit = true;
    } else {
        app.should_quit = true;
    }
}

fn handle_quit_confirm(app: &mut App, key: KeyCode) {
    match key {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            app.confirm_quit = false;
            app.should_quit = true;
        }
        _ => {
            app.confirm_quit = false;
            app.status_message = Some("Quit cancelled".to_string());
        }
    }
}

/// Pane switcher: attach, kill, or dismiss without touching the underlying session list.
fn handle_pane_list(app: &mut App, key: KeyCode) {
    let Some(mux) = app.mux.as_mut() else {
        app.mode = Mode::Normal;
        return;
    };
    let count = mux.panes.len();
    if count == 0 {
        app.mode = Mode::Normal;
        return;
    }

    match key {
        KeyCode::Up | KeyCode::Char('k') => {
            app.pane_selected = app.pane_selected.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.pane_selected = (app.pane_selected + 1).min(count - 1);
        }
        KeyCode::Char(digit @ '1'..='9') => {
            let index = digit as usize - '1' as usize;
            if index < count {
                app.pane_selected = index;
            }
        }
        KeyCode::Enter => {
            let index = app.pane_selected.min(count - 1);
            mux.select_index(index);
            let id = mux.panes[index].id;
            app.mode = Mode::Normal;
            app.view = View::Attached(id);
            crate::mux_input::sync_workspace_panels(app);
        }
        KeyCode::Char('x') => {
            let index = app.pane_selected.min(count - 1);
            let id = mux.panes[index].id;
            let title = mux.panes[index].title.clone();
            mux.remove(id);
            app.pane_selected = app.pane_selected.min(mux.panes.len().saturating_sub(1));
            if mux.panes.is_empty() {
                app.mode = Mode::Normal;
            }
            app.view = View::List;
            app.status_message = Some(format!("Ended '{title}'"));
        }
        KeyCode::Esc | KeyCode::Char('q') => {
            app.mode = Mode::Normal;
        }
        _ => {}
    }
}

fn handle_normal(app: &mut App, key: KeyCode) {
    match key {
        KeyCode::Char('q') | KeyCode::Esc => {
            request_quit(app);
        }
        KeyCode::Up | KeyCode::Char('k') => app.move_up(),
        KeyCode::Down | KeyCode::Char('j') => app.move_down(),
        KeyCode::Home => {
            app.selected = 0;
            app.scroll_offset = 0;
        }
        KeyCode::End => {
            if !app.filtered_indices.is_empty() {
                app.selected = app.filtered_indices.len() - 1;
                if app.selected >= app.visible_rows {
                    app.scroll_offset = app.selected - app.visible_rows + 1;
                }
            }
        }
        KeyCode::Enter => resume_selected(app),
        KeyCode::Char('/') => {
            app.mode = Mode::Search;
            app.search_query.clear();
        }
        KeyCode::Char('r') => {
            if let Some(session) = app.selected_session() {
                app.rename_input = session.summary.clone().unwrap_or_default();
                app.mode = Mode::Rename;
            }
        }
        KeyCode::Char('e') => open_scratchpad(app),
        KeyCode::Char('t') => toggle_terminal(app),
        KeyCode::Char(' ') => match app.toggle_selected_favorite() {
            Ok(Some(true)) => app.status_message = Some("Added to favorites".to_string()),
            Ok(Some(false)) => app.status_message = Some("Removed from favorites".to_string()),
            Ok(None) => {}
            Err(error) => {
                app.status_message = Some(format!("Failed to save favorite: {error}"));
            }
        },
        KeyCode::Char('d') => begin_delete(app),
        KeyCode::Char('f') | KeyCode::Char('p') => {
            app.project_selected = 0;
            app.project_scroll_offset = 0;
            app.project_search_query.clear();
            app.mode = Mode::FilterProject;
        }
        KeyCode::Char('s') => {
            app.cycle_sort();
            app.status_message = Some(format!("Sorted by: {}", app.sort_label()));
        }
        KeyCode::Char('c') => {
            app.set_project_filter(None);
            app.status_message = Some("Filter cleared".to_string());
        }
        KeyCode::Char('n') => {
            if let Some(cwd) = app.new_session_dir() {
                if app.mux_enabled() {
                    let title = project_title(&cwd);
                    match app.attach_new_session(&cwd, title) {
                        Ok(()) => crate::mux_input::sync_workspace_panels(app),
                        Err(error) => {
                            app.status_message = Some(format!("Cannot start session: {error}"))
                        }
                    }
                } else {
                    app.should_new_session = Some(NewSessionRequest::Normal { cwd });
                }
            } else {
                app.status_message =
                    Some("Filter by a project first (f) to start a new session".to_string());
            }
        }
        KeyCode::Char('N') => begin_worktree_session(app),
        KeyCode::Char('?') => app.mode = Mode::Help,
        KeyCode::Char(',') => {
            app.settings_selected = 0;
            app.settings_editing = None;
            app.settings_input.clear();
            app.mode = Mode::Settings;
        }
        KeyCode::Char('.') => begin_project_settings(app),
        KeyCode::Char('u') => {
            if app.update_info.is_some() {
                app.should_update = true;
            } else {
                app.status_message = Some("No update available".to_string());
            }
        }
        _ => {}
    }
}

fn resume_selected(app: &mut App) {
    let Some(session) = app.selected_session() else {
        return;
    };
    let (id, cwd, title) = (
        session.id.clone(),
        session.cwd.clone(),
        session.display_name().to_string(),
    );

    if app.mux_enabled() {
        // A pane we own is not "busy elsewhere" — re-focus it instead of refusing.
        if app.pane_for_session(&id).is_none() && session.is_active {
            app.status_message = Some("Cannot resume: session is already active".to_string());
            return;
        }
        match app.attach_session(&id, &cwd, title) {
            Ok(()) => crate::mux_input::sync_workspace_panels(app),
            Err(error) => app.status_message = Some(format!("Cannot attach session: {error}")),
        }
        return;
    }

    if session.is_active {
        app.status_message = Some("Cannot resume: session is already active".to_string());
    } else {
        app.should_resume = Some((id, cwd));
    }
}

/// Short, human-readable pane label derived from a project directory.
fn project_title(cwd: &str) -> String {
    Path::new(cwd)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(cwd)
        .to_string()
}

fn toggle_terminal(app: &mut App) {
    let Some(session) = app.selected_session() else {
        return;
    };
    let session_id = session.id.clone();
    let session_name = session.display_name().to_string();
    let cwd = session.cwd.clone();

    if app.terminal.is_visible() && app.terminal.active_session_id() == Some(session_id.as_str()) {
        app.terminal.hide();
        app.status_message = Some("Terminal hidden; shell is still running".to_string());
        return;
    }

    match app
        .terminal
        .activate(session_id, session_name, cwd, &app.config.terminal)
    {
        Ok(crate::terminal_pane::Activation::Opened) => {
            app.status_message = Some("Terminal opened in session directory".to_string())
        }
        Ok(crate::terminal_pane::Activation::Focused) => {
            app.status_message = Some("Terminal focused".to_string())
        }
        Ok(crate::terminal_pane::Activation::Restarted) => {
            app.status_message = Some("Terminal shell restarted".to_string())
        }
        Err(error) => {
            app.status_message = Some(format!("Cannot open terminal: {error}"));
        }
    }
}

fn handle_terminal(app: &mut App, event: Event) {
    if let Event::Key(key) = &event {
        if key.kind == KeyEventKind::Press
            && key.code == KeyCode::Char('b')
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            app.terminal.unfocus();
            app.status_message = Some("Terminal remains open; press t to hide it".to_string());
            return;
        }

        if key.kind == KeyEventKind::Press
            && key.code == KeyCode::Enter
            && app
                .terminal
                .active()
                .is_some_and(|terminal| !terminal.is_running())
        {
            match app.terminal.restart_active(&app.config.terminal) {
                Ok(()) => {
                    app.status_message = Some("Terminal shell restarted".to_string());
                }
                Err(error) => {
                    app.status_message = Some(format!("Cannot restart terminal: {error}"));
                }
            }
            return;
        }
    }

    if let Some(terminal) = app.terminal.active_mut() {
        if let Err(error) = terminal.handle_event(event) {
            app.status_message = Some(format!("Terminal input failed: {error}"));
        }
    }
}

fn begin_delete(app: &mut App) {
    let Some(session) = app.selected_session() else {
        return;
    };
    if session.is_active || loader::session_is_active(&session.dir_path) {
        app.status_message = Some("Cannot delete: session is currently active".to_string());
        return;
    }

    let cwd = session.cwd.clone();

    match worktree::managed_worktree_for_cwd(Path::new(&cwd)) {
        Ok(Some(entry)) => match worktree::is_dirty(&entry) {
            Ok(dirty) => {
                app.pending_delete = Some(DeleteTarget::Managed { entry, dirty });
                app.mode = Mode::ConfirmDelete;
            }
            Err(error) => {
                app.status_message = Some(format!("Cannot inspect worktree: {error}"));
            }
        },
        Ok(None) => {
            app.pending_delete = Some(DeleteTarget::SessionOnly);
            app.mode = Mode::ConfirmDelete;
        }
        Err(error) => {
            app.status_message = Some(format!("Cannot inspect worktree registry: {error}"));
        }
    }
}

fn open_scratchpad(app: &mut App) {
    let Some(session) = app.selected_session() else {
        return;
    };
    let session_id = session.id.clone();
    let session_name = session.display_name().to_string();
    match crate::scratchpad::Scratchpad::open(&session_id, session_name) {
        Ok(scratchpad) => {
            app.scratchpad = Some(scratchpad);
            app.mode = Mode::Scratchpad;
        }
        Err(error) => {
            app.status_message = Some(format!("Cannot open scratchpad: {error}"));
        }
    }
}

fn handle_scratchpad(app: &mut App, event: Event) {
    let Some(scratchpad) = app.scratchpad.as_mut() else {
        app.mode = Mode::Normal;
        return;
    };
    match scratchpad.handle_event(event) {
        Ok(crate::scratchpad::InputOutcome::Continue) => {}
        Ok(crate::scratchpad::InputOutcome::Close) => {
            app.scratchpad = None;
            app.mode = Mode::Normal;
            app.status_message = Some("Scratchpad saved".to_string());
        }
        Err(error) => {
            scratchpad.status_message = Some(format!("Scratchpad save failed: {error}"));
        }
    }
}

fn begin_worktree_session(app: &mut App) {
    let Some(project) = app.active_project() else {
        app.status_message =
            Some("Filter by a project first (f) to create an isolated session".to_string());
        return;
    };

    let repository = match worktree::resolve_repository_root(Path::new(&project)) {
        Ok(repository) => repository,
        Err(error) => {
            app.status_message = Some(format!("Cannot create worktree: {error}"));
            return;
        }
    };
    let effective = match config::effective_worktree(&app.config, &repository) {
        Ok(effective) => effective,
        Err(error) => {
            app.status_message = Some(error.to_string());
            return;
        }
    };

    app.branch_input = worktree::generated_branch_name(&effective.branch_prefix);
    app.branch_config = Some(effective);
    app.mode = Mode::BranchName;
}

fn begin_project_settings(app: &mut App) {
    let Some(project) = app.active_project() else {
        app.status_message =
            Some("Filter by a project first (f) to edit project settings".to_string());
        return;
    };
    let repository = match worktree::resolve_repository_root(Path::new(&project)) {
        Ok(repository) => repository,
        Err(error) => {
            app.status_message = Some(format!(
                "Project settings require a Git repository: {error}"
            ));
            return;
        }
    };

    match config::ProjectSettings::load(&repository, &app.config) {
        Ok(settings) => {
            app.project_settings = Some(settings);
            app.project_settings_selected = 0;
            app.project_settings_editing = false;
            app.project_settings_input.clear();
            app.mode = Mode::ProjectSettings;
        }
        Err(error) => app.status_message = Some(error.to_string()),
    }
}

fn handle_search(app: &mut App, key: KeyCode) {
    match key {
        KeyCode::Esc => {
            app.mode = Mode::Normal;
            app.search_query.clear();
            app.apply_filter();
        }
        KeyCode::Enter => app.mode = Mode::Normal,
        KeyCode::Backspace => {
            app.search_query.pop();
            app.apply_filter();
        }
        KeyCode::Char(c) => {
            app.search_query.push(c);
            app.apply_filter();
        }
        KeyCode::Up => app.move_up(),
        KeyCode::Down => app.move_down(),
        _ => {}
    }
}

fn handle_rename(app: &mut App, key: KeyCode) {
    match key {
        KeyCode::Esc => app.mode = Mode::Normal,
        KeyCode::Enter => {
            if let Some(idx) = app.selected_real_index() {
                let dir = app.sessions[idx].dir_path.clone();
                let new_name = app.rename_input.clone();
                match manager::rename_session(&dir, &new_name) {
                    Ok(()) => {
                        app.sessions[idx].summary = Some(new_name);
                        app.status_message = Some("Session renamed".to_string());
                    }
                    Err(error) => {
                        app.status_message = Some(format!("Rename failed: {error}"));
                    }
                }
            }
            app.mode = Mode::Normal;
        }
        KeyCode::Backspace => {
            app.rename_input.pop();
        }
        KeyCode::Char(c) => app.rename_input.push(c),
        _ => {}
    }
}

fn handle_confirm_delete(app: &mut App, key: KeyCode) {
    if matches!(key, KeyCode::Char('y') | KeyCode::Char('Y')) {
        if matches!(
            app.pending_delete,
            Some(DeleteTarget::Managed { dirty: true, .. })
        ) {
            app.mode = Mode::ConfirmForceDelete;
        } else {
            perform_delete(app, false);
        }
    } else {
        cancel_delete(app);
    }
}

fn handle_confirm_force_delete(app: &mut App, key: KeyCode) {
    if key == KeyCode::Char('Y') {
        perform_delete(app, true);
    } else {
        cancel_delete(app);
    }
}

fn perform_delete(app: &mut App, force: bool) {
    let Some(idx) = app.selected_real_index() else {
        cancel_delete(app);
        return;
    };
    if app.sessions[idx].is_active || loader::session_is_active(&app.sessions[idx].dir_path) {
        app.mode = Mode::Normal;
        app.pending_delete = None;
        app.status_message = Some("Cannot delete: session became active".to_string());
        return;
    }

    let dir = app.sessions[idx].dir_path.clone();
    let session_id = app.sessions[idx].id.clone();
    let target = app
        .pending_delete
        .clone()
        .unwrap_or(DeleteTarget::SessionOnly);
    app.terminal.remove(&session_id);
    let result = match target {
        DeleteTarget::SessionOnly => {
            manager::delete_session(&dir).map(|_| "Session deleted".to_string())
        }
        DeleteTarget::Managed { entry, .. } => manager::delete_managed_session(&dir, &entry, force),
    };

    match result {
        Ok(message) => {
            app.sessions.remove(idx);
            let favorite_error = app.forget_favorite(&session_id).err();
            let scratchpad_error = crate::scratchpad::delete(&session_id).err();
            app.apply_filter();
            let mut errors = Vec::new();
            if let Some(error) = favorite_error {
                errors.push(format!("favorites: {error}"));
            }
            if let Some(error) = scratchpad_error {
                errors.push(format!("scratchpad: {error}"));
            }
            app.status_message = Some(if errors.is_empty() {
                message
            } else {
                format!("{message}; cleanup failed for {}", errors.join(", "))
            });
        }
        Err(error) => {
            app.status_message = Some(format!("Delete failed: {error}"));
        }
    }
    app.pending_delete = None;
    app.mode = Mode::Normal;
}

fn cancel_delete(app: &mut App) {
    app.pending_delete = None;
    app.mode = Mode::Normal;
    app.status_message = Some("Delete cancelled".to_string());
}

fn handle_filter_project(app: &mut App, key: KeyCode) {
    let filtered = app.filtered_project_indices();
    let has_all_option = app.project_search_query.is_empty();
    let total = filtered.len() + usize::from(has_all_option);

    match key {
        KeyCode::Esc => app.mode = Mode::Normal,
        KeyCode::Up => {
            if app.project_selected > 0 {
                app.project_selected -= 1;
                if app.project_selected < app.project_scroll_offset {
                    app.project_scroll_offset = app.project_selected;
                }
            }
        }
        KeyCode::Down => {
            if app.project_selected + 1 < total {
                app.project_selected += 1;
                if app.project_selected >= app.project_scroll_offset + app.project_visible_rows {
                    app.project_scroll_offset = app.project_selected - app.project_visible_rows + 1;
                }
            }
        }
        KeyCode::Enter => {
            if total == 0 {
                return;
            }
            if has_all_option && app.project_selected == 0 {
                app.set_project_filter(None);
                app.status_message = Some("Showing all projects".to_string());
            } else {
                let list_idx = if has_all_option {
                    app.project_selected - 1
                } else {
                    app.project_selected
                };
                if let Some(&project_index) = filtered.get(list_idx) {
                    let project = app.unique_projects[project_index].clone();
                    app.status_message = Some(format!("Filtered to: {project}"));
                    app.set_project_filter(Some(project));
                }
            }
            app.mode = Mode::Normal;
        }
        KeyCode::Backspace => {
            app.project_search_query.pop();
            app.project_selected = 0;
            app.project_scroll_offset = 0;
        }
        KeyCode::Char(c) => {
            app.project_search_query.push(c);
            app.project_selected = 0;
            app.project_scroll_offset = 0;
        }
        _ => {}
    }
}

fn handle_help(app: &mut App, key: KeyCode) {
    if matches!(
        key,
        KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') | KeyCode::Enter
    ) {
        app.mode = Mode::Normal;
    }
}

const SETTINGS_COUNT: usize = 8;

fn handle_settings(app: &mut App, key: KeyCode) {
    if let Some(field) = app.settings_editing {
        match key {
            KeyCode::Esc => {
                app.settings_editing = None;
                app.settings_input.clear();
            }
            KeyCode::Enter => commit_global_setting(app, field),
            KeyCode::Backspace => {
                app.settings_input.pop();
            }
            KeyCode::Char(character) => app.settings_input.push(character),
            _ => {}
        }
        return;
    }

    match key {
        KeyCode::Esc | KeyCode::Char(',') => match config::save(&app.persistable_config()) {
            Ok(()) => {
                app.mode = Mode::Normal;
                app.status_message = Some("Global settings saved".to_string());
            }
            Err(error) => {
                app.status_message = Some(format!("Failed to save global settings: {error}"));
            }
        },
        KeyCode::Up | KeyCode::Char('k') => {
            app.settings_selected = app.settings_selected.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.settings_selected + 1 < SETTINGS_COUNT {
                app.settings_selected += 1;
            }
        }
        KeyCode::Enter | KeyCode::Char(' ') => match app.settings_selected {
            0 => {
                app.config.yolo = !app.config.yolo;
            }
            1 => begin_global_edit(
                app,
                SettingsEditField::Model,
                app.config.model.clone().unwrap_or_default(),
            ),
            2 => cycle_reasoning_effort(app),
            3 => begin_global_edit(
                app,
                SettingsEditField::BranchPrefix,
                app.config.worktree.branch_prefix.clone(),
            ),
            4 => begin_global_edit(
                app,
                SettingsEditField::WorktreeRoot,
                app.config.worktree.root.to_string_lossy().to_string(),
            ),
            5 => {
                // The row reflects the persisted value; a `--mux` override only lasts
                // for this invocation and must not be flipped by editing settings.
                app.mux_on_disk = !app.mux_on_disk;
                app.status_message =
                    Some("Multiplexer setting applies the next time CST starts".to_string());
            }
            6 => begin_global_edit(
                app,
                SettingsEditField::MuxPrefix,
                app.config.mux_prefix.clone(),
            ),
            7 => begin_global_edit(
                app,
                SettingsEditField::TerminalShell,
                app.config.terminal.shell.clone().unwrap_or_default(),
            ),
            _ => {}
        },
        _ => {}
    }
}

fn begin_global_edit(app: &mut App, field: SettingsEditField, value: String) {
    app.settings_editing = Some(field);
    app.settings_input = value;
}

fn commit_global_setting(app: &mut App, field: SettingsEditField) {
    let value = app.settings_input.trim().to_string();
    match field {
        SettingsEditField::Model => {
            app.config.model = (!value.is_empty()).then_some(value);
        }
        SettingsEditField::BranchPrefix => {
            if let Err(error) = worktree::validate_branch_prefix(&value) {
                app.status_message = Some(error.to_string());
                return;
            }
            app.config.worktree.branch_prefix = value;
        }
        SettingsEditField::WorktreeRoot => {
            if value.is_empty() {
                app.status_message = Some("Worktree root cannot be empty".to_string());
                return;
            }
            app.config.worktree.root = PathBuf::from(value);
        }
        SettingsEditField::MuxPrefix => {
            // Reject unparseable chords here rather than silently defaulting at startup.
            let Some(chord) = crate::mux::KeyChord::parse(&value) else {
                app.status_message = Some(format!(
                    "'{value}' is not a valid prefix (try C-b, C-g, C-a)"
                ));
                return;
            };
            app.config.mux_prefix = chord.label();
            if let Some(mux) = app.mux.as_mut() {
                mux.prefix = chord;
            }
        }
        SettingsEditField::TerminalShell => {
            app.config.terminal.shell = (!value.is_empty()).then_some(value);
        }
    }
    app.settings_editing = None;
    app.settings_input.clear();
    if let Err(error) = config::save(&app.persistable_config()) {
        app.status_message = Some(format!("Failed to save global settings: {error}"));
    }
}

fn cycle_reasoning_effort(app: &mut App) {
    let efforts = config::REASONING_EFFORTS;
    app.config.reasoning_effort = match &app.config.reasoning_effort {
        None => Some(efforts[0].to_string()),
        Some(current) => match efforts.iter().position(|effort| *effort == current) {
            Some(index) if index + 1 < efforts.len() => Some(efforts[index + 1].to_string()),
            _ => None,
        },
    };
}

fn handle_project_settings(app: &mut App, key: KeyCode) {
    if app.project_settings_editing {
        match key {
            KeyCode::Esc => {
                app.project_settings_editing = false;
                app.project_settings_input.clear();
            }
            KeyCode::Enter => commit_project_setting(app),
            KeyCode::Backspace => {
                app.project_settings_input.pop();
            }
            KeyCode::Char(character) => app.project_settings_input.push(character),
            _ => {}
        }
        return;
    }

    match key {
        KeyCode::Esc | KeyCode::Char('.') => save_and_close_project_settings(app),
        KeyCode::Up | KeyCode::Char('k') => {
            app.project_settings_selected = app.project_settings_selected.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.project_settings_selected < 1 {
                app.project_settings_selected += 1;
            }
        }
        KeyCode::Char(' ') => toggle_project_override(app),
        KeyCode::Enter => begin_project_edit(app),
        _ => {}
    }
}

fn toggle_project_override(app: &mut App) {
    let Some(settings) = app.project_settings.as_mut() else {
        return;
    };
    match app.project_settings_selected {
        0 => {
            if settings.branch_prefix_override().is_some() {
                settings.set_branch_prefix_override(None);
            } else {
                settings.set_branch_prefix_override(Some(
                    settings.effective_branch_prefix().to_string(),
                ));
            }
        }
        1 => {
            if settings.root_override().is_some() {
                settings.set_root_override(None);
            } else {
                settings.set_root_override(Some(settings.effective_root()));
            }
        }
        _ => {}
    }
}

fn begin_project_edit(app: &mut App) {
    let Some(settings) = app.project_settings.as_ref() else {
        return;
    };
    app.project_settings_input = match app.project_settings_selected {
        0 => settings.effective_branch_prefix().to_string(),
        1 => settings.effective_root().to_string_lossy().to_string(),
        _ => return,
    };
    app.project_settings_editing = true;
}

fn commit_project_setting(app: &mut App) {
    let value = app.project_settings_input.trim().to_string();
    let Some(settings) = app.project_settings.as_mut() else {
        return;
    };
    match app.project_settings_selected {
        0 => {
            if let Err(error) = worktree::validate_branch_prefix(&value) {
                app.status_message = Some(error.to_string());
                return;
            }
            settings.set_branch_prefix_override(Some(value));
        }
        1 => {
            if value.is_empty() {
                app.status_message = Some("Worktree root cannot be empty".to_string());
                return;
            }
            settings.set_root_override(Some(PathBuf::from(value)));
        }
        _ => return,
    }
    app.project_settings_editing = false;
    app.project_settings_input.clear();
}

fn save_and_close_project_settings(app: &mut App) {
    let Some(settings) = app.project_settings.as_ref() else {
        app.mode = Mode::Normal;
        return;
    };
    match settings.save() {
        Ok(()) => {
            app.project_settings = None;
            app.mode = Mode::Normal;
            app.status_message = Some("Project settings saved".to_string());
        }
        Err(error) => {
            app.status_message = Some(format!("Failed to save project settings: {error}"));
        }
    }
}

/// Create the worktree and attach it as a pane, rolling back if the pane cannot start.
///
/// Runs from the main loop rather than the key handler so the "creating…" notice is
/// already on screen before this blocks.
pub fn run_pending_worktree(app: &mut App, pending: PendingWorktree) {
    let PendingWorktree {
        project,
        branch,
        config,
    } = pending;
    let created = match worktree::create_managed_worktree(Path::new(&project), &branch, &config) {
        Ok(created) => created,
        Err(error) => {
            app.branch_config = None;
            app.status_message = Some(format!("Cannot create worktree: {error}"));
            return;
        }
    };

    let path = created.entry.path.clone();
    let title = branch.clone();
    match app.attach_new_session(&path.to_string_lossy(), title) {
        Ok(()) => {
            app.branch_config = None;
            app.status_message = match created.notice {
                Some(notice) => Some(format!("Isolated session on '{branch}' — {notice}")),
                None => Some(format!("Isolated session on '{branch}'")),
            };
        }
        Err(error) => {
            // The worktree exists but has no session; undo it rather than leaking one.
            let rollback = worktree::rollback_created_worktree(&created.entry);
            app.branch_config = None;
            app.status_message = Some(match rollback {
                Ok(()) => format!("Cannot start session: {error}; worktree rolled back"),
                Err(rollback_error) => format!(
                    "Cannot start session: {error}; worktree rollback also failed: {rollback_error}"
                ),
            });
        }
    }
}

fn handle_branch_name(app: &mut App, key: KeyCode) {
    match key {
        KeyCode::Esc => {
            app.branch_config = None;
            app.mode = Mode::Normal;
            app.status_message = Some("Isolated session cancelled".to_string());
        }
        KeyCode::Enter => {
            let Some(project) = app.active_project() else {
                app.mode = Mode::Normal;
                app.status_message = Some("Project filter was cleared".to_string());
                return;
            };
            if let Err(error) = worktree::validate_branch(Path::new(&project), &app.branch_input) {
                app.status_message = Some(error.to_string());
                return;
            }
            let Some(config) = app.branch_config.clone() else {
                app.mode = Mode::Normal;
                app.status_message = Some("Worktree configuration is unavailable".to_string());
                return;
            };
            if app.mux_enabled() {
                // Creating a worktree copies files and talks to Git, which can take a
                // few seconds. Hand it to the main loop so a progress notice is painted
                // before we block.
                app.pending_worktree = Some(PendingWorktree {
                    project,
                    branch: app.branch_input.clone(),
                    config,
                });
                app.mode = Mode::Normal;
                app.status_message = Some(format!("Creating worktree for '{}'…", app.branch_input));
                return;
            }
            app.should_new_session = Some(NewSessionRequest::Worktree {
                source_project: project,
                branch: app.branch_input.clone(),
                config,
            });
        }
        KeyCode::Backspace => {
            app.branch_input.pop();
        }
        KeyCode::Char(character) => app.branch_input.push(character),
        _ => {}
    }
}

/// Load details for the currently selected session if not already loaded.
pub fn maybe_load_details(app: &mut App) {
    if let Some(session) = app.selected_session() {
        let id = session.id.clone();
        if app.detail_loaded_for.as_deref() != Some(&id) {
            if let Some(idx) = app.selected_real_index() {
                let _ = loader::load_session_details(&mut app.sessions[idx]);
                app.detail_loaded_for = Some(id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelling_inherited_project_edits_does_not_create_overrides() {
        let temp = tempfile::tempdir().unwrap();
        let mut global = config::UserConfig::default();
        global.worktree.branch_prefix = "global/".to_string();
        global.worktree.root = temp.path().join("global-worktrees");
        let mut app = App::new(Vec::new(), global.clone());
        app.project_settings = Some(config::ProjectSettings::load(temp.path(), &global).unwrap());

        for selected in 0..=1 {
            app.project_settings_selected = selected;
            begin_project_edit(&mut app);
            assert!(app.project_settings_editing);
            handle_project_settings(&mut app, KeyCode::Esc);
        }

        let settings = app.project_settings.as_ref().unwrap();
        assert!(settings.branch_prefix_override().is_none());
        assert!(settings.root_override().is_none());
        settings.save().unwrap();
        assert!(!temp.path().join(".cst.json").exists());
    }

    #[test]
    fn new_session_uses_cwd_project_without_a_filter() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join(".git")).unwrap();
        let mut app = App::new(Vec::new(), config::UserConfig::default());
        app.set_cwd_context(temp.path().to_string_lossy().to_string(), false);
        assert!(app.project_filter.is_none());

        handle_normal(&mut app, KeyCode::Char('n'));

        match app.should_new_session {
            Some(NewSessionRequest::Normal { ref cwd }) => {
                assert_eq!(cwd, app.cwd_project.as_ref().unwrap());
            }
            other => panic!("expected a normal new session request, got {other:?}"),
        }
    }

    #[test]
    fn cwd_project_is_offered_in_the_project_list_and_auto_filtered() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join(".git")).unwrap();
        let mut app = App::new(Vec::new(), config::UserConfig::default());

        app.set_cwd_context(temp.path().to_string_lossy().to_string(), true);

        let project = app.cwd_project.clone().unwrap();
        assert_eq!(app.project_filter.as_ref(), Some(&project));
        assert!(app.unique_projects.contains(&project));
    }
}
