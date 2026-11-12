//! Wire data for Inbox guide snapshots (versions 1–5), never working-tree diffs.
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Deserialize)]
pub(super) struct Snapshot {
    pub kind: String,
    pub version: u64,
    pub guide: Guide,
    pub review: Review,
}

impl Snapshot {
    pub(super) fn check(&self) -> Result<()> {
        ensure!(self.kind == "plannotator-guided-review", "not a Plannotator guided review");
        ensure!((1..=5).contains(&self.version), "unsupported guide snapshot version {}", self.version);
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct Guide {
    pub title: String,
    #[serde(default)]
    pub intent: String,
    pub overview: Option<Overview>,
    pub sections: Vec<Chapter>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct Overview {
    pub text: String,
    pub diagram: Option<Diagram>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct Diagram {
    pub kind: String,
    pub source: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct Chapter {
    pub title: String,
    pub overview: String,
    pub diffs: Vec<Reference>,
    pub diagram: Option<Diagram>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct Reference {
    pub file: String,
    pub summary: Option<String>,
    #[serde(default)]
    pub notes: Vec<Note>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Note {
    pub line_start: usize,
    pub line_end: usize,
    pub side: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Review {
    pub raw_patch: String,
    #[serde(default)]
    pub git_ref: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct GuideAnswer {
    pub snapshot: Snapshot,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GuideLink {
    pub(crate) id: String,
    pub(crate) guide: GuideRef,
    #[serde(default)]
    pub(super) guide_reviewed: Vec<bool>,
    #[serde(default)]
    pub(super) guide_viewed: Vec<String>,
    #[serde(default)]
    pub(super) seq: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GuideRef {
    pub(crate) title: String,
    #[serde(default)]
    pub(crate) review: bool,
    #[serde(default)]
    pub(crate) walkthrough: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct Record {
    pub id: String,
    pub message_id: String,
    pub guide: Option<Value>,
    pub annotation: Value,
}

#[derive(Debug, Deserialize)]
pub(super) struct Attachments {
    #[serde(rename = "serverSession")]
    pub server_session: String,
    pub annotations: Vec<Record>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Contents {
    pub old_content: Option<String>,
    pub new_content: Option<String>,
}

pub(super) fn links(thread: &Value) -> Result<Vec<GuideLink>> {
    thread
        .get("messages")
        .and_then(Value::as_array)
        .context("Inbox thread omitted messages")?
        .iter()
        .filter(|message| message.get("guide").is_some_and(Value::is_object))
        .map(|message| serde_json::from_value(message.clone()).context("reading Inbox guide card"))
        .collect()
}

pub(super) fn read_only(thread: &Value, message: &GuideLink) -> bool {
    thread.get("resolved_at").is_some_and(|value| !value.is_null())
        && thread.get("archive_seq").and_then(Value::as_u64).is_none_or(|seq| message.seq <= seq)
}

pub(super) fn segment(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}
