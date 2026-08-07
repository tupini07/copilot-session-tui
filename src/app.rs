use crate::config::{EffectiveWorktreeConfig, ProjectSettings, UserConfig};
use crate::mux::{KeyChord, MuxState, Pane, PaneSpec};
use crate::session::manager;
use crate::session::worktree::ManagedWorktree;
use crate::session::Session;
use crate::updater::UpdateInfo;
use anyhow::Result;
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use std::path::PathBuf;
use std::sync::mpsc;

/// Which surface the user is looking at: the session list, or a live session pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    List,
    Attached(crate::mux::PaneId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Search,
    Rename,
    ConfirmDelete,
    ConfirmForceDelete,
    FilterProject,
    Help,
    Settings,
    ProjectSettings,
    BranchName,
    /// Pane switcher opened with `prefix w`.
    PaneList,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortField {
    LastUsed,
    Created,
    Name,
    Project,
}

#[derive(Debug, Clone)]
pub enum NewSessionRequest {
    Normal {
        cwd: String,
    },
    Worktree {
        source_project: String,
        branch: String,
        config: EffectiveWorktreeConfig,
    },
}

#[derive(Debug, Clone)]
pub enum DeleteTarget {
    SessionOnly,
    Managed { entry: ManagedWorktree, dirty: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsEditField {
    Model,
    BranchPrefix,
    WorktreeRoot,
    MuxPrefix,
}

pub struct App {
    pub sessions: Vec<Session>,
    pub filtered_indices: Vec<usize>,
    pub selected: usize,
    pub scroll_offset: usize,
    pub mode: Mode,
    pub search_query: String,
    pub rename_input: String,
    pub project_filter: Option<String>,
    /// Project root of the directory `cst` was launched from (git-aware).
    /// `None` when the launch directory is not inside a Git repository.
    pub cwd_project: Option<String>,
    /// Raw directory `cst` was launched from.
    pub cwd: Option<String>,
    pub unique_projects: Vec<String>,
    pub project_selected: usize,
    pub project_scroll_offset: usize,
    pub project_visible_rows: usize,
    pub project_search_query: String,
    pub sort_field: SortField,
    pub detail_loaded_for: Option<String>,
    pub should_quit: bool,
    pub should_resume: Option<(String, String)>, // (session_id, cwd)
    pub should_new_session: Option<NewSessionRequest>,
    pub status_message: Option<String>,
    pub visible_rows: usize,
    pub update_info: Option<UpdateInfo>,
    pub update_receiver: Option<mpsc::Receiver<Option<UpdateInfo>>>,
    pub should_update: bool,
    pub config: UserConfig,
    pub settings_selected: usize,
    pub settings_editing: Option<SettingsEditField>,
    pub settings_input: String,
    pub project_settings: Option<ProjectSettings>,
    pub project_settings_selected: usize,
    pub project_settings_editing: bool,
    pub project_settings_input: String,
    pub branch_input: String,
    pub branch_config: Option<EffectiveWorktreeConfig>,
    pub pending_delete: Option<DeleteTarget>,
    /// Present only when multiplexing is enabled; owns every live pane.
    pub mux: Option<MuxState>,
    pub view: View,
    /// Rows/cols available to a pane, kept in sync with the terminal size.
    pub pane_size: (u16, u16),
    /// Set when the user tries to quit while panes are still running.
    pub confirm_quit: bool,
    /// Highlighted row in the pane switcher.
    pub pane_selected: usize,
    /// The `mux` value stored on disk, so a `--mux` / `--no-mux` override for this
    /// invocation is never accidentally persisted by the settings popup.
    pub mux_on_disk: bool,
    /// Directory of the last focused pane, captured before panes are torn down so the
    /// shell wrapper can still auto-`cd` there.
    pub exit_dir: Option<String>,
}

impl App {
    pub fn new(sessions: Vec<Session>, config: UserConfig) -> Self {
        let unique_projects = extract_unique_projects(&sessions);
        let filtered_indices: Vec<usize> = (0..sessions.len()).collect();
        let mux_on_disk = config.mux;

        // An unparseable prefix falls back to the default rather than refusing to start.
        let mux = config.mux.then(|| {
            let prefix = KeyChord::parse(&config.mux_prefix)
                .or_else(|| KeyChord::parse(crate::config::DEFAULT_MUX_PREFIX))
                .expect("the default mux prefix must parse");
            MuxState::new(prefix)
        });

        App {
            sessions,
            filtered_indices,
            selected: 0,
            scroll_offset: 0,
            mode: Mode::Normal,
            search_query: String::new(),
            rename_input: String::new(),
            project_filter: None,
            cwd_project: None,
            cwd: None,
            unique_projects,
            project_selected: 0,
            project_scroll_offset: 0,
            project_visible_rows: 10,
            project_search_query: String::new(),
            sort_field: SortField::LastUsed,
            detail_loaded_for: None,
            should_quit: false,
            should_resume: None,
            should_new_session: None,
            status_message: None,
            visible_rows: 20,
            update_info: None,
            update_receiver: None,
            should_update: false,
            config,
            settings_selected: 0,
            settings_editing: None,
            settings_input: String::new(),
            project_settings: None,
            project_settings_selected: 0,
            project_settings_editing: false,
            project_settings_input: String::new(),
            branch_input: String::new(),
            branch_config: None,
            pending_delete: None,
            mux,
            view: View::List,
            pane_size: (24, 80),
            confirm_quit: false,
            pane_selected: 0,
            mux_on_disk,
            exit_dir: None,
        }
    }

    /// Config as it should be written to disk, with any per-invocation override undone.
    pub fn persistable_config(&self) -> UserConfig {
        let mut config = self.config.clone();
        config.mux = self.mux_on_disk;
        config
    }

    /// Open the pane switcher, pre-selecting the currently focused pane.
    pub fn open_pane_list(&mut self) {
        let Some(mux) = self.mux.as_ref() else {
            return;
        };
        if mux.panes.is_empty() {
            self.status_message = Some("No running sessions".to_string());
            return;
        }
        self.pane_selected = mux
            .focused
            .and_then(|id| mux.panes.iter().position(|pane| pane.id == id))
            .unwrap_or(0);
        self.view = View::List;
        self.mode = Mode::PaneList;
    }

    pub fn mux_enabled(&self) -> bool {
        self.mux.is_some()
    }

    /// Status message that also warns when the prefix key had to be defaulted.
    pub fn prefix_label(&self) -> Option<String> {
        self.mux.as_ref().map(|mux| mux.prefix.label())
    }

    pub fn selected_session(&self) -> Option<&Session> {
        self.filtered_indices
            .get(self.selected)
            .and_then(|&idx| self.sessions.get(idx))
    }

    pub fn selected_real_index(&self) -> Option<usize> {
        self.filtered_indices.get(self.selected).copied()
    }

    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            if self.selected < self.scroll_offset {
                self.scroll_offset = self.selected;
            }
        }
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.filtered_indices.len() {
            self.selected += 1;
            if self.selected >= self.scroll_offset + self.visible_rows {
                self.scroll_offset = self.selected - self.visible_rows + 1;
            }
        }
    }

    pub fn apply_filter(&mut self) {
        let matcher = SkimMatcherV2::default();

        self.filtered_indices = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                // Project filter
                if let Some(ref proj) = self.project_filter {
                    if !s.project_root.eq_ignore_ascii_case(proj) {
                        return false;
                    }
                }
                // Search filter
                if !self.search_query.is_empty() {
                    let haystack =
                        format!("{} {} {} {}", s.display_name(), s.project_root, s.cwd, s.id);
                    return matcher.fuzzy_match(&haystack, &self.search_query).is_some();
                }
                true
            })
            .map(|(i, _)| i)
            .collect();

        // Reset selection if out of bounds
        if self.selected >= self.filtered_indices.len() {
            self.selected = self.filtered_indices.len().saturating_sub(1);
        }
        self.scroll_offset = 0;
    }

    pub fn cycle_sort(&mut self) {
        self.sort_field = match self.sort_field {
            SortField::LastUsed => SortField::Created,
            SortField::Created => SortField::Name,
            SortField::Name => SortField::Project,
            SortField::Project => SortField::LastUsed,
        };
        self.sort_sessions();
    }

    fn sort_sessions(&mut self) {
        match self.sort_field {
            SortField::LastUsed => {
                self.sessions.sort_by(|a, b| {
                    let at = a.updated_at.or(a.created_at);
                    let bt = b.updated_at.or(b.created_at);
                    bt.cmp(&at)
                });
            }
            SortField::Created => {
                self.sessions
                    .sort_by(|a, b| b.created_at.cmp(&a.created_at));
            }
            SortField::Name => {
                self.sessions.sort_by(|a, b| {
                    a.display_name()
                        .to_lowercase()
                        .cmp(&b.display_name().to_lowercase())
                });
            }
            SortField::Project => {
                self.sessions
                    .sort_by(|a, b| a.project_root.cmp(&b.project_root));
            }
        }
        self.apply_filter();
    }

    /// Returns indices into `unique_projects` that match the current project search query.
    pub fn filtered_project_indices(&self) -> Vec<usize> {
        if self.project_search_query.is_empty() {
            return (0..self.unique_projects.len()).collect();
        }
        let matcher = SkimMatcherV2::default();
        self.unique_projects
            .iter()
            .enumerate()
            .filter(|(_, project)| {
                let short_name = std::path::Path::new(project)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(project);
                matcher
                    .fuzzy_match(short_name, &self.project_search_query)
                    .is_some()
            })
            .map(|(i, _)| i)
            .collect()
    }

    pub fn set_project_filter(&mut self, project: Option<String>) {
        self.project_filter = project;
        self.apply_filter();
    }

    /// Record the directory `cst` was launched from. When that directory belongs to a
    /// Git project it becomes selectable in the project filter even if it has no
    /// sessions yet, and is auto-selected as the current filter.
    pub fn set_cwd_context(&mut self, cwd: String, auto_filter: bool) {
        let project = crate::session::loader::detect_project_root(&cwd);
        self.cwd = Some(cwd);

        let Some(project) = project else {
            return;
        };

        let known = self
            .unique_projects
            .iter()
            .any(|p| p.eq_ignore_ascii_case(&project));
        if !known {
            self.unique_projects.insert(0, project.clone());
        }
        self.cwd_project = Some(project.clone());

        if auto_filter {
            self.set_project_filter(Some(project));
        }
    }

    /// Project used by project-scoped actions: the active filter, falling back to the
    /// project of the launch directory.
    pub fn active_project(&self) -> Option<String> {
        self.project_filter
            .clone()
            .or_else(|| self.cwd_project.clone())
    }

    /// Directory to start a plain new session in.
    pub fn new_session_dir(&self) -> Option<String> {
        self.active_project().or_else(|| self.cwd.clone())
    }

    /// Attach an existing Copilot session as a pane, or focus it if already attached.
    pub fn attach_session(&mut self, session_id: &str, cwd: &str, title: String) -> Result<()> {
        // A pane that already exited is not worth re-focusing; drop it and resume afresh.
        if let Some(mux) = self.mux.as_mut() {
            if let Some(existing) = mux.pane_for_session(session_id) {
                let running = mux.pane(existing).is_some_and(|pane| pane.is_running());
                if running {
                    mux.focused = Some(existing);
                    self.view = View::Attached(existing);
                    return Ok(());
                }
                mux.remove(existing);
            }
        }
        let (program, args) = manager::resume_command(session_id, &self.config)?;
        self.spawn_pane(
            title,
            PathBuf::from(cwd),
            Some(session_id.to_string()),
            program,
            args,
        )
    }

    /// Start a brand new Copilot session as a pane.
    pub fn attach_new_session(&mut self, cwd: &str, title: String) -> Result<()> {
        let (program, args) = manager::new_session_command(&self.config)?;
        self.spawn_pane(title, PathBuf::from(cwd), None, program, args)
    }

    fn spawn_pane(
        &mut self,
        title: String,
        cwd: PathBuf,
        session_id: Option<String>,
        program: String,
        args: Vec<String>,
    ) -> Result<()> {
        let (rows, cols) = self.pane_size;
        let Some(mux) = self.mux.as_mut() else {
            anyhow::bail!("Multiplexing is disabled");
        };
        let id = mux.allocate_id();
        let pane = Pane::spawn(
            PaneSpec {
                id,
                title,
                cwd,
                session_id,
                program,
                args,
            },
            rows,
            cols,
            mux.events.clone(),
        )?;
        mux.push(pane);
        self.view = View::Attached(id);
        Ok(())
    }

    /// Leave the focused pane running and return to the session list.
    pub fn detach(&mut self) {
        self.view = View::List;
        if let Some(mux) = self.mux.as_mut() {
            mux.prefix_pending = false;
        }
    }

    /// Panes owned by this instance, for marking rows in the session list.
    pub fn pane_for_session(&self, session_id: &str) -> Option<crate::mux::PaneId> {
        self.mux
            .as_ref()
            .and_then(|mux| mux.pane_for_session(session_id))
    }

    /// Whether a session is live in one of our panes. Exited panes are not marked, since
    /// the session is resumable again.
    pub fn has_running_pane_for(&self, session_id: &str) -> bool {
        self.mux.as_ref().is_some_and(|mux| {
            mux.pane_for_session(session_id)
                .and_then(|id| mux.pane(id))
                .is_some_and(|pane| pane.is_running())
        })
    }

    pub fn sort_label(&self) -> &str {
        match self.sort_field {
            SortField::LastUsed => "Last Used",
            SortField::Created => "Created",
            SortField::Name => "Name",
            SortField::Project => "Project",
        }
    }

    pub fn poll_update(&mut self) {
        if self.update_info.is_some() {
            return;
        }
        if let Some(ref rx) = self.update_receiver {
            if let Ok(result) = rx.try_recv() {
                self.update_info = result;
                self.update_receiver = None;
            }
        }
    }
}

fn extract_unique_projects(sessions: &[Session]) -> Vec<String> {
    use chrono::{DateTime, Utc};
    // Track the most recent updated_at per project (using resolved project_root)
    let mut latest: std::collections::HashMap<String, DateTime<Utc>> =
        std::collections::HashMap::new();
    for s in sessions {
        if s.project_root.is_empty() {
            continue;
        }
        if let Some(updated) = s.updated_at {
            let entry = latest.entry(s.project_root.clone()).or_insert(updated);
            if updated > *entry {
                *entry = updated;
            }
        } else {
            latest
                .entry(s.project_root.clone())
                .or_insert_with(|| DateTime::<Utc>::MIN_UTC);
        }
    }
    let mut projects: Vec<String> = latest.keys().cloned().collect();
    projects.sort_by(|a, b| latest[b].cmp(&latest[a])); // most recent first
    projects
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with(mux: bool) -> App {
        let config = UserConfig {
            mux,
            ..UserConfig::default()
        };
        App::new(Vec::new(), config)
    }

    #[test]
    fn cli_override_is_not_persisted_by_settings() {
        // Simulates `--mux` on a machine whose config file says mux is off.
        let mut app = app_with(true);
        app.mux_on_disk = false;

        assert!(app.mux_enabled());
        assert!(!app.persistable_config().mux);
    }

    #[test]
    fn settings_toggle_is_persisted() {
        let mut app = app_with(false);
        app.mux_on_disk = true;
        assert!(app.persistable_config().mux);
    }

    #[test]
    fn pane_list_reports_when_there_is_nothing_to_switch_to() {
        let mut app = app_with(true);
        app.open_pane_list();
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.status_message.is_some());
    }

    #[test]
    fn pane_list_is_inert_without_the_multiplexer() {
        let mut app = app_with(false);
        app.open_pane_list();
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.status_message.is_none());
    }
}
