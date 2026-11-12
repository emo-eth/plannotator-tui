use super::model::segment;
use super::{Mode, ReviewPane, SendKind, feedback};
use crate::inbox::{client::Client, send::new_key};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

impl ReviewPane {
    pub(super) fn send_options(&self) -> Vec<SendKind> {
        if !self.verdict {
            return vec![SendKind::Feedback];
        }
        if self.records.is_empty() {
            vec![SendKind::Approve, SendKind::ApprovalNote, SendKind::Changes, SendKind::Feedback]
        } else {
            vec![
                SendKind::Changes,
                SendKind::ApproveComments,
                SendKind::ApprovalNote,
                SendKind::DiscardApprove,
                SendKind::Feedback,
            ]
        }
    }

    pub(super) fn begin_send(&mut self, client: &Client) -> Result<()> {
        if self.pending.is_some() {
            return self.retry_send(client);
        }
        self.writable()?;
        self.refresh_comments(client)?;
        if let Some(option) = self.send_options().first() {
            self.mode = Mode::Send(*option);
        }
        Ok(())
    }

    pub(super) fn send(&mut self, client: &Client, kind: SendKind) -> Result<()> {
        self.writable()?;
        if kind == SendKind::DiscardApprove {
            // Explicitly selected destructive approval; confirmed by Enter in the decision screen.
            // Remove only comments shown in that screen, never comments another window adds later.
            while let Some(record) = self.records.first() {
                let id = record.id.clone();
                let _: Value = Self::post(
                    client,
                    &format!("/api/inbox/annotations/{}/remove", segment(&id)),
                    json!({}),
                )?;
                self.records.retain(|record| record.id != id);
            }
        }
        let include = matches!(kind, SendKind::Changes | SendKind::ApproveComments | SendKind::Feedback);
        let records: Vec<_> = self.records.iter().filter(|_| include).collect();
        let mut words = if kind == SendKind::Feedback {
            String::new()
        } else {
            feedback::verdict_words(kind != SendKind::Changes, &self.message.guide.title, records.len())
        };
        if !self.note.trim().is_empty() {
            if !words.is_empty() {
                words.push_str("\n\n");
            }
            words.push_str(self.note.trim());
        }
        ensure!(
            !words.is_empty() || !records.is_empty(),
            "Add a note with n or save a comment before sending feedback"
        );
        let mut body = json!({"words":words, "questions":[], "idempotency_key":new_key()});
        let fields = body.as_object_mut().context("Review reply must be an object")?;
        if kind != SendKind::Feedback {
            fields.insert(
                "review_verdict".into(),
                json!(if kind == SendKind::Changes { "changes_requested" } else { "approved" }),
            );
        }
        if !records.is_empty() {
            fields.insert(
                "feedback".into(),
                Value::String(feedback::export(&self.message.guide.title, &records)),
            );
            fields.insert(
                "annotation_ids".into(),
                json!(records.iter().map(|record| &record.id).collect::<Vec<_>>()),
            );
        }
        self.pending = Some(body);
        self.mode = Mode::Browse;
        self.retry_send(client)
    }

    pub(super) fn retry_send(&mut self, client: &Client) -> Result<()> {
        let Some(body) = &self.pending else { return Ok(()) };
        let response: Value = Self::post(
            client,
            &format!("/api/inbox/messages/{}/reply", segment(&self.message.id)),
            body.clone(),
        )?;
        ensure!(
            response.get("held").and_then(Value::as_bool) != Some(true),
            "Reply is held by the Inbox outbox, not confirmed; s retries the same request"
        );
        if let Some(ids) = body.get("annotation_ids").and_then(Value::as_array) {
            self.records.retain(|record| !ids.iter().any(|id| id.as_str() == Some(record.id.as_str())));
        }
        self.pending = None;
        self.note.clear();
        self.mode = Mode::Browse;
        self.status = "Review reply sent to the agent".into();
        Ok(())
    }
}

impl SendKind {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Approve => "Approve",
            Self::Changes => "Request changes / send comments",
            Self::ApproveComments => "Approve with comments (non-blocking)",
            Self::ApprovalNote => "Approve with note; keep comments unsent",
            Self::DiscardApprove => "Approve and discard all listed comments",
            Self::Feedback => "Send feedback to the agent (no verdict)",
        }
    }
}
