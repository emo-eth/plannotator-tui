//! Machine-aware Inbox routes, shared by every Client transport.
//!
//! Learn ownership from the merged list before opening rows, and from successful
//! thread/file responses before using their child IDs. Local records always win;
//! competing remote claims fail closed. A transport failure never changes ownership.
//! Only an explicit `409 id_is_local` for the request's own ID permits a local retry.
//! The local serverSession guards the forward; a remote response's session/cursor
//! must never replace the local event-stream session/cursor. Call `displayed` after
//! rendering messages, not after fetching them: replies carry that message sequence,
//! never the remote store's global cursor. Pending/refused outbox receipts are errors,
//! not successful mutations, even for a caller requesting untyped JSON.
//! User-confirmed file uploads use `Client::upload_for_record`: its record owner
//! selects the actual file endpoint, and the caller retains one key for retries.

use std::borrow::Cow;
use std::collections::HashMap;

use anyhow::{Context, Result, bail, ensure};
use serde::de::DeserializeOwned;
use serde_json::Value;

const PREFIX: &str = "/api/inbox/";
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const ID_KEYS: &[&str] = &[
    "id",
    "thread_id",
    "message_id",
    "attachment_id",
    "annotation_id",
    "project_id",
    "upload_id",
    "markup_id",
];

#[derive(Debug)]
enum Owner {
    Local,
    Machine(String),
    Contested,
}

/// A prepared URL borrows only its input path, never the resolver's mutable state.
#[derive(Debug)]
pub(crate) struct Route<'a> {
    path: Cow<'a, str>,
    machine: Option<String>,
    named: Option<Cow<'a, str>>,
}

impl Route<'_> {
    pub(crate) fn path(&self) -> &str {
        &self.path
    }
    pub(crate) fn machine(&self) -> Option<&str> {
        self.machine.as_deref()
    }
    pub(crate) fn is_remote(&self) -> bool {
        self.machine.is_some()
    }

    fn tail(&self) -> Option<&str> {
        let path = self.path.strip_prefix(PREFIX)?;
        if self.is_remote() {
            path.strip_prefix("m/")?.split_once('/').map(|(_, tail)| tail)
        } else {
            Some(path)
        }
    }

    fn resource(&self) -> Option<(&str, &str, &str)> {
        let tail = self.tail()?.split('?').next()?;
        let (kind, rest) = tail.split_once('/')?;
        let (id, suffix) = rest.split_once('/').unwrap_or((rest, ""));
        Some((kind, id, suffix))
    }
}

#[derive(Debug, Default)]
pub(crate) struct Routes {
    self_machine: Option<String>,
    owners: HashMap<String, Owner>,
    thread_of: HashMap<String, String>,
    fetched_through: HashMap<String, u64>,
    displayed_through: HashMap<String, u64>,
}

impl Routes {
    /// Unknown IDs use the local endpoint, as upstream does. A known remote ID
    /// stays remote when its machine is offline, unlinked, or missing from a refresh.
    pub(crate) fn machine_of(&self, id: &str) -> Result<Option<&str>> {
        match self.owners.get(id) {
            Some(Owner::Contested) => bail!(
                "machine_conflict: two linked machines claim {id}; unlink the one that should not have it"
            ),
            Some(Owner::Machine(machine)) => Ok(Some(machine)),
            Some(Owner::Local) | None => Ok(None),
        }
    }

    pub(crate) fn resolve<'a>(&self, path: &'a str) -> Result<Route<'a>> {
        ensure!(
            path.starts_with('/') && !path.starts_with("//") && !path.contains('#'),
            "expected a same-origin API path"
        );
        let Some(tail) = path.strip_prefix(PREFIX) else {
            return Ok(Route { path: Cow::Borrowed(path), machine: None, named: None });
        };
        let (explicit, tail) = if let Some(forward) = tail.strip_prefix("m/") {
            let (machine, tail) =
                forward.split_once('/').context("forwarded Inbox route omitted its path")?;
            ensure!(machine_id(machine), "invalid Inbox machine id {machine:?}");
            (Some(machine), tail)
        } else {
            (None, tail)
        };
        let bare = tail.split('?').next().unwrap_or(tail);
        ensure!(!bare.split('/').any(unsafe_segment), "invalid Inbox route");
        let named = resource_id(bare);
        let learned = named.map(|id| self.machine_of(id)).transpose()?.flatten();
        let local = named.is_some_and(|id| matches!(self.owners.get(id), Some(Owner::Local)));
        let explicit = explicit.filter(|machine| self.self_machine.as_deref() != Some(*machine));
        if !local && let (Some(given), Some(learned)) = (explicit, learned) {
            ensure!(
                given == learned,
                "machine_conflict: route names {given}, but its record belongs to {learned}"
            );
        }
        let machine = if local { None } else { learned.or(explicit) };
        let routed = match machine {
            Some(machine) if explicit == Some(machine) && path.starts_with("/api/inbox/m/") => {
                Cow::Borrowed(path)
            }
            Some(machine) => Cow::Owned(format!("{PREFIX}m/{machine}/{tail}")),
            None if path.starts_with("/api/inbox/m/") => Cow::Owned(format!("{PREFIX}{tail}")),
            None => Cow::Borrowed(path),
        };
        Ok(Route { path: routed, machine: machine.map(str::to_owned), named: named.map(Cow::Borrowed) })
    }

    /// Attachment comments name their owner in the body instead of the URL.
    pub(crate) fn resolve_write<'a>(&self, path: &'a str, body: &Value) -> Result<Route<'a>> {
        let mut route = self.resolve(path)?;
        if route.tail().and_then(|tail| tail.split('?').next()) != Some("annotations") {
            return Ok(route);
        }
        let Some(id) = body.get("attachment_id").and_then(Value::as_str) else { return Ok(route) };
        let machine = self.machine_of(id)?;
        let local = matches!(self.owners.get(id), Some(Owner::Local));
        if let (Some(explicit), Some(learned)) = (route.machine(), machine) {
            ensure!(explicit == learned, "machine_conflict: attachment belongs to another machine");
        }
        if local {
            route.path = Cow::Owned(format!("{PREFIX}{}", route.tail().context("missing annotation route")?));
            route.machine = None;
        } else if let Some(machine) = machine.filter(|machine| route.machine() != Some(*machine)) {
            route.path = Cow::Owned(format!(
                "{PREFIX}m/{machine}/{}",
                route.tail().context("missing annotation route")?
            ));
            route.machine = Some(machine.to_owned());
        }
        route.named = Some(Cow::Owned(id.to_owned()));
        Ok(route)
    }

    /// Learn only successful responses. In particular, a local draft that mentions
    /// a remote project is not proof that the project belongs to this machine.
    pub(crate) fn learn_response(&mut self, route: &Route<'_>, value: &Value) -> Result<()> {
        if let Some(machine) = route.machine() {
            self.learn_ids(value, Some(machine), false, 0);
        } else {
            let tail = route.tail().and_then(|tail| tail.split('?').next()).unwrap_or("");
            if matches!(tail, "threads" | "threads/archived" | "projects" | "machines") {
                if let Some(machine) = value.get("self").and_then(Value::as_str) {
                    ensure!(machine_id(machine), "merged Inbox list has an invalid self machine id");
                    self.self_machine = Some(machine.to_owned());
                }
                // First establish local ownership throughout the list. Order of
                // duplicate local and remote rows must not affect their destination.
                self.learn_ids(value, None, true, 0);
                self.learn_machine_records(value, 0)?;
            } else if route.resource().is_some_and(|(kind, _, _)| kind == "threads") {
                self.learn_ids(value, None, true, 0);
            }
        }
        let thread = route.resource().filter(|(kind, _, _)| *kind == "threads").map(|(_, id, _)| id);
        self.learn_context(value, thread, 0);
        if let (Some(("threads", id, "")), Some(model)) = (route.resource(), value.get("thread")) {
            if let Some(actual) = model.get("thread_id").and_then(Value::as_str) {
                ensure!(actual == id, "Inbox returned another thread for {id}");
            }
            if let Some(messages) = model.get("messages").and_then(Value::as_array)
                && let Some(seq) =
                    messages.iter().filter_map(|message| message.get("seq").and_then(Value::as_u64)).max()
            {
                ensure!(seq <= MAX_SAFE_INTEGER, "message sequence exceeds the Inbox's integer range");
                self.fetched_through.insert(id.to_owned(), seq);
            }
        }
        Ok(())
    }

    /// A renderer reports only messages actually on screen. Fetching a newer
    /// snapshot does not silently advance this watermark or a queued reply's cap.
    pub(crate) fn displayed(&mut self, thread_id: &str, through_seq: u64) -> Result<()> {
        ensure!(through_seq <= MAX_SAFE_INTEGER, "displayed sequence exceeds the Inbox's integer range");
        if self.machine_of(thread_id)?.is_none() {
            return Ok(());
        }
        let fetched =
            self.fetched_through.get(thread_id).context("remote thread has no fetched message sequences")?;
        ensure!(through_seq <= *fetched, "displayed sequence exceeds the newest fetched message");
        self.displayed_through
            .entry(thread_id.to_owned())
            .and_modify(|seq| *seq = (*seq).max(through_seq))
            .or_insert(through_seq);
        Ok(())
    }

    pub(crate) fn thread_context<'a>(&'a self, route: &'a Route<'_>) -> Option<&'a str> {
        if !route.is_remote() {
            return None;
        }
        if let Some(("threads", id, _)) = route.resource() {
            return Some(id);
        }
        self.thread_of.get(route.named.as_deref()?).map(String::as_str)
    }

    /// `fallback_key` belongs to the caller's staged operation, not a retry. An
    /// existing key is never replaced. Explicit older reply caps survive retries.
    pub(crate) fn prepare_write(
        &self,
        route: &Route<'_>,
        body: &mut Value,
        local_session: &str,
        fallback_key: Option<&str>,
    ) -> Result<()> {
        ensure!(!local_session.is_empty(), "Inbox write requires the local serverSession");
        let body = body.as_object_mut().context("Inbox write body must be an object")?;
        body.insert("serverSession".to_owned(), Value::String(local_session.to_owned()));
        if !route.is_remote() {
            return Ok(());
        }
        if let Some(key) = body.get("idempotency_key") {
            ensure!(
                key.as_str().is_some_and(|key| !key.trim().is_empty()),
                "remote write requires a nonempty idempotency_key"
            );
        } else {
            let key = fallback_key
                .filter(|key| !key.trim().is_empty())
                .context("remote write requires a staged idempotency_key")?;
            body.insert("idempotency_key".to_owned(), Value::String(key.to_owned()));
        }
        if let Some(("messages", _, "reply")) = route.resource() {
            let thread = self
                .thread_context(route)
                .context("remote reply has no thread context; read and display its thread first")?;
            ensure!(
                self.machine_of(thread)? == route.machine(),
                "remote reply thread belongs to another machine"
            );
            let displayed = self
                .displayed_through
                .get(thread)
                .context("remote reply has no displayed message sequence; display its thread first")?;
            let seen = if let Some(value) = body.get("seen_through_seq") {
                let seq = value.as_u64().context("seen_through_seq must be a nonnegative safe integer")?;
                ensure!(
                    seq <= *displayed && seq <= MAX_SAFE_INTEGER,
                    "seen_through_seq exceeds the newest displayed message"
                );
                seq
            } else {
                *displayed
            };
            body.insert("seen_through_seq".to_owned(), Value::from(seen));
        }
        Ok(())
    }

    /// Raw uploads/markups carry the local session and remote operation key in
    /// their query. They are sent immediately or refused, never put in the outbox.
    pub(crate) fn prepare_file_write(
        route: &Route<'_>,
        local_session: &str,
        key: Option<&str>,
    ) -> Result<String> {
        ensure!(!local_session.is_empty(), "Inbox upload requires the local serverSession");
        let (path, query) = route.path().split_once('?').unwrap_or((route.path(), ""));
        let pairs = url::form_urlencoded::parse(query.as_bytes());
        let existing = pairs.clone().find(|(name, _)| name == "idempotency_key").map(|(_, value)| value);
        let key = existing.as_deref().or(key);
        if route.is_remote() {
            ensure!(
                key.is_some_and(|key| !key.trim().is_empty()),
                "remote upload requires a staged idempotency_key"
            );
        }
        let mut encoded = url::form_urlencoded::Serializer::new(String::new());
        for (name, value) in pairs {
            if name != "serverSession" && name != "idempotency_key" {
                encoded.append_pair(&name, &value);
            }
        }
        encoded.append_pair("serverSession", local_session);
        if let Some(key) = key {
            encoded.append_pair("idempotency_key", key);
        }
        Ok(format!("{path}?{}", encoded.finish()))
    }

    /// The only legitimate local reroute: the local server explicitly refused a
    /// forward because it owns this request's ID. Offline/404/timeout never retry.
    pub(crate) fn local_retry(
        &mut self,
        route: &Route<'_>,
        status: u16,
        payload: &Value,
    ) -> Result<Option<String>> {
        if !route.is_remote()
            || status != 409
            || payload.get("code").and_then(Value::as_str) != Some("id_is_local")
        {
            return Ok(None);
        }
        let id = payload.get("id").and_then(Value::as_str).context("id_is_local omitted its record id")?;
        ensure!(
            route.named.as_deref() == Some(id),
            "id_is_local did not name the requested record; refusing a local retry"
        );
        self.remember_local(id);
        Ok(Some(format!("{PREFIX}{}", route.tail().context("forwarded route omitted its path")?)))
    }

    /// Decode before the caller's response type: Value and permissive/defaulted
    /// structs must not mistake a 202 receipt for a successful write.
    pub(crate) fn decode_write<T: DeserializeOwned>(
        &mut self,
        route: &Route<'_>,
        status: u16,
        value: Value,
    ) -> Result<T> {
        if route.is_remote() && status == 202 {
            let kind = route.tail().and_then(|tail| tail.split(['/', '?']).next());
            ensure!(
                !matches!(kind, Some("uploads" | "markups")),
                "remote upload returned a held response; file delivery is unconfirmed and cannot be queued"
            );
            let entry = value
                .get("outbox")
                .filter(|entry| entry.is_object())
                .context("remote write returned 202 without an outbox receipt; delivery is unconfirmed")?;
            ensure!(
                entry.get("machine").and_then(Value::as_str) == route.machine(),
                "outbox receipt belongs to another machine"
            );
            if entry.get("state").and_then(Value::as_str) == Some("delivered") {
                ensure!(
                    entry.get("answer").is_some_and(|answer| !answer.is_null()),
                    "delivered outbox receipt omitted its answer; delivery cannot be decoded"
                );
            }
        }
        let answer: Value = super::machines::decode_write(value)?;
        self.learn_response(route, &answer)?;
        serde_json::from_value(answer).context("decoding Inbox write response")
    }

    fn remember_local(&mut self, id: &str) {
        if id.is_empty() || matches!(self.owners.get(id), Some(Owner::Local)) {
            return;
        }
        self.owners.insert(id.to_owned(), Owner::Local);
        self.displayed_through.remove(id);
    }

    fn remember_machine(&mut self, id: &str, machine: &str) {
        if id.is_empty() {
            return;
        }
        if self.self_machine.as_deref() == Some(machine) {
            self.remember_local(id);
            return;
        }
        match self.owners.get(id) {
            Some(Owner::Machine(held)) if held != machine => {
                self.owners.insert(id.to_owned(), Owner::Contested);
                self.displayed_through.remove(id);
            }
            Some(Owner::Local | Owner::Contested | Owner::Machine(_)) => {}
            None => {
                self.owners.insert(id.to_owned(), Owner::Machine(machine.to_owned()));
            }
        }
    }

    fn learn_ids(&mut self, value: &Value, machine: Option<&str>, only_local: bool, depth: usize) {
        if depth > 12 {
            return;
        }
        match value {
            Value::Array(items) => {
                for item in items {
                    self.learn_ids(item, machine, only_local, depth + 1);
                }
            }
            Value::Object(object) => {
                if only_local
                    && object
                        .get("machine")
                        .and_then(Value::as_str)
                        .is_some_and(|owner| self.self_machine.as_deref() != Some(owner))
                {
                    return;
                }
                for (key, value) in object {
                    if let Some(id) =
                        value.as_str().filter(|id| ID_KEYS.contains(&key.as_str()) && record_id(id))
                    {
                        if let Some(machine) = machine {
                            self.remember_machine(id, machine);
                        } else {
                            self.remember_local(id);
                        }
                    } else {
                        self.learn_ids(value, machine, only_local, depth + 1);
                    }
                }
            }
            _ => {}
        }
    }

    fn learn_machine_records(&mut self, value: &Value, depth: usize) -> Result<()> {
        if depth > 12 {
            return Ok(());
        }
        match value {
            Value::Array(items) => {
                for item in items {
                    self.learn_machine_records(item, depth + 1)?;
                }
            }
            Value::Object(object) => {
                if let Some(machine) = object.get("machine").and_then(Value::as_str) {
                    ensure!(machine_id(machine), "merged Inbox record has an invalid machine id");
                    self.learn_ids(value, Some(machine), false, depth);
                } else {
                    for child in object.values() {
                        self.learn_machine_records(child, depth + 1)?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn learn_context(&mut self, value: &Value, inherited: Option<&str>, depth: usize) {
        if depth > 12 {
            return;
        }
        match value {
            Value::Array(items) => {
                for item in items {
                    self.learn_context(item, inherited, depth + 1);
                }
            }
            Value::Object(object) => {
                let thread = object.get("thread_id").and_then(Value::as_str).or(inherited);
                if let Some(thread) = thread {
                    for key in ["id", "message_id", "attachment_id", "annotation_id"] {
                        if let Some(id) = object
                            .get(key)
                            .and_then(Value::as_str)
                            .filter(|id| record_id(id) && !id.starts_with("prj_"))
                        {
                            self.thread_of.entry(id.to_owned()).or_insert_with(|| thread.to_owned());
                        }
                    }
                }
                for child in object.values() {
                    self.learn_context(child, thread, depth + 1);
                }
            }
            _ => {}
        }
    }
}

impl super::client::Client {
    /// Upload bytes the person explicitly selected and confirmed. `record_id` is
    /// the reply's thread (or another already-read record on the destination);
    /// None is a new local compose. Keep `key` with the selected file across retries.
    /// This sends no message and makes no draft: retain the returned upload ID in
    /// the same destination's reply draft until the person sends or removes it.
    pub(crate) fn upload_for_record(
        &self,
        record_id: Option<&str>,
        name: &str,
        key: &str,
        bytes: &[u8],
    ) -> Result<Value> {
        ensure!(bytes.len() <= 100 * 1024 * 1024, "Inbox upload is larger than 100 MiB");
        let machine = record_id.map(|id| self.machine_of(id)).transpose()?.flatten();
        let path = record_upload_path(machine.as_deref(), name, self.server_session(), key)?;
        let value = self.post_bytes(&path, "application/octet-stream", bytes)?;
        confirm_upload(&value, name, bytes.len())?;
        Ok(value)
    }
}

fn record_upload_path(machine: Option<&str>, name: &str, session: &str, key: &str) -> Result<String> {
    ensure!(
        !name.trim().is_empty()
            && !matches!(name, "." | "..")
            && name.len() <= 255
            && !name.contains(['/', '\\'])
            && !name.chars().any(char::is_control),
        "upload must name one file, not a local path"
    );
    ensure!(!session.is_empty(), "Inbox upload requires the local serverSession");
    ensure!(!key.trim().is_empty(), "Inbox upload requires a staged idempotency key");
    if let Some(machine) = machine {
        ensure!(machine_id(machine), "invalid Inbox upload machine id");
    }
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("name", name)
        .append_pair("serverSession", session)
        .append_pair("idempotency_key", key)
        .finish();
    Ok(match machine {
        Some(machine) => format!("{PREFIX}m/{machine}/uploads?{query}"),
        None => format!("{PREFIX}uploads?{query}"),
    })
}

fn confirm_upload(value: &Value, name: &str, size: usize) -> Result<()> {
    ensure!(value.get("outbox").is_none(), "file upload cannot be held in the outbox");
    let upload = value
        .get("upload")
        .filter(|upload| upload.is_object())
        .context("Inbox upload omitted its saved-file receipt")?;
    let id = upload.get("upload_id").and_then(Value::as_str).context("Inbox upload omitted upload_id")?;
    ensure!(id.starts_with("upl_") && record_id(id), "Inbox returned an invalid upload id");
    ensure!(
        upload.get("name").and_then(Value::as_str) == Some(name),
        "Inbox upload receipt names another file"
    );
    ensure!(
        upload.get("size").and_then(Value::as_u64) == Some(size as u64),
        "Inbox upload receipt has another byte count"
    );
    Ok(())
}

fn unsafe_segment(part: &str) -> bool {
    part.is_empty()
        || part == "."
        || part == ".."
        || part.contains('\\')
        || part.as_bytes().windows(3).any(|bytes| {
            bytes.eq_ignore_ascii_case(b"%2f")
                || bytes.eq_ignore_ascii_case(b"%2e")
                || bytes.eq_ignore_ascii_case(b"%5c")
        })
}

fn machine_id(id: &str) -> bool {
    id.strip_prefix("ibx_").is_some_and(|suffix| suffix.len() == 26 && suffix.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'A'..=b'H' | b'J'..=b'K' | b'M'..=b'N' | b'P'..=b'T' | b'V'..=b'Z')))
}

fn record_id(id: &str) -> bool {
    (16..=80).contains(&id.len())
        && !id.starts_with("ibx_")
        && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn resource_id(tail: &str) -> Option<&str> {
    let (kind, rest) = tail.split_once('/')?;
    if !matches!(kind, "threads" | "messages" | "attachments" | "projects" | "annotations" | "uploads") {
        return None;
    }
    let id = rest.split('/').next()?;
    record_id(id).then_some(id)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing, reason = "tests assert protocol invariants")]
mod tests {
    use super::*;
    use serde_json::json;

    const SELF: &str = "ibx_00000000000000000000000001";
    const MAC: &str = "ibx_00000000000000000000000002";
    const OTHER: &str = "ibx_00000000000000000000000003";
    const THREAD: &str = "msg_00000000000000000000000001";
    const MESSAGE: &str = "msg_00000000000000000000000002";
    const FILE: &str = "att_00000000000000000000000001";
    const PROJECT: &str = "prj_00000000000000000000000001";

    fn learn(routes: &mut Routes, path: &str, value: &Value) {
        let route = routes.resolve(path).expect("route");
        routes.learn_response(&route, value).expect("response");
    }

    fn remote() -> Routes {
        let mut routes = Routes::default();
        learn(
            &mut routes,
            "/api/inbox/threads?machines=1",
            &json!({
                "self":SELF, "sections":[{"threads":[{"thread_id":THREAD,"machine":MAC}]}],
                "projects":[{"id":PROJECT,"machine":MAC}]
            }),
        );
        routes
    }

    fn thread(routes: &mut Routes) {
        learn(
            routes,
            &format!("/api/inbox/threads/{THREAD}"),
            &json!({
                "serverSession":"remote-run", "cursor":900,
                "thread":{"thread_id":THREAD,"project":{"id":PROJECT},"messages":[
                    {"id":THREAD,"seq":12},{"id":MESSAGE,"seq":21,"attachments":[{"id":FILE}]}
                ]}
            }),
        );
    }

    #[test]
    fn merged_routes_learn_children_and_keep_query_and_local_endpoints() {
        let mut routes = remote();
        thread(&mut routes);
        for (kind, id) in
            [("threads", THREAD), ("projects", PROJECT), ("messages", MESSAGE), ("attachments", FILE)]
        {
            let path = format!("/api/inbox/{kind}/{id}/view?version=sent&path=a%20b");
            let route = routes.resolve(&path).expect("resolve");
            assert_eq!(route.path(), format!("/api/inbox/m/{MAC}/{kind}/{id}/view?version=sent&path=a%20b"));
        }
        for path in [
            "/api/inbox/settings",
            "/api/inbox/threads?machines=1",
            "/api/shapes",
            "/api/html-assets/token/file",
        ] {
            assert_eq!(routes.resolve(path).expect("local").path(), path);
        }
        let path = format!("/api/inbox/m/{SELF}/sessions");
        assert_eq!(routes.resolve(&path).expect("self").path(), "/api/inbox/sessions");
    }

    #[test]
    fn local_records_win_independently_of_list_order_and_later_claims() {
        for local_first in [false, true] {
            let mut routes = remote();
            let local = json!({"thread_id":THREAD,"machine":SELF,"project":{"id":PROJECT}});
            let far = json!({"thread_id":THREAD,"machine":MAC});
            let rows = if local_first { vec![local, far] } else { vec![far, local] };
            learn(
                &mut routes,
                "/api/inbox/threads?machines=1",
                &json!({"self":SELF,"sections":[{"threads":rows}]}),
            );
            assert_eq!(routes.machine_of(THREAD).expect("owner"), None);
            assert_eq!(routes.machine_of(PROJECT).expect("project owner"), None);
            let explicit = format!("/api/inbox/m/{MAC}/threads/{THREAD}");
            assert_eq!(
                routes.resolve(&explicit).expect("local wins").path(),
                format!("/api/inbox/threads/{THREAD}")
            );
        }
    }

    #[test]
    fn remote_collisions_are_not_first_last_or_local_wins() {
        let mut routes = remote();
        learn(
            &mut routes,
            "/api/inbox/threads?machines=1",
            &json!({"sections":[{"threads":[{"thread_id":THREAD,"machine":OTHER}]}]}),
        );
        assert!(routes.resolve(&format!("/api/inbox/threads/{THREAD}")).is_err());
        assert!(routes.resolve(&format!("/api/inbox/m/{MAC}/threads/{THREAD}")).is_err());
        learn(
            &mut routes,
            "/api/inbox/threads?machines=1",
            &json!({"sections":[{"threads":[{"thread_id":THREAD}]}]}),
        );
        assert_eq!(routes.machine_of(THREAD).expect("local evidence resolves collision"), None);
    }

    #[test]
    fn arbitrary_local_replies_do_not_steal_remote_project_ids() {
        let mut routes = remote();
        learn(&mut routes, "/api/inbox/drafts", &json!({"drafts":[{"project":{"id":PROJECT}}]}));
        learn(&mut routes, "/api/inbox/threads?machines=1", &json!({"sections":[],"projects":[]}));
        assert_eq!(routes.machine_of(PROJECT).expect("owner survives"), Some(MAC));
        assert_eq!(routes.machine_of(THREAD).expect("missing row is not local"), Some(MAC));
    }

    #[test]
    fn replies_require_displayed_message_sequence_not_global_cursor() {
        let mut routes = remote();
        thread(&mut routes);
        let path = format!("/api/inbox/messages/{MESSAGE}/reply");
        let route = routes.resolve(&path).expect("reply");
        let mut body =
            json!({"words":"keep these","idempotency_key":"same-key","serverSession":"remote-run"});
        assert!(routes.prepare_write(&route, &mut body, "local-run", None).is_err());
        assert!(routes.displayed(THREAD, 900).is_err());
        routes.displayed(THREAD, 12).expect("first message displayed");
        routes.prepare_write(&route, &mut body, "local-run", None).expect("prepared");
        assert_eq!(body["seen_through_seq"], 12);
        assert_eq!(body["serverSession"], "local-run");
        assert_eq!(routes.thread_context(&route), Some(THREAD));
        routes.displayed(THREAD, 21).expect("next message displayed");
        routes.prepare_write(&route, &mut body, "local-run", Some("different-key")).expect("retry");
        assert_eq!(body["idempotency_key"], "same-key");
        assert_eq!(body["seen_through_seq"], 12);
        body["seen_through_seq"] = json!(900);
        assert!(routes.prepare_write(&route, &mut body, "local-run", None).is_err());
    }

    #[test]
    fn refresh_does_not_mark_undisplayed_arrivals_seen() {
        let mut routes = remote();
        thread(&mut routes);
        routes.displayed(THREAD, 21).expect("displayed");
        learn(
            &mut routes,
            &format!("/api/inbox/threads/{THREAD}"),
            &json!({"cursor":1000,"thread":{"thread_id":THREAD,"messages":[{"id":MESSAGE,"seq":55}]}}),
        );
        let path = format!("/api/inbox/messages/{MESSAGE}/reply");
        let route = routes.resolve(&path).expect("route");
        let mut body = json!({});
        routes.prepare_write(&route, &mut body, "local", Some("staged-key")).expect("prepared");
        assert_eq!(body["seen_through_seq"], 21);
    }

    #[test]
    fn annotations_resolve_body_owner_and_upload_query_retains_key() {
        let mut routes = remote();
        thread(&mut routes);
        let route = routes
            .resolve_write("/api/inbox/annotations", &json!({"attachment_id":FILE}))
            .expect("annotation");
        assert_eq!(route.path(), format!("/api/inbox/m/{MAC}/annotations"));
        assert_eq!(routes.thread_context(&route), Some(THREAD));
        let path =
            format!("/api/inbox/m/{MAC}/uploads?name=a%20b.png&serverSession=wrong&idempotency_key=old-key");
        let route = routes.resolve(&path).expect("upload");
        let path = Routes::prepare_file_write(&route, "local run", Some("new-key")).expect("file query");
        let url = url::Url::parse(&format!("http://localhost{path}")).expect("url");
        let params: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(params.get("name").map(String::as_str), Some("a b.png"));
        assert_eq!(params.get("serverSession").map(String::as_str), Some("local run"));
        assert_eq!(params.get("idempotency_key").map(String::as_str), Some("old-key"));
    }

    #[test]
    fn no_silent_local_fallback_on_offline_missing_or_wrong_id() {
        let mut routes = remote();
        let path = format!("/api/inbox/threads/{THREAD}");
        let route = routes.resolve(&path).expect("route");
        for (status, code) in
            [(503, "machine_offline"), (404, "machine_not_linked"), (409, "server_session_mismatch")]
        {
            assert!(
                routes
                    .local_retry(&route, status, &json!({"code":code,"id":THREAD}))
                    .expect("no retry")
                    .is_none()
            );
        }
        assert!(routes.local_retry(&route, 409, &json!({"code":"id_is_local","id":MESSAGE})).is_err());
        assert_eq!(routes.machine_of(THREAD).expect("still remote"), Some(MAC));
        assert_eq!(
            routes
                .local_retry(&route, 409, &json!({"code":"id_is_local","id":THREAD}))
                .expect("confirmed local"),
            Some(path)
        );
        assert_eq!(routes.machine_of(THREAD).expect("local"), None);
    }

    #[test]
    fn held_refused_cancelled_and_uncertain_receipts_never_succeed_as_json() {
        let mut routes = remote();
        let path = format!("/api/inbox/threads/{THREAD}/read");
        let route = routes.resolve(&path).expect("route");
        for state in ["queued", "sending", "refused", "cancelled", "unlinked"] {
            let result = routes.decode_write::<Value>(&route, 202, json!({"outbox":{"machine":MAC,"state":state,"key":"stable","words":"saved words","answer":{"error":"refused"}}}));
            let error = result.expect_err("receipt is not success");
            if matches!(state, "queued" | "sending") {
                assert_eq!(
                    error.downcast_ref::<super::super::machines::HeldWrite>().expect("held").entry.key,
                    "stable"
                );
            } else {
                assert_eq!(
                    error
                        .downcast_ref::<super::super::machines::UndeliveredWrite>()
                        .expect("terminal")
                        .entry
                        .words
                        .as_deref(),
                    Some("saved words")
                );
            }
        }
        assert!(routes.decode_write::<Value>(&route, 202, json!({"ok":true})).is_err());
        assert!(
            routes
                .decode_write::<Value>(&route, 202, json!({"outbox":{"machine":MAC,"state":"delivered"}}))
                .is_err()
        );
        let upload_path = format!("/api/inbox/m/{MAC}/uploads?name=file.png");
        let upload = routes.resolve(&upload_path).expect("upload route");
        assert!(
            routes
                .decode_write::<Value>(&upload, 202, json!({"outbox":{"machine":MAC,"state":"queued"}}))
                .is_err()
        );
        assert!(
            routes
                .decode_write::<Value>(
                    &route,
                    202,
                    json!({"outbox":{"machine":OTHER,"state":"delivered","answer":{"ok":true}}})
                )
                .is_err()
        );
        let answer = routes.decode_write::<Value>(&route, 202, json!({"outbox":{"machine":MAC,"state":"delivered","answer":{"ok":true,"attachment_id":FILE}}})).expect("confirmed");
        assert_eq!(answer["ok"], true);
        assert_eq!(routes.machine_of(FILE).expect("confirmed child owner"), Some(MAC));
    }

    #[test]
    fn record_upload_routes_exact_bytes_to_owner_with_stable_query_key() -> Result<()> {
        let mut routes = remote();
        thread(&mut routes);
        let machine = routes.machine_of(THREAD)?;
        let first = record_upload_path(machine, "report & résumé.txt", "local?run", "stable/key")?;
        assert_eq!(first, record_upload_path(machine, "report & résumé.txt", "local?run", "stable/key")?);
        let url = url::Url::parse(&format!("http://localhost{first}"))?;
        assert_eq!(url.path(), format!("/api/inbox/m/{MAC}/uploads"));
        let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(query.get("name").map(String::as_str), Some("report & résumé.txt"));
        assert_eq!(query.get("serverSession").map(String::as_str), Some("local?run"));
        assert_eq!(query.get("idempotency_key").map(String::as_str), Some("stable/key"));
        assert!(record_upload_path(machine, "/private/report.txt", "run", "key").is_err());
        assert!(record_upload_path(machine, "report.txt", "run", "").is_err());
        assert!(record_upload_path(None, "report.txt", "run", "key")?.starts_with("/api/inbox/uploads?"));
        Ok(())
    }

    #[test]
    fn upload_receipt_must_confirm_file_not_held_or_fabricated_success() -> Result<()> {
        let saved =
            json!({"upload":{"upload_id":"upl_00000000000000000000000001","name":"file.txt","size":3}});
        confirm_upload(&saved, "file.txt", 3)?;
        assert!(confirm_upload(&saved, "other.txt", 3).is_err());
        assert!(confirm_upload(&saved, "file.txt", 4).is_err());
        assert!(confirm_upload(&json!({"ok":true}), "file.txt", 3).is_err());
        assert!(confirm_upload(&json!({"outbox":{"state":"queued"}}), "file.txt", 3).is_err());
        Ok(())
    }

    #[test]
    fn confirmed_remote_upload_uses_forwarded_binary_transport() -> Result<()> {
        use std::io::{BufRead as _, Read as _, Write as _};
        use std::net::TcpListener;
        use std::time::{Duration, Instant};

        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let bytes = b"selected\0file\xff";
        let receipt = json!({"upload":{"upload_id":"upl_00000000000000000000000001","name":"selected.bin","size":bytes.len()}});
        let expected = receipt.clone();
        let server = std::thread::spawn(move || -> Result<Vec<(String, Vec<u8>)>> {
            let answers = [
                json!({"serverSession":"local-run","version":"0.1"}),
                json!({"self":SELF,"sections":[{"threads":[{"thread_id":THREAD,"machine":MAC}]}]}),
                receipt,
            ];
            let mut requests = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(5);
            for answer in answers {
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            ensure!(Instant::now() < deadline, "upload test did not receive its request");
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        Err(error) => return Err(error.into()),
                    }
                };
                stream.set_nonblocking(false)?;
                stream.set_read_timeout(Some(Duration::from_secs(3)))?;
                let mut reader = std::io::BufReader::new(stream.try_clone()?);
                let mut headers = String::new();
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    ensure!(reader.read_line(&mut line)? > 0, "request ended before its headers");
                    if let Some((name, value)) = line.split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse::<usize>()?;
                    }
                    headers.push_str(&line);
                    if line == "\r\n" {
                        break;
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body)?;
                requests.push((headers, body));
                let body = answer.to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )?;
            }
            Ok(requests)
        });
        let client = super::super::client::Client::from_url(&format!("http://{address}"))?;
        let _: Value = client.get_json("GET", "/api/inbox/threads?machines=1")?;
        assert_eq!(
            client.upload_for_record(Some(THREAD), "selected.bin", "staged-upload-key", bytes)?,
            expected
        );
        let requests = server.join().map_err(|_| anyhow::anyhow!("upload test server panicked"))??;
        let (headers, sent) = requests.get(2).context("missing remote upload")?;
        let path = record_upload_path(Some(MAC), "selected.bin", "local-run", "staged-upload-key")?;
        assert!(headers.starts_with(&format!("POST {path} HTTP/1.1\r\n")), "{headers}");
        assert!(headers.to_ascii_lowercase().contains("content-type: application/octet-stream"));
        assert_eq!(sent, bytes);
        Ok(())
    }

    #[test]
    fn malformed_machine_and_path_are_refused_without_downgrading() {
        let routes = remote();
        for path in [
            "//elsewhere/api/inbox/threads",
            "/api/inbox/m/not-a-machine/messages",
            "/api/inbox/m/ibx_00000000000000000000000002/../settings",
            "/api/inbox/threads/x%2fsettings",
        ] {
            assert!(routes.resolve(path).is_err(), "{path}");
        }
    }
}
