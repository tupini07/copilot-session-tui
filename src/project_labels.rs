//! What a project is called in the filter popup, the session list and the title bar.
//!
//! A project's name is the last component of its root, which is the right length for a
//! list and almost always enough. It stops being enough when several roots end the same
//! way: a tree of `requests/c01/inputs`, `requests/c02/inputs`, `judgments/c05/inputs`
//! folders, none of them in a Git repository, is sixteen projects that all print as
//! `inputs` and cannot be told apart or searched for. So each label is the shortest
//! trailing part of its root that no other root shares; roots whose last component is
//! already unique keep the short name they always had.

use std::collections::HashMap;
use std::path::{Component, Path, MAIN_SEPARATOR_STR};

/// Labels for `roots`, in the same order, each as short as it can be while differing
/// from every other.
pub fn distinct_labels(roots: &[String]) -> Vec<String> {
    let parts: Vec<Vec<String>> = roots.iter().map(|root| components(root)).collect();
    let longest = parts.iter().map(Vec::len).max().unwrap_or(0);
    let mut labels = Vec::with_capacity(roots.len());
    for (index, own) in parts.iter().enumerate() {
        let mut depth = 1;
        while depth < own.len() && depth < longest {
            let clash = parts
                .iter()
                .enumerate()
                .any(|(other, theirs)| other != index && same_tail(own, theirs, depth));
            if !clash {
                break;
            }
            depth += 1;
        }
        labels.push(if depth >= own.len() {
            // Everything the root has, so show it as it was written rather than rebuilt
            // from components and missing its drive or leading slash.
            roots[index].clone()
        } else {
            let tail = &own[own.len() - depth..];
            tail.join(MAIN_SEPARATOR_STR)
        });
    }
    labels
}

/// Labels keyed by root, for callers that look a project up rather than list them.
pub fn label_map(roots: &[String]) -> HashMap<String, String> {
    roots.iter().cloned().zip(distinct_labels(roots)).collect()
}

/// The last component alone, for a root that is not in any list: the name a project
/// had before labels learned to tell twins apart.
pub fn short_name(root: &str) -> &str {
    Path::new(root)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(root)
}

fn components(root: &str) -> Vec<String> {
    Path::new(root)
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            // The drive or leading slash is never a useful suffix on its own; a root
            // that needs it needs all of itself, and gets the original string.
            _ => None,
        })
        .collect()
}

fn same_tail(own: &[String], theirs: &[String], depth: usize) -> bool {
    if theirs.len() < depth {
        return false;
    }
    // Paths compare case-insensitively everywhere else in CST, because Windows does.
    own[own.len() - depth..]
        .iter()
        .zip(&theirs[theirs.len() - depth..])
        .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::MAIN_SEPARATOR;

    fn roots(list: &[&str]) -> Vec<String> {
        list.iter()
            .map(|root| root.replace('/', MAIN_SEPARATOR_STR))
            .collect()
    }

    fn labels(list: &[&str]) -> Vec<String> {
        distinct_labels(&roots(list))
            .into_iter()
            .map(|label| label.replace(MAIN_SEPARATOR, "/"))
            .collect()
    }

    #[test]
    fn a_project_whose_name_nobody_else_has_keeps_just_its_name() {
        assert_eq!(
            labels(&["/work/alpha", "/work/beta", "/home/me/gamma"]),
            vec!["alpha", "beta", "gamma"]
        );
    }

    #[test]
    fn projects_ending_the_same_way_show_as_much_of_their_path_as_tells_them_apart() {
        // Sixteen rows all reading `inputs` was the bug: distinct folders in one
        // orchestration tree, none a Git repository, so each is its own project.
        assert_eq!(
            labels(&[
                "/big/requests/c01-r00/inputs",
                "/big/requests/c03-r00/inputs",
                "/big/judgments/c05/inputs",
                "/work/alpha",
            ]),
            vec!["c01-r00/inputs", "c03-r00/inputs", "c05/inputs", "alpha"]
        );
    }

    #[test]
    fn a_clash_deeper_down_takes_as_many_components_as_it_needs() {
        assert_eq!(
            labels(&["/a/requests/c01/inputs", "/b/judgments/c01/inputs"]),
            vec!["requests/c01/inputs", "judgments/c01/inputs"]
        );
    }

    #[test]
    fn a_root_that_is_entirely_the_tail_of_another_is_shown_in_full() {
        // Trimmed to components it would read `a/inputs` for both; the shorter one has
        // nothing more to add, so it is shown as written, drive or slash included.
        let list = roots(&["/a/inputs", "/x/a/inputs"]);
        assert_eq!(distinct_labels(&list), list);
    }

    #[test]
    fn the_same_root_written_in_two_cases_does_not_loop_forever() {
        let list = roots(&["/Work/Alpha", "/work/alpha"]);
        assert_eq!(distinct_labels(&list), list);
    }

    #[test]
    fn case_differences_alone_do_not_make_two_names_distinct() {
        // `Inputs` and `inputs` are one folder name on Windows; showing them as if they
        // differed would send the eye looking for a difference that is not there.
        assert_eq!(
            labels(&["/a/one/Inputs", "/a/two/inputs"]),
            vec!["one/Inputs", "two/inputs"]
        );
    }

    #[test]
    fn the_map_answers_by_root_and_an_empty_list_is_fine() {
        assert!(distinct_labels(&[]).is_empty());
        // Two components each, so telling them apart takes the whole root, drive or
        // slash included: `C:\alpha` and `D:\alpha` differ only there.
        let list = roots(&["/w/alpha", "/v/alpha"]);
        let map = label_map(&list);
        assert_eq!(map[&list[0]], list[0]);
        assert_eq!(map[&list[1]], list[1]);
        assert_eq!(short_name(&list[0]), "alpha");
    }
}
