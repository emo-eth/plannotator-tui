use std::collections::HashMap;

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

use crate::inbox::client::Client;

#[derive(Default)]
pub(super) struct Counts {
    pub(super) readable: usize,
    pub(super) questions: usize,
    pub(super) stopped: usize,
    pub(super) holds: usize,
}

#[derive(Deserialize)]
struct List {
    #[serde(rename = "serverSession")]
    session: String,
    sections: Vec<Section>,
}

#[derive(Deserialize)]
struct Section {
    threads: Vec<Row>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub(super) struct Row {
    project: Identity,
    section: String,
    unseen: u64,
    answered_not_sent: bool,
    resolved_at: Option<String>,
    read_at: Option<String>,
    back_from_archive: bool,
    questions: Questions,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Identity {
    id: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Questions {
    open: usize,
    picked: usize,
    stopped: bool,
    holds_up: Vec<serde_json::Value>,
}

pub(super) fn readable(row: &Row) -> bool {
    (row.resolved_at.is_none() || row.back_from_archive)
        && (row.read_at.is_none() || row.unseen > 0)
        && (matches!(row.section.as_str(), "stopped" | "holding" | "waiting")
            || (row.section == "new" && (row.unseen > 0 || row.answered_not_sent)))
}

pub(super) fn read(client: &Client) -> Result<HashMap<String, Counts>> {
    let list: List =
        client.get_json("GET", "/api/inbox/threads").context("Reading project batch eligibility")?;
    ensure!(
        list.session == client.server_session(),
        "GET /api/inbox/threads: Inbox serverSession changed; reconnect before continuing"
    );
    let mut counts = HashMap::<String, Counts>::new();
    for row in list.sections.into_iter().flat_map(|section| section.threads) {
        let eligible = readable(&row);
        let count = counts.entry(row.project.id).or_default();
        count.readable += usize::from(eligible);
        count.questions += row.questions.open + row.questions.picked;
        if row.questions.open > 0 {
            count.stopped += usize::from(row.questions.stopped);
            count.holds += row.questions.holds_up.len();
        }
    }
    Ok(counts)
}
