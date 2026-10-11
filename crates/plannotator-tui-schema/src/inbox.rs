//! Plannotator Inbox wire shapes.
//!
//! Mirrors `packages/inbox/api.ts` and `@plannotator/core/inbox-types.ts`.
//! Zero I/O, pure serialization/deserialization.

use serde::{Deserialize, Serialize};

/// The server health response (`GET /api/inbox/health`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxHealth {
    pub ok: bool,
    pub app: String,
    pub version: String,
    #[serde(rename = "serverSession")]
    pub server_session: String,
    pub pid: u32,
    #[serde(default)]
    pub update: Option<InboxUpdate>,
}

/// The Inbox update state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxUpdate {
    #[serde(default)]
    pub available: bool,
    pub state: String,
    pub version: String,
    pub current: String,
    pub action: String,
    #[serde(rename = "releaseUrl")]
    pub release_url: String,
    #[serde(rename = "publishedAt", default)]
    pub published_at: Option<String>,
}

/// A project folder as listed in `ListModel`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectFolder {
    pub id: String,
    pub key: String,
    pub name: String,
    pub root: String,
    pub created_at: String,
    #[serde(default)]
    pub threads: usize,
    #[serde(default)]
    pub unread: usize,
}

/// An Inbox project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxProject {
    pub id: String,
    pub key: String,
    pub name: String,
    pub root: String,
    pub created_at: String,
    #[serde(default)]
    pub repo: Option<InboxProjectRepo>,
}

/// The forge repository a project's git origin names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxProjectRepo {
    pub display: String,
    #[serde(default)]
    pub host: Option<String>,
}

/// Who sent a message: person or agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InboxAuthor {
    Person,
    Agent {
        #[serde(default)]
        host: Option<String>,
        #[serde(default)]
        session: Option<String>,
        #[serde(default)]
        name: Option<String>,
    },
}

impl InboxAuthor {
    pub fn display_name(&self) -> &str {
        match self {
            Self::Person => "You",
            Self::Agent { name, host, .. } => {
                if let Some(n) = name.as_deref() {
                    n
                } else if let Some(h) = host.as_deref() {
                    match h {
                        "claude-code" | "claude" => "Claude Code",
                        "omp" => "omp",
                        "pi" => "Pi",
                        "opencode" => "OpenCode",
                        "codex" => "Codex",
                        "cursor" => "Cursor",
                        other => other,
                    }
                } else {
                    "An agent"
                }
            }
        }
    }

    pub fn host(&self) -> Option<&str> {
        match self {
            Self::Agent { host, .. } => host.as_deref(),
            Self::Person => None,
        }
    }
}

/// An attached file on an inbox message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxAttachment {
    pub id: String,
    pub path: String,
    #[serde(default)]
    pub named_path: Option<String>,
    pub name: String,
    pub kind: String,
    pub sent_sha256: String,
    pub size: usize,
    pub sent_at: String,
    pub sent_mtime: String,
    #[serde(default)]
    pub media_type: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

/// Approved inbox section identifiers in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InboxSectionId {
    Stopped,
    Holding,
    Waiting,
    Sent,
    New,
    Quiet,
}

impl InboxSectionId {
    pub fn label(self) -> &'static str {
        match self {
            Self::Stopped => "Stopped on you",
            Self::Holding => "Holding up work",
            Self::Waiting => "Waiting on you",
            Self::Sent => "Sent",
            Self::New => "New since you looked",
            Self::Quiet => "Quiet",
        }
    }

    pub const ALL: [Self; 6] =
        [Self::Stopped, Self::Holding, Self::Waiting, Self::Sent, Self::New, Self::Quiet];
}

/// One section of the thread list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxListSection {
    pub id: InboxSectionId,
    pub label: String,
    pub threads: Vec<InboxListRow>,
}

/// One row of the thread list.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxListRow {
    pub thread_id: String,
    pub project_id: String,
    pub subject: Option<String>,
    pub author: InboxAuthor,
    pub created_at: String,
    pub last_at: String,
    pub last_author: String,
    pub message_count: usize,
    pub resolved_at: Option<String>,
    pub questions: InboxQuestionSummary,
    pub waiting_on_person: bool,
    pub project: InboxRowProject,
    #[serde(default)]
    pub thread_name: Option<String>,
    #[serde(default)]
    pub section: Option<InboxSectionId>,
    #[serde(default)]
    pub unread: bool,
    #[serde(default)]
    pub answered_not_sent: bool,
    #[serde(default)]
    pub waiting_since: Option<String>,
    #[serde(default)]
    pub unseen: usize,
    #[serde(default)]
    pub sent: Option<InboxRowSent>,
    #[serde(default)]
    pub guide: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxRowProject {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxRowSent {
    pub at: String,
    #[serde(default)]
    pub checked_at: Option<String>,
}

/// Notification question summary on a list row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxQuestionSummary {
    pub open: usize,
    pub picked: usize,
    pub stopped: bool,
    #[serde(default)]
    pub holds_up: Vec<String>,
    #[serde(default)]
    pub prompt: Option<String>,
}

/// The list response (`GET /api/inbox/threads[?project=]`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListModel {
    #[serde(rename = "serverSession")]
    pub server_session: String,
    pub version: String,
    pub cursor: u64,
    #[serde(default)]
    pub update: Option<InboxUpdate>,
    #[serde(default)]
    pub updated: Option<String>,
    #[serde(default)]
    pub notice: Option<String>,
    pub projects: Vec<ProjectFolder>,
    #[serde(default)]
    pub project: Option<String>,
    pub sections: Vec<InboxListSection>,
    pub decisions_waiting: usize,
}

/// One thread detail response (`GET /api/inbox/threads/:id`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadModel {
    #[serde(rename = "serverSession")]
    pub server_session: String,
    pub cursor: u64,
    pub thread: InboxThread,
    #[serde(default)]
    pub decisions: Vec<InboxDecision>,
}

/// One inbox thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxThread {
    pub thread_id: String,
    pub project: InboxProject,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub thread_name: Option<String>,
    #[serde(default)]
    pub resolved_at: Option<String>,
    pub messages: Vec<InboxMessageWire>,
}

/// One message on the wire, including parsed questions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxMessageWire {
    pub id: String,
    pub project_id: String,
    pub thread_id: String,
    #[serde(default)]
    pub reply_to: Option<String>,
    pub author: InboxAuthor,
    #[serde(default)]
    pub subject: Option<String>,
    pub body: String,
    pub created_at: String,
    #[serde(default)]
    pub resolved_at: Option<String>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub thread_name: Option<String>,
    #[serde(default)]
    pub person_seen_seq: Option<u64>,
    #[serde(default)]
    pub agent_checked_seq: Option<u64>,
    #[serde(default)]
    pub agent_checked_at: Option<String>,
    #[serde(default)]
    pub attachments: Option<Vec<InboxAttachment>>,
    #[serde(default)]
    pub delivery: Option<InboxDeliveryRecord>,
    #[serde(default)]
    pub questions: Option<Vec<InboxQuestion>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxDeliveryRecord {
    pub state: String,
    pub host: String,
    pub session: String,
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxQuestionChoice {
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub recommended: bool,
    #[serde(default)]
    pub settled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionKind {
    Single,
    Multi,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InboxQuestionState {
    Open,
    Picked,
    Sent,
    Closed,
    Superseded,
}

/// An answered question record on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionAnswer {
    pub v: u32,
    pub key: String,
    pub kind: QuestionKind,
    pub prompt: String,
    #[serde(default)]
    pub selected: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<bool>,
    #[serde(rename = "sourceLine", default, skip_serializing_if = "Option::is_none")]
    pub source_line: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxPicker {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxDecisionDraft {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxSupersededBy {
    pub message_id: String,
    #[serde(default)]
    pub key: Option<String>,
}

/// One question in a message as served by the Inbox API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxQuestion {
    pub key: String,
    pub position: usize,
    pub kind: QuestionKind,
    pub prompt: String,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub choices: Vec<InboxQuestionChoice>,
    #[serde(default)]
    pub recommendation: Option<String>,
    #[serde(default)]
    pub suggested_text: Option<String>,
    #[serde(default)]
    pub decision_on_answer: bool,
    #[serde(default)]
    pub stopped: Option<String>,
    #[serde(default)]
    pub holds_up: Vec<String>,
    #[serde(default)]
    pub asked_by_agent_id: Option<String>,
    #[serde(default)]
    pub orphaned: bool,
    pub state: InboxQuestionState,
    #[serde(default)]
    pub answer: Option<QuestionAnswer>,
    pub revision: u64,
    pub sent_revision: u64,
    #[serde(default)]
    pub picked_by: Option<InboxPicker>,
    #[serde(default)]
    pub picked_at: Option<String>,
    #[serde(default)]
    pub sent_reply_id: Option<String>,
    #[serde(default)]
    pub decision_id: Option<String>,
    pub message_id: String,
    #[serde(default)]
    pub decision_recording: bool,
    #[serde(default)]
    pub decision_draft: Option<InboxDecisionDraft>,
    #[serde(default)]
    pub superseded_by: Option<InboxSupersededBy>,
}

/// One standing decision in a project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxDecision {
    pub id: String,
    pub project_id: String,
    pub text: String,
    #[serde(default)]
    pub reason: Option<String>,
    pub source: InboxDecisionSource,
    pub state: String,
    pub version: u64,
    #[serde(default)]
    pub replaces_id: Option<String>,
    #[serde(default)]
    pub replacement_id: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub changed_at: Option<String>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxDecisionSource {
    pub kind: String,
    #[serde(default)]
    pub question_id: Option<String>,
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub agent: Option<InboxDecisionAgent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxDecisionAgent {
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

// ── Requests & Responses ──

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeenRequest {
    #[serde(rename = "serverSession")]
    pub server_session: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PickItem {
    pub key: String,
    pub revision: u64,
    pub answer: Option<QuestionAnswer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PicksRequest {
    #[serde(rename = "serverSession")]
    pub server_session: String,
    pub questions: Vec<PickItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PicksResponse {
    pub message_id: String,
    pub questions: Vec<InboxQuestion>,
    #[serde(default)]
    pub reply: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyQuestionRef {
    pub key: String,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyRequest {
    #[serde(rename = "serverSession")]
    pub server_session: String,
    pub idempotency_key: String,
    pub words: String,
    pub questions: Vec<ReplyQuestionRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotation_ids: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyResult {
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(default)]
    pub questions: Vec<InboxQuestion>,
    #[serde(default)]
    pub reply: Option<serde_json::Value>,
    #[serde(default)]
    pub replayed: bool,
    #[serde(default)]
    pub decisions: Vec<InboxDecision>,
    #[serde(default)]
    pub decisions_refused: Vec<DecisionRefusal>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionRefusal {
    pub key: String,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolveRequest {
    #[serde(rename = "serverSession")]
    pub server_session: String,
    pub resolved: bool,
}

// ── SSE Event Shapes ──

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SseHello {
    #[serde(rename = "serverSession")]
    pub server_session: String,
    pub cursor: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SseStatus {
    pub update: Option<InboxUpdate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxEventRecord {
    pub seq: u64,
    pub kind: String,
    pub id: String,
    #[serde(default)]
    pub project: Option<InboxProject>,
    #[serde(default)]
    pub message: Option<InboxMessageWire>,
    #[serde(default)]
    pub question: Option<InboxQuestion>,
    #[serde(default)]
    pub decision: Option<InboxDecision>,
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboxServerEvent {
    Hello(SseHello),
    Record(InboxEventRecord),
    Status(SseStatus),
    Ping,
}
