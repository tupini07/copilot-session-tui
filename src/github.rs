use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
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
}

impl fmt::Display for GithubError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for GithubError {}

trait GhRunner {
    fn run(&self, cwd: &Path, args: &[String]) -> Result<Vec<u8>, GithubError>;
}

struct ProcessGhRunner {
    cancelled: Arc<AtomicBool>,
}

impl GhRunner for ProcessGhRunner {
    fn run(&self, cwd: &Path, args: &[String]) -> Result<Vec<u8>, GithubError> {
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

        if status.success() {
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
    } else if lower.contains("http 404") || lower.contains("not found") {
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

pub fn fetch_item(
    cwd: PathBuf,
    number: u64,
    cancelled: Arc<AtomicBool>,
) -> Result<GithubItem, GithubError> {
    fetch_item_with(&ProcessGhRunner { cancelled }, &cwd, number)
}

fn fetch_item_with(
    runner: &dyn GhRunner,
    cwd: &Path,
    number: u64,
) -> Result<GithubItem, GithubError> {
    let repository = resolve_repository(runner, cwd)?;
    let issue_endpoint = repository.endpoint(&format!("issues/{number}"));
    let issue: ApiIssue = api_object(runner, cwd, &repository, &issue_endpoint, "item")?;

    if issue.pull_request.is_some() {
        load_pull_request(runner, cwd, repository, number)
    } else {
        load_issue(runner, cwd, repository, issue)
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

fn api_args(repository: &RepositoryRef, endpoint: &str, paginate: bool) -> Vec<String> {
    let mut args = strings(&["api", "--hostname", &repository.host]);
    if paginate {
        args.extend(strings(&["--paginate", "--slurp"]));
    }
    args.push(endpoint.to_string());
    args
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
