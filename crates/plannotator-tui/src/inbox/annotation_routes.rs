//! Pending message comments follow their author's session; files and guides follow words.

use super::attachments::AnnotationRecord;
use super::client::{Message, Thread};
use super::send::{self, Target};
use anyhow::Result;

fn same_session(a: &Message, b: &Message) -> bool {
    a.author.kind == "agent"
        && b.author.kind == "agent"
        && a.author.session.is_some()
        && a.author.session == b.author.session
        && a.author.host.as_deref().unwrap_or("") == b.author.host.as_deref().unwrap_or("")
}

pub(crate) fn route(
    thread: &Thread,
    targets: &mut Vec<Target>,
    records: Vec<AnnotationRecord>,
) -> Result<()> {
    let Some(last) = targets.last() else { return Ok(()) };
    let last_key = last.idempotency_key.clone();
    let last_message = last
        .to
        .is_none()
        .then(|| thread.messages.iter().find(|message| message.id == last.message_id))
        .flatten();
    let mut carried: std::collections::HashMap<String, Vec<AnnotationRecord>> =
        std::collections::HashMap::new();
    for record in records {
        let message = record
            .message
            .as_ref()
            .and_then(|_| thread.messages.iter().find(|message| message.id == record.message_id));
        let key = if let Some(message) = message.filter(|message| {
            !last_message.is_some_and(|last| message.id == last.id || same_session(message, last))
        }) {
            if let Some(target) = targets.iter().find(|target| {
                target.to.is_none()
                    && thread.messages.iter().find(|candidate| candidate.id == target.message_id).is_some_and(
                        |candidate| candidate.id == message.id || same_session(candidate, message),
                    )
            }) {
                target.idempotency_key.clone()
            } else {
                let key = send::new_key();
                targets.push(Target {
                    message_id: message.id.clone(),
                    idempotency_key: key.clone(),
                    ..Target::default()
                });
                key
            }
        } else {
            last_key.clone()
        };
        carried.entry(key).or_default().push(record);
    }
    for target in targets.iter_mut() {
        let Some(records) = carried.remove(&target.idempotency_key) else { continue };
        target.annotation_ids = records.iter().map(|record| record.id.clone()).collect();
        let guide_records =
            records.iter().map(serde_json::to_value).collect::<std::result::Result<Vec<_>, _>>()?;
        let feedback = [
            super::attachments::feedback(&records),
            super::attachments::message_feedback(&records),
            super::reviews::guide_feedback(&guide_records),
        ]
        .into_iter()
        .filter(|words| !words.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
        target.feedback = (!feedback.is_empty()).then_some(feedback);
    }
    targets.sort_by_key(|target| {
        (
            target.idempotency_key == last_key,
            thread.messages.iter().position(|message| message.id == target.message_id).unwrap_or(0),
        )
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::client::Author;
    use super::*;

    #[test]
    fn message_comments_follow_author_sessions_once_while_files_follow_words() -> Result<()> {
        let message = |id: &str, session: &str| Message {
            id: id.to_owned(),
            author: Author {
                kind: "agent".to_owned(),
                session: Some(session.to_owned()),
                ..Author::default()
            },
            ..Message::default()
        };
        let thread = Thread {
            messages: vec![message("first", "one"), message("second", "two"), message("third", "one")],
            ..Thread::default()
        };
        let record = |id: &str, message_id: &str, on_message: bool| -> Result<AnnotationRecord> {
            Ok(serde_json::from_value(serde_json::json!({
                "id":id,"project_id":"p","thread_id":"t","message_id":message_id,
                "attachment_id":"a","path":"file.md","version":"v",
                "annotation":{"body":"note"},"created_at":"","updated_at":"",
                "message":if on_message { serde_json::json!({"quote":"text"}) } else { serde_json::Value::Null }
            }))?)
        };
        let mut targets = vec![Target {
            message_id: "second".to_owned(),
            idempotency_key: "words".to_owned(),
            ..Target::default()
        }];
        route(
            &thread,
            &mut targets,
            vec![record("a", "first", true)?, record("b", "third", true)?, record("file", "first", false)?],
        )?;
        assert_eq!(targets.len(), 2);
        let first = targets.first().ok_or_else(|| anyhow::anyhow!("missing author's target"))?;
        assert_eq!(first.message_id, "first");
        assert_eq!(first.annotation_ids, ["a", "b"]);
        let last = targets.last().ok_or_else(|| anyhow::anyhow!("missing words target"))?;
        assert_eq!(last.message_id, "second");
        assert_eq!(last.annotation_ids, ["file"]);
        Ok(())
    }
}
