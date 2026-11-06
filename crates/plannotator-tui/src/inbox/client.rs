#![allow(clippy::all, clippy::pedantic)]

//! HTTP client for Plannotator Inbox API.
//!
//! Provides sync HTTP operations via `ureq` with automatic 409 conflict handling
//! (re-reads `inbox.json` for updated `serverSession` and retries once).

use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use plannotator_tui_schema::inbox::{
    InboxHealth, ListModel, PickItem, PicksRequest, PicksResponse, ReplyQuestionRef, ReplyRequest,
    ReplyResult, ResolveRequest, SeenRequest, ThreadModel,
};
use serde::{Deserialize, Serialize};

use super::discovery::{InboxConnectionInfo, reload_server_session};

#[derive(Debug, Clone)]
pub(crate) struct InboxClient {
    pub(crate) base_url: String,
    pub(crate) server_session: Arc<RwLock<String>>,
    pub(crate) config_path: PathBuf,
    agent: ureq::Agent,
}

impl InboxClient {
    pub(crate) fn new(info: InboxConnectionInfo) -> Self {
        let agent = ureq::builder()
            .timeout_connect(Duration::from_secs(5))
            .timeout_read(Duration::from_secs(15))
            .timeout_write(Duration::from_secs(10))
            .build();

        Self {
            base_url: info.url,
            server_session: Arc::new(RwLock::new(info.server_session)),
            config_path: info.config_path,
            agent,
        }
    }

    pub(crate) fn current_session(&self) -> String {
        self.server_session.read().map(|s| s.clone()).unwrap_or_default()
    }

    pub(crate) fn update_session(&self, new_session: String) {
        if let Ok(mut lock) = self.server_session.write() {
            *lock = new_session;
        }
    }

    fn endpoint_url(&self, path: &str) -> String {
        let trimmed_base = self.base_url.trim_end_matches('/');
        let trimmed_path = path.trim_start_matches('/');
        format!("{trimmed_base}/{trimmed_path}")
    }

    /// Perform a GET request and deserialize JSON response.
    fn get<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T> {
        let url = self.endpoint_url(path);
        let resp = self
            .agent
            .get(&url)
            .set("Accept", "application/json")
            .call()
            .with_context(|| format!("GET {url}"))?;

        let val = resp.into_json::<T>().with_context(|| format!("parsing JSON from {url}"))?;
        Ok(val)
    }

    /// Perform a POST request with automatic 409 retry on server session mismatch.
    fn post<B: Serialize + Clone, T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        build_body: impl Fn(&str) -> B,
    ) -> Result<T> {
        let url = self.endpoint_url(path);
        let session = self.current_session();
        let body = build_body(&session);

        let res = self
            .agent
            .post(&url)
            .set("Accept", "application/json")
            .set("Content-Type", "application/json")
            .send_json(body.clone());

        match res {
            Ok(resp) => {
                let parsed = resp.into_json::<T>().with_context(|| format!("parsing JSON from {url}"))?;
                Ok(parsed)
            }
            Err(ureq::Error::Status(409, resp)) => {
                // Server session mismatch: re-read inbox.json and retry once
                drop(resp);
                if let Ok(fresh_session) = reload_server_session(&self.config_path) {
                    self.update_session(fresh_session.clone());
                    let retry_body = build_body(&fresh_session);
                    let retry_res = self
                        .agent
                        .post(&url)
                        .set("Accept", "application/json")
                        .set("Content-Type", "application/json")
                        .send_json(retry_body)
                        .with_context(|| format!("retry POST {url} after 409"))?;
                    let parsed = retry_res.into_json::<T>()?;
                    return Ok(parsed);
                }
                bail!("409 Conflict from {url} and could not reload serverSession");
            }
            Err(ureq::Error::Status(code, resp)) => {
                let body = resp.into_string().unwrap_or_default();
                bail!("POST {url} returned HTTP {code}: {body}")
            }
            Err(err) => Err(anyhow::anyhow!("POST {url} failed: {err}")),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn health(&self) -> Result<InboxHealth> {
        self.get("api/inbox/health")
    }

    /// GET `/api/inbox/threads[?project=]`
    pub(crate) fn list_threads(&self, project: Option<&str>) -> Result<ListModel> {
        let path = if let Some(p) = project {
            format!("api/inbox/threads?project={}", urlencoding(p))
        } else {
            "api/inbox/threads".to_owned()
        };
        self.get(&path)
    }

    /// GET `/api/inbox/threads/:id`
    pub(crate) fn get_thread(&self, thread_id: &str) -> Result<ThreadModel> {
        self.get(&format!("api/inbox/threads/{}", urlencoding(thread_id)))
    }

    /// POST `/api/inbox/threads/:id/seen`
    pub(crate) fn mark_seen(&self, thread_id: &str) -> Result<()> {
        let path = format!("api/inbox/threads/{}/seen", urlencoding(thread_id));
        let _: serde_json::Value =
            self.post(&path, |sess| SeenRequest { server_session: sess.to_owned() })?;
        Ok(())
    }

    /// POST `/api/inbox/messages/:id/picks`
    pub(crate) fn save_picks(&self, message_id: &str, questions: Vec<PickItem>) -> Result<PicksResponse> {
        let path = format!("api/inbox/messages/{}/picks", urlencoding(message_id));
        self.post(&path, |sess| PicksRequest {
            server_session: sess.to_owned(),
            questions: questions.clone(),
        })
    }

    /// POST `/api/inbox/messages/:id/reply`
    pub(crate) fn send_reply(
        &self,
        message_id: &str,
        words: &str,
        questions: Vec<ReplyQuestionRef>,
        feedback: Option<String>,
        annotation_ids: Option<Vec<String>>,
    ) -> Result<ReplyResult> {
        let path = format!("api/inbox/messages/{}/reply", urlencoding(message_id));
        let idempotency_key = uuid_v4_simple();
        self.post(&path, |sess| ReplyRequest {
            server_session: sess.to_owned(),
            idempotency_key: idempotency_key.clone(),
            words: words.to_owned(),
            questions: questions.clone(),
            feedback: feedback.clone(),
            annotation_ids: annotation_ids.clone(),
        })
    }

    /// POST `/api/inbox/messages/:id/resolve`
    pub(crate) fn resolve_thread(&self, message_id: &str, resolved: bool) -> Result<()> {
        let path = format!("api/inbox/messages/{}/resolve", urlencoding(message_id));
        let _: serde_json::Value =
            self.post(&path, |sess| ResolveRequest { server_session: sess.to_owned(), resolved })?;
        Ok(())
    }
}

fn urlencoding(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn uuid_v4_simple() -> String {
    use std::time::SystemTime;
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("tui-reply-{:x}", now)
}
