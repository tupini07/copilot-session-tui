use crate::session::Session;
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Search,
    Rename,
    ConfirmDelete,
    FilterProject,
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortField {
    LastUsed,
    Created,
    Name,
    Project,
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
    pub unique_projects: Vec<String>,
    pub project_selected: usize,
    pub sort_field: SortField,
    pub detail_loaded_for: Option<String>,
    pub should_quit: bool,
    pub should_resume: Option<(String, String)>, // (session_id, cwd)
    pub status_message: Option<String>,
    pub visible_rows: usize,
}

impl App {
    pub fn new(sessions: Vec<Session>) -> Self {
        let unique_projects = extract_unique_projects(&sessions);
        let filtered_indices: Vec<usize> = (0..sessions.len()).collect();

        App {
            sessions,
            filtered_indices,
            selected: 0,
            scroll_offset: 0,
            mode: Mode::Normal,
            search_query: String::new(),
            rename_input: String::new(),
            project_filter: None,
            unique_projects,
            project_selected: 0,
            sort_field: SortField::LastUsed,
            detail_loaded_for: None,
            should_quit: false,
            should_resume: None,
            status_message: None,
            visible_rows: 20,
        }
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
                    if !s.cwd.eq_ignore_ascii_case(proj) {
                        return false;
                    }
                }
                // Search filter
                if !self.search_query.is_empty() {
                    let haystack = format!(
                        "{} {} {}",
                        s.display_name(),
                        s.cwd,
                        s.id
                    );
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
                self.sessions.sort_by(|a, b| b.created_at.cmp(&a.created_at));
            }
            SortField::Name => {
                self.sessions.sort_by(|a, b| {
                    a.display_name()
                        .to_lowercase()
                        .cmp(&b.display_name().to_lowercase())
                });
            }
            SortField::Project => {
                self.sessions.sort_by(|a, b| a.cwd.cmp(&b.cwd));
            }
        }
        self.apply_filter();
    }

    pub fn set_project_filter(&mut self, project: Option<String>) {
        self.project_filter = project;
        self.apply_filter();
    }

    pub fn sort_label(&self) -> &str {
        match self.sort_field {
            SortField::LastUsed => "Last Used",
            SortField::Created => "Created",
            SortField::Name => "Name",
            SortField::Project => "Project",
        }
    }
}

fn extract_unique_projects(sessions: &[Session]) -> Vec<String> {
    use chrono::{DateTime, Utc};
    // Track the most recent updated_at per project
    let mut latest: std::collections::HashMap<String, DateTime<Utc>> =
        std::collections::HashMap::new();
    for s in sessions {
        if s.cwd.is_empty() {
            continue;
        }
        if let Some(updated) = s.updated_at {
            let entry = latest.entry(s.cwd.clone()).or_insert(updated);
            if updated > *entry {
                *entry = updated;
            }
        } else {
            latest.entry(s.cwd.clone()).or_insert_with(|| DateTime::<Utc>::MIN_UTC);
        }
    }
    let mut projects: Vec<String> = latest.keys().cloned().collect();
    projects.sort_by(|a, b| latest[b].cmp(&latest[a])); // most recent first
    projects
}
