//! A thread on screen: each message's header, its Markdown body as sent, and after it one
//! answer card per question the server parsed out of it. Bodies are rendered once per
//! width; cards are cheap and rebuilt whenever a pick or the focus changes.

use std::collections::{HashMap, HashSet};

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use time::OffsetDateTime;

use super::card::{self, Card, Shown};
use super::client::{Message, ThreadModel};
use super::format::{ago, author};
use super::questions::{Answer, Kind, Question, State};
use crate::doc::Document;
use crate::layout::DocLayout;
use crate::theme::palette;

/// The answer card under the keyboard: which card, and which of its rows is highlighted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Focus {
    pub(crate) card: usize,
    pub(crate) choice: usize,
}

#[derive(Debug)]
pub(crate) struct ThreadView {
    pub(crate) model: ThreadModel,
    width: usize,
    bodies: HashMap<String, Vec<Line<'static>>>,
    lines: Vec<Line<'static>>,
    cards: Vec<Card>,
    pub(crate) scroll: usize,
    focus: Option<Focus>,
    /// Picks the person made here, by `message id/key`, until the server has them.
    drafts: HashMap<String, Answer>,
}

fn draft_key(message_id: &str, key: &str) -> String {
    format!("{message_id}/{key}")
}

/// A message's questions in the server's order, action items last.
fn ordered(message: &Message) -> Vec<&Question> {
    let mut questions: Vec<&Question> = message.questions.iter().collect();
    questions.sort_by_key(|q| (q.kind == Kind::Action, q.position));
    questions
}

impl ThreadView {
    pub(crate) fn new(model: ThreadModel, width: usize) -> Self {
        let mut view = Self {
            model,
            width: width.max(1),
            bodies: HashMap::new(),
            lines: Vec::new(),
            cards: Vec::new(),
            scroll: 0,
            focus: None,
            drafts: HashMap::new(),
        };
        view.relayout();
        view
    }

    pub(crate) fn id(&self) -> &str {
        &self.model.thread.thread_id
    }

    pub(crate) fn lines(&self) -> &[Line<'static>] {
        &self.lines
    }

    pub(crate) fn focus(&self) -> Option<Focus> {
        self.focus
    }

    /// The same thread, read again: scroll, focus and unsaved picks survive, and a body
    /// that did not change keeps its rendering.
    pub(crate) fn replace(&mut self, model: ThreadModel) {
        let focused =
            self.focus.and_then(|f| self.cards.get(f.card)).map(|c| (c.message_id.clone(), c.key.clone()));
        let old = &self.model.thread.messages;
        let unchanged: HashSet<String> = model
            .thread
            .messages
            .iter()
            .filter(|m| old.iter().any(|was| was.id == m.id && was.body == m.body))
            .map(|m| m.id.clone())
            .collect();
        self.bodies.retain(|id, _| unchanged.contains(id));
        self.model = model;
        self.relayout();
        let focus = focused
            .and_then(|(message_id, key)| {
                self.cards.iter().position(|c| c.message_id == message_id && c.key == key)
            })
            .map(|card| Focus { card, choice: self.focus.map_or(0, |f| f.choice) });
        if focus != self.focus {
            self.focus = focus;
            self.relayout();
        }
        self.scroll = self.scroll.min(self.lines.len().saturating_sub(1));
    }

    pub(crate) fn reflow(&mut self, width: usize) {
        let width = width.max(1);
        if width != self.width {
            self.width = width;
            self.bodies.clear();
            self.relayout();
        }
    }

    /// A pick the server now holds: the message's questions as it answered, draft dropped.
    pub(crate) fn apply_questions(&mut self, message_id: &str, questions: Vec<Question>) {
        if let Some(message) = self.model.thread.messages.iter_mut().find(|m| m.id == message_id) {
            message.questions = questions;
        }
        let prefix = format!("{message_id}/");
        self.drafts.retain(|key, _| !key.starts_with(&prefix));
        self.relayout();
    }

    pub(crate) fn relayout(&mut self) {
        let now = OffsetDateTime::now_utc();
        let width = self.width;
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut cards: Vec<Card> = Vec::new();
        for (index, message) in self.model.thread.messages.iter().enumerate() {
            if index > 0 {
                lines.push(Line::default());
            }
            lines.push(message_header(message, now, width));
            let body =
                self.bodies.entry(message.id.clone()).or_insert_with(|| render_body(&message.body, width));
            if !body.is_empty() {
                lines.push(Line::default());
                lines.extend(body.iter().cloned());
            }
            for question in ordered(message) {
                let answer = self
                    .drafts
                    .get(&draft_key(&message.id, &question.key))
                    .cloned()
                    .or_else(|| question.answer.clone())
                    .unwrap_or_default();
                let number = cards.len() + 1;
                let shown = Shown {
                    number,
                    answer: &answer,
                    drafted: self.drafts.contains_key(&draft_key(&message.id, &question.key)),
                    focus: self.focus.filter(|f| f.card + 1 == number).map(|f| f.choice),
                };
                let (card_lines, rows) = card::lines(question, &shown, width);
                lines.push(Line::default());
                let first = lines.len();
                lines.extend(card_lines);
                cards.push(Card {
                    message_id: message.id.clone(),
                    key: question.key.clone(),
                    first,
                    last: lines.len().saturating_sub(1),
                    rows: rows.into_iter().map(|r| r + first).collect(),
                    answerable: card::answerable(question),
                });
            }
        }
        self.lines = lines;
        self.cards = cards;
        if let Some(focus) = self.focus
            && focus.card >= self.cards.len()
        {
            self.focus = None;
        }
    }

    pub(crate) fn scroll_by(&mut self, delta: i64) {
        let last = self.lines.len().saturating_sub(1) as i64;
        self.scroll = (self.scroll as i64 + delta).clamp(0, last) as usize;
    }

    pub(crate) fn scroll_to(&mut self, row: usize) {
        self.scroll = row.min(self.lines.len().saturating_sub(1));
    }

    /// Keep the focused card on screen, `height` rows tall: whole when it fits, else its
    /// highlighted row.
    pub(crate) fn ensure_visible(&mut self, height: usize) {
        let height = height.max(1);
        let Some(focus) = self.focus else { return };
        let Some(card) = self.cards.get(focus.card) else { return };
        let row = card.rows.get(focus.choice).copied().unwrap_or(card.first);
        let fits = card.last - card.first < height;
        let (top, bottom) = if fits { (card.first, card.last) } else { (row, row) };
        if top < self.scroll {
            self.scroll = top;
        } else if bottom >= self.scroll + height {
            self.scroll = bottom + 1 - height;
        }
    }

    /// Focus the next (or previous) card that can still be answered, wrapping around.
    pub(crate) fn focus_step(&mut self, forward: bool) {
        let answerable: Vec<usize> =
            self.cards.iter().enumerate().filter(|(_, c)| c.answerable).map(|(i, _)| i).collect();
        if answerable.is_empty() {
            return;
        }
        let on_screen = |i: &usize| self.cards.get(*i).is_some_and(|c| c.last >= self.scroll);
        let next = match self.focus {
            Some(focus) if forward => {
                answerable.iter().copied().find(|&i| i > focus.card).or(answerable.first().copied())
            }
            Some(focus) => {
                answerable.iter().rev().copied().find(|&i| i < focus.card).or(answerable.last().copied())
            }
            None if forward => answerable.iter().copied().find(on_screen).or(answerable.first().copied()),
            None => answerable.last().copied(),
        };
        self.focus = next.map(|card| Focus { card, choice: 0 });
        self.relayout();
    }

    pub(crate) fn unfocus(&mut self) {
        if self.focus.take().is_some() {
            self.relayout();
        }
    }

    pub(crate) fn move_choice(&mut self, delta: i64) {
        let Some(mut focus) = self.focus else { return };
        let Some(card) = self.cards.get(focus.card) else { return };
        let last = card.rows.len().saturating_sub(1) as i64;
        focus.choice = (focus.choice as i64 + delta).clamp(0, last) as usize;
        self.focus = Some(focus);
        self.relayout();
    }

    /// Land the highlight on row `choice` of the focused card; false when it has none.
    pub(crate) fn set_choice(&mut self, choice: usize) -> bool {
        let Some(mut focus) = self.focus else { return false };
        if self.cards.get(focus.card).is_none_or(|card| choice >= card.rows.len()) {
            return false;
        }
        focus.choice = choice;
        self.focus = Some(focus);
        self.relayout();
        true
    }

    /// The focused question, with the message it is on.
    pub(crate) fn focused(&self) -> Option<(&Message, &Question)> {
        let card = self.cards.get(self.focus?.card)?;
        let message = self.model.thread.messages.iter().find(|m| m.id == card.message_id)?;
        let question = message.questions.iter().find(|q| q.key == card.key)?;
        Some((message, question))
    }

    /// The draft on `message_id/key`, when the person edited it here.
    pub(crate) fn answer_on(&self, message_id: &str, key: &str) -> Option<Answer> {
        self.drafts.get(&draft_key(message_id, key)).cloned()
    }

    /// The answer the focused card shows: the draft, else the server's, else empty.
    pub(crate) fn focused_answer(&self) -> Option<Answer> {
        let (message, question) = self.focused()?;
        Some(
            self.drafts
                .get(&draft_key(&message.id, &question.key))
                .cloned()
                .or_else(|| question.answer.clone())
                .unwrap_or_default(),
        )
    }

    /// Keep an edited answer as the card's draft until the server confirms it.
    pub(crate) fn draft(&mut self, message_id: &str, key: &str, answer: Answer) {
        self.drafts.insert(draft_key(message_id, key), answer);
        self.relayout();
    }

    pub(crate) fn has_picks(&self) -> bool {
        self.model.thread.messages.iter().flat_map(|m| &m.questions).any(|q| q.state == State::Picked)
    }

    pub(crate) fn open_questions(&self) -> usize {
        self.cards.iter().filter(|c| c.answerable).count()
    }
}

fn message_header(message: &Message, now: OffsetDateTime, width: usize) -> Line<'static> {
    let who = author(&message.author);
    let color = if message.author.kind == "person" { palette().comment } else { palette().accent };
    let label = format!("── {who} · {} ", ago(&message.created_at, now));
    let fill = "─".repeat(width.saturating_sub(label.chars().count()));
    Line::from(vec![
        Span::styled(label, Style::new().fg(color).add_modifier(Modifier::BOLD)),
        Span::styled(fill, Style::new().fg(color).dim()),
    ])
}

/// The body as sent, rendered through tui-markdown and wrapped to `width`.
fn render_body(body: &str, width: usize) -> Vec<Line<'static>> {
    if body.trim().is_empty() {
        return Vec::new();
    }
    let doc = Document::parse(body.to_owned());
    let layout = DocLayout::build(&doc, width);
    let mut lines = Vec::new();
    for (index, block) in layout.blocks.iter().enumerate() {
        if index > 0 {
            lines.push(Line::default());
        }
        lines.extend(block.rows.iter().map(|row| row.line.clone()));
    }
    lines
}
