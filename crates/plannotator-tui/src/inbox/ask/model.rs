//! Wire types for `packages/server/inbox-ask.ts` and the shared AI endpoints.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(super) const TEXT_MAX: usize = 1_000_000;

#[derive(Debug, Clone, Deserialize)]
pub(super) struct Model {
    #[serde(rename = "serverSession")]
    pub server_session: String,
    pub subject: Option<String>,
    pub project: String,
    pub conversation: Option<Conversation>,
    #[serde(default)]
    pub damaged: bool,
    pub running: Option<String>,
    pub can_ask: bool,
    pub agent: Agent,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct Agent {
    pub host: Option<String>,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct Conversation {
    pub thread_id: String,
    pub seen_at: Option<String>,
    pub turns: Vec<Turn>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct Turn {
    pub id: String,
    pub at: String,
    pub session: String,
    pub host: Option<String>,
    pub question: String,
    pub passage: Option<String>,
    pub source: Option<String>,
    pub answer: Option<String>,
    pub answered_at: Option<String>,
    #[serde(default)]
    pub stopped: bool,
    pub failure: Option<String>,
    pub kind: Option<String>,
    pub show_me: Option<Value>,
    pub again_shape: Option<String>,
    pub shape: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct Chat {
    pub thread_id: String,
    pub subject: Option<String>,
    pub project: String,
    pub host: Option<String>,
    pub at: String,
    pub unread: bool,
    pub answering: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct Chats {
    pub chats: Vec<Chat>,
}

#[derive(Debug, Deserialize)]
pub(super) struct Posted {
    pub conversation: Conversation,
    pub turn: Turn,
    pub provider_id: Option<String>,
    pub reason: Option<String>,
    #[serde(default)]
    pub running: bool,
}

/// Frozen until the POST's outcome is known. A retry reuses every original field.
#[derive(Debug, Clone, Serialize)]
pub(super) struct Question {
    pub turn_id: String,
    #[serde(rename = "question")]
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub passage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_me: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub again_shape: Option<String>,
}

impl Question {
    pub(super) fn plain(turn_id: String, question: String, source: Option<String>) -> Self {
        Self { turn_id, text: question, source, passage: None, kind: None, show_me: None, again_shape: None }
    }

    pub(super) fn from_turn(turn: &Turn) -> Self {
        Self {
            turn_id: turn.id.clone(),
            text: turn.question.clone(),
            passage: turn.passage.clone(),
            source: turn.source.clone(),
            kind: turn.kind.clone(),
            show_me: turn.show_me.clone(),
            again_shape: turn.again_shape.clone(),
        }
    }

    pub(super) fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            (8..=80).contains(&self.turn_id.len())
                && self.turn_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
            "Ask turn id must be 8–80 letters, digits, underscores or dashes"
        );
        anyhow::ensure!(!self.text.trim().is_empty(), "Write a question first");
        for value in
            [Some(self.text.as_str()), self.passage.as_deref(), self.source.as_deref()].into_iter().flatten()
        {
            anyhow::ensure!(
                value.encode_utf16().count() <= TEXT_MAX,
                "Ask text exceeds {TEXT_MAX} characters"
            );
        }
        Ok(())
    }

    pub(super) fn prompt(&self) -> String {
        match self.source.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(source) => format!("{source}\n\n{}", self.text),
            None => self.text.clone(),
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct LiveAnswer {
    pub turn_id: String,
    pub owner_session: String,
    pub ai_session: Option<String>,
    pub text: String,
    pub status: Option<String>,
    pub error_code: Option<String>,
    pub error: Option<String>,
    pub active: bool,
    pub permissions: Vec<Permission>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Permission {
    pub request_id: String,
    pub tool_name: String,
    #[serde(default)]
    pub tool_input: Value,
    pub title: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub decided: Option<bool>,
    #[serde(default)]
    pub pending: bool,
}

impl LiveAnswer {
    pub(super) fn apply(&mut self, event: &Value) {
        match event.get("type").and_then(Value::as_str) {
            Some("text_delta") => {
                if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                    self.text.push_str(delta);
                    self.status = None;
                }
            }
            Some("text") if self.text.is_empty() => {
                if let Some(text) = event.get("text").and_then(Value::as_str) {
                    self.text.push_str(text);
                }
            }
            Some("result") => {
                if self.text.is_empty()
                    && let Some(text) = event.get("result").and_then(Value::as_str)
                {
                    self.text.push_str(text);
                }
                self.active = false;
                self.status = None;
            }
            Some("status") => self.status = event.get("status").and_then(Value::as_str).map(str::to_owned),
            Some("error") => {
                self.error =
                    Some(event.get("error").and_then(Value::as_str).unwrap_or("Ask failed").to_owned());
                self.error_code = event.get("code").and_then(Value::as_str).map(str::to_owned);
                self.active = false;
                self.status = None;
            }
            Some("permission_request") => {
                if let Ok(permission) = serde_json::from_value::<Permission>(event.clone())
                    && !self.permissions.iter().any(|p| p.request_id == permission.request_id)
                {
                    self.permissions.push(permission);
                }
            }
            _ => {}
        }
    }
}

pub(super) fn shown_question(turn: &Turn) -> String {
    if turn.kind.as_deref() != Some("show_me") {
        return turn.question.clone();
    }
    let Some(target) = turn.show_me.as_ref() else { return turn.question.clone() };
    let file = target.get("file").and_then(Value::as_str).unwrap_or("this review");
    let mut shown = format!("Show me on {file}");
    if let Some(lines) = target.get("lines").filter(|v| !v.is_null()) {
        let _ = write!(shown, " · {lines}");
    }
    if let Some(words) = target.get("words").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        shown.push('\n');
        shown.push_str(words);
    }
    shown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_text_survives_stream_failure_and_final_result() {
        let mut live = LiveAnswer { active: true, ..LiveAnswer::default() };
        live.apply(&serde_json::json!({"type":"text_delta","delta":"partial"}));
        live.apply(&serde_json::json!({"type":"error","code":"session_taken_over","error":"Taken over"}));
        live.apply(&serde_json::json!({"type":"result","result":"duplicate"}));
        assert_eq!(live.text, "partial");
        assert_eq!(live.error.as_deref(), Some("Taken over"));
        assert!(!live.active);
    }

    #[test]
    fn retry_keeps_show_me_and_context() -> anyhow::Result<()> {
        let turn: Turn = serde_json::from_value(
            serde_json::json!({"id":"ask_turn1","at":"now","session":"s","host":null,"question":"draw it","source":"[file]","kind":"show_me","show_me":{"file":"a.rs"},"again_shape":"sh1"}),
        )?;
        let retry = Question::from_turn(&turn);
        retry.validate()?;
        let wire = serde_json::to_value(&retry)?;
        assert_eq!(wire.get("turn_id"), Some(&serde_json::json!("ask_turn1")));
        assert_eq!(wire.get("again_shape"), Some(&serde_json::json!("sh1")));
        assert_eq!(wire.pointer("/show_me/file"), Some(&serde_json::json!("a.rs")));
        assert_eq!(retry.prompt(), "[file]\n\ndraw it");
        Ok(())
    }
}
