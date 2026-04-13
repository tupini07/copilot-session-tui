use crate::app::{App, Mode};
use crate::config;
use crate::session::loader;
use crate::session::manager;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

pub fn handle_input(app: &mut App) -> anyhow::Result<bool> {
    if !event::poll(std::time::Duration::from_millis(100))? {
        return Ok(false);
    }

    let Event::Key(key) = event::read()? else {
        return Ok(false);
    };

    // Only handle key press events (ignore Release/Repeat to avoid double input on Windows)
    if key.kind != KeyEventKind::Press {
        return Ok(false);
    }

    // Ctrl+C always quits
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.should_quit = true;
        return Ok(true);
    }

    match app.mode {
        Mode::Normal => handle_normal(app, key.code),
        Mode::Search => handle_search(app, key.code),
        Mode::Rename => handle_rename(app, key.code),
        Mode::ConfirmDelete => handle_confirm_delete(app, key.code),
        Mode::FilterProject => handle_filter_project(app, key.code),
        Mode::Help => handle_help(app, key.code),
        Mode::Settings => handle_settings(app, key.code),
    }

    Ok(true)
}

fn handle_normal(app: &mut App, key: KeyCode) {
    match key {
        KeyCode::Char('q') | KeyCode::Esc => {
            app.should_quit = true;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.move_up();
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.move_down();
        }
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
        KeyCode::Char('d') => {
            if app.selected_session().is_some() {
                app.mode = Mode::ConfirmDelete;
            }
        }
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
            // Clear project filter
            app.set_project_filter(None);
            app.status_message = Some("Filter cleared".to_string());
        }
        KeyCode::Char('n') => {
            // Start new session in filtered project directory
            if let Some(ref project) = app.project_filter {
                app.should_new_session = Some(project.clone());
            } else {
                app.status_message = Some("Filter by a project first (f) to start a new session".to_string());
            }
        }
        KeyCode::Char('?') => {
            app.mode = Mode::Help;
        }
        KeyCode::Char(',') => {
            app.settings_selected = 0;
            app.settings_editing_model = false;
            app.settings_model_input = app.config.model.clone().unwrap_or_default();
            app.mode = Mode::Settings;
        }
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

fn handle_search(app: &mut App, key: KeyCode) {
    match key {
        KeyCode::Esc => {
            app.mode = Mode::Normal;
            app.search_query.clear();
            app.apply_filter();
        }
        KeyCode::Enter => {
            app.mode = Mode::Normal;
            // keep filter active
        }
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
        KeyCode::Esc => {
            app.mode = Mode::Normal;
        }
        KeyCode::Enter => {
            if let Some(idx) = app.selected_real_index() {
                let dir = app.sessions[idx].dir_path.clone();
                let new_name = app.rename_input.clone();
                match manager::rename_session(&dir, &new_name) {
                    Ok(()) => {
                        app.sessions[idx].summary = Some(new_name);
                        app.status_message = Some("Session renamed".to_string());
                    }
                    Err(e) => {
                        app.status_message = Some(format!("Rename failed: {}", e));
                    }
                }
            }
            app.mode = Mode::Normal;
        }
        KeyCode::Backspace => {
            app.rename_input.pop();
        }
        KeyCode::Char(c) => {
            app.rename_input.push(c);
        }
        _ => {}
    }
}

fn handle_confirm_delete(app: &mut App, key: KeyCode) {
    match key {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            if let Some(idx) = app.selected_real_index() {
                let dir = app.sessions[idx].dir_path.clone();
                match manager::delete_session(&dir) {
                    Ok(()) => {
                        app.sessions.remove(idx);
                        app.apply_filter();
                        app.status_message = Some("Session deleted".to_string());
                    }
                    Err(e) => {
                        app.status_message = Some(format!("Delete failed: {}", e));
                    }
                }
            }
            app.mode = Mode::Normal;
        }
        _ => {
            app.mode = Mode::Normal;
            app.status_message = Some("Delete cancelled".to_string());
        }
    }
}

fn handle_filter_project(app: &mut App, key: KeyCode) {
    let filtered = app.filtered_project_indices();
    let has_all_option = app.project_search_query.is_empty();
    let total = filtered.len() + if has_all_option { 1 } else { 0 };

    match key {
        KeyCode::Esc => {
            app.mode = Mode::Normal;
        }
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
                // No matches, do nothing
            } else if has_all_option && app.project_selected == 0 {
                app.set_project_filter(None);
                app.status_message = Some("Showing all projects".to_string());
                app.mode = Mode::Normal;
            } else {
                let list_idx = if has_all_option {
                    app.project_selected - 1
                } else {
                    app.project_selected
                };
                if list_idx < filtered.len() {
                    let proj_idx = filtered[list_idx];
                    let project = app.unique_projects[proj_idx].clone();
                    app.status_message = Some(format!("Filtered to: {}", project));
                    app.set_project_filter(Some(project));
                }
                app.mode = Mode::Normal;
            }
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
    match key {
        KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') | KeyCode::Enter => {
            app.mode = Mode::Normal;
        }
        _ => {}
    }
}

// Settings rows: 0 = Yolo, 1 = Model, 2 = Reasoning Effort
const SETTINGS_COUNT: usize = 3;

fn handle_settings(app: &mut App, key: KeyCode) {
    // If editing the model field, handle text input
    if app.settings_editing_model {
        match key {
            KeyCode::Esc => {
                app.settings_editing_model = false;
            }
            KeyCode::Enter => {
                let val = app.settings_model_input.trim().to_string();
                app.config.model = if val.is_empty() { None } else { Some(val) };
                app.settings_editing_model = false;
                let _ = config::save(&app.config);
            }
            KeyCode::Backspace => {
                app.settings_model_input.pop();
            }
            KeyCode::Char(c) => {
                app.settings_model_input.push(c);
            }
            _ => {}
        }
        return;
    }

    match key {
        KeyCode::Esc | KeyCode::Char(',') => {
            let _ = config::save(&app.config);
            app.mode = Mode::Normal;
            app.status_message = Some("Settings saved".to_string());
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if app.settings_selected > 0 {
                app.settings_selected -= 1;
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.settings_selected + 1 < SETTINGS_COUNT {
                app.settings_selected += 1;
            }
        }
        KeyCode::Enter | KeyCode::Char(' ') => {
            match app.settings_selected {
                0 => {
                    // Toggle yolo
                    app.config.yolo = !app.config.yolo;
                    let _ = config::save(&app.config);
                }
                1 => {
                    // Enter model editing mode
                    app.settings_model_input = app.config.model.clone().unwrap_or_default();
                    app.settings_editing_model = true;
                }
                2 => {
                    // Cycle reasoning effort: None → low → medium → high → xhigh → None
                    let efforts = config::REASONING_EFFORTS;
                    app.config.reasoning_effort = match &app.config.reasoning_effort {
                        None => Some(efforts[0].to_string()),
                        Some(current) => {
                            match efforts.iter().position(|&e| e == current.as_str()) {
                                Some(i) if i + 1 < efforts.len() => {
                                    Some(efforts[i + 1].to_string())
                                }
                                _ => None,
                            }
                        }
                    };
                    let _ = config::save(&app.config);
                }
                _ => {}
            }
        }
        _ => {}
    }
}

/// Load details for the currently selected session if not already loaded
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
