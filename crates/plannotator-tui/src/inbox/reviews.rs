//! Inbox reviews: stored walkthroughs, changed files, line/global comments and verdicts.
//!
//! `run` borrows the Inbox terminal; it never starts a server or reads a working-tree diff.
//! Tab switches Walkthrough/All files, [] jumps chapters/files, v selects a line range,
//! c/C comments on selected lines/the whole review, m lists comments, e edits and d removes.
//! x marks a chapter Reviewed; V marks a file Viewed. s opens the explicit send decision,
//! n adds a note, Enter confirms. Failed sends retain their exact idempotent request for retry.
//! Plain guides and teammate reviews offer feedback, never an approval. Archived messages are
//! read-only, except messages after the thread's archive sequence (the web Inbox's rule).
//!
//! a stages selected code in Ask; z/Z stages Show me for lines/the overview. The Ask
//! pane owns its live bridge and returned boards open through Shapes. O opens this exact
//! review in the web Inbox for graphical diagrams. Notes and place survive reopening in
//! this process; saved comments/ticks live in the server, not the terminal cache.
//! The terminal currently draws unified diffs and diagram source, not a graphical canvas.
//! p opens verified guide-to-forge posting: only matching diff anchors go inline; unmatched
//! comments remain quoted in the review body, and partial submissions have narrowed retries.

mod ask;
mod comments;
pub(crate) mod diff;
mod draw;
mod feedback;
mod input;
mod model;
mod run;
mod send;
#[cfg(test)]
mod tests;

use super::client::Client;
use crate::app::compose::Compose;
use anyhow::{Context, Result, ensure};
pub(crate) use feedback::guide_feedback;
use model::{Attachments, Contents, GuideAnswer, GuideLink, Record, Snapshot, segment};
use ratatui::text::Line;
pub(crate) use run::{run, run_guide};
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReviewAction {
    Stay,
    Close,
    PostForge,
    Ask { words: String, show_me: Option<Value> },
}

#[derive(Debug, Clone)]
struct Location {
    file: usize,
    old: Option<usize>,
    new: Option<usize>,
    hunk: usize,
    text: String,
    outside: bool,
}

#[derive(Debug, Clone)]
struct RenderRow {
    line: Line<'static>,
    location: Option<Location>,
    file: Option<usize>,
    chapter: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Walkthrough,
    Files,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Old,
    New,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SendKind {
    Approve,
    Changes,
    ApproveComments,
    ApprovalNote,
    DiscardApprove,
    Feedback,
}

#[derive(Debug)]
enum Mode {
    Browse,
    Comments,
    Comment(Value),
    Suggestion(Value),
    Send(SendKind),
    Note(SendKind),
    Remove(String),
    Jump,
}

pub(crate) struct ReviewPane {
    thread_id: String,
    message: GuideLink,
    snapshot: Snapshot,
    files: Vec<diff::File>,
    records: Vec<Record>,
    read_only: bool,
    code_review: bool,
    verdict: bool,
    view: View,
    mode: Mode,
    compose: Compose,
    note: String,
    rows: Vec<RenderRow>,
    width: usize,
    cursor: usize,
    scroll: usize,
    height: usize,
    horizontal: u16,
    selected_comment: usize,
    selection: Option<usize>,
    side: Side,
    expanded: Option<(usize, Contents)>,
    pending: Option<Value>,
    pub(crate) status: String,
}

impl ReviewPane {
    pub(crate) fn load(client: &Client, thread_id: &str, message_id: &str) -> Result<Self> {
        let envelope: Value =
            client.get_json("GET", &format!("/api/inbox/threads/{}", segment(thread_id)))?;
        ensure!(
            envelope.get("serverSession").and_then(Value::as_str) == Some(client.server_session()),
            "Inbox server session changed; reopen Reviews"
        );
        let thread = envelope.get("thread").context("Inbox omitted review thread")?;
        let message = model::links(thread)?
            .into_iter()
            .find(|message| message.id == message_id)
            .context("this thread no longer contains the guided review")?;
        let answer: GuideAnswer =
            client.get_json("GET", &format!("/api/inbox/messages/{}/guide", segment(message_id)))?;
        answer.snapshot.check()?;
        let files = diff::parse(&answer.snapshot.review.raw_patch)?;
        let attachments: Attachments =
            client.get_json("GET", &format!("/api/inbox/threads/{}/attachments", segment(thread_id)))?;
        ensure!(
            attachments.server_session == client.server_session(),
            "Inbox server session changed; reopen Reviews"
        );
        let settings: Value = client.get_json("GET", "/api/inbox/settings")?;
        let code_review = settings.pointer("/code_review/on").and_then(Value::as_bool) == Some(true);
        let verdict = code_review
            && message.guide.review
            && !message.guide.walkthrough
            && thread.get("pull_theirs").and_then(Value::as_bool) != Some(true);
        let read_only = model::read_only(thread, &message);
        let records = attachments
            .annotations
            .into_iter()
            .filter(|record| record.message_id == message.id && record.guide.is_some())
            .collect();
        Ok(Self {
            thread_id: thread_id.into(),
            message,
            snapshot: answer.snapshot,
            files,
            records,
            read_only,
            code_review,
            verdict,
            view: View::Walkthrough,
            mode: Mode::Browse,
            compose: Compose::default(),
            note: String::new(),
            rows: Vec::new(),
            width: 0,
            cursor: 0,
            scroll: 0,
            height: 20,
            horizontal: 0,
            selected_comment: 0,
            selection: None,
            side: Side::New,
            expanded: None,
            pending: None,
            status: String::new(),
        })
    }

    pub(crate) fn paste(&mut self, text: &str) {
        if matches!(self.mode, Mode::Comment(_) | Mode::Suggestion(_) | Mode::Note(_) | Mode::Jump) {
            self.compose.insert_text(text);
        }
    }

    fn invalidate(&mut self) {
        self.width = 0;
    }

    fn writable(&self) -> Result<()> {
        ensure!(!self.read_only, "This archived review is read-only; unarchive the thread first");
        ensure!(
            self.pending.is_none(),
            "A send is unconfirmed; s retries the same request, D explicitly forgets it"
        );
        Ok(())
    }

    fn post<T: serde::de::DeserializeOwned>(client: &Client, path: &str, mut body: Value) -> Result<T> {
        body.as_object_mut()
            .context("Review write body must be an object")?
            .insert("serverSession".into(), Value::String(client.server_session().into()));
        client.post_json(path, &body)
    }

    fn guide_path(&self, suffix: &str) -> String {
        format!("/api/inbox/messages/{}/guide{suffix}", segment(&self.message.id))
    }

    fn refresh_comments(&mut self, client: &Client) -> Result<()> {
        let response: Attachments = client
            .get_json("GET", &format!("/api/inbox/threads/{}/attachments", segment(&self.thread_id)))?;
        ensure!(
            response.server_session == client.server_session(),
            "Inbox server session changed; reopen Reviews"
        );
        self.records = response
            .annotations
            .into_iter()
            .filter(|r| r.message_id == self.message.id && r.guide.is_some())
            .collect();
        self.selected_comment = self.selected_comment.min(self.records.len().saturating_sub(1));
        Ok(())
    }

    fn toggle_reviewed(&mut self, client: &Client) -> Result<()> {
        self.writable()?;
        ensure!(
            self.code_review && self.message.guide.review && !self.message.guide.walkthrough,
            "Reviewed ticks are available on code reviews, not walkthroughs or plain guides"
        );
        let chapter =
            self.rows.get(self.cursor).and_then(|row| row.chapter).context("Move into a chapter first")?;
        let mut reviewed = self.message.guide_reviewed.clone();
        reviewed.resize(self.snapshot.guide.sections.len(), false);
        if let Some(value) = reviewed.get_mut(chapter) {
            *value = !*value;
        }
        let response: Value =
            Self::post(client, &self.guide_path("/reviewed"), json!({"reviewed":reviewed}))?;
        self.message.guide_reviewed = serde_json::from_value(
            response.get("reviewed").cloned().context("Inbox omitted reviewed ticks")?,
        )?;
        self.invalidate();
        Ok(())
    }

    fn toggle_viewed(&mut self, client: &Client) -> Result<()> {
        self.writable()?;
        ensure!(
            self.code_review && self.message.guide.review && !self.message.guide.walkthrough,
            "Viewed ticks are available on code reviews, not walkthroughs or plain guides"
        );
        let file = self
            .rows
            .get(self.cursor)
            .and_then(|row| row.file)
            .and_then(|index| self.files.get(index))
            .context("Move into a file first")?;
        let mut viewed = self.message.guide_viewed.clone();
        if viewed.contains(&file.path) {
            viewed.retain(|path| path != &file.path);
        } else {
            viewed.push(file.path.clone());
        }
        let response: Value = Self::post(client, &self.guide_path("/viewed"), json!({"viewed":viewed}))?;
        self.message.guide_viewed =
            serde_json::from_value(response.get("viewed").cloned().context("Inbox omitted viewed files")?)?;
        self.invalidate();
        Ok(())
    }
}
