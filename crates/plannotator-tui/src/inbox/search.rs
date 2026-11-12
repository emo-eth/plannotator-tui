//! The Inbox's server-backed search and list-filtering API.
//!
//! Search and filtering query the server's index, which owns tokenization,
//! matching, ranking, and status interpretation. Pagination requests continue
//! one result group at a time. Saved recent searches change only through the
//! explicit remember/clear methods after a user action.

use anyhow::{Result, ensure};
use serde::Deserialize;
use serde_json::json;
use url::form_urlencoded;

use super::client::Client;

/// Server search result groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SearchGroup {
    Threads,
    Messages,
    Files,
    Projects,
}

impl SearchGroup {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Threads => "threads",
            Self::Messages => "messages",
            Self::Files => "files",
            Self::Projects => "projects",
        }
    }
}

/// Server status filters for both text searches and an empty-query list browse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchStatus {
    Waiting,
    New,
    Sent,
    Quiet,
    Archived,
}

impl SearchStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::New => "new",
            Self::Sent => "sent",
            Self::Quiet => "quiet",
            Self::Archived => "archived",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchFrom {
    Agent,
    Me,
}

impl SearchFrom {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Me => "me",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SearchSort {
    #[default]
    Best,
    Newest,
}

/// A search request. Filters are interpreted by the server, including for an
/// empty `q` (a filtered thread-list browse); an entirely empty request asks
/// for the server's recent view.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SearchQuery {
    pub(crate) q: String,
    pub(crate) project: Option<String>,
    pub(crate) status: Option<SearchStatus>,
    pub(crate) from: Option<SearchFrom>,
    pub(crate) group: Option<SearchGroup>,
    pub(crate) offset: usize,
    pub(crate) limit: Option<usize>,
    /// Ranking tie-break only; it is never a project filter.
    pub(crate) current: Option<String>,
    pub(crate) sort: SearchSort,
}

impl SearchQuery {
    /// A request for one server-ranked group page. `offset` is the number of
    /// rows already displayed in that group, not the number of results in all
    /// groups.
    pub(crate) fn page(&self, group: SearchGroup, offset: usize) -> Self {
        let mut next = self.clone();
        next.group = Some(group);
        next.offset = offset;
        next.limit = Some(SEARCH_PAGE_SIZE);
        next
    }

    fn path(&self) -> Result<String> {
        ensure!(self.offset <= SEARCH_OFFSET_MAX, "Inbox search offset is too large");
        if let Some(limit) = self.limit {
            ensure!(
                (1..=SEARCH_PAGE_SIZE).contains(&limit),
                "Inbox search limit must be from 1 to {SEARCH_PAGE_SIZE}"
            );
        }
        let mut params = form_urlencoded::Serializer::new(String::new());
        params.append_pair("q", &self.q);
        if let Some(project) = &self.project {
            params.append_pair("project", project);
        }
        if let Some(status) = self.status {
            params.append_pair("status", status.as_str());
        }
        if let Some(from) = self.from {
            params.append_pair("from", from.as_str());
        }
        if let Some(group) = self.group {
            params.append_pair("group", group.as_str());
        }
        if self.offset > 0 {
            params.append_pair("offset", &self.offset.to_string());
        }
        if let Some(limit) = self.limit {
            params.append_pair("limit", &limit.to_string());
        }
        if let Some(current) = &self.current {
            params.append_pair("current", current);
        }
        if self.sort == SearchSort::Newest {
            params.append_pair("sort", "newest");
        }
        Ok(format!("/api/inbox/search?{}", params.finish()))
    }
}

const SEARCH_PAGE_SIZE: usize = 50;
const SEARCH_OFFSET_MAX: usize = 1_000_000;

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub(crate) struct SearchProjectRef {
    pub(crate) id: String,
    pub(crate) name: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct SearchThreadRow {
    pub(crate) id: String,
    pub(crate) thread_id: String,
    pub(crate) title: String,
    /// `[start, length]` pairs in `title`.
    pub(crate) title_marks: Vec<[usize; 2]>,
    pub(crate) thread_name: Option<String>,
    pub(crate) project: SearchProjectRef,
    pub(crate) section: Option<String>,
    pub(crate) archived: bool,
    /// ISO timestamp for the thread's latest message.
    pub(crate) at: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct SearchAuthor {
    /// `agent` or `me`.
    pub(crate) kind: String,
    pub(crate) name: String,
    pub(crate) host: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct SearchSnippet {
    pub(crate) text: String,
    /// `[start, length]` pairs in `text`.
    pub(crate) marks: Vec<[usize; 2]>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct SearchQuestionHit {
    pub(crate) key: String,
    pub(crate) replaced: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct SearchMessageRow {
    pub(crate) id: String,
    pub(crate) thread_id: String,
    pub(crate) project: SearchProjectRef,
    pub(crate) section: Option<String>,
    pub(crate) archived: bool,
    /// ISO timestamp for this message.
    pub(crate) at: String,
    pub(crate) author: SearchAuthor,
    pub(crate) title: String,
    pub(crate) thread_name: Option<String>,
    pub(crate) snippet: SearchSnippet,
    pub(crate) question: Option<SearchQuestionHit>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct SearchFileRow {
    pub(crate) id: String,
    pub(crate) thread_id: String,
    pub(crate) message_id: String,
    pub(crate) project: SearchProjectRef,
    pub(crate) section: Option<String>,
    pub(crate) archived: bool,
    pub(crate) at: String,
    pub(crate) name: String,
    /// `[start, length]` pairs in `name`.
    pub(crate) name_marks: Vec<[usize; 2]>,
    pub(crate) folder: Option<String>,
    pub(crate) kind: String,
    pub(crate) title: String,
    pub(crate) thread_name: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct SearchProjectRow {
    pub(crate) id: String,
    pub(crate) project: SearchProjectRef,
    pub(crate) name: String,
    /// `[start, length]` pairs in `name`.
    pub(crate) name_marks: Vec<[usize; 2]>,
    pub(crate) root: String,
    pub(crate) at: String,
}

/// One indexed result. `guide` is separate from `file` but shares the same
/// attachment-like fields on the wire and occupies the Files result group.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum SearchRow {
    #[serde(rename = "thread")]
    Thread(SearchThreadRow),
    #[serde(rename = "message")]
    Message(SearchMessageRow),
    #[serde(rename = "file")]
    File(SearchFileRow),
    #[serde(rename = "guide")]
    Guide(SearchFileRow),
    #[serde(rename = "project")]
    Project(SearchProjectRow),
}

impl SearchRow {
    pub(crate) fn id(&self) -> &str {
        match self {
            Self::Thread(row) => &row.id,
            Self::Message(row) => &row.id,
            Self::File(row) | Self::Guide(row) => &row.id,
            Self::Project(row) => &row.id,
        }
    }

    pub(crate) const fn kind(&self) -> &'static str {
        match self {
            Self::Thread(_) => "thread",
            Self::Message(_) => "message",
            Self::File(_) => "file",
            Self::Guide(_) => "guide",
            Self::Project(_) => "project",
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct SearchGroups {
    pub(crate) threads: Vec<SearchRow>,
    pub(crate) messages: Vec<SearchRow>,
    pub(crate) files: Vec<SearchRow>,
    pub(crate) projects: Vec<SearchRow>,
}

impl SearchGroups {
    pub(crate) fn get(&self, group: SearchGroup) -> &[SearchRow] {
        match group {
            SearchGroup::Threads => &self.threads,
            SearchGroup::Messages => &self.messages,
            SearchGroup::Files => &self.files,
            SearchGroup::Projects => &self.projects,
        }
    }

    fn get_mut(&mut self, group: SearchGroup) -> &mut Vec<SearchRow> {
        match group {
            SearchGroup::Threads => &mut self.threads,
            SearchGroup::Messages => &mut self.messages,
            SearchGroup::Files => &mut self.files,
            SearchGroup::Projects => &mut self.projects,
        }
    }

    fn extend_distinct(&mut self, group: SearchGroup, page: impl IntoIterator<Item = SearchRow>) -> usize {
        let rows = self.get_mut(group);
        let mut seen: std::collections::HashSet<(String, String)> =
            rows.iter().map(|row| (row.kind().to_owned(), row.id().to_owned())).collect();
        let mut added = 0;
        for row in page {
            if seen.insert((row.kind().to_owned(), row.id().to_owned())) {
                rows.push(row);
                added += 1;
            }
        }
        added
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct SearchTotals {
    pub(crate) threads: usize,
    pub(crate) messages: usize,
    pub(crate) files: usize,
    pub(crate) projects: usize,
}

impl SearchTotals {
    pub(crate) const fn get(&self, group: SearchGroup) -> usize {
        match group {
            SearchGroup::Threads => self.threads,
            SearchGroup::Messages => self.messages,
            SearchGroup::Files => self.files,
            SearchGroup::Projects => self.projects,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct SearchAnswer {
    pub(crate) server_session: String,
    pub(crate) q: String,
    pub(crate) terms: Vec<String>,
    pub(crate) took_ms: u64,
    pub(crate) indexing: bool,
    pub(crate) threads: usize,
    pub(crate) totals: Option<SearchTotals>,
    pub(crate) groups: Option<SearchGroups>,
    pub(crate) recent: Option<Vec<SearchThreadRow>>,
    #[serde(rename = "recent_searches")]
    pub(crate) recent_searches: Option<Vec<String>>,
}

impl SearchAnswer {
    pub(crate) fn rows(&self, group: SearchGroup) -> &[SearchRow] {
        self.groups.as_ref().map_or(&[], |groups| groups.get(group))
    }

    pub(crate) fn can_load_more(&self, group: SearchGroup) -> bool {
        self.totals.as_ref().is_some_and(|totals| totals.get(group) > self.rows(group).len())
    }

    /// Offset for the next server page. This is the count already retained,
    /// including results from any previous pages.
    pub(crate) fn next_offset(&self, group: SearchGroup) -> usize {
        self.rows(group).len()
    }

    /// Merge a server-backed page into the matching group. Duplicate rows are
    /// discarded because new matches can arrive between offset-based reads.
    /// Returns the number of rows added; an indexing or different-query answer
    /// is not a page and is left untouched.
    pub(crate) fn append_page(&mut self, group: SearchGroup, page: SearchAnswer) -> usize {
        if page.indexing || page.q.trim() != self.q.trim() || page.server_session != self.server_session {
            return 0;
        }
        let Some(page_rows) = page.groups.map(|groups| match group {
            SearchGroup::Threads => groups.threads,
            SearchGroup::Messages => groups.messages,
            SearchGroup::Files => groups.files,
            SearchGroup::Projects => groups.projects,
        }) else {
            return 0;
        };
        self.groups.get_or_insert_with(SearchGroups::default).extend_distinct(group, page_rows)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct RecentSearchesResponse {
    recent_searches: Vec<String>,
}

impl Client {
    /// Remember a search after its result is opened, as the Inbox window does.
    /// This is an explicit user-initiated write; merely searching does not
    /// change recent searches.
    pub(crate) fn remember_search(&self, q: &str) -> Result<Vec<String>> {
        let response: RecentSearchesResponse = self.post_json(
            "/api/inbox/search/recent",
            &json!({ "serverSession": self.server_session(), "q": q }),
        )?;
        Ok(response.recent_searches)
    }

    /// Clear the server's saved recent searches after an explicit clear action.
    pub(crate) fn clear_recent_searches(&self) -> Result<Vec<String>> {
        let response: RecentSearchesResponse = self.post_json(
            "/api/inbox/search/recent",
            &json!({ "serverSession": self.server_session(), "clear": true }),
        )?;
        Ok(response.recent_searches)
    }
}

impl Client {
    /// Ask the Inbox's indexed search API for results or an empty-query filtered
    /// list. Search and list filters are server evaluated, never applied to just
    /// the currently loaded client rows.
    pub(crate) fn search(&self, query: &SearchQuery) -> Result<SearchAnswer> {
        let answer: SearchAnswer = self.get_json("GET", &query.path()?)?;
        ensure!(
            answer.server_session == self.server_session(),
            "GET {}: Inbox serverSession changed; rediscover before continuing",
            self.url()
        );
        Ok(answer)
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "tests assert valid search fixtures and expected rows"
)]
mod tests {
    use super::*;
    use url::Url;

    #[test]
    fn query_encodes_text_and_all_supported_server_filters() {
        let query = SearchQuery {
            q: "résumé + retry".to_owned(),
            project: Some("prj_01ABC".to_owned()),
            status: Some(SearchStatus::Archived),
            from: Some(SearchFrom::Me),
            current: Some("prj_current".to_owned()),
            sort: SearchSort::Newest,
            ..SearchQuery::default()
        };
        let url = Url::parse(&format!("http://localhost{}", query.path().expect("path"))).expect("URL");
        let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(params.get("q").map(String::as_str), Some("résumé + retry"));
        assert_eq!(params.get("project").map(String::as_str), Some("prj_01ABC"));
        assert_eq!(params.get("status").map(String::as_str), Some("archived"));
        assert_eq!(params.get("from").map(String::as_str), Some("me"));
        assert_eq!(params.get("current").map(String::as_str), Some("prj_current"));
        assert_eq!(params.get("sort").map(String::as_str), Some("newest"));
        assert!(!params.contains_key("group"));
        assert!(!params.contains_key("offset"));
    }

    #[test]
    fn group_page_keeps_filters_and_requests_the_server_page() {
        let query = SearchQuery {
            q: "retry".to_owned(),
            status: Some(SearchStatus::Waiting),
            project: Some("prj_one".to_owned()),
            sort: SearchSort::Newest,
            ..SearchQuery::default()
        }
        .page(SearchGroup::Messages, 50);
        let url = Url::parse(&format!("http://localhost{}", query.path().expect("path"))).expect("URL");
        let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(params.get("group").map(String::as_str), Some("messages"));
        assert_eq!(params.get("offset").map(String::as_str), Some("50"));
        assert_eq!(params.get("limit").map(String::as_str), Some("50"));
        assert_eq!(params.get("status").map(String::as_str), Some("waiting"));
        assert_eq!(params.get("project").map(String::as_str), Some("prj_one"));
        assert_eq!(params.get("sort").map(String::as_str), Some("newest"));
    }

    #[test]
    fn next_offset_is_per_group_and_page_merge_deduplicates_shifted_rows() {
        let mut answer: SearchAnswer = serde_json::from_str(
            r#"{"serverSession":"s","q":"rate","totals":{"threads":1,"messages":3,"files":0,"projects":0},"groups":{"threads":[],"messages":[{"type":"message","id":"m1"}]}}"#,
        )
        .expect("initial answer");
        let page: SearchAnswer = serde_json::from_str(
            r#"{"serverSession":"s","q":"rate","groups":{"messages":[{"type":"message","id":"m1"},{"type":"message","id":"m2"}]}}"#,
        )
        .expect("page");
        assert_eq!(answer.next_offset(SearchGroup::Messages), 1);
        assert!(answer.can_load_more(SearchGroup::Messages));
        assert_eq!(answer.append_page(SearchGroup::Messages, page), 1);
        assert_eq!(answer.next_offset(SearchGroup::Messages), 2);
        assert_eq!(
            answer.rows(SearchGroup::Messages).iter().map(SearchRow::id).collect::<Vec<_>>(),
            ["m1", "m2"]
        );
    }

    #[test]
    fn rejects_server_page_parameters_outside_endpoint_limits() {
        assert!(SearchQuery { offset: SEARCH_OFFSET_MAX + 1, ..SearchQuery::default() }.path().is_err());
        assert!(SearchQuery { limit: Some(SEARCH_PAGE_SIZE + 1), ..SearchQuery::default() }.path().is_err());
        assert!(SearchQuery { limit: Some(0), ..SearchQuery::default() }.path().is_err());
    }

    #[test]
    fn deserializes_typed_results_and_search_recent_state() {
        let answer: SearchAnswer = serde_json::from_str(
            r#"{
                "serverSession":"s","q":"retry","terms":["retry"],"took_ms":2,
                "indexing":false,"threads":12,
                "totals":{"threads":1,"messages":1,"files":1,"projects":1},
                "groups":{
                    "threads":[{"type":"thread","id":"m1","thread_id":"m1","title":"Retry","title_marks":[[0,5]],"project":{"id":"p1","name":"Project"},"section":"waiting","archived":false,"at":"2026-01-01T00:00:00Z"}],
                    "messages":[{"type":"message","id":"m2","thread_id":"m1","author":{"kind":"agent","name":"Claude"},"snippet":{"text":"retry soon","marks":[[0,5]]}}],
                    "files":[{"type":"guide","id":"m3","thread_id":"m1","message_id":"m3","name":"Review guide","kind":"guide"}],
                    "projects":[{"type":"project","id":"p1","project":{"id":"p1","name":"Project"},"name":"Project","root":"/project","at":"2026-01-01T00:00:00Z"}]
                },
                "recent":[{"id":"m1","thread_id":"m1","title":"Retry","project":{"id":"p1","name":"Project"}}],
                "recent_searches":["retry budget"]
            }"#,
        )
        .expect("search answer");
        assert_eq!(answer.rows(SearchGroup::Threads).len(), 1);
        assert_eq!(answer.rows(SearchGroup::Messages).len(), 1);
        assert_eq!(answer.rows(SearchGroup::Files)[0].kind(), "guide");
        assert_eq!(answer.rows(SearchGroup::Projects)[0].id(), "p1");
        assert_eq!(answer.recent.as_ref().map(Vec::len), Some(1));
        assert_eq!(answer.recent_searches.as_deref(), Some(["retry budget".to_owned()].as_slice()));
    }
}
