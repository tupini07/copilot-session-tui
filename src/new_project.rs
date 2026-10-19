//! The `P` dialog: adopt an existing folder as a project, or create a new one.
//!
//! Projects are otherwise discovered from the sessions that ran in them, so starting
//! something brand new used to mean leaving CST for a shell to make the folder, then a
//! plain Copilot session to give it a history, before CST would offer it at all.

use std::fs;
use std::path::{is_separator, PathBuf, MAIN_SEPARATOR};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewProjectKind {
    Existing,
    Create,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewProjectDialog {
    /// `None` while still asking which.
    pub kind: Option<NewProjectKind>,
    pub input: String,
    pub error: Option<String>,
    /// The Git project the typed folder sits inside, when that is not the folder
    /// itself. Sessions are grouped by repository root, so that is what gets added —
    /// said before Enter rather than discovered after.
    pub enclosing_project: Option<String>,
}

impl NewProjectDialog {
    /// Start the path at `start`, ready for a folder name to be typed after it.
    pub fn new(start: Option<PathBuf>) -> Self {
        let input = start
            .or_else(dirs::home_dir)
            .map(|directory| with_trailing_separator(&directory.to_string_lossy()))
            .unwrap_or_default();
        Self {
            input,
            ..Self::default()
        }
    }

    pub fn choose(&mut self, kind: NewProjectKind) {
        self.kind = Some(kind);
        self.refresh();
    }

    /// Back to the first question, keeping what was typed.
    pub fn back(&mut self) {
        self.kind = None;
        self.error = None;
    }

    pub fn push(&mut self, character: char) {
        self.input.push(character);
        self.refresh();
    }

    pub fn pop(&mut self) {
        self.input.pop();
        self.refresh();
    }

    pub fn complete(&mut self) {
        if let Some(completed) = complete_directory(&self.input) {
            self.input = completed;
            self.refresh();
        }
    }

    // Kept out of the draw: walking up for `.git` on every frame would be wasted work
    // for an answer that only changes when the path does.
    fn refresh(&mut self) {
        self.error = None;
        self.enclosing_project = self.target().ok().and_then(|directory| {
            crate::session::loader::detect_project_root(&directory.to_string_lossy())
                .filter(|root| fs::canonicalize(root).ok() != fs::canonicalize(&directory).ok())
        });
    }

    /// The folder typed, if it is a full path.
    pub fn target(&self) -> Result<PathBuf, String> {
        let typed = self.input.trim();
        if typed.is_empty() {
            return Err("Type the folder's full path".to_string());
        }
        let directory = expand_home(typed);
        // Relative to what? CST's own directory is not something the user is looking
        // at, so guessing would put the folder somewhere they did not mean.
        if !directory.is_absolute() {
            return Err("Type a full path, not a relative one".to_string());
        }
        Ok(directory)
    }

    /// Check the folder exists, or create it, ready to become a project.
    pub fn prepare(&self) -> Result<PathBuf, String> {
        let directory = self.target()?;
        // The path is on screen right above these, and repeating one that wraps would
        // push the key hints out of the dialog.
        match self.kind {
            Some(NewProjectKind::Existing) if directory.is_dir() => Ok(directory),
            Some(NewProjectKind::Existing) if directory.exists() => {
                Err("That is a file, not a folder".to_string())
            }
            Some(NewProjectKind::Existing) => Err("There is no folder there".to_string()),
            Some(NewProjectKind::Create) if directory.exists() => {
                Err("That already exists; Esc, then e to add it as an existing folder".to_string())
            }
            Some(NewProjectKind::Create) => fs::create_dir_all(&directory)
                .map(|()| directory.clone())
                .map_err(|error| format!("Cannot create that folder: {error}")),
            None => Err("Choose an existing folder or a new one first".to_string()),
        }
    }
}

fn with_trailing_separator(path: &str) -> String {
    if path.ends_with(is_separator) {
        path.to_string()
    } else {
        format!("{path}{MAIN_SEPARATOR}")
    }
}

fn expand_home(typed: &str) -> PathBuf {
    let rest = typed
        .strip_prefix('~')
        .filter(|rest| rest.is_empty() || rest.starts_with(is_separator));
    match (rest, dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest.trim_start_matches(is_separator)),
        _ => PathBuf::from(typed),
    }
}

/// Extend the last segment of `input` as far as the folders beside it agree, the way
/// a shell's Tab does. `None` when there is nothing to add.
fn complete_directory(input: &str) -> Option<String> {
    let split = input.rfind(is_separator)?;
    let (parent, partial) = (&input[..=split], &input[split + 1..]);
    let partial_lower = partial.to_lowercase();
    let mut names: Vec<String> = fs::read_dir(expand_home(parent))
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.to_lowercase().starts_with(&partial_lower))
        // Dot-folders are clutter unless asked for, as in a shell.
        .filter(|name| partial.starts_with('.') || !name.starts_with('.'))
        .collect();
    names.sort();
    let completed = match names.as_slice() {
        [] => return None,
        [only] => format!("{only}{MAIN_SEPARATOR}"),
        [first, rest @ ..] => {
            // Case-insensitive, because so are Windows and macOS file systems.
            let agreed = first
                .chars()
                .enumerate()
                .take_while(|(index, character)| {
                    rest.iter().all(|name| {
                        name.chars()
                            .nth(*index)
                            .is_some_and(|other| other.to_lowercase().eq(character.to_lowercase()))
                    })
                })
                .count();
            first.chars().take(agreed).collect()
        }
    };
    (completed.chars().count() > partial.chars().count()).then(|| format!("{parent}{completed}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn typed(directory: &Path) -> String {
        with_trailing_separator(&directory.to_string_lossy())
    }

    #[test]
    fn the_path_starts_ready_for_a_folder_name_so_only_the_name_needs_typing() {
        let temp = tempfile::tempdir().unwrap();

        let dialog = NewProjectDialog::new(Some(temp.path().to_path_buf()));

        assert_eq!(dialog.input, typed(temp.path()));
    }

    #[test]
    fn tab_completes_a_unique_folder_and_adds_the_separator_for_the_next_one() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("copilot-session-tui")).unwrap();
        fs::create_dir(temp.path().join("other")).unwrap();

        let completed = complete_directory(&format!("{}cop", typed(temp.path())));

        assert_eq!(
            completed,
            Some(format!(
                "{}copilot-session-tui{MAIN_SEPARATOR}",
                typed(temp.path())
            ))
        );
    }

    #[test]
    fn tab_stops_where_several_folders_stop_agreeing_so_it_never_guesses_between_them() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("project-alpha")).unwrap();
        fs::create_dir(temp.path().join("project-beta")).unwrap();

        let completed = complete_directory(&format!("{}pro", typed(temp.path())));

        assert_eq!(completed, Some(format!("{}project-", typed(temp.path()))));
        assert_eq!(
            complete_directory(&format!("{}project-", typed(temp.path()))),
            None,
            "nothing more to agree on means nothing to add"
        );
    }

    #[test]
    fn tab_ignores_files_because_a_project_is_a_folder() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("notes.txt"), "").unwrap();

        assert_eq!(
            complete_directory(&format!("{}no", typed(temp.path()))),
            None
        );
    }

    #[test]
    fn adding_an_existing_folder_that_is_not_there_is_refused_with_the_reason() {
        let temp = tempfile::tempdir().unwrap();
        let mut dialog = NewProjectDialog::new(Some(temp.path().to_path_buf()));
        dialog.choose(NewProjectKind::Existing);
        for character in "missing".chars() {
            dialog.push(character);
        }

        let error = dialog.prepare().unwrap_err();

        assert!(error.contains("no folder"), "{error}");
    }

    #[test]
    fn creating_a_folder_makes_it_including_any_missing_parents() {
        let temp = tempfile::tempdir().unwrap();
        let mut dialog = NewProjectDialog::new(Some(temp.path().to_path_buf()));
        dialog.choose(NewProjectKind::Create);
        dialog
            .input
            .push_str(&format!("outer{MAIN_SEPARATOR}fresh"));

        let directory = dialog.prepare().unwrap();

        assert!(directory.is_dir());
        assert!(directory.ends_with(Path::new("outer").join("fresh")));
    }

    #[test]
    fn creating_a_folder_that_exists_is_refused_so_nobody_adopts_one_by_accident() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("taken")).unwrap();
        let mut dialog = NewProjectDialog::new(Some(temp.path().to_path_buf()));
        dialog.choose(NewProjectKind::Create);
        dialog.input.push_str("taken");

        let error = dialog.prepare().unwrap_err();

        assert!(error.contains("already exists"), "{error}");
    }

    #[test]
    fn a_relative_path_is_refused_rather_than_resolved_against_somewhere_unseen() {
        let mut dialog = NewProjectDialog::new(None);
        dialog.choose(NewProjectKind::Create);
        dialog.input = "relative-folder".to_string();

        assert!(dialog.prepare().is_err());
        assert!(!Path::new("relative-folder").exists());
    }

    #[test]
    fn a_folder_inside_a_repository_says_which_project_it_will_join_before_enter() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();
        let mut dialog = NewProjectDialog::new(Some(temp.path().to_path_buf()));
        dialog.choose(NewProjectKind::Create);
        for character in "packages".chars() {
            dialog.push(character);
        }

        assert_eq!(
            dialog.enclosing_project.map(PathBuf::from),
            Some(fs::canonicalize(temp.path()).unwrap())
                .map(|path| PathBuf::from(path.to_string_lossy().trim_start_matches(r"\\?\")))
        );
    }

    #[test]
    fn a_repository_root_itself_is_not_reported_as_inside_another_project() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();
        let mut dialog = NewProjectDialog::new(None);
        dialog.input = temp.path().to_string_lossy().to_string();
        dialog.choose(NewProjectKind::Existing);

        assert_eq!(dialog.enclosing_project, None);
    }
}
