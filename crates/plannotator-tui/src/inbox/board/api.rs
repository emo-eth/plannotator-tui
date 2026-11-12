//! Board HTTP contract. Every POST carries the connected server incarnation.
use std::collections::BTreeMap;

use anyhow::{Context, Result, anyhow, ensure};
use serde::Deserialize;
use serde_json::{Value, json};

use super::super::client::Client;
use super::model::{Card, CardDetail, CardsModel, Project, ResolvedRef, Target};

pub(super) fn id(value: &str) -> Result<&str> {
    ensure!(
        !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
        "Invalid Inbox id: {value}"
    );
    Ok(value)
}

impl Client {
    pub(crate) fn board_cards(&self) -> Result<CardsModel> {
        let model: CardsModel = self.get_json("GET", "/api/inbox/cards")?;
        ensure!(
            model.server_session == self.server_session(),
            "Board belongs to another Inbox incarnation; reconnect first"
        );
        Ok(model)
    }

    pub(crate) fn board_card(&self, card: &str) -> Result<CardDetail> {
        self.get_json("GET", &format!("/api/inbox/cards/{}", id(card)?))
    }

    pub(crate) fn board_refs(&self, card: &str) -> Result<Vec<ResolvedRef>> {
        #[derive(Deserialize)]
        struct Response {
            refs: Vec<ResolvedRef>,
        }
        let response: Response = self.get_json("GET", &format!("/api/inbox/cards/{}/refs", id(card)?))?;
        Ok(response.refs)
    }

    pub(crate) fn board_projects(&self) -> Result<Vec<Project>> {
        #[derive(Deserialize)]
        struct Response {
            projects: Vec<Project>,
        }
        let response: Response = self.get_json("GET", "/api/inbox/threads")?;
        Ok(response.projects)
    }

    pub(super) fn board_post(&self, path: &str, mut body: Value) -> Result<Value> {
        body.as_object_mut()
            .ok_or_else(|| anyhow!("Board request must be an object"))?
            .insert("serverSession".to_owned(), json!(self.server_session()));
        self.post_json(path, &body)
    }

    pub(crate) fn board_change(&self, card: &str, change: &Value) -> Result<Card> {
        let response = self.board_post(&format!("/api/inbox/cards/{}", id(card)?), change.clone())?;
        serde_json::from_value(
            response.get("card").cloned().ok_or_else(|| anyhow!("Board response omitted card"))?,
        )
        .context("Reading changed card")
    }

    pub(super) fn board_marks(&self, cards: &[String]) -> Result<BTreeMap<String, Value>> {
        let mut marks = BTreeMap::new();
        for chunk in cards.chunks(200) {
            let response =
                self.board_post("/api/inbox/cards/marks", json!({"cards": chunk, "refresh": true}))?;
            let batch: BTreeMap<String, Value> = serde_json::from_value(
                response
                    .get("marks")
                    .cloned()
                    .ok_or_else(|| anyhow!("Board marks response omitted marks"))?,
            )?;
            marks.extend(batch);
        }
        Ok(marks)
    }

    pub(super) fn board_targets(&self) -> Result<Vec<Target>> {
        let response: Value = self.get_json("GET", "/api/inbox/sessions")?;
        let mut targets = Vec::new();
        for group in response.get("projects").and_then(Value::as_array).into_iter().flatten() {
            let project = group.pointer("/project/id").and_then(Value::as_str).unwrap_or("");
            let name = group.pointer("/project/name").and_then(Value::as_str).unwrap_or("Project");
            for session in group.get("sessions").and_then(Value::as_array).into_iter().flatten() {
                let Some(host) = session.get("host").and_then(Value::as_str) else {
                    continue;
                };
                let Some(session_id) = session.get("session").and_then(Value::as_str) else {
                    continue;
                };
                let label = session.get("label").and_then(Value::as_str).unwrap_or(host);
                targets.push(Target {
                    host: host.to_owned(),
                    session: session_id.to_owned(),
                    label: format!("{label} · {name} · {session_id}"),
                    project: project.to_owned(),
                });
            }
        }
        Ok(targets)
    }
}

/// A message followed by a card write. Keep the key and confirmed message across failures:
/// retrying the card write must never send the agent another message.
pub(super) struct Delivery {
    card: String,
    target: Target,
    body: String,
    key: String,
    existing_thread: Option<String>,
    sent_message: Option<String>,
    start: bool,
}

impl Delivery {
    pub(super) fn new(card: &Card, target: Target, start: bool) -> Self {
        let own_agent = card.agent.as_ref().and_then(|agent| agent.session.as_deref());
        let existing_thread =
            if own_agent == Some(target.session.as_str()) { card.threads.first().cloned() } else { None };
        let body = if start {
            let notes = card
                .notes
                .as_deref()
                .filter(|notes| !notes.trim().is_empty())
                .map_or(String::new(), |notes| format!("\n\n{}", notes.trim()));
            format!(
                "Please start on “{}” (Board card {}).{notes}\n\nMove the card as the work goes.",
                card.title, card.id
            )
        } else {
            format!(
                "Where is “{}”? (Board card {}) Move the card, or say where it stands with the card action.",
                card.title, card.id
            )
        };
        Self {
            card: card.id.clone(),
            target,
            body,
            key: super::super::send::new_key(),
            existing_thread,
            sent_message: None,
            start,
        }
    }

    pub(super) fn perform(&mut self, client: &Client) -> Result<()> {
        if self.sent_message.is_none() {
            let response = if let Some(thread) = &self.existing_thread {
                client.board_post(
                    &format!("/api/inbox/threads/{}/message", id(thread)?),
                    json!({
                        "session": self.target.session, "body": self.body, "idempotency_key": self.key
                    }),
                )?
            } else {
                client.board_post(
                    "/api/inbox/messages",
                    json!({
                        "to": {"host": self.target.host, "session": self.target.session},
                        "body": self.body, "idempotency_key": self.key
                    }),
                )?
            };
            self.sent_message = Some(
                response
                    .pointer("/message/id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        anyhow!("Agent message was not confirmed; press ! to retry the same request")
                    })?
                    .to_owned(),
            );
        }
        if self.start || self.existing_thread.is_none() {
            client.board_change(
                &self.card,
                &json!({"op": if self.start {"send"} else {"link"}, "thread_id": self.sent_message}),
            )?;
        }
        Ok(())
    }
}
