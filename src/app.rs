use crate::config::{self, EffectiveWorktreeConfig, ProjectSettings, UserConfig};
use crate::mux::{KeyChord, MuxState, Pane, PaneSpec};
use crate::scratchpad::Scratchpad;
use crate::session::manager;
use crate::session::worktree::ManagedWorktree;
use crate::session::Session;
use crate::terminal_pane::TerminalManager;
use crate::updater::UpdateInfo;
use crate::workspace_state;
use anyhow::Result;
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use ratatui::layout::Rect;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc;

/// Which surface the user is looking at: the session list, or a live session pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    List,
    Attached(crate::mux::PaneId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceFocus {
    Chat,
    Scratchpad,
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceHelp {
    Scratchpad,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorkspaceAreas {
    pub chat: Rect,
    pub scratchpad: Option<Rect>,
    pub terminal: Option<Rect>,
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
    Scratchpad,
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

/// A worktree creation deferred to the main loop so a progress notice can be drawn first.
#[derive(Debug, Clone)]
pub struct PendingWorktree {
    pub project: String,
    pub branch: String,
    pub config: EffectiveWorktreeConfig,
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
    TerminalShell,
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
    pub copilot_home: PathBuf,
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
    pub pane_origin: (u16, u16),
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
    /// A worktree the main loop should create on its next iteration.
    pub pending_worktree: Option<PendingWorktree>,
    pub scratchpad: Option<Scratchpad>,
    pub scratchpad_owner: Option<crate::mux::PaneId>,
    pub scratchpad_open: HashSet<crate::mux::PaneId>,
    pub terminal: TerminalManager,
    pub terminal_owner: Option<crate::mux::PaneId>,
    pub terminal_open: HashSet<crate::mux::PaneId>,
    pub workspace_focus: WorkspaceFocus,
    pub workspace_help: Option<WorkspaceHelp>,
    pub workspace_areas: WorkspaceAreas,
    pub host_sequences: Vec<Vec<u8>>,
    workspace_state_enabled: bool,
    workspace_state_root: PathBuf,
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

        let mut app = App {
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
            copilot_home: crate::session::loader::copilot_home(),
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
            pane_origin: (0, 0),
            confirm_quit: false,
            pane_selected: 0,
            mux_on_disk,
            exit_dir: None,
            pending_worktree: None,
            scratchpad: None,
            scratchpad_owner: None,
            scratchpad_open: HashSet::new(),
            terminal: TerminalManager::default(),
            terminal_owner: None,
            terminal_open: HashSet::new(),
            workspace_focus: WorkspaceFocus::Chat,
            workspace_help: None,
            workspace_areas: WorkspaceAreas::default(),
            host_sequences: Vec::new(),
            workspace_state_enabled: true,
            workspace_state_root: workspace_state::workspace_root(),
        };
        app.apply_filter();
        app
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
        self.workspace_focus = WorkspaceFocus::Chat;
        self.terminal.unfocus();
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

    /// True while the prefix has been pressed and a command key is awaited.
    pub fn prefix_pending(&self) -> bool {
        self.mux
            .as_ref()
            .is_some_and(|mux| mux.prefix_pending || mux.help_pending)
    }

    pub fn help_pending(&self) -> bool {
        self.mux.as_ref().is_some_and(|mux| mux.help_pending)
    }

    /// Live panes owned by this instance, for the session list footer.
    pub fn running_pane_count(&self) -> usize {
        self.mux
            .as_ref()
            .map(|mux| mux.panes.iter().filter(|pane| pane.is_running()).count())
            .unwrap_or(0)
    }

    pub fn attached_scratchpad_visible(&self) -> bool {
        matches!(self.view, View::Attached(id) if self.scratchpad_owner == Some(id)
            && self.scratchpad_open.contains(&id))
            && self.scratchpad.is_some()
    }

    pub fn attached_terminal_visible(&self) -> bool {
        matches!(self.view, View::Attached(id) if self.terminal_owner == Some(id)
            && self.terminal_open.contains(&id))
            && self.terminal.is_visible()
    }

    pub fn collapse_stopped_terminals(&mut self) {
        let stopped: HashSet<String> = self.terminal.stopped_session_ids().into_iter().collect();
        if stopped.is_empty() {
            return;
        }

        let open_stopped_panels: Vec<(crate::mux::PaneId, String)> = self
            .mux
            .as_ref()
            .map(|mux| {
                mux.panes
                    .iter()
                    .filter_map(|pane| {
                        let session_id = pane.session_id.as_ref()?;
                        (self.terminal_open.contains(&pane.id) && stopped.contains(session_id))
                            .then(|| (pane.id, session_id.clone()))
                    })
                    .collect()
            })
            .unwrap_or_default();

        for (pane_id, session_id) in &open_stopped_panels {
            self.remember_terminal_panel(*pane_id, session_id, false);
        }

        let active_stopped = self
            .terminal
            .active_session_id()
            .is_some_and(|session_id| stopped.contains(session_id));
        if active_stopped {
            self.terminal.hide();
            self.terminal_owner = None;
            if self.workspace_focus == WorkspaceFocus::Terminal {
                self.workspace_focus = WorkspaceFocus::Chat;
            }
        }
    }

    pub fn forget_workspace_panels(&mut self, pane_id: crate::mux::PaneId) -> bool {
        if self.scratchpad_owner == Some(pane_id) {
            if let Some(error) = self
                .scratchpad
                .as_mut()
                .and_then(|scratchpad| scratchpad.save().err())
            {
                self.status_message = Some(format!("Scratchpad save failed: {error}"));
                return false;
            }
            self.scratchpad = None;
            self.scratchpad_owner = None;
        }
        self.scratchpad_open.remove(&pane_id);

        if self.terminal_owner == Some(pane_id) {
            self.terminal.hide();
            self.terminal_owner = None;
        }
        self.terminal_open.remove(&pane_id);
        true
    }

    pub fn remember_scratchpad_panel(
        &mut self,
        pane_id: crate::mux::PaneId,
        session_id: &str,
        open: bool,
    ) {
        if open {
            self.scratchpad_open.insert(pane_id);
        } else {
            self.scratchpad_open.remove(&pane_id);
        }
        if self.workspace_state_enabled {
            if let Err(error) = workspace_state::set_scratchpad_open_in(
                &self.workspace_state_root,
                session_id,
                open,
            ) {
                self.status_message = Some(format!("Cannot save workspace state: {error}"));
            }
        }
    }

    pub fn remember_terminal_panel(
        &mut self,
        pane_id: crate::mux::PaneId,
        session_id: &str,
        open: bool,
    ) {
        if open {
            self.terminal_open.insert(pane_id);
        } else {
            self.terminal_open.remove(&pane_id);
        }
        if self.workspace_state_enabled {
            if let Err(error) =
                workspace_state::set_terminal_open_in(&self.workspace_state_root, session_id, open)
            {
                self.status_message = Some(format!("Cannot save workspace state: {error}"));
            }
        }
    }

    fn restore_workspace_panels(&mut self, pane_id: crate::mux::PaneId, session_id: &str) {
        if !self.workspace_state_enabled {
            return;
        }
        match workspace_state::load_in(&self.workspace_state_root, session_id) {
            Ok(state) => {
                if state.scratchpad_open {
                    self.scratchpad_open.insert(pane_id);
                }
                if state.terminal_open {
                    self.terminal_open.insert(pane_id);
                }
            }
            Err(error) => {
                self.status_message = Some(format!("Cannot restore workspace state: {error}"));
            }
        }
    }

    #[cfg(test)]
    pub fn disable_workspace_state_persistence(&mut self) {
        self.workspace_state_enabled = false;
    }

    #[cfg(test)]
    fn set_workspace_state_root(&mut self, root: PathBuf) {
        self.workspace_state_root = root;
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

        if self.project_filter.is_none() && self.search_query.is_empty() {
            self.filtered_indices
                .sort_by_key(|&index| !self.config.favorites.contains(&self.sessions[index].id));
        }

        // Reset selection if out of bounds
        if self.selected >= self.filtered_indices.len() {
            self.selected = self.filtered_indices.len().saturating_sub(1);
        }
        self.scroll_offset = 0;
    }

    pub fn is_favorite(&self, session_id: &str) -> bool {
        self.config.favorites.contains(session_id)
    }

    pub fn toggle_selected_favorite(&mut self) -> anyhow::Result<Option<bool>> {
        let Some(session_id) = self.selected_session().map(|session| session.id.clone()) else {
            return Ok(None);
        };
        let was_favorite = self.config.favorites.contains(&session_id);

        if was_favorite {
            self.config.favorites.remove(&session_id);
        } else {
            self.config.favorites.insert(session_id.clone());
        }

        if let Err(error) = config::save(&self.persistable_config()) {
            if was_favorite {
                self.config.favorites.insert(session_id);
            } else {
                self.config.favorites.remove(&session_id);
            }
            return Err(error);
        }

        self.apply_filter();
        if let Some(display_index) = self
            .filtered_indices
            .iter()
            .position(|&index| self.sessions[index].id == session_id)
        {
            self.selected = display_index;
            if self.selected >= self.visible_rows {
                self.scroll_offset = self.selected - self.visible_rows + 1;
            }
        }

        Ok(Some(!was_favorite))
    }

    pub fn forget_favorite(&mut self, session_id: &str) -> anyhow::Result<bool> {
        if !self.config.favorites.remove(session_id) {
            return Ok(false);
        }
        if let Err(error) = config::save(&self.persistable_config()) {
            self.config.favorites.insert(session_id.to_string());
            return Err(error);
        }
        Ok(true)
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
        // Sessions with no name yet would otherwise render as a blank tab and produce
        // messages like "Session '' exited".
        let title = match title.trim() {
            "" => format!("session {id}"),
            trimmed => trimmed.to_string(),
        };
        let workspace_session_id = session_id.clone();
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
        if let Some(session_id) = workspace_session_id.as_deref() {
            self.restore_workspace_panels(id, session_id);
        }
        Ok(())
    }

    /// Leave the focused pane running and return to the session list.
    pub fn detach(&mut self) {
        if let Some(error) = self
            .scratchpad
            .as_mut()
            .and_then(|scratchpad| scratchpad.save().err())
        {
            self.status_message = Some(format!("Scratchpad save failed: {error}"));
        }
        self.terminal.unfocus();
        self.workspace_focus = WorkspaceFocus::Chat;
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
    use chrono::{DateTime, Utc};

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

    #[test]
    fn panel_preferences_restore_for_a_new_pane_after_restart() {
        let temp = tempfile::tempdir().unwrap();
        let session_id = "workspace-restart-session";

        let mut first_run = app_with(true);
        first_run.set_workspace_state_root(temp.path().to_path_buf());
        first_run.remember_scratchpad_panel(1, session_id, true);
        first_run.remember_terminal_panel(1, session_id, true);

        let mut restarted = app_with(true);
        restarted.set_workspace_state_root(temp.path().to_path_buf());
        restarted.restore_workspace_panels(42, session_id);

        assert!(restarted.scratchpad_open.contains(&42));
        assert!(restarted.terminal_open.contains(&42));
        assert!(!restarted.scratchpad_open.contains(&1));
        assert!(!restarted.terminal_open.contains(&1));
    }

    #[test]
    fn stopped_attached_terminal_collapses_and_clears_persisted_open_state() {
        let directory = tempfile::tempdir().unwrap();
        let state_root = directory.path().join("state");
        let session_id = "terminal-exit-session";
        let mut app = app_with(true);
        app.set_workspace_state_root(state_root.clone());

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
        let events = app.mux.as_ref().unwrap().events.clone();
        let pane = crate::mux::Pane::spawn(
            crate::mux::PaneSpec {
                id: 42,
                title: "Session".to_string(),
                cwd: directory.path().to_path_buf(),
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
        app.view = View::Attached(42);

        app.terminal
            .activate(
                session_id.to_string(),
                "Terminal".to_string(),
                directory.path().to_string_lossy().to_string(),
                &app.config.terminal,
            )
            .unwrap();
        app.terminal_owner = Some(42);
        app.remember_terminal_panel(42, session_id, true);
        app.workspace_focus = WorkspaceFocus::Terminal;
        app.terminal.exit_active_for_test().unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline && app.terminal.stopped_session_ids().is_empty()
        {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        app.collapse_stopped_terminals();

        assert!(!app.terminal.is_visible());
        assert_eq!(app.terminal_owner, None);
        assert!(!app.terminal_open.contains(&42));
        assert_eq!(app.workspace_focus, WorkspaceFocus::Chat);
        assert!(
            !crate::workspace_state::load_in(&state_root, session_id)
                .unwrap()
                .terminal_open
        );
        assert_eq!(
            app.terminal
                .activate(
                    session_id.to_string(),
                    "Terminal".to_string(),
                    directory.path().to_string_lossy().to_string(),
                    &app.config.terminal,
                )
                .unwrap(),
            crate::terminal_pane::Activation::Restarted
        );

        app.terminal.shutdown();
        let _ = app.mux.as_mut().unwrap().shutdown();
    }

    fn session(id: &str, project: &str, updated_at: &str) -> Session {
        Session {
            id: id.to_string(),
            cwd: project.to_string(),
            project_root: project.to_string(),
            summary: Some(format!("Session {id}")),
            created_at: None,
            updated_at: Some(
                DateTime::parse_from_rfc3339(updated_at)
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            is_active: false,
            dir_path: PathBuf::from(id),
            edited_files: Vec::new(),
            last_user_message: None,
            turn_count: 0,
            tool_call_count: 0,
        }
    }

    fn visible_ids(app: &App) -> Vec<&str> {
        app.filtered_indices
            .iter()
            .map(|&index| app.sessions[index].id.as_str())
            .collect()
    }

    #[test]
    fn favorites_lead_only_in_fully_unfiltered_view() {
        let sessions = vec![
            session("newest", "project-a", "2026-08-14T12:00:00Z"),
            session("favorite", "project-a", "2026-08-13T12:00:00Z"),
            session("oldest", "project-b", "2026-08-12T12:00:00Z"),
        ];
        let mut config = UserConfig::default();
        config.favorites.insert("favorite".to_string());
        let mut app = App::new(sessions, config);

        assert_eq!(visible_ids(&app), vec!["favorite", "newest", "oldest"]);

        app.set_project_filter(Some("project-a".to_string()));
        assert_eq!(visible_ids(&app), vec!["newest", "favorite"]);

        app.set_project_filter(None);
        app.search_query = "Session".to_string();
        app.apply_filter();
        assert_eq!(visible_ids(&app), vec!["newest", "favorite", "oldest"]);
    }

    #[test]
    fn favorites_preserve_the_selected_sort_within_each_group() {
        let sessions = vec![
            session("zebra", "project", "2026-08-14T12:00:00Z"),
            session("beta", "project", "2026-08-13T12:00:00Z"),
            session("alpha", "project", "2026-08-12T12:00:00Z"),
        ];
        let mut config = UserConfig::default();
        config.favorites.insert("beta".to_string());
        config.favorites.insert("alpha".to_string());
        let mut app = App::new(sessions, config);

        app.cycle_sort();
        app.cycle_sort();
        assert_eq!(visible_ids(&app), vec!["alpha", "beta", "zebra"]);
        assert_eq!(visible_ids(&app), vec!["alpha", "beta", "zebra"]);
    }
}
