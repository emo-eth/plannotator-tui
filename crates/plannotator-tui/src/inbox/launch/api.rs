//! Explicit window requests only. The server remains authoritative about capabilities and races.

use anyhow::{Context, Result, bail, ensure};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::super::client::{Client, HttpError};
use super::super::send;
use super::model::{Sessions, Settings, ThreadRead};

pub(super) fn route(machine: Option<&str>, path: &str) -> Result<String> {
    match machine {
        Some(machine) => Ok(format!("/api/inbox/m/{}/{}", id(machine)?, path.trim_start_matches('/'))),
        None => Ok(format!("/api/inbox/{}", path.trim_start_matches('/'))),
    }
}

pub(super) fn id(value: &str) -> Result<&str> {
    ensure!(
        !value.is_empty() && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')),
        "Invalid Inbox identifier"
    );
    Ok(value)
}

pub(super) fn query(pairs: &[(&str, &str)]) -> String {
    url::form_urlencoded::Serializer::new(String::new()).extend_pairs(pairs.iter().copied()).finish()
}

pub(super) fn get<T: DeserializeOwned>(client: &Client, machine: Option<&str>, path: &str) -> Result<T> {
    client.get_json("GET", &route(machine, path)?)
}

pub(super) fn post(client: &Client, machine: Option<&str>, path: &str, body: &Value) -> Result<Value> {
    let mut body = body.clone();
    let object = body.as_object_mut().context("Inbox write must be an object")?;
    object.insert("serverSession".into(), Value::String(client.server_session().into()));
    // Forwarded writes are keyed end-to-end. Callers retain a key across uncertain retries.
    if machine.is_some() && !object.contains_key("idempotency_key") {
        bail!("A forwarded launch action must retain an idempotency key");
    }
    let answer: Value = client.post_json(&route(machine, path)?, &body)?;
    if let Some(entry) = answer.get("outbox") {
        match entry.get("state").and_then(Value::as_str) {
            Some("delivered") => {
                return entry.get("answer").cloned().context("Delivered write omitted its answer");
            }
            Some("queued" | "sending") => return Ok(answer),
            Some("cancelled") => bail!("Cancelled: this write was never sent to the other machine"),
            _ => bail!("Remote write was not delivered: {}", entry.get("answer").unwrap_or(entry)),
        }
    }
    Ok(answer)
}

pub(super) fn settings(client: &Client) -> Result<Settings> {
    let answer: Settings = get(client, None, "settings?store=0")?;
    ensure!(
        answer.server_session == client.server_session(),
        "Inbox restarted; reopen it before making changes"
    );
    Ok(answer)
}

pub(super) fn sessions(
    client: &Client,
    machine: Option<&str>,
    trust: Option<(&str, &str)>,
) -> Result<Sessions> {
    let path = trust.map_or_else(
        || "sessions".to_owned(),
        |(agent, project)| {
            format!("sessions?{}", query(&[("trust_agent", agent), ("trust_project", project)]))
        },
    );
    let answer: Sessions = get(client, machine, &path)?;
    ensure!(
        machine.is_some() || answer.server_session == client.server_session(),
        "Inbox restarted; reopen it before making changes"
    );
    Ok(answer)
}

pub(super) fn thread(client: &Client, machine: Option<&str>, thread_id: &str) -> Result<ThreadRead> {
    let answer: ThreadRead = get(client, machine, &format!("threads/{}", id(thread_id)?))?;
    ensure!(
        machine.is_some() || answer.server_session == client.server_session(),
        "Inbox restarted; reopen it before making changes"
    );
    ensure!(answer.thread.thread_id == thread_id, "Inbox answered with another thread");
    Ok(answer)
}

/// The merged list names the machine; conflicting claims never pick an arbitrary owner.
/// A successful local read always wins, matching the server's `id_is_local` protection.
pub(super) fn locate(client: &Client, thread_id: &str) -> Result<(Option<String>, ThreadRead)> {
    fn visit(value: &Value, thread_id: &str, machines: &mut Vec<String>) {
        match value {
            Value::Object(object) => {
                if object.get("thread_id").and_then(Value::as_str) == Some(thread_id)
                    && let Some(machine) = object.get("machine").and_then(Value::as_str)
                    && !machines.iter().any(|m| m == machine)
                {
                    machines.push(machine.into());
                }
                for value in object.values() {
                    visit(value, thread_id, machines);
                }
            }
            Value::Array(values) => {
                for value in values {
                    visit(value, thread_id, machines);
                }
            }
            _ => {}
        }
    }
    match thread(client, None, thread_id) {
        Ok(read) => return Ok((None, read)),
        Err(error) if error.downcast_ref::<HttpError>().is_some_and(|e| e.status == 404) => {}
        Err(error) => return Err(error),
    }
    let list: Value = get(client, None, "threads")?;
    let mut machines = Vec::new();
    visit(&list, thread_id, &mut machines);
    ensure!(
        machines.len() == 1,
        "Thread not found locally, or its linked-machine ownership is unknown or conflicting"
    );
    let machine = machines.pop().context("Missing linked machine")?;
    let read = thread(client, Some(&machine), thread_id)?;
    Ok((Some(machine), read))
}

pub(super) fn keyed() -> Value {
    json!({ "idempotency_key": send::new_key() })
}

pub(super) fn set(body: &mut Value, field: &str, value: Value) -> Result<()> {
    body.as_object_mut().context("Inbox write must be an object")?.insert(field.into(), value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_cannot_inject_segments_or_queries() {
        assert!(route(Some("../../launch"), "sessions").is_err());
        assert!(id("msg_x?other=1").is_err());
        assert_eq!(query(&[("path", "~/a & b")]), "path=%7E%2Fa+%26+b");
    }
}
