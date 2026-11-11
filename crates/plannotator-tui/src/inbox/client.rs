use std::error::Error as StdError;
use std::fmt;
use std::io::{BufRead, BufReader};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ureq::{Agent, AgentBuilder, Response};
use url::Url;

use super::questions::Question;

#[derive(Debug)]
pub(crate) struct Client {
    url: String,
    server_session: String,
    agent: Agent,
}

#[derive(Debug, Deserialize)]
struct Registry {
    url: String,
    #[serde(rename = "serverSession")]
    server_session: String,
}

#[derive(Debug, Deserialize)]
struct Health {
    #[serde(rename = "serverSession")]
    server_session: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct List {
    #[serde(rename = "serverSession")]
    pub(crate) server_session: String,
    pub(crate) cursor: u64,
    pub(crate) notice: Option<String>,
    pub(crate) sections: Vec<Section>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Project {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) root: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Section {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) threads: Vec<Row>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Row {
    pub(crate) thread_id: String,
    pub(crate) subject: Option<String>,
    pub(crate) thread_name: Option<String>,
    pub(crate) project: Project,
    pub(crate) section: String,
    pub(crate) author: Author,
    pub(crate) last_author: String,
    pub(crate) created_at: String,
    pub(crate) last_at: String,
    pub(crate) message_count: u64,
    pub(crate) resolved_at: Option<String>,
    pub(crate) unread: bool,
    pub(crate) answered_not_sent: bool,
    pub(crate) waiting_on_person: bool,
    pub(crate) waiting_since: Option<String>,
    pub(crate) questions: QuestionSummary,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct QuestionSummary {
    pub(crate) open: u64,
    pub(crate) picked: u64,
    pub(crate) stopped: bool,
    pub(crate) holds_up: Vec<String>,
    pub(crate) prompt: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Author {
    pub(crate) kind: String,
    pub(crate) host: Option<String>,
    pub(crate) session: Option<String>,
    pub(crate) name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ThreadModel {
    #[serde(rename = "serverSession")]
    pub(crate) server_session: String,
    pub(crate) thread: Thread,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
#[allow(clippy::struct_field_names)]
pub(crate) struct Thread {
    pub(crate) thread_id: String,
    pub(crate) subject: Option<String>,
    pub(crate) thread_name: Option<String>,
    pub(crate) resolved_at: Option<String>,
    pub(crate) project: Project,
    pub(crate) messages: Vec<Message>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Message {
    pub(crate) id: String,
    pub(crate) thread_id: String,
    pub(crate) reply_to: Option<String>,
    pub(crate) author: Author,
    pub(crate) subject: Option<String>,
    pub(crate) body: String,
    pub(crate) created_at: String,
    pub(crate) questions: Vec<Question>,
    pub(crate) to: Option<Addressee>,
    pub(crate) took_over: bool,
    pub(crate) handoff: Option<Handoff>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Addressee {
    pub(crate) host: Option<String>,
    pub(crate) session: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Handoff {
    pub(crate) undone_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Pick {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) message_id: Option<String>,
    pub(crate) key: String,
    pub(crate) revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) answer: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Skip {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) message_id: Option<String>,
    pub(crate) key: String,
}

#[derive(Debug)]
pub(crate) struct HttpError {
    pub(crate) status: u16,
    pub(crate) code: Option<String>,
    pub(crate) message: String,
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.code {
            Some(code) => write!(f, "HTTP {} {code}: {}", self.status, self.message),
            None => write!(f, "HTTP {}: {}", self.status, self.message),
        }
    }
}

impl StdError for HttpError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Live {
    Connected { cursor: u64 },
    Record { seq: u64, kind: String, id: String, thread_id: Option<String> },
    Refresh,
    Lost(String),
}

impl Client {
    pub(crate) fn discover() -> Result<Option<Self>> {
        let data_dir = crate::workspace_paths::data_dir();
        let registry_path = data_dir.join("inbox").join("inbox.json");
        let text = match std::fs::read_to_string(&registry_path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("reading Inbox registry {}", registry_path.display()));
            }
        };
        let registry: Registry = serde_json::from_str(&text).context("parsing Inbox registry")?;
        Self::connect(&registry.url, Some(&registry.server_session)).map(Some)
    }

    pub(crate) fn from_url(url: &str) -> Result<Self> {
        Self::connect(url, None)
    }

    pub(crate) fn url(&self) -> &str {
        &self.url
    }

    pub(crate) fn list(&self) -> Result<List> {
        let list: List = self.get_json("GET", "/api/inbox/threads")?;
        anyhow::ensure!(
            list.server_session == self.server_session,
            "GET {}: Inbox serverSession changed; rediscover before continuing",
            self.url
        );
        Ok(list)
    }

    pub(crate) fn thread(&self, thread_id: &str) -> Result<ThreadModel> {
        let path = format!("/api/inbox/threads/{}", path_id(thread_id)?);
        let thread: ThreadModel = self.get_json("GET", &path)?;
        anyhow::ensure!(
            thread.server_session == self.server_session,
            "GET {}: Inbox serverSession changed; rediscover before continuing",
            endpoint(&self.url, &path)?
        );
        Ok(thread)
    }

    pub(crate) fn pick(
        &self,
        message_id: &str,
        idempotency_key: &str,
        questions: &[Pick],
    ) -> Result<Vec<Question>> {
        let body = json!({
            "serverSession": self.server_session,
            "idempotency_key": idempotency_key,
            "questions": questions,
        });
        let path = format!("/api/inbox/messages/{}/picks", path_id(message_id)?);
        let response: QuestionsResponse = self.post_json(&path, &body)?;
        Ok(response.questions)
    }

    pub(crate) fn reply(
        &self,
        message_id: &str,
        idempotency_key: &str,
        words: Option<&str>,
        questions: &[Pick],
        skip: &[Skip],
    ) -> Result<Vec<Question>> {
        let body = if let Some(words) = words {
            json!({
                "serverSession": self.server_session,
                "idempotency_key": idempotency_key,
                "questions": questions,
                "skip": skip,
                "words": words,
            })
        } else {
            json!({
                "serverSession": self.server_session,
                "idempotency_key": idempotency_key,
                "questions": questions,
                "skip": skip,
            })
        };
        let path = format!("/api/inbox/messages/{}/reply", path_id(message_id)?);
        let response: QuestionsResponse = self.post_json(&path, &body)?;
        Ok(response.questions)
    }

    pub(crate) fn subscribe(&self, cursor: u64) -> Receiver<Live> {
        let (tx, rx) = mpsc::channel();
        let url = self.url.clone();
        let server_session = self.server_session.clone();
        thread::spawn(move || event_worker(&url, &server_session, cursor, &tx));
        rx
    }

    fn connect(url: &str, expected_session: Option<&str>) -> Result<Self> {
        let parsed = Url::parse(url).with_context(|| format!("parsing Inbox URL {url}"))?;
        anyhow::ensure!(
            matches!(parsed.scheme(), "http" | "https"),
            "Inbox URL must use HTTP or HTTPS: {url}"
        );
        anyhow::ensure!(parsed.host_str().is_some(), "Inbox URL has no host: {url}");
        anyhow::ensure!(
            parsed.username().is_empty() && parsed.password().is_none(),
            "Inbox URL must not contain credentials: {url}"
        );
        let url = parsed.to_string();
        let agent = AgentBuilder::new()
            .timeout_connect(Duration::from_secs(3))
            .timeout_read(Duration::from_secs(20))
            .build();
        let health_url = endpoint(&url, "/api/inbox/health")?;
        let health: Health = get_with(&agent, &url, "GET", &health_url)?;
        if let Some(expected) = expected_session {
            anyhow::ensure!(
                health.server_session == expected,
                "GET {health_url}: Inbox registry serverSession mismatch; registry may be stale"
            );
        }
        anyhow::ensure!(
            !health.server_session.is_empty(),
            "GET {health_url}: Inbox health omitted serverSession"
        );
        Ok(Self { url, server_session: health.server_session, agent })
    }

    fn get_json<T: DeserializeOwned>(&self, method: &str, path: &str) -> Result<T> {
        let url = endpoint(&self.url, path)?;
        get_with(&self.agent, &self.url, method, &url)
    }

    fn post_json<T: DeserializeOwned>(&self, path: &str, body: &Value) -> Result<T> {
        let url = endpoint(&self.url, path)?;
        let origin = origin_of(&self.url)?;
        let response = self
            .agent
            .post(&url)
            .set("Content-Type", "application/json")
            .set("Accept", "application/json")
            .set("Origin", &origin)
            .send_json(body);
        parse_response(response, "POST", &url)
    }
}

#[derive(Debug, Deserialize)]
struct QuestionsResponse {
    questions: Vec<Question>,
}

fn endpoint(base: &str, path: &str) -> Result<String> {
    let base = Url::parse(base).context("parsing Inbox URL")?;
    Ok(base.join(path)?.to_string())
}

fn origin_of(base: &str) -> Result<String> {
    let base = Url::parse(base).context("parsing Inbox URL")?;
    Ok(base.origin().ascii_serialization())
}

fn path_id(value: &str) -> Result<&str> {
    anyhow::ensure!(
        !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
        "invalid Inbox id {value:?}"
    );
    Ok(value)
}

fn get_with<T: DeserializeOwned>(agent: &Agent, base: &str, method: &str, url: &str) -> Result<T> {
    let origin = origin_of(base)?;
    let response = agent.get(url).set("Accept", "application/json").set("Origin", &origin).call();
    parse_response(response, method, url)
}

fn parse_response<T: DeserializeOwned>(
    response: std::result::Result<Response, ureq::Error>,
    method: &str,
    url: &str,
) -> Result<T> {
    let response = match response {
        Ok(response) => response,
        Err(ureq::Error::Status(status, response)) => {
            let body = response.into_string().unwrap_or_default();
            let payload: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            let code = payload.get("code").and_then(Value::as_str).map(str::to_owned);
            let message = payload
                .get("error")
                .or_else(|| payload.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("request failed")
                .to_owned();
            return Err(anyhow::Error::new(HttpError { status, code, message }))
                .with_context(|| format!("{method} {url}"));
        }
        Err(error) => return Err(anyhow!(error)).with_context(|| format!("{method} {url}")),
    };
    response.into_json().with_context(|| format!("decoding {method} response from {url}"))
}

fn event_worker(url: &str, server_session: &str, mut cursor: u64, tx: &Sender<Live>) {
    let agent = AgentBuilder::new()
        .timeout_connect(Duration::from_secs(3))
        .timeout_read(Duration::from_secs(20))
        .build();
    let origin = match origin_of(url) {
        Ok(origin) => origin,
        Err(error) => {
            let _ = tx.send(Live::Lost(error.to_string()));
            return;
        }
    };
    let mut backoff = Duration::from_secs(1);
    loop {
        let target = match endpoint(url, &format!("/api/inbox/events?cursor={cursor}")) {
            Ok(target) => target,
            Err(error) => {
                if tx.send(Live::Lost(error.to_string())).is_err() {
                    return;
                }
                return;
            }
        };
        let response = agent
            .get(&target)
            .set("Accept", "text/event-stream")
            .set("Cache-Control", "no-cache")
            .set("Origin", &origin)
            .set("Last-Event-ID", &cursor.to_string())
            .call();
        match response {
            Ok(response) => {
                backoff = Duration::from_secs(1);
                if let Err(error) =
                    read_events(BufReader::new(response.into_reader()), server_session, &mut cursor, tx)
                {
                    if tx.send(Live::Lost(error.to_string())).is_err() {
                        return;
                    }
                    if error.to_string().contains("serverSession mismatch")
                        || error.to_string().contains("event receiver was dropped")
                    {
                        return;
                    }
                } else if tx.send(Live::Lost("Inbox event stream closed; reconnecting".to_owned())).is_err() {
                    return;
                }
            }
            Err(error) => {
                if tx.send(Live::Lost(format!("Inbox event stream: {error}"))).is_err() {
                    return;
                }
            }
        }
        thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_secs(8));
    }
}

fn read_events<R: BufRead>(
    mut reader: R,
    server_session: &str,
    cursor: &mut u64,
    tx: &Sender<Live>,
) -> Result<()> {
    let mut line = String::new();
    let mut event = String::new();
    let mut data = String::new();
    let mut event_id = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line).context("reading Inbox event stream")?;
        if read == 0 {
            return Ok(());
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if let Some(next) = dispatch_event(&event, &data, &event_id, server_session, cursor, tx)? {
                return Err(anyhow!("Inbox {next}"));
            }
            event.clear();
            data.clear();
            event_id.clear();
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => {
                event.clear();
                event.push_str(value);
            }
            "data" => {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(value);
            }
            "id" if !value.contains('\0') => {
                event_id.clear();
                event_id.push_str(value);
            }
            _ => {}
        }
    }
}

fn dispatch_event(
    event: &str,
    data: &str,
    event_id: &str,
    server_session: &str,
    cursor: &mut u64,
    tx: &Sender<Live>,
) -> Result<Option<&'static str>> {
    if data.is_empty() {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(data).context("parsing Inbox event JSON")?;
    if event == "hello" {
        let Some(session) = value.get("serverSession").and_then(Value::as_str) else {
            return Ok(Some("hello omitted serverSession"));
        };
        if session != server_session {
            return Ok(Some("serverSession mismatch; rediscover the Inbox"));
        }
        let seen_cursor = value.get("cursor").and_then(Value::as_u64).unwrap_or(*cursor);
        if tx.send(Live::Connected { cursor: seen_cursor }).is_err() {
            return Ok(Some("event receiver was dropped"));
        }
        if tx.send(Live::Refresh).is_err() {
            return Ok(Some("event receiver was dropped"));
        }
        return Ok(None);
    }
    if event == "record" {
        let seq = event_id.parse::<u64>().ok().or_else(|| value.get("seq").and_then(Value::as_u64));
        let Some(seq) = seq else { return Ok(None) };
        let kind = value.get("kind").and_then(Value::as_str).unwrap_or_default().to_owned();
        let id = value.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
        let thread_id = value
            .get("thread_id")
            .and_then(Value::as_str)
            .or_else(|| value.pointer("/message/thread_id").and_then(Value::as_str))
            .map(str::to_owned);
        *cursor = (*cursor).max(seq);
        if tx.send(Live::Record { seq, kind, id, thread_id }).is_err() {
            return Ok(Some("event receiver was dropped"));
        }
        return Ok(None);
    }
    if matches!(
        event,
        "rows" | "pins" | "snoozes" | "machines" | "drafts" | "deliveries" | "reach" | "status" | "updated"
    ) && tx.send(Live::Refresh).is_err()
    {
        return Ok(Some("event receiver was dropped"));
    }
    Ok(None)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing, reason = "tests assert by panicking")]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn recorded_list_and_thread_wire_responses_deserialize() {
        let list: List =
            serde_json::from_str(include_str!("fixtures/list.json")).expect("recorded list response");
        assert_eq!(list.cursor, 3);
        assert_eq!(list.sections[2].threads[0].questions.open, 1);
        assert_eq!(list.sections[2].threads[0].project.name, "Example project");
        let detail: ThreadModel =
            serde_json::from_str(include_str!("fixtures/thread.json")).expect("recorded thread response");
        assert_eq!(detail.server_session, "fixture-session");
        assert_eq!(detail.thread.messages[0].questions[0].choices[1].label, "Second");
        assert_eq!(detail.thread.messages[0].questions[0].key, "q-fixture");
    }

    #[test]
    fn pick_none_omits_fields_and_explicit_null_clears() {
        let saved = Pick { message_id: None, key: "q-fixture".to_owned(), revision: 2, answer: None };
        assert_eq!(serde_json::to_value(saved).expect("pick json"), json!({"key":"q-fixture","revision":2}));
        let clear = Pick {
            message_id: Some("msg_other".to_owned()),
            key: "q-fixture".to_owned(),
            revision: 2,
            answer: Some(Value::Null),
        };
        assert_eq!(
            serde_json::to_value(clear).expect("clear json"),
            json!({"message_id":"msg_other","key":"q-fixture","revision":2,"answer":null})
        );
        let skip = Skip { message_id: None, key: "q-fixture".to_owned() };
        assert_eq!(serde_json::to_value(skip).expect("skip json"), json!({"key":"q-fixture"}));
    }

    #[test]
    fn recorded_sse_uses_live_parser_and_reconnect_frame_contract() {
        let (tx, rx) = mpsc::channel();
        let mut cursor = 0;
        read_events(Cursor::new(include_str!("fixtures/events.txt")), "fixture-session", &mut cursor, &tx)
            .expect("parse SSE");
        assert_eq!(cursor, 3);
        assert_eq!(rx.recv().expect("connected"), Live::Connected { cursor: 3 });
        assert_eq!(rx.recv().expect("refresh"), Live::Refresh);
        assert!(
            matches!(rx.recv().expect("project record"), Live::Record { seq: 1, kind, .. } if kind == "project")
        );
        assert!(
            matches!(rx.recv().expect("message record"), Live::Record { seq: 2, thread_id: Some(thread_id), .. } if thread_id == "msg_fixture")
        );
        assert!(
            matches!(rx.recv().expect("question record"), Live::Record { seq: 3, thread_id: Some(thread_id), .. } if thread_id == "msg_fixture")
        );
    }

    #[test]
    fn stale_session_hello_stops_replay() {
        let (tx, _rx) = mpsc::channel();
        let mut cursor = 9;
        let error = dispatch_event(
            "hello",
            r#"{"serverSession":"other","cursor":15}"#,
            "",
            "fixture-session",
            &mut cursor,
            &tx,
        )
        .expect("well-formed hello");
        assert_eq!(error, Some("serverSession mismatch; rediscover the Inbox"));
        assert_eq!(cursor, 9);
    }
}
