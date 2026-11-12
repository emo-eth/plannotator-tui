//! Forge writes require an explicit confirmation and retain the commit the person read.
//! In a pull review: M confirms a merge, R opens risk ranking/rules/provider/model,
//! and H confirms ownership by an existing thread (linkage only; no agent message).
//! g opens the linked guide's separate verified guide-to-forge transfer flow.
//! Read models retain only fields consumed by the TUI; Serde ignores other response metadata.
mod manage;
mod operations;
mod pane;
mod risk;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::form_urlencoded;

use super::client::Client;

pub(crate) use pane::{PullAction, ThreadPullsPane};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Platform {
    Github,
    Gitlab,
}

impl Platform {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Github => "GitHub",
            Self::Gitlab => "GitLab",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PullState {
    Open,
    Draft,
    Merged,
    Closed,
    Unknown,
}

impl PullState {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::Draft => "Draft",
            Self::Merged => "Merged",
            Self::Closed => "Closed",
            Self::Unknown => "State not known yet",
        }
    }

    pub(crate) fn is_open(self) -> bool {
        matches!(self, Self::Open | Self::Draft)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ThreadChecks {
    Passing,
    Failing,
    Running,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReviewState {
    Approved,
    ChangesRequested,
    ReviewRequired,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ThreadPull {
    pub(crate) pull: String,
    pub(crate) url: String,
    pub(crate) removed_at: Option<String>,
    pub(crate) state: PullState,
    pub(crate) title: Option<String>,
    #[serde(rename = "ref")]
    pub(crate) reference: String,
    pub(crate) checks: Option<ThreadChecks>,
    pub(crate) review: Option<ReviewState>,
}

impl ThreadPull {
    pub(crate) fn extras(&self) -> impl Iterator<Item = &'static str> {
        let checks = if self.state.is_open() {
            match self.checks {
                Some(ThreadChecks::Failing) => Some("checks failing"),
                Some(ThreadChecks::Running) => Some("checks running"),
                _ => None,
            }
        } else {
            None
        };
        let review = if self.state == PullState::Open {
            match self.review {
                Some(ReviewState::Approved) => Some("approved"),
                Some(ReviewState::ChangesRequested) => Some("changes requested"),
                Some(ReviewState::ReviewRequired) => Some("review required"),
                None => None,
            }
        } else {
            None
        };
        checks.into_iter().chain(review)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum GroupKind {
    Stack,
    Related,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PullGroup {
    pub(crate) kind: GroupKind,
    pub(crate) pulls: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ThreadPulls {
    pub(crate) pulls: Vec<ThreadPull>,
    pub(crate) groups: Vec<PullGroup>,
    pub(crate) removed: Vec<ThreadPull>,
}

impl ThreadPulls {
    pub(crate) fn ordered(&self) -> impl DoubleEndedIterator<Item = &ThreadPull> + Clone {
        self.groups.iter().flat_map(|group| {
            group.pulls.iter().filter_map(|key| self.pulls.iter().find(|pull| &pull.pull == key))
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct PullChange {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) add: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) remove: Vec<String>,
}

impl PullChange {
    pub(crate) fn add_text(text: &str) -> Self {
        Self {
            add: text
                .split(|c: char| c.is_whitespace() || c == ',')
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
                .collect(),
            remove: Vec::new(),
        }
    }

    pub(crate) fn remove(reference: &str) -> Self {
        Self { add: Vec::new(), remove: vec![reference.to_owned()] }
    }

    pub(crate) fn remove_all() -> Self {
        Self::remove("all")
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Pull {
    pub(crate) key: String,
    pub(crate) platform: Platform,
    pub(crate) repo: String,
    pub(crate) number: u64,
    pub(crate) url: String,
    pub(crate) title: String,
    pub(crate) mine: bool,
    pub(crate) state: PullState,
    pub(crate) head_sha: Option<String>,
}

impl Pull {
    pub(crate) fn postable(&self) -> bool {
        self.state.is_open() && self.head_sha.as_ref().is_some_and(|sha| !sha.is_empty())
    }

    pub(crate) fn action_refusal(&self, action: ReviewAction) -> Option<&'static str> {
        if !self.state.is_open() {
            return Some("This pull request is no longer open.");
        }
        if self.platform == Platform::Github && self.mine && action != ReviewAction::Comment {
            return Some("You cannot approve or request changes on your own GitHub pull request.");
        }
        None
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PullLink {
    pub(crate) thread_id: String,
    pub(crate) guide: Option<PullGuide>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PullGuide {
    pub(crate) message_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PullList {
    pub(crate) pulls: Vec<Pull>,
    pub(crate) links: BTreeMap<String, PullLink>,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PullOptions {
    pub(crate) refresh: bool,
    pub(crate) others: bool,
    pub(crate) everywhere: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DiffScope {
    Layer,
    Stack,
}

impl DiffScope {
    fn wire(self) -> &'static str {
        match self {
            Self::Layer => "layer",
            Self::Stack => "stack",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PullDiff {
    pub(crate) key: String,
    pub(crate) scope: DiffScope,
    pub(crate) base: String,
    pub(crate) head: String,
    pub(crate) patch: String,
    pub(crate) head_sha: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PullDetails {
    pub(crate) description: Option<String>,
    pub(crate) labels: Vec<String>,
    pub(crate) reviewers: Vec<Reviewer>,
    pub(crate) checks: Vec<Check>,
    pub(crate) comments: Vec<DetailComment>,
    #[serde(default)]
    pub(crate) threads: Vec<Discussion>,
    #[serde(default)]
    pub(crate) threads_incomplete: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Reviewer {
    pub(crate) login: String,
    pub(crate) state: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Check {
    pub(crate) name: String,
    pub(crate) state: String,
    pub(crate) note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct DetailComment {
    pub(crate) author: String,
    pub(crate) at: String,
    pub(crate) body: String,
    pub(crate) file: Option<String>,
    pub(crate) line: Option<u64>,
    pub(crate) hidden: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Discussion {
    pub(crate) id: String,
    pub(crate) file: String,
    pub(crate) line: Option<u64>,
    pub(crate) resolved: bool,
    pub(crate) outdated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReviewAction {
    Approve,
    Comment,
    RequestChanges,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub(crate) enum DiffSide {
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct FileComment {
    pub(crate) path: String,
    pub(crate) line: u64,
    pub(crate) side: DiffSide,
    pub(crate) body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) start_line: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) start_side: Option<DiffSide>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct FileLevelComment {
    pub(crate) path: String,
    pub(crate) body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ReviewPost {
    pub(crate) key: String,
    pub(crate) head_sha: String,
    pub(crate) action: ReviewAction,
    pub(crate) body: String,
    #[serde(rename = "fileComments")]
    pub(crate) file_comments: Vec<FileComment>,
    #[serde(rename = "fileLevelComments")]
    pub(crate) file_level_comments: Vec<FileLevelComment>,
}

impl ReviewPost {
    pub(crate) fn retry(&self, partial: &ReviewPartial) -> Self {
        Self {
            key: self.key.clone(),
            head_sha: self.head_sha.clone(),
            action: partial.retry.action,
            body: partial.retry.body.clone().unwrap_or_default(),
            file_comments: partial.retry.file_comments.clone(),
            file_level_comments: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PostedReview {
    pub(crate) ok: bool,
    #[serde(rename = "prUrl")]
    pub(crate) pr_url: String,
    pub(crate) submission: ReviewSubmission,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub(crate) enum ReviewSubmission {
    Complete {
        #[serde(rename = "postedAs")]
        posted_as: Option<ReviewAction>,
    },
    Partial(ReviewPartial),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReviewPartial {
    pub(crate) posted_file_comment_count: usize,
    pub(crate) failed_file_comments: Vec<CommentFailure>,
    pub(crate) review_body_posted: bool,
    pub(crate) approval: ApprovalOutcome,
    pub(crate) approval_error: Option<String>,
    pub(crate) review_body_error: Option<String>,
    pub(crate) retry: ReviewRetry,
    pub(crate) posted_as: Option<ReviewAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ApprovalOutcome {
    NotRequested,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CommentFailure {
    pub(crate) comment: FileComment,
    pub(crate) error: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ReviewRetry {
    pub(crate) action: ReviewAction,
    #[serde(rename = "fileComments")]
    pub(crate) file_comments: Vec<FileComment>,
    pub(crate) body: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Response<T> {
    #[serde(rename = "serverSession")]
    server_session: String,
    #[serde(flatten)]
    answer: T,
}

impl<T> Response<T> {
    fn checked(self, session: &str) -> Result<T> {
        ensure!(self.server_session == session, "Inbox serverSession changed; rediscover before continuing");
        Ok(self.answer)
    }
}

fn thread_path(id: &str) -> Result<String> {
    ensure!(
        !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
        "Thread PRs are only supported for local Inbox thread ids: {id:?}"
    );
    Ok(format!("/api/inbox/threads/{id}/pulls"))
}

fn query_path(path: &str, pairs: &[(&str, &str)]) -> String {
    let query = form_urlencoded::Serializer::new(String::new()).extend_pairs(pairs.iter().copied()).finish();
    if query.is_empty() { path.to_owned() } else { format!("{path}?{query}") }
}

fn session_body(session: &str, fields: impl Serialize) -> Result<Value> {
    let mut body = serde_json::to_value(fields)?;
    body.as_object_mut()
        .context("Pull write must be an object")?
        .insert("serverSession".into(), json!(session));
    Ok(body)
}

impl Client {
    fn pull_get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let response: Response<T> = self.get_json("GET", path)?;
        response.checked(self.server_session())
    }

    fn pull_post<T: DeserializeOwned>(&self, path: &str, fields: impl Serialize) -> Result<T> {
        let response: Response<T> = self.post_json(path, &session_body(self.server_session(), fields)?)?;
        response.checked(self.server_session())
    }

    pub(crate) fn thread_pulls(&self, thread_id: &str) -> Result<ThreadPulls> {
        self.pull_get(&thread_path(thread_id)?)
    }

    pub(crate) fn change_thread_pulls(&self, thread_id: &str, change: &PullChange) -> Result<ThreadPulls> {
        self.pull_post(&thread_path(thread_id)?, change)
    }

    pub(crate) fn pulls(&self, options: PullOptions) -> Result<PullList> {
        let mut pairs = Vec::new();
        if options.refresh {
            pairs.push(("refresh", "1"));
        }
        if options.others {
            pairs.push(("others", "1"));
        }
        if options.everywhere {
            pairs.push(("everywhere", "1"));
        }
        self.pull_get(&query_path("/api/inbox/pulls", &pairs))
    }

    pub(crate) fn pull_details(&self, key: &str) -> Result<PullDetails> {
        self.pull_get(&query_path("/api/inbox/pulls/details", &[("key", key)]))
    }

    pub(crate) fn pull_diff(&self, key: &str, scope: DiffScope) -> Result<PullDiff> {
        self.pull_get(&query_path("/api/inbox/pulls/diff", &[("key", key), ("scope", scope.wire())]))
    }

    pub(crate) fn post_pull_review(&self, review: &ReviewPost) -> Result<PostedReview> {
        self.pull_post("/api/inbox/pulls/review", review)
    }
}
