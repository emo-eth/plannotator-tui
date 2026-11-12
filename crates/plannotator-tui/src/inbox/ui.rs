//! The Inbox's state and keys: the threads grouped by section, one open thread with its
//! answer cards, and the compose box for replies and written answers. Pure over the data
//! it is given: every read or write of the server is a [`Request`] the caller performs, so
//! a snapshot draws the same state without ever sending anything.
//! Help and feature selection share one overlay; retry, discard and restart state belongs
//! to the pending Send and is cleared when that Send is confirmed or discarded.

use std::collections::{HashMap, HashSet};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;

use super::client::{List, Live, ThreadModel};
use super::features::{self, Feature};
use super::questions::{Answer, Kind, Question, State};
use super::send::{self, Target};
use super::thread::ThreadView;
use crate::app::compose::{Compose, ComposeAction};

/// Groups closed until opened, as the window has them (packages/inbox/listGroups.ts).
const CLOSED_BY_DEFAULT: [&str; 2] = ["quiet", "snoozed"];

/// Said when a key would change what an unconfirmed Send carries.
const UNCONFIRMED: &str = "an earlier send is unconfirmed: s retries it as it was, D discards it";

/// What the state needs the server for, in the order given.
#[derive(Debug, Clone)]
pub(crate) enum Request {
    List,
    Thread(String),
    Feature(Feature),
    ComposeReply,
    /// Save one pick: `answer` is the answer's JSON, or `Value::Null` to clear it.
    Pick {
        message_id: String,
        key: String,
        revision: u64,
        answer: Value,
    },
    Send(Vec<Target>),
}

/// What the compose box is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ComposeFor {
    Reply,
    Text { message_id: String, key: String },
    Other { message_id: String, key: String },
    Note { message_id: String, key: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Mode {
    List,
    Thread,
    Compose(ComposeFor),
}

/// One cursor stop of the list: a section's header, or a row of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Entry {
    Header(usize),
    Row(usize, usize),
}

/// Whether what is on screen keeps up with the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LiveState {
    /// One frame read through `GET` only: nothing is sent, nothing is watched.
    Snapshot,
    /// The event stream has not said hello yet.
    Connecting,
    Live,
    Lost(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Overlay {
    Closed,
    Features(usize),
    Help,
}

pub(crate) struct Inbox {
    pub(crate) url: String,
    pub(crate) list: Option<List>,
    pub(crate) entries: Vec<Entry>,
    pub(crate) cursor: usize,
    pub(crate) list_scroll: usize,
    collapsed: HashSet<String>,
    pub(crate) thread: Option<ThreadView>,
    pub(crate) mode: Mode,
    pub(crate) compose: Compose,
    pub(crate) status: Option<String>,
    pub(crate) shift_enter: bool,
    pub(crate) quit: bool,
    /// Whether the server's event stream keeps this screen current.
    pub(crate) live: LiveState,
    /// The body's size at the last draw: paging, and the width a thread is laid out for.
    pub(crate) body_height: usize,
    pub(crate) body_width: usize,
    /// What the compose box held when it was submitted, put back if the server refuses it.
    submitted: Option<(ComposeFor, String)>,
    /// A Send the server has not confirmed, replayed as it was until it is.
    pending_send: Option<Unconfirmed>,
    pub(crate) overlay: Overlay,
    pub(crate) project_filter: Option<String>,
    pub(crate) agent_filter: Option<super::agents::AgentListFilter>,
    pub(crate) draft: Option<super::drafts::DraftState>,
    remote_drafts: HashMap<String, super::drafts::DraftState>,
    pub(crate) writes_ready: bool,
}

/// The replies of a Send not yet confirmed, keys included, and the thread they answer. After
/// a refusal or an unanswered reply the rest wait here, sent again exactly as they were: the
/// server answers a replayed key with the reply it wrote, so a lost answer never writes twice.
/// Only a refusal of the first try proves nothing was written; once a try went unanswered,
/// every later failure keeps the Send until the person resolves it.
#[derive(Debug, Clone)]
struct Unconfirmed {
    thread_id: String,
    targets: Vec<Target>,
    retried: bool,
    discard_armed: bool,
    restarted: bool,
}

/// A cursor stop named so it can be found again in a new list.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Keep {
    Row(String),
    Header(String),
}

impl Inbox {
    pub(crate) fn new(url: String) -> Self {
        Self {
            url,
            list: None,
            entries: Vec::new(),
            cursor: 0,
            list_scroll: 0,
            collapsed: CLOSED_BY_DEFAULT.iter().map(|s| (*s).to_owned()).collect(),
            thread: None,
            mode: Mode::List,
            compose: Compose::default(),
            status: None,
            shift_enter: false,
            quit: false,
            live: LiveState::Connecting,
            body_height: 20,
            body_width: 80,
            submitted: None,
            pending_send: None,
            overlay: Overlay::Closed,
            project_filter: None,
            agent_filter: None,
            draft: None,
            remote_drafts: HashMap::new(),
            writes_ready: true,
        }
    }

    // ── What the server answered ──────────────────────────────────────────────────

    pub(crate) fn loaded_list(&mut self, list: List) {
        let keep = self.selection();
        self.list = Some(list);
        self.rebuild_entries(keep.as_ref());
    }

    pub(crate) fn opened_thread(&mut self, model: ThreadModel) {
        if let Some(thread) = self.thread.as_mut().filter(|t| t.id() == model.thread.thread_id) {
            thread.replace(model);
        } else {
            self.keep_remote_draft();
            self.draft = self.remote_drafts.remove(&model.thread.thread_id);
            self.thread = Some(ThreadView::new(model, self.body_width));
            self.mode = Mode::Thread;
        }
        if self.mode == Mode::List {
            self.mode = Mode::Thread;
        }
    }

    pub(crate) fn picked(&mut self, message_id: &str, questions: Vec<Question>) {
        if let Some(thread) = self.thread.as_mut() {
            thread.apply_questions(message_id, questions);
        }
        self.submitted = None;
        self.status = Some("pick saved".to_owned());
    }

    pub(crate) fn sent(&mut self, replies: usize) {
        self.submitted = None;
        self.pending_send = None;
        self.status = Some(if replies == 1 { "sent".to_owned() } else { format!("sent {replies} replies") });
    }

    /// A request the server refused or that never arrived: say so, and put back what the
    /// compose box held so nothing typed is lost.
    pub(crate) fn failed(&mut self, error: &str) {
        self.status = Some(error.to_owned());
        if let Some((purpose, text)) = self.submitted.take() {
            self.compose = Compose::with_text(&text);
            self.mode = Mode::Compose(purpose);
        }
    }

    /// What one event of the server's stream asks to be read again.
    pub(crate) fn live(&mut self, event: Live) -> Vec<Request> {
        let open = self.thread.as_ref().map(|t| t.id().to_owned());
        match event {
            Live::Connected { .. } => {
                self.live = LiveState::Live;
                let mut requests = vec![Request::List];
                requests.extend(open.map(Request::Thread));
                requests
            }
            Live::Record { thread_id, .. } => {
                let mut requests = vec![Request::List];
                if let Some(open) = open
                    && thread_id.as_deref().is_none_or(|id| id == open)
                {
                    requests.push(Request::Thread(open));
                }
                requests
            }
            Live::Refresh => vec![Request::List],
            Live::Restarted { .. } => {
                self.restarting();
                Vec::new()
            }
            Live::Lost(reason) => {
                self.status = Some(format!("live updates lost: {reason}"));
                self.live = LiveState::Lost(reason);
                Vec::new()
            }
        }
    }

    pub(crate) fn restarting(&mut self) {
        self.writes_ready = false;
        if let Some(pending) = self.pending_send.as_mut() {
            pending.restarted = true;
        }
        self.live = LiveState::Connecting;
        self.status = Some("verifying Inbox restart; writes paused".to_owned());
    }

    pub(crate) fn has_unconfirmed(&self) -> bool {
        self.pending_send.is_some()
    }

    /// Keep the server draft in step with the reply editor without writing on every key.
    pub(crate) fn sync_reply_draft(&mut self) -> anyhow::Result<bool> {
        if self.has_unconfirmed() || self.mode != Mode::Compose(ComposeFor::Reply) {
            return Ok(false);
        }
        let text = self.compose.value();
        if let Some(draft) = self.draft.as_mut()
            && draft.body() != text
        {
            draft.set_body(text)?;
            return Ok(true);
        }
        Ok(false)
    }

    pub(crate) fn save_reply_draft(&mut self, client: &super::client::Client) -> anyhow::Result<()> {
        if self.has_unconfirmed() || !self.writes_ready {
            return Ok(());
        }
        if let Some(draft) = self.draft.as_mut()
            && draft.is_dirty()
            && draft.save(client)? == super::drafts::SaveOutcome::Conflict
        {
            anyhow::bail!("reply draft changed in another window; F2 → Reply draft resolves the conflict");
        }
        Ok(())
    }

    // ── The list ──────────────────────────────────────────────────────────────────

    /// The cursor's stop, as something that survives a new list: a thread id or a section id.
    fn selection(&self) -> Option<Keep> {
        let list = self.list.as_ref()?;
        match self.entries.get(self.cursor).copied()? {
            Entry::Row(s, r) => Some(Keep::Row(list.sections.get(s)?.threads.get(r)?.thread_id.clone())),
            Entry::Header(s) => Some(Keep::Header(list.sections.get(s)?.id.clone())),
        }
    }

    fn rebuild_entries(&mut self, keep: Option<&Keep>) {
        let mut entries = Vec::new();
        if let Some(list) = &self.list {
            for (s, section) in list.sections.iter().enumerate() {
                if !section.threads.iter().any(|row| self.matches_project(row)) {
                    continue;
                }
                entries.push(Entry::Header(s));
                if !self.collapsed.contains(&section.id) {
                    entries.extend(
                        section
                            .threads
                            .iter()
                            .enumerate()
                            .filter(|(_, row)| self.matches_project(row))
                            .map(|(r, _)| Entry::Row(s, r)),
                    );
                }
            }
        }
        self.entries = entries;
        let found = match (keep, &self.list) {
            (Some(Keep::Row(id)), Some(_)) => {
                self.entries.iter().position(|e| self.row_id(*e).as_deref() == Some(id.as_str()))
            }
            (Some(Keep::Header(id)), Some(list)) => self.entries.iter().position(|e| {
                matches!(e, Entry::Header(s) if list.sections.get(*s).is_some_and(|section| section.id == *id))
            }),
            _ => None,
        };
        self.cursor = found.unwrap_or(self.cursor).min(self.entries.len().saturating_sub(1));
    }

    fn row_id(&self, entry: Entry) -> Option<String> {
        match entry {
            Entry::Row(s, r) => {
                self.list.as_ref()?.sections.get(s)?.threads.get(r).map(|row| row.thread_id.clone())
            }
            Entry::Header(_) => None,
        }
    }

    pub(crate) fn context_thread_id(&self) -> Option<String> {
        self.thread
            .as_ref()
            .map(|t| t.id().to_owned())
            .or_else(|| self.entries.get(self.cursor).and_then(|entry| self.row_id(*entry)))
    }

    fn matches_project(&self, row: &super::client::Row) -> bool {
        self.project_filter.as_ref().is_none_or(|id| row.project.id == *id)
            && self.agent_filter.as_ref().is_none_or(|filter| {
                super::agents::agent_row_matches(
                    row,
                    filter,
                    self.list.as_ref().and_then(|list| list.self_machine.as_deref()),
                )
            })
    }

    pub(crate) fn set_project_filter(&mut self, project: Option<String>) {
        self.project_filter = project;
        self.keep_remote_draft();
        self.thread = None;
        self.draft = None;
        self.mode = Mode::List;
        self.cursor = 0;
        self.list_scroll = 0;
        self.rebuild_entries(None);
    }

    pub(crate) fn set_agent_filter(&mut self, filter: Option<super::agents::AgentListFilter>) {
        self.agent_filter = filter;
        self.keep_remote_draft();
        self.thread = None;
        self.draft = None;
        self.mode = Mode::List;
        self.cursor = 0;
        self.list_scroll = 0;
        self.rebuild_entries(None);
    }

    pub(crate) fn compose_reply(&mut self, text: &str) {
        self.compose = Compose::with_text(text);
        self.mode = Mode::Compose(ComposeFor::Reply);
    }

    pub(crate) fn replace_pending(&mut self, targets: Vec<Target>) {
        if let Some(pending) = self.pending_send.as_mut() {
            pending.targets = targets;
        }
    }

    pub(crate) fn cancel_preparation(&mut self, message: &str) {
        self.pending_send = None;
        if let Some(draft) = self.draft.as_mut() {
            draft.release_send();
        }
        self.failed(message);
    }

    pub(crate) fn is_collapsed(&self, section_id: &str) -> bool {
        self.collapsed.contains(section_id)
    }

    fn move_cursor(&mut self, delta: i64) {
        let last = self.entries.len().saturating_sub(1) as i64;
        self.cursor = (self.cursor as i64 + delta).clamp(0, last) as usize;
    }

    /// Keep the list cursor inside the `height` rows the list gets.
    pub(crate) fn ensure_cursor_visible(&mut self, height: usize) {
        let height = height.max(1);
        if self.cursor < self.list_scroll {
            self.list_scroll = self.cursor;
        } else if self.cursor >= self.list_scroll + height {
            self.list_scroll = self.cursor + 1 - height;
        }
    }

    fn list_key(&mut self, key: KeyEvent) -> Vec<Request> {
        let page = self.body_height.max(1) as i64;
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.move_cursor(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_cursor(1),
            KeyCode::PageUp => self.move_cursor(-page),
            KeyCode::PageDown => self.move_cursor(page),
            KeyCode::Home | KeyCode::Char('g') => self.cursor = 0,
            KeyCode::End | KeyCode::Char('G') => self.cursor = self.entries.len().saturating_sub(1),
            KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Right => {
                match self.entries.get(self.cursor).copied() {
                    Some(Entry::Header(s)) => self.toggle_section(s),
                    Some(entry @ Entry::Row(..)) => {
                        if let Some(id) = self.row_id(entry) {
                            return vec![Request::Thread(id)];
                        }
                    }
                    None => {}
                }
            }
            KeyCode::Left => {
                if let Some(Entry::Row(s, _) | Entry::Header(s)) = self.entries.get(self.cursor).copied() {
                    self.collapse_section(s);
                }
            }
            KeyCode::Char('r' | 'R') => return vec![Request::List],
            KeyCode::Esc | KeyCode::Char('q') => self.quit = true,
            _ => {}
        }
        Vec::new()
    }

    fn toggle_section(&mut self, section: usize) {
        let Some(id) = self.list.as_ref().and_then(|l| l.sections.get(section)).map(|s| s.id.clone()) else {
            return;
        };
        if !self.collapsed.remove(&id) {
            self.collapsed.insert(id);
        }
        let keep = self.selection();
        self.rebuild_entries(keep.as_ref());
    }

    fn collapse_section(&mut self, section: usize) {
        let Some(id) = self.list.as_ref().and_then(|l| l.sections.get(section)).map(|s| s.id.clone()) else {
            return;
        };
        self.collapsed.insert(id);
        self.cursor = self.entries.iter().position(|e| *e == Entry::Header(section)).unwrap_or(self.cursor);
        let keep = self.selection();
        self.rebuild_entries(keep.as_ref());
    }

    // ── The thread ────────────────────────────────────────────────────────────────
    fn keep_remote_draft(&mut self) {
        if let Some(draft) = self.draft.take().filter(super::drafts::DraftState::is_remote) {
            self.remote_drafts.insert(draft.thread_id.clone(), draft);
        }
    }

    fn close_thread(&mut self) {
        if self.blocked_by_unconfirmed() {
            return;
        }
        self.keep_remote_draft();
        self.thread = None;
        self.draft = None;
        self.mode = Mode::List;
    }

    /// The unconfirmed Send of the open thread, if any.
    fn unconfirmed(&self) -> Option<&[Target]> {
        let pending = self.pending_send.as_ref()?;
        let thread = self.thread.as_ref()?;
        (pending.thread_id == thread.id()).then_some(pending.targets.as_slice())
    }

    /// Whether a key that would change a pick or the words is refused for an unconfirmed Send.
    fn blocked_by_unconfirmed(&mut self) -> bool {
        let blocked = self.unconfirmed().is_some();
        if blocked {
            self.status = Some(UNCONFIRMED.to_owned());
        }
        blocked
    }

    /// `D` twice: give up on an unconfirmed Send. The first press says what that risks.
    fn discard_send(&mut self) {
        if self.unconfirmed().is_none() {
            self.status = Some("no unconfirmed send".to_owned());
            return;
        }
        if self.pending_send.as_ref().is_some_and(|pending| pending.discard_armed) {
            self.pending_send = None;
            if let Some(draft) = self.draft.as_mut() {
                draft.release_send();
            }
            self.status = Some(
                "unconfirmed send discarded: what it wrote, if anything, is in the thread; sending it again would write it twice"
                    .to_owned(),
            );
        } else if let Some(pending) = self.pending_send.as_mut() {
            pending.discard_armed = true;
            self.status = Some(
                "that send may already have been delivered; D again discards it anyway (s retries it as it was)"
                    .to_owned(),
            );
        }
    }

    fn thread_key(&mut self, key: KeyEvent) -> Vec<Request> {
        let page = self.body_height.max(1) as i64;
        let Some(thread) = self.thread.as_mut() else {
            self.mode = Mode::List;
            return Vec::new();
        };
        let focused = thread.focus().is_some();
        match key.code {
            KeyCode::Tab | KeyCode::Char('n') => thread.focus_step(true),
            KeyCode::BackTab | KeyCode::Char('p') => thread.focus_step(false),
            KeyCode::Up | KeyCode::Char('k') if focused => thread.move_choice(-1),
            KeyCode::Down | KeyCode::Char('j') if focused => thread.move_choice(1),
            KeyCode::Up | KeyCode::Char('k') => thread.scroll_by(-1),
            KeyCode::Down | KeyCode::Char('j') => thread.scroll_by(1),
            KeyCode::PageUp | KeyCode::Char('b') => thread.scroll_by(-page),
            KeyCode::PageDown | KeyCode::Char(' ') if !focused => thread.scroll_by(page),
            KeyCode::Home | KeyCode::Char('g') => thread.scroll_to(0),
            KeyCode::End | KeyCode::Char('G') => thread.scroll_to(usize::MAX),
            KeyCode::Esc if focused => thread.unfocus(),
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Backspace => self.close_thread(),
            KeyCode::Enter | KeyCode::Char(' ') if focused => return self.pick_highlighted(),
            KeyCode::Char(digit @ '1'..='9') if focused => {
                let index = usize::from(digit as u8 - b'1');
                if thread.set_choice(index) {
                    return self.pick_highlighted();
                }
            }
            KeyCode::Char('o') if focused => {
                self.open_answer_box(|id, key| ComposeFor::Other { message_id: id, key });
            }
            KeyCode::Char('t') if focused => {
                self.open_answer_box(|id, key| ComposeFor::Text { message_id: id, key });
            }
            KeyCode::Char('N') if focused => {
                self.open_answer_box(|id, key| ComposeFor::Note { message_id: id, key });
            }
            KeyCode::Char('x') if focused => return self.clear_pick(),
            KeyCode::Enter | KeyCode::Char('r') => {
                let seed = self.unconfirmed().and_then(|p| p.iter().find_map(|t| t.words.clone()));
                if let Some(seed) = seed {
                    self.compose_reply(&seed);
                } else {
                    return vec![Request::ComposeReply];
                }
            }
            KeyCode::Char('s') => return self.send(""),
            KeyCode::Char('R') => return vec![Request::Thread(thread.id().to_owned())],
            KeyCode::Char('D') => self.discard_send(),
            _ => {}
        }
        Vec::new()
    }

    /// The focused question and its message id, cloned out so the view can be edited.
    fn focused_question(&self) -> Option<(String, Question, Answer)> {
        let thread = self.thread.as_ref()?;
        let (message, question) = thread.focused()?;
        let answer = thread.focused_answer()?;
        Some((message.id.clone(), question.clone(), answer))
    }

    /// Pick the highlighted row of the focused card: a choice, Other…, or the text answer.
    fn pick_highlighted(&mut self) -> Vec<Request> {
        if self.blocked_by_unconfirmed() {
            return Vec::new();
        }
        let Some((message_id, question, mut answer)) = self.focused_question() else { return Vec::new() };
        let Some(choice) = self.thread.as_ref().and_then(ThreadView::focus).map(|f| f.choice) else {
            return Vec::new();
        };
        match question.kind {
            Kind::Single | Kind::Multi => {
                let Some(label) = question.choices.get(choice).map(|c| c.label.clone()) else {
                    self.open_answer_box(|id, key| ComposeFor::Other { message_id: id, key });
                    return Vec::new();
                };
                if question.kind == Kind::Multi {
                    match answer.selected.iter().position(|s| *s == label) {
                        Some(at) => {
                            answer.selected.remove(at);
                        }
                        None => answer.selected.push(label),
                    }
                } else {
                    answer.selected = vec![label];
                    answer.other = None;
                }
                answer.text = None;
                vec![self.save_pick(&message_id, &question, answer)]
            }
            Kind::Text => {
                self.open_answer_box(|id, key| ComposeFor::Text { message_id: id, key });
                Vec::new()
            }
            Kind::Action | Kind::Unknown => {
                self.status = Some("this item cannot be answered here".to_owned());
                Vec::new()
            }
        }
    }

    fn clear_pick(&mut self) -> Vec<Request> {
        if self.blocked_by_unconfirmed() {
            return Vec::new();
        }
        let Some((message_id, question, _)) = self.focused_question() else { return Vec::new() };
        if question.state != State::Picked && question.answer.is_none() {
            return Vec::new();
        }
        if let Some(thread) = self.thread.as_mut() {
            thread.draft(&message_id, &question.key, Answer::default());
        }
        vec![Request::Pick {
            message_id,
            key: question.key,
            revision: question.revision,
            answer: Value::Null,
        }]
    }

    /// Open the compose box on the focused question, seeded with what the answer holds.
    fn open_answer_box(&mut self, purpose: impl FnOnce(String, String) -> ComposeFor) {
        if self.blocked_by_unconfirmed() {
            return;
        }
        let Some((message_id, question, answer)) = self.focused_question() else { return };
        if !matches!(question.state, State::Open | State::Picked) {
            self.status = Some("this question is no longer open".to_owned());
            return;
        }
        let purpose = purpose(message_id, question.key.clone());
        let seed = match &purpose {
            ComposeFor::Text { .. } => answer.text.clone().or_else(|| question.suggested_text.clone()),
            ComposeFor::Other { .. } => answer.other.clone(),
            ComposeFor::Note { .. } => answer.note.clone(),
            ComposeFor::Reply => None,
        };
        if matches!(purpose, ComposeFor::Other { .. }) && question.kind == Kind::Text {
            self.status = Some("a free-text question takes a written answer: t".to_owned());
            return;
        }
        if matches!(purpose, ComposeFor::Text { .. }) && question.kind != Kind::Text {
            self.status = Some("this question takes a choice; o writes an Other… answer".to_owned());
            return;
        }
        self.compose = Compose::with_text(seed.as_deref().unwrap_or(""));
        self.mode = Mode::Compose(purpose);
    }

    /// Save a pick as the draft and ask the server to hold it; an answer with nothing in it
    /// clears the pick (the server takes `null`, never an empty answer).
    fn save_pick(&mut self, message_id: &str, question: &Question, answer: Answer) -> Request {
        let value = if answer.is_empty() { Value::Null } else { answer.serialize(question) };
        if let Some(thread) = self.thread.as_mut() {
            thread.draft(message_id, &question.key, answer);
        }
        Request::Pick {
            message_id: message_id.to_owned(),
            key: question.key.clone(),
            revision: question.revision,
            answer: value,
        }
    }

    /// A Send: the saved picks, and `words` when there are any, as the window would send them.
    /// While an earlier Send is unconfirmed, `s` or the same words send it again exactly as
    /// it was, keys included; other words wait until it is confirmed or discarded.
    fn send(&mut self, words: &str) -> Vec<Request> {
        if self.pending_send.as_ref().is_some_and(|pending| pending.restarted) {
            self.status = Some("Inbox restarted after an unconfirmed send; inspect the thread, then D twice discards it. Automatic replay across restart is disabled.".to_owned());
            return Vec::new();
        }
        let Some(thread) = self.thread.as_ref() else { return Vec::new() };
        if let Some(pending) = self.pending_send.as_mut().filter(|p| p.thread_id == thread.id()) {
            if !words.is_empty() && !pending.targets.iter().any(|t| t.words.as_deref() == Some(words)) {
                self.status = Some(UNCONFIRMED.to_owned());
                return Vec::new();
            }
            pending.retried = true;
            return vec![Request::Send(pending.targets.clone())];
        }
        let targets = send::targets(&thread.model.thread, words);
        self.pending_send = Some(Unconfirmed {
            thread_id: thread.id().to_owned(),
            targets: targets.clone(),
            retried: false,
            discard_armed: false,
            restarted: false,
        });
        vec![Request::Send(targets)]
    }

    /// A Send that stopped at a reply the server refused or never answered: `went` replies
    /// before it were confirmed, `remaining` were not. A refusal (`refused`) of a first try
    /// means the server wrote nothing of it, so the Send is over and the words go back into
    /// the compose box to be edited. Anything else leaves `remaining` unconfirmed: it may or
    /// may not have been written, so it stays to be sent again as it was.
    pub(crate) fn send_failed(&mut self, went: usize, remaining: Vec<Target>, error: &str, refused: bool) {
        let words_unsent = remaining.iter().any(|t| t.words.is_some());
        if refused && self.pending_send.as_ref().is_none_or(|p| !p.retried) {
            if let Some(draft) = self.draft.as_mut() {
                draft.release_send();
            }
            self.pending_send = None;
            if words_unsent {
                self.failed(error);
            } else {
                self.submitted = None;
                self.status = Some(error.to_owned());
            }
            return;
        }
        let total = went + remaining.len();
        if let Some(pending) = self.pending_send.as_mut() {
            pending.targets = remaining;
        }
        let went = if went > 0 { format!("; {went} of {total} replies went") } else { String::new() };
        if words_unsent {
            self.failed(&format!(
                "{error} (unconfirmed{went}: send again unchanged to retry it; Esc then D discards it)"
            ));
        } else {
            self.submitted = None;
            self.status = Some(format!("{error} (unconfirmed{went}: s retries it as it was, D discards it)"));
        }
    }

    // ── The compose box ───────────────────────────────────────────────────────────

    fn compose_key(&mut self, purpose: ComposeFor, key: KeyEvent) -> Vec<Request> {
        if key.code == KeyCode::F(2) && purpose == ComposeFor::Reply {
            if !self.blocked_by_unconfirmed() {
                if let Err(error) = self.sync_reply_draft() {
                    self.failed(&format!("{error:#}"));
                    return Vec::new();
                }
                return vec![Request::Feature(Feature::Drafts)];
            }
            return Vec::new();
        }
        match self.compose.handle_key(key) {
            ComposeAction::Edited => Vec::new(),
            ComposeAction::Cancel => {
                self.mode = Mode::Thread;
                Vec::new()
            }
            ComposeAction::Save => {
                let text = self.compose.value();
                self.mode = Mode::Thread;
                self.submit(purpose, text)
            }
        }
    }

    fn submit(&mut self, purpose: ComposeFor, text: String) -> Vec<Request> {
        let requests = match &purpose {
            ComposeFor::Reply => {
                if text.trim().is_empty() && self.draft.as_ref().is_none_or(|draft| draft.files().is_empty())
                {
                    self.status = Some("the reply is empty".to_owned());
                    return Vec::new();
                }
                let retry = self.has_unconfirmed();
                let mut requests = self.send(&text);
                if text.trim().is_empty() && !retry && !requests.is_empty() {
                    let message_id = self
                        .thread
                        .as_ref()
                        .and_then(|thread| send::words_message(&thread.model.thread))
                        .map(|message| message.id.clone());
                    if let (Some(Request::Send(targets)), Some(message_id)) =
                        (requests.first_mut(), message_id)
                    {
                        if let Some(target) =
                            targets.iter_mut().find(|target| target.message_id == message_id)
                        {
                            target.words = Some(String::new());
                        } else {
                            targets.push(Target {
                                message_id,
                                words: Some(String::new()),
                                idempotency_key: send::new_key(),
                                ..Target::default()
                            });
                        }
                        self.replace_pending(targets.clone());
                    }
                }
                if requests.is_empty() && self.unconfirmed().is_some() {
                    self.compose = Compose::with_text(&text);
                    self.mode = Mode::Compose(ComposeFor::Reply);
                }
                requests
            }
            ComposeFor::Text { message_id, key }
            | ComposeFor::Other { message_id, key }
            | ComposeFor::Note { message_id, key } => {
                if self.blocked_by_unconfirmed() {
                    return Vec::new();
                }
                let Some((question, mut answer)) = self.question_on(message_id, key) else {
                    self.status = Some("that question is gone".to_owned());
                    return Vec::new();
                };
                let value = (!text.trim().is_empty()).then(|| text.clone());
                match &purpose {
                    ComposeFor::Text { .. } => answer.text = value,
                    ComposeFor::Other { .. } => {
                        answer.other = value;
                        if question.kind == Kind::Single {
                            answer.selected.clear();
                        }
                    }
                    ComposeFor::Note { .. } | ComposeFor::Reply => answer.note = value,
                }
                vec![self.save_pick(message_id, &question, answer)]
            }
        };
        if !requests.is_empty() {
            self.submitted = Some((purpose, text));
        }
        requests
    }

    fn question_on(&self, message_id: &str, key: &str) -> Option<(Question, Answer)> {
        let thread = self.thread.as_ref()?;
        let question = thread
            .model
            .thread
            .messages
            .iter()
            .find(|m| m.id == message_id)?
            .questions
            .iter()
            .find(|q| q.key == key)?
            .clone();
        let answer =
            thread.answer_on(message_id, key).or_else(|| question.answer.clone()).unwrap_or_default();
        Some((question, answer))
    }

    // ── Input ─────────────────────────────────────────────────────────────────────

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> Vec<Request> {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return Vec::new();
        }
        if self.overlay == Overlay::Help {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('q' | '?') | KeyCode::F(1)) {
                self.overlay = Overlay::Closed;
            }
            return Vec::new();
        }
        if let Overlay::Features(cursor) = self.overlay {
            let last = features::ALL.len().saturating_sub(1);
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::F(2) => self.overlay = Overlay::Closed,
                KeyCode::Down | KeyCode::Char('j') => {
                    self.overlay = Overlay::Features((cursor + 1).min(last));
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.overlay = Overlay::Features(cursor.saturating_sub(1));
                }
                KeyCode::Home => self.overlay = Overlay::Features(0),
                KeyCode::End => self.overlay = Overlay::Features(last),
                KeyCode::Enter => {
                    if let Some(feature) = features::ALL.get(cursor).copied() {
                        self.overlay = Overlay::Closed;
                        if feature.needs_thread() && self.context_thread_id().is_none() {
                            self.status = Some("select or open a thread first".to_owned());
                        } else {
                            return vec![Request::Feature(feature)];
                        }
                    }
                }
                _ => {}
            }
            return Vec::new();
        }
        if !matches!(self.mode, Mode::Compose(_)) {
            if matches!(key.code, KeyCode::Char('?') | KeyCode::F(1)) {
                self.overlay = Overlay::Help;
                return Vec::new();
            }
            let feature = match key.code {
                KeyCode::Char('/') => Some(Feature::Search),
                KeyCode::Char('L') => Some(Feature::Launch),
                _ => None,
            };
            if key.code == KeyCode::F(2) || feature.is_some() {
                if self.blocked_by_unconfirmed() {
                    return Vec::new();
                }
                if let Some(feature) = feature {
                    return vec![Request::Feature(feature)];
                }
                self.overlay = Overlay::Features(0);
                return Vec::new();
            }
        }
        self.status = None;
        if let Some(pending) = self.pending_send.as_mut() {
            pending.discard_armed &= key.code == KeyCode::Char('D');
        }
        match self.mode.clone() {
            Mode::Compose(purpose) => self.compose_key(purpose, key),
            Mode::List => self.list_key(key),
            Mode::Thread => self.thread_key(key),
        }
    }

    pub(crate) fn paste(&mut self, text: &str) {
        if matches!(self.mode, Mode::Compose(_)) {
            self.compose.insert_text(text);
        }
    }

    /// The header's words for where the person is.
    pub(crate) fn title(&self) -> String {
        if let Some(thread) = &self.thread {
            let t = &thread.model.thread;
            let mut title =
                super::format::title(t.subject.as_deref(), t.thread_name.as_deref(), &t.thread_id);
            title.push_str(" · ");
            title.push_str(&t.project.name);
            return title;
        }
        let threads: usize =
            self.list.as_ref().map_or(0, |l| l.sections.iter().map(|s| s.threads.len()).sum());
        let filter = self.project_filter.as_deref().map_or(String::new(), |id| format!(" · project {id}"));
        let agent =
            self.agent_filter.as_ref().map_or(String::new(), |filter| format!(" · agent {}", filter.name));
        format!("Inbox · {threads} threads{filter}{agent} · {}", self.url)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic, reason = "tests assert by panicking")]
mod tests {
    use super::super::client::{Author, Message, Thread, ThreadModel};
    use super::super::questions::Choice;
    use super::*;

    const THREAD: &str = "msg_1";

    fn question(key: &str, state: State) -> Question {
        let choice = |label: &str| Choice { label: label.to_owned(), ..Choice::default() };
        Question {
            key: key.to_owned(),
            kind: Kind::Single,
            prompt: "Which?".to_owned(),
            choices: vec![choice("First"), choice("Second")],
            state,
            revision: 1,
            ..Question::default()
        }
    }

    fn asked(id: &str, session: &str, questions: Vec<Question>) -> Message {
        Message {
            id: id.to_owned(),
            thread_id: THREAD.to_owned(),
            author: Author {
                kind: "agent".to_owned(),
                session: Some(session.to_owned()),
                ..Author::default()
            },
            body: "Choose.".to_owned(),
            questions,
            ..Message::default()
        }
    }

    fn model(thread_id: &str, messages: Vec<Message>) -> ThreadModel {
        ThreadModel {
            server_session: String::new(),
            cursor: 0,
            thread: Thread { thread_id: thread_id.to_owned(), messages, ..Thread::default() },
        }
    }

    fn inbox_with(messages: Vec<Message>) -> Inbox {
        let mut inbox = Inbox::new("http://127.0.0.1:1".to_owned());
        inbox.opened_thread(model(THREAD, messages));
        inbox
    }

    fn one_send(requests: Vec<Request>) -> Vec<Target> {
        match requests.into_iter().next() {
            Some(Request::Send(targets)) => targets,
            other => panic!("expected one Send, got {other:?}"),
        }
    }

    fn press(inbox: &mut Inbox, code: KeyCode) -> Vec<Request> {
        inbox.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn help_and_features_share_one_overlay() {
        let mut inbox = Inbox::new("http://127.0.0.1:1".to_owned());
        assert!(press(&mut inbox, KeyCode::F(1)).is_empty());
        assert_eq!(inbox.overlay, Overlay::Help);
        assert!(press(&mut inbox, KeyCode::F(2)).is_empty());
        assert_eq!(inbox.overlay, Overlay::Help);
        assert!(press(&mut inbox, KeyCode::Esc).is_empty());
        assert_eq!(inbox.overlay, Overlay::Closed);
        assert!(press(&mut inbox, KeyCode::F(2)).is_empty());
        assert_eq!(inbox.overlay, Overlay::Features(0));
        assert!(press(&mut inbox, KeyCode::Down).is_empty());
        assert_eq!(inbox.overlay, Overlay::Features(1));
        assert!(press(&mut inbox, KeyCode::F(2)).is_empty());
        assert_eq!(inbox.overlay, Overlay::Closed);
    }

    #[test]
    fn an_unanswered_send_is_sent_again_exactly_as_it_was() {
        let mut inbox = inbox_with(vec![asked("msg_1", "s1", vec![question("q1", State::Picked)])]);
        let first = one_send(inbox.send("hello"));
        assert_eq!(first.len(), 1);
        assert!(!first[0].idempotency_key.is_empty());
        inbox.send_failed(0, first.clone(), "timed out", false);
        // The server had written it: the thread read again shows the pick sent.
        inbox.opened_thread(model(THREAD, vec![asked("msg_1", "s1", vec![question("q1", State::Sent)])]));
        let again = one_send(inbox.send("hello"));
        assert_eq!(again[0].idempotency_key, first[0].idempotency_key);
        assert_eq!(again[0].questions.len(), 1);
        assert_eq!(again[0].words.as_deref(), Some("hello"));
        let by_s = one_send(inbox.send(""));
        assert_eq!(by_s[0].idempotency_key, first[0].idempotency_key);
        assert_eq!(by_s[0].words.as_deref(), Some("hello"));
        inbox.sent(1);
        let fresh = one_send(inbox.send("hello"));
        assert_ne!(fresh[0].idempotency_key, first[0].idempotency_key);
        assert!(fresh[0].questions.is_empty());
    }

    #[test]
    fn other_words_and_picks_wait_for_the_unconfirmed_send() {
        let mut inbox = inbox_with(vec![asked("msg_1", "s1", vec![question("q1", State::Picked)])]);
        let first = one_send(inbox.send("hello"));
        inbox.send_failed(0, first, "timed out", false);
        inbox.mode = Mode::Thread;
        assert!(inbox.submit(ComposeFor::Reply, "other".to_owned()).is_empty());
        assert_eq!(inbox.mode, Mode::Compose(ComposeFor::Reply));
        assert_eq!(inbox.compose.value(), "other");
        assert_eq!(inbox.status.as_deref(), Some(UNCONFIRMED));
        inbox.mode = Mode::Thread;
        inbox.thread.as_mut().expect("thread").focus_step(true);
        assert!(press(&mut inbox, KeyCode::Enter).is_empty());
        assert_eq!(inbox.status.as_deref(), Some(UNCONFIRMED));
        assert!(press(&mut inbox, KeyCode::Char('x')).is_empty());
        assert_eq!(inbox.status.as_deref(), Some(UNCONFIRMED));
        press(&mut inbox, KeyCode::Char('q'));
        assert!(inbox.thread.is_some(), "the thread stays open while a send is unconfirmed");
        press(&mut inbox, KeyCode::Char('r'));
        assert_eq!(inbox.compose.value(), "hello");
    }

    #[test]
    fn a_partial_send_retries_only_the_rest() {
        let mut inbox = inbox_with(vec![
            asked("msg_1", "s1", vec![question("q1", State::Picked)]),
            asked("msg_2", "s2", vec![question("q2", State::Picked)]),
        ]);
        let first = one_send(inbox.send("hi"));
        assert_eq!(first.len(), 2);
        assert_eq!(first[1].words.as_deref(), Some("hi"));
        inbox.send_failed(1, vec![first[1].clone()], "timed out", false);
        inbox.opened_thread(model(
            THREAD,
            vec![
                asked("msg_1", "s1", vec![question("q1", State::Sent)]),
                asked("msg_2", "s2", vec![question("q2", State::Picked)]),
            ],
        ));
        let again = one_send(inbox.send(""));
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].message_id, "msg_2");
        assert_eq!(again[0].idempotency_key, first[1].idempotency_key);
    }

    #[test]
    fn a_refused_first_try_is_over_but_a_refused_retry_is_not() {
        let mut inbox = inbox_with(vec![asked("msg_1", "s1", vec![question("q1", State::Picked)])]);
        let first = one_send(inbox.submit(ComposeFor::Reply, "hello".to_owned()));
        inbox.send_failed(0, first.clone(), "HTTP 409", true);
        assert_eq!(inbox.mode, Mode::Compose(ComposeFor::Reply));
        let second = one_send(inbox.send("hello"));
        assert_ne!(second[0].idempotency_key, first[0].idempotency_key);
        inbox.send_failed(0, second.clone(), "timed out", false);
        let retry = one_send(inbox.send(""));
        inbox.send_failed(0, retry, "HTTP 401", true);
        let still = one_send(inbox.send(""));
        assert_eq!(still[0].idempotency_key, second[0].idempotency_key);
    }

    #[test]
    fn discarding_takes_two_presses_and_frees_a_new_send() {
        let mut inbox = inbox_with(vec![asked("msg_1", "s1", vec![question("q1", State::Picked)])]);
        let first = one_send(inbox.send(""));
        inbox.send_failed(0, first.clone(), "timed out", false);
        press(&mut inbox, KeyCode::Char('D'));
        assert!(inbox.status.as_deref().is_some_and(|s| s.contains("may already have been delivered")));
        press(&mut inbox, KeyCode::Char('j'));
        press(&mut inbox, KeyCode::Char('D'));
        assert!(inbox.unconfirmed().is_some(), "a D after another key only warns again");
        press(&mut inbox, KeyCode::Char('D'));
        assert!(inbox.unconfirmed().is_none());
        let fresh = one_send(inbox.send("new words"));
        assert_ne!(fresh[0].idempotency_key, first[0].idempotency_key);
    }

    #[test]
    fn feature_navigation_cannot_overtake_an_unconfirmed_send() {
        let mut inbox = inbox_with(vec![asked("msg_1", "s1", vec![question("q1", State::Picked)])]);
        let sent = one_send(inbox.send("words"));
        inbox.send_failed(0, sent, "timed out", false);
        assert!(press(&mut inbox, KeyCode::F(2)).is_empty());
        assert_eq!(inbox.overlay, Overlay::Closed);
        assert!(press(&mut inbox, KeyCode::Char('L')).is_empty());
        assert!(press(&mut inbox, KeyCode::Char('/')).is_empty());
        assert!(inbox.has_unconfirmed());
    }

    #[test]
    fn permission_cards_never_enter_generic_pick_or_reply_targets() {
        let mut inbox = inbox_with(vec![asked("permission", "perm:s1", vec![question("q1", State::Picked)])]);
        let thread = inbox.thread.as_mut().expect("thread");
        thread.focus_step(true);
        assert!(thread.focus().is_none());
        assert!(send::targets(&thread.model.thread, "words").is_empty());
        assert!(send::targets(&thread.model.thread, "").is_empty());
    }

    #[test]
    fn restart_never_replays_an_old_unconfirmed_send() {
        let mut inbox = inbox_with(vec![asked("msg_1", "s1", vec![question("q1", State::Picked)])]);
        let first = one_send(inbox.send("words"));
        inbox.send_failed(0, first, "timed out", false);
        inbox.restarting();
        inbox.writes_ready = true;
        assert!(inbox.send("").is_empty());
        assert!(inbox.has_unconfirmed());
        assert!(inbox.status.as_deref().is_some_and(|status| status.contains("restart")));
        assert!(press(&mut inbox, KeyCode::Char('D')).is_empty());
        assert!(inbox.has_unconfirmed());
        assert!(press(&mut inbox, KeyCode::Char('D')).is_empty());
        assert!(!inbox.has_unconfirmed());
        let fresh = one_send(inbox.send("new words"));
        assert_eq!(fresh[0].words.as_deref(), Some("new words"));
    }
}
