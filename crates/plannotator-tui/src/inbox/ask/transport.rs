//! Ask's owner-window transport. Reading and posting a turn alone does not start
//! an answer: the returned provider must create an AI session and stream a query.

use std::io::{BufRead, BufReader};
use std::time::Duration;

use anyhow::{Context as _, Result};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use ureq::{Agent, AgentBuilder, Response};
use url::Url;

use super::model::{Chats, Model, Posted, Question};
use crate::inbox::client::{Client, HttpError};

#[derive(Debug, Clone)]
pub(super) struct Wire {
    base: Url,
    origin: String,
    pub server_session: String,
    agent: Agent,
    stream_agent: Agent,
}

impl Wire {
    pub(super) fn new(client: &Client) -> Result<Self> {
        let base = Url::parse(client.url()).context("parsing Ask server URL")?;
        let origin = base.origin().ascii_serialization();
        Ok(Self {
            base,
            origin,
            server_session: client.server_session().to_owned(),
            agent: AgentBuilder::new()
                .timeout_connect(Duration::from_secs(3))
                .timeout_read(Duration::from_secs(30))
                .build(),
            // A busy host may wait arbitrarily long without sending a frame.
            // Stop is a separate request, so the UI remains responsive.
            stream_agent: AgentBuilder::new().timeout_connect(Duration::from_secs(3)).build(),
        })
    }

    fn request(&self, method: &str, path: &str, stream: bool) -> Result<ureq::Request> {
        let agent = if stream { &self.stream_agent } else { &self.agent };
        Ok(agent
            .request(method, self.base.join(path)?.as_str())
            .set("Origin", &self.origin)
            .set("Sec-Fetch-Site", "same-origin")
            .set("Accept", if stream { "text/event-stream" } else { "application/json" }))
    }

    pub(super) fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        response(self.request("GET", path, false)?.call())?.into_json().context("decoding Ask response")
    }

    pub(super) fn post<T: DeserializeOwned>(&self, path: &str, mut body: Value) -> Result<T> {
        body.as_object_mut()
            .context("Ask request body must be an object")?
            .insert("serverSession".to_owned(), Value::String(self.server_session.clone()));
        response(self.request("POST", path, false)?.send_json(body))?
            .into_json()
            .context("decoding Ask response")
    }

    pub(super) fn chats(&self) -> Result<Chats> {
        self.get("/api/inbox/ask")
    }

    pub(super) fn read(&self, thread: &str) -> Result<Model> {
        let model: Model = self.get(&thread_path(thread, "")?)?;
        anyhow::ensure!(
            model.server_session == self.server_session,
            "Inbox restarted; reopen Ask before sending. Your draft is retained."
        );
        if let Some(conversation) = &model.conversation {
            anyhow::ensure!(
                conversation.thread_id == thread,
                "Ask returned a conversation for another thread"
            );
        }
        Ok(model)
    }

    pub(super) fn send(&self, thread: &str, question: &Question) -> Result<Posted> {
        question.validate()?;
        let posted: Posted = self.post(&thread_path(thread, "")?, serde_json::to_value(question)?)?;
        anyhow::ensure!(
            posted.conversation.thread_id == thread && posted.turn.id == question.turn_id,
            "Ask returned another turn; the send is unconfirmed and retained"
        );
        Ok(posted)
    }

    pub(super) fn session(&self, provider: &str) -> Result<String> {
        let value: Value = self.post("/api/ai/session", json!({"providerId":provider,"context":{"mode":"annotate","annotate":{"filePath":"","content":""}}}))?;
        value
            .get("sessionId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .context("AI session response omitted sessionId")
    }

    pub(super) fn query(
        &self,
        session: &str,
        question: &Question,
        policy: Option<&str>,
        emit: impl FnMut(Value) -> Result<()>,
    ) -> Result<()> {
        let mut body =
            json!({"serverSession":self.server_session,"sessionId":session,"prompt":question.prompt()});
        if let Some(policy) = policy {
            anyhow::ensure!(matches!(policy, "wait" | "interrupt"), "Unknown Ask busy policy");
            body.as_object_mut()
                .context("Ask query body must be an object")?
                .insert("busyPolicy".to_owned(), Value::String(policy.to_owned()));
        }
        let response = response(self.request("POST", "/api/ai/query", true)?.send_json(body))?;
        read_stream(BufReader::new(response.into_reader()), emit)
    }
}

pub(super) fn thread_path(thread: &str, suffix: &str) -> Result<String> {
    anyhow::ensure!(
        !thread.is_empty() && thread.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
        "Invalid Inbox thread id"
    );
    Ok(format!("/api/inbox/threads/{thread}/ask{suffix}"))
}

fn response(result: std::result::Result<Response, ureq::Error>) -> Result<Response> {
    match result {
        Ok(response) => Ok(response),
        Err(ureq::Error::Status(status, response)) => {
            let body: Value = response.into_json().unwrap_or(Value::Null);
            Err(HttpError {
                status,
                code: body.get("code").and_then(Value::as_str).map(str::to_owned),
                message: body
                    .get("error")
                    .or_else(|| body.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("Ask request failed")
                    .to_owned(),
                payload: body,
            }
            .into())
        }
        Err(error) => Err(error.into()),
    }
}

/// The AI endpoint emits one JSON event per SSE frame plus `[DONE]`. A truncated
/// stream is not success: keep the partial answer and offer an explicit retry.
fn read_stream(mut reader: impl BufRead, mut emit: impl FnMut(Value) -> Result<()>) -> Result<()> {
    let mut line = String::new();
    let mut data = String::new();
    let mut complete = false;
    loop {
        line.clear();
        let eof = reader.read_line(&mut line).context("reading Ask answer stream")? == 0;
        if eof || line.trim_end_matches(['\r', '\n']).is_empty() {
            if !data.is_empty() {
                if data.trim() == "[DONE]" {
                    complete = true;
                } else {
                    let value: Value = serde_json::from_str(&data)
                        .context("decoding Ask stream event; partial answer retained")?;
                    if matches!(value.get("type").and_then(Value::as_str), Some("result" | "error")) {
                        complete = true;
                    }
                    emit(value)?;
                }
                data.clear();
            }
            if eof {
                break;
            }
        } else if let Some(value) = line.trim_end_matches(['\r', '\n']).strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(value.strip_prefix(' ').unwrap_or(value));
        }
    }
    anyhow::ensure!(complete, "Ask connection ended before the answer finished; partial answer retained");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_handles_crlf_comments_and_done() -> Result<()> {
        let mut values = Vec::new();
        read_stream(&b": heartbeat\r\n\r\ndata: {\"type\":\"text_delta\",\"delta\":\"hi\"}\r\n\r\ndata: [DONE]\r\n\r\n"[..], |v| { values.push(v); Ok(()) })?;
        assert_eq!(values.len(), 1);
        assert_eq!(values.first().and_then(|v| v.get("delta")).and_then(Value::as_str), Some("hi"));
        Ok(())
    }

    #[test]
    fn truncated_stream_keeps_delivered_text() {
        let mut count = 0;
        let result = read_stream(&b"data: {\"type\":\"text_delta\",\"delta\":\"partial\"}\n\n"[..], |_| {
            count += 1;
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(count, 1);
    }

    #[test]
    fn rejects_path_escape() {
        assert!(thread_path("../other", "").is_err());
    }
}
