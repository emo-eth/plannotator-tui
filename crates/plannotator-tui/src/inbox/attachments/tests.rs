#![allow(clippy::expect_used, clippy::indexing_slicing, reason = "tests assert by panicking")]

use std::collections::BTreeMap;

use plannotator_tui_schema::{Anchor, Annotation, Kind, NoteExtras, SourceRange, State};
use serde_json::json;

use super::open::attachment_for_link;
use super::*;

fn attachment() -> Attachment {
    serde_json::from_value(json!({
        "id":"att_01J00000000000000000000000", "path":"/project/plan.md", "named_path":null,
        "name":"plan.md", "kind":"markdown", "sent_sha256":"a".repeat(64), "size":7,
        "sent_at":"2026-11-10T00:00:00Z", "sent_mtime":"2026-11-10T00:00:00Z",
        "message_id":"msg_1", "current":{"sha256":"b".repeat(64),"size":8,"mtime":"2026-11-11T00:00:00Z"},
        "changed_since_sent":true,"unavailable":null
    }))
    .expect("attachment")
}

fn note(kind: Kind) -> Annotation {
    let source = "# Hello\n";
    Annotation {
        id: "local-1".to_owned(),
        document_id: String::new(),
        anchor: Anchor::new(
            "Hello",
            source,
            SourceRange { start: 2, end: 7, version: plannotator_tui_schema::blob_sha(source.as_bytes()) },
            kind,
            Some(0),
        ),
        body: "A note".to_owned(),
        author: None,
        author_name: None,
        state: State::Open,
        attachments: Vec::new(),
        plannotator_tui: NoteExtras::default(),
        created_at: "2026-11-11T00:00:00Z".to_owned(),
        updated_at: "2026-11-11T00:00:00Z".to_owned(),
        replies: Vec::new(),
        other: BTreeMap::new(),
    }
}

#[test]
fn changed_files_stay_current_until_user_selects_sent() {
    let mut file = attachment();
    assert_eq!(file.readable_version(), Version::Current);
    file.unavailable =
        Some(Unavailable { code: "attachment_missing".to_owned(), message: "gone".to_owned() });
    assert_eq!(file.readable_version(), Version::Sent);
    assert_eq!(Version::Sent.query(), "?version=sent");
}

#[test]
fn annotation_wire_is_viewer_shape_not_workspace_shape() {
    let wire = annotation::to_wire(&note(Kind::Comment), "# Hello\n").expect("wire");
    assert_eq!(wire["originalText"], "Hello");
    assert_eq!(wire["type"], "COMMENT");
    assert_eq!(wire["id"], "local-1");
    assert_eq!(wire["blockId"], "external");
    assert_eq!(wire["plannotator_tui"]["lines"], json!([1, 1]));
    assert!(wire.get("body").is_none());
    assert!(wire.get("anchor").is_none());
}

#[test]
fn deletion_and_positive_feedback_survive_conversion() {
    let deleted = annotation::to_wire(&note(Kind::Delete), "# Hello\n").expect("deletion");
    assert_eq!(deleted["type"], "DELETION");
    let positive = annotation::to_wire(&note(Kind::LooksGood), "# Hello\n").expect("positive");
    assert_eq!(positive["type"], "COMMENT");
    assert_eq!(positive["text"], "Looks good. A note");
}

#[test]
fn stale_source_is_refused_before_submission() {
    assert!(annotation::to_wire(&note(Kind::Comment), "# Other\n").is_err());
}

#[test]
fn unknown_attachment_kinds_are_downloads() {
    let mut value = serde_json::to_value(attachment()).expect("serialize");
    value["kind"] = json!("future-format");
    let file: Attachment = serde_json::from_value(value).expect("future kind");
    assert_eq!(file.kind, AttachmentKind::File);
    assert!(!file.kind.is_text());
}

#[test]
fn ambiguous_basenames_are_not_prototypes() {
    let mut first = attachment();
    first.name = "page.html".to_owned();
    first.path = "/project/a/page.html".to_owned();
    first.kind = AttachmentKind::Html;
    let mut second = first.clone();
    second.id = "att_other".to_owned();
    second.path = "/project/b/page.html".to_owned();
    let files = [first, second];
    assert!(attachment_for_link("page.html", &files, "/project", None).is_none());
    assert_eq!(
        attachment_for_link("a/page.html#title", &files, "/project", None).map(|a| a.id.as_str()),
        Some("att_01J00000000000000000000000")
    );
}

#[test]
fn feedback_excludes_closed_records_and_preserves_sent_version() {
    let record: AnnotationRecord = serde_json::from_value(json!({
        "id":"server-id","project_id":"prj_1","thread_id":"msg_1","message_id":"msg_1",
        "attachment_id":"att_1","path":"/plan.md","version":"a".repeat(64),
        "annotation":{"type":"COMMENT","originalText":"Hello","text":"Fix it"},
        "created_at":"2026-11-11T00:00:00Z","updated_at":"2026-11-11T00:00:00Z",
        "removed_at":null,"sent_reply_id":null
    }))
    .expect("record");
    let text = feedback(std::slice::from_ref(&record));
    assert!(text.contains("/plan.md (the version you sent)"));
    assert!(text.contains("Fix it"));
    let mut closed = record;
    closed.sent_reply_id = Some("msg_reply".to_owned());
    assert!(feedback(&[closed]).is_empty());
}

#[test]
fn importing_pending_comments_preserves_server_ids_and_foreign_fields() {
    let record: AnnotationRecord = serde_json::from_value(json!({
        "id":"server-id","project_id":"prj_1","thread_id":"msg_1","message_id":"msg_1",
        "attachment_id":"att_1","path":"/plan.md","version":"current",
        "annotation":{"id":"server-id","type":"COMMENT","originalText":"Hello","text":"Fix it","htmlAnchor":{"selector":"button"},"images":[{"path":"/server/markups/img.png","name":"markup"}]},
        "created_at":"2026-11-11T00:00:00Z","updated_at":"2026-11-11T00:00:00Z",
        "removed_at":null,"sent_reply_id":null
    })).expect("record");
    let mut note = annotation::import(&record).expect("import");
    note.body = "New words".to_owned();
    let wire = annotation::to_wire(&note, "Hello").expect("export");
    assert_eq!(wire["id"], "server-id");
    assert_eq!(wire["text"], "New words");
    assert_eq!(wire["htmlAnchor"], record.annotation["htmlAnchor"]);
    assert_eq!(wire["images"], record.annotation["images"]);
}
