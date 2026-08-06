use crate::app::{App, DeleteTarget, Mode, NewSessionRequest, SettingsEditField};
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

    let Event::Key(key) = event::read()? else {
        return Ok(false);
    };

    if key.kind != KeyEventKind::Press {
        return Ok(false);
    }

    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.should_quit = true;
        return Ok(true);
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
    }

    Ok(true)
}

fn handle_normal(app: &mut App, key: KeyCode) {
    match key {
        KeyCode::Char('q') | KeyCode::Esc => {
            app.should_quit = true;
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
        KeyCode::Enter => {
            if let Some(session) = app.selected_session() {
                if session.is_active {
                    app.status_message =
                        Some("Cannot resume: session is already active".to_string());
                } else {
                    app.should_resume = Some((session.id.clone(), session.cwd.clone()));
                }
            }
        }
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
            if let Some(ref project) = app.project_filter {
                app.should_new_session = Some(NewSessionRequest::Normal {
                    cwd: project.clone(),
                });
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

fn begin_worktree_session(app: &mut App) {
    let Some(project) = app.project_filter.clone() else {
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
    let Some(project) = app.project_filter.clone() else {
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
    let target = app
        .pending_delete
        .clone()
        .unwrap_or(DeleteTarget::SessionOnly);
    let result = match target {
        DeleteTarget::SessionOnly => {
            manager::delete_session(&dir).map(|_| "Session deleted".to_string())
        }
        DeleteTarget::Managed { entry, .. } => manager::delete_managed_session(&dir, &entry, force),
    };

    match result {
        Ok(message) => {
            app.sessions.remove(idx);
            app.apply_filter();
            app.status_message = Some(message);
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

const SETTINGS_COUNT: usize = 5;

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
        KeyCode::Esc | KeyCode::Char(',') => match config::save(&app.config) {
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
    }
    app.settings_editing = None;
    app.settings_input.clear();
    if let Err(error) = config::save(&app.config) {
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

fn handle_branch_name(app: &mut App, key: KeyCode) {
    match key {
        KeyCode::Esc => {
            app.branch_config = None;
            app.mode = Mode::Normal;
            app.status_message = Some("Isolated session cancelled".to_string());
        }
        KeyCode::Enter => {
            let Some(project) = app.project_filter.clone() else {
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
}
