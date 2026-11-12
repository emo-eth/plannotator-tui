use std::cell::RefCell;
use std::error::Error as StdError;
use std::fmt;
use std::io::{BufRead, BufReader};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ureq::{Agent, AgentBuilder, Response};
use url::Url;

use super::actions::QuestionsResponse;
use super::questions::Question;

#[derive(Debug)]
pub(crate) struct Client {
    url: String,
    server_session: String,
    version: Option<String>,
    last_cursor: Arc<AtomicU64>,
    routes: RefCell<super::remote::Routes>,
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
    version: Option<String>,
    #[serde(default)]
    follows: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct List {
    #[serde(rename = "serverSession")]
    pub(crate) server_session: String,
    pub(crate) cursor: u64,
    pub(crate) notice: Option<String>,
    pub(crate) sections: Vec<Section>,
    #[serde(default)]
    pub(crate) pins: Vec<Row>,
    #[serde(default)]
    pub(crate) snoozed: Vec<Row>,
    #[serde(default)]
    pub(crate) agents: Vec<Value>,
    #[serde(rename = "self", default)]
    pub(crate) self_machine: Option<String>,
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
    pub(crate) questions: QuestionSummary,
    pub(crate) waiting_since: Option<String>,
    pub(crate) machine: Option<String>,
    pub(crate) addressee: Option<Addressee>,
    pub(crate) participants: Vec<Participant>,
    pub(crate) last_agent_at: Option<String>,
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

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Participant {
    pub(crate) session: String,
}
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ThreadModel {
    #[serde(rename = "serverSession")]
    pub(crate) server_session: String,
    pub(crate) cursor: u64,
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
    pub(crate) seq: Option<u64>,
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
    pub(crate) payload: Value,
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
    Restarted { session: String, cursor: u64 },
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

    pub(super) fn url(&self) -> &str {
        &self.url
    }

    pub(super) fn server_session(&self) -> &str {
        &self.server_session
    }
    pub(crate) fn list(&self) -> Result<List> {
        let mut list: List =
            self.get_json("GET", "/api/inbox/threads?machines=1&archived=exclude&snoozed=exclude")?;
        anyhow::ensure!(
            list.server_session == self.server_session,
            "GET {}: Inbox serverSession changed; rediscover before continuing",
            self.url
        );
        self.last_cursor.fetch_max(list.cursor, Ordering::Relaxed);
        if !list.snoozed.is_empty() {
            list.sections.push(Section {
                id: "snoozed".to_owned(),
                label: "Snoozed".to_owned(),
                threads: std::mem::take(&mut list.snoozed),
            });
        }
        if !list.pins.is_empty() {
            list.sections.insert(
                0,
                Section {
                    id: "pinned".to_owned(),
                    label: "Starred".to_owned(),
                    threads: std::mem::take(&mut list.pins),
                },
            );
        }
        Ok(list)
    }

    pub(crate) fn thread(&self, thread_id: &str) -> Result<ThreadModel> {
        let path = format!("/api/inbox/threads/{}", path_id(thread_id)?);
        let thread: ThreadModel = self.get_json("GET", &path)?;
        if self.machine_of(thread_id)?.is_none() {
            anyhow::ensure!(
                thread.server_session == self.server_session,
                "GET {}: Inbox serverSession changed; rediscover before continuing",
                endpoint(&self.url, &path)?
            );
            self.last_cursor.fetch_max(thread.cursor, Ordering::Relaxed);
        }
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

    pub(crate) fn subscribe(&self, cursor: u64) -> Receiver<Live> {
        let (tx, rx) = mpsc::channel();
        let url = self.url.clone();
        let server_session = self.server_session.clone();
        let version = self.version.clone();
        let latest_read = Arc::clone(&self.last_cursor);
        latest_read.fetch_max(cursor, Ordering::Relaxed);
        thread::spawn(move || {
            event_worker(&url, &server_session, version.as_deref(), cursor, &latest_read, &tx);
        });
        rx
    }

    fn connect(url: &str, expected_session: Option<&str>) -> Result<Self> {
        let parsed = Url::parse(url).with_context(|| format!("parsing Inbox URL {url}"))?;
        anyhow::ensure!(
            matches!(parsed.scheme(), "http" | "https"),
            "Inbox URL must use HTTP or HTTPS: {url}"
        );
        anyhow::ensure!(parsed.host_str().is_some(), "Inbox URL has no host: {url}");
        let url = parsed.to_string();
        let agent = AgentBuilder::new()
            .timeout_connect(Duration::from_secs(3))
            .timeout_read(Duration::from_secs(20))
            .redirects(0)
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
        Ok(Self {
            url,
            server_session: health.server_session,
            version: health.version,
            last_cursor: Arc::new(AtomicU64::new(0)),
            routes: RefCell::new(super::remote::Routes::default()),
            agent,
        })
    }

    pub(super) fn adopt_session(&mut self, session: &str, cursor: u64) -> Result<()> {
        anyhow::ensure!(!session.is_empty(), "Inbox restart hello omitted serverSession");
        anyhow::ensure!(session != self.server_session, "Inbox restart session did not change");
        anyhow::ensure!(
            cursor >= self.last_cursor.load(Ordering::Relaxed),
            "Inbox restart cursor is behind the latest read"
        );
        let health: Health = self.get_json("GET", "/api/inbox/health")?;
        anyhow::ensure!(
            health.server_session == session,
            "Inbox restart health belongs to another server run"
        );
        anyhow::ensure!(
            self.version.as_deref().is_some_and(|version| health.version.as_deref() == Some(version)),
            "Inbox restart release changed; reload the Inbox"
        );
        anyhow::ensure!(
            health.follows.iter().any(|previous| previous == &self.server_session),
            "Inbox restart does not follow this server session; rediscover before continuing"
        );
        session.clone_into(&mut self.server_session);
        self.last_cursor.fetch_max(cursor, Ordering::Relaxed);
        Ok(())
    }

    pub(crate) fn remember_displayed(&self, thread_id: &str, through_seq: u64) -> Result<()> {
        self.routes.borrow_mut().displayed(thread_id, through_seq)
    }

    pub(super) fn get_json<T: DeserializeOwned>(&self, method: &str, path: &str) -> Result<T> {
        let route = self.routes.borrow().resolve(path)?;
        let url = endpoint(&self.url, route.path())?;
        let origin = origin_of(&self.url)?;
        let response = self
            .agent
            .get(&url)
            .set("Accept", "application/json")
            .set("Origin", &origin)
            .set("Sec-Fetch-Site", "same-origin")
            .call();
        let (_status, response): (u16, Value) = match parse_response(response, method, &url) {
            Ok(response) => response,
            Err(error) => {
                let retry = if let Some(http) = error.downcast_ref::<HttpError>().filter(|_| method == "GET")
                {
                    self.routes.borrow_mut().local_retry(&route, http.status, &http.payload)?
                } else {
                    None
                };
                if let Some(path) = retry {
                    return self.get_json(method, &path);
                }
                return Err(error);
            }
        };
        self.routes.borrow_mut().learn_response(&route, &response)?;
        serde_json::from_value(response).with_context(|| format!("decoding {method} response from {url}"))
    }

    pub(super) fn get_bytes(&self, method: &str, path: &str) -> Result<Vec<u8>> {
        let route = self.routes.borrow().resolve(path)?;
        let url = endpoint(&self.url, route.path())?;
        let origin = origin_of(&self.url)?;
        let response =
            self.agent.get(&url).set("Origin", &origin).set("Sec-Fetch-Site", "same-origin").call();
        let response = match parse_raw_response(response, method, &url) {
            Ok(response) => response,
            Err(error) => {
                let retry = if let Some(http) = error.downcast_ref::<HttpError>().filter(|_| method == "GET")
                {
                    self.routes.borrow_mut().local_retry(&route, http.status, &http.payload)?
                } else {
                    None
                };
                if let Some(path) = retry {
                    return self.get_bytes(method, &path);
                }
                return Err(error);
            }
        };
        let mut bytes = Vec::new();
        let mut reader = response.into_reader();
        std::io::Read::read_to_end(&mut reader, &mut bytes)
            .with_context(|| format!("reading {method} response from {url}"))?;
        Ok(bytes)
    }

    pub(super) fn get_text(&self, method: &str, path: &str) -> Result<String> {
        String::from_utf8(self.get_bytes(method, path)?).context("Inbox response was not UTF-8")
    }

    pub(super) fn post_json<T: DeserializeOwned>(&self, path: &str, body: &Value) -> Result<T> {
        let route = self.routes.borrow().resolve_write(path, body)?;
        let mut body = body.clone();
        self.routes.borrow().prepare_write(&route, &mut body, &self.server_session, None)?;
        let url = endpoint(&self.url, route.path())?;
        let origin = origin_of(&self.url)?;
        let mut request = self
            .agent
            .post(&url)
            .set("Content-Type", "application/json")
            .set("Accept", "application/json")
            .set("Origin", &origin)
            .set("Sec-Fetch-Site", "same-origin");
        if let Some(thread) = self.routes.borrow().thread_context(&route) {
            request = request.set("X-Plannotator-Thread", thread);
        }
        let response = request.send_json(&body);
        let (status, value): (u16, Value) = parse_response(response, "POST", &url)?;
        self.routes.borrow_mut().decode_write(&route, status, value)
    }
    pub(super) fn machine_of(&self, id: &str) -> Result<Option<String>> {
        Ok(self.routes.borrow().machine_of(id)?.map(str::to_owned))
    }

    pub(super) fn prepare_upload_path(&self, path: &str, key: Option<&str>) -> Result<String> {
        let route = self.routes.borrow().resolve(path)?;
        super::remote::Routes::prepare_file_write(&route, &self.server_session, key)
    }

    pub(super) fn post_bytes(&self, path: &str, content_type: &str, bytes: &[u8]) -> Result<Value> {
        let routed_path = self.prepare_upload_path(path, None)?;
        let route = self.routes.borrow().resolve(&routed_path)?;
        let url = endpoint(&self.url, route.path())?;
        let origin = origin_of(&self.url)?;
        let mut request = self
            .agent
            .post(&url)
            .set("Content-Type", content_type)
            .set("Accept", "application/json")
            .set("Origin", &origin)
            .set("Sec-Fetch-Site", "same-origin");
        if let Some(thread) = self.routes.borrow().thread_context(&route) {
            request = request.set("X-Plannotator-Thread", thread);
        }
        let response = request.send_bytes(bytes);
        let (status, value): (u16, Value) = parse_response(response, "POST", &url)?;
        self.routes.borrow_mut().decode_write(&route, status, value)
    }
}
pub(super) fn path_id(value: &str) -> Result<&str> {
    anyhow::ensure!(
        !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
        "invalid Inbox id {value:?}"
    );
    Ok(value)
}
fn endpoint(base: &str, path: &str) -> Result<String> {
    let base = Url::parse(base).context("parsing Inbox base URL")?;
    let url = base.join(path).context("joining Inbox API path")?;
    anyhow::ensure!(url.origin() == base.origin(), "Inbox API path must stay on the same origin");
    Ok(url.into())
}

fn origin_of(url: &str) -> Result<String> {
    let url = Url::parse(url).context("parsing Inbox origin")?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
        "Inbox origin must use HTTP or HTTPS with a host"
    );
    Ok(url.origin().ascii_serialization())
}

fn parse_raw_response(
    response: std::result::Result<Response, ureq::Error>,
    method: &str,
    url: &str,
) -> Result<Response> {
    match response {
        Ok(response) => Ok(response),
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
            Err(anyhow::Error::new(HttpError { status, code, message, payload }))
                .with_context(|| format!("{method} {url}"))
        }
        Err(error) => Err(anyhow!(error)).with_context(|| format!("{method} {url}")),
    }
}

fn get_with<T: DeserializeOwned>(agent: &Agent, base: &str, method: &str, url: &str) -> Result<T> {
    let origin = origin_of(base)?;
    let response = agent
        .get(url)
        .set("Accept", "application/json")
        .set("Origin", &origin)
        .set("Sec-Fetch-Site", "same-origin")
        .call();
    parse_response(response, method, url).map(|(_, value)| value)
}
fn parse_response<T: DeserializeOwned>(
    response: std::result::Result<Response, ureq::Error>,
    method: &str,
    url: &str,
) -> Result<(u16, T)> {
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
            return Err(anyhow::Error::new(HttpError { status, code, message, payload }))
                .with_context(|| format!("{method} {url}"));
        }
        Err(error) => return Err(anyhow!(error)).with_context(|| format!("{method} {url}")),
    };
    let status = response.status();
    let value = response.into_json().with_context(|| format!("decoding {method} response from {url}"))?;
    Ok((status, value))
}

fn event_worker(
    url: &str,
    server_session: &str,
    expected_version: Option<&str>,
    mut cursor: u64,
    latest_read: &AtomicU64,
    tx: &Sender<Live>,
) {
    let agent = AgentBuilder::new()
        .timeout_connect(Duration::from_secs(3))
        .timeout_read(Duration::from_secs(20))
        .redirects(0)
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
            .set("Sec-Fetch-Site", "same-origin")
            .set("Last-Event-ID", &cursor.to_string())
            .call();
        match response {
            Ok(response) => {
                backoff = Duration::from_secs(1);
                if let Err(error) = read_events(
                    url,
                    BufReader::new(response.into_reader()),
                    server_session,
                    expected_version,
                    &mut cursor,
                    latest_read,
                    tx,
                ) {
                    if tx.send(Live::Lost(error.to_string())).is_err() {
                        return;
                    }
                    if error.to_string().contains("event receiver was dropped")
                        || error.to_string().contains("serverSession changed")
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
    url: &str,
    mut reader: R,
    server_session: &str,
    expected_version: Option<&str>,
    cursor: &mut u64,
    latest_read: &AtomicU64,
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
            let frame = EventFrame { kind: &event, data: &data, id: &event_id };
            if let Some(next) =
                dispatch_event(url, &frame, server_session, expected_version, cursor, latest_read, tx)?
            {
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

struct EventFrame<'a> {
    kind: &'a str,
    data: &'a str,
    id: &'a str,
}

fn dispatch_event(
    url: &str,
    event: &EventFrame<'_>,
    server_session: &str,
    expected_version: Option<&str>,
    cursor: &mut u64,
    latest_read: &AtomicU64,
    tx: &Sender<Live>,
) -> Result<Option<&'static str>> {
    if event.data.is_empty() {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(event.data).context("parsing Inbox event JSON")?;
    if event.kind == "hello" {
        let Some(session) = value.get("serverSession").and_then(Value::as_str) else {
            return Ok(Some("hello omitted serverSession"));
        };
        let Some(seen_cursor) = value.get("cursor").and_then(Value::as_u64) else {
            return Ok(Some("hello omitted cursor"));
        };
        if session != server_session {
            if seen_cursor < latest_read.load(Ordering::Relaxed) {
                return Ok(Some("Inbox restart cursor is behind the latest read"));
            }
            let health_url = endpoint(url, "/api/inbox/health")?;
            let agent = AgentBuilder::new()
                .timeout_connect(Duration::from_secs(3))
                .timeout_read(Duration::from_secs(3))
                .redirects(0)
                .build();
            let health: Health = get_with(&agent, url, "GET", &health_url)?;
            if health.server_session != session
                || expected_version.is_none_or(|version| health.version.as_deref() != Some(version))
                || !health.follows.iter().any(|previous| previous == server_session)
            {
                return Ok(Some("Inbox restart health did not prove same release and lineage"));
            }
            if tx.send(Live::Restarted { session: session.to_owned(), cursor: seen_cursor }).is_err() {
                return Ok(Some("event receiver was dropped"));
            }
            return Ok(Some("serverSession changed"));
        }
        if tx.send(Live::Connected { cursor: seen_cursor }).is_err() {
            return Ok(Some("event receiver was dropped"));
        }
        if tx.send(Live::Refresh).is_err() {
            return Ok(Some("event receiver was dropped"));
        }
        return Ok(None);
    }
    if event.kind == "record" {
        let seq = event.id.parse::<u64>().ok().or_else(|| value.get("seq").and_then(Value::as_u64));
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
        event.kind,
        "rows"
            | "pins"
            | "snoozes"
            | "machines"
            | "drafts"
            | "deliveries"
            | "reach"
            | "status"
            | "updated"
            | "thread-pulls"
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
        read_events(
            "http://127.0.0.1",
            Cursor::new(include_str!("fixtures/events.txt")),
            "fixture-session",
            None,
            &mut cursor,
            &AtomicU64::new(0),
            &tx,
        )
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
        use std::io::Write as _;
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("health fixture");
        let url = format!("http://{}", listener.local_addr().expect("health address"));
        let health = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("health request");
            let mut reader = BufReader::new(&mut stream);
            let mut header = String::new();
            loop {
                header.clear();
                assert!(
                    reader.read_line(&mut header).expect("read health request") > 0,
                    "request ended before its headers"
                );
                if header == "\r\n" {
                    break;
                }
            }
            let body = r#"{"serverSession":"other","version":"fixture-version","follows":[]}"#;
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).expect("write health response");
        });
        let (tx, rx) = mpsc::channel();
        let mut cursor = 9;
        let error = dispatch_event(
            &url,
            &EventFrame { kind: "hello", data: r#"{"serverSession":"other","cursor":15}"#, id: "" },
            "fixture-session",
            Some("fixture-version"),
            &mut cursor,
            &AtomicU64::new(9),
            &tx,
        )
        .expect("well-formed hello");
        health.join().expect("health fixture completed");
        assert_eq!(error, Some("Inbox restart health did not prove same release and lineage"));
        assert_eq!(cursor, 9);
        assert!(rx.try_recv().is_err());
    }
}
