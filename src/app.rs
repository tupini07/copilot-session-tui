use crate::config::{self, EffectiveWorktreeConfig, ProjectSettings, UserConfig};
use crate::github::{GithubError, GithubItem};
use crate::mux::{KeyChord, MuxState, Pane, PaneSpec, PrefixState};
use crate::scratchpad::Scratchpad;
use crate::session::manager;
use crate::session::worktree::ManagedWorktree;
use crate::session::Session;
use crate::snippets::{SnippetModal, SnippetUpdate};
use crate::terminal_pane::TerminalManager;
use crate::updater::{UpdateCheckResult, UpdateInfo};
use crate::workspace_state;
use anyhow::{Context, Result};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use ratatui::layout::Rect;
use ratatui::text::Line;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GithubTab {
    Overview,
    Comments,
    Files,
}

impl GithubTab {
    pub fn index(self) -> usize {
        match self {
            Self::Overview => 0,
            Self::Comments => 1,
            Self::Files => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesPane {
    Tree,
    Diff,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GithubInspectorScreen {
    NumberPrompt,
    Loading,
    Ready(GithubItem),
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubInspector {
    pub screen: GithubInspectorScreen,
    pub input: String,
    pub prompt_error: Option<String>,
    pub tab: GithubTab,
    pub scroll_offsets: [usize; 3],
    pub max_scroll: usize,
    pub selected_file: usize,
    /// Which half of the Files tab receives keys.
    pub files_pane: FilesPane,
    /// Selected row of the flattened changed-file tree.
    pub tree_selected: usize,
    pub tree_offset: usize,
    pub visible_tree_rows: usize,
    /// Directories the user collapsed; everything else stays expanded.
    pub collapsed_dirs: std::collections::BTreeSet<String>,
    /// Pane rectangles from the last draw, so the mouse can target them.
    pub tree_area: Rect,
    pub diff_area: Rect,
    pub diff_scroll: usize,
    pub diff_horizontal: usize,
    pub max_diff_scroll: usize,
    pub max_diff_horizontal: usize,
    pub diff_render_cache: Option<DiffRenderCache>,
    pub request_id: u64,
    pub request_cwd: Option<PathBuf>,
    pub number: Option<u64>,
}

impl GithubInspector {
    pub fn number_prompt() -> Self {
        Self {
            screen: GithubInspectorScreen::NumberPrompt,
            input: String::new(),
            prompt_error: None,
            tab: GithubTab::Overview,
            scroll_offsets: [0; 3],
            max_scroll: 0,
            selected_file: 0,
            files_pane: FilesPane::Tree,
            tree_selected: 0,
            tree_offset: 0,
            visible_tree_rows: 0,
            collapsed_dirs: std::collections::BTreeSet::new(),
            tree_area: Rect::default(),
            diff_area: Rect::default(),
            diff_scroll: 0,
            diff_horizontal: 0,
            max_diff_scroll: 0,
            max_diff_horizontal: 0,
            diff_render_cache: None,
            request_id: 0,
            request_cwd: None,
            number: None,
        }
    }

    pub fn ready_item(&self) -> Option<&GithubItem> {
        match &self.screen {
            GithubInspectorScreen::Ready(item) => Some(item),
            _ => None,
        }
    }

    pub fn tab_count(&self) -> usize {
        self.ready_item()
            .map(|item| if item.is_pull_request() { 3 } else { 2 })
            .unwrap_or(0)
    }

    pub fn cycle_tab(&mut self, forward: bool) {
        let count = self.tab_count();
        if count == 0 {
            return;
        }
        let current = self.tab.index().min(count - 1);
        let next = if forward {
            (current + 1) % count
        } else if current == 0 {
            count - 1
        } else {
            current - 1
        };
        self.tab = match next {
            0 => GithubTab::Overview,
            1 => GithubTab::Comments,
            _ => GithubTab::Files,
        };
        self.max_scroll = 0;
        self.files_pane = FilesPane::Tree;
    }

    pub fn active_scroll(&self) -> usize {
        self.scroll_offsets[self.tab.index()]
    }

    pub fn set_active_scroll(&mut self, offset: usize) {
        self.scroll_offsets[self.tab.index()] = offset.min(self.max_scroll);
    }

    pub fn scroll_active_by(&mut self, amount: isize) {
        let current = self.active_scroll();
        let next = if amount < 0 {
            current.saturating_sub(amount.unsigned_abs())
        } else {
            current.saturating_add(amount as usize)
        };
        self.set_active_scroll(next);
    }

    /// Put the tree cursor on the first file so the diff pane has something to
    /// show as soon as a pull request opens.
    pub fn select_first_tree_file(&mut self) {
        let Some(item) = self.ready_item() else {
            return;
        };
        let rows = crate::ui::file_tree::build_rows(item.files(), &self.collapsed_dirs);
        if let Some(row) = rows.iter().position(|row| row.file_index().is_some()) {
            self.tree_selected = row;
            self.selected_file = rows[row].file_index().unwrap_or(0);
        }
    }

    fn reset_navigation(&mut self) {
        self.tab = GithubTab::Overview;
        self.scroll_offsets = [0; 3];
        self.max_scroll = 0;
        self.selected_file = 0;
        self.files_pane = FilesPane::Tree;
        self.tree_selected = 0;
        self.tree_offset = 0;
        self.visible_tree_rows = 0;
        self.collapsed_dirs.clear();
        self.tree_area = Rect::default();
        self.diff_area = Rect::default();
        self.diff_scroll = 0;
        self.diff_horizontal = 0;
        self.max_diff_scroll = 0;
        self.max_diff_horizontal = 0;
        self.diff_render_cache = None;
    }
}

pub struct GithubLoadResult {
    request_id: u64,
    result: std::result::Result<crate::github::FetchedItem, GithubError>,
    /// A background refresh of something already on screen.
    ///
    /// These must never replace the view with a spinner or an error: the user
    /// is reading a perfectly good copy.
    revalidation: bool,
    reference_generation: Option<u64>,
}

pub struct GithubPatchResult {
    request_id: u64,
    result: std::result::Result<Vec<(String, Option<String>)>, GithubError>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffRenderCache {
    pub file_index: usize,
    pub line_count: usize,
    pub max_width: usize,
    pub lines: Vec<Line<'static>>,
}

/// An item kept around so reopening it is instant.
struct CachedGithubItem {
    repository: crate::github::RepositoryRef,
    number: u64,
    item: GithubItem,
}

/// How many recently viewed items stay in memory.
///
/// Items are small next to the cost of refetching them, and the working set is
/// however many pull requests one conversation refers to.
const GITHUB_CACHE_LIMIT: usize = 12;

/// How often the focused pane is scanned for `#1234` references.
const REFERENCE_SCAN_INTERVAL: Duration = Duration::from_millis(750);
const REFERENCE_REVALIDATE_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Slowest the scan backs off to after repeated lookup failures.
const REFERENCE_SCAN_BACKOFF_LIMIT: Duration = Duration::from_secs(60);

/// What each `#1234` seen in a repository turned out to be.
type ReferenceLookup = std::collections::HashMap<
    (crate::github::RepositoryRef, u64),
    Option<crate::github::ReferenceStatus>,
>;

/// One batch of answers coming back from a lookup.
struct ResolvedReferences {
    repository: crate::github::RepositoryRef,
    statuses: Vec<(u64, Option<crate::github::ReferenceStatus>)>,
    periodic: bool,
    periodic_batch_len: usize,
    generations: HashMap<u64, u64>,
}
pub struct App {
    pub sessions: Vec<Session>,
    pub filtered_indices: Vec<usize>,
    pub selected: usize,
    pub scroll_offset: usize,
    /// Session id currently being dragged through the favorite order, if any.
    ///
    /// Held by id rather than row index so re-sorting after each move cannot lose
    /// track of what the user grabbed.
    pub grabbed_favorite: Option<String>,
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
    /// Selection waiting out a settle delay before its (large) event log is read.
    pub detail_pending: Option<(String, std::time::Instant)>,
    pub should_quit: bool,
    /// First visible line of the help popup, which is taller than it can draw.
    pub help_scroll: usize,
    pub should_resume: Option<(String, String)>, // (session_id, cwd)
    pub should_new_session: Option<NewSessionRequest>,
    pub status_message: Option<String>,
    pub visible_rows: usize,
    pub update_info: Option<UpdateInfo>,
    pub update_receiver: Option<mpsc::Receiver<UpdateCheckResult>>,
    session_load_receiver: Option<mpsc::Receiver<crate::session::loader::SessionLoadResult>>,
    pub update_check_requested: bool,
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
    pub snippet_modal: Option<SnippetModal>,
    pub workspace_areas: WorkspaceAreas,
    pub host_sequences: Vec<Vec<u8>>,
    pub github_inspector: Option<GithubInspector>,
    github_request_receiver: Option<mpsc::Receiver<GithubLoadResult>>,
    github_request_cancel: Option<Arc<AtomicBool>>,
    github_patch_receiver: Option<mpsc::Receiver<GithubPatchResult>>,
    github_patch_cancel: Option<Arc<AtomicBool>>,
    github_repo_receiver: Option<mpsc::Receiver<(PathBuf, Option<crate::github::RepositoryRef>)>>,
    github_reference_receiver: Option<mpsc::Receiver<ResolvedReferences>>,
    /// What each seen `#1234` points at; `None` means "not a reference".
    ///
    /// Negative answers are kept deliberately: a terminal is full of numbers
    /// that merely look like references, and re-asking about them every second
    /// would be worse than useless.
    github_references: ReferenceLookup,
    github_reference_generations: HashMap<(crate::github::RepositoryRef, u64), u64>,
    next_github_reference_generation: u64,
    github_reference_repo: Option<crate::github::RepositoryRef>,
    github_reference_scan: Option<Instant>,
    /// Grows when lookups fail, so a broken network is not hammered.
    github_reference_interval: Duration,
    github_reference_refreshed_at: HashMap<(crate::github::RepositoryRef, u64), Instant>,
    github_reference_periodic_remaining: HashMap<crate::github::RepositoryRef, Vec<u64>>,
    /// Repositories already resolved, keyed by working directory.
    github_repositories: std::collections::HashMap<PathBuf, Option<crate::github::RepositoryRef>>,
    /// Most recently viewed items, newest last.
    github_cache: Vec<CachedGithubItem>,
    next_github_request_id: u64,
    workspace_state_enabled: bool,
    /// Disabled in tests so reordering favorites never rewrites the real user config.
    config_persistence_enabled: bool,
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
            grabbed_favorite: None,
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
            detail_pending: None,
            should_quit: false,
            help_scroll: 0,
            should_resume: None,
            should_new_session: None,
            status_message: None,
            visible_rows: 20,
            update_info: None,
            update_receiver: None,
            session_load_receiver: None,
            update_check_requested: false,
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
            snippet_modal: None,
            workspace_areas: WorkspaceAreas::default(),
            host_sequences: Vec::new(),
            github_inspector: None,
            github_request_receiver: None,
            github_request_cancel: None,
            github_patch_receiver: None,
            github_patch_cancel: None,
            github_repositories: std::collections::HashMap::new(),
            github_repo_receiver: None,
            github_reference_receiver: None,
            github_references: ReferenceLookup::new(),
            github_reference_generations: HashMap::new(),
            next_github_reference_generation: 1,
            github_reference_repo: None,
            github_reference_scan: None,
            github_reference_interval: REFERENCE_SCAN_INTERVAL,
            github_reference_refreshed_at: HashMap::new(),
            github_reference_periodic_remaining: HashMap::new(),
            github_cache: Vec::new(),
            next_github_request_id: 1,
            workspace_state_enabled: true,
            config_persistence_enabled: true,
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
        // New Copilot sessions do not exist on disk until the child has started. Refresh
        // only the live pane ids here: walking the user's entire Copilot history would
        // make opening this tiny switcher noticeably slow.
        self.refresh_live_pane_sessions();
        let mux = self.mux.as_ref().expect("multiplexer checked above");
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

    pub fn open_snippets(&mut self) {
        let (global, global_error) = if self.config_persistence_enabled {
            match config::load_checked() {
                Ok(persisted) => {
                    let snippets = persisted.snippets.clone();
                    self.adopt_persisted_config(persisted);
                    (snippets, None)
                }
                Err(error) => (
                    self.config.snippets.clone(),
                    Some(format!("Cannot refresh global snippets: {error}")),
                ),
            }
        } else {
            (self.config.snippets.clone(), None)
        };
        let project_root = self
            .mux
            .as_ref()
            .and_then(|mux| mux.focused_pane())
            .and_then(|pane| {
                crate::session::loader::detect_project_root(&pane.cwd.to_string_lossy())
            })
            .map(PathBuf::from);
        let (project, project_error, project_root) = match project_root {
            Some(root) => match ProjectSettings::load(&root, &self.config) {
                Ok(settings) => (settings.snippets().to_vec(), None, Some(root)),
                Err(error) => (
                    Vec::new(),
                    Some(format!("Cannot load project snippets: {error}")),
                    None,
                ),
            },
            None => (Vec::new(), None, None),
        };
        let mut modal = SnippetModal::new(global, project, project_root);
        modal.error = global_error.or(project_error);
        self.snippet_modal = Some(modal);
    }

    pub fn persist_snippets(&mut self, update: &SnippetUpdate) -> Result<()> {
        if update.project_root.is_none() && !update.project.is_empty() {
            anyhow::bail!("Project-scoped snippets require a Git project");
        }
        let global_to_save = if update.global_dirty && self.config_persistence_enabled {
            Some(reconcile_global_snippets(
                config::load_checked()?,
                &update.original_global,
                &update.global,
            )?)
        } else {
            None
        };
        let mut project_settings = if update.project_dirty {
            update
                .project_root
                .as_deref()
                .map(|root| ProjectSettings::load(root, &self.config))
                .transpose()?
        } else {
            None
        };
        let previous_project = project_settings
            .as_ref()
            .map(|settings| settings.snippets().to_vec())
            .unwrap_or_default();

        if !self.config_persistence_enabled {
            self.config.snippets = update.global.clone();
            return Ok(());
        }

        if update.project_dirty && previous_project != update.original_project {
            anyhow::bail!("Project snippets changed on disk; close and reopen the snippet manager");
        }
        if update.project_dirty {
            let settings = project_settings
                .as_mut()
                .context("Project settings disappeared while saving snippets")?;
            settings.set_snippets(update.project.clone());
            settings.save()?;
        }

        if let Some(persisted) = global_to_save {
            if let Err(error) = config::save(&persisted) {
                if update.project_dirty {
                    let settings = project_settings
                        .as_mut()
                        .context("Project settings disappeared while rolling back snippets")?;
                    settings.set_snippets(previous_project);
                    if let Err(rollback) = settings.save() {
                        return Err(error).context(format!(
                            "Failed to save global snippets; project rollback also failed: {rollback}"
                        ));
                    }
                }
                return Err(error).context("Failed to save global snippets");
            }
            self.adopt_persisted_config(persisted);
        } else {
            self.config.snippets = update.global.clone();
        }
        Ok(())
    }

    fn adopt_persisted_config(&mut self, mut persisted: UserConfig) {
        // Mux is fixed for the lifetime of this App, including a CLI-only override.
        // Remember the refreshed disk value for future saves but keep the live mode.
        let runtime_mux = self.config.mux;
        self.mux_on_disk = persisted.mux;
        persisted.mux = runtime_mux;
        self.config = persisted;
    }

    /// Status message that also warns when the prefix key had to be defaulted.
    pub fn prefix_label(&self) -> Option<String> {
        self.mux.as_ref().map(|mux| mux.prefix.label())
    }

    /// True while the prefix has been pressed and a command key is awaited.
    pub fn prefix_pending(&self) -> bool {
        self.mux
            .as_ref()
            .is_some_and(|mux| mux.prefix_state != PrefixState::Idle)
    }

    pub fn help_pending(&self) -> bool {
        self.mux
            .as_ref()
            .is_some_and(|mux| mux.prefix_state == PrefixState::Help)
    }

    pub fn github_prefix_pending(&self) -> bool {
        self.mux
            .as_ref()
            .is_some_and(|mux| mux.prefix_state == PrefixState::Github)
    }

    /// Live panes owned by this instance, for the session list footer.
    pub fn running_pane_count(&self) -> usize {
        self.mux
            .as_ref()
            .map(|mux| mux.panes.iter().filter(|pane| pane.is_running()).count())
            .unwrap_or(0)
    }

    pub fn open_github_inspector(&mut self) {
        if !matches!(self.view, View::Attached(_)) {
            self.status_message =
                Some("Attach to a session before inspecting a GitHub item".to_string());
            return;
        }
        self.cancel_github_request();
        self.github_inspector = Some(GithubInspector::number_prompt());
        // Resolving the repository is a network round trip that does not depend
        // on the number, so it can happen while the user is still typing it.
        self.prefetch_github_repository();
    }

    fn prefetch_github_repository(&mut self) {
        let Some(cwd) = self
            .mux
            .as_ref()
            .and_then(|mux| mux.focused_pane())
            .map(|pane| pane.cwd.clone())
        else {
            return;
        };
        if self.github_repositories.contains_key(&cwd) || self.github_repo_receiver.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        std::thread::spawn(move || {
            // A failure is recorded too: without it, a session outside any
            // GitHub repository would keep spawning `gh` forever.
            let resolved = crate::github::resolve_repository_for(&cwd, cancelled).ok();
            let _ = sender.send((cwd, resolved));
        });
        self.github_repo_receiver = Some(receiver);
    }

    fn poll_github_repository(&mut self) {
        let Some(receiver) = self.github_repo_receiver.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok((cwd, repository)) => {
                self.github_repo_receiver = None;
                self.github_repositories.insert(cwd, repository);
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => self.github_repo_receiver = None,
        }
    }

    /// Status of a `#1234` seen in the focused pane, once it is known.
    pub fn github_reference_status(&self, number: u64) -> Option<crate::github::ReferenceStatus> {
        let repository = self.github_reference_repo.as_ref()?;
        self.github_references
            .get(&(repository.clone(), number))
            .copied()
            .flatten()
    }

    /// Look up any references on screen that have not been looked up yet.
    ///
    /// One query covers every new number, so a busy screen still costs a single
    /// round trip.
    pub fn refresh_github_references(&mut self) {
        let Some(cwd) = self
            .mux
            .as_ref()
            .and_then(|mux| mux.focused_pane())
            .map(|pane| pane.cwd.clone())
        else {
            self.github_reference_repo = None;
            return;
        };
        let Some(repository) = self.github_repositories.get(&cwd).cloned().flatten() else {
            self.github_reference_repo = None;
            // Resolving the repository is what makes decoration possible at all,
            // and it is needed for the inspector anyway.
            self.prefetch_github_repository();
            return;
        };
        self.github_reference_repo = Some(repository.clone());
        if self.github_reference_receiver.is_some() {
            return;
        }
        // Scanning the whole screen every frame would be wasteful for something
        // that changes on human timescales.
        let now = Instant::now();
        if self
            .github_reference_scan
            .is_some_and(|last| now.duration_since(last) < self.github_reference_interval)
        {
            return;
        }
        self.github_reference_scan = Some(now);

        let Some(pane) = self.mux.as_ref().and_then(|mux| mux.focused_pane()) else {
            return;
        };
        let visible = pane.github_references();
        if visible.is_empty() {
            return;
        }
        let stale: Vec<u64> = visible
            .iter()
            .copied()
            .filter(|number| {
                self.github_reference_refreshed_at
                    .get(&(repository.clone(), *number))
                    .is_none_or(|last| now.duration_since(*last) >= REFERENCE_REVALIDATE_INTERVAL)
            })
            .collect();
        let numbers: Vec<u64> = self
            .github_reference_periodic_remaining
            .entry(repository.clone())
            .or_insert(stale)
            .iter()
            .copied()
            .take(crate::github::REFERENCE_BATCH)
            .collect();
        if numbers.is_empty() {
            self.github_reference_periodic_remaining.remove(&repository);
            return;
        }
        let periodic = true;
        let periodic_batch_len = numbers.len();
        let generations: HashMap<u64, u64> = numbers
            .iter()
            .map(|number| {
                (
                    *number,
                    self.issue_github_reference_generation(repository.clone(), *number),
                )
            })
            .collect();

        let (sender, receiver) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_repository = repository.clone();
        std::thread::spawn(move || {
            if let Ok(statuses) = crate::github::resolve_references(
                cwd,
                worker_repository.clone(),
                numbers,
                cancelled,
            ) {
                let _ = sender.send(ResolvedReferences {
                    repository: worker_repository,
                    statuses,
                    periodic,
                    periodic_batch_len,
                    generations,
                });
            }
        });
        self.github_reference_receiver = Some(receiver);
    }

    fn poll_github_references(&mut self) {
        let Some(receiver) = self.github_reference_receiver.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(resolved) => {
                self.github_reference_receiver = None;
                self.github_reference_interval = REFERENCE_SCAN_INTERVAL;
                if resolved.periodic {
                    let completed = self
                        .github_reference_periodic_remaining
                        .get_mut(&resolved.repository)
                        .is_none_or(|remaining| {
                            let consumed = resolved.periodic_batch_len.min(remaining.len());
                            remaining.drain(..consumed);
                            remaining.is_empty()
                        });
                    if completed {
                        self.github_reference_periodic_remaining
                            .remove(&resolved.repository);
                    }
                }
                for (number, status) in resolved.statuses {
                    let key = (resolved.repository.clone(), number);
                    let captured = resolved.generations.get(&number).copied().unwrap_or(0);
                    let current = self
                        .github_reference_generations
                        .get(&key)
                        .copied()
                        .unwrap_or(0);
                    if current == captured {
                        self.github_references.insert(key, status);
                        self.github_reference_refreshed_at
                            .insert((resolved.repository.clone(), number), Instant::now());
                    }
                }
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.github_reference_receiver = None;
                // The numbers stay unknown, so without a backoff a persistent
                // failure would mean a `gh` process every scan.
                self.github_reference_interval =
                    (self.github_reference_interval * 2).min(REFERENCE_SCAN_BACKOFF_LIMIT);
            }
        }
    }

    pub fn inspect_github_item(&mut self, number: u64) {
        let Some(cwd) = self
            .mux
            .as_ref()
            .and_then(|mux| mux.focused_pane())
            .map(|pane| pane.cwd.clone())
        else {
            self.status_message = Some("The focused session has no working directory".to_string());
            return;
        };
        self.github_inspector = Some(GithubInspector::number_prompt());
        self.start_github_request(cwd, number);
    }

    pub fn close_github_inspector(&mut self) {
        self.cancel_github_request();
        self.cancel_github_patches();
        self.github_inspector = None;
    }

    pub fn github_loading(&self) -> bool {
        self.github_inspector
            .as_ref()
            .is_some_and(|inspector| matches!(inspector.screen, GithubInspectorScreen::Loading))
    }

    pub fn submit_github_number(&mut self) {
        let number = {
            let Some(inspector) = self.github_inspector.as_mut() else {
                return;
            };
            let Ok(number) = inspector.input.parse::<u64>() else {
                inspector.prompt_error = Some("Enter a positive issue or PR number".to_string());
                return;
            };
            if number == 0 {
                inspector.prompt_error = Some("Item numbers start at 1".to_string());
                return;
            }
            number
        };
        let Some(cwd) = self
            .mux
            .as_ref()
            .and_then(|mux| mux.focused_pane())
            .map(|pane| pane.cwd.clone())
        else {
            if let Some(inspector) = self.github_inspector.as_mut() {
                inspector.prompt_error =
                    Some("The focused session has no working directory".to_string());
            }
            return;
        };
        self.start_github_request(cwd, number);
    }

    pub fn retry_github_request(&mut self) {
        let Some((cwd, number)) = self
            .github_inspector
            .as_ref()
            .and_then(|inspector| Some((inspector.request_cwd.clone()?, inspector.number?)))
        else {
            return;
        };
        // An explicit retry should not hand back the copy the user is trying to
        // get away from.
        self.forget_cached_github_item(&cwd, number);
        self.start_github_request(cwd, number);
    }

    fn forget_cached_github_item(&mut self, cwd: &Path, number: u64) {
        let Some(repository) = self.github_repositories.get(cwd).cloned().flatten() else {
            return;
        };
        self.github_cache
            .retain(|entry| entry.number != number || entry.repository != repository);
    }

    fn cached_github_item(&self, cwd: &Path, number: u64) -> Option<&GithubItem> {
        let repository = self.github_repositories.get(cwd)?.as_ref()?;
        self.github_cache
            .iter()
            .find(|entry| entry.number == number && &entry.repository == repository)
            .map(|entry| &entry.item)
    }

    fn store_github_item(&mut self, repository: crate::github::RepositoryRef, item: GithubItem) {
        let number = item.common().number;
        self.github_cache
            .retain(|entry| entry.number != number || entry.repository != repository);
        self.github_cache.push(CachedGithubItem {
            repository,
            number,
            item,
        });
        if self.github_cache.len() > GITHUB_CACHE_LIMIT {
            self.github_cache.remove(0);
        }
    }

    fn store_fetched_github_item(
        &mut self,
        repository: crate::github::RepositoryRef,
        item: GithubItem,
        request_generation: Option<u64>,
    ) {
        let number = item.common().number;
        let key = (repository.clone(), number);
        let generation = request_generation
            .unwrap_or_else(|| self.issue_github_reference_generation(repository.clone(), number));
        if self.github_reference_generations.get(&key) == Some(&generation) {
            self.github_references
                .insert(key, Some(item.reference_status()));
            self.github_reference_refreshed_at
                .insert((repository.clone(), number), Instant::now());
        }
        self.store_github_item(repository, item);
    }

    fn issue_github_reference_generation(
        &mut self,
        repository: crate::github::RepositoryRef,
        number: u64,
    ) -> u64 {
        let generation = self.next_github_reference_generation;
        self.next_github_reference_generation = generation.wrapping_add(1).max(1);
        self.github_reference_generations
            .insert((repository, number), generation);
        generation
    }

    fn start_github_request(&mut self, cwd: PathBuf, number: u64) {
        // A cached copy goes on screen straight away, and is checked for
        // staleness in the background rather than making the user wait.
        if let Some(item) = self.cached_github_item(&cwd, number).cloned() {
            self.show_github_item(cwd, number, item);
            return;
        }
        self.spawn_github_request(cwd, number, false);
    }

    /// Put an already-known item on screen and quietly check it is current.
    fn show_github_item(&mut self, cwd: PathBuf, number: u64, item: GithubItem) {
        let inspector = self
            .github_inspector
            .get_or_insert_with(GithubInspector::number_prompt);
        inspector.prompt_error = None;
        inspector.request_cwd = Some(cwd.clone());
        inspector.number = Some(number);
        inspector.screen = GithubInspectorScreen::Ready(item);
        inspector.reset_navigation();
        inspector.select_first_tree_file();
        self.spawn_github_request(cwd, number, true);
    }

    fn spawn_github_request(&mut self, cwd: PathBuf, number: u64, revalidation: bool) {
        self.cancel_github_request();
        let request_id = self.next_github_request_id;
        self.next_github_request_id = self.next_github_request_id.wrapping_add(1).max(1);
        let (sender, receiver) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let worker_cwd = cwd.clone();
        let known_repository = self.github_repositories.get(&cwd).cloned().flatten();
        let reference_generation = known_repository
            .as_ref()
            .map(|repository| self.issue_github_reference_generation(repository.clone(), number));
        std::thread::spawn(move || {
            let result =
                crate::github::fetch_item(worker_cwd, number, known_repository, worker_cancelled);
            let _ = sender.send(GithubLoadResult {
                request_id,
                result,
                revalidation,
                reference_generation,
            });
        });

        let inspector = self
            .github_inspector
            .get_or_insert_with(GithubInspector::number_prompt);
        inspector.request_id = request_id;
        if !revalidation {
            inspector.screen = GithubInspectorScreen::Loading;
            inspector.prompt_error = None;
            inspector.request_cwd = Some(cwd);
            inspector.number = Some(number);
            inspector.reset_navigation();
        }
        self.github_request_receiver = Some(receiver);
        self.github_request_cancel = Some(cancelled);
    }

    fn cancel_github_request(&mut self) {
        if let Some(cancelled) = self.github_request_cancel.take() {
            cancelled.store(true, Ordering::Release);
        }
        self.github_request_receiver = None;
    }

    pub fn poll_github(&mut self) {
        self.poll_github_repository();
        self.poll_github_references();
        self.poll_github_item();
        self.poll_github_patches();
    }

    fn poll_github_item(&mut self) {
        let Some(receiver) = self.github_request_receiver.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.github_request_receiver = None;
                self.github_request_cancel = None;
                self.apply_github_result(result);
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.github_request_receiver = None;
                self.github_request_cancel = None;
                if let Some(inspector) = self.github_inspector.as_mut() {
                    if matches!(inspector.screen, GithubInspectorScreen::Loading) {
                        inspector.screen = GithubInspectorScreen::Error(
                            "GitHub loader stopped before returning a result".to_string(),
                        );
                    }
                }
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }

    fn apply_github_result(&mut self, result: GithubLoadResult) {
        let stale = self
            .github_inspector
            .as_ref()
            .is_none_or(|inspector| inspector.request_id != result.request_id);
        if stale {
            return;
        }

        let fetched = match result.result {
            Ok(fetched) => fetched,
            Err(error) => {
                // A failed background check leaves the copy on screen alone; the
                // user did not ask for it and cannot act on it.
                if !result.revalidation {
                    if let Some(inspector) = self.github_inspector.as_mut() {
                        inspector.screen = GithubInspectorScreen::Error(error.to_string());
                    }
                }
                return;
            }
        };

        if let Some(cwd) = self
            .github_inspector
            .as_ref()
            .and_then(|inspector| inspector.request_cwd.clone())
        {
            self.github_repositories
                .insert(cwd, Some(fetched.repository.clone()));
        }

        if result.revalidation {
            let unchanged = self
                .github_inspector
                .as_ref()
                .and_then(|inspector| inspector.ready_item())
                .is_some_and(|shown| shown.common().updated_at == fetched.item.common().updated_at);
            self.store_fetched_github_item(
                fetched.repository,
                fetched.item.clone(),
                result.reference_generation,
            );
            // Redrawing an identical item would throw away the user's scroll
            // position and any diffs already fetched.
            if unchanged {
                return;
            }
        } else {
            self.store_fetched_github_item(
                fetched.repository,
                fetched.item.clone(),
                result.reference_generation,
            );
        }

        let Some(inspector) = self.github_inspector.as_mut() else {
            return;
        };
        inspector.screen = GithubInspectorScreen::Ready(fetched.item);
        inspector.select_first_tree_file();
        inspector.reset_navigation();
    }

    /// True while the inspector is showing a pull request's Files tab.
    pub fn github_files_tab_active(&self) -> bool {
        self.github_inspector.as_ref().is_some_and(|inspector| {
            inspector.tab == GithubTab::Files
                && inspector
                    .ready_item()
                    .is_some_and(|item| item.is_pull_request())
        })
    }

    /// True while the diffs for the pull request on screen are still arriving.
    #[cfg(test)]
    pub fn github_patches_loading(&self) -> bool {
        self.github_patch_receiver.is_some()
    }

    /// Fetch diffs for the pull request on screen, unless that is already done.    ///
    /// The single-call loader lists changed files without their patches, so the
    /// Files tab asks for them the first time it is opened.
    pub fn ensure_github_patches(&mut self) {
        let Some(inspector) = self.github_inspector.as_ref() else {
            return;
        };
        let (Some(cwd), Some(number)) = (inspector.request_cwd.clone(), inspector.number) else {
            return;
        };
        let needed = inspector.ready_item().is_some_and(|item| match item {
            GithubItem::PullRequest(pull) => !pull.patches_loaded && !pull.files.is_empty(),
            GithubItem::Issue(_) => false,
        });
        if !needed || self.github_patch_receiver.is_some() {
            return;
        }
        let Some(repository) = self.github_repositories.get(&cwd).cloned().flatten() else {
            return;
        };

        let request_id = inspector.request_id;
        let (sender, receiver) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        std::thread::spawn(move || {
            let result = crate::github::fetch_patches(cwd, repository, number, worker_cancelled);
            let _ = sender.send(GithubPatchResult { request_id, result });
        });
        self.github_patch_receiver = Some(receiver);
        self.github_patch_cancel = Some(cancelled);
    }

    fn poll_github_patches(&mut self) {
        let Some(receiver) = self.github_patch_receiver.as_ref() else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.github_patch_receiver = None;
                self.github_patch_cancel = None;
                return;
            }
        };
        self.github_patch_receiver = None;
        self.github_patch_cancel = None;

        let Ok(patches) = result.result else {
            // Leaving `patches_loaded` false lets opening the tab try again.
            return;
        };
        let Some(inspector) = self.github_inspector.as_mut() else {
            return;
        };
        if inspector.request_id != result.request_id {
            return;
        }
        let GithubInspectorScreen::Ready(GithubItem::PullRequest(pull)) = &mut inspector.screen
        else {
            return;
        };
        let patches: std::collections::HashMap<String, Option<String>> =
            patches.into_iter().collect();
        for file in &mut pull.files {
            if let Some(patch) = patches.get(&file.path) {
                file.patch = patch.clone();
            }
        }
        pull.patches_loaded = true;
        inspector.diff_render_cache = None;

        let repository = pull.common.repository.clone();
        let item = GithubItem::PullRequest(pull.clone());
        self.store_github_item(repository, item);
    }

    fn cancel_github_patches(&mut self) {
        if let Some(cancelled) = self.github_patch_cancel.take() {
            cancelled.store(true, Ordering::Release);
        }
        self.github_patch_receiver = None;
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
                        let session_id = &pane.session_id;
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

    /// Write the user config, unless a test has opted out of touching real files.
    fn save_config(&self) -> anyhow::Result<()> {
        if !self.config_persistence_enabled {
            return Ok(());
        }
        config::save(&self.persistable_config())
    }

    #[cfg(test)]
    pub fn disable_config_persistence(&mut self) {
        self.config_persistence_enabled = false;
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

    fn refresh_live_pane_sessions(&mut self) {
        let session_ids: Vec<String> = self
            .mux
            .as_ref()
            .map(|mux| {
                mux.panes
                    .iter()
                    .map(|pane| pane.session_id.clone())
                    .collect()
            })
            .unwrap_or_default();
        let mut refreshed = Vec::new();
        for session_id in session_ids {
            match crate::session::loader::load_session(&self.copilot_home, &session_id) {
                Ok(Some(session)) => refreshed.push(session),
                Ok(None) => {}
                Err(error) => {
                    self.status_message =
                        Some(format!("Cannot refresh session {session_id}: {error}"));
                }
            }
        }
        self.merge_session_metadata(refreshed);
    }

    fn merge_session_metadata(&mut self, sessions: Vec<Session>) {
        if sessions.is_empty() {
            return;
        }

        let selected_id = self.selected_session().map(|session| session.id.clone());
        for session in sessions {
            if let Some(index) = self
                .sessions
                .iter()
                .position(|existing| existing.id == session.id)
            {
                let old = std::mem::replace(&mut self.sessions[index], session);
                carry_session_details(&mut self.sessions[index], old);
            } else {
                self.sessions.push(session);
            }
        }
        self.rebuild_unique_projects();
        self.sort_sessions();
        if let Some(session_id) = selected_id {
            self.focus_session(&session_id);
        }
    }

    pub fn begin_session_load(
        &mut self,
        receiver: mpsc::Receiver<crate::session::loader::SessionLoadResult>,
    ) {
        self.session_load_receiver = Some(receiver);
    }

    pub fn sessions_loading(&self) -> bool {
        self.session_load_receiver.is_some()
    }

    pub fn poll_session_load(&mut self) {
        let Some(receiver) = self.session_load_receiver.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(sessions)) => {
                self.apply_loaded_catalog(sessions);
                self.session_load_receiver = None;
            }
            Ok(Err(error)) => {
                self.status_message = Some(format!("Cannot load sessions: {error}"));
                self.session_load_receiver = None;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.status_message =
                    Some("Cannot load sessions: background worker stopped".to_string());
                self.session_load_receiver = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }

    fn apply_loaded_catalog(&mut self, sessions: Vec<Session>) {
        let selected_id = self.selected_session().map(|session| session.id.clone());
        let mut previous: HashMap<String, Session> = std::mem::take(&mut self.sessions)
            .into_iter()
            .map(|session| (session.id.clone(), session))
            .collect();
        let mut catalog = Vec::with_capacity(sessions.len() + previous.len());
        for session in sessions {
            if let Some(current) = previous.remove(&session.id) {
                // Favorites and newly created panes can be refreshed while the worker is
                // scanning. Their in-memory metadata is newer than its snapshot.
                catalog.push(current);
            } else if session.dir_path.join("workspace.yaml").is_file() {
                // A session deleted after the worker read it must not come back as a ghost.
                catalog.push(session);
            }
        }
        // A session can be created while the worker is scanning. Keep anything merged
        // after its snapshot rather than making it briefly disappear at completion.
        catalog.extend(previous.into_values());
        self.sessions = catalog;
        self.rebuild_unique_projects();
        self.sort_sessions();
        if let Some(session_id) = selected_id {
            self.focus_session(&session_id);
        }
    }

    fn rebuild_unique_projects(&mut self) {
        let selected_project = self.unique_projects.get(self.project_selected).cloned();
        self.unique_projects = extract_unique_projects(&self.sessions);
        if let Some(project) = self.cwd_project.as_ref() {
            if !self
                .unique_projects
                .iter()
                .any(|known| known.eq_ignore_ascii_case(project))
            {
                self.unique_projects.insert(0, project.clone());
            }
        }
        self.project_selected = selected_project
            .as_ref()
            .and_then(|selected| {
                self.unique_projects
                    .iter()
                    .position(|project| project.eq_ignore_ascii_case(selected))
            })
            .unwrap_or_else(|| {
                self.project_selected
                    .min(self.unique_projects.len().saturating_sub(1))
            });
    }

    /// Persisted Copilot names are more useful in the pane switcher than OSC window
    /// titles, which can lag behind `/rename` and generated-name updates.
    pub fn pane_session_title<'a>(&'a self, session_id: &str, fallback: &'a str) -> &'a str {
        self.sessions
            .iter()
            .find(|session| session.id == session_id)
            .map(Session::display_name)
            .filter(|title| *title != "(unnamed)")
            .unwrap_or(fallback)
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
            // Favorites lead in the order the user arranged; everything else keeps the
            // selected sort, which the stable sort preserves.
            let mut ordered = std::mem::take(&mut self.filtered_indices);
            ordered.sort_by_key(|&index| {
                self.favorite_rank(&self.sessions[index].id)
                    .unwrap_or(usize::MAX)
            });
            self.filtered_indices = ordered;
        }

        // Reset selection if out of bounds
        if self.selected >= self.filtered_indices.len() {
            self.selected = self.filtered_indices.len().saturating_sub(1);
        }
        self.scroll_offset = 0;
    }

    pub fn is_favorite(&self, session_id: &str) -> bool {
        self.favorite_rank(session_id).is_some()
    }

    /// Position of a session within the user's favorite order, if it is one.
    pub fn favorite_rank(&self, session_id: &str) -> Option<usize> {
        self.config.favorites.iter().position(|id| id == session_id)
    }

    /// True when the list is showing the arranged favorites group.
    ///
    /// Grouping only applies to the unfiltered view, so a search or project filter
    /// leaves the chosen sort untouched.
    pub fn favorites_section_active(&self) -> bool {
        self.project_filter.is_none()
            && self.search_query.is_empty()
            && self
                .filtered_indices
                .iter()
                .any(|&index| self.is_favorite(&self.sessions[index].id))
    }

    /// Lines the list spends on group headers.
    ///
    /// Both the renderer and the scroll maths need this, and they must agree or the
    /// selection drifts out of view, so it is computed in exactly one place.
    pub fn list_header_lines(&self) -> usize {
        if !self.favorites_section_active() {
            return 0;
        }
        // `apply_filter` sorts favorites to the front, so counting the leading run is
        // exact and costs the size of the group rather than the whole list — this
        // runs on every frame.
        let favorites = self
            .filtered_indices
            .iter()
            .take_while(|&&index| self.is_favorite(&self.sessions[index].id))
            .count();
        // The trailing header only exists when ordinary sessions follow.
        1 + usize::from(favorites < self.filtered_indices.len())
    }

    pub fn toggle_selected_favorite(&mut self) -> anyhow::Result<Option<bool>> {
        let Some(session_id) = self.selected_session().map(|session| session.id.clone()) else {
            return Ok(None);
        };
        let previous = self.config.favorites.clone();
        let was_favorite = self.is_favorite(&session_id);

        if was_favorite {
            self.config.favorites.retain(|id| id != &session_id);
        } else {
            // New favorites land at the end so starring one never silently
            // rearranges the order the user already set.
            self.config.favorites.push(session_id.clone());
        }

        if let Err(error) = self.save_config() {
            self.config.favorites = previous;
            return Err(error);
        }

        self.apply_filter();
        self.focus_session(&session_id);

        Ok(Some(!was_favorite))
    }

    pub fn forget_favorite(&mut self, session_id: &str) -> anyhow::Result<bool> {
        let Some(rank) = self.favorite_rank(session_id) else {
            return Ok(false);
        };
        let removed = self.config.favorites.remove(rank);
        if let Err(error) = self.save_config() {
            self.config.favorites.insert(rank, removed);
            return Err(error);
        }
        Ok(true)
    }

    /// Start or stop dragging the selected favorite through the order.
    ///
    /// Returns the message to show, or `None` when nothing was grabbed.
    pub fn toggle_favorite_grab(&mut self) -> Option<String> {
        if self.grabbed_favorite.take().is_some() {
            return Some("Order saved".to_string());
        }
        let session_id = self.selected_session()?.id.clone();
        if !self.is_favorite(&session_id) {
            return Some(
                "Only favorites can be reordered — press Space to add this one".to_string(),
            );
        }
        if !self.favorites_section_active() {
            return Some("Clear the filter and search to reorder favorites".to_string());
        }
        self.grabbed_favorite = Some(session_id);
        Some("Moving favorite — ↑/↓ to move, Enter or Esc to drop".to_string())
    }

    pub fn release_favorite_grab(&mut self) -> bool {
        self.grabbed_favorite.take().is_some()
    }

    /// Move the grabbed favorite one place up (`-1`) or down (`1`).
    ///
    /// Persists immediately, so the arrangement survives however the session ends —
    /// which is the whole point of arranging it.
    pub fn move_grabbed_favorite(&mut self, offset: isize) -> Option<String> {
        let session_id = self.grabbed_favorite.clone()?;
        let from = self.favorite_rank(&session_id)?;
        // Favorites whose session no longer exists stay in the config but never
        // appear in the list, so movement is measured in visible steps. Otherwise a
        // keypress could swap the row past an invisible neighbour and look stuck.
        let visible: Vec<usize> = self
            .config
            .favorites
            .iter()
            .enumerate()
            .filter(|(_, id)| self.sessions.iter().any(|session| &session.id == *id))
            .map(|(rank, _)| rank)
            .collect();
        let Some(to) = visible
            .iter()
            .position(|&rank| rank == from)
            .and_then(|position| position.checked_add_signed(offset))
            .and_then(|position| visible.get(position).copied())
        else {
            // Already at the end of the run; staying put is friendlier than wrapping.
            return None;
        };

        let previous = self.config.favorites.clone();
        let moved = self.config.favorites.remove(from);
        self.config.favorites.insert(to, moved);

        if let Err(error) = self.save_config() {
            self.config.favorites = previous;
            self.grabbed_favorite = None;
            return Some(format!("Failed to save order: {error}"));
        }

        self.apply_filter();
        self.focus_session(&session_id);
        None
    }

    /// Put the cursor on a session and scroll it into view.
    fn focus_session(&mut self, session_id: &str) {
        let Some(display_index) = self
            .filtered_indices
            .iter()
            .position(|&index| self.sessions[index].id == session_id)
        else {
            return;
        };
        self.selected = display_index;
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.visible_rows > 0 && self.selected >= self.scroll_offset + self.visible_rows {
            self.scroll_offset = self.selected + 1 - self.visible_rows;
        }
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
            session_id.to_string(),
            program,
            args,
        )
    }

    /// Start a brand new Copilot session as a pane.
    pub fn attach_new_session(&mut self, cwd: &str, title: String) -> Result<()> {
        let (program, args, session_id) = manager::new_session_command(&self.config)?;
        self.spawn_pane(title, PathBuf::from(cwd), session_id, program, args)
    }

    fn spawn_pane(
        &mut self,
        title: String,
        cwd: PathBuf,
        session_id: String,
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
        self.restore_workspace_panels(id, &workspace_session_id);
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
            mux.prefix_state = PrefixState::Idle;
        }
        self.refresh_live_pane_sessions();
    }

    /// `prefix q` — end the focused session and CST in one step.
    ///
    /// Quitting kills every pane, so this confirms when panes *other* than the one on
    /// screen would go with it. The focused session is the one the user just asked to
    /// end, so ending it alone needs no second keystroke.
    pub fn request_quit_from_pane(&mut self) {
        let background = self
            .mux
            .as_mut()
            .map(|mux| {
                mux.reap();
                mux.background_running_count()
            })
            .unwrap_or(0);
        if background > 0 {
            self.confirm_quit = true;
        } else {
            self.should_quit = true;
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
        if self.update_info.is_some() && !self.update_check_requested {
            return;
        }
        let Some(rx) = self.update_receiver.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(result)) => {
                self.update_info = result;
                if self.update_check_requested {
                    if self.update_info.is_some() {
                        self.should_update = true;
                    } else {
                        self.status_message = Some("No update available".to_string());
                    }
                }
                self.update_receiver = None;
                self.update_check_requested = false;
            }
            Ok(Err(error)) => {
                if self.update_check_requested {
                    self.status_message = Some(format!("Update check failed: {error}"));
                }
                self.update_receiver = None;
                self.update_check_requested = false;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                if self.update_check_requested {
                    self.status_message =
                        Some("Update check failed: background worker stopped".to_string());
                }
                self.update_receiver = None;
                self.update_check_requested = false;
            }
            Err(mpsc::TryRecvError::Empty) => {}
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

fn carry_session_details(session: &mut Session, old: Session) {
    session.edited_files = old.edited_files;
    session.last_user_message = old.last_user_message;
    session.turn_count = old.turn_count;
    session.tool_call_count = old.tool_call_count;
    session.details_parsed_len = old.details_parsed_len;
}

fn reconcile_global_snippets(
    mut persisted: UserConfig,
    original: &[crate::config::PromptSnippet],
    updated: &[crate::config::PromptSnippet],
) -> Result<UserConfig> {
    if persisted.snippets != original {
        anyhow::bail!("Global snippets changed on disk; close and reopen the snippet manager");
    }
    persisted.snippets = updated.to_vec();
    Ok(persisted)
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
    fn project_snippet_save_uses_the_project_config_without_touching_global_config() {
        let project = tempfile::tempdir().unwrap();
        let mut app = app_with(false);
        let snippet = crate::config::PromptSnippet {
            name: "Repository review".to_string(),
            prompt: "Review this repository.".to_string(),
        };

        app.persist_snippets(&SnippetUpdate {
            global: Vec::new(),
            project: vec![snippet.clone()],
            original_global: Vec::new(),
            original_project: Vec::new(),
            project_root: Some(project.path().to_path_buf()),
            global_dirty: false,
            project_dirty: true,
        })
        .unwrap();

        assert!(app.config.snippets.is_empty());
        let settings = ProjectSettings::load(project.path(), &app.config).unwrap();
        assert_eq!(settings.snippets(), &[snippet]);
    }

    #[test]
    fn project_snippet_save_refuses_to_overwrite_an_external_change() {
        let project = tempfile::tempdir().unwrap();
        let original = crate::config::PromptSnippet {
            name: "Original".to_string(),
            prompt: "original".to_string(),
        };
        let external = crate::config::PromptSnippet {
            name: "External".to_string(),
            prompt: "changed elsewhere".to_string(),
        };
        let mut settings = ProjectSettings::load(project.path(), &UserConfig::default()).unwrap();
        settings.set_snippets(vec![external.clone()]);
        settings.save().unwrap();
        let mut app = app_with(false);

        let error = app
            .persist_snippets(&SnippetUpdate {
                global: Vec::new(),
                project: vec![crate::config::PromptSnippet {
                    name: "Mine".to_string(),
                    prompt: "my edit".to_string(),
                }],
                original_global: Vec::new(),
                original_project: vec![original],
                project_root: Some(project.path().to_path_buf()),
                global_dirty: false,
                project_dirty: true,
            })
            .unwrap_err()
            .to_string();

        assert!(error.contains("changed on disk"));
        let reloaded = ProjectSettings::load(project.path(), &app.config).unwrap();
        assert_eq!(reloaded.snippets(), &[external]);
    }

    #[test]
    fn global_snippet_reconcile_preserves_fresh_settings_and_detects_conflicts() {
        let original = crate::config::PromptSnippet {
            name: "Original".to_string(),
            prompt: "old".to_string(),
        };
        let updated = crate::config::PromptSnippet {
            name: "Original".to_string(),
            prompt: "new".to_string(),
        };
        let persisted = UserConfig {
            yolo: true,
            model: Some("fresh-external-model".to_string()),
            snippets: vec![original.clone()],
            ..UserConfig::default()
        };

        let merged =
            reconcile_global_snippets(persisted, std::slice::from_ref(&original), &[updated])
                .unwrap();
        assert!(merged.yolo);
        assert_eq!(merged.model.as_deref(), Some("fresh-external-model"));

        let error = reconcile_global_snippets(
            UserConfig {
                snippets: vec![crate::config::PromptSnippet {
                    name: "External".to_string(),
                    prompt: "changed".to_string(),
                }],
                ..UserConfig::default()
            },
            &[original],
            &[],
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("changed on disk"));
    }

    #[test]
    fn adopting_fresh_global_config_keeps_runtime_mux_override() {
        let mut app = app_with(true);
        app.mux_on_disk = false;
        app.adopt_persisted_config(UserConfig {
            mux: false,
            model: Some("fresh-model".to_string()),
            snippets: vec![crate::config::PromptSnippet {
                name: "Fresh".to_string(),
                prompt: "fresh prompt".to_string(),
            }],
            ..UserConfig::default()
        });

        assert!(app.config.mux, "the live CLI override remains active");
        assert!(!app.mux_on_disk, "future saves retain the disk value");
        assert_eq!(app.config.model.as_deref(), Some("fresh-model"));
        assert_eq!(app.config.snippets[0].name, "Fresh");
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
                session_id: session_id.to_string(),
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
            details_parsed_len: 0,
        }
    }

    fn visible_ids(app: &App) -> Vec<&str> {
        app.filtered_indices
            .iter()
            .map(|&index| app.sessions[index].id.as_str())
            .collect()
    }

    #[test]
    fn refreshing_sessions_adds_new_metadata_without_losing_cached_details_or_selection() {
        let mut existing = session("existing", "project-a", "2026-08-21T10:00:00Z");
        existing.edited_files = vec!["src/main.rs".to_string()];
        existing.last_user_message = Some("keep me".to_string());
        existing.turn_count = 7;
        existing.tool_call_count = 11;
        existing.details_parsed_len = 1234;
        let mut app = App::new(vec![existing], UserConfig::default());

        let renamed = Session {
            summary: Some("Current persisted name".to_string()),
            ..session("existing", "project-a", "2026-08-21T11:00:00Z")
        };
        let new = Session {
            summary: Some("Brand new session".to_string()),
            ..session("new", "project-b", "2026-08-21T12:00:00Z")
        };
        app.merge_session_metadata(vec![renamed, new]);

        assert_eq!(app.selected_session().unwrap().id, "existing");
        let existing = app
            .sessions
            .iter()
            .find(|session| session.id == "existing")
            .unwrap();
        assert_eq!(existing.display_name(), "Current persisted name");
        assert_eq!(existing.edited_files, ["src/main.rs"]);
        assert_eq!(existing.last_user_message.as_deref(), Some("keep me"));
        assert_eq!(existing.turn_count, 7);
        assert_eq!(existing.tool_call_count, 11);
        assert_eq!(existing.details_parsed_len, 1234);
        assert!(app.sessions.iter().any(|session| session.id == "new"));
        assert!(app
            .unique_projects
            .iter()
            .any(|project| project == "project-b"));
    }

    #[test]
    fn pane_switcher_prefers_the_refreshed_session_name_to_an_old_window_title() {
        let persisted = Session {
            summary: Some("EmbViz".to_string()),
            ..session("new-session", "project", "2026-08-21T12:00:00Z")
        };
        let app = App::new(vec![persisted], UserConfig::default());

        assert_eq!(
            app.pane_session_title(
                "new-session",
                "Create Embedding Space Visualizations - GitHub Copilot"
            ),
            "EmbViz"
        );
        assert_eq!(
            app.pane_session_title("not-on-disk-yet", "Starting Copilot"),
            "Starting Copilot"
        );
    }

    #[test]
    fn a_session_created_after_startup_is_discovered_from_disk() {
        let home = tempfile::tempdir().unwrap();
        let session_dir = home.path().join("session-state").join("fresh-session");
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(
            session_dir.join("workspace.yaml"),
            format!(
                "id: fresh-session\ncwd: {}\nname: Fresh name\n\
                 created_at: 2026-08-21T12:00:00Z\nupdated_at: 2026-08-21T12:00:01Z\n",
                home.path().display()
            ),
        )
        .unwrap();

        let mut app = App::new(Vec::new(), UserConfig::default());
        app.copilot_home = home.path().to_path_buf();
        let fresh = crate::session::loader::load_session(&app.copilot_home, "fresh-session")
            .unwrap()
            .expect("new session metadata");
        app.merge_session_metadata(vec![fresh]);

        assert_eq!(app.sessions.len(), 1);
        assert_eq!(app.sessions[0].id, "fresh-session");
        assert_eq!(app.sessions[0].display_name(), "Fresh name");
    }

    #[test]
    fn background_catalog_completion_keeps_selection_details_and_concurrent_sessions() {
        let mut favorite = session("favorite", "project-a", "2026-08-21T10:00:00Z");
        favorite.summary = Some("Current favorite name".to_string());
        favorite.turn_count = 9;
        favorite.details_parsed_len = 456;
        let concurrent = session("created-during-scan", "project-c", "2026-08-21T12:30:00Z");
        let mut app = App::new(vec![favorite, concurrent], UserConfig::default());
        app.focus_session("favorite");

        let loaded_favorite = Session {
            summary: Some("Stale worker name".to_string()),
            ..session("favorite", "project-a", "2026-08-21T12:00:00Z")
        };
        let temp = tempfile::tempdir().unwrap();
        let historical_dir = temp.path().join("historical");
        std::fs::create_dir(&historical_dir).unwrap();
        std::fs::write(historical_dir.join("workspace.yaml"), "id: historical\n").unwrap();
        let historical = Session {
            dir_path: historical_dir,
            ..session("historical", "project-b", "2026-08-20T12:00:00Z")
        };
        let (sender, receiver) = mpsc::channel();
        app.begin_session_load(receiver);
        assert!(app.sessions_loading());
        sender.send(Ok(vec![loaded_favorite, historical])).unwrap();

        app.poll_session_load();

        assert!(!app.sessions_loading());
        assert_eq!(app.selected_session().unwrap().id, "favorite");
        let favorite = app
            .sessions
            .iter()
            .find(|session| session.id == "favorite")
            .unwrap();
        assert_eq!(favorite.display_name(), "Current favorite name");
        assert_eq!(favorite.turn_count, 9);
        assert_eq!(favorite.details_parsed_len, 456);
        assert!(app
            .sessions
            .iter()
            .any(|session| session.id == "historical"));
        assert!(
            app.sessions
                .iter()
                .any(|session| session.id == "created-during-scan"),
            "a session created after the worker's directory snapshot must survive"
        );
    }

    #[test]
    fn background_catalog_does_not_resurrect_deleted_sessions() {
        let temp = tempfile::tempdir().unwrap();
        let deleted = Session {
            dir_path: temp.path().join("already-deleted"),
            ..session("deleted", "project", "2026-08-21T12:00:00Z")
        };
        let mut app = App::new(Vec::new(), UserConfig::default());

        app.apply_loaded_catalog(vec![deleted]);

        assert!(app.sessions.is_empty());
    }

    #[test]
    fn background_catalog_keeps_a_launch_project_with_no_sessions() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join(".git")).unwrap();
        let mut app = App::new(Vec::new(), UserConfig::default());
        app.set_cwd_context(temp.path().to_string_lossy().to_string(), false);
        let launch_project = app.cwd_project.clone().unwrap();

        app.apply_loaded_catalog(Vec::new());

        assert!(app
            .unique_projects
            .iter()
            .any(|project| project.eq_ignore_ascii_case(&launch_project)));
    }

    #[test]
    fn favorites_lead_only_in_fully_unfiltered_view() {
        let sessions = vec![
            session("newest", "project-a", "2026-08-14T12:00:00Z"),
            session("favorite", "project-a", "2026-08-13T12:00:00Z"),
            session("oldest", "project-b", "2026-08-12T12:00:00Z"),
        ];
        let mut config = UserConfig::default();
        config.favorites.push("favorite".to_string());
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
    fn favorites_keep_their_arranged_order_regardless_of_sort() {
        let sessions = vec![
            session("zebra", "project", "2026-08-14T12:00:00Z"),
            session("beta", "project", "2026-08-13T12:00:00Z"),
            session("alpha", "project", "2026-08-12T12:00:00Z"),
        ];
        let mut config = UserConfig::default();
        // Deliberately not alphabetical: the arrangement is the user's, and a sort
        // change must not quietly rewrite it.
        config.favorites.push("beta".to_string());
        config.favorites.push("alpha".to_string());
        let mut app = App::new(sessions, config);

        assert_eq!(visible_ids(&app), vec!["beta", "alpha", "zebra"]);

        app.cycle_sort();
        app.cycle_sort();
        assert_eq!(visible_ids(&app), vec!["beta", "alpha", "zebra"]);
    }

    /// Three favorites arranged `beta, alpha, zebra`, with the cursor on `alpha`.
    fn reorderable_app() -> App {
        let sessions = vec![
            session("zebra", "project", "2026-08-14T12:00:00Z"),
            session("beta", "project", "2026-08-13T12:00:00Z"),
            session("alpha", "project", "2026-08-12T12:00:00Z"),
        ];
        let mut config = UserConfig::default();
        for id in ["beta", "alpha", "zebra"] {
            config.favorites.push(id.to_string());
        }
        let mut app = App::new(sessions, config);
        app.disable_config_persistence();
        app.selected = 1;
        app
    }

    #[test]
    fn grabbing_a_favorite_and_moving_it_rewrites_the_order() {
        let mut app = reorderable_app();

        app.toggle_favorite_grab();
        assert_eq!(app.grabbed_favorite.as_deref(), Some("alpha"));

        app.move_grabbed_favorite(-1);

        assert_eq!(app.config.favorites, vec!["alpha", "beta", "zebra"]);
        assert_eq!(visible_ids(&app), vec!["alpha", "beta", "zebra"]);
    }

    #[test]
    fn the_cursor_follows_the_item_being_moved() {
        let mut app = reorderable_app();
        app.toggle_favorite_grab();

        app.move_grabbed_favorite(-1);

        // Losing the cursor mid-move would make a second press move a different row.
        assert_eq!(app.selected, 0);
        assert_eq!(
            app.selected_session().map(|session| session.id.as_str()),
            Some("alpha")
        );
    }

    #[test]
    fn moving_stops_at_the_ends_instead_of_wrapping() {
        let mut app = reorderable_app();
        app.selected = 0;
        app.toggle_favorite_grab();

        app.move_grabbed_favorite(-1);
        assert_eq!(app.config.favorites, vec!["beta", "alpha", "zebra"]);

        app.selected = 2;
        app.release_favorite_grab();
        app.toggle_favorite_grab();
        app.move_grabbed_favorite(1);
        assert_eq!(app.config.favorites, vec!["beta", "alpha", "zebra"]);
    }

    #[test]
    fn a_favorite_cannot_be_moved_below_the_ordinary_sessions() {
        let sessions = vec![
            session("plain", "project", "2026-08-14T12:00:00Z"),
            session("beta", "project", "2026-08-13T12:00:00Z"),
            session("alpha", "project", "2026-08-12T12:00:00Z"),
        ];
        let config = UserConfig {
            favorites: vec!["beta".to_string(), "alpha".to_string()],
            ..Default::default()
        };
        let mut app = App::new(sessions, config);
        app.disable_config_persistence();
        // The last favorite, sitting directly above the ordinary sessions.
        app.selected = 1;
        app.toggle_favorite_grab();

        app.move_grabbed_favorite(1);

        // Moving down must not push it out of the group or drag `plain` in.
        assert_eq!(app.config.favorites, vec!["beta", "alpha"]);
        assert_eq!(visible_ids(&app), vec!["beta", "alpha", "plain"]);
    }

    #[test]
    fn moving_steps_over_favorites_whose_sessions_are_gone() {
        let sessions = vec![
            session("beta", "project", "2026-08-13T12:00:00Z"),
            session("alpha", "project", "2026-08-12T12:00:00Z"),
        ];
        // `ghost` is a favorite whose session directory no longer exists, so it is
        // kept in the config but never listed.
        let config = UserConfig {
            favorites: vec!["beta".to_string(), "ghost".to_string(), "alpha".to_string()],
            ..Default::default()
        };
        let mut app = App::new(sessions, config);
        app.disable_config_persistence();
        app.selected = 1;
        app.toggle_favorite_grab();

        app.move_grabbed_favorite(-1);

        // One press moves one *visible* row; swapping with `ghost` would look stuck.
        assert_eq!(visible_ids(&app), vec!["alpha", "beta"]);
        assert_eq!(app.config.favorites, vec!["alpha", "beta", "ghost"]);
    }

    #[test]
    fn only_favorites_can_be_grabbed() {
        let sessions = vec![
            session("plain", "project", "2026-08-14T12:00:00Z"),
            session("starred", "project", "2026-08-13T12:00:00Z"),
        ];
        let mut config = UserConfig::default();
        config.favorites.push("starred".to_string());
        let mut app = App::new(sessions, config);
        app.disable_config_persistence();
        // Row 1 is the unstarred session, since the favorite leads the list.
        app.selected = 1;

        let message = app.toggle_favorite_grab();

        assert!(app.grabbed_favorite.is_none());
        assert!(message.is_some_and(|text| text.contains("Only favorites")));
    }

    #[test]
    fn reordering_is_refused_while_the_list_is_filtered() {
        let mut app = reorderable_app();
        app.search_query = "session".to_string();
        app.apply_filter();

        let message = app.toggle_favorite_grab();

        // The group is not shown while filtering, so moving within it would be
        // rearranging something the user cannot see.
        assert!(app.grabbed_favorite.is_none());
        assert!(message.is_some_and(|text| text.contains("Clear the filter")));
    }

    #[test]
    fn newly_starred_sessions_are_appended_rather_than_sorted_in() {
        let mut app = reorderable_app();
        app.config.favorites = vec!["beta".to_string()];
        app.apply_filter();
        app.selected = visible_ids(&app)
            .iter()
            .position(|&id| id == "alpha")
            .expect("alpha is listed");

        app.toggle_selected_favorite().expect("save is disabled");

        assert_eq!(app.config.favorites, vec!["beta", "alpha"]);
    }

    #[test]
    fn headers_are_only_budgeted_when_a_favorites_group_is_shown() {
        let mut app = reorderable_app();
        // Every session is a favorite here, so there is no trailing group to label.
        assert_eq!(app.list_header_lines(), 1);

        app.config.favorites = vec!["beta".to_string()];
        app.apply_filter();
        assert_eq!(app.list_header_lines(), 2);

        app.config.favorites.clear();
        app.apply_filter();
        assert_eq!(app.list_header_lines(), 0);
    }

    /// A pull request with one changed file and no patch yet, as the fast path
    /// produces it.
    fn cached_pull(number: u64, updated_at: &str) -> GithubItem {
        use crate::github::{Author, ChangedFile, ItemCommon, PullRequest, RepositoryRef};
        GithubItem::PullRequest(PullRequest {
            common: ItemCommon {
                repository: RepositoryRef {
                    host: "github.com".to_string(),
                    owner: "octo".to_string(),
                    name: "widgets".to_string(),
                },
                number,
                title: format!("Change {number}"),
                state: "open".to_string(),
                author: Author {
                    login: "monalisa".to_string(),
                },
                labels: Vec::new(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: updated_at.to_string(),
                url: String::new(),
                body: String::new(),
            },
            draft: false,
            merged: false,
            mergeable_state: None,
            base_ref: "main".to_string(),
            head_ref: "feature".to_string(),
            additions: 1,
            deletions: 0,
            changed_files: 1,
            discussion: Vec::new(),
            files: vec![ChangedFile {
                path: "src/lib.rs".to_string(),
                status: "modified".to_string(),
                additions: 1,
                deletions: 0,
                changes: 1,
                patch: None,
            }],
            patches_loaded: false,
        })
    }

    #[test]
    fn reference_styling_is_scoped_to_the_repository_that_was_asked() {
        use crate::github::{ReferenceKind, ReferenceState, ReferenceStatus};

        let mut app = App::new(Vec::new(), UserConfig::default());
        let open_issue = ReferenceStatus {
            kind: ReferenceKind::Issue,
            state: ReferenceState::Open,
        };
        app.github_reference_repo = Some(repository());
        app.github_references
            .insert((repository(), 11), Some(open_issue));
        // A number that turned out not to be a reference is remembered as such,
        // so it is never asked about again.
        app.github_references.insert((repository(), 314), None);

        assert_eq!(app.github_reference_status(11), Some(open_issue));
        assert_eq!(app.github_reference_status(314), None);
        assert_eq!(app.github_reference_status(99), None);
        assert!(app.github_references.contains_key(&(repository(), 314)));

        // The same number in a different repository is a different thing.
        let other = crate::github::RepositoryRef {
            host: "github.com".to_string(),
            owner: "octo".to_string(),
            name: "gadgets".to_string(),
        };
        app.github_reference_repo = Some(other);
        assert_eq!(app.github_reference_status(11), None);
    }

    #[test]
    fn pane_switch_updates_reference_repository_even_while_a_request_is_running() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = app_with(true);
        let current = repository();
        app.github_repositories
            .insert(directory.path().to_path_buf(), Some(current.clone()));
        app.github_reference_repo = Some(crate::github::RepositoryRef {
            host: "github.com".to_string(),
            owner: "old".to_string(),
            name: "repository".to_string(),
        });
        let (_sender, receiver) = mpsc::channel();
        app.github_reference_receiver = Some(receiver);

        let (program, args) = if cfg!(windows) {
            (
                "cmd.exe".to_string(),
                vec!["/c".to_string(), "ping -n 3 127.0.0.1 >nul".to_string()],
            )
        } else {
            (
                "/bin/sh".to_string(),
                vec!["-c".to_string(), "sleep 2".to_string()],
            )
        };
        let events = app.mux.as_ref().unwrap().events.clone();
        let pane = Pane::spawn(
            PaneSpec {
                id: 1,
                title: "Current".to_string(),
                cwd: directory.path().to_path_buf(),
                session_id: "current-session".to_string(),
                program,
                args,
            },
            24,
            80,
            events,
        )
        .unwrap();
        app.mux.as_mut().unwrap().push(pane);
        app.view = View::Attached(1);

        app.refresh_github_references();

        assert_eq!(app.github_reference_repo.as_ref(), Some(&current));
        let _ = app.mux.as_mut().unwrap().shutdown();
    }

    #[test]
    fn inspecting_an_item_backfills_its_fresh_state_into_chat_decoration() {
        use crate::github::{ReferenceKind, ReferenceState, ReferenceStatus};

        let mut app = App::new(Vec::new(), UserConfig::default());
        app.github_reference_repo = Some(repository());
        app.github_references.insert(
            (repository(), 2175),
            Some(ReferenceStatus {
                kind: ReferenceKind::PullRequest,
                state: ReferenceState::Open,
            }),
        );
        let mut item = cached_pull(2175, "2026-08-21T20:50:27Z");
        if let GithubItem::PullRequest(pull) = &mut item {
            pull.common.state = "closed".to_string();
            pull.merged = true;
        }

        app.store_fetched_github_item(repository(), item, None);

        assert_eq!(
            app.github_reference_status(2175),
            Some(ReferenceStatus {
                kind: ReferenceKind::PullRequest,
                state: ReferenceState::Merged,
            }),
            "closing the inspector must leave the chat with the fetched state"
        );

        let (sender, receiver) = mpsc::channel();
        app.github_reference_receiver = Some(receiver);
        sender
            .send(ResolvedReferences {
                repository: repository(),
                statuses: vec![(
                    2175,
                    Some(ReferenceStatus {
                        kind: ReferenceKind::PullRequest,
                        state: ReferenceState::Open,
                    }),
                )],
                periodic: false,
                periodic_batch_len: 0,
                generations: HashMap::from([(2175, 0)]),
            })
            .unwrap();
        app.poll_github_references();
        assert_eq!(
            app.github_reference_status(2175).unwrap().state,
            ReferenceState::Merged,
            "a batch started before the inspector fetch must not overwrite it"
        );

        app.store_github_item(repository(), cached_pull(2175, "2026-08-21T20:49:00Z"));
        assert_eq!(
            app.github_reference_status(2175).unwrap().state,
            ReferenceState::Merged,
            "patch/cache enrichment is not authoritative for reference state"
        );
    }

    #[test]
    fn periodic_reference_batches_update_their_repository_and_finish_the_cycle() {
        use crate::github::{ReferenceKind, ReferenceState, ReferenceStatus};

        let mut app = App::new(Vec::new(), UserConfig::default());
        let repository = repository();
        app.github_reference_periodic_remaining
            .insert(repository.clone(), (1..=41).collect());
        let closed = ReferenceStatus {
            kind: ReferenceKind::Issue,
            state: ReferenceState::Closed,
        };

        let (sender, receiver) = mpsc::channel();
        app.github_reference_receiver = Some(receiver);
        sender
            .send(ResolvedReferences {
                repository: repository.clone(),
                statuses: (1..=40).map(|number| (number, Some(closed))).collect(),
                periodic: true,
                periodic_batch_len: 40,
                generations: (1..=40).map(|number| (number, 0)).collect(),
            })
            .unwrap();
        app.poll_github_references();

        assert_eq!(
            app.github_reference_periodic_remaining[&repository],
            vec![41]
        );
        assert!(app
            .github_reference_refreshed_at
            .contains_key(&(repository.clone(), 1)));
        assert!(!app
            .github_reference_refreshed_at
            .contains_key(&(repository.clone(), 41)));
        assert_eq!(
            app.github_references[&(repository.clone(), 1)],
            Some(closed)
        );

        let (sender, receiver) = mpsc::channel();
        app.github_reference_receiver = Some(receiver);
        sender
            .send(ResolvedReferences {
                repository: repository.clone(),
                statuses: vec![(41, Some(closed))],
                periodic: true,
                periodic_batch_len: 1,
                generations: HashMap::from([(41, 0)]),
            })
            .unwrap();
        app.poll_github_references();

        assert!(!app
            .github_reference_periodic_remaining
            .contains_key(&repository));
        assert!(app
            .github_reference_refreshed_at
            .contains_key(&(repository.clone(), 41)));
        assert_eq!(app.github_references[&(repository, 41)], Some(closed));
    }

    #[test]
    fn newer_periodic_request_beats_an_older_slow_inspector_request() {
        use crate::github::{ReferenceKind, ReferenceState, ReferenceStatus};

        let mut app = App::new(Vec::new(), UserConfig::default());
        let repository = repository();
        app.github_reference_repo = Some(repository.clone());
        let inspector_generation = app.issue_github_reference_generation(repository.clone(), 7);
        let periodic_generation = app.issue_github_reference_generation(repository.clone(), 7);
        let closed = ReferenceStatus {
            kind: ReferenceKind::PullRequest,
            state: ReferenceState::Closed,
        };
        let (sender, receiver) = mpsc::channel();
        app.github_reference_receiver = Some(receiver);
        sender
            .send(ResolvedReferences {
                repository: repository.clone(),
                statuses: vec![(7, Some(closed))],
                periodic: false,
                periodic_batch_len: 0,
                generations: HashMap::from([(7, periodic_generation)]),
            })
            .unwrap();
        app.poll_github_references();

        app.store_fetched_github_item(
            repository,
            cached_pull(7, "2026-08-21T20:00:00Z"),
            Some(inspector_generation),
        );

        assert_eq!(app.github_reference_status(7), Some(closed));
    }

    fn repository() -> crate::github::RepositoryRef {
        crate::github::RepositoryRef {
            host: "github.com".to_string(),
            owner: "octo".to_string(),
            name: "widgets".to_string(),
        }
    }

    #[test]
    fn a_second_look_at_the_same_item_skips_the_spinner() {
        let mut app = App::new(Vec::new(), UserConfig::default());
        let cwd = PathBuf::from("C:/Workspace/widgets");
        app.github_repositories
            .insert(cwd.clone(), Some(repository()));
        app.store_github_item(repository(), cached_pull(7, "2026-01-02T00:00:00Z"));

        app.start_github_request(cwd, 7);

        // The whole point: no Loading screen on the way back to something the
        // user just looked at.
        let inspector = app.github_inspector.as_ref().unwrap();
        assert!(matches!(inspector.screen, GithubInspectorScreen::Ready(_)));
        assert_eq!(inspector.ready_item().unwrap().common().number, 7);
    }

    #[test]
    fn an_unchanged_revalidation_leaves_the_open_item_alone() {
        let mut app = App::new(Vec::new(), UserConfig::default());
        let cwd = PathBuf::from("C:/Workspace/widgets");
        app.github_repositories
            .insert(cwd.clone(), Some(repository()));
        let item = cached_pull(7, "2026-01-02T00:00:00Z");
        app.github_inspector = Some(GithubInspector::number_prompt());
        {
            let inspector = app.github_inspector.as_mut().unwrap();
            inspector.request_id = 42;
            inspector.request_cwd = Some(cwd);
            inspector.number = Some(7);
            inspector.screen = GithubInspectorScreen::Ready(item.clone());
            inspector.scroll_offsets[0] = 25;
        }

        app.apply_github_result(GithubLoadResult {
            request_id: 42,
            result: Ok(crate::github::FetchedItem {
                repository: repository(),
                item,
            }),
            revalidation: true,
            reference_generation: None,
        });

        // Replacing an identical item would throw away where the user had
        // scrolled to.
        assert_eq!(app.github_inspector.as_ref().unwrap().scroll_offsets[0], 25);
    }

    #[test]
    fn a_revalidation_that_moved_on_replaces_what_is_shown() {
        let mut app = App::new(Vec::new(), UserConfig::default());
        let cwd = PathBuf::from("C:/Workspace/widgets");
        app.github_repositories
            .insert(cwd.clone(), Some(repository()));
        app.github_inspector = Some(GithubInspector::number_prompt());
        {
            let inspector = app.github_inspector.as_mut().unwrap();
            inspector.request_id = 42;
            inspector.request_cwd = Some(cwd);
            inspector.number = Some(7);
            inspector.screen = GithubInspectorScreen::Ready(cached_pull(7, "2026-01-02T00:00:00Z"));
        }

        let mut newer = cached_pull(7, "2026-03-03T00:00:00Z");
        if let GithubItem::PullRequest(pull) = &mut newer {
            pull.common.title = "Reworked".to_string();
        }
        app.apply_github_result(GithubLoadResult {
            request_id: 42,
            result: Ok(crate::github::FetchedItem {
                repository: repository(),
                item: newer,
            }),
            revalidation: true,
            reference_generation: None,
        });

        let shown = app.github_inspector.as_ref().unwrap();
        assert_eq!(shown.ready_item().unwrap().common().title, "Reworked");
    }

    #[test]
    fn a_failed_background_check_keeps_the_copy_on_screen() {
        let mut app = App::new(Vec::new(), UserConfig::default());
        app.github_inspector = Some(GithubInspector::number_prompt());
        {
            let inspector = app.github_inspector.as_mut().unwrap();
            inspector.request_id = 42;
            inspector.screen = GithubInspectorScreen::Ready(cached_pull(7, "2026-01-02T00:00:00Z"));
        }

        app.apply_github_result(GithubLoadResult {
            request_id: 42,
            result: Err(crate::github::GithubError {
                kind: crate::github::GithubErrorKind::Cli,
                message: "network is down".to_string(),
            }),
            revalidation: true,
            reference_generation: None,
        });

        // The user did not ask for the refresh and cannot act on its failure.
        let inspector = app.github_inspector.as_ref().unwrap();
        assert!(
            inspector.ready_item().is_some(),
            "item was replaced by an error"
        );
    }

    #[test]
    fn a_failed_first_load_is_reported() {
        let mut app = App::new(Vec::new(), UserConfig::default());
        app.github_inspector = Some(GithubInspector::number_prompt());
        app.github_inspector.as_mut().unwrap().request_id = 42;

        app.apply_github_result(GithubLoadResult {
            request_id: 42,
            result: Err(crate::github::GithubError {
                kind: crate::github::GithubErrorKind::NotFound,
                message: "no such item".to_string(),
            }),
            revalidation: false,
            reference_generation: None,
        });

        let screen = &app.github_inspector.as_ref().unwrap().screen;
        assert!(
            matches!(screen, GithubInspectorScreen::Error(message) if message.contains("no such item"))
        );
    }

    #[test]
    fn the_cache_keeps_only_the_most_recent_items() {
        let mut app = App::new(Vec::new(), UserConfig::default());
        for number in 1..=(GITHUB_CACHE_LIMIT as u64 + 3) {
            app.store_github_item(repository(), cached_pull(number, "2026-01-01T00:00:00Z"));
        }

        assert_eq!(app.github_cache.len(), GITHUB_CACHE_LIMIT);
        // The oldest are dropped, not the newest.
        assert!(app.github_cache.iter().all(|entry| entry.number > 3));
    }

    #[test]
    fn reopening_an_item_refreshes_its_place_in_the_cache() {
        let mut app = App::new(Vec::new(), UserConfig::default());
        app.store_github_item(repository(), cached_pull(1, "2026-01-01T00:00:00Z"));
        app.store_github_item(repository(), cached_pull(2, "2026-01-01T00:00:00Z"));
        app.store_github_item(repository(), cached_pull(1, "2026-02-01T00:00:00Z"));

        // Storing twice must update in place rather than keep two copies.
        assert_eq!(app.github_cache.len(), 2);
        assert_eq!(app.github_cache.last().unwrap().number, 1);
    }

    #[test]
    fn retrying_bypasses_the_cached_copy() {
        let mut app = App::new(Vec::new(), UserConfig::default());
        let cwd = PathBuf::from("C:/Workspace/widgets");
        app.github_repositories
            .insert(cwd.clone(), Some(repository()));
        app.store_github_item(repository(), cached_pull(7, "2026-01-02T00:00:00Z"));
        app.github_inspector = Some(GithubInspector::number_prompt());
        {
            let inspector = app.github_inspector.as_mut().unwrap();
            inspector.request_cwd = Some(cwd.clone());
            inspector.number = Some(7);
        }

        app.retry_github_request();

        // Handing back the copy the user is trying to get away from would make
        // the retry key do nothing.
        assert!(app.cached_github_item(&cwd, 7).is_none());
    }

    #[test]
    fn diffs_are_only_requested_for_a_pull_request_that_lacks_them() {
        let mut app = App::new(Vec::new(), UserConfig::default());
        let cwd = PathBuf::from("C:/Workspace/widgets");
        app.github_repositories
            .insert(cwd.clone(), Some(repository()));
        app.github_inspector = Some(GithubInspector::number_prompt());
        {
            let inspector = app.github_inspector.as_mut().unwrap();
            inspector.request_cwd = Some(cwd);
            inspector.number = Some(7);
            inspector.screen = GithubInspectorScreen::Ready(cached_pull(7, "2026-01-02T00:00:00Z"));
        }

        app.ensure_github_patches();
        assert!(
            app.github_patches_loading(),
            "diffs should have been requested"
        );

        // Already loaded: opening the tab again must not refetch.
        app.cancel_github_patches();
        if let Some(GithubItem::PullRequest(pull)) =
            app.github_inspector
                .as_mut()
                .and_then(|inspector| match &mut inspector.screen {
                    GithubInspectorScreen::Ready(item) => Some(item),
                    _ => None,
                })
        {
            pull.patches_loaded = true;
        }
        app.ensure_github_patches();
        assert!(!app.github_patches_loading());
    }

    #[test]
    fn arriving_patches_invalidate_the_rendered_diff_cache() {
        let mut app = App::new(Vec::new(), UserConfig::default());
        app.github_inspector = Some(GithubInspector::number_prompt());
        {
            let inspector = app.github_inspector.as_mut().unwrap();
            inspector.request_id = 42;
            inspector.screen = GithubInspectorScreen::Ready(cached_pull(7, "2026-01-02T00:00:00Z"));
            inspector.diff_render_cache = Some(DiffRenderCache {
                file_index: 0,
                line_count: 1,
                max_width: 10,
                lines: vec![Line::from("old render")],
            });
        }
        let (sender, receiver) = mpsc::channel();
        app.github_patch_receiver = Some(receiver);
        sender
            .send(GithubPatchResult {
                request_id: 42,
                result: Ok(vec![(
                    "src/lib.rs".to_string(),
                    Some("@@ -1 +1 @@\n-old\n+new".to_string()),
                )]),
            })
            .unwrap();

        app.poll_github_patches();

        assert!(app
            .github_inspector
            .as_ref()
            .unwrap()
            .diff_render_cache
            .is_none());
    }

    #[test]
    fn requested_update_check_starts_an_available_update() {
        let mut app = App::new(Vec::new(), UserConfig::default());
        let (tx, rx) = mpsc::channel();
        app.update_receiver = Some(rx);
        app.update_check_requested = true;
        tx.send(Ok(Some(UpdateInfo {
            current_version: "0.9.0".to_string(),
            latest_version: "0.10.0".to_string(),
        })))
        .unwrap();

        app.poll_update();

        assert!(app.should_update);
        assert!(app.update_info.is_some());
        assert!(!app.update_check_requested);
        assert!(app.update_receiver.is_none());
    }

    #[test]
    fn requested_update_check_reports_when_current() {
        let mut app = App::new(Vec::new(), UserConfig::default());
        let (tx, rx) = mpsc::channel();
        app.update_receiver = Some(rx);
        app.update_check_requested = true;
        tx.send(Ok(None)).unwrap();

        app.poll_update();

        assert_eq!(app.status_message.as_deref(), Some("No update available"));
        assert!(!app.should_update);
        assert!(!app.update_check_requested);
        assert!(app.update_receiver.is_none());
    }
}
