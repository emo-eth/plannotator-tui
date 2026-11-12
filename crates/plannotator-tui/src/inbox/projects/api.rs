use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use super::model::{Answer, ProjectList};
use crate::inbox::client::{Client, path_id};

pub(super) enum Effect {
    Name,
    Combine { into: Option<String> },
    Batch { archive: bool },
    Undo,
    Delete,
}

pub(super) struct Mutation {
    pub(super) project: String,
    pub(super) label: String,
    pub(super) session: String,
    pub(super) route: &'static str,
    pub(super) body: Value,
    pub(super) effect: Effect,
}

impl Mutation {
    pub(super) fn new(
        client: &Client,
        project: &str,
        label: String,
        route: &'static str,
        body: Value,
        effect: Effect,
    ) -> Self {
        Self {
            project: project.to_owned(),
            label,
            session: client.server_session().to_owned(),
            route,
            body,
            effect,
        }
    }

    pub(super) fn apply(&self, client: &Client) -> Result<Answer> {
        ensure!(
            self.session == client.server_session(),
            "{}: Inbox serverSession changed; this pending action belongs to the previous server. Refresh before starting a new action.",
            self.label
        );
        let path = format!("/api/inbox/projects/{}/{}", path_id(&self.project)?, self.route);
        let mut body = self.body.clone();
        body.as_object_mut()
            .context("Project write must be an object")?
            .insert("serverSession".into(), json!(self.session));
        let answer: Answer = client.post_json(&path, &body).with_context(|| self.label.clone())?;
        ensure!(
            answer.server_session.as_deref().is_none_or(|session| session == self.session),
            "{}: POST {path} answered from a different Inbox serverSession; outcome unconfirmed",
            self.label
        );
        if matches!(self.effect, Effect::Delete) {
            ensure!(answer.ok == Some(true), "{}: POST {path} did not confirm deletion", self.label);
        }
        Ok(answer)
    }

    pub(super) fn inverse(&self, answer: &Answer) -> Option<Self> {
        let (route, body) = match &self.effect {
            Effect::Combine { into: Some(_) } => ("combine", json!({"into": null})),
            Effect::Batch { archive: false } if answer.batch.len() > 0 => {
                ("unread", json!({"threads": answer.batch.read, "looked": answer.batch.looked}))
            }
            Effect::Batch { archive: true } if answer.batch.len() > 0 => (
                "resolve",
                json!({"resolved": false, "threads": {
                    "resolved": answer.batch.resolved, "read": answer.batch.read,
                }}),
            ),
            _ => return None,
        };
        Some(Self {
            project: self.project.clone(),
            label: format!("Undo {}", self.label),
            session: self.session.clone(),
            route,
            body,
            effect: Effect::Undo,
        })
    }
}

pub(super) fn read(client: &Client) -> Result<ProjectList> {
    let list: ProjectList =
        client.get_json("GET", "/api/inbox/projects").context("Reading Inbox projects")?;
    ensure!(
        list.server_session == client.server_session(),
        "GET /api/inbox/projects: Inbox serverSession changed; reconnect before continuing"
    );
    Ok(list)
}

pub(super) fn batch_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let at = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |time| time.as_nanos());
    format!("tui-project-{}-{at}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed))
}
