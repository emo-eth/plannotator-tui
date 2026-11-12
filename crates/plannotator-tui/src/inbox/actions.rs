//! Explicit Inbox commands, following the v0.28.61 window routes. Opening this module's
//! panel only reads. Every write uses Client's origin/session-checked transport; nothing
//! retries, rediscovers a server, silently skips questions, or changes a revision.
//!
//! Mark read (`read`) closes waiting questions; Move back (`unread`) undoes that mark.
//! Mark unread (`mark-unread`) only makes the latest agent message new again. Action
//! marks have no revision precondition upstream and wait for Send. Acknowledgements can
//! reach the agent after the server's Undo window. Reply keys belong to the caller and
//! must survive an ambiguous transport failure unchanged.

use std::fmt::Write as _;

use anyhow::{Context, Result, ensure};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::client::{Addressee, Client, Message, Pick, Row, Skip, Thread, path_id};
use super::machines::{HeldWrite, OutboxEntry, UndeliveredWrite};
use super::questions::{Kind, Question, State};
use crate::app::compose::{Compose, ComposeAction};
use crate::theme::palette;

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct Pin {
    pub(crate) thread_id: String,
    pub(crate) pinned_at: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct Snooze {
    pub(crate) until: String,
    pub(crate) at: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct MarkUnread {
    pub(crate) wrote: bool,
    pub(crate) through_seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Reaction {
    pub(crate) emoji: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) label: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Acknowledgement<'a> {
    Undo,
    LooksGood,
    Reaction(&'a Reaction),
}

#[derive(Debug, Deserialize)]
pub(crate) struct AckResult {
    pub(crate) message: Message,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ActionFacts {
    #[serde(rename = "where")]
    pub(crate) where_: Option<String>,
    pub(crate) where_url: Option<String>,
    pub(crate) run: Option<String>,
    pub(crate) watching: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ActionOutcome {
    Done,
    Cant,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PersonActionResult {
    pub(crate) outcome: ActionOutcome,
    pub(crate) note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ActionResult {
    pub(crate) outcome: ActionOutcome,
    pub(crate) note: Option<String>,
    pub(crate) by: String,
    #[serde(default)]
    pub(crate) staged: bool,
    pub(crate) person: Option<PersonActionResult>,
}

impl ActionResult {
    /// An agent may finish the task while the person's note still waits in their draft.
    pub(crate) fn in_draft(&self, state: State, revision: u64, sent_revision: u64) -> bool {
        if self.by == "person" {
            return self.staged && state == State::Picked;
        }
        self.by == "agent"
            && state == State::Closed
            && !(revision > 0 && sent_revision == revision)
            && self
                .person
                .as_ref()
                .and_then(|person| person.note.as_deref())
                .is_some_and(|note| !note.trim().is_empty())
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ActionMark<'a> {
    pub(crate) outcome: ActionOutcome,
    pub(crate) note: Option<&'a str>,
    /// A late note edit must not recreate a mark another window took back.
    pub(crate) note_only: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ActionRef {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) message_id: Option<String>,
    pub(crate) key: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReviewVerdict {
    Approved,
    ChangesRequested,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum TagMode {
    Ask,
    Add,
    Handoff,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ReplyTag {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) host: Option<String>,
    pub(crate) session: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mode: Option<TagMode>,
}

#[derive(Debug, Default)]
pub(crate) struct ReplyItems<'a> {
    pub(crate) questions: &'a [Pick],
    pub(crate) skip: &'a [Skip],
    pub(crate) actions: &'a [ActionRef],
    pub(crate) annotation_ids: &'a [String],
    pub(crate) uploads: &'a [String],
    pub(crate) feedback: Option<&'a str>,
    pub(crate) viewing: Option<&'a Value>,
    pub(crate) to: Option<&'a Addressee>,
    pub(crate) seen_through_seq: Option<u64>,
    pub(crate) clear_draft: Option<&'a str>,
    pub(crate) review_verdict: Option<ReviewVerdict>,
    pub(crate) tags: &'a [ReplyTag],
}

#[derive(Debug, Deserialize)]
pub(crate) struct ReplyResult {
    pub(crate) message_id: Option<String>,
    pub(crate) questions: Vec<Question>,
    pub(crate) reply: Option<Message>,
    pub(crate) replayed: bool,
    #[serde(default)]
    pub(crate) nothing_to_send: bool,
    #[serde(skip)]
    pub(crate) held: Option<OutboxEntry>,
}

#[derive(Debug, Deserialize)]
struct RowResponse {
    thread: Row,
}

#[derive(Debug, Deserialize)]
struct ResolveResponse {
    thread: Thread,
}

#[derive(Debug, Deserialize)]
struct PinsResponse {
    pins: Vec<Pin>,
}

#[derive(Debug, Deserialize)]
struct SnoozeResponse {
    snoozed: Option<Snooze>,
}

#[derive(Debug, Deserialize)]
pub(super) struct QuestionsResponse {
    pub(super) questions: Vec<Question>,
}

impl Client {
    /// Explicitly mark only the messages the person actually saw, not a later global cursor.
    pub(crate) fn mark_seen(&self, thread_id: &str, through_seq: u64) -> Result<Row> {
        safe_seq(through_seq)?;
        let path = format!("/api/inbox/threads/{}/seen", path_id(thread_id)?);
        let body = json!({"serverSession": self.server_session(), "through_seq": through_seq});
        Ok(self.post_json::<RowResponse>(&path, &body)?.thread)
    }

    /// `false` is Move back (undo Mark read), not Mark unread.
    pub(crate) fn mark_read(&self, thread_id: &str, read: bool) -> Result<Row> {
        let operation = if read { "read" } else { "unread" };
        let path = format!("/api/inbox/threads/{}/{operation}", path_id(thread_id)?);
        Ok(self.post_json::<RowResponse>(&path, &json!({"serverSession": self.server_session()}))?.thread)
    }

    pub(crate) fn mark_unread(&self, thread_id: &str) -> Result<MarkUnread> {
        let path = format!("/api/inbox/threads/{}/mark-unread", path_id(thread_id)?);
        self.post_json(&path, &json!({"serverSession": self.server_session()}))
    }

    /// Archive is a message route but resolves the whole thread; unarchive uses false.
    pub(crate) fn resolve(&self, message_id: &str, resolved: bool) -> Result<Thread> {
        let path = format!("/api/inbox/messages/{}/resolve", path_id(message_id)?);
        let body = json!({"serverSession": self.server_session(), "resolved": resolved});
        Ok(self.post_json::<ResolveResponse>(&path, &body)?.thread)
    }

    /// `at` restores an earlier snooze's own timestamp on Undo. The server owns time validation.
    pub(crate) fn snooze(
        &self,
        thread_id: &str,
        until: Option<&str>,
        at: Option<&str>,
    ) -> Result<Option<Snooze>> {
        let path = format!("/api/inbox/threads/{}/snooze", path_id(thread_id)?);
        let mut body = Map::from_iter([
            ("serverSession".to_owned(), json!(self.server_session())),
            ("until".to_owned(), json!(until)),
        ]);
        if let Some(at) = at {
            body.insert("at".to_owned(), json!(at));
        }
        Ok(self.post_json::<SnoozeResponse>(&path, &Value::Object(body))?.snoozed)
    }

    pub(crate) fn pin(&self, thread_id: &str, pinned: bool) -> Result<Vec<Pin>> {
        let path = format!("/api/inbox/threads/{}/pin", path_id(thread_id)?);
        let body = json!({"serverSession": self.server_session(), "pinned": pinned});
        Ok(self.post_json::<PinsResponse>(&path, &body)?.pins)
    }

    pub(crate) fn pin_order(&self, order: &[String]) -> Result<Vec<Pin>> {
        for id in order {
            path_id(id)?;
        }
        let body = json!({"serverSession": self.server_session(), "order": order});
        Ok(self.post_json::<PinsResponse>("/api/inbox/pins", &body)?.pins)
    }

    pub(crate) fn pin_restore(&self, pin: &Pin, index: usize) -> Result<Vec<Pin>> {
        path_id(&pin.thread_id)?;
        let body = json!({"serverSession": self.server_session(), "restore": {
            "thread_id": pin.thread_id, "index": index, "pinned_at": pin.pinned_at,
        }});
        Ok(self.post_json::<PinsResponse>("/api/inbox/pins", &body)?.pins)
    }

    pub(crate) fn acknowledge(&self, message_id: &str, ack: Acknowledgement<'_>) -> Result<Message> {
        let path = format!("/api/inbox/messages/{}/ack", path_id(message_id)?);
        Ok(self.post_json::<AckResult>(&path, &ack_body(self.server_session(), ack))?.message)
    }

    /// Returns all questions, including `action_result/revision`, just like the upstream route.
    /// No revision is sent: the server does not implement compare-and-set for action marks.
    pub(crate) fn mark_action(
        &self,
        message_id: &str,
        key: &str,
        mark: Option<ActionMark<'_>>,
    ) -> Result<Vec<Question>> {
        let path = format!("/api/inbox/messages/{}/actions/{}", path_id(message_id)?, action_key(key)?);
        let body = action_body(self.server_session(), mark)?;
        Ok(self.post_json::<QuestionsResponse>(&path, &body)?.questions)
    }

    /// A Send is exactly the supplied items. Skips and action references have no revision
    /// upstream; questions retain their observed revisions and are atomically checked there.
    pub(crate) fn reply_items(
        &self,
        message_id: &str,
        idempotency_key: &str,
        words: Option<&str>,
        items: &ReplyItems<'_>,
    ) -> Result<ReplyResult> {
        let path = format!("/api/inbox/messages/{}/reply", path_id(message_id)?);
        let body = reply_body(self.server_session(), idempotency_key, words, items)?;
        match self.post_json(&path, &body) {
            Ok(value) => reply_result(value),
            Err(error) => {
                if let Some(held) = error.downcast_ref::<HeldWrite>() {
                    return Ok(held_result(held.entry.clone()));
                }
                if let Some(undelivered) = error.downcast_ref::<UndeliveredWrite>() {
                    return Ok(held_result(undelivered.entry.clone()));
                }
                if let Some(http) = error.downcast_ref::<super::client::HttpError>()
                    && matches!(http.status, 202 | 409)
                    && http.payload.get("outbox").is_some_and(Value::is_object)
                {
                    return reply_result(http.payload.clone());
                }
                Err(error)
            }
        }
    }

    /// Explicit skip-only Send; it never discovers or adds other open questions.
    pub(crate) fn skip_questions(
        &self,
        message_id: &str,
        idempotency_key: &str,
        skip: &[Skip],
    ) -> Result<ReplyResult> {
        ensure!(!skip.is_empty(), "choose at least one question to skip");
        self.reply_items(message_id, idempotency_key, None, &ReplyItems { skip, ..ReplyItems::default() })
    }
}

fn safe_seq(value: u64) -> Result<()> {
    ensure!(value <= MAX_SAFE_INTEGER, "Inbox sequence exceeds JavaScript's safe integer range");
    Ok(())
}

fn action_key(key: &str) -> Result<&str> {
    let suffix = key.strip_prefix("a-").context("invalid Inbox action key")?;
    let (hash, repeat) = suffix.split_once('-').map_or((suffix, None), |(hash, repeat)| (hash, Some(repeat)));
    ensure!(
        hash.len() == 8 && hash.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "invalid Inbox action key {key:?}"
    );
    ensure!(
        repeat.is_none_or(|repeat| !repeat.is_empty()
            && repeat.len() <= 3
            && repeat.bytes().all(|byte| byte.is_ascii_digit())),
        "invalid Inbox action key {key:?}"
    );
    Ok(key)
}

fn ack_body(session: &str, ack: Acknowledgement<'_>) -> Value {
    let mut body = Map::from_iter([
        ("serverSession".to_owned(), json!(session)),
        ("ack".to_owned(), json!(!matches!(ack, Acknowledgement::Undo))),
    ]);
    match ack {
        Acknowledgement::LooksGood => {
            body.insert("reaction".to_owned(), Value::Null);
        }
        Acknowledgement::Reaction(reaction) => {
            body.insert("reaction".to_owned(), json!(reaction));
        }
        Acknowledgement::Undo => {}
    }
    Value::Object(body)
}

fn action_body(session: &str, mark: Option<ActionMark<'_>>) -> Result<Value> {
    let mut body =
        Map::from_iter([("serverSession".to_owned(), json!(session)), ("outcome".to_owned(), Value::Null)]);
    if let Some(mark) = mark {
        let note = mark.note.map(str::trim).filter(|note| !note.is_empty());
        ensure!(
            note.is_none_or(|note| note.encode_utf16().count() <= 2000),
            "action note: at most 2000 UTF-16 units"
        );
        ensure!(
            mark.outcome != ActionOutcome::Cant || note.is_some(),
            "say why you cannot do this action; the agent receives the reason"
        );
        body.insert("outcome".to_owned(), json!(mark.outcome));
        if let Some(note) = note {
            body.insert("note".to_owned(), json!(note));
        }
        if mark.note_only {
            body.insert("note_only".to_owned(), json!(true));
        }
    }
    body.insert("staged".to_owned(), json!(true));
    Ok(Value::Object(body))
}

fn reply_body(session: &str, key: &str, words: Option<&str>, items: &ReplyItems<'_>) -> Result<Value> {
    ensure!(!key.trim().is_empty(), "idempotency_key is required on a Send");
    for question in items.questions {
        safe_seq(question.revision)?;
        if let Some(id) = &question.message_id {
            path_id(id)?;
        }
    }
    for skip in items.skip {
        ensure!(!skip.key.starts_with("a-"), "action items cannot be skipped as questions");
        if let Some(id) = &skip.message_id {
            path_id(id)?;
        }
    }
    for action in items.actions {
        action_key(&action.key)?;
        if let Some(id) = &action.message_id {
            path_id(id)?;
        }
    }
    let mut body = Map::from_iter([
        ("serverSession".to_owned(), json!(session)),
        ("idempotency_key".to_owned(), json!(key)),
        ("questions".to_owned(), json!(items.questions)),
        ("skip".to_owned(), json!(items.skip)),
        ("actions".to_owned(), json!(items.actions)),
    ]);
    if let Some(words) = words {
        body.insert("words".to_owned(), json!(words));
    }
    if !items.annotation_ids.is_empty() {
        body.insert("annotation_ids".to_owned(), json!(items.annotation_ids));
    }
    if !items.uploads.is_empty() {
        body.insert("uploads".to_owned(), json!(items.uploads));
    }
    if let Some(feedback) = items.feedback {
        body.insert("feedback".to_owned(), json!(feedback));
    }
    if let Some(viewing) = items.viewing {
        body.insert("viewing".to_owned(), viewing.clone());
    }
    if let Some(to) = items.to {
        let mut target = Map::from_iter([("session".to_owned(), json!(to.session))]);
        if let Some(host) = &to.host {
            target.insert("host".to_owned(), json!(host));
        }
        body.insert("to".to_owned(), Value::Object(target));
    }
    if let Some(seq) = items.seen_through_seq {
        safe_seq(seq)?;
        body.insert("seen_through_seq".to_owned(), json!(seq));
    }
    if let Some(draft) = items.clear_draft {
        body.insert("clear_draft".to_owned(), json!(draft));
    }
    if let Some(verdict) = items.review_verdict {
        body.insert("review_verdict".to_owned(), json!(verdict));
    }
    if !items.tags.is_empty() {
        body.insert("tags".to_owned(), json!(items.tags));
    }
    Ok(Value::Object(body))
}

fn reply_result(mut value: Value) -> Result<ReplyResult> {
    if let Some(outbox) = value.get_mut("outbox") {
        let held: OutboxEntry = serde_json::from_value(outbox.take()).context("decoding held Inbox reply")?;
        if held.state == "delivered" {
            return serde_json::from_value(held.answer).context("decoding delivered Inbox outbox answer");
        }
        return Ok(held_result(held));
    }
    serde_json::from_value(value).context("decoding Inbox reply")
}

fn held_result(held: OutboxEntry) -> ReplyResult {
    ReplyResult {
        message_id: None,
        questions: Vec::new(),
        reply: None,
        replayed: false,
        nothing_to_send: false,
        held: Some(held),
    }
}

#[derive(Debug, Deserialize)]
struct ActionSnapshot {
    #[serde(rename = "serverSession")]
    server_session: String,
    thread: Thread,
    row: Option<ActionRow>,
}

#[derive(Debug, Deserialize)]
struct ActionRow {
    snoozed: Option<Snooze>,
}

#[derive(Debug, Deserialize)]
struct MarkReadSettings {
    mark_read_warning: Option<MarkReadWarning>,
}

#[derive(Debug, Deserialize)]
struct MarkReadWarning {
    on: bool,
}

#[derive(Debug, Deserialize)]
struct PinsSnapshot {
    #[serde(rename = "serverSession")]
    server_session: String,
    pins: Option<Vec<Pin>>,
}

#[derive(Debug, Clone, Copy)]
enum Command {
    Seen,
    Read(bool),
    Unread,
    Resolve(bool),
    Snooze,
    Unsnooze,
    UndoSnooze,
    Pin(bool),
    PinMove(bool),
    UndoPin,
    Ack(usize, bool),
    React(usize),
    Action(usize, usize, Option<ActionOutcome>, bool),
    Skip(usize, usize),
    ReadWarning,
}

struct PendingSkip {
    message_id: String,
    key: String,
    skip: Vec<Skip>,
}

enum SnoozeUndo {
    Clear,
    Restore(Snooze),
}

struct ActionPanel {
    snapshot: ActionSnapshot,
    pins: Option<Vec<Pin>>,
    pin_undo: Option<(Pin, usize)>,
    snooze_undo: Option<SnoozeUndo>,
    pending: Option<PendingSkip>,
    status: String,
}

impl ActionPanel {
    fn load(client: &Client, thread_id: &str) -> Result<Self> {
        let snapshot = load_snapshot(client, thread_id)?;
        let pins = load_pins(client)?;
        Ok(Self {
            snapshot,
            pins,
            pin_undo: None,
            snooze_undo: None,
            pending: None,
            status: "Choose a command; Enter confirms before any write. Esc returns.".to_owned(),
        })
    }

    fn entries(&self) -> Vec<(String, Command)> {
        let thread = &self.snapshot.thread;
        let mut entries = vec![
            (mark_read_label(thread), Command::Read(true)),
            ("Move back — undo Mark read, reopen its questions".to_owned(), Command::Read(false)),
            ("Mark unread — show the newest agent message as new".to_owned(), Command::Unread),
            ("Mark seen through this thread snapshot".to_owned(), Command::Seen),
            (
                if thread.resolved_at.is_some() { "Unarchive thread" } else { "Archive thread" }.to_owned(),
                Command::Resolve(thread.resolved_at.is_none()),
            ),
            ("Snooze until a date and time…".to_owned(), Command::Snooze),
            ("Unsnooze".to_owned(), Command::Unsnooze),
            ("Toggle mark-read warning preference (newer Inbox)".to_owned(), Command::ReadWarning),
        ];
        if self.snooze_undo.is_some() {
            entries.push(("Undo last snooze change".to_owned(), Command::UndoSnooze));
        }
        if let Some(pins) = &self.pins {
            let pinned = pins.iter().any(|pin| pin.thread_id == thread.thread_id);
            entries.push((
                if pinned { "Unstar thread" } else { "Star thread" }.to_owned(),
                Command::Pin(!pinned),
            ));
            if pinned {
                entries.push(("Move star up".to_owned(), Command::PinMove(true)));
                entries.push(("Move star down".to_owned(), Command::PinMove(false)));
            }
        }
        if self.pin_undo.is_some() {
            entries.push(("Undo Unstar at its original position".to_owned(), Command::UndoPin));
        }
        if let Some((index, message)) =
            thread.messages.iter().enumerate().rev().find(|(_, message)| message.author.kind == "agent")
        {
            entries.push((
                format!("Looks good on {} — sends after Undo window", message.id),
                Command::Ack(index, true),
            ));
            entries
                .push((format!("React to {}… — sends after Undo window", message.id), Command::React(index)));
            entries.push((format!("Undo acknowledgement on {}", message.id), Command::Ack(index, false)));
        }
        for (mi, message) in
            thread.messages.iter().enumerate().filter(|(_, message)| message.author.kind == "agent")
        {
            for (qi, question) in message.questions.iter().enumerate() {
                if question.kind == Kind::Action {
                    let draft = question.action_result.as_ref().is_some_and(|result| {
                        result.in_draft(question.state, question.revision, question.sent_revision)
                    });
                    if question.is_open() {
                        entries.push((
                            format!("Done (stage for Send): {}", question.prompt),
                            Command::Action(mi, qi, Some(ActionOutcome::Done), false),
                        ));
                        entries.push((
                            format!("Can't do it (stage reason): {}", question.prompt),
                            Command::Action(mi, qi, Some(ActionOutcome::Cant), false),
                        ));
                    }
                    if draft {
                        let result = question.action_result.as_ref();
                        let outcome = result.map(|result| {
                            result.person.as_ref().map_or(result.outcome, |person| person.outcome)
                        });
                        entries.push((
                            format!("Edit staged note: {}", question.prompt),
                            Command::Action(mi, qi, outcome, true),
                        ));
                    }
                    if question.state == State::Picked || draft {
                        entries.push((
                            format!("Undo action mark: {}", question.prompt),
                            Command::Action(mi, qi, None, false),
                        ));
                    }
                } else if question.state == State::Open {
                    entries.push((
                        format!("Skip and send only this question: {}", question.prompt),
                        Command::Skip(mi, qi),
                    ));
                }
            }
        }
        entries
    }

    fn refresh(&mut self, client: &Client) -> Result<()> {
        self.snapshot = load_snapshot(client, &self.snapshot.thread.thread_id)?;
        self.pins = load_pins(client)?;
        Ok(())
    }

    fn send_pending(&mut self, client: &Client) -> Result<()> {
        let pending = self.pending.as_ref().context("no unconfirmed Skip")?;
        match client.skip_questions(&pending.message_id, &pending.key, &pending.skip) {
            Ok(result) => {
                self.status = if let Some(held) = result.held {
                    format!("Skip is in outbox ({}, key {}), not confirmed delivered.", held.state, held.key)
                } else if result.nothing_to_send {
                    "Question closed meanwhile; nothing was sent.".to_owned()
                } else {
                    "Skip saved in a reply; this is not a claim of agent delivery.".to_owned()
                };
                self.pending = None;
                Ok(())
            }
            Err(error) => {
                if error
                    .downcast_ref::<super::client::HttpError>()
                    .is_some_and(|error| (400..500).contains(&error.status))
                {
                    self.pending = None;
                }
                Err(error)
            }
        }
    }

    fn react(
        &mut self,
        client: &Client,
        terminal: &mut ratatui::DefaultTerminal,
        index: usize,
    ) -> Result<bool> {
        let message = self.snapshot.thread.messages.get_mut(index).context("message disappeared")?;
        let Some(emoji) = prompt(terminal, "One reaction emoji (server validates it)", "")? else {
            return Ok(false);
        };
        let Some(label) = prompt(terminal, "Optional reaction name; blank means no name", "")? else {
            return Ok(false);
        };
        *message = client.acknowledge(
            &message.id,
            Acknowledgement::Reaction(&Reaction { emoji, label: (!label.is_empty()).then_some(label) }),
        )?;
        Ok(true)
    }

    fn stage_action(
        &self,
        client: &Client,
        terminal: &mut ratatui::DefaultTerminal,
        mi: usize,
        qi: usize,
        outcome: Option<ActionOutcome>,
        note_only: bool,
    ) -> Result<bool> {
        let message = self.snapshot.thread.messages.get(mi).context("message disappeared")?;
        let question = message.questions.get(qi).context("action disappeared")?;
        let note = if outcome.is_some() {
            let previous = question
                .action_result
                .as_ref()
                .and_then(|result| {
                    result.person.as_ref().map_or(result.note.as_deref(), |person| person.note.as_deref())
                })
                .unwrap_or("");
            let title = if outcome == Some(ActionOutcome::Cant) {
                "Why can't you do it? (required; staged for Send)"
            } else {
                "Optional note (staged for Send)"
            };
            let Some(note) = prompt(terminal, title, previous)? else {
                return Ok(false);
            };
            Some(note)
        } else {
            None
        };
        let mark = outcome.map(|outcome| ActionMark { outcome, note: note.as_deref(), note_only });
        client.mark_action(&message.id, &question.key, mark)?;
        Ok(true)
    }

    fn execute(
        &mut self,
        client: &Client,
        terminal: &mut ratatui::DefaultTerminal,
        command: Command,
    ) -> Result<()> {
        let thread = &self.snapshot.thread;
        match command {
            Command::Seen => {
                let through = thread
                    .messages
                    .iter()
                    .filter(|message| message.author.kind == "agent")
                    .filter_map(|message| message.seq)
                    .max()
                    .context("server provided no message sequences; cannot safely mark this snapshot seen")?;
                client.mark_seen(&thread.thread_id, through)?;
            }
            Command::Read(read) => {
                if read && !confirm_mark_read(client, terminal, thread)? {
                    return Ok(());
                }
                client.mark_read(&thread.thread_id, read)?;
            }
            Command::ReadWarning => {
                let settings: MarkReadSettings = client.get_json("GET", "/api/inbox/settings?store=0")?;
                let current = settings
                    .mark_read_warning
                    .context("this Inbox does not offer the mark-read warning preference")?;
                set_read_warning(client, !current.on)?;
            }
            Command::Unread => {
                let result = client.mark_unread(&thread.thread_id)?;
                self.status = if result.wrote {
                    format!("Marked unread through message sequence {}.", result.through_seq)
                } else {
                    "No unread marker changed.".to_owned()
                };
                return Ok(());
            }
            Command::Resolve(resolved) => {
                client.resolve(&thread.thread_id, resolved)?;
            }
            Command::Snooze => {
                let Some(until) = prompt(
                    terminal,
                    "Snooze until (RFC 3339, including timezone, e.g. 2026-11-12T09:00:00-08:00)",
                    "",
                )?
                else {
                    return Ok(());
                };
                let old = self.snapshot.row.as_ref().and_then(|row| row.snoozed.clone());
                client.snooze(&thread.thread_id, Some(until.trim()), None)?;
                self.snooze_undo = Some(old.map_or(SnoozeUndo::Clear, SnoozeUndo::Restore));
            }
            Command::Unsnooze => {
                let old = self.snapshot.row.as_ref().and_then(|row| row.snoozed.clone());
                client.snooze(&thread.thread_id, None, None)?;
                self.snooze_undo = Some(old.map_or(SnoozeUndo::Clear, SnoozeUndo::Restore));
            }
            Command::UndoSnooze => {
                let previous = self.snooze_undo.as_ref().context("no snooze change to undo")?;
                let (until, at) = match previous {
                    SnoozeUndo::Clear => (None, None),
                    SnoozeUndo::Restore(snooze) => (Some(snooze.until.as_str()), Some(snooze.at.as_str())),
                };
                client.snooze(&thread.thread_id, until, at)?;
                self.snooze_undo = None;
            }
            Command::Pin(pinned) => {
                let old = self
                    .pins
                    .as_ref()
                    .and_then(|pins| {
                        pins.iter().enumerate().find(|(_, pin)| pin.thread_id == thread.thread_id)
                    })
                    .map(|(index, pin)| (pin.clone(), index));
                self.pins = Some(client.pin(&thread.thread_id, pinned)?);
                if !pinned {
                    self.pin_undo = old;
                }
            }
            Command::PinMove(up) => {
                let pins = self.pins.as_ref().context("server does not support stars")?;
                let order = moved_pins(pins, &thread.thread_id, up)?;
                self.pins = Some(client.pin_order(&order)?);
            }
            Command::UndoPin => {
                let (pin, index) = self.pin_undo.as_ref().context("no Unstar to undo")?;
                self.pins = Some(client.pin_restore(pin, *index)?);
                self.pin_undo = None;
            }
            Command::Ack(index, on) => {
                let message = self.snapshot.thread.messages.get_mut(index).context("message disappeared")?;
                *message = client.acknowledge(
                    &message.id,
                    if on { Acknowledgement::LooksGood } else { Acknowledgement::Undo },
                )?;
            }
            Command::React(index) => {
                if !self.react(client, terminal, index)? {
                    return Ok(());
                }
            }
            Command::Action(mi, qi, outcome, note_only) => {
                if !self.stage_action(client, terminal, mi, qi, outcome, note_only)? {
                    return Ok(());
                }
            }
            Command::Skip(mi, qi) => {
                let message = thread.messages.get(mi).context("message disappeared")?;
                let question = message.questions.get(qi).context("question disappeared")?;
                self.pending = Some(PendingSkip {
                    message_id: message.id.clone(),
                    key: super::send::new_key(),
                    skip: vec![Skip { message_id: None, key: question.key.clone() }],
                });
                return self.send_pending(client);
            }
        }
        "Saved. Staged action marks wait for Send; acknowledgements use the server's Undo window."
            .clone_into(&mut self.status);
        Ok(())
    }
}

fn load_snapshot(client: &Client, thread_id: &str) -> Result<ActionSnapshot> {
    let path = format!("/api/inbox/threads/{}", path_id(thread_id)?);
    let snapshot: ActionSnapshot = client.get_json("GET", &path)?;
    ensure!(
        snapshot.server_session == client.server_session(),
        "Inbox serverSession changed; rediscover before continuing"
    );
    Ok(snapshot)
}

fn load_pins(client: &Client) -> Result<Option<Vec<Pin>>> {
    let snapshot: PinsSnapshot = client.get_json("GET", "/api/inbox/threads")?;
    ensure!(
        snapshot.server_session == client.server_session(),
        "Inbox serverSession changed; rediscover before continuing"
    );
    Ok(snapshot.pins)
}

fn moved_pins(pins: &[Pin], thread_id: &str, up: bool) -> Result<Vec<String>> {
    let index =
        pins.iter().position(|pin| pin.thread_id == thread_id).context("thread is no longer starred")?;
    let other =
        if up { index.checked_sub(1) } else { index.checked_add(1).filter(|next| *next < pins.len()) }
            .context("star is already at this end")?;
    let mut order: Vec<String> = pins.iter().map(|pin| pin.thread_id.clone()).collect();
    order.swap(index, other);
    Ok(order)
}

/// A modal list of explicit commands. Opening, refreshing, drawing and cancelling are GET-only.
pub(crate) fn run(client: &Client, terminal: &mut ratatui::DefaultTerminal, thread_id: &str) -> Result<()> {
    let mut panel = ActionPanel::load(client, thread_id)?;
    let mut selection = ListState::default().with_selected(Some(0));
    loop {
        let entries = panel.entries();
        let selected = selection.selected().unwrap_or(0).min(entries.len().saturating_sub(1));
        selection.select(Some(selected));
        terminal.draw(|frame| {
            let [body, status, help] =
                Layout::vertical([Constraint::Min(1), Constraint::Length(3), Constraint::Length(2)])
                    .areas(frame.area());
            let items = entries.iter().map(|(label, _)| ListItem::new(label.as_str()));
            let title = panel
                .snapshot
                .thread
                .thread_name
                .as_deref()
                .or(panel.snapshot.thread.subject.as_deref())
                .unwrap_or(thread_id);
            frame.render_stateful_widget(
                List::new(items)
                    .block(Block::default().borders(Borders::ALL).title(format!(" Thread actions: {title} ")))
                    .highlight_style(Style::default().fg(palette().accent).add_modifier(Modifier::BOLD))
                    .highlight_symbol("> "),
                body,
                &mut selection,
            );
            frame.render_widget(Paragraph::new(panel.status.as_str()).wrap(Wrap { trim: false }), status);
            let keys = if panel.pending.is_some() {
                "Unconfirmed Skip: r retry same key · D discard retry · other writes and exit blocked"
            } else {
                "j/k or arrows · Enter choose/confirm · r refresh · Esc return"
            };
            frame.render_widget(Paragraph::new(keys).wrap(Wrap { trim: false }), help);
        })?;
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if panel.pending.is_some() {
            match key.code {
                KeyCode::Char('r') => {
                    if let Err(error) = panel.send_pending(client) {
                        panel.status = format!("Skip unconfirmed: {error:#}");
                    }
                    if let Err(error) = panel.refresh(client) {
                        let _ = write!(panel.status, "; refresh failed: {error:#}");
                    }
                }
                KeyCode::Char('D')
                    if confirm(
                        terminal,
                        "Discard retry tracking? The earlier Skip may already have been saved.",
                    )? =>
                {
                    panel.pending = None;
                    "Retry discarded; the server may still have saved the Skip."
                        .clone_into(&mut panel.status);
                }
                _ => {}
            }
            continue;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selection.select(Some(selected.saturating_sub(1))),
            KeyCode::Down | KeyCode::Char('j') => {
                selection.select(Some((selected + 1).min(entries.len().saturating_sub(1))));
            }
            KeyCode::Char('r') => {
                panel.status = match panel.refresh(client) {
                    Ok(()) => "Refreshed without writing.".to_owned(),
                    Err(error) => format!("Refresh failed: {error:#}"),
                };
            }
            KeyCode::Enter => {
                if let Some((label, command)) = entries.get(selected)
                    && confirm(terminal, label)?
                {
                    if let Err(error) = panel.execute(client, terminal, *command) {
                        panel.status = format!("Not confirmed: {error:#}");
                    }
                    if let Err(error) = panel.refresh(client) {
                        let _ = write!(panel.status, "; refresh failed: {error:#}");
                    }
                }
            }
            _ => {}
        }
    }
}

fn confirm(terminal: &mut ratatui::DefaultTerminal, label: &str) -> Result<bool> {
    loop {
        terminal.draw(|frame| {
            frame.render_widget(
                Paragraph::new(format!("{label}\n\nThis writes to the Inbox. y confirms; n or Esc cancels."))
                    .wrap(Wrap { trim: false })
                    .block(Block::default().borders(Borders::ALL).title(" Confirm action ")),
                frame.area(),
            );
        })?;
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Char('y') => return Ok(true),
                KeyCode::Char('n') | KeyCode::Esc => return Ok(false),
                _ => {}
            }
        }
    }
}

fn prompt(terminal: &mut ratatui::DefaultTerminal, title: &str, initial: &str) -> Result<Option<String>> {
    let mut compose = Compose::with_text(initial);
    loop {
        terminal.draw(|frame| {
            let [input, help] =
                Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).areas(frame.area());
            let (lines, row, col) = compose.wrapped(usize::from(input.width.saturating_sub(2).max(1)));
            frame.render_widget(
                Paragraph::new(lines.into_iter().map(Line::from).collect::<Vec<_>>())
                    .block(Block::default().borders(Borders::ALL).title(title)),
                input,
            );
            if row < usize::from(input.height.saturating_sub(2)) {
                frame.set_cursor_position((input.x + 1 + col as u16, input.y + 1 + row as u16));
            }
            frame.render_widget(
                Paragraph::new("Enter submits this value · Esc cancels without writing"),
                help,
            );
        })?;
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match compose.handle_key(key) {
                ComposeAction::Save => return Ok(Some(compose.value())),
                ComposeAction::Cancel => return Ok(None),
                ComposeAction::Edited => {}
            },
            Event::Paste(text) => compose.insert_text(&text),
            _ => {}
        }
    }
}

fn set_aside_counts(thread: &Thread) -> (usize, usize) {
    thread.messages.iter().flat_map(|message| &message.questions).fold(
        (0, 0),
        |(questions, actions), question| {
            if question.kind == Kind::Action {
                (questions, actions + usize::from(question.state == State::Open))
            } else {
                (questions + usize::from(question.is_open()), actions)
            }
        },
    )
}

fn mark_read_label(thread: &Thread) -> String {
    let (questions, actions) = set_aside_counts(thread);
    format!(
        "Mark read — close {questions} questions and {actions} action items unanswered; tell agents not to wait"
    )
}

fn set_read_warning(client: &Client, on: bool) -> Result<()> {
    let result: MarkReadSettings = client.post_json(
        "/api/inbox/settings",
        &json!({"serverSession": client.server_session(), "mark_read_warning": {"on": on}}),
    )?;
    ensure!(
        result.mark_read_warning.is_some_and(|setting| setting.on == on),
        "Inbox did not confirm the mark-read warning preference"
    );
    Ok(())
}

fn confirm_mark_read(
    client: &Client,
    terminal: &mut ratatui::DefaultTerminal,
    thread: &Thread,
) -> Result<bool> {
    let (questions, actions) = set_aside_counts(thread);
    if questions + actions == 0 {
        return Ok(true);
    }
    let settings: MarkReadSettings = client.get_json("GET", "/api/inbox/settings?store=0")?;
    let Some(setting) = settings.mark_read_warning else {
        return Ok(true);
    };
    if !setting.on {
        return Ok(true);
    }
    loop {
        terminal.draw(|frame| frame.render_widget(Paragraph::new(format!("Set aside {questions} questions and {actions} action items?\n\nMark read closes them unanswered and tells the agents not to wait on them.\n\ny Mark read\nn or Esc Cancel\nd Mark read and don't show this warning again (saved in Inbox settings)" )).wrap(Wrap { trim: false }).block(Block::default().borders(Borders::ALL).title(" Mark read warning ")), frame.area()))?;
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Char('y') => return Ok(true),
                KeyCode::Char('d') => {
                    set_read_warning(client, false)?;
                    return Ok(true);
                }
                KeyCode::Char('n') | KeyCode::Esc => return Ok(false),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing, reason = "tests assert by panicking")]
mod tests {
    use super::*;

    #[test]
    fn reactions_clear_replace_and_undo_distinctly() {
        assert_eq!(
            ack_body("run", Acknowledgement::LooksGood),
            json!({"serverSession":"run","ack":true,"reaction":null})
        );
        assert_eq!(ack_body("run", Acknowledgement::Undo), json!({"serverSession":"run","ack":false}));
        let reaction = Reaction { emoji: "\u{1f680}".to_owned(), label: Some("Ship it".to_owned()) };
        assert_eq!(
            ack_body("run", Acknowledgement::Reaction(&reaction)),
            json!({"serverSession":"run","ack":true,"reaction":{"emoji":"\u{1f680}","label":"Ship it"}})
        );
    }

    #[test]
    fn action_mark_respects_staging_note_races_and_utf16_limit() {
        let mark =
            ActionMark { outcome: ActionOutcome::Cant, note: Some("  Need access  "), note_only: true };
        assert_eq!(
            action_body("run", Some(mark)).expect("valid reason"),
            json!({"serverSession":"run","outcome":"cant","note":"Need access","note_only":true,"staged":true})
        );
        assert_eq!(
            action_body("run", None).expect("undo"),
            json!({"serverSession":"run","outcome":null,"staged":true})
        );
        let done = ActionMark { outcome: ActionOutcome::Done, note: None, note_only: false };
        assert_eq!(
            action_body("run", Some(done)).expect("staged mark"),
            json!({"serverSession":"run","outcome":"done","staged":true})
        );
        assert!(action_body("run", Some(ActionMark { note: Some(" \n "), ..mark })).is_err());
        let boundary = "\u{1f680}".repeat(1000);
        let over = "\u{1f680}".repeat(1001);
        assert!(action_body("run", Some(ActionMark { note: Some(&boundary), ..done })).is_ok());
        assert!(action_body("run", Some(ActionMark { note: Some(&over), ..done })).is_err());
    }

    #[test]
    fn action_keys_match_route_without_allowing_path_injection() {
        for key in ["a-0123abcd", "a-0123abcd-2", "a-0123abcd-999"] {
            assert!(action_key(key).is_ok(), "{key}");
        }
        for key in [
            "q-0123abcd",
            "a-ABCDEF12",
            "a-0123abcd-1000",
            "a-0123abcd-",
            "a-0123abcd/resolve",
            "a-0123abcd%2fack",
            "a-0123abcd?staged=false",
        ] {
            assert!(action_key(key).is_err(), "{key}");
        }
        assert!(safe_seq(MAX_SAFE_INTEGER).is_ok());
        assert!(safe_seq(MAX_SAFE_INTEGER + 1).is_err());
    }

    #[test]
    fn reply_preserves_key_revisions_explicit_targets_and_draft_metadata() {
        let questions = [Pick {
            message_id: Some("msg_earlier".to_owned()),
            key: "q-question".to_owned(),
            revision: 7,
            answer: None,
        }];
        let skips = [Skip { message_id: Some("msg_earlier".to_owned()), key: "q-other".to_owned() }];
        let actions = [ActionRef { message_id: None, key: "a-0123abcd".to_owned() }];
        let annotations = ["ann_saved".to_owned()];
        let uploads = ["upl_saved".to_owned()];
        let target = Addressee { host: Some("pi".to_owned()), session: "session-b".to_owned() };
        let tags = [ReplyTag {
            host: None,
            session: "session-c".to_owned(),
            label: Some("Reviewer".to_owned()),
            mode: Some(TagMode::Add),
        }];
        let viewing = json!({"message_id":"msg_earlier","attachment_id":"att_file"});
        let items = ReplyItems {
            questions: &questions,
            skip: &skips,
            actions: &actions,
            annotation_ids: &annotations,
            uploads: &uploads,
            feedback: Some("Review notes"),
            viewing: Some(&viewing),
            to: Some(&target),
            seen_through_seq: Some(19),
            clear_draft: Some("2026-11-11T10:00:00Z"),
            review_verdict: Some(ReviewVerdict::ChangesRequested),
            tags: &tags,
        };
        let first =
            reply_body("run", "stable-attempt-key", Some("Keep these words"), &items).expect("payload");
        let retry = reply_body("run", "stable-attempt-key", Some("Keep these words"), &items).expect("retry");
        assert_eq!(first, retry);
        assert_eq!(
            first,
            json!({"serverSession":"run","idempotency_key":"stable-attempt-key","words":"Keep these words","questions":[{"message_id":"msg_earlier","key":"q-question","revision":7}],"skip":[{"message_id":"msg_earlier","key":"q-other"}],"actions":[{"key":"a-0123abcd"}],"annotation_ids":["ann_saved"],"uploads":["upl_saved"],"feedback":"Review notes","viewing":viewing,"to":{"host":"pi","session":"session-b"},"seen_through_seq":19,"clear_draft":"2026-11-11T10:00:00Z","review_verdict":"changes_requested","tags":[{"session":"session-c","label":"Reviewer","mode":"add"}]})
        );
        assert!(first.get("skip_unanswered").is_none());
    }

    #[test]
    fn skips_cannot_silently_include_actions_and_blank_keys_never_send() {
        assert!(reply_body("run", "  ", Some("hello"), &ReplyItems::default()).is_err());
        let skip = [Skip { message_id: None, key: "a-0123abcd".to_owned() }];
        assert!(
            reply_body("run", "key", None, &ReplyItems { skip: &skip, ..ReplyItems::default() }).is_err()
        );
        let picks = [Pick {
            message_id: Some("../other".to_owned()),
            key: "q-abc".to_owned(),
            revision: 1,
            answer: None,
        }];
        assert!(
            reply_body("run", "key", None, &ReplyItems { questions: &picks, ..ReplyItems::default() })
                .is_err()
        );
        let empty =
            reply_body("run", "key", None, &ReplyItems::default()).expect("server owns empty Send refusal");
        assert!(empty.get("words").is_none());
        assert!(empty.get("to").is_none());
        assert!(empty.get("clear_draft").is_none());
    }

    #[test]
    fn agent_completion_keeps_only_unsent_person_notes_in_draft() {
        let mut result = ActionResult {
            outcome: ActionOutcome::Done,
            note: None,
            by: "person".to_owned(),
            staged: true,
            person: None,
        };
        assert!(result.in_draft(State::Picked, 2, 1));
        assert!(!result.in_draft(State::Sent, 2, 2));
        result.staged = false;
        assert!(!result.in_draft(State::Picked, 2, 1));
        result.by = "agent".to_owned();
        result.person =
            Some(PersonActionResult { outcome: ActionOutcome::Cant, note: Some("Had no access".to_owned()) });
        assert!(result.in_draft(State::Closed, 3, 1));
        assert!(!result.in_draft(State::Closed, 3, 3));
        result.person.as_mut().expect("person note").note = Some("  ".to_owned());
        assert!(!result.in_draft(State::Closed, 3, 1));
    }

    #[test]
    fn held_and_noop_responses_do_not_claim_delivery() {
        let no_op = reply_result(json!({"message_id":"msg_root","questions":[],"reply":null,"replayed":false,"nothing_to_send":true})).expect("skip closed meanwhile");
        assert!(no_op.nothing_to_send);
        assert!(no_op.reply.is_none());
        assert!(no_op.held.is_none());
        for state in ["queued", "sending", "parked", "cancelled", "refused", "dropped"] {
            let result = reply_result(json!({"outbox":{"key":"stable-key","machine":"ibx_peer","route":"messages/msg_root/reply","state":state,"answer":null}})).expect("outbox receipt");
            assert!(result.reply.is_none());
            assert!(!result.replayed);
            let held = result.held.expect("retained receipt");
            assert_eq!(held.key, "stable-key");
            assert_eq!(held.state, state);
        }
        let delivered = reply_result(json!({"outbox":{"key":"stable-key","state":"delivered","answer":{"questions":[],"reply":{"id":"msg_reply","body":"once"},"replayed":true}}})).expect("delivered replay");
        assert!(delivered.replayed);
        assert_eq!(delivered.reply.expect("written reply").id, "msg_reply");
        assert!(delivered.held.is_none());
        assert!(
            reply_result(json!({"ok":true})).is_err(),
            "an unrelated 200 must not be mistaken for a Send"
        );
    }

    #[test]
    fn moving_a_pin_preserves_every_other_thread_and_rejects_edges() {
        let pins: Vec<Pin> = ["msg_a", "msg_b", "msg_c"]
            .into_iter()
            .map(|id| Pin { thread_id: id.to_owned(), pinned_at: "original-time".to_owned() })
            .collect();
        assert_eq!(moved_pins(&pins, "msg_b", true).expect("up"), ["msg_b", "msg_a", "msg_c"]);
        assert_eq!(moved_pins(&pins, "msg_b", false).expect("down"), ["msg_a", "msg_c", "msg_b"]);
        assert!(moved_pins(&pins, "msg_a", true).is_err());
        assert!(moved_pins(&pins, "msg_c", false).is_err());
        assert!(moved_pins(&pins, "msg_missing", false).is_err());
        assert_eq!(pins[1].pinned_at, "original-time");
    }

    #[test]
    fn mark_read_warning_counts_picked_questions_but_not_staged_actions() {
        let thread = Thread {
            messages: vec![Message {
                questions: vec![
                    Question { kind: Kind::Single, state: State::Open, ..Question::default() },
                    Question { kind: Kind::Text, state: State::Picked, ..Question::default() },
                    Question { kind: Kind::Single, state: State::Sent, ..Question::default() },
                    Question { kind: Kind::Action, state: State::Open, ..Question::default() },
                    Question { kind: Kind::Action, state: State::Picked, ..Question::default() },
                ],
                ..Message::default()
            }],
            ..Thread::default()
        };
        assert_eq!(set_aside_counts(&thread), (2, 1));
        assert!(mark_read_label(&thread).contains("2 questions and 1 action items"));
    }
}
