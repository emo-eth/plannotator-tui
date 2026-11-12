//! Ask chat, history and the live interaction panel.
//!
//! `run` borrows the Inbox's terminal without changing terminal modes. Opening,
//! refreshing and subscribing are GET-only; sending, stopping, seen and ending
//! are explicit user actions. HTTP work runs off the terminal thread. The pane
//! retains per-thread drafts, uncertain sends and partial answers when closed,
//! switched or refreshed. Retry uses the original turn id and full payload.
//! The wire follows `packages/server/inbox-ask.ts` and `packages/ai/endpoints.ts`.

mod model;
mod transport;
mod view;

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serde_json::{Value, json};

use self::model::{Chat, Conversation, LiveAnswer, Model, Posted, Question, shown_question};
use self::transport::{Wire, thread_path};
use super::client::{Client, Live};
use crate::app::compose::{Compose, ComposeAction};

#[derive(Debug, Clone)]
pub(crate) enum Action {
    Close,
    Reply { thread_id: String, words: String },
    OpenShape { thread_id: String, shape_id: String },
}

#[derive(Debug)]
pub(crate) struct Answer {
    pub(crate) text: String,
    pub(crate) provider_id: String,
}

/// Blocking API for another pane's already-confirmed question (e.g. a Shape
/// note). The caller owns its stable id; failures never manufacture an answer.
pub(crate) fn answer(
    client: &Client,
    thread_id: &str,
    turn_id: &str,
    prompt: &str,
    busy_policy: Option<&str>,
) -> Result<Answer> {
    let wire = Wire::new(client)?;
    let question = Question::plain(turn_id.to_owned(), prompt.to_owned(), None);
    let posted = wire.send(thread_id, &question)?;
    if let Some(text) = posted.turn.answer {
        anyhow::ensure!(
            !posted.turn.stopped && posted.turn.failure.is_none(),
            "The saved Ask answer was stopped or incomplete"
        );
        return Ok(Answer {
            text,
            provider_id: format!("inbox-ask:{}#{}", posted.turn.session, posted.turn.id),
        });
    }
    let provider_id = posted
        .provider_id
        .context("The agent cannot answer now; the question is saved and may be retried with the same id")?;
    let session = wire.session(&provider_id)?;
    let mut live = LiveAnswer { active: true, ..LiveAnswer::default() };
    wire.query(&session, &question, busy_policy, |value| {
        anyhow::ensure!(
            value.get("type").and_then(Value::as_str) != Some("permission_request"),
            "The agent requested permission. Open Ask to stop this turn, then retry interactively."
        );
        live.apply(&value);
        Ok(())
    })?;
    anyhow::ensure!(live.permissions.is_empty(), "The answer needs a permission decision in Ask");
    anyhow::ensure!(live.error.is_none(), "{}", live.error.as_deref().unwrap_or_default());
    anyhow::ensure!(!live.text.is_empty(), "The agent returned no answer");
    Ok(Answer { text: live.text, provider_id })
}

#[derive(Debug, Default)]
struct ReadState {
    active: bool,
    again: bool,
}

#[derive(Debug, Default)]
struct ChatState {
    model: Option<Model>,
    revision: u64,
    compose: Compose,
    sources: Vec<(String, String)>,
    pending: Option<Question>,
    posting: bool,
    read: ReadState,
    stopping: bool,
    mutating: bool,
    live: Option<LiveAnswer>,
    retained_answers: HashMap<String, LiveAnswer>,
    problem: Option<String>,
    selected: usize,
    scroll: u16,
}

impl ChatState {
    fn running(&self) -> Option<&str> {
        self.live
            .as_ref()
            .filter(|live| live.active)
            .map(|live| live.turn_id.as_str())
            .or_else(|| self.model.as_ref().and_then(|model| model.running.as_deref()))
    }

    fn turns(&self) -> &[model::Turn] {
        self.model.as_ref().and_then(|m| m.conversation.as_ref()).map_or(&[], |c| c.turns.as_slice())
    }
}

#[derive(Debug, Clone, Copy)]
enum Confirm {
    End,
    Interrupt,
    Discard,
}

#[derive(Debug)]
enum Message {
    Chats(Result<Vec<Chat>>),
    Read(String, u64, Result<Model>),
    Posted(String, Result<Box<Posted>>),
    Session(String, String, String),
    Stream(String, String, Value),
    Finished(String, String, Result<()>),
    Mutation(String, Mutation, Result<Value>),
}

#[derive(Debug)]
enum Mutation {
    Stop,
    Seen,
    End,
    Permission { request_id: String, allow: bool },
}

#[derive(Debug)]
pub(crate) struct Pane {
    current: Option<String>,
    chats: Vec<Chat>,
    states: HashMap<String, ChatState>,
    history: bool,
    history_index: usize,
    editing: bool,
    confirm: Option<Confirm>,
    notice: Option<String>,
    list_read: ReadState,
    restarted: bool,
    server_session: Option<String>,
    tx: Sender<Message>,
    rx: Receiver<Message>,
}

impl Pane {
    pub(crate) fn new(thread_id: Option<String>) -> Self {
        let (tx, rx) = mpsc::channel();
        let history = thread_id.is_none();
        let mut pane = Self {
            current: None,
            chats: Vec::new(),
            states: HashMap::new(),
            history,
            history_index: 0,
            editing: false,
            confirm: None,
            notice: None,
            list_read: ReadState::default(),
            restarted: false,
            server_session: None,
            tx,
            rx,
        };
        if let Some(thread) = thread_id {
            pane.select(thread);
        }
        pane
    }

    fn select(&mut self, thread_id: String) {
        self.states.entry(thread_id.clone()).or_default();
        self.current = Some(thread_id);
        self.history = false;
        self.editing = false;
        self.confirm = None;
    }

    /// Append a quoted passage without replacing an existing question. Source
    /// metadata is only sent while its corresponding quotation remains present.
    pub(crate) fn seed(&mut self, thread_id: String, words: &str, source: Option<&str>) {
        self.select(thread_id);
        if let Some(state) = self.current.as_ref().and_then(|id| self.states.get_mut(id)) {
            let old = state.compose.value();
            state.compose = Compose::with_text(&if old.trim().is_empty() {
                words.to_owned()
            } else {
                format!("{}\n{words}", old.trim_end())
            });
            if let Some(source) = source.filter(|_| !words.trim().is_empty()) {
                state.sources.push((words.trim().to_owned(), source.to_owned()));
            }
        }
        self.editing = true;
    }

    pub(crate) fn paste(&mut self, words: &str) {
        if let Some(state) = self.current.as_ref().and_then(|id| self.states.get_mut(id)) {
            state.compose.insert_text(words);
            self.editing = true;
        }
    }

    pub(crate) fn refresh(&mut self, client: &Client) {
        let wire = match Wire::new(client) {
            Ok(wire) => wire,
            Err(e) => {
                self.notice = Some(format!("{e:#}"));
                return;
            }
        };
        if self.list_read.active {
            self.list_read.again = true;
        } else {
            self.list_read.active = true;
            let tx = self.tx.clone();
            let wire = wire.clone();
            thread::spawn(move || {
                let _ = tx.send(Message::Chats(wire.chats().map(|c| c.chats)));
            });
        }
        if let Some(id) = self.current.clone() {
            self.read_thread(&wire, id);
        }
    }

    fn read_thread(&mut self, wire: &Wire, id: String) {
        let state = self.states.entry(id.clone()).or_default();
        if state.read.active {
            state.read.again = true;
            return;
        }
        state.read.active = true;
        let revision = state.revision;
        let tx = self.tx.clone();
        let wire = wire.clone();
        thread::spawn(move || {
            let result = wire.read(&id);
            let _ = tx.send(Message::Read(id, revision, result));
        });
    }

    pub(crate) fn on_live(&mut self, client: &Client) {
        self.refresh(client);
    }

    pub(crate) fn tick(&mut self, client: &Client) {
        while let Ok(message) = self.rx.try_recv() {
            match message {
                Message::Chats(result) => {
                    self.list_read.active = false;
                    match result {
                        Ok(chats) => {
                            let selected = self.chats.get(self.history_index).map(|c| c.thread_id.clone());
                            self.chats = chats;
                            self.history_index = selected
                                .and_then(|id| self.chats.iter().position(|c| c.thread_id == id))
                                .unwrap_or(self.history_index)
                                .min(self.chats.len().saturating_sub(1));
                        }
                        Err(error) => {
                            self.notice =
                                Some(format!("History read failed; previous history retained: {error:#}"));
                        }
                    }
                    if std::mem::take(&mut self.list_read.again) {
                        self.refresh(client);
                    }
                }
                Message::Read(id, revision, result) => {
                    let state = self.states.entry(id.clone()).or_default();
                    state.read.active = false;
                    match result {
                        Ok(model) if revision == state.revision => {
                            let old_len = state.turns().len();
                            let last_selected = state.selected >= old_len.saturating_sub(1);
                            let len = model.conversation.as_ref().map_or(0, |c| c.turns.len());
                            state.selected = if last_selected {
                                len.saturating_sub(1)
                            } else {
                                state.selected.min(len.saturating_sub(1))
                            };
                            state.model = Some(model);
                        }
                        Ok(_) => state.read.again = true,
                        Err(error) => {
                            state.problem = Some(format!(
                                "Read failed; draft and previous conversation retained: {error:#}"
                            ));
                        }
                    }
                    if std::mem::take(&mut state.read.again)
                        && let Ok(wire) = Wire::new(client)
                    {
                        self.read_thread(&wire, id);
                    }
                }
                Message::Posted(id, result) => self.apply_posted(id, result),
                Message::Session(id, turn_id, session) => {
                    if let Some(live) = self
                        .states
                        .get_mut(&id)
                        .and_then(|s| s.live.as_mut())
                        .filter(|l| l.turn_id == turn_id)
                    {
                        live.ai_session = Some(session);
                    }
                }
                Message::Stream(id, turn_id, value) => {
                    if let Some(live) = self
                        .states
                        .get_mut(&id)
                        .and_then(|s| s.live.as_mut())
                        .filter(|l| l.turn_id == turn_id)
                    {
                        live.apply(&value);
                    }
                }
                Message::Finished(id, turn_id, result) => {
                    if let Some(state) = self.states.get_mut(&id)
                        && let Some(live) = state.live.as_mut().filter(|l| l.turn_id == turn_id)
                    {
                        live.active = false;
                        if let Err(error) = result {
                            state.problem =
                                Some(format!("Answer interrupted: {error:#}. Partial text retained."));
                        }
                    }
                    if let Ok(wire) = Wire::new(client) {
                        self.read_thread(&wire, id);
                    }
                    self.refresh(client);
                }
                Message::Mutation(id, mutation, result) => self.apply_mutation(client, id, mutation, result),
            }
        }
    }

    fn apply_posted(&mut self, id: String, result: Result<Box<Posted>>) {
        let state = self.states.entry(id).or_default();
        state.posting = false;
        match result {
            Ok(posted) => {
                let posted = *posted;
                state.revision = state.revision.wrapping_add(1);
                if let Some(sent) = state.pending.take() {
                    if state.compose.value().trim() == sent.text.trim() {
                        state.compose = Compose::default();
                    }
                    state.sources.retain(|(quote, _)| !sent.text.contains(quote));
                }
                let running = posted.provider_id.is_some() || posted.running;
                let owner_session = posted.turn.session.clone();
                let turn_id = posted.turn.id.clone();
                if let Some(model) = &mut state.model {
                    model.running = running.then(|| turn_id.clone());
                    model.conversation = Some(posted.conversation);
                    model.damaged = false;
                }
                state.selected = state.turns().len().saturating_sub(1);
                state.scroll = u16::MAX;
                if let Some(previous) = state.live.take()
                    && !previous.text.is_empty()
                {
                    state.retained_answers.insert(previous.turn_id.clone(), previous);
                }
                state.live = posted.provider_id.map(|_| LiveAnswer {
                    turn_id,
                    owner_session,
                    active: true,
                    ..LiveAnswer::default()
                });
                if let Some(reason) = posted.reason {
                    state.problem = Some(format!(
                        "Question saved, but no answer started ({reason}). Use a to ask again, or R to reply in the thread."
                    ));
                }
            }
            Err(error) => {
                state.problem = Some(format!(
                    "Send not confirmed: {error:#}. Draft and original turn id retained; a retries, D discards the pending send."
                ));
            }
        }
    }

    fn apply_mutation(&mut self, client: &Client, id: String, mutation: Mutation, result: Result<Value>) {
        let state = self.states.entry(id.clone()).or_default();
        state.mutating = false;
        state.stopping = false;
        match result {
            Ok(_) => match mutation {
                Mutation::End => {
                    state.revision = state.revision.wrapping_add(1);
                    state.retained_answers.clear();
                    if let Some(model) = &mut state.model {
                        model.conversation = None;
                        model.running = None;
                        model.damaged = false;
                    }
                    state.live = None;
                    state.pending = None;
                    state.selected = 0;
                    state.problem = Some("Conversation ended. Unsent words retained.".to_owned());
                }
                Mutation::Permission { request_id, allow } => {
                    if let Some(p) = state
                        .live
                        .as_mut()
                        .and_then(|l| l.permissions.iter_mut().find(|p| p.request_id == request_id))
                    {
                        p.pending = false;
                        p.decided = Some(allow);
                    }
                }
                Mutation::Stop | Mutation::Seen => {}
            },
            Err(error) => {
                if let Mutation::Permission { request_id, .. } = mutation
                    && let Some(p) = state
                        .live
                        .as_mut()
                        .and_then(|l| l.permissions.iter_mut().find(|p| p.request_id == request_id))
                {
                    p.pending = false;
                }
                state.problem = Some(format!("Action failed; conversation retained: {error:#}"));
            }
        }
        if let Ok(wire) = Wire::new(client) {
            self.read_thread(&wire, id);
        }
        self.refresh(client);
    }

    fn send(&mut self, client: &Client, retry: bool, policy: Option<&str>) {
        if self.restarted {
            self.notice =
                Some("Inbox restarted. Close and reopen Ask to reconnect; drafts are retained.".to_owned());
            return;
        }
        let Some(id) = self.current.clone() else { return };
        let state = self.states.entry(id.clone()).or_default();
        if state.posting || state.running().is_some() || state.mutating {
            return;
        }
        if state.model.is_none() {
            state.problem = Some("Read the conversation before sending".to_owned());
            return;
        }
        let question = if let Some(pending) = &state.pending {
            if !retry {
                state.problem = Some(
                    "An unconfirmed send is retained. a retries it unchanged; D discards it.".to_owned(),
                );
                return;
            }
            pending.clone()
        } else if retry {
            let Some(turn) = state.turns().get(state.selected) else { return };
            if turn.answer.is_some() || turn.stopped {
                state.problem = Some("This turn already finished. Write a new question instead.".to_owned());
                return;
            }
            Question::from_turn(turn)
        } else {
            let words = state.compose.value().trim().to_owned();
            let mut lines: Vec<&str> = Vec::new();
            for (quote, source) in &state.sources {
                if words.contains(quote) && !lines.contains(&source.as_str()) {
                    lines.push(source);
                }
            }
            let source = (!lines.is_empty()).then(|| lines.join("\n"));
            Question::plain(turn_id(), words, source)
        };
        if let Err(error) = question.validate() {
            state.problem = Some(error.to_string());
            return;
        }
        let wire = match Wire::new(client) {
            Ok(wire) => wire,
            Err(e) => {
                state.problem = Some(e.to_string());
                return;
            }
        };
        let shown_owner = state.live.as_ref().map(|l| l.owner_session.clone());
        state.pending = Some(question.clone());
        state.posting = true;
        state.revision = state.revision.wrapping_add(1);
        state.problem = None;
        let tx = self.tx.clone();
        let policy = policy.map(str::to_owned);
        thread::spawn(move || {
            let posted = match wire.send(&id, &question) {
                Ok(posted) => posted,
                Err(error) => {
                    let _ = tx.send(Message::Posted(id, Err(error)));
                    return;
                }
            };
            // A wait/interrupt choice applies to the agent the person saw, not a
            // new owner a hand-off may have assigned in the meantime.
            let policy = policy.filter(|_| shown_owner.as_deref() == Some(posted.turn.session.as_str()));
            let provider = posted.provider_id.clone();
            let _ = tx.send(Message::Posted(id.clone(), Ok(Box::new(posted))));
            let Some(provider) = provider else { return };
            let result = (|| {
                let session = wire.session(&provider)?;
                tx.send(Message::Session(id.clone(), question.turn_id.clone(), session.clone()))?;
                wire.query(&session, &question, policy.as_deref(), |value| {
                    tx.send(Message::Stream(id.clone(), question.turn_id.clone(), value))
                        .context("Ask pane closed")
                })
            })();
            let _ = tx.send(Message::Finished(id, question.turn_id, result));
        });
    }

    fn mutate(&mut self, client: &Client, mutation: Mutation) {
        if self.restarted {
            self.notice = Some("Inbox restarted. Reopen Ask before taking an action.".to_owned());
            return;
        }
        let Some(id) = self.current.clone() else { return };
        let state = self.states.entry(id.clone()).or_default();
        if state.mutating || state.posting {
            return;
        }
        let request: Result<(String, Value)> = (|| match &mutation {
            Mutation::Stop => {
                let running = state.running().context("No answer is running")?;
                Ok((thread_path(&id, "/stop")?, json!({"turn_id":running})))
            }
            Mutation::End => Ok((thread_path(&id, "/end")?, json!({}))),
            Mutation::Seen => {
                let seen = newest_answer(state.model.as_ref().and_then(|m| m.conversation.as_ref()))
                    .context("There is no answer to mark seen")?;
                Ok((thread_path(&id, "/seen")?, json!({"seen_at":seen})))
            }
            Mutation::Permission { request_id, allow } => {
                let session = state
                    .live
                    .as_ref()
                    .and_then(|l| l.ai_session.as_ref())
                    .context("No live AI session for this permission")?;
                Ok((
                    "/api/ai/permission".to_owned(),
                    json!({"sessionId":session,"requestId":request_id,"allow":allow}),
                ))
            }
        })();
        let (path, body) = match request {
            Ok(v) => v,
            Err(e) => {
                state.problem = Some(e.to_string());
                return;
            }
        };
        let wire = match Wire::new(client) {
            Ok(wire) => wire,
            Err(e) => {
                state.problem = Some(e.to_string());
                return;
            }
        };
        state.mutating = true;
        state.revision = state.revision.wrapping_add(1);
        state.stopping = matches!(mutation, Mutation::Stop);
        if let Mutation::Permission { request_id, .. } = &mutation
            && let Some(p) = state
                .live
                .as_mut()
                .and_then(|l| l.permissions.iter_mut().find(|p| p.request_id == *request_id))
        {
            p.pending = true;
        }
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = wire.post(&path, body);
            let _ = tx.send(Message::Mutation(id, mutation, result));
        });
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent, client: &Client) -> Option<Action> {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        if let Some(confirm) = self.confirm.take() {
            if matches!(key.code, KeyCode::Char('y' | 'Y')) {
                match confirm {
                    Confirm::End => self.mutate(client, Mutation::End),
                    Confirm::Interrupt => self.send(client, true, Some("interrupt")),
                    Confirm::Discard => {
                        if let Some(state) = self.current.as_ref().and_then(|id| self.states.get_mut(id)) {
                            state.pending = None;
                            state.problem = Some("Pending send discarded locally. A previously accepted turn may still appear in history.".to_owned());
                        }
                    }
                }
            }
            return None;
        }
        if self.editing {
            let action = self
                .current
                .as_ref()
                .and_then(|id| self.states.get_mut(id))
                .map(|s| s.compose.handle_key(key));
            match action {
                Some(ComposeAction::Cancel) => self.editing = false,
                Some(ComposeAction::Save) => {
                    self.editing = false;
                    self.send(client, false, None);
                }
                _ => {}
            }
            return None;
        }
        if self.history
            && !matches!(
                key.code,
                KeyCode::Esc
                    | KeyCode::Tab
                    | KeyCode::Enter
                    | KeyCode::Down
                    | KeyCode::Up
                    | KeyCode::Char('q' | 'h' | 'r' | 'i' | 'j' | 'k')
            )
        {
            return None;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Some(Action::Close),
            KeyCode::Char('h') | KeyCode::Tab => {
                self.history = !self.history;
                self.refresh(client);
            }
            KeyCode::Char('r') => self.refresh(client),
            KeyCode::Char('i') if self.current.is_some() => {
                self.history = false;
                self.editing = true;
            }
            KeyCode::Enter if self.history => {
                if let Some(chat) = self.chats.get(self.history_index) {
                    self.select(chat.thread_id.clone());
                    self.refresh(client);
                }
            }
            KeyCode::Down | KeyCode::Char('j') if self.history => {
                self.history_index = (self.history_index + 1).min(self.chats.len().saturating_sub(1));
            }
            KeyCode::Up | KeyCode::Char('k') if self.history => {
                self.history_index = self.history_index.saturating_sub(1);
            }
            _ => return self.chat_key(key.code, client),
        }
        None
    }

    fn chat_key(&mut self, code: KeyCode, client: &Client) -> Option<Action> {
        match code {
            KeyCode::Char('a') => self.send(client, true, None),
            KeyCode::Char('w') => {
                if self.busy_choice() {
                    self.send(client, true, Some("wait"));
                }
            }
            KeyCode::Char('I') => {
                if self.busy_choice() {
                    self.confirm = Some(Confirm::Interrupt);
                }
            }
            KeyCode::Char('s') => self.mutate(client, Mutation::Stop),
            KeyCode::Char('m') => self.mutate(client, Mutation::Seen),
            KeyCode::Char('X') => self.confirm = Some(Confirm::End),
            KeyCode::Char('D') => self.confirm = Some(Confirm::Discard),
            KeyCode::Char('y' | 'n') => {
                if let Some(permission) = self
                    .current
                    .as_ref()
                    .and_then(|id| self.states.get(id))
                    .and_then(|s| s.live.as_ref())
                    .and_then(|l| l.permissions.iter().find(|p| p.decided.is_none() && !p.pending))
                {
                    self.mutate(
                        client,
                        Mutation::Permission {
                            request_id: permission.request_id.clone(),
                            allow: code == KeyCode::Char('y'),
                        },
                    );
                }
            }
            KeyCode::Char('R') => {
                if let Some(id) = &self.current
                    && let Some(turn) = self.states.get(id).and_then(|s| s.turns().get(s.selected))
                {
                    return Some(Action::Reply { thread_id: id.clone(), words: shown_question(turn) });
                }
            }
            KeyCode::Char('o') => {
                if let Some(id) = &self.current
                    && let Some(shape_id) = self
                        .states
                        .get(id)
                        .and_then(|s| s.turns().get(s.selected))
                        .and_then(|t| t.shape.as_ref())
                {
                    return Some(Action::OpenShape { thread_id: id.clone(), shape_id: shape_id.clone() });
                }
            }
            _ => {
                if let Some(state) = self.current.as_ref().and_then(|id| self.states.get_mut(id)) {
                    match code {
                        KeyCode::Char(']') => {
                            state.selected = (state.selected + 1).min(state.turns().len().saturating_sub(1));
                        }
                        KeyCode::Char('[') => state.selected = state.selected.saturating_sub(1),
                        KeyCode::Down | KeyCode::Char('j') => state.scroll = state.scroll.saturating_add(1),
                        KeyCode::Up | KeyCode::Char('k') => state.scroll = state.scroll.saturating_sub(1),
                        KeyCode::PageDown => state.scroll = state.scroll.saturating_add(12),
                        KeyCode::PageUp => state.scroll = state.scroll.saturating_sub(12),
                        KeyCode::Home => state.scroll = 0,
                        KeyCode::End => state.scroll = u16::MAX,
                        _ => {}
                    }
                }
            }
        }
        None
    }

    fn busy_choice(&self) -> bool {
        self.current.as_ref().and_then(|id| self.states.get(id)).is_some_and(|state| {
            state.live.as_ref().is_some_and(|live| {
                live.error_code.as_deref() == Some("agent_busy")
                    && state.turns().get(state.selected).is_some_and(|turn| turn.id == live.turn_id)
            })
        })
    }
}

fn newest_answer(conversation: Option<&Conversation>) -> Option<&str> {
    conversation?.turns.iter().filter_map(|turn| turn.answered_at.as_deref()).max()
}

fn turn_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("ask_{now:x}_{:x}_{:x}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed))
}

thread_local! { static KEPT: RefCell<Option<Pane>> = const { RefCell::new(None) }; }

/// Hand a quotation or transcribed text to Ask without sending it.
pub(crate) fn seed(thread_id: &str, words: &str, source: Option<&str>) {
    KEPT.with(|kept| {
        let mut kept = kept.borrow_mut();
        let pane = kept.get_or_insert_with(|| Pane::new(Some(thread_id.to_owned())));
        pane.seed(thread_id.to_owned(), words, source);
    });
}

/// Stage the canonical Show me request. The caller constructs `question` with
/// the upstream `inboxShowMeRequest` contract; `a` is the user's explicit Send.
pub(crate) fn seed_show_me(
    thread_id: &str,
    question: &str,
    target: Value,
    again_shape: Option<String>,
) -> Result<()> {
    thread_path(thread_id, "")?;
    let mut question = Question::plain(turn_id(), question.to_owned(), None);
    question.kind = Some("show_me".to_owned());
    question.show_me = Some(target);
    question.again_shape = again_shape;
    question.validate()?;
    KEPT.with(|kept| {
        let mut kept = kept.borrow_mut();
        let pane = kept.get_or_insert_with(|| Pane::new(Some(thread_id.to_owned())));
        pane.select(thread_id.to_owned());
        let state = pane.states.entry(thread_id.to_owned()).or_default();
        anyhow::ensure!(
            state.pending.is_none() && !state.posting,
            "Ask already has a retained send; retry or discard it before staging Show me"
        );
        state.pending = Some(question);
        state.problem = Some(
            "Show me request staged. a sends it; D cancels the staged request. Your draft is unchanged."
                .to_owned(),
        );
        Ok(())
    })
}

/// Run on an existing terminal. The in-process cache is the terminal equivalent
/// of the window's sessionStorage: leaving Ask does not discard unsent words.
pub(crate) fn run(
    client: &Client,
    terminal: &mut ratatui::DefaultTerminal,
    thread_id: Option<&str>,
) -> Result<Option<Action>> {
    let mut pane =
        KEPT.with(|kept| kept.borrow_mut().take()).unwrap_or_else(|| Pane::new(thread_id.map(str::to_owned)));
    if pane.server_session.as_deref().is_some_and(|session| session != client.server_session()) {
        // Drop callbacks from the old server without discarding the person's
        // words or retry ids. Their status must be read from the new instance.
        let (tx, rx) = mpsc::channel();
        pane.tx = tx;
        pane.rx = rx;
        pane.list_read = ReadState::default();
        for state in pane.states.values_mut() {
            state.read.active = false;
            state.posting = false;
            state.mutating = false;
            state.stopping = false;
            if let Some(live) = &mut state.live {
                live.active = false;
                live.ai_session = None;
            }
            if let Some(model) = &mut state.model {
                model.running = None;
            }
        }
    }
    pane.server_session = Some(client.server_session().to_owned());
    if let Some(id) = thread_id {
        pane.select(id.to_owned());
    } else {
        pane.history = true;
    }
    pane.restarted = false;
    pane.notice = None;
    pane.refresh(client);
    let live = client.subscribe(0);
    let result = (|| {
        loop {
            pane.tick(client);
            if pane.restarted {
                pane.editing = false;
            }
            let mut refresh = false;
            while let Ok(event) = live.try_recv() {
                match event {
                    Live::Restarted { .. } => {
                        pane.restarted = true;
                        pane.notice = Some(
                            "Inbox restarted: close and reopen Ask. Drafts and uncertain sends are retained."
                                .to_owned(),
                        );
                    }
                    Live::Lost(error) => {
                        pane.notice = Some(format!(
                            "Live connection lost: {error}. Press r to refresh; retained text is unchanged."
                        ));
                    }
                    Live::Refresh | Live::Record { .. } | Live::Connected { .. } => refresh = true,
                }
            }
            if refresh && !pane.restarted {
                pane.on_live(client);
            }
            terminal.draw(|frame| {
                let area = frame.area();
                pane.draw(frame, area);
            })?;
            if !event::poll(Duration::from_millis(60))? {
                continue;
            }
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                        return Ok(None);
                    }
                    if let Some(action) = pane.handle_key(key, client) {
                        return Ok(if matches!(action, Action::Close) { None } else { Some(action) });
                    }
                }
                Event::Paste(text) => pane.paste(&text),
                _ => {}
            }
        }
    })();
    KEPT.with(|kept| *kept.borrow_mut() = Some(pane));
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeding_preserves_drafts_across_threads() {
        let mut pane = Pane::new(Some("thread_a".to_owned()));
        pane.paste("existing");
        pane.seed("thread_b".to_owned(), "> selected\n\n", Some("[b.rs]"));
        pane.select("thread_a".to_owned());
        assert_eq!(pane.states.get("thread_a").map(|s| s.compose.value()).as_deref(), Some("existing"));
        pane.seed("thread_a".to_owned(), "more", None);
        assert_eq!(pane.states.get("thread_a").map(|s| s.compose.value()).as_deref(), Some("existing\nmore"));
    }

    #[test]
    fn generated_turn_ids_meet_server_contract() -> Result<()> {
        let first = turn_id();
        let second = turn_id();
        assert_ne!(first, second);
        Question::plain(first, "Question".to_owned(), None).validate()
    }
}
