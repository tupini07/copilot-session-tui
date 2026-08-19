use crate::github::ChangedFile;
use std::collections::{BTreeMap, BTreeSet};

/// What a flattened tree row represents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKind {
    Directory {
        /// Full path of the directory, used as the collapse key.
        path: String,
        files: usize,
        additions: u64,
        deletions: u64,
        expanded: bool,
    },
    File {
        /// Index into the pull request's changed-file list.
        index: usize,
    },
}

/// One rendered line of the changed-file tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    pub depth: usize,
    pub label: String,
    pub kind: RowKind,
}

impl TreeRow {
    pub fn file_index(&self) -> Option<usize> {
        match self.kind {
            RowKind::File { index } => Some(index),
            RowKind::Directory { .. } => None,
        }
    }

    pub fn directory_path(&self) -> Option<&str> {
        match &self.kind {
            RowKind::Directory { path, .. } => Some(path.as_str()),
            RowKind::File { .. } => None,
        }
    }
}

/// Directories and files sort separately and alphabetically, matching how
/// file trees are conventionally presented.
#[derive(Default)]
struct Dir {
    dirs: BTreeMap<String, Dir>,
    files: BTreeMap<String, usize>,
}

impl Dir {
    fn totals(&self, files: &[ChangedFile]) -> (usize, u64, u64) {
        let mut count = self.files.len();
        let mut additions = 0;
        let mut deletions = 0;
        for index in self.files.values() {
            if let Some(file) = files.get(*index) {
                additions += file.additions;
                deletions += file.deletions;
            }
        }
        for dir in self.dirs.values() {
            let (sub_count, sub_additions, sub_deletions) = dir.totals(files);
            count += sub_count;
            additions += sub_additions;
            deletions += sub_deletions;
        }
        (count, additions, deletions)
    }
}

/// Flatten changed files into displayable tree rows.
///
/// Directories listed in `collapsed` hide their contents; everything else is
/// expanded, so a freshly opened pull request shows all of its files.
pub fn build_rows(files: &[ChangedFile], collapsed: &BTreeSet<String>) -> Vec<TreeRow> {
    let mut root = Dir::default();
    for (index, file) in files.iter().enumerate() {
        let mut segments: Vec<&str> = file
            .path
            .split('/')
            .filter(|segment| !segment.is_empty())
            .collect();
        let Some(name) = segments.pop() else {
            continue;
        };
        let mut node = &mut root;
        for segment in segments {
            node = node.dirs.entry(segment.to_string()).or_default();
        }
        node.files.insert(name.to_string(), index);
    }

    let mut rows = Vec::new();
    flatten(&root, "", 0, files, collapsed, &mut rows);
    rows
}

fn flatten(
    dir: &Dir,
    prefix: &str,
    depth: usize,
    files: &[ChangedFile],
    collapsed: &BTreeSet<String>,
    rows: &mut Vec<TreeRow>,
) {
    for (name, child) in &dir.dirs {
        // A directory whose only content is another directory is shown as one row
        // (`a/b/c`), which keeps deep paths from wasting the pane's width.
        let mut label = name.clone();
        let mut node = child;
        while node.files.is_empty() && node.dirs.len() == 1 {
            let (child_name, grandchild) = node.dirs.iter().next().expect("one child");
            label.push('/');
            label.push_str(child_name);
            node = grandchild;
        }

        let path = if prefix.is_empty() {
            label.clone()
        } else {
            format!("{prefix}/{label}")
        };
        let expanded = !collapsed.contains(&path);
        let (count, additions, deletions) = node.totals(files);
        rows.push(TreeRow {
            depth,
            label,
            kind: RowKind::Directory {
                path: path.clone(),
                files: count,
                additions,
                deletions,
                expanded,
            },
        });
        if expanded {
            flatten(node, &path, depth + 1, files, collapsed, rows);
        }
    }

    for (name, index) in &dir.files {
        rows.push(TreeRow {
            depth,
            label: name.clone(),
            kind: RowKind::File { index: *index },
        });
    }
}

/// The row holding `row`'s parent directory, for jumping outward.
pub fn parent_row(rows: &[TreeRow], row: usize) -> Option<usize> {
    let depth = rows.get(row)?.depth;
    if depth == 0 {
        return None;
    }
    rows[..row]
        .iter()
        .rposition(|candidate| candidate.depth < depth && candidate.directory_path().is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, additions: u64, deletions: u64) -> ChangedFile {
        ChangedFile {
            path: path.to_string(),
            status: "modified".to_string(),
            additions,
            deletions,
            changes: additions + deletions,
            patch: None,
        }
    }

    fn labels(rows: &[TreeRow]) -> Vec<String> {
        rows.iter()
            .map(|row| format!("{}{}", "  ".repeat(row.depth), row.label))
            .collect()
    }

    #[test]
    fn groups_files_under_directories() {
        let files = vec![
            file("src/ui/pane.rs", 1, 1),
            file("src/app.rs", 2, 2),
            file("README.md", 3, 3),
        ];

        let rows = build_rows(&files, &BTreeSet::new());

        assert_eq!(
            labels(&rows),
            vec!["src", "  ui", "    pane.rs", "  app.rs", "README.md"]
        );
    }

    #[test]
    fn collapses_single_child_directory_chains() {
        let files = vec![
            file("PrototypeRoot/RetrievalBenchmark/README.md", 27, 18),
            file("PrototypeRoot/RetrievalBenchmark/models.py", 4, 3),
            file("PrototypeRoot/RetrievalBenchmark/tests/test_arms.py", 5, 4),
            file("data/research/notes.md", 14, 5),
        ];

        let rows = build_rows(&files, &BTreeSet::new());

        assert_eq!(
            labels(&rows),
            vec![
                "PrototypeRoot/RetrievalBenchmark",
                "  tests",
                "    test_arms.py",
                "  README.md",
                "  models.py",
                "data/research",
                "  notes.md",
            ]
        );
    }

    #[test]
    fn directory_rows_aggregate_descendant_totals() {
        let files = vec![
            file("src/a.rs", 5, 1),
            file("src/deep/b.rs", 3, 2),
            file("other.rs", 100, 100),
        ];

        let rows = build_rows(&files, &BTreeSet::new());

        let RowKind::Directory {
            files: count,
            additions,
            deletions,
            ..
        } = &rows[0].kind
        else {
            panic!("expected a directory row, got {:?}", rows[0]);
        };
        assert_eq!(*count, 2);
        assert_eq!(*additions, 8);
        assert_eq!(*deletions, 3);
    }

    #[test]
    fn collapsed_directories_hide_their_contents() {
        let files = vec![file("src/ui/pane.rs", 1, 1), file("src/app.rs", 1, 1)];
        let collapsed = BTreeSet::from(["src/ui".to_string()]);

        let rows = build_rows(&files, &collapsed);

        assert_eq!(labels(&rows), vec!["src", "  ui", "  app.rs"]);
        let RowKind::Directory { expanded, .. } = rows[1].kind else {
            panic!("expected a directory row");
        };
        assert!(!expanded);
    }

    #[test]
    fn collapsing_a_merged_chain_uses_its_full_path() {
        let files = vec![file("a/b/c.rs", 1, 1)];
        let collapsed = BTreeSet::from(["a/b".to_string()]);

        let rows = build_rows(&files, &collapsed);

        assert_eq!(labels(&rows), vec!["a/b"]);
    }

    #[test]
    fn parent_row_walks_outward() {
        // `src` holds a file of its own, so the chain cannot be collapsed.
        let files = vec![file("src/ui/pane.rs", 1, 1), file("src/main.rs", 1, 1)];
        let rows = build_rows(&files, &BTreeSet::new());

        assert_eq!(
            labels(&rows),
            vec!["src", "  ui", "    pane.rs", "  main.rs"]
        );
        assert_eq!(parent_row(&rows, 3), Some(0));
        assert_eq!(parent_row(&rows, 2), Some(1));
        assert_eq!(parent_row(&rows, 1), Some(0));
        assert_eq!(parent_row(&rows, 0), None);
    }

    #[test]
    fn file_rows_keep_their_original_indices() {
        let files = vec![file("z.rs", 1, 1), file("a.rs", 1, 1)];

        let rows = build_rows(&files, &BTreeSet::new());

        // Sorted for display, but each row still points at its source file.
        assert_eq!(labels(&rows), vec!["a.rs", "z.rs"]);
        assert_eq!(rows[0].file_index(), Some(1));
        assert_eq!(rows[1].file_index(), Some(0));
    }

    #[test]
    fn handles_no_files() {
        assert!(build_rows(&[], &BTreeSet::new()).is_empty());
    }
}
