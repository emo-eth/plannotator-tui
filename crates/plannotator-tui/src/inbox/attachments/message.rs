use std::fmt::Write as _;

use anyhow::{Result, ensure};
use plannotator_tui_schema::{DocumentSource, Provenance};
use serde_json::{Value, json};

use super::{AnnotationRecord, SavedAnnotation, annotation, id, ui};
use crate::app::App;
use crate::delivery::Caller;
use crate::gate::Gate;
use crate::inbox::client::Client;

impl Client {
    pub(crate) fn save_message_annotation(
        &self,
        message_id: &str,
        annotation: &Value,
    ) -> Result<AnnotationRecord> {
        let path = format!("/api/inbox/messages/{}/annotations", id(message_id)?);
        let saved: SavedAnnotation = self.post_json(
            &path,
            &json!({
                "serverSession":self.server_session(),"annotation":annotation,
            }),
        )?;
        ensure!(
            saved.annotation.message_id == message_id && saved.annotation.message.is_some(),
            "Inbox returned an annotation on a different message"
        );
        Ok(saved.annotation)
    }
}

pub(crate) fn annotate_message(
    client: &Client,
    terminal: &mut ratatui::DefaultTerminal,
    thread_id: &str,
    message_id: &str,
    body: &str,
) -> Result<()> {
    let thread = client.thread(thread_id)?;
    let message = thread.thread.messages.iter().find(|message| message.id == message_id);
    ensure!(
        message.is_some_and(|message| message.author.kind == "agent" && message.body == body),
        "Only the current text of an agent's message can be annotated; refresh the thread"
    );
    let pending: Vec<_> = client
        .attachments(thread_id)?
        .annotations
        .into_iter()
        .filter(|record| record.message.is_some() && record.message_id == message_id && record.pending())
        .collect();
    let source =
        DocumentSource::new(body.to_owned(), format!("Message {message_id}"), true, Provenance::Stdin);
    let mut app =
        App::open(source, usize::from(terminal.size()?.width.saturating_sub(36).max(20)), Box::new(Caller))?;
    app.gate = Gate::Decision;
    app.set_status(
        "Send saves message comments; send the thread reply to deliver them to this message's author."
            .to_owned(),
    );
    app.replace_annotations(pending.iter().map(annotation::import).collect::<Result<_>>()?)?;
    let Some(notes) = ui::edit_app(terminal, app)? else { return Ok(()) };
    ui::save_with_retry(terminal, || {
        let wire: Vec<_> = notes
            .iter()
            .map(|note| {
                ensure!(
                    note.plannotator_tui.images.is_empty() && note.attachments.is_empty(),
                    "Message comments take text only; image attachments are not accepted by the Inbox"
                );
                annotation::to_wire(note, body)
            })
            .collect::<Result<_>>()?;
        for value in &wire {
            client.save_message_annotation(message_id, value)?;
        }
        for record in &pending {
            if !notes.iter().any(|note| note.id == record.id) {
                client.remove_attachment_annotation(&record.id)?;
            }
        }
        Ok(())
    })
}

pub(crate) fn message_feedback(records: &[AnnotationRecord]) -> String {
    let mut out = String::new();
    for record in records.iter().filter(|record| record.message.is_some() && record.pending()) {
        let quote = record.annotation.get("originalText").and_then(Value::as_str).unwrap_or("");
        let text = record.annotation.get("text").and_then(Value::as_str).unwrap_or("");
        if out.is_empty() {
            out.push_str("# Feedback on your messages\n");
        }
        let _ = write!(out, "\n## Feedback on your message ({})\n\n", record.message_id);
        if record.annotation.get("type").and_then(Value::as_str) == Some("DELETION") {
            let fence = "`".repeat(quote.split(|ch| ch != '`').map(str::len).max().unwrap_or(0).max(2) + 1);
            let _ = writeln!(out, "Remove this:\n{fence}\n{quote}\n{fence}");
        } else {
            let _ =
                writeln!(out, "Feedback on: \"{}\"", quote.split_whitespace().collect::<Vec<_>>().join(" "));
        }
        if !text.is_empty() {
            let _ = writeln!(out, "> {}", text.replace('\n', "\n> "));
        }
    }
    out.trim_end().to_owned()
}
