use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use super::super::client::{Client, HttpError};

pub(super) const MESSAGE_MAX: usize = 4_000;
pub(super) const CONTACT_MAX: usize = 200;
pub(super) const CATEGORIES: &[&str] = &["None", "Bug", "Idea", "Other"];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Category {
    #[default]
    None,
    Bug,
    Idea,
    Other,
}

impl Category {
    pub(super) fn from_index(index: usize) -> Self {
        match index {
            1 => Self::Bug,
            2 => Self::Idea,
            3 => Self::Other,
            _ => Self::None,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::None => "None (optional)",
            Self::Bug => "Bug",
            Self::Idea => "Idea",
            Self::Other => "Other",
        }
    }

    fn wire(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Bug => Some("bug"),
            Self::Idea => Some("idea"),
            Self::Other => Some("other"),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Draft {
    pub(super) message: String,
    pub(super) category: Category,
    pub(super) contact: String,
}

impl Draft {
    pub(super) fn body(&self) -> Result<Value> {
        let message = self.message.trim();
        let contact = self.contact.trim();
        ensure!(!message.is_empty(), "Write your feedback first.");
        ensure!(message.chars().count() <= MESSAGE_MAX, "Keep it under 4,000 characters.");
        ensure!(contact.chars().count() <= CONTACT_MAX, "Keep the contact under 200 characters.");
        Ok(json!({ "message": message, "category": self.category.wire(),
            "contact": if contact.is_empty() { None } else { Some(contact) } }))
    }
}

pub(super) fn submit(client: &Client, draft: &Draft) -> Result<()> {
    let mut body = draft.body()?;
    body.as_object_mut()
        .context("feedback body is not an object")?
        .insert("serverSession".to_owned(), Value::String(client.server_session().to_owned()));
    let response: Value = client.post_json("/api/inbox/feedback", &body)?;
    ensure!(
        response.get("ok").and_then(Value::as_bool) == Some(true),
        "Submission was not confirmed by the Inbox. It may have been delivered; check before sending again."
    );
    Ok(())
}

pub(super) fn failure(error: &anyhow::Error) -> String {
    if let Some(http) = error.downcast_ref::<HttpError>() {
        let message = if http.status == 404 {
            "This Inbox does not expose product feedback; update Plannotator or copy the agent prompt."
        } else {
            &http.message
        };
        format!("{message}\nYour draft is kept. Nothing will retry automatically.")
    } else {
        format!(
            "Submission was not confirmed: {error:#}\nYour draft is kept. It may have been delivered; check before sending again. Nothing will retry automatically."
        )
    }
}

pub(super) fn disclosure(settings: Option<&Value>) -> String {
    let version = settings.and_then(|s| s.get("version")).and_then(Value::as_str);
    let version =
        version.map_or_else(|| "this Inbox's Plannotator version".to_owned(), |v| format!("Plannotator {v}"));
    format!(
        "Send to the Plannotator maintainers: your message, optional category/contact, {version}, this computer's OS name and your agents' names. Nothing from your threads or files. Anonymous unless you add contact information. Typing and related-help matching stay here; only confirming Send submits feedback."
    )
}
