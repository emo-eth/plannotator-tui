use std::fmt::Write as _;

use anyhow::{Context, Result, ensure};
use plannotator_tui_schema::{Anchor, Annotation, DocumentSource, Kind, NoteExtras, Provenance, State};
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::{AnnotationRecord, AttachmentKind, AttachmentView};
use crate::app::App;
use crate::delivery::Caller;
use crate::gate::Gate;

impl AttachmentView {
    pub(super) fn annotator_source(&self) -> String {
        if matches!(self.attachment.kind, AttachmentKind::Markdown | AttachmentKind::Text) {
            self.text.clone()
        } else {
            crate::terminal::document(&self.text)
        }
    }

    pub(crate) fn open_app(&self, width: usize) -> Result<App> {
        ensure!(
            self.attachment.kind.is_text(),
            "{} is not text; use its external viewer",
            self.attachment.name
        );
        let version = if self.version == "current" { "current file" } else { "sent copy" };
        let source = DocumentSource::new(
            self.annotator_source(),
            format!("{} · {version}", self.attachment.name),
            true,
            Provenance::Stdin,
        );
        let mut app = App::open(source, width, Box::new(Caller))?;
        app.gate = Gate::Decision;
        app.set_status(
            "Send saves these notes to the Inbox; send the thread reply to deliver them.".to_owned(),
        );
        Ok(app)
    }
}

pub(super) fn to_wire(note: &Annotation, source: &str) -> Result<Value> {
    if let Some(original) = note.other.get("inbox_annotation") {
        let mut value = original.clone();
        let object = value.as_object_mut().context("Stored Inbox annotation is not an object")?;
        object.insert("id".to_owned(), json!(note.id));
        object.insert("text".to_owned(), json!(note.body));
        return Ok(value);
    }
    let quote = note.anchor.rendered();
    ensure!(!quote.is_empty(), "Annotation {} has no text anchor", note.id);
    let created =
        OffsetDateTime::parse(&note.created_at, &Rfc3339).context("reading annotation creation time")?;
    let created_ms = i64::try_from(created.unix_timestamp_nanos() / 1_000_000)
        .context("annotation creation time out of range")?;
    let kind = note.anchor.kind();
    let text = if kind == Kind::LooksGood {
        if note.body.trim().is_empty() {
            "Looks good.".to_owned()
        } else {
            format!("Looks good. {}", note.body)
        }
    } else {
        note.body.clone()
    };
    let lines = note
        .anchor
        .plannotator_tui
        .as_ref()
        .and_then(|extra| extra.source.as_ref())
        .map(|range| -> Result<(usize, usize)> {
            ensure!(
                note.anchor
                    .plannotator_tui
                    .as_ref()
                    .is_some_and(|extra| source.get(range.start..range.end) == Some(extra.quote.as_str())),
                "Annotation was made on different document bytes"
            );
            ensure!(
                range.start <= range.end && source.get(range.start..range.end).is_some(),
                "Annotation has an invalid source range"
            );
            Ok(crate::export::line_span(source, &(range.start..range.end)))
        })
        .transpose()?;
    let mut wire = json!({
        "id": note.id,
        "blockId": "external",
        "startOffset": 0,
        "endOffset": 0,
        "type": if kind == Kind::Delete { "DELETION" } else { "COMMENT" },
        "text": text,
        "originalText": quote,
        "createdA": created_ms,
        "source": "plannotator-tui",
        "plannotator_tui": {"anchor": note.anchor, "lines": lines},
    });
    if let Some(author) = note.author_name.as_ref().or(note.author.as_ref()) {
        wire.as_object_mut()
            .context("Inbox annotation is not an object")?
            .insert("author".to_owned(), json!(author));
    }
    Ok(wire)
}

pub(super) fn import(record: &AnnotationRecord) -> Result<Annotation> {
    let wire = &record.annotation;
    let quote = wire.get("originalText").and_then(Value::as_str).unwrap_or("");
    let mut anchor: Anchor = wire
        .pointer("/plannotator_tui/anchor")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .context("reading stored terminal annotation anchor")?
        .unwrap_or_else(|| Anchor {
            original_text: quote.to_owned(),
            quote: quote.to_owned(),
            ..Anchor::default()
        });
    if wire.get("type").and_then(Value::as_str) == Some("DELETION") {
        anchor.plannotator_tui.get_or_insert_default().kind = Kind::Delete;
    }
    let mut other = std::collections::BTreeMap::new();
    other.insert("inbox_annotation".to_owned(), wire.clone());
    Ok(Annotation {
        id: record.id.clone(),
        document_id: String::new(),
        anchor,
        body: wire.get("text").and_then(Value::as_str).unwrap_or("").to_owned(),
        author: wire.get("author").and_then(Value::as_str).map(str::to_owned),
        author_name: None,
        state: State::Open,
        attachments: Vec::new(),
        plannotator_tui: NoteExtras::default(),
        created_at: record.created_at.clone(),
        updated_at: record.updated_at.clone(),
        replies: Vec::new(),
        other,
    })
}

pub(super) fn code_target(wire: &mut Value, note: &Annotation, displayed: &str, text: &str) -> Result<()> {
    if note.other.contains_key("inbox_annotation") {
        return Ok(());
    }
    let range = note
        .anchor
        .plannotator_tui
        .as_ref()
        .and_then(|extra| extra.source.as_ref())
        .context("Code comment has no source range")?;
    let (start, end) = crate::export::line_span(displayed, &(range.start..range.end));
    ensure!(start > 1 && end > 1, "Select code text rather than the display fence");
    let start = start - 1;
    let end = end - 1;
    ensure!(end <= text.lines().count(), "Select source lines rather than the display fence");
    let code = text.lines().skip(start - 1).take(end - start + 1).collect::<Vec<_>>().join("\n");
    let object = wire.as_object_mut().context("Inbox annotation is not an object")?;
    object.insert("blockId".to_owned(), json!(format!("code-L{start}-{end}")));
    object.insert("inboxCode".to_owned(), json!({"lineStart":start,"lineEnd":end,"code":code}));
    object
        .get_mut("plannotator_tui")
        .and_then(Value::as_object_mut)
        .context("Inbox annotation has no terminal anchor metadata")?
        .insert("lines".to_owned(), json!([start, end]));
    Ok(())
}

pub(crate) fn feedback(records: &[AnnotationRecord]) -> String {
    let records: Vec<_> = records.iter().filter(|r| r.is_file() && r.pending()).collect();
    if records.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "# Feedback on the attached files\n\nThe following feedback is on the files you attached, one section per file and version.\n",
    );
    let mut groups: Vec<(&str, &str, Vec<&AnnotationRecord>)> = Vec::new();
    for record in records {
        if let Some((_, _, notes)) =
            groups.iter_mut().find(|(path, version, _)| *path == record.path && *version == record.version)
        {
            notes.push(record);
        } else {
            groups.push((&record.path, &record.version, vec![record]));
        }
    }
    for (path, version, notes) in groups {
        let suffix = if version == "current" { "" } else { " (the version you sent)" };
        let _ = writeln!(out, "\n### {}{suffix}\n", one_line(path));
        for (index, record) in notes.iter().enumerate() {
            let note = &record.annotation;
            let quote = note.get("originalText").and_then(Value::as_str).unwrap_or("");
            let text = note.get("text").and_then(Value::as_str).unwrap_or("");
            let location = note
                .pointer("/plannotator_tui/lines")
                .and_then(Value::as_array)
                .and_then(|lines| Some((lines.first()?.as_u64()?, lines.get(1)?.as_u64()?)))
                .map(|(start, end)| {
                    if start == end {
                        format!(" (line {start})")
                    } else {
                        format!(" (lines {start}–{end})")
                    }
                })
                .unwrap_or_default();
            let _ = writeln!(out, "#### Annotation {}{location}\n", index + 1);
            if note.get("type").and_then(Value::as_str) == Some("DELETION") {
                let longest = quote.split(|ch| ch != '`').map(str::len).max().unwrap_or(0);
                let fence = "`".repeat(longest.max(2) + 1);
                let _ = writeln!(out, "Remove this:\n{fence}\n{quote}\n{fence}");
                if text.trim().is_empty() {
                    out.push_str("> I don't want this.\n");
                }
            } else {
                let _ = writeln!(out, "Feedback on: \"{}\"", one_line(quote));
            }
            if !text.trim().is_empty() {
                let _ = writeln!(out, "> {}", text.replace('\n', "\n> "));
            }
            if let Some(images) = note.get("images").and_then(Value::as_array) {
                for image in images {
                    if let Some(path) = image.get("path").and_then(Value::as_str) {
                        let _ = writeln!(out, "Image: {}", one_line(path));
                    }
                }
            }
            if let Some(seconds) = note.pointer("/inboxVideo/time").and_then(Value::as_f64) {
                let _ = writeln!(out, "At {seconds} seconds");
            }
            out.push('\n');
        }
    }
    out.trim_end().to_owned()
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
