//! Noticing that a watched thread has moved, without paying for the check.
//!
//! ## Why this does not use the notifications inbox
//!
//! The obvious design is one conditional `GET /notifications`, which covers every
//! subscription in a single free request. It was built that way first, and it does not
//! work, for a reason that is invisible until you try it against a real account:
//!
//! **GitHub never notifies you about your own activity, and every CST agent comments as
//! the same account.** Verified rather than reasoned about — an agent posted to a real
//! issue and the inbox stayed at `304` for half a minute afterwards, with the thread
//! absent from the notification list entirely. The one case the inbox *would* have
//! reported is a comment by somebody else, which is exactly the case CST refuses to act
//! on automatically. It would have rung only when it must not wake anything.
//!
//! So each watched thread is asked about directly. That costs one request per thread
//! instead of one in total, and buys back three things: it sees same-account traffic,
//! which is the entire point; it touches the user's notification inbox not at all, so
//! there is no way to eat their unread badges; and it works for discussions without
//! depending on whether those appear in notifications.
//!
//! The cost stays near zero anyway. `GET /repos/{o}/{r}/issues/{n}` returns an `ETag`,
//! and a request carrying `If-None-Match` answers `304` **without being charged against
//! the rate limit** — confirmed against the live API. A handful of watched threads is a
//! handful of free requests a minute.
//!
//! `ureq` and not `gh api`: `gh` exits non-zero on a `304`, which would land in
//! `cli_error`'s stderr sniffing, a mechanism `github.rs` itself flags as brittle.

use std::time::Duration;

use super::{ThreadKind, ThreadRef};

/// How often to ask, when nothing says otherwise.
pub const DEFAULT_POLL_SECONDS: u64 = 60;

/// Why a poll came back without an answer, sorted by what would fix it.
///
/// The watcher treats these very differently, which is the reason they are not a plain
/// string. GitHub being out of reach mends itself, so the watcher keeps asking; a
/// refused token waits for the user to log in; one unreadable thread says nothing about
/// the others.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollError {
    /// No answer, or GitHub's own error: a network that is down, a laptop waking from
    /// sleep, an outage. Nothing to do but ask again.
    Unreachable(String),
    /// The credential was turned away. The same token will be turned away again.
    Refused(String),
    /// This one thread can no longer be read.
    Gone(String),
}

impl PollError {
    /// The `gh` path, used for discussions and comment lists, sorted the same way.
    pub fn from_github(error: crate::github::GithubError) -> Self {
        use crate::github::GithubErrorKind;
        match error.kind {
            GithubErrorKind::Authentication => Self::Refused(error.message),
            GithubErrorKind::NotFound => Self::Gone(error.message),
            // `gh` reports a dropped connection as a generic failure, and a guess that
            // retries costs far less than one that stops watching for good.
            _ => Self::Unreachable(error.message),
        }
    }

    /// What the user is told, saying first whether anything is still being watched:
    /// a long network error is cut off at the edge of the status line, and that is the
    /// part that has to survive.
    pub fn describe(&self, thread: &ThreadRef) -> String {
        // `gh` explains itself over several lines, and a status line has one.
        let first_line = |detail: &str| detail.lines().next().unwrap_or_default().to_string();
        match self {
            Self::Unreachable(detail) => {
                format!("Thread watching will keep retrying: {}", first_line(detail))
            }
            Self::Refused(detail) => {
                format!(
                    "Thread watching is waiting for a login: {}",
                    first_line(detail)
                )
            }
            Self::Gone(detail) => format!(
                "Thread watching cannot read {}: {}",
                thread.url(),
                first_line(detail)
            ),
        }
    }
}

/// What one thread poll produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ring {
    /// Unchanged since the stored cursor, and the request was free.
    Quiet,
    /// Something happened; `cursor` is what to send next time.
    Moved { cursor: Option<String> },
}

/// A raw HTTP response, narrow enough that tests can build one by hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoorbellResponse {
    pub status: u16,
    /// `ETag`, the cursor for the next conditional request.
    pub cursor: Option<String>,
    pub body: Vec<u8>,
}

/// How the poll reaches GitHub, behind a trait so the 304 path can be tested offline.
pub trait ThreadTransport {
    fn fetch(
        &self,
        url: &str,
        token: &str,
        if_none_match: Option<&str>,
    ) -> Result<DoorbellResponse, String>;
}

/// The API URL that reflects a thread's activity.
///
/// Pull requests are served by the `issues` endpoint as well, and it is the one that
/// changes when somebody comments — `pulls/{n}` tracks the branch, not the conversation.
///
/// Discussions have no REST representation at all; they are handled through GraphQL
/// elsewhere, so this returns `None` and the caller takes the other path.
pub fn thread_api_url(thread: &ThreadRef) -> Option<String> {
    if thread.kind == ThreadKind::Discussion {
        return None;
    }
    let base = if thread.host.eq_ignore_ascii_case("github.com") {
        "https://api.github.com".to_string()
    } else {
        // Enterprise serves its API under /api/v3 on its own hostname.
        format!("https://{}/api/v3", thread.host)
    };
    Some(format!(
        "{base}/repos/{}/{}/issues/{}",
        thread.owner, thread.repo, thread.number
    ))
}

/// How long to wait before the next round of polls.
pub fn poll_delay(configured: Duration) -> Duration {
    configured.max(Duration::from_secs(5))
}

/// Turn a response into whether this thread moved.
pub fn interpret(response: &DoorbellResponse) -> Result<Ring, PollError> {
    match response.status {
        304 => Ok(Ring::Quiet),
        200 => Ok(Ring::Moved {
            cursor: response.cursor.clone(),
        }),
        401 | 403 => Err(PollError::Refused(
            "GitHub refused the request; run `gh auth login` and check the token can read \
             this repository"
                .to_string(),
        )),
        404 | 410 => Err(PollError::Gone(
            "it may have been deleted or made private".to_string(),
        )),
        // A 5xx or a 429 is GitHub having a bad moment, which passes like a dropped
        // connection does.
        other => Err(PollError::Unreachable(format!(
            "GitHub answered with HTTP {other}"
        ))),
    }
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

impl ThreadTransport for UreqTransport {
    fn fetch(
        &self,
        url: &str,
        token: &str,
        if_none_match: Option<&str>,
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
        if let Some(etag) = if_none_match {
            request = request.header("If-None-Match", etag);
        }

        let response = request
            .call()
            .map_err(|error| format!("Could not reach GitHub: {error}"))?;
        let status = response.status().as_u16();
        let cursor = response
            .headers()
            .get("etag")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let body = response
            .into_body()
            .read_to_vec()
            .map_err(|error| format!("Could not read GitHub's reply: {error}"))?;

        Ok(DoorbellResponse {
            status,
            cursor,
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
        // `gh`'s first line is what tells a dropped network from a lost login, and the
        // watcher's message is the only place the user will see it. Alone, because the
        // status line has room for little more.
        let stderr = String::from_utf8_lossy(&output.stderr);
        let why = stderr.lines().next().unwrap_or_default().trim();
        return Err(if why.is_empty() {
            format!("`gh` could not say which account is logged in to {host}")
        } else {
            format!("gh: {why}")
        });
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

    fn thread(kind: ThreadKind) -> ThreadRef {
        ThreadRef {
            host: "github.com".to_string(),
            owner: "microsoft".to_string(),
            repo: "maps".to_string(),
            number: 2366,
            kind,
        }
    }

    fn response(status: u16) -> DoorbellResponse {
        DoorbellResponse {
            status,
            cursor: Some("W/\"abc\"".to_string()),
            body: Vec::new(),
        }
    }

    #[test]
    fn an_unchanged_thread_costs_nothing() {
        // The economic argument for polling every thread separately: a 304 is free, so
        // a handful of watched threads is a handful of free requests a minute.
        assert_eq!(interpret(&response(304)).unwrap(), Ring::Quiet);
    }

    #[test]
    fn a_thread_that_moved_hands_back_the_cursor_for_next_time() {
        assert_eq!(
            interpret(&response(200)).unwrap(),
            Ring::Moved {
                cursor: Some("W/\"abc\"".to_string()),
            }
        );
    }

    #[test]
    fn a_pull_request_is_polled_on_the_issues_endpoint_that_tracks_its_conversation() {
        // `pulls/{n}` changes when the branch changes; `issues/{n}` changes when
        // somebody comments, which is what anyone here is waiting for.
        assert_eq!(
            thread_api_url(&thread(ThreadKind::PullRequest)).unwrap(),
            "https://api.github.com/repos/microsoft/maps/issues/2366"
        );
        assert_eq!(
            thread_api_url(&thread(ThreadKind::Issue)).unwrap(),
            "https://api.github.com/repos/microsoft/maps/issues/2366"
        );
    }

    #[test]
    fn a_discussion_has_no_rest_url_and_says_so_rather_than_inventing_one() {
        assert_eq!(thread_api_url(&thread(ThreadKind::Discussion)), None);
    }

    #[test]
    fn an_enterprise_host_is_asked_on_its_own_domain_not_api_github_com() {
        let mut enterprise = thread(ThreadKind::Issue);
        enterprise.host = "github.mycorp.example".to_string();

        assert_eq!(
            thread_api_url(&enterprise).unwrap(),
            "https://github.mycorp.example/api/v3/repos/microsoft/maps/issues/2366"
        );
    }

    #[test]
    fn a_thread_that_can_no_longer_be_read_says_so_instead_of_retrying_silently() {
        let error = interpret(&response(404)).unwrap_err();
        assert!(matches!(error, PollError::Gone(_)), "got: {error:?}");
        let message = error.describe(&thread(ThreadKind::Issue));
        assert!(
            message.contains("deleted or made private"),
            "got: {message}"
        );
        assert!(message.contains("/issues/2366"), "got: {message}");

        let error = interpret(&response(403)).unwrap_err();
        assert!(matches!(error, PollError::Refused(_)), "got: {error:?}");
        let message = error.describe(&thread(ThreadKind::Issue));
        assert!(message.contains("gh auth login"), "got: {message}");
    }

    #[test]
    fn github_having_a_bad_moment_is_retried_like_a_dropped_connection() {
        // Taken as a refusal, an outage on GitHub's side would read as "log in again"
        // and wait the long way round for a login that was never the problem.
        for status in [500, 502, 503, 429] {
            let error = interpret(&response(status)).unwrap_err();
            assert!(
                matches!(error, PollError::Unreachable(_)),
                "HTTP {status} got: {error:?}"
            );
        }
    }

    #[test]
    fn a_dropped_connection_says_it_is_still_watching_before_saying_why() {
        // The status line cuts off what does not fit, and a network error can be long.
        // That watching goes on is the part the user needs; the detail can be lost.
        let message = PollError::Unreachable("Could not reach GitHub: io: timed out".into())
            .describe(&thread(ThreadKind::Issue));
        assert!(
            message.starts_with("Thread watching will keep retrying"),
            "got: {message}"
        );
        assert!(!message.contains("paused"), "got: {message}");
    }

    #[test]
    fn gh_explaining_itself_over_several_lines_still_fits_one_status_line() {
        let message = PollError::Unreachable(
            "`gh` could not load the GitHub item: error connecting to api.github.com\n\
             check your internet connection or https://githubstatus.com"
                .into(),
        )
        .describe(&thread(ThreadKind::Discussion));

        assert!(!message.contains('\n'), "got: {message:?}");
        assert!(message.ends_with("api.github.com"), "got: {message:?}");
    }

    #[test]
    fn a_failure_through_gh_is_sorted_the_same_way_as_one_over_http() {
        use crate::github::{GithubError, GithubErrorKind};
        let error = |kind| GithubError {
            kind,
            message: "x".to_string(),
        };
        assert!(matches!(
            PollError::from_github(error(GithubErrorKind::Authentication)),
            PollError::Refused(_)
        ));
        assert!(matches!(
            PollError::from_github(error(GithubErrorKind::NotFound)),
            PollError::Gone(_)
        ));
        // `gh` reports a network that is down as a plain failure.
        assert!(matches!(
            PollError::from_github(error(GithubErrorKind::Cli)),
            PollError::Unreachable(_)
        ));
    }

    /// Proves against the real API that a conditional poll is free, and — the thing that
    /// forced this design — that a comment from our own account is visible here.
    ///
    /// Ignored by default because it spends requests against the running user's account.
    #[test]
    #[ignore = "talks to the real GitHub API; run with CST_DOORBELL_LIVE=1"]
    fn a_conditional_poll_really_is_free_against_the_live_api() {
        if std::env::var_os("CST_DOORBELL_LIVE").is_none() {
            return;
        }
        let transport = UreqTransport::new();
        let token = token_for("github.com").expect("gh must be logged in for this test");
        let thread = ThreadRef {
            host: "github.com".to_string(),
            owner: "rust-lang".to_string(),
            repo: "rust".to_string(),
            number: 100_000,
            kind: ThreadKind::Issue,
        };
        let url = thread_api_url(&thread).unwrap();

        let first = transport.fetch(&url, &token, None).unwrap();
        assert_eq!(first.status, 200);
        let cursor = first
            .cursor
            .expect("GitHub must send an ETag or there is no cursor to store");

        let second = transport.fetch(&url, &token, Some(&cursor)).unwrap();
        assert_eq!(
            second.status, 304,
            "the same request with the cursor must be Not Modified"
        );
    }
}
