//! A write is not complete just because a linked machine accepted it into the outbox.

use anyhow::{Context, Result, bail};
use ratatui::DefaultTerminal;
use serde_json::Value;

use super::super::client::{Client, HttpError};
use super::{
    api,
    screen::{self, Choice},
};

pub(super) enum Outcome {
    Delivered(Value),
    Cancelled,
    Held,
}

/// The same payload, including its key, is retained until confirmed or explicitly left unconfirmed.
pub(super) fn submit(
    client: &Client,
    terminal: &mut DefaultTerminal,
    machine: Option<&str>,
    path: &str,
    body: &Value,
) -> Result<Outcome> {
    let mut uncertain = false;
    loop {
        match api::post(client, machine, path, body) {
            Ok(answer) => {
                if let Some(entry) = answer.get("outbox") {
                    return held(client, terminal, machine, path, body, entry);
                }
                return Ok(Outcome::Delivered(answer));
            }
            Err(error) => {
                let refused =
                    error.downcast_ref::<HttpError>().is_some_and(|e| (400..500).contains(&e.status));
                uncertain |= !refused;
                let detail = format!(
                    "{error:#}\n\n{}",
                    if uncertain {
                        "The server has not confirmed this write. It may have happened. Retry sends the identical request and key; do not create another copy."
                    } else {
                        "The server refused this write. Nothing was confirmed. Return to review the latest state."
                    }
                );
                let options = if uncertain {
                    vec![
                        "Retry the identical request".into(),
                        "Leave unconfirmed (do not send a new copy)".into(),
                    ]
                } else {
                    vec!["Return without retrying".into()]
                };
                if matches!(
                    screen::choose(terminal, "Write not confirmed", &detail, &options, false)?,
                    Choice::Selected(0)
                ) && uncertain
                {
                    continue;
                }
                return Err(error);
            }
        }
    }
}

fn held(
    client: &Client,
    terminal: &mut DefaultTerminal,
    machine: Option<&str>,
    path: &str,
    body: &Value,
    initial: &Value,
) -> Result<Outcome> {
    let key = initial.get("key").and_then(Value::as_str).context("Held launch omitted its outbox key")?;
    let mut entry = initial.clone();
    loop {
        let state = entry.get("state").and_then(Value::as_str).unwrap_or("unknown");
        match state {
            "delivered" => {
                return Ok(Outcome::Delivered(
                    entry.get("answer").cloned().context("Delivered launch omitted its answer")?,
                ));
            }
            "cancelled" => return Ok(Outcome::Cancelled),
            "refused" | "parked" | "dropped" => {
                bail!("Remote write was not delivered: {}", entry.get("answer").unwrap_or(&entry))
            }
            _ => {}
        }
        let detail = format!(
            "State: {state}\nOutbox key: {key}\nThis action is waiting for the other machine, not completed. It may apply hours or days later.\nCancel is available only until it leaves this Inbox."
        );
        let mut labels = vec!["Keep waiting".into(), "Return to Inbox (action remains queued)".into()];
        if state == "queued" {
            labels.push("Cancel before it leaves".into());
        }
        match screen::choose(terminal, "Waiting for linked machine", &detail, &labels, true)? {
            Choice::Cancel | Choice::Selected(1) => return Ok(Outcome::Held),
            Choice::Selected(2)
                if state == "queued"
                    && screen::confirm(
                        terminal,
                        "Cancel queued action?",
                        "Only an action that never left can be cancelled. A new launch's message will remain in this composer after cancellation.",
                        "Cancel queued action",
                    )? =>
            {
                let response =
                    api::post(client, None, &format!("outbox/{}/cancel", api::id(key)?), &api::keyed())?;
                entry = response.get("entry").cloned().context("Cancel omitted its outbox entry")?;
                continue;
            }
            _ => {}
        }
        let response: Value = api::get(client, None, "outbox")?;
        let current = ["waiting", "notices"]
            .iter()
            .filter_map(|name| response.get(name).and_then(Value::as_array))
            .flatten()
            .find(|e| e.get("key").and_then(Value::as_str) == Some(key));
        if let Some(current) = current {
            entry = current.clone();
        } else {
            // Listings omit settled entries. Replaying the original key asks the
            // server for its stored outcome; it cannot start a second agent.
            let answer = api::post(client, machine, path, body)?;
            if let Some(current) = answer.get("outbox") {
                entry = current.clone();
            } else {
                return Ok(Outcome::Delivered(answer));
            }
        }
    }
}
