//! The changelog CST shows after it updates itself.
//!
//! `CHANGELOG.md` is compiled into the binary rather than fetched. The post-update
//! screen only ever needs versions at or below the one now running, and every one of
//! those sections was, by construction, in the file committed when this binary was
//! tagged. Embedding therefore removes the offline case, the unauthenticated rate limit
//! shared with the update check, and any cache to invalidate. The cost is that a typo
//! fixed after a release never reaches anyone who already has it.
//!
//! Deleting `CHANGELOG.md` breaks the build, which is the point.

use crate::app_state;

const RAW: &str = include_str!("../CHANGELOG.md");

/// How many releases a single screen will show before it starts counting instead.
///
/// At this project's cadence that is a few weeks of change, which is about as far back
/// as anyone reads. Past it a list stops being information.
const MAX_RELEASES: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub date: String,
    pub bullets: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhatsNew {
    /// The version the user was on. Empty when that is not known.
    pub from_version: String,
    pub current_version: String,
    /// Newest first, capped at [`MAX_RELEASES`].
    pub releases: Vec<Release>,
    /// Releases in the span that did not fit.
    pub skipped: usize,
    /// Oldest release in the span, named so the count is never mysterious.
    pub oldest_skipped: Option<String>,
}

impl WhatsNew {
    pub fn is_empty(&self) -> bool {
        self.releases.is_empty()
    }
}

/// What to show at startup, advancing the marker as a side effect.
///
/// The marker moves here rather than when the screen is dismissed: a user who quits or
/// crashes without dismissing would otherwise see the same screen on every launch
/// forever. The cost is one lost screen after a crash, and the command palette can
/// reopen it.
pub fn whats_new_on_startup() -> Option<WhatsNew> {
    whats_new_on_startup_in(
        &app_state::state_root(),
        &parse(RAW),
        env!("CARGO_PKG_VERSION"),
    )
}

fn whats_new_on_startup_in(
    root: &std::path::Path,
    releases: &[Release],
    current: &str,
) -> Option<WhatsNew> {
    // Order matters: the span has to be computed against the *old* marker, before it
    // is advanced. Advancing first would leave every span empty and quietly retire the
    // whole feature.
    let previous = app_state::load_in(root).last_ran_version;
    let result = span(releases, &previous, current);

    // Advancing only moves it forward, so an older instance running alongside cannot
    // drag it back and make this one repeat itself.
    let _ = app_state::advance_last_ran_version_in(root, current);

    result.filter(|whats_new| !whats_new.is_empty())
}

/// The same screen on demand, for the command palette. Never touches the marker.
pub fn whats_new_for_current_version() -> WhatsNew {
    let current = env!("CARGO_PKG_VERSION");
    let releases = parse(RAW);
    let shown: Vec<Release> = releases.iter().take(MAX_RELEASES).cloned().collect();
    let skipped = releases.len().saturating_sub(shown.len());
    WhatsNew {
        from_version: String::new(),
        current_version: current.to_string(),
        oldest_skipped: (skipped > 0)
            .then(|| releases.last().map(|release| release.version.clone()))
            .flatten(),
        releases: shown,
        skipped,
    }
}

/// Releases strictly newer than `from`, up to and including `to`.
///
/// Returns `None` when there is nothing to say: an unknown previous version (a fresh
/// install, which must not be shown the entire history) or a previous version at or
/// above the current one (a downgrade).
fn span(all: &[Release], from: &str, to: &str) -> Option<WhatsNew> {
    let from_version = semver::Version::parse(from).ok()?;
    let to_version = semver::Version::parse(to).ok()?;
    if from_version >= to_version {
        return None;
    }

    // Filtering the changelog's own versions by range, rather than diffing tag lists,
    // tolerates a tagged version that never got a section.
    let in_span: Vec<Release> = all
        .iter()
        .filter(|release| match semver::Version::parse(&release.version) {
            Ok(version) => version > from_version && version <= to_version,
            Err(_) => false,
        })
        .cloned()
        .collect();
    if in_span.is_empty() {
        return None;
    }

    let shown: Vec<Release> = in_span.iter().take(MAX_RELEASES).cloned().collect();
    let skipped = in_span.len().saturating_sub(shown.len());
    Some(WhatsNew {
        from_version: from.to_string(),
        current_version: to.to_string(),
        oldest_skipped: (skipped > 0)
            .then(|| in_span.last().map(|release| release.version.clone()))
            .flatten(),
        releases: shown,
        skipped,
    })
}

/// Parse the file into releases, newest first.
///
/// Intentionally strict and tiny: the format is a contract the CI extractor and the
/// release skill also depend on, and a lenient parser here would let the file drift
/// away from what they can read.
fn parse(raw: &str) -> Vec<Release> {
    let mut releases: Vec<Release> = Vec::new();
    for line in raw.lines() {
        if let Some((version, date)) = parse_heading(line) {
            releases.push(Release {
                version,
                date,
                bullets: Vec::new(),
            });
        } else if let Some(bullet) = line.strip_prefix("- ") {
            if let Some(release) = releases.last_mut() {
                let bullet = bullet.trim();
                if !bullet.is_empty() {
                    release.bullets.push(bullet.to_string());
                }
            }
        }
    }
    releases
}

/// `## v0.29.0 - 2026-09-14` into its two halves.
fn parse_heading(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("## v")?;
    let (version, date) = rest.split_once(" - ")?;
    let version = version.trim();
    let date = date.trim();
    if version.is_empty() || date.is_empty() {
        return None;
    }
    Some((version.to_string(), date.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_changelog_has_a_section_for_the_version_being_built() {
        // The keystone. This makes "released without writing the changelog" a failing
        // test rather than a failed release, which is what lets the release workflow
        // refuse to publish without one.
        let current = env!("CARGO_PKG_VERSION");
        let releases = parse(RAW);
        let section = releases
            .iter()
            .find(|release| release.version == current)
            .unwrap_or_else(|| {
                panic!("CHANGELOG.md has no `## v{current}` section; write one before releasing")
            });
        assert!(
            (1..=10).contains(&section.bullets.len()),
            "v{current} has {} bullets; the changelog is for humans",
            section.bullets.len()
        );
    }

    #[test]
    fn every_section_is_flat_bullets_that_survive_plain_text_rendering() {
        // CST renders this file with no markdown renderer, so anything fancier than a
        // flat bullet shows up literally. Enforcing it here means a stray backtick in a
        // contributor's PR fails tests instead of rendering as noise in the popup.
        let mut seen_first_heading = false;
        for (number, line) in RAW.lines().enumerate() {
            let where_ = format!("CHANGELOG.md line {}", number + 1);
            if line.starts_with("## ") {
                assert!(
                    parse_heading(line).is_some(),
                    "{where_}: heading must be `## vX.Y.Z - YYYY-MM-DD`, got {line:?}"
                );
                seen_first_heading = true;
                continue;
            }
            if !seen_first_heading {
                continue; // the title and intro block
            }
            if line.trim().is_empty() {
                continue;
            }
            assert!(
                line.starts_with("- "),
                "{where_}: expected a `- ` bullet at column 0, got {line:?}"
            );
            for forbidden in ['`', '*', '_', '|', '#'] {
                assert!(
                    !line.contains(forbidden),
                    "{where_}: {forbidden:?} renders literally in the What's New screen"
                );
            }
            assert!(
                !line.contains("]("),
                "{where_}: links render literally; write the URL or leave it out"
            );
        }
    }

    #[test]
    fn sections_are_newest_first_with_no_repeated_version() {
        let releases = parse(RAW);
        assert!(releases.len() >= 2, "expected a backfilled changelog");
        let mut seen = std::collections::BTreeSet::new();
        let mut previous: Option<semver::Version> = None;
        for release in &releases {
            let version = semver::Version::parse(&release.version)
                .unwrap_or_else(|_| panic!("v{} is not a version", release.version));
            assert!(
                seen.insert(release.version.clone()),
                "v{} appears twice",
                release.version
            );
            if let Some(previous) = previous {
                assert!(
                    version < previous,
                    "v{version} is listed below v{previous}; newest goes first"
                );
            }
            previous = Some(version);
        }
    }

    fn release(version: &str) -> Release {
        Release {
            version: version.to_string(),
            date: "2026-01-01".to_string(),
            bullets: vec![format!("something in {version}")],
        }
    }

    fn history() -> Vec<Release> {
        ["0.30.0", "0.29.0", "0.28.0", "0.27.0", "0.26.0", "0.25.0"]
            .iter()
            .map(|version| release(version))
            .collect()
    }

    #[test]
    fn only_the_releases_between_the_two_versions_are_shown() {
        let whats_new = span(&history(), "0.28.0", "0.30.0").expect("a span to show");
        let versions: Vec<&str> = whats_new
            .releases
            .iter()
            .map(|release| release.version.as_str())
            .collect();
        assert_eq!(versions, ["0.30.0", "0.29.0"]);
        assert_eq!(whats_new.skipped, 0);
        assert_eq!(whats_new.from_version, "0.28.0");
    }

    #[test]
    fn a_long_span_is_bounded_and_says_how_many_releases_were_left_out() {
        let whats_new = span(&history(), "0.20.0", "0.30.0").expect("a span to show");
        assert_eq!(whats_new.releases.len(), MAX_RELEASES);
        assert_eq!(whats_new.skipped, 1);
        assert_eq!(
            whats_new.oldest_skipped.as_deref(),
            Some("0.25.0"),
            "the truncation has to name where it stops, not just that it did"
        );
    }

    #[test]
    fn a_fresh_install_is_shown_nothing_rather_than_every_release_ever() {
        assert_eq!(span(&history(), "", "0.30.0"), None);
        assert_eq!(span(&history(), "not-a-version", "0.30.0"), None);
    }

    #[test]
    fn running_the_same_version_again_shows_nothing() {
        assert_eq!(span(&history(), "0.30.0", "0.30.0"), None);
    }

    #[test]
    fn a_downgrade_shows_nothing_rather_than_notes_for_versions_you_no_longer_have() {
        assert_eq!(span(&history(), "0.30.0", "0.28.0"), None);
    }

    #[test]
    fn a_tagged_version_with_no_changelog_section_does_not_break_the_span() {
        // Filtering the changelog rather than the tag list is what makes this safe.
        let sparse = vec![release("0.30.0"), release("0.27.0")];
        let whats_new = span(&sparse, "0.26.0", "0.30.0").expect("a span to show");
        assert_eq!(whats_new.releases.len(), 2);
    }

    #[test]
    fn the_palette_entry_shows_recent_releases_without_needing_a_previous_version() {
        let whats_new = whats_new_for_current_version();
        assert!(!whats_new.is_empty(), "the embedded changelog is not empty");
        assert!(whats_new.from_version.is_empty());
        assert_eq!(whats_new.current_version, env!("CARGO_PKG_VERSION"));
    }
    #[test]
    fn the_span_is_computed_before_the_marker_moves_and_the_screen_shows_once() {
        let temp = tempfile::tempdir().unwrap();
        crate::app_state::advance_last_ran_version_in(temp.path(), "0.27.0").unwrap();

        let first = whats_new_on_startup_in(temp.path(), &history(), "0.29.0")
            .expect("an update should have something to say");
        let versions: Vec<&str> = first
            .releases
            .iter()
            .map(|release| release.version.as_str())
            .collect();
        assert_eq!(versions, ["0.29.0", "0.28.0"]);

        // Second launch of the same version: the marker has caught up, so nothing.
        assert_eq!(
            whats_new_on_startup_in(temp.path(), &history(), "0.29.0"),
            None,
            "the screen must not reappear on every launch"
        );
        assert_eq!(
            crate::app_state::load_in(temp.path()).last_ran_version,
            "0.29.0"
        );
    }

    #[test]
    fn a_first_ever_launch_records_the_version_without_showing_anything() {
        let temp = tempfile::tempdir().unwrap();

        assert_eq!(
            whats_new_on_startup_in(temp.path(), &history(), "0.29.0"),
            None,
            "a new user has not missed anything"
        );
        // But the marker is set, so their *next* update does show notes.
        assert_eq!(
            crate::app_state::load_in(temp.path()).last_ran_version,
            "0.29.0"
        );
        assert!(whats_new_on_startup_in(temp.path(), &history(), "0.30.0").is_some());
    }
}
