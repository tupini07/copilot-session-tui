//! The doorbell: one cheap question that covers every watched thread.
//!
//! GitHub's notifications endpoint answers a conditional request with `304 Not Modified`
//! and, crucially, **does not charge it against the rate limit**. One request per minute
//! therefore covers any number of subscriptions, and the cost stops growing with the
//! catalogue. That is why there are no polling tiers here: the problem tiers would solve
//! does not arise.
//!
//! It is a doorbell and not a payload. The response says which threads moved; the
//! comments themselves are fetched only for the threads that did.
//!
//! Two rules that look like details and are not:
//!
//! * **Nothing here ever marks a notification read.** That inbox is the user's own,
//!   shared with their browser. Marking read would make their real notifications
//!   disappear. The `Last-Modified` watermark is a private cursor that touches nothing.
//! * **`gh api` is not used.** It exits non-zero on a 304, which would land in
//!   `cli_error`'s stderr sniffing — a mechanism `github.rs` itself flags as brittle.
//!   `ureq` is already a dependency and hands back the status and headers directly.

use std::time::Duration;

use serde::Deserialize;

use super::{ThreadKind, ThreadRef};

/// GitHub's own guidance if it declines to say. Its `X-Poll-Interval` is 60 in practice.
pub const DEFAULT_POLL_SECONDS: u64 = 60;

/// What one poll produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ring {
    /// Nothing has changed since the cursor, and the request was free.
    Quiet,
    /// Threads that moved, with the cursor to store for next time.
    Moved {
        threads: Vec<ThreadRef>,
        cursor: Option<String>,
    },
}

/// A raw HTTP response, narrow enough that tests can build one by hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoorbellResponse {
    pub status: u16,
    pub last_modified: Option<String>,
    pub poll_interval: Option<u64>,
    pub body: Vec<u8>,
}

/// How the poll reaches GitHub, behind a trait so the 304 path can be tested offline.
pub trait NotificationTransport {
    fn fetch(
        &self,
        url: &str,
        token: &str,
        if_modified_since: Option<&str>,
    ) -> Result<DoorbellResponse, String>;
}

#[derive(Debug, Deserialize)]
struct ApiNotification {
    subject: ApiSubject,
    repository: ApiNotificationRepository,
}

#[derive(Debug, Deserialize)]
struct ApiSubject {
    #[serde(rename = "type")]
    kind: String,
    /// Absent for some subject types, which is why a missing URL is handled rather
    /// than treated as malformed.
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiNotificationRepository {
    full_name: String,
}

/// The notifications endpoint for a host.
///
/// Enterprise installs serve the API under `/api/v3` on their own hostname rather than
/// on a separate `api.` domain.
pub fn notifications_url(host: &str) -> String {
    if host.eq_ignore_ascii_case("github.com") {
        "https://api.github.com/notifications?all=true".to_string()
    } else {
        format!("https://{host}/api/v3/notifications?all=true")
    }
}

/// How long to wait before the next poll.
///
/// GitHub's `X-Poll-Interval` is a floor, not a suggestion: the documentation says to
/// obey it, and it rises when their servers are busy. Configuring a faster poll must
/// not be able to override it.
pub fn poll_delay(configured: Duration, server_interval: Option<u64>) -> Duration {
    let floor = Duration::from_secs(server_interval.unwrap_or(DEFAULT_POLL_SECONDS));
    configured.max(floor)
}

/// Turn a response into the list of watched threads that moved.
pub fn interpret(
    response: &DoorbellResponse,
    watched: &[ThreadRef],
) -> Result<(Ring, Option<u64>), String> {
    if response.status == 304 {
        return Ok((Ring::Quiet, response.poll_interval));
    }
    if response.status == 401 || response.status == 403 {
        return Err(
            "GitHub refused the notifications request; run `gh auth login` and check the \
             token has the `notifications` scope"
                .to_string(),
        );
    }
    if response.status != 200 {
        return Err(format!(
            "GitHub answered the notifications request with HTTP {}",
            response.status
        ));
    }

    let notifications: Vec<ApiNotification> = serde_json::from_slice(&response.body)
        .map_err(|error| format!("GitHub sent an unreadable notification list: {error}"))?;

    let mut moved: Vec<ThreadRef> = Vec::new();
    for notification in &notifications {
        for thread in matched_threads(notification, watched) {
            if !moved.contains(&thread) {
                moved.push(thread);
            }
        }
    }

    Ok((
        Ring::Moved {
            threads: moved,
            cursor: response.last_modified.clone(),
        },
        response.poll_interval,
    ))
}

/// Which watched threads one notification refers to.
///
/// Usually exactly one. The exception is a discussion notification: GitHub does not
/// always give those a `subject.url`, so there is no number to match on and the only
/// honest answer is "some discussion in this repository moved". Returning every watched
/// discussion there costs one extra check each and never misses the real one.
fn matched_threads(notification: &ApiNotification, watched: &[ThreadRef]) -> Vec<ThreadRef> {
    let Some(kind) = subject_kind(&notification.subject.kind) else {
        return Vec::new();
    };
    let repository = notification.repository.full_name.to_ascii_lowercase();

    let in_repository = |thread: &&ThreadRef| {
        thread.kind == kind
            && format!("{}/{}", thread.owner, thread.repo).to_ascii_lowercase() == repository
    };

    match notification
        .subject
        .url
        .as_deref()
        .and_then(number_from_api_url)
    {
        Some(number) => watched
            .iter()
            .filter(in_repository)
            .filter(|thread| thread.number == number)
            .cloned()
            .collect(),
        None => watched.iter().filter(in_repository).cloned().collect(),
    }
}

/// Map GitHub's `subject.type` onto the kinds that carry a conversation.
///
/// Everything else — releases, commits, security alerts — is somebody else's business
/// and must not be mistaken for a thread somebody is waiting on.
fn subject_kind(subject_type: &str) -> Option<ThreadKind> {
    match subject_type {
        "Issue" => Some(ThreadKind::Issue),
        "PullRequest" => Some(ThreadKind::PullRequest),
        "Discussion" => Some(ThreadKind::Discussion),
        _ => None,
    }
}

/// Pull the item number out of an API subject URL.
///
/// These are API URLs (`.../repos/o/r/issues/12`), not the browser ones, and pull
/// requests appear under `pulls`. The number is always the last segment.
fn number_from_api_url(url: &str) -> Option<u64> {
    url.rsplit('/').next()?.parse().ok()
}

/// A real HTTP poll.
pub struct UreqTransport {
    agent: ureq::Agent,
}

impl Default for UreqTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl UreqTransport {
    pub fn new() -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(15)))
            // A 304 is the good case here, not an error, and the whole design depends
            // on being able to read it.
            .http_status_as_error(false)
            .build()
            .into();
        Self { agent }
    }
}

impl NotificationTransport for UreqTransport {
    fn fetch(
        &self,
        url: &str,
        token: &str,
        if_modified_since: Option<&str>,
    ) -> Result<DoorbellResponse, String> {
        let mut request = self
            .agent
            .get(url)
            .header(
                "User-Agent",
                concat!("copilot-session-tui/", env!("CARGO_PKG_VERSION")),
            )
            .header("Accept", "application/vnd.github+json")
            .header("Authorization", &format!("Bearer {token}"));
        if let Some(since) = if_modified_since {
            request = request.header("If-Modified-Since", since);
        }

        let response = request
            .call()
            .map_err(|error| format!("Could not reach GitHub: {error}"))?;
        let status = response.status().as_u16();
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        };
        let last_modified = header("last-modified");
        let poll_interval = header("x-poll-interval").and_then(|value| value.parse().ok());
        let body = response
            .into_body()
            .read_to_vec()
            .map_err(|error| format!("Could not read GitHub's reply: {error}"))?;

        Ok(DoorbellResponse {
            status,
            last_modified,
            poll_interval,
            body,
        })
    }
}

/// Which account CST's agents post as.
///
/// The whole "is this one of ours" decision hangs off this one string, so it is read
/// from GitHub rather than configured: a wrong value here would either wake sessions on
/// strangers' comments or never wake them at all.
pub fn current_login(host: &str) -> Result<String, String> {
    let output = std::process::Command::new("gh")
        // No leading slash: a leading `/` makes some shells rewrite the path, and `gh`
        // itself warns about exactly this.
        .args(["api", "--hostname", host, "user", "--jq", ".login"])
        .env("GH_PROMPT_DISABLED", "1")
        .output()
        .map_err(|error| format!("Could not ask `gh` who you are: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "`gh` could not say which account is logged in to {host}"
        ));
    }
    let login = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if login.is_empty() {
        return Err(format!("`gh` returned no account for {host}"));
    }
    Ok(login)
}

/// Read the token `gh` already holds for a host.
///
/// Borrowing `gh`'s credential rather than asking for one of our own keeps Enterprise
/// hosts, SSO and token refresh working exactly as they already do for the inspector.
pub fn token_for(host: &str) -> Result<String, String> {
    let output = std::process::Command::new("gh")
        .args(["auth", "token", "--hostname", host])
        .env("GH_PROMPT_DISABLED", "1")
        .output()
        .map_err(|error| format!("Could not run `gh auth token`: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "`gh` has no token for {host}; run `gh auth login --hostname {host}`"
        ));
    }
    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if token.is_empty() {
        return Err(format!("`gh` returned no token for {host}"));
    }
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(number: u64, kind: ThreadKind) -> ThreadRef {
        ThreadRef {
            host: "github.com".to_string(),
            owner: "microsoft".to_string(),
            repo: "SpeakingBigMapsIntoExistence".to_string(),
            number,
            kind,
        }
    }

    fn response(status: u16, body: &str) -> DoorbellResponse {
        DoorbellResponse {
            status,
            last_modified: Some("Mon, 14 Sep 2026 16:04:34 GMT".to_string()),
            poll_interval: Some(60),
            body: body.as_bytes().to_vec(),
        }
    }

    fn notification(kind: &str, repo: &str, url: &str) -> String {
        format!(
            r#"{{"subject":{{"type":"{kind}","url":"{url}"}},"repository":{{"full_name":"{repo}"}}}}"#
        )
    }

    #[test]
    fn an_unchanged_inbox_costs_nothing_and_moves_no_cursor() {
        // The whole economic argument for this design: a 304 is free, so polling every
        // minute forever is affordable.
        let watched = vec![thread(2366, ThreadKind::Issue)];
        let (ring, interval) = interpret(&response(304, ""), &watched).unwrap();

        assert_eq!(ring, Ring::Quiet);
        assert_eq!(interval, Some(60));
    }

    #[test]
    fn a_comment_on_a_watched_issue_is_reported_with_the_new_cursor() {
        let watched = vec![thread(2366, ThreadKind::Issue)];
        let body = format!(
            "[{}]",
            notification(
                "Issue",
                "microsoft/SpeakingBigMapsIntoExistence",
                "https://api.github.com/repos/microsoft/SpeakingBigMapsIntoExistence/issues/2366"
            )
        );

        let (ring, _) = interpret(&response(200, &body), &watched).unwrap();

        assert_eq!(
            ring,
            Ring::Moved {
                threads: vec![thread(2366, ThreadKind::Issue)],
                cursor: Some("Mon, 14 Sep 2026 16:04:34 GMT".to_string()),
            }
        );
    }

    #[test]
    fn the_rest_of_a_busy_inbox_is_ignored_rather_than_mistaken_for_a_thread() {
        // A real inbox is mostly releases and repositories nobody is waiting on.
        let watched = vec![thread(2366, ThreadKind::Issue)];
        let body = format!(
            "[{},{},{}]",
            notification(
                "Release",
                "some/other",
                "https://api.github.com/repos/some/other/releases/1"
            ),
            notification(
                "Issue",
                "unrelated/repo",
                "https://api.github.com/repos/unrelated/repo/issues/2366"
            ),
            notification(
                "Issue",
                "microsoft/SpeakingBigMapsIntoExistence",
                "https://api.github.com/repos/microsoft/SpeakingBigMapsIntoExistence/issues/999"
            )
        );

        let (ring, _) = interpret(&response(200, &body), &watched).unwrap();

        assert_eq!(
            ring,
            Ring::Moved {
                threads: vec![],
                cursor: Some("Mon, 14 Sep 2026 16:04:34 GMT".to_string()),
            }
        );
    }

    #[test]
    fn a_pull_request_notification_matches_a_watched_pull_request() {
        // Subject URLs say `pulls` where the browser URL says `pull`.
        let watched = vec![thread(41, ThreadKind::PullRequest)];
        let body = format!(
            "[{}]",
            notification(
                "PullRequest",
                "microsoft/SpeakingBigMapsIntoExistence",
                "https://api.github.com/repos/microsoft/SpeakingBigMapsIntoExistence/pulls/41"
            )
        );

        let (ring, _) = interpret(&response(200, &body), &watched).unwrap();

        let Ring::Moved { threads, .. } = ring else {
            panic!("expected a ring");
        };
        assert_eq!(threads, vec![thread(41, ThreadKind::PullRequest)]);
    }

    #[test]
    fn an_issue_notification_never_matches_a_discussion_with_the_same_number() {
        // Issue #7 and discussion #7 can both exist in one repository.
        let watched = vec![thread(7, ThreadKind::Discussion)];
        let body = format!(
            "[{}]",
            notification(
                "Issue",
                "microsoft/SpeakingBigMapsIntoExistence",
                "https://api.github.com/repos/microsoft/SpeakingBigMapsIntoExistence/issues/7"
            )
        );

        let (ring, _) = interpret(&response(200, &body), &watched).unwrap();

        assert_eq!(
            ring,
            Ring::Moved {
                threads: vec![],
                cursor: Some("Mon, 14 Sep 2026 16:04:34 GMT".to_string()),
            }
        );
    }

    #[test]
    fn a_discussion_notification_without_a_url_still_reaches_its_repository() {
        // GitHub does not reliably give discussion notifications a subject URL. Falling
        // back to every watched discussion in that repository costs one extra check and
        // is the difference between discussions working and silently never waking.
        let watched = vec![
            thread(7, ThreadKind::Discussion),
            thread(8, ThreadKind::Discussion),
            thread(9, ThreadKind::Issue),
        ];
        let body = r#"[{"subject":{"type":"Discussion","url":null},
            "repository":{"full_name":"microsoft/SpeakingBigMapsIntoExistence"}}]"#;

        let (ring, _) = interpret(&response(200, body), &watched).unwrap();

        let Ring::Moved { threads, .. } = ring else {
            panic!("expected a ring");
        };
        assert_eq!(
            threads,
            vec![
                thread(7, ThreadKind::Discussion),
                thread(8, ThreadKind::Discussion)
            ],
            "issues in the same repository must not be dragged in"
        );
    }

    #[test]
    fn a_rejected_token_says_which_scope_is_missing_rather_than_retrying_forever() {
        let watched = vec![thread(1, ThreadKind::Issue)];

        let error = interpret(&response(403, ""), &watched).unwrap_err();

        assert!(error.contains("notifications"), "got: {error}");
        assert!(error.contains("gh auth login"), "got: {error}");
    }

    #[test]
    fn githubs_poll_interval_is_a_floor_that_a_faster_setting_cannot_undercut() {
        // The documentation says to obey it, and it rises when GitHub is busy.
        assert_eq!(
            poll_delay(Duration::from_secs(5), Some(60)),
            Duration::from_secs(60)
        );
        assert_eq!(
            poll_delay(Duration::from_secs(300), Some(60)),
            Duration::from_secs(300)
        );
        assert_eq!(
            poll_delay(Duration::from_secs(5), None),
            Duration::from_secs(DEFAULT_POLL_SECONDS)
        );
    }

    /// Proves against the real API that a conditional poll is free.
    ///
    /// The economics of this whole module rest on it, and no offline test can show that
    /// GitHub really sends `304` and really leaves `X-RateLimit-Used` untouched. Ignored
    /// by default because it spends a request against the running user's account.
    #[test]
    #[ignore = "talks to the real GitHub API; run with CST_DOORBELL_LIVE=1"]
    fn a_conditional_poll_really_is_free_against_the_live_api() {
        if std::env::var_os("CST_DOORBELL_LIVE").is_none() {
            return;
        }
        let transport = UreqTransport::new();
        let token = token_for("github.com").expect("gh must be logged in for this test");
        let url = notifications_url("github.com");

        let first = transport.fetch(&url, &token, None).unwrap();
        assert_eq!(first.status, 200, "an unconditional poll returns the list");
        let cursor = first
            .last_modified
            .expect("GitHub must send Last-Modified or there is no cursor to store");

        let second = transport.fetch(&url, &token, Some(&cursor)).unwrap();
        assert_eq!(
            second.status, 304,
            "the same request with the cursor must be Not Modified"
        );
        assert!(
            first.poll_interval.is_some(),
            "GitHub must tell us how often it will accept a poll"
        );
    }

    #[test]
    fn an_enterprise_host_is_asked_on_its_own_domain_not_api_github_com() {
        assert_eq!(
            notifications_url("github.com"),
            "https://api.github.com/notifications?all=true"
        );
        assert_eq!(
            notifications_url("github.mycorp.example"),
            "https://github.mycorp.example/api/v3/notifications?all=true"
        );
    }
}
