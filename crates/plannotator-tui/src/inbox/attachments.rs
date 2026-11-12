mod annotation;
mod message;
mod open;
mod ui;
mod upload;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::client::Client;

pub(crate) use annotation::feedback;
pub(crate) use message::{annotate_message, message_feedback};
pub(crate) use open::{open_browser, prototype_url};
pub(crate) use ui::{open_attachment, run};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum AttachmentKind {
    Markdown,
    Text,
    Html,
    Mermaid,
    Graphviz,
    Code,
    Image,
    Video,
    Audio,
    Pdf,
    #[serde(other)]
    File,
}

impl AttachmentKind {
    pub(crate) fn is_text(self) -> bool {
        matches!(self, Self::Markdown | Self::Text | Self::Html | Self::Mermaid | Self::Graphviz | Self::Code)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct Attachment {
    pub(crate) id: String,
    pub(crate) path: String,
    pub(crate) named_path: Option<String>,
    pub(crate) name: String,
    pub(crate) kind: AttachmentKind,
    #[serde(skip)]
    pub(crate) thread_id: String,
    pub(crate) sent_sha256: String,
    pub(crate) size: u64,
    pub(crate) sent_at: String,
    pub(crate) sent_mtime: String,
    pub(crate) message_id: String,
    pub(crate) media_type: Option<String>,
    pub(crate) width: Option<u64>,
    pub(crate) height: Option<u64>,
    pub(crate) duration_ms: Option<f64>,
    #[serde(rename = "ref")]
    pub(crate) image_ref: Option<String>,
    pub(crate) captured: Option<String>,
    pub(crate) captured_at: Option<String>,
    pub(crate) current: Option<Current>,
    pub(crate) changed_since_sent: bool,
    pub(crate) unavailable: Option<Unavailable>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct Current {
    pub(crate) sha256: String,
    pub(crate) size: u64,
    pub(crate) mtime: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct Unavailable {
    pub(crate) code: String,
    pub(crate) message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Version {
    Current,
    Sent,
}

impl Version {
    pub(crate) fn query(self) -> &'static str {
        match self {
            Self::Current => "?version=current",
            Self::Sent => "?version=sent",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Current => "current file",
            Self::Sent => "sent copy",
        }
    }
}

impl Attachment {
    pub(crate) fn readable_version(&self) -> Version {
        if self.unavailable.is_some() { Version::Sent } else { Version::Current }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct AnnotationRecord {
    pub(crate) id: String,
    pub(crate) project_id: String,
    pub(crate) thread_id: String,
    pub(crate) message_id: String,
    pub(crate) attachment_id: String,
    pub(crate) path: String,
    pub(crate) version: String,
    pub(crate) annotation: Value,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    pub(crate) removed_at: Option<String>,
    pub(crate) sent_reply_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) guide: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) message: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) live: Option<Value>,
}

impl AnnotationRecord {
    pub(crate) fn is_file(&self) -> bool {
        self.guide.is_none() && self.message.is_none() && self.live.is_none()
    }

    pub(crate) fn pending(&self) -> bool {
        self.removed_at.is_none() && self.sent_reply_id.is_none()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct AttachmentsModel {
    #[serde(rename = "serverSession")]
    pub(crate) server_session: String,
    pub(crate) attachments: Vec<Attachment>,
    #[serde(default)]
    pub(crate) inline_images: Vec<Attachment>,
    pub(crate) annotations: Vec<AnnotationRecord>,
    #[serde(default)]
    pub(crate) earlier_annotations: Vec<AnnotationRecord>,
}

impl AttachmentsModel {
    pub(crate) fn files(&self) -> impl Iterator<Item = &Attachment> {
        self.attachments.iter().chain(&self.inline_images)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct AttachmentView {
    #[serde(rename = "serverSession")]
    pub(crate) server_session: String,
    pub(crate) attachment: Attachment,
    pub(crate) version: String,
    pub(crate) text: String,
}

#[derive(Deserialize)]
struct SavedAnnotation {
    annotation: AnnotationRecord,
}

impl Client {
    pub(crate) fn attachments(&self, thread_id: &str) -> Result<AttachmentsModel> {
        let path = format!("/api/inbox/threads/{}/attachments", id(thread_id)?);
        let mut model: AttachmentsModel = self.get_json("GET", &path)?;
        ensure!(
            model.server_session == self.server_session(),
            "Inbox serverSession changed; reopen the Inbox before continuing"
        );
        for attachment in model.attachments.iter_mut().chain(&mut model.inline_images) {
            thread_id.clone_into(&mut attachment.thread_id);
        }
        Ok(model)
    }

    pub(crate) fn attachment_view(&self, attachment_id: &str, version: Version) -> Result<AttachmentView> {
        let path = format!("/api/inbox/attachments/{}/view{}", id(attachment_id)?, version.query());
        let view: AttachmentView = self.get_json("GET", &path)?;
        ensure!(
            view.server_session == self.server_session(),
            "Inbox serverSession changed; reopen the Inbox before continuing"
        );
        ensure!(view.attachment.id == attachment_id, "Inbox returned a different attachment");
        let expected = match version {
            Version::Current => "current",
            Version::Sent => &view.attachment.sent_sha256,
        };
        ensure!(view.version == expected, "Inbox returned a different attachment version");
        Ok(view)
    }

    pub(crate) fn save_attachment_annotation(
        &self,
        view: &AttachmentView,
        annotation: &Value,
    ) -> Result<AnnotationRecord> {
        ensure!(
            view.server_session == self.server_session(),
            "Attachment belongs to an earlier Inbox server session; reopen it before saving"
        );
        ensure!(
            view.version == "current" || view.version == view.attachment.sent_sha256,
            "Annotation version does not belong to this attachment"
        );
        let saved: SavedAnnotation = self.post_json(
            "/api/inbox/annotations",
            &json!({
                "serverSession": self.server_session(),
                "attachment_id": view.attachment.id,
                "version": view.version,
                "annotation": annotation,
            }),
        )?;
        ensure!(
            saved.annotation.path == view.attachment.path && saved.annotation.version == view.version,
            "Inbox saved an annotation on a different file or version"
        );
        Ok(saved.annotation)
    }

    pub(crate) fn edit_annotation_text(
        &self,
        record: &AnnotationRecord,
        text: &str,
    ) -> Result<AnnotationRecord> {
        ensure!(record.pending(), "This annotation has already been sent or removed");
        let mut annotation = record.annotation.clone();
        annotation
            .as_object_mut()
            .context("Stored Inbox annotation is not an object")?
            .insert("text".to_owned(), json!(text));
        let saved: SavedAnnotation = self.post_json(
            "/api/inbox/annotations",
            &json!({
                "serverSession":self.server_session(),"attachment_id":record.attachment_id,
                "version":record.version,"annotation":annotation,
            }),
        )?;
        Ok(saved.annotation)
    }

    pub(crate) fn remove_attachment_annotation(&self, annotation_id: &str) -> Result<AnnotationRecord> {
        ensure!(
            !annotation_id.is_empty()
                && annotation_id.len() <= 200
                && annotation_id.bytes().all(|c| c.is_ascii_alphanumeric() || b"_.:-".contains(&c)),
            "Invalid Inbox annotation id"
        );
        let saved: SavedAnnotation = self.post_json(
            &format!("/api/inbox/annotations/{annotation_id}/remove"),
            &json!({"serverSession": self.server_session()}),
        )?;
        Ok(saved.annotation)
    }
}

pub(super) fn id(value: &str) -> Result<&str> {
    ensure!(
        !value.is_empty() && value.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_'),
        "Invalid Inbox id {value:?}"
    );
    Ok(value)
}

impl AttachmentView {
    pub(crate) fn save_annotations(
        &self,
        client: &Client,
        annotations: &[plannotator_tui_schema::Annotation],
    ) -> Result<Vec<AnnotationRecord>> {
        ensure!(self.attachment.kind.is_text(), "This attachment needs its external viewer");
        let source = self.annotator_source();
        let mut wire = Vec::with_capacity(annotations.len());
        for note in annotations {
            let mut value = annotation::to_wire(note, &source)?;
            if !matches!(self.attachment.kind, AttachmentKind::Markdown | AttachmentKind::Text) {
                annotation::code_target(&mut value, note, &source, &self.text)?;
            }
            upload::attach_images(client, &mut value, note)?;
            wire.push(value);
        }
        let mut saved = Vec::with_capacity(wire.len());
        for note in &wire {
            let record = client.save_attachment_annotation(self, note)
                .with_context(|| format!("saving attachment annotation ({} of {} saved; successful records remain pending in the Inbox)", saved.len(), wire.len()))?;
            saved.push(record);
        }
        Ok(saved)
    }
}

#[cfg(test)]
mod tests;
