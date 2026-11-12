//! Terminal Board, matching the Inbox's cards, initiatives and agent-owned checklists.
//!
//! Integration: `refresh` and `draw` are read-only. Route keys to `handle_key`, paste to
//! `paste`, and refresh on card/initiative/steps records. The parent handles `OpenThread`
//! and `Compose`; after compose succeeds, call `composed` with its returned thread id.
//! `s` opens the shared agent/launch composer; `c` creates a linked conversation without
//! moving the card. `b` starts work and `a` asks for an update from its live agent.
//!
//! Navigation: arrows/j/k, Enter opens, Esc backs out. `1`–`6` selects a lane, `0` all;
//! `p`/`f` filter project/initiative; `/` finds titles/notes. `n` creates, `m` moves,
//! `e` renames, `N` edits notes, `l`/`L` links a URL/thread, `i` assigns an initiative,
//! `I` manages initiatives. `d` cancels with confirmation; `u` restores. `t` opens a
//! linked thread; `o` opens a linked URL; `v` marks the displayed Board read; `V` marks
//! shown agent cancellations seen; `M` refreshes forge marks (may auto-move merged work
//! to Done, as upstream does). `T` previews and sends scoped Tidy requests. `R` refreshes.
//! `?` shows all keys. Forms retain their text
//! on errors; Tab selects fields, arrows select choices, Enter submits, Esc cancels.

mod api;
mod detail;
mod draw;
mod forms;
mod input;
mod modal;
mod model;
#[cfg(test)]
mod tests;
mod tidy;

use std::collections::BTreeMap;

use anyhow::Result;
use serde_json::Value;

use super::client::Client;
use model::{Card, CardDetail, CardsModel, Column, Project, ResolvedRef, initiative_of};

pub(crate) enum BoardAction {
    Close,
    OpenThread(String),
    Compose { card_id: String, body: String, send: bool },
}

#[derive(Default)]
pub(crate) struct Board {
    model: CardsModel,
    projects: Vec<Project>,
    visible: Vec<usize>,
    cursor: usize,
    scroll: usize,
    height: usize,
    column: Option<Column>,
    project: Option<String>,
    /// None means every initiative; "none" means unassigned.
    initiative: Option<String>,
    query: String,
    opened: Option<CardDetail>,
    refs: Vec<ResolvedRef>,
    detail_scroll: u16,
    marks: BTreeMap<String, Value>,
    modal: Option<forms::Modal>,
    pending: Option<api::Delivery>,
    pending_link: Option<(String, String, bool)>,
    status: Option<String>,
    help: bool,
    detail_rows: Vec<ratatui::text::Line<'static>>,
    detail_width: u16,
}

impl Board {
    /// A fresh read never marks cards seen or invokes the write-capable marks endpoint.
    pub(crate) fn refresh(&mut self, client: &Client) -> Result<()> {
        let model = client.board_cards()?;
        let keep = self.selected().map(|card| card.id.clone());
        self.model = model;
        self.projects = client.board_projects()?;
        self.rebuild(keep.as_deref());
        if let Some(id) = self.opened.as_ref().map(|detail| detail.card.id.clone()) {
            self.open_card(client, &id)?;
        }
        Ok(())
    }

    fn selected(&self) -> Option<&Card> {
        if let Some(detail) = &self.opened {
            return Some(&detail.card);
        }
        self.visible.get(self.cursor).and_then(|index| self.model.cards.get(*index))
    }

    fn rebuild(&mut self, keep: Option<&str>) {
        let query = self.query.to_lowercase();
        self.visible = self
            .model
            .cards
            .iter()
            .enumerate()
            .filter(|(_, card)| {
                self.column.is_none_or(|column| card.column == column)
                    && self.project.as_deref().is_none_or(|id| card.project_id == id)
                    && self.initiative.as_deref().is_none_or(|id| {
                        let actual = initiative_of(card, &self.model.initiatives);
                        if id == "none" { actual.is_none() } else { actual == Some(id) }
                    })
                    && (query.is_empty()
                        || card.title.to_lowercase().contains(&query)
                        || card.notes.as_deref().is_some_and(|text| text.to_lowercase().contains(&query)))
            })
            .map(|(index, _)| index)
            .collect();
        self.cursor = keep
            .and_then(|id| {
                self.visible
                    .iter()
                    .position(|index| self.model.cards.get(*index).is_some_and(|card| card.id == id))
            })
            .unwrap_or(self.cursor)
            .min(self.visible.len().saturating_sub(1));
        self.scroll = self.scroll.min(self.cursor);
    }

    fn open_card(&mut self, client: &Client, id: &str) -> Result<()> {
        let detail = client.board_card(id)?;
        let refs = if detail.card.references().is_empty() {
            Vec::new()
        } else {
            match client.board_refs(id) {
                Ok(refs) => refs,
                Err(error) => {
                    self.status = Some(format!("Card opened; link states unavailable: {error:#}"));
                    Vec::new()
                }
            }
        };
        self.opened = Some(detail);
        self.refs = refs;
        self.detail_rows.clear();
        self.detail_width = 0;
        Ok(())
    }

    pub(crate) fn failed(&mut self, error: &str) {
        self.status = Some(error.to_owned());
    }

    pub(crate) fn cursor(&self) -> u64 {
        self.model.cursor
    }

    pub(crate) fn paste(&mut self, text: &str) {
        if let Some(forms::Modal::Form(form)) = self.modal.as_mut() {
            form.paste(text);
        }
    }

    /// Link a successful shared composer result. A failed second step is retained for `!`
    /// to retry; the person must not send a duplicate message just to finish the linkage.
    pub(crate) fn composed(
        &mut self,
        client: &Client,
        card_id: &str,
        thread_id: &str,
        send: bool,
    ) -> Result<()> {
        self.pending_link = Some((card_id.to_owned(), thread_id.to_owned(), send));
        self.finish_link(client)
    }

    fn finish_link(&mut self, client: &Client) -> Result<()> {
        let Some((card, thread, send)) = &self.pending_link else {
            return Ok(());
        };
        client.board_change(
            card,
            &serde_json::json!({"op": if *send {"send"} else {"link"}, "thread_id": thread}),
        )?;
        self.pending_link = None;
        self.status = Some("Thread linked to card.".to_owned());
        self.refresh(client)
    }

    fn project_name(&self, id: &str) -> &str {
        self.projects
            .iter()
            .find(|project| project.id == id)
            .map_or("Unknown project", |project| project.name.as_str())
    }
}
