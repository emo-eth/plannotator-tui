use std::fmt::Write as _;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Value, json};
use url::Url;

use super::model::{
    Drawing, History, Loaded, Note, Pulls, SetupProject, SetupProjects, ShapeList, Stamp, Summary,
    SystemProject, SystemProjects,
};
use crate::inbox::client::{Client, path_id};

pub(super) fn list(client: &Client, finished: bool) -> Result<Vec<Summary>> {
    let path = if finished { "/api/shapes?state=all" } else { "/api/shapes" };
    Ok(client.get_json::<ShapeList>("GET", path)?.shapes)
}

pub(super) fn load(client: &Client, id: &str, n: Option<u64>) -> Result<Loaded> {
    let mut path = format!("/api/shapes/{}", path_id(id)?);
    if let Some(n) = n {
        write!(path, "?n={n}")?;
    }
    client.get_json("GET", &path)
}

pub(super) fn history(client: &Client, id: &str) -> Result<Vec<Stamp>> {
    Ok(client.get_json::<History>("GET", &format!("/api/shapes/{}/versions", path_id(id)?))?.versions)
}

pub(super) fn projects(client: &Client) -> Result<Vec<SystemProject>> {
    Ok(client.get_json::<SystemProjects>("GET", "/api/inbox/system/projects")?.projects)
}

pub(super) fn query(path: &str, name: &str, value: &str) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new()).append_pair(name, value).finish();
    format!("{path}?{query}")
}

pub(super) fn drawing(client: &Client, project: &str) -> Result<Drawing> {
    client.get_json("GET", &query("/api/inbox/system/drawing", "project", project))
}

pub(super) fn pulls(client: &Client, project: &str) -> Result<Pulls> {
    client.get_json("GET", &query("/api/inbox/system/pulls", "project", project))
}

pub(super) fn setup(client: &Client, project: &str) -> Result<SetupProject> {
    client
        .get_json::<SetupProjects>("GET", "/api/inbox/system/setup")?
        .projects
        .into_iter()
        .find(|item| item.id == project)
        .context("This System project cannot be set up here: its local folder needs a Git remote")
}

pub(super) fn write(client: &Client, path: &str, mut body: Value) -> Result<Value> {
    let object = body.as_object_mut().context("Shape write body must be an object")?;
    object.insert("serverSession".to_owned(), json!(client.server_session()));
    client.post_json(path, &body)
}

pub(super) fn note(client: &Client, id: &str, input: Value) -> Result<Note> {
    let result = write(client, &format!("/api/shapes/{}/notes", path_id(id)?), input)?;
    serde_json::from_value(result.get("note").cloned().context("Saved Shape response omitted note")?)
        .context("Reading saved Shape note")
}

pub(super) fn file(client: &Client, shape: &str, path: &str) -> Result<String> {
    #[derive(Deserialize)]
    struct File {
        content: String,
    }
    let route = format!("/api/shapes/{}/code/file", path_id(shape)?);
    Ok(client.get_json::<File>("GET", &query(&route, "path", path))?.content)
}

/// Use the actual Inbox router, retaining authentication query parameters.
pub(super) fn browser_url(client: &Client, fragment: &str) -> Result<String> {
    let mut url = Url::parse(client.url()).context("Reading Inbox browser URL")?;
    url.set_fragment(Some(fragment));
    Ok(url.into())
}

pub(super) fn encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

pub(super) fn shape_url(client: &Client, loaded: &Loaded) -> Result<String> {
    let prefix = loaded
        .shape
        .thread
        .as_ref()
        .map_or_else(|| "shapes".to_owned(), |thread| format!("thread={}", encode(thread)));
    browser_url(client, &format!("{prefix}&shape={}", encode(&loaded.shape.id)))
}

pub(super) fn asset_url(client: &Client, shape: &str, target: &str) -> Result<String> {
    let name = target
        .strip_prefix("files/")
        .context("This image is not a kept Shape file; open the graphical board")?;
    anyhow::ensure!(
        !name.is_empty() && !name.contains(['/', '\\']) && !name.starts_with('.'),
        "Invalid Shape image name"
    );
    Ok(Url::parse(client.url())?
        .join(&format!("/api/shapes/{}/files/{}", path_id(shape)?, encode(name)))?
        .into())
}

pub(super) fn setup_body(project: &str, text: &str) -> Result<Value> {
    let mut value: Value = serde_json::from_str(text).context("System setup must be JSON")?;
    let object = value.as_object_mut().context("System setup must be a JSON object")?;
    anyhow::ensure!(
        object.keys().all(|key| matches!(key.as_str(), "repos" | "docs" | "agent" | "model")),
        "Setup only accepts repos, docs, agent and model; project identity cannot be edited"
    );
    anyhow::ensure!(
        object.get("repos").and_then(Value::as_array).is_some_and(|items| items.iter().all(Value::is_string)),
        "repos must be an array of strings"
    );
    anyhow::ensure!(
        object.get("docs").and_then(Value::as_array).is_some_and(|items| items.iter().all(Value::is_string)),
        "docs must be an array of strings"
    );
    anyhow::ensure!(
        matches!(object.get("agent").and_then(Value::as_str), Some("claude" | "pi" | "opencode")),
        "agent must be claude, pi or opencode"
    );
    anyhow::ensure!(
        object.get("model").is_some_and(|model| model.is_null() || model.is_string()),
        "model must be a string or null"
    );
    object.insert("project_id".to_owned(), json!(project));
    Ok(value)
}

/// The note is durable before this starts. A failure never resends it or fabricates an answer.
pub(super) fn deliver(
    client: &Client,
    loaded: &Loaded,
    note: &Note,
) -> Option<std::sync::mpsc::Receiver<Result<String>>> {
    let thread = loaded.shape.thread.clone()?;
    if loaded.session.state == "gone" {
        return None;
    }
    let base = client.url().to_owned();
    let session = client.server_session().to_owned();
    let shape = loaded.shape.id.clone();
    let title = loaded.shape.title.clone();
    let note = note.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| -> Result<String> {
            let client = Client::from_url(&base)?;
            anyhow::ensure!(
                client.server_session() == session,
                "Inbox restarted; rediscover before live delivery"
            );
            let change = note.kind == "change";
            if change {
                let settings: Value = client.get_json("GET", "/api/inbox/settings")?;
                if settings.pointer("/code_review/on").and_then(Value::as_bool) != Some(true) {
                    return Ok(
                        "Change saved; it waits for the agent's next Shape read (Code review wake is off)."
                            .to_owned(),
                    );
                }
            }
            let prompt = if change {
                format!(
                    "The person left changes on the Shape board \"{title}\" ({shape}). Run `plannotator shape read {shape} --notes` and draw them as a new version. Then say what changed in one sentence."
                )
            } else {
                let where_on = note.view.as_ref().map_or(String::new(), |view| format!(", the {view} view"));
                let quote = note.quote.as_ref().map_or(String::new(), |quote| {
                    format!(
                        ", \"{}\"",
                        quote
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" ")
                            .chars()
                            .take(300)
                            .collect::<String>()
                    )
                });
                format!(
                    "About the Shape board \"{title}\" ({shape}, v{}{where_on}{quote}): {}\n\nAnswer in one or two sentences. If it needs a change to the board, draw it as a new version.",
                    note.v, note.text
                )
            };
            let turn = format!("ask_shape_{}", note.id);
            let answer =
                crate::inbox::ask::answer(&client, &thread, &turn, &prompt, change.then_some("wait"))?;
            if !change {
                write(
                    &client,
                    &format!("/api/shapes/{}/answer", path_id(&shape)?),
                    json!({
                        "re": note.id, "text": answer.text,
                        "from": { "host": "inbox-ask", "id": answer.provider_id }
                    }),
                )?;
            }
            Ok(if change {
                "Change saved and the drawing agent was asked to pick it up."
            } else {
                "Question and live agent answer saved."
            }
            .to_owned())
        })();
        let _ = tx.send(result);
    });
    Some(rx)
}
