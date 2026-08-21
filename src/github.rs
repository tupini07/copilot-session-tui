use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepositoryRef {
    pub host: String,
    pub owner: String,
    pub name: String,
}

impl RepositoryRef {
    pub fn name_with_owner(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }

    fn endpoint(&self, suffix: &str) -> String {
        format!("repos/{}/{}/{}", self.owner, self.name, suffix)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Author {
    pub login: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    pub name: String,
    pub color: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemCommon {
    pub repository: RepositoryRef,
    pub number: u64,
    pub title: String,
    pub state: String,
    pub author: Author,
    pub labels: Vec<Label>,
    pub created_at: String,
    pub updated_at: String,
    pub url: String,
    pub body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscussionKind {
    Comment,
    Review,
    InlineReview,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscussionEntry {
    pub kind: DiscussionKind,
    pub author: Author,
    pub body: String,
    pub created_at: String,
    pub review_state: Option<String>,
    pub path: Option<String>,
    pub line: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: String,
    pub status: String,
    pub additions: u64,
    pub deletions: u64,
    pub changes: u64,
    pub patch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub common: ItemCommon,
    pub comments: Vec<DiscussionEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub common: ItemCommon,
    pub draft: bool,
    pub merged: bool,
    pub mergeable_state: Option<String>,
    pub base_ref: String,
    pub head_ref: String,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    pub discussion: Vec<DiscussionEntry>,
    pub files: Vec<ChangedFile>,
    /// False while the changed-file list is known but the patches are not.
    ///
    /// The fast path lists files without their diffs, so `patch: None` alone
    /// cannot distinguish "not fetched yet" from "GitHub omitted it".
    pub patches_loaded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GithubItem {
    Issue(Issue),
    PullRequest(PullRequest),
}

impl GithubItem {
    pub fn common(&self) -> &ItemCommon {
        match self {
            Self::Issue(issue) => &issue.common,
            Self::PullRequest(pull) => &pull.common,
        }
    }

    pub fn discussion(&self) -> &[DiscussionEntry] {
        match self {
            Self::Issue(issue) => &issue.comments,
            Self::PullRequest(pull) => &pull.discussion,
        }
    }

    pub fn files(&self) -> &[ChangedFile] {
        match self {
            Self::Issue(_) => &[],
            Self::PullRequest(pull) => &pull.files,
        }
    }

    pub fn is_pull_request(&self) -> bool {
        matches!(self, Self::PullRequest(_))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GithubErrorKind {
    MissingCli,
    Repository,
    Authentication,
    NotFound,
    InvalidResponse,
    Cancelled,
    Cli,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubError {
    pub kind: GithubErrorKind,
    pub message: String,
}

impl GithubError {
    fn new(kind: GithubErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    fn invalid(context: &str, error: impl fmt::Display) -> Self {
        Self::new(
            GithubErrorKind::InvalidResponse,
            format!("GitHub returned invalid {context} data: {error}"),
        )
    }

    /// True when retrying the request a different way cannot help.
    ///
    /// Missing credentials or a missing item fail identically on every
    /// endpoint, so falling back would only double the time spent failing.
    fn is_fatal(&self) -> bool {
        matches!(
            self.kind,
            GithubErrorKind::MissingCli
                | GithubErrorKind::Authentication
                | GithubErrorKind::NotFound
                | GithubErrorKind::Repository
        )
    }
}

impl fmt::Display for GithubError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for GithubError {}

trait GhRunner {
    fn run(&self, cwd: &Path, args: &[String]) -> Result<Vec<u8>, GithubError>;

    /// Run, keeping stdout even when `gh` reports failure.
    ///
    /// GraphQL answers a partially invalid request with usable data *and* an
    /// error, which `gh` surfaces as a non-zero exit.
    fn run_partial(&self, cwd: &Path, args: &[String]) -> Result<Vec<u8>, GithubError> {
        self.run(cwd, args)
    }
}

struct ProcessGhRunner {
    cancelled: Arc<AtomicBool>,
}

impl ProcessGhRunner {
    fn execute(
        &self,
        cwd: &Path,
        args: &[String],
    ) -> Result<(ExitStatus, Vec<u8>, Vec<u8>), GithubError> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(cancelled_error());
        }

        let mut child = Command::new("gh")
            .args(args)
            .current_dir(cwd)
            .env("GH_PROMPT_DISABLED", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    GithubError::new(
                        GithubErrorKind::MissingCli,
                        "GitHub inspection requires the `gh` CLI in PATH",
                    )
                } else {
                    GithubError::new(GithubErrorKind::Cli, format!("Failed to run `gh`: {error}"))
                }
            })?;

        let stdout = child.stdout.take().ok_or_else(|| {
            GithubError::new(GithubErrorKind::Cli, "Failed to capture `gh` output")
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            GithubError::new(GithubErrorKind::Cli, "Failed to capture `gh` errors")
        })?;
        let stdout_reader = read_pipe(stdout);
        let stderr_reader = read_pipe(stderr);

        let status = loop {
            if self.cancelled.load(Ordering::Acquire) {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(cancelled_error());
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => std::thread::sleep(Duration::from_millis(40)),
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = stdout_reader.join();
                    let _ = stderr_reader.join();
                    return Err(GithubError::new(
                        GithubErrorKind::Cli,
                        format!("Failed while waiting for `gh`: {error}"),
                    ));
                }
            }
        };
        let stdout = stdout_reader.join().map_err(|_| {
            GithubError::new(
                GithubErrorKind::Cli,
                "Failed to collect `gh` output: reader stopped",
            )
        })?;
        let stdout = stdout.map_err(|error| {
            GithubError::new(
                GithubErrorKind::Cli,
                format!("Failed to read `gh` output: {error}"),
            )
        })?;
        let stderr = stderr_reader.join().map_err(|_| {
            GithubError::new(
                GithubErrorKind::Cli,
                "Failed to collect `gh` errors: reader stopped",
            )
        })?;
        let stderr = stderr.map_err(|error| {
            GithubError::new(
                GithubErrorKind::Cli,
                format!("Failed to read `gh` errors: {error}"),
            )
        })?;

        Ok((status, stdout, stderr))
    }
}

impl GhRunner for ProcessGhRunner {
    fn run(&self, cwd: &Path, args: &[String]) -> Result<Vec<u8>, GithubError> {
        let (status, stdout, stderr) = self.execute(cwd, args)?;
        if status.success() {
            return Ok(stdout);
        }
        cli_error(status, &stderr)
    }

    fn run_partial(&self, cwd: &Path, args: &[String]) -> Result<Vec<u8>, GithubError> {
        let (status, stdout, stderr) = self.execute(cwd, args)?;
        if status.success() || !stdout.is_empty() {
            return Ok(stdout);
        }
        cli_error(status, &stderr)
    }
}

fn read_pipe(
    mut pipe: impl Read + Send + 'static,
) -> std::thread::JoinHandle<std::io::Result<Vec<u8>>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes)?;
        Ok(bytes)
    })
}

fn cli_error(status: ExitStatus, stderr: &[u8]) -> Result<Vec<u8>, GithubError> {
    let stderr = String::from_utf8_lossy(stderr).trim().to_string();
    let lower = stderr.to_ascii_lowercase();
    let (kind, guidance) = if lower.contains("http 401")
        || lower.contains("http 403")
        || lower.contains("authentication")
        || lower.contains("not logged")
        || lower.contains("gh auth login")
    {
        (
            GithubErrorKind::Authentication,
            "GitHub authentication failed; run `gh auth login` for this repository host",
        )
    } else if lower.contains("http 404")
        || lower.contains("not found")
        || lower.contains("could not resolve")
    {
        (
            GithubErrorKind::NotFound,
            "The GitHub repository or item was not found",
        )
    } else {
        (GithubErrorKind::Cli, "`gh` could not load the GitHub item")
    };
    let message = if stderr.is_empty() {
        format!("{guidance} ({status})")
    } else {
        format!("{guidance}: {stderr}")
    };
    Err(GithubError::new(kind, message))
}

fn cancelled_error() -> GithubError {
    GithubError::new(GithubErrorKind::Cancelled, "GitHub request cancelled")
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepoView {
    name_with_owner: String,
    url: String,
}

#[derive(Debug, Deserialize)]
struct ApiUser {
    login: String,
}

#[derive(Debug, Deserialize)]
struct ApiLabel {
    name: String,
    color: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiIssue {
    number: u64,
    title: String,
    state: String,
    user: Option<ApiUser>,
    #[serde(default)]
    labels: Vec<ApiLabel>,
    created_at: String,
    updated_at: String,
    html_url: String,
    body: Option<String>,
    pull_request: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct ApiRef {
    #[serde(rename = "ref")]
    name: String,
}

#[derive(Debug, Deserialize)]
struct ApiPullRequest {
    number: u64,
    title: String,
    state: String,
    user: Option<ApiUser>,
    #[serde(default)]
    labels: Vec<ApiLabel>,
    created_at: String,
    updated_at: String,
    html_url: String,
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    merged_at: Option<String>,
    mergeable_state: Option<String>,
    base: ApiRef,
    head: ApiRef,
    additions: u64,
    deletions: u64,
    changed_files: u64,
}

#[derive(Debug, Deserialize)]
struct ApiComment {
    user: Option<ApiUser>,
    body: Option<String>,
    created_at: String,
}

#[derive(Debug, Deserialize)]
struct ApiReview {
    user: Option<ApiUser>,
    body: Option<String>,
    state: Option<String>,
    submitted_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiInlineComment {
    user: Option<ApiUser>,
    body: Option<String>,
    created_at: String,
    path: Option<String>,
    line: Option<u64>,
    original_line: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ApiFile {
    filename: String,
    status: String,
    additions: u64,
    deletions: u64,
    changes: u64,
    patch: Option<String>,
}

/// An item plus the repository it came from, so callers can cache the lookup.
pub struct FetchedItem {
    pub repository: RepositoryRef,
    pub item: GithubItem,
}

pub fn fetch_item(
    cwd: PathBuf,
    number: u64,
    known_repository: Option<RepositoryRef>,
    cancelled: Arc<AtomicBool>,
) -> Result<FetchedItem, GithubError> {
    let runner = ProcessGhRunner { cancelled };
    // Resolving the repository is a network round trip of its own, and it can
    // never change for a given directory while CST is running.
    let repository = match known_repository {
        Some(repository) => repository,
        None => resolve_repository(&runner, &cwd)?,
    };
    let item = fetch_item_from(&runner, &cwd, &repository, number)?;
    Ok(FetchedItem { repository, item })
}

/// Resolve the repository for a working directory.
///
/// Exposed so callers can warm this up before it is needed: it is a network
/// round trip, and repository identity never changes while CST is running.
pub fn resolve_repository_for(
    cwd: &Path,
    cancelled: Arc<AtomicBool>,
) -> Result<RepositoryRef, GithubError> {
    resolve_repository(&ProcessGhRunner { cancelled }, cwd)
}

/// Fetch a pull request's diffs, which the fast path deliberately skips.
///
/// Returns patches keyed by path, for merging into an already-listed set of
/// changed files.
pub fn fetch_patches(
    cwd: PathBuf,
    repository: RepositoryRef,
    number: u64,
    cancelled: Arc<AtomicBool>,
) -> Result<Vec<(String, Option<String>)>, GithubError> {
    let runner = ProcessGhRunner { cancelled };
    let endpoint = repository.endpoint(&format!("pulls/{number}/files"));
    let files: Vec<ApiFile> = api_pages(&runner, &cwd, &repository, &endpoint, "changed files")?;
    Ok(files
        .into_iter()
        .map(|file| (file.filename, file.patch))
        .collect())
}

fn fetch_item_from(
    runner: &dyn GhRunner,
    cwd: &Path,
    repository: &RepositoryRef,
    number: u64,
) -> Result<GithubItem, GithubError> {
    // One GraphQL round trip replaces up to six REST ones. It only covers items
    // whose comment, review and file lists fit in a single page, so anything
    // larger falls back to the paginated REST loader rather than truncating.
    match fetch_item_graphql(runner, cwd, repository, number) {
        Ok(Some(item)) => return Ok(item),
        Ok(None) => {}
        Err(error) if error.kind == GithubErrorKind::Cancelled => return Err(error),
        Err(error) if error.is_fatal() => return Err(error),
        // A GraphQL-specific failure (an unsupported schema on an old
        // Enterprise server, say) must not make the inspector unusable.
        Err(_) => {}
    }
    fetch_item_rest(runner, cwd, repository, number)
}

fn fetch_item_rest(
    runner: &dyn GhRunner,
    cwd: &Path,
    repository: &RepositoryRef,
    number: u64,
) -> Result<GithubItem, GithubError> {
    let issue_endpoint = repository.endpoint(&format!("issues/{number}"));
    let issue: ApiIssue = api_object(runner, cwd, repository, &issue_endpoint, "item")?;

    if issue.pull_request.is_some() {
        load_pull_request(runner, cwd, repository.clone(), number)
    } else {
        load_issue(runner, cwd, repository.clone(), issue)
    }
}

fn resolve_repository(runner: &dyn GhRunner, cwd: &Path) -> Result<RepositoryRef, GithubError> {
    let args = strings(&["repo", "view", "--json", "nameWithOwner,url"]);
    let output = runner.run(cwd, &args).map_err(|mut error| {
        if error.kind == GithubErrorKind::Cli || error.kind == GithubErrorKind::NotFound {
            error.kind = GithubErrorKind::Repository;
            error.message = format!(
                "Cannot resolve a GitHub repository from {}: {}",
                cwd.display(),
                error.message
            );
        }
        error
    })?;
    let view: RepoView = serde_json::from_slice(&output)
        .map_err(|error| GithubError::invalid("repository", error))?;
    repository_from_view(view)
}

fn repository_from_view(view: RepoView) -> Result<RepositoryRef, GithubError> {
    let (owner, name) = view.name_with_owner.split_once('/').ok_or_else(|| {
        GithubError::new(
            GithubErrorKind::InvalidResponse,
            "GitHub returned a repository name without an owner",
        )
    })?;
    let without_scheme = view
        .url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(view.url.as_str());
    let host = without_scheme
        .split('/')
        .next()
        .filter(|host| !host.is_empty())
        .ok_or_else(|| {
            GithubError::new(
                GithubErrorKind::InvalidResponse,
                "GitHub returned a repository URL without a hostname",
            )
        })?;
    Ok(RepositoryRef {
        host: host.to_string(),
        owner: owner.to_string(),
        name: name.to_string(),
    })
}

fn load_issue(
    runner: &dyn GhRunner,
    cwd: &Path,
    repository: RepositoryRef,
    issue: ApiIssue,
) -> Result<GithubItem, GithubError> {
    let comments_endpoint = repository.endpoint(&format!("issues/{}/comments", issue.number));
    let comments: Vec<ApiComment> = api_pages(
        runner,
        cwd,
        &repository,
        &comments_endpoint,
        "issue comments",
    )?;
    let comments = comments
        .into_iter()
        .map(|comment| DiscussionEntry {
            kind: DiscussionKind::Comment,
            author: author(comment.user),
            body: comment.body.unwrap_or_default(),
            created_at: comment.created_at,
            review_state: None,
            path: None,
            line: None,
        })
        .collect();

    Ok(GithubItem::Issue(Issue {
        common: common_from_issue(repository, issue),
        comments,
    }))
}

fn load_pull_request(
    runner: &dyn GhRunner,
    cwd: &Path,
    repository: RepositoryRef,
    number: u64,
) -> Result<GithubItem, GithubError> {
    let pull_endpoint = repository.endpoint(&format!("pulls/{number}"));
    let pull: ApiPullRequest =
        api_object(runner, cwd, &repository, &pull_endpoint, "pull request")?;
    let issue_comments_endpoint = repository.endpoint(&format!("issues/{number}/comments"));
    let reviews_endpoint = repository.endpoint(&format!("pulls/{number}/reviews"));
    let inline_endpoint = repository.endpoint(&format!("pulls/{number}/comments"));
    let files_endpoint = repository.endpoint(&format!("pulls/{number}/files"));

    let comments: Vec<ApiComment> = api_pages(
        runner,
        cwd,
        &repository,
        &issue_comments_endpoint,
        "pull request comments",
    )?;
    let reviews: Vec<ApiReview> = api_pages(
        runner,
        cwd,
        &repository,
        &reviews_endpoint,
        "pull request reviews",
    )?;
    let inline_comments: Vec<ApiInlineComment> = api_pages(
        runner,
        cwd,
        &repository,
        &inline_endpoint,
        "inline review comments",
    )?;
    let files: Vec<ApiFile> =
        api_pages(runner, cwd, &repository, &files_endpoint, "changed files")?;

    let mut discussion = Vec::with_capacity(comments.len() + reviews.len() + inline_comments.len());
    discussion.extend(comments.into_iter().map(|comment| DiscussionEntry {
        kind: DiscussionKind::Comment,
        author: author(comment.user),
        body: comment.body.unwrap_or_default(),
        created_at: comment.created_at,
        review_state: None,
        path: None,
        line: None,
    }));
    discussion.extend(reviews.into_iter().map(|review| DiscussionEntry {
        kind: DiscussionKind::Review,
        author: author(review.user),
        body: review.body.unwrap_or_default(),
        created_at: review.submitted_at.unwrap_or_default(),
        review_state: review.state,
        path: None,
        line: None,
    }));
    discussion.extend(inline_comments.into_iter().map(|comment| DiscussionEntry {
        kind: DiscussionKind::InlineReview,
        author: author(comment.user),
        body: comment.body.unwrap_or_default(),
        created_at: comment.created_at,
        review_state: None,
        path: comment.path,
        line: comment.line.or(comment.original_line),
    }));
    discussion.sort_by(|left, right| left.created_at.cmp(&right.created_at));

    let common = ItemCommon {
        repository: repository.clone(),
        number: pull.number,
        title: pull.title,
        state: pull.state,
        author: author(pull.user),
        labels: labels(pull.labels),
        created_at: pull.created_at,
        updated_at: pull.updated_at,
        url: pull.html_url,
        body: pull.body.unwrap_or_default(),
    };
    let files = files
        .into_iter()
        .map(|file| ChangedFile {
            path: file.filename,
            status: file.status,
            additions: file.additions,
            deletions: file.deletions,
            changes: file.changes,
            patch: file.patch,
        })
        .collect();

    Ok(GithubItem::PullRequest(PullRequest {
        common,
        draft: pull.draft,
        merged: pull.merged_at.is_some(),
        mergeable_state: pull.mergeable_state,
        base_ref: pull.base.name,
        head_ref: pull.head.name,
        additions: pull.additions,
        deletions: pull.deletions,
        changed_files: pull.changed_files,
        discussion,
        files,
        patches_loaded: true,
    }))
}

fn common_from_issue(repository: RepositoryRef, issue: ApiIssue) -> ItemCommon {
    ItemCommon {
        repository,
        number: issue.number,
        title: issue.title,
        state: issue.state,
        author: author(issue.user),
        labels: labels(issue.labels),
        created_at: issue.created_at,
        updated_at: issue.updated_at,
        url: issue.html_url,
        body: issue.body.unwrap_or_default(),
    }
}

fn author(user: Option<ApiUser>) -> Author {
    Author {
        login: user
            .map(|user| user.login)
            .unwrap_or_else(|| "ghost".to_string()),
    }
}

fn labels(input: Vec<ApiLabel>) -> Vec<Label> {
    input
        .into_iter()
        .map(|label| Label {
            name: label.name,
            color: label.color,
        })
        .collect()
}

fn api_object<T: DeserializeOwned>(
    runner: &dyn GhRunner,
    cwd: &Path,
    repository: &RepositoryRef,
    endpoint: &str,
    context: &str,
) -> Result<T, GithubError> {
    let output = runner.run(cwd, &api_args(repository, endpoint, false))?;
    serde_json::from_slice(&output).map_err(|error| GithubError::invalid(context, error))
}

fn api_pages<T: DeserializeOwned>(
    runner: &dyn GhRunner,
    cwd: &Path,
    repository: &RepositoryRef,
    endpoint: &str,
    context: &str,
) -> Result<Vec<T>, GithubError> {
    let output = runner.run(cwd, &api_args(repository, endpoint, true))?;
    let pages: Vec<Vec<T>> =
        serde_json::from_slice(&output).map_err(|error| GithubError::invalid(context, error))?;
    Ok(pages.into_iter().flatten().collect())
}

/// What a `#1234` in the conversation actually points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceKind {
    Issue,
    PullRequest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceState {
    Open,
    Closed,
    Merged,
    Draft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReferenceStatus {
    pub kind: ReferenceKind,
    pub state: ReferenceState,
}

/// Most references resolved in one query.
///
/// Bounded so a screen full of numbers cannot build a request large enough for
/// GitHub to reject on node count.
pub const REFERENCE_BATCH: usize = 40;

/// Look up several issue/pull-request numbers in a single round trip.
///
/// A `None` status means the number is not a reference at all — plenty of
/// `#123` in a terminal are version numbers or line numbers — and callers are
/// expected to remember that so they stop asking.
pub fn resolve_references(
    cwd: PathBuf,
    repository: RepositoryRef,
    numbers: Vec<u64>,
    cancelled: Arc<AtomicBool>,
) -> Result<Vec<(u64, Option<ReferenceStatus>)>, GithubError> {
    resolve_references_with(&ProcessGhRunner { cancelled }, &cwd, &repository, &numbers)
}

fn resolve_references_with(
    runner: &dyn GhRunner,
    cwd: &Path,
    repository: &RepositoryRef,
    numbers: &[u64],
) -> Result<Vec<(u64, Option<ReferenceStatus>)>, GithubError> {
    if numbers.is_empty() {
        return Ok(Vec::new());
    }
    let numbers: Vec<u64> = numbers.iter().copied().take(REFERENCE_BATCH).collect();
    let args = graphql_args(
        &repository.host,
        &reference_query(&numbers),
        &[
            ("owner", repository.owner.clone()),
            ("name", repository.name.clone()),
        ],
    );
    // Unknown numbers make GitHub report an error alongside perfectly good data
    // for every other alias, and `gh` exits non-zero for it, so the body has to
    // be read regardless of the exit status.
    let output = runner.run_partial(cwd, &args)?;
    let response: GraphResponse<GraphReferenceData> = serde_json::from_slice(&output)
        .map_err(|error| GithubError::invalid("references", error))?;
    let mut nodes = response
        .data
        .and_then(|data| data.repository)
        .unwrap_or_default();

    Ok(numbers
        .into_iter()
        .map(|number| {
            let status = nodes
                .remove(&reference_alias(number))
                .flatten()
                .map(|node| node.into_status());
            (number, status)
        })
        .collect())
}

fn reference_alias(number: u64) -> String {
    format!("r{number}")
}

fn reference_query(numbers: &[u64]) -> String {
    let mut query =
        String::from("query($owner:String!,$name:String!){repository(owner:$owner,name:$name){");
    for number in numbers {
        query.push_str(&format!(
            "{}:issueOrPullRequest(number:{number}){{__typename ... on Issue{{state}} ... on PullRequest{{state isDraft}}}} ",
            reference_alias(*number)
        ));
    }
    query.push_str("}}");
    query
}

#[derive(Debug, Deserialize)]
struct GraphReferenceData {
    repository: Option<std::collections::HashMap<String, Option<GraphReferenceNode>>>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "__typename")]
enum GraphReferenceNode {
    Issue {
        state: String,
    },
    PullRequest {
        state: String,
        #[serde(rename = "isDraft", default)]
        is_draft: bool,
    },
}

impl GraphReferenceNode {
    fn into_status(self) -> ReferenceStatus {
        match self {
            Self::Issue { state } => ReferenceStatus {
                kind: ReferenceKind::Issue,
                state: if state.eq_ignore_ascii_case("closed") {
                    ReferenceState::Closed
                } else {
                    ReferenceState::Open
                },
            },
            Self::PullRequest { state, is_draft } => ReferenceStatus {
                kind: ReferenceKind::PullRequest,
                state: if state.eq_ignore_ascii_case("merged") {
                    ReferenceState::Merged
                } else if state.eq_ignore_ascii_case("closed") {
                    ReferenceState::Closed
                } else if is_draft {
                    ReferenceState::Draft
                } else {
                    ReferenceState::Open
                },
            },
        }
    }
}

fn api_args(repository: &RepositoryRef, endpoint: &str, paginate: bool) -> Vec<String> {
    let mut args = strings(&["api", "--hostname", &repository.host]);
    if paginate {
        args.extend(strings(&["--paginate", "--slurp"]));
    }
    args.push(endpoint.to_string());
    args
}

/// How many entries of each connection the single-call query asks for.
///
/// An item that exceeds this in any list falls back to the REST loader, so the
/// figure trades how often that happens against the size of one response.
const GRAPH_PAGE: usize = 100;

const ITEM_QUERY: &str = r#"
query($owner:String!,$name:String!,$number:Int!,$page:Int!){
  repository(owner:$owner,name:$name){
    issueOrPullRequest(number:$number){
      __typename
      ... on Issue {
        number title state body url createdAt updatedAt
        author{login}
        labels(first:$page){nodes{name color} pageInfo{hasNextPage}}
        comments(first:$page){nodes{body createdAt author{login}} pageInfo{hasNextPage}}
      }
      ... on PullRequest {
        number title state body url createdAt updatedAt
        isDraft merged mergeable additions deletions changedFiles
        baseRefName headRefName
        author{login}
        labels(first:$page){nodes{name color} pageInfo{hasNextPage}}
        comments(first:$page){nodes{body createdAt author{login}} pageInfo{hasNextPage}}
        reviews(first:$page){nodes{body state submittedAt author{login}} pageInfo{hasNextPage}}
        reviewThreads(first:$page){nodes{comments(first:20){nodes{body path line originalLine createdAt author{login}} pageInfo{hasNextPage}}} pageInfo{hasNextPage}}
        files(first:$page){nodes{path additions deletions changeType} pageInfo{hasNextPage}}
      }
    }
  }
}
"#;

fn graphql_args(host: &str, query: &str, variables: &[(&str, String)]) -> Vec<String> {
    let mut args = strings(&["api", "graphql", "--hostname", host]);
    for (name, value) in variables {
        // -F types numbers and booleans; -f would send them as strings and the
        // Int! variables would be rejected.
        args.push("-F".to_string());
        args.push(format!("{name}={value}"));
    }
    args.push("-f".to_string());
    args.push(format!("query={query}"));
    args
}

/// Fetch an item in a single round trip.
///
/// `Ok(None)` means the item is too large for one page and the caller should
/// use the paginated REST loader instead.
fn fetch_item_graphql(
    runner: &dyn GhRunner,
    cwd: &Path,
    repository: &RepositoryRef,
    number: u64,
) -> Result<Option<GithubItem>, GithubError> {
    let args = graphql_args(
        &repository.host,
        ITEM_QUERY,
        &[
            ("owner", repository.owner.clone()),
            ("name", repository.name.clone()),
            ("number", number.to_string()),
            ("page", GRAPH_PAGE.to_string()),
        ],
    );
    let output = runner.run(cwd, &args)?;
    let response: GraphResponse<GraphItemData> =
        serde_json::from_slice(&output).map_err(|error| GithubError::invalid("item", error))?;
    let item = response
        .data
        .and_then(|data| data.repository)
        .and_then(|repository| repository.item)
        .ok_or_else(|| {
            GithubError::new(
                GithubErrorKind::NotFound,
                format!(
                    "No issue or pull request #{number} in {}",
                    repository.name_with_owner()
                ),
            )
        })?;
    Ok(convert_graph_item(repository, item))
}

fn convert_graph_item(repository: &RepositoryRef, item: GraphItem) -> Option<GithubItem> {
    match item {
        GraphItem::Issue(issue) => {
            if issue.labels.truncated() || issue.comments.truncated() {
                return None;
            }
            let comments = issue
                .comments
                .nodes
                .into_iter()
                .map(|comment| DiscussionEntry {
                    kind: DiscussionKind::Comment,
                    author: graph_author(comment.author),
                    body: comment.body.unwrap_or_default(),
                    created_at: comment.created_at,
                    review_state: None,
                    path: None,
                    line: None,
                })
                .collect();
            Some(GithubItem::Issue(Issue {
                common: ItemCommon {
                    repository: repository.clone(),
                    number: issue.number,
                    title: issue.title,
                    state: issue.state.to_ascii_lowercase(),
                    author: graph_author(issue.author),
                    labels: graph_labels(issue.labels.nodes),
                    created_at: issue.created_at,
                    updated_at: issue.updated_at,
                    url: issue.url,
                    body: issue.body.unwrap_or_default(),
                },
                comments,
            }))
        }
        GraphItem::PullRequest(pull) => {
            if pull.labels.truncated()
                || pull.comments.truncated()
                || pull.reviews.truncated()
                || pull.review_threads.truncated()
                || pull.files.truncated()
                || pull
                    .review_threads
                    .nodes
                    .iter()
                    .any(|thread| thread.comments.truncated())
            {
                return None;
            }

            let mut discussion = Vec::new();
            discussion.extend(
                pull.comments
                    .nodes
                    .into_iter()
                    .map(|comment| DiscussionEntry {
                        kind: DiscussionKind::Comment,
                        author: graph_author(comment.author),
                        body: comment.body.unwrap_or_default(),
                        created_at: comment.created_at,
                        review_state: None,
                        path: None,
                        line: None,
                    }),
            );
            discussion.extend(
                pull.reviews
                    .nodes
                    .into_iter()
                    .map(|review| DiscussionEntry {
                        kind: DiscussionKind::Review,
                        author: graph_author(review.author),
                        body: review.body.unwrap_or_default(),
                        created_at: review.submitted_at.unwrap_or_default(),
                        review_state: review.state,
                        path: None,
                        line: None,
                    }),
            );
            for thread in pull.review_threads.nodes {
                discussion.extend(thread.comments.nodes.into_iter().map(|comment| {
                    DiscussionEntry {
                        kind: DiscussionKind::InlineReview,
                        author: graph_author(comment.author),
                        body: comment.body.unwrap_or_default(),
                        created_at: comment.created_at,
                        review_state: None,
                        path: comment.path,
                        line: comment.line.or(comment.original_line),
                    }
                }));
            }
            discussion.sort_by(|left, right| left.created_at.cmp(&right.created_at));

            let files = pull
                .files
                .nodes
                .into_iter()
                .map(|file| ChangedFile {
                    path: file.path,
                    status: graph_file_status(&file.change_type),
                    additions: file.additions,
                    deletions: file.deletions,
                    changes: file.additions + file.deletions,
                    patch: None,
                })
                .collect();

            // REST reports a merged pull request as closed and flags the merge
            // separately; matching that keeps the rest of the app unaware of
            // which loader produced the item.
            let merged = pull.merged;
            let state = if pull.state.eq_ignore_ascii_case("merged") {
                "closed".to_string()
            } else {
                pull.state.to_ascii_lowercase()
            };

            Some(GithubItem::PullRequest(PullRequest {
                common: ItemCommon {
                    repository: repository.clone(),
                    number: pull.number,
                    title: pull.title,
                    state,
                    author: graph_author(pull.author),
                    labels: graph_labels(pull.labels.nodes),
                    created_at: pull.created_at,
                    updated_at: pull.updated_at,
                    url: pull.url,
                    body: pull.body.unwrap_or_default(),
                },
                draft: pull.is_draft,
                merged,
                mergeable_state: pull.mergeable.map(|state| state.to_ascii_lowercase()),
                base_ref: pull.base_ref_name,
                head_ref: pull.head_ref_name,
                additions: pull.additions,
                deletions: pull.deletions,
                changed_files: pull.changed_files,
                discussion,
                files,
                patches_loaded: false,
            }))
        }
    }
}

fn graph_author(actor: Option<GraphActor>) -> Author {
    Author {
        login: actor
            .map(|actor| actor.login)
            .unwrap_or_else(|| "ghost".to_string()),
    }
}

fn graph_labels(nodes: Vec<GraphLabel>) -> Vec<Label> {
    nodes
        .into_iter()
        .map(|label| Label {
            name: label.name,
            color: label.color,
        })
        .collect()
}

/// Translate GraphQL's change vocabulary into the REST one the UI already uses.
fn graph_file_status(change_type: &str) -> String {
    match change_type.to_ascii_lowercase().as_str() {
        "added" => "added",
        "deleted" => "removed",
        "renamed" => "renamed",
        "copied" => "copied",
        _ => "modified",
    }
    .to_string()
}

#[derive(Debug, Deserialize)]
struct GraphResponse<T> {
    data: Option<T>,
}

#[derive(Debug, Deserialize)]
struct GraphItemData {
    repository: Option<GraphItemRepository>,
}

#[derive(Debug, Deserialize)]
struct GraphItemRepository {
    #[serde(rename = "issueOrPullRequest")]
    item: Option<GraphItem>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "__typename")]
enum GraphItem {
    Issue(GraphIssue),
    PullRequest(GraphPull),
}

#[derive(Debug, Deserialize)]
struct GraphConnection<T> {
    #[serde(default = "Vec::new")]
    nodes: Vec<T>,
    #[serde(rename = "pageInfo")]
    page_info: GraphPageInfo,
}

impl<T> GraphConnection<T> {
    fn truncated(&self) -> bool {
        self.page_info.has_next_page
    }
}

#[derive(Debug, Deserialize)]
struct GraphPageInfo {
    #[serde(rename = "hasNextPage", default)]
    has_next_page: bool,
}

#[derive(Debug, Deserialize)]
struct GraphActor {
    login: String,
}

#[derive(Debug, Deserialize)]
struct GraphLabel {
    name: String,
    color: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphIssue {
    number: u64,
    title: String,
    state: String,
    body: Option<String>,
    url: String,
    created_at: String,
    updated_at: String,
    author: Option<GraphActor>,
    labels: GraphConnection<GraphLabel>,
    comments: GraphConnection<GraphComment>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphPull {
    number: u64,
    title: String,
    state: String,
    body: Option<String>,
    url: String,
    created_at: String,
    updated_at: String,
    is_draft: bool,
    merged: bool,
    mergeable: Option<String>,
    additions: u64,
    deletions: u64,
    changed_files: u64,
    base_ref_name: String,
    head_ref_name: String,
    author: Option<GraphActor>,
    labels: GraphConnection<GraphLabel>,
    comments: GraphConnection<GraphComment>,
    reviews: GraphConnection<GraphReview>,
    review_threads: GraphConnection<GraphThread>,
    files: GraphConnection<GraphFile>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphComment {
    body: Option<String>,
    created_at: String,
    author: Option<GraphActor>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphReview {
    body: Option<String>,
    state: Option<String>,
    submitted_at: Option<String>,
    author: Option<GraphActor>,
}

#[derive(Debug, Deserialize)]
struct GraphThread {
    comments: GraphConnection<GraphThreadComment>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphThreadComment {
    body: Option<String>,
    path: Option<String>,
    line: Option<u64>,
    original_line: Option<u64>,
    created_at: String,
    author: Option<GraphActor>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphFile {
    path: String,
    additions: u64,
    deletions: u64,
    change_type: String,
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A runner that replays canned `gh` output and records what was asked for.
    struct FakeRunner {
        responses: RefCell<Vec<Result<Vec<u8>, GithubError>>>,
        calls: RefCell<Vec<Vec<String>>>,
    }

    impl FakeRunner {
        fn new(responses: Vec<Result<Vec<u8>, GithubError>>) -> Self {
            Self {
                responses: RefCell::new(responses),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn ok<S: AsRef<str>>(bodies: &[S]) -> Self {
            Self::new(
                bodies
                    .iter()
                    .map(|body| Ok(body.as_ref().as_bytes().to_vec()))
                    .collect(),
            )
        }
    }

    impl GhRunner for FakeRunner {
        fn run(&self, _cwd: &Path, args: &[String]) -> Result<Vec<u8>, GithubError> {
            self.calls.borrow_mut().push(args.to_vec());
            if self.responses.borrow().is_empty() {
                return Err(GithubError::new(
                    GithubErrorKind::Cli,
                    format!("unexpected call: {args:?}"),
                ));
            }
            self.responses.borrow_mut().remove(0)
        }
    }

    #[test]
    fn resolves_a_mixed_batch_of_references() {
        let runner = FakeRunner::ok(&[r#"{"data":{"repository":{
            "r1":{"__typename":"Issue","state":"OPEN"},
            "r2":{"__typename":"Issue","state":"CLOSED"},
            "r3":{"__typename":"PullRequest","state":"MERGED","isDraft":false},
            "r4":{"__typename":"PullRequest","state":"OPEN","isDraft":true},
            "r5":{"__typename":"PullRequest","state":"CLOSED","isDraft":false},
            "r6":null}},"errors":[{"message":"Could not resolve"}]}"#]);

        let resolved = resolve_references_with(
            &runner,
            Path::new("/repo"),
            &repository(),
            &[1, 2, 3, 4, 5, 6],
        )
        .expect("resolve");

        assert_eq!(
            resolved,
            vec![
                (
                    1,
                    Some(ReferenceStatus {
                        kind: ReferenceKind::Issue,
                        state: ReferenceState::Open
                    })
                ),
                (
                    2,
                    Some(ReferenceStatus {
                        kind: ReferenceKind::Issue,
                        state: ReferenceState::Closed
                    })
                ),
                (
                    3,
                    Some(ReferenceStatus {
                        kind: ReferenceKind::PullRequest,
                        state: ReferenceState::Merged
                    })
                ),
                (
                    4,
                    Some(ReferenceStatus {
                        kind: ReferenceKind::PullRequest,
                        state: ReferenceState::Draft
                    })
                ),
                (
                    5,
                    Some(ReferenceStatus {
                        kind: ReferenceKind::PullRequest,
                        state: ReferenceState::Closed
                    })
                ),
                // A number that is not a reference at all still gets an answer,
                // so the caller can stop asking about it.
                (6, None),
            ]
        );
    }

    #[test]
    fn reference_lookup_uses_one_call_and_respects_the_batch_limit() {
        let aliases: String = (1..=REFERENCE_BATCH + 10)
            .map(|number| format!("\"r{number}\":{{\"__typename\":\"Issue\",\"state\":\"OPEN\"}},"))
            .collect();
        let body = format!(
            "{{\"data\":{{\"repository\":{{{}\"unused\":null}}}}}}",
            aliases
        );
        let runner = FakeRunner::ok(&[body]);
        let numbers: Vec<u64> = (1..=(REFERENCE_BATCH + 10) as u64).collect();

        let resolved =
            resolve_references_with(&runner, Path::new("/repo"), &repository(), &numbers)
                .expect("resolve");

        assert_eq!(resolved.len(), REFERENCE_BATCH);
        let calls = runner.calls.borrow();
        assert_eq!(calls.len(), 1, "one round trip for the whole batch");
        let query = calls[0]
            .iter()
            .find(|argument| argument.starts_with("query="))
            .expect("query argument");
        assert!(query.contains(&format!("r{REFERENCE_BATCH}:issueOrPullRequest")));
        assert!(!query.contains(&format!("r{}:issueOrPullRequest", REFERENCE_BATCH + 1)));
    }

    #[test]
    fn resolving_no_references_makes_no_calls() {
        let runner = FakeRunner::ok::<String>(&[]);

        let resolved =
            resolve_references_with(&runner, Path::new("/repo"), &repository(), &[]).expect("ok");

        assert!(resolved.is_empty());
        assert!(runner.calls.borrow().is_empty());
    }

    fn repository() -> RepositoryRef {
        RepositoryRef {
            host: "github.com".to_string(),
            owner: "octo".to_string(),
            name: "widgets".to_string(),
        }
    }

    fn graph_pull(page_info: &str, files: &str) -> String {
        format!(
            r#"{{"data":{{"repository":{{"issueOrPullRequest":{{
                "__typename":"PullRequest",
                "number":7,"title":"A change","state":"MERGED","body":"details",
                "url":"https://github.com/octo/widgets/pull/7",
                "createdAt":"2026-01-01T00:00:00Z","updatedAt":"2026-01-02T00:00:00Z",
                "isDraft":false,"merged":true,"mergeable":"MERGEABLE",
                "additions":10,"deletions":3,"changedFiles":2,
                "baseRefName":"main","headRefName":"feature",
                "author":{{"login":"monalisa"}},
                "labels":{{"nodes":[{{"name":"bug","color":"ff0000"}}],"pageInfo":{{"hasNextPage":false}}}},
                "comments":{{"nodes":[{{"body":"looks good","createdAt":"2026-01-01T02:00:00Z","author":{{"login":"octo"}}}}],"pageInfo":{{"hasNextPage":false}}}},
                "reviews":{{"nodes":[{{"body":"approved","state":"APPROVED","submittedAt":"2026-01-01T03:00:00Z","author":{{"login":"hubot"}}}}],"pageInfo":{{"hasNextPage":false}}}},
                "reviewThreads":{{"nodes":[{{"comments":{{"nodes":[{{"body":"nit","path":"src/lib.rs","line":12,"originalLine":null,"createdAt":"2026-01-01T01:00:00Z","author":{{"login":"hubot"}}}}],"pageInfo":{{"hasNextPage":false}}}}}}],"pageInfo":{{"hasNextPage":{page_info}}}}},
                "files":{{"nodes":{files},"pageInfo":{{"hasNextPage":false}}}}
            }}}}}}}}"#
        )
    }

    const GRAPH_FILES: &str = r#"[
        {"path":"src/lib.rs","additions":8,"deletions":2,"changeType":"MODIFIED"},
        {"path":"old.rs","additions":0,"deletions":1,"changeType":"DELETED"}
    ]"#;

    #[test]
    fn a_pull_request_arrives_from_a_single_graphql_call() {
        let runner = FakeRunner::ok(&[graph_pull("false", GRAPH_FILES)]);

        let item = fetch_item_from(&runner, Path::new("."), &repository(), 7).unwrap();

        assert_eq!(runner.calls.borrow().len(), 1, "one round trip only");
        let GithubItem::PullRequest(pull) = item else {
            panic!("expected a pull request");
        };
        assert_eq!(pull.common.title, "A change");
        // REST reports a merged pull request as closed, and the rest of the app
        // relies on that, so the GraphQL MERGED state has to be translated.
        assert_eq!(pull.common.state, "closed");
        assert!(pull.merged);
        assert_eq!(pull.base_ref, "main");
        assert_eq!(pull.common.labels[0].name, "bug");
        assert!(!pull.patches_loaded, "diffs are deliberately deferred");
        assert!(pull.files.iter().all(|file| file.patch.is_none()));
    }

    #[test]
    fn graphql_file_changes_use_the_rest_vocabulary() {
        let runner = FakeRunner::ok(&[graph_pull("false", GRAPH_FILES)]);

        let item = fetch_item_from(&runner, Path::new("."), &repository(), 7).unwrap();

        let statuses: Vec<&str> = item
            .files()
            .iter()
            .map(|file| file.status.as_str())
            .collect();
        // The file-tree markers switch on these strings.
        assert_eq!(statuses, vec!["modified", "removed"]);
        assert_eq!(item.files()[0].changes, 10);
    }

    #[test]
    fn every_kind_of_pull_request_comment_lands_in_one_timeline() {
        let runner = FakeRunner::ok(&[graph_pull("false", GRAPH_FILES)]);

        let item = fetch_item_from(&runner, Path::new("."), &repository(), 7).unwrap();

        let kinds: Vec<DiscussionKind> = item.discussion().iter().map(|entry| entry.kind).collect();
        // Sorted by timestamp, so the inline comment at 01:00 comes first.
        assert_eq!(
            kinds,
            vec![
                DiscussionKind::InlineReview,
                DiscussionKind::Comment,
                DiscussionKind::Review
            ]
        );
        assert_eq!(item.discussion()[0].path.as_deref(), Some("src/lib.rs"));
    }

    #[test]
    fn an_item_larger_than_one_page_falls_back_to_the_rest_loader() {
        // `hasNextPage` on the review threads means the single call would have
        // silently dropped comments.
        let runner = FakeRunner::new(vec![
            Ok(graph_pull("true", GRAPH_FILES).into_bytes()),
            Err(GithubError::new(GithubErrorKind::Cli, "rest reached")),
        ]);

        let error = fetch_item_from(&runner, Path::new("."), &repository(), 7).unwrap_err();

        assert_eq!(error.message, "rest reached");
        let calls = runner.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1][1], "--hostname", "the retry is a REST call");
    }

    #[test]
    fn a_graphql_outage_still_serves_the_item_over_rest() {
        let issue = r#"{"number":7,"title":"Broken","state":"open","user":{"login":"octo"},
            "labels":[],"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z",
            "html_url":"https://github.com/octo/widgets/issues/7","body":"text"}"#;
        let runner = FakeRunner::new(vec![
            Err(GithubError::new(GithubErrorKind::Cli, "graphql disabled")),
            Ok(issue.as_bytes().to_vec()),
            Ok(b"[[]]".to_vec()),
        ]);

        let item = fetch_item_from(&runner, Path::new("."), &repository(), 7).unwrap();

        assert_eq!(item.common().title, "Broken");
    }

    #[test]
    fn a_missing_item_is_not_retried_over_rest() {
        let runner = FakeRunner::new(vec![Err(GithubError::new(
            GithubErrorKind::NotFound,
            "no such item",
        ))]);

        let error = fetch_item_from(&runner, Path::new("."), &repository(), 7).unwrap_err();

        // REST would fail identically, so retrying only doubles the wait.
        assert_eq!(error.kind, GithubErrorKind::NotFound);
        assert_eq!(runner.calls.borrow().len(), 1);
    }

    #[test]
    fn graphql_arguments_keep_numbers_typed() {
        let args = graphql_args(
            "github.com",
            "query{}",
            &[("owner", "octo".to_string()), ("number", "7".to_string())],
        );

        // `-F` types values; `-f` would send 7 as a string and the Int! variable
        // would be rejected.
        assert_eq!(
            args[0..4],
            strings(&["api", "graphql", "--hostname", "github.com"])[..]
        );
        assert!(args.contains(&"-F".to_string()));
        assert!(args.contains(&"number=7".to_string()));
        assert!(args.contains(&"query=query{}".to_string()));
    }

    #[test]
    fn a_deleted_account_reads_as_ghost_rather_than_blank() {
        assert_eq!(graph_author(None).login, "ghost");
    }

    #[test]
    fn repository_view_preserves_enterprise_hostname() {
        let repository = repository_from_view(RepoView {
            name_with_owner: "octo/widgets".to_string(),
            url: "https://github.example.com/octo/widgets".to_string(),
        })
        .unwrap();

        assert_eq!(repository.host, "github.example.com");
        assert_eq!(repository.name_with_owner(), "octo/widgets");
    }

    #[test]
    fn paginated_api_arguments_request_slurped_pages() {
        let repository = RepositoryRef {
            host: "github.com".to_string(),
            owner: "octo".to_string(),
            name: "widgets".to_string(),
        };

        assert_eq!(
            api_args(&repository, "repos/octo/widgets/issues/1/comments", true),
            strings(&[
                "api",
                "--hostname",
                "github.com",
                "--paginate",
                "--slurp",
                "repos/octo/widgets/issues/1/comments"
            ])
        );
    }

    #[test]
    fn paginated_arrays_flatten_in_page_order() {
        let pages: Vec<Vec<ApiComment>> = serde_json::from_str(
            r#"[
                [{"user":{"login":"one"},"body":"first","created_at":"2026-01-01T00:00:00Z"}],
                [{"user":{"login":"two"},"body":"second","created_at":"2026-01-02T00:00:00Z"}]
            ]"#,
        )
        .unwrap();
        let comments: Vec<ApiComment> = pages.into_iter().flatten().collect();

        assert_eq!(comments.len(), 2);
        assert_eq!(comments[1].body.as_deref(), Some("second"));
    }

    #[test]
    fn issue_pull_request_marker_classifies_items() {
        let issue: ApiIssue = serde_json::from_str(
            r#"{
                "number":7,
                "title":"A change",
                "state":"open",
                "user":{"login":"octo"},
                "labels":[],
                "created_at":"2026-01-01T00:00:00Z",
                "updated_at":"2026-01-02T00:00:00Z",
                "html_url":"https://github.com/octo/widgets/pull/7",
                "body":"body",
                "pull_request":{"url":"https://api.github.com/repos/octo/widgets/pulls/7"}
            }"#,
        )
        .unwrap();

        assert!(issue.pull_request.is_some());
    }
}
