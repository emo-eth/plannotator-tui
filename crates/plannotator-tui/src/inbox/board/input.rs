use anyhow::{Result, ensure};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;

use super::super::client::Client;
use super::api::Delivery;
use super::forms::{Choice, Form, FormKind, Modal, Picker, card_form};
use super::model::{Card, Column};
use super::{Board, BoardAction};

impl Board {
    pub(crate) fn handle_key(&mut self, key: KeyEvent, client: &Client) -> Result<Option<BoardAction>> {
        let result = self.key(key, client);
        if let Err(error) = &result {
            self.status = Some(format!("{error:#}"));
        }
        result
    }

    fn key(&mut self, key: KeyEvent, client: &Client) -> Result<Option<BoardAction>> {
        if self.modal.is_some() {
            return self.modal_key(key, client);
        }
        if self.help {
            self.help = false;
            return Ok(None);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(Some(BoardAction::Close));
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                if self.opened.take().is_none() {
                    return Ok(Some(BoardAction::Close));
                }
                self.detail_scroll = 0;
            }
            KeyCode::Down | KeyCode::Char('j') => self.navigate(1),
            KeyCode::Up | KeyCode::Char('k') => self.navigate(-1),
            KeyCode::PageDown => self.navigate(self.height.max(1) as isize),
            KeyCode::PageUp => self.navigate(-(self.height.max(1) as isize)),
            KeyCode::Home => {
                self.cursor = 0;
                self.detail_scroll = 0;
            }
            KeyCode::End => {
                self.cursor = self.visible.len().saturating_sub(1);
                if self.opened.is_some() {
                    self.detail_scroll = u16::MAX;
                }
            }
            KeyCode::Enter if self.opened.is_none() => {
                if let Some(id) = self.selected().map(|card| card.id.clone()) {
                    self.open_card(client, &id)?;
                    self.detail_scroll = 0;
                }
            }
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char('R') => {
                self.refresh(client)?;
                self.status = Some("Board refreshed.".to_owned());
            }
            KeyCode::Char('!') => {
                if self.pending_link.is_some() {
                    self.finish_link(client)?;
                }
                if let Some(delivery) = self.pending.as_mut() {
                    delivery.perform(client)?;
                    self.pending = None;
                    self.status = Some("Agent message confirmed.".to_owned());
                    self.refresh(client)?;
                }
            }
            KeyCode::Char(digit @ '0'..='6') if self.opened.is_none() => {
                self.column = digit
                    .to_digit(10)
                    .and_then(|index| index.checked_sub(1))
                    .and_then(|index| Column::ALL.get(index as usize))
                    .copied();
                self.cursor = 0;
                self.rebuild(None);
            }
            KeyCode::Char('p') if self.opened.is_none() => {
                let items = std::iter::once(("Every project".to_owned(), Choice::Project(None)))
                    .chain(self.projects.iter().map(|project| {
                        (
                            format!("{} · {}", project.name, project.root),
                            Choice::Project(Some(project.id.clone())),
                        )
                    }))
                    .collect();
                self.pick("Project filter", items);
            }
            KeyCode::Char('f') if self.opened.is_none() => {
                let items = [
                    ("Every initiative".to_owned(), Choice::Filter(None)),
                    ("No initiative".to_owned(), Choice::Filter(Some("none".to_owned()))),
                ]
                .into_iter()
                .chain(
                    self.model
                        .initiatives
                        .iter()
                        .filter(|item| item.deleted_at.is_none())
                        .map(|item| (item.name.clone(), Choice::Filter(Some(item.id.clone())))),
                )
                .collect();
                self.pick("Initiative filter", items);
            }
            KeyCode::Char('/') if self.opened.is_none() => {
                self.modal = Some(Modal::Form(Form::single(
                    "Find cards · empty clears",
                    FormKind::Search,
                    "Title / notes",
                    &self.query,
                )));
            }
            KeyCode::Char('n') => {
                self.modal = Some(Modal::Form(Form::create(
                    &self.projects,
                    &self.model.initiatives,
                    self.project.as_deref(),
                    self.initiative.as_deref(),
                )));
            }
            KeyCode::Char('I') => self.initiatives(),
            KeyCode::Char('T') => self.open_tidy(client)?,
            KeyCode::Char('v') => {
                let response =
                    client.board_post("/api/inbox/cards/seen", json!({"through_seq":self.model.cursor}))?;
                self.model.seen_seq = response.get("seen_seq").and_then(serde_json::Value::as_u64);
                self.status = Some("Board marked read through this view.".to_owned());
            }
            KeyCode::Char('V') => self.mark_cancelled_seen(client)?,
            KeyCode::Char('M') => self.refresh_visible_marks(client)?,
            KeyCode::Char(character) => return self.card_key(character, client),
            _ => {}
        }
        Ok(None)
    }

    fn mark_cancelled_seen(&mut self, client: &Client) -> Result<()> {
        let cards: Vec<_> = self
            .visible
            .iter()
            .filter_map(|index| self.model.cards.get(*index))
            .filter(|card| card.column == Column::Dropped)
            .map(|card| card.id.clone())
            .collect();
        ensure!(!cards.is_empty(), "No cancelled cards in this view");
        for chunk in cards.chunks(1000) {
            client
                .board_post("/api/inbox/cards/dropped/seen", json!({"project":self.project,"cards":chunk}))?;
        }
        self.refresh(client)
    }

    fn refresh_visible_marks(&mut self, client: &Client) -> Result<()> {
        let ids: Vec<_> = self
            .visible
            .iter()
            .skip(self.scroll)
            .take(self.height.max(1))
            .filter_map(|index| self.model.cards.get(*index))
            .map(|card| card.id.clone())
            .collect();
        self.marks.extend(client.board_marks(&ids)?);
        self.refresh(client)?;
        self.status = Some("Visible forge marks refreshed; fully merged cards may move to Done.".to_owned());
        Ok(())
    }

    fn card_key(&mut self, key: char, client: &Client) -> Result<Option<BoardAction>> {
        let Some(card) = self.selected().cloned() else {
            return Ok(None);
        };
        if let Some(form) = card_form(&card, key) {
            self.modal = Some(Modal::Form(form));
            return Ok(None);
        }
        match key {
            'm' => self.pick(
                "Move card",
                Column::ALL
                    .into_iter()
                    .filter(|column| *column != card.column)
                    .map(|column| (column.label().to_owned(), Choice::Move(card.id.clone(), column)))
                    .collect(),
            ),
            'u' if card.column == Column::Dropped => {
                client.board_change(&card.id, &json!({"op":"restore"}))?;
                self.refresh(client)?;
            }
            'i' => {
                let items = std::iter::once(("None".to_owned(), Choice::Assign(card.id.clone(), None)))
                    .chain(self.model.initiatives.iter().filter(|item| item.deleted_at.is_none()).map(
                        |item| (item.name.clone(), Choice::Assign(card.id.clone(), Some(item.id.clone()))),
                    ))
                    .collect();
                self.pick("Card initiative", items);
            }
            't' | 'w' => {
                let items = card
                    .threads
                    .iter()
                    .map(|id| {
                        let subject = self
                            .model
                            .threads
                            .get(id)
                            .and_then(|row| row.get("subject"))
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or(id);
                        (subject.to_owned(), Choice::Thread(id.clone()))
                    })
                    .collect();
                self.pick("Open thread · waiting checklist steps lead here", items);
            }
            'o' => self.pick(
                "Open link in browser",
                card.references()
                    .iter()
                    .map(|reference| {
                        (
                            format!("{} · {}", reference.label, reference.url),
                            Choice::Url(reference.url.clone()),
                        )
                    })
                    .collect(),
            ),
            's' | 'c' => {
                ensure!(
                    card.column != Column::Dropped,
                    "Restore this cancelled card before starting a conversation"
                );
                ensure!(
                    self.pending_link.is_none() && self.pending.is_none(),
                    "Finish the unconfirmed action with ! before sending another"
                );
                let body = if key == 's' {
                    let text = card
                        .notes
                        .as_deref()
                        .filter(|notes| !notes.trim().is_empty())
                        .unwrap_or(&card.title)
                        .trim();
                    format!(
                        "{text}\n\n(This is “{}” on my Board, card {}. It is yours now: move it there as the work goes.)",
                        card.title, card.id
                    )
                } else {
                    String::new()
                };
                return Ok(Some(BoardAction::Compose { card_id: card.id, body, send: key == 's' }));
            }
            'a' | 'b' => self.pick_agent(client, &card, key == 'b')?,
            _ => {}
        }
        Ok(None)
    }

    fn pick_agent(&mut self, client: &Client, card: &Card, start: bool) -> Result<()> {
        ensure!(
            !matches!(card.column, Column::Done | Column::Dropped),
            "Move this card to an active lane before asking an agent"
        );
        ensure!(
            !start || matches!(card.column, Column::Backlog | Column::Todo),
            "Start is available for Backlog and Todo cards"
        );
        ensure!(
            !start || card.sent_waiting().is_none(),
            "This card was already sent; ask for an update instead"
        );
        ensure!(
            self.pending.is_none() && self.pending_link.is_none(),
            "An agent action is unconfirmed; ! retries the same action"
        );
        let mut targets = client.board_targets()?;
        targets.retain(|target| {
            target.project == card.project_id
                || card.agent.as_ref().and_then(|agent| agent.session.as_deref())
                    == Some(target.session.as_str())
        });
        ensure!(
            !targets.is_empty(),
            "No agent is running in this project; s opens Send to agent and can launch one"
        );
        let own = card.agent.as_ref().and_then(|agent| agent.session.as_deref());
        targets.sort_by_key(|target| usize::from(Some(target.session.as_str()) != own));
        self.pick(
            if start { "Start work · choose agent" } else { "Ask for an update · choose agent" },
            targets.into_iter().map(|target| (target.label.clone(), Choice::Target(target, start))).collect(),
        );
        Ok(())
    }

    pub(super) fn pick(&mut self, title: &str, items: Vec<(String, Choice)>) {
        self.modal = Some(Modal::Pick(Picker { title: title.to_owned(), items, cursor: 0 }));
    }

    pub(super) fn initiatives(&mut self) {
        let items = std::iter::once(("New initiative…".to_owned(), Choice::NewInitiative))
            .chain(self.model.initiatives.iter().map(|initiative| {
                let deleted = if initiative.deleted_at.is_some() { " (deleted; restore)" } else { "" };
                (format!("{}{deleted}", initiative.name), Choice::Initiative(initiative.id.clone()))
            }))
            .collect();
        self.pick("Manage initiatives", items);
    }

    fn navigate(&mut self, delta: isize) {
        if self.opened.is_some() {
            self.detail_scroll = self
                .detail_scroll
                .saturating_add_signed(delta.clamp(i16::MIN as isize, i16::MAX as isize) as i16);
        } else {
            self.cursor = self.cursor.saturating_add_signed(delta).min(self.visible.len().saturating_sub(1));
        }
    }

    pub(super) fn deliver(
        &mut self,
        client: &Client,
        target: super::model::Target,
        start: bool,
    ) -> Result<()> {
        let Some(card) = self.selected() else {
            return Ok(());
        };
        self.pending = Some(Delivery::new(card, target, start));
        if let Some(delivery) = self.pending.as_mut() {
            delivery.perform(client)?;
        }
        self.pending = None;
        self.status = Some(if start { "Start sent." } else { "Update requested." }.to_owned());
        self.refresh(client)
    }
}
