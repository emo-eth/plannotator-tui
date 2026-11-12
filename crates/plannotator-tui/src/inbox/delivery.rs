//! Prepare the complete reply exactly once, then freeze every byte-bearing field and
//! idempotency key until the server confirms it. No transport retry changes its payload.

use anyhow::{Context as _, Result};

use super::actions::ReplyItems;
use super::client::{Client, HttpError};
use super::send::{self, Target};
use super::ui::{Inbox, Request};

fn prepare(inbox: &mut Inbox, client: &Client, targets: &mut Vec<Target>) -> Result<()> {
    if !targets.is_empty() && targets.iter().all(|target| target.prepared) {
        return Ok(());
    }
    let thread = &inbox.thread.as_ref().context("open a thread before sending")?.model.thread;
    let annotations = client.attachments(&thread.thread_id)?.annotations;
    let pending: Vec<_> =
        annotations.into_iter().filter(super::attachments::AnnotationRecord::pending).collect();
    // `s` sends saved picks/actions/annotations, not a reply the person has not
    // opened. Only submitting the visible reply composer authorizes draft delivery.
    let draft_send = if let Some(words) = targets.iter().find_map(|target| target.words.clone()) {
        if inbox.draft.is_none() {
            inbox.draft = Some(super::drafts::DraftState::load(client, &thread.thread_id)?);
        }
        let draft = inbox.draft.as_mut().context("reply draft is unavailable")?;
        draft.set_body(words)?;
        Some(draft.prepare_send(client, thread)?)
    } else {
        None
    };
    let destination = draft_send
        .as_ref()
        .map(|draft| draft.message_id.as_str())
        .or_else(|| send::words_message(thread).map(|message| message.id.as_str()));
    if draft_send.is_some() || !pending.is_empty() {
        let destination = destination.context("no non-permission message can receive this reply")?;
        let separate = draft_send.as_ref().is_some_and(|draft| draft.to.is_some());
        if draft_send.is_some() {
            for target in targets.iter_mut() {
                target.words = None;
            }
        }
        let index = targets
            .iter()
            .position(|target| !separate && target.message_id == destination)
            .unwrap_or_else(|| {
                targets.push(Target {
                    message_id: destination.to_owned(),
                    idempotency_key: send::new_key(),
                    ..Target::default()
                });
                targets.len() - 1
            });
        let target = targets.get_mut(index).context("reply destination disappeared")?;
        if let Some(draft) = draft_send {
            target.words = (!draft.words.trim().is_empty()).then_some(draft.words);
            target.uploads = draft.uploads;
            target.tags = draft.tags;
            target.to = draft.to;
            target.clear_draft = (!draft.clear_draft.is_empty()).then_some(draft.clear_draft);
        }
        let last = targets.remove(index);
        targets.push(last);
    }
    super::annotation_routes::route(thread, targets, pending)?;
    targets.retain(|target| {
        target.words.is_some()
            || !target.questions.is_empty()
            || !target.actions.is_empty()
            || !target.annotation_ids.is_empty()
            || !target.uploads.is_empty()
    });
    for target in targets.iter_mut() {
        target.prepared = true;
    }
    inbox.replace_pending(targets.clone());
    Ok(())
}

pub(crate) fn send(inbox: &mut Inbox, client: &Client, mut targets: Vec<Target>) {
    if let Err(error) = prepare(inbox, client, &mut targets) {
        inbox.cancel_preparation(&format!("send not started: {error:#}"));
        return;
    }
    if targets.is_empty() {
        inbox.cancel_preparation("nothing to send: pick an answer, annotate a file or write a reply");
        return;
    }
    let mut nothing = 0;
    let mut replayed = 0;
    for (index, target) in targets.iter().enumerate() {
        let items = ReplyItems {
            questions: &target.questions,
            actions: &target.actions,
            annotation_ids: &target.annotation_ids,
            uploads: &target.uploads,
            feedback: target.feedback.as_deref(),
            to: target.to.as_ref(),
            clear_draft: target.clear_draft.as_deref(),
            tags: &target.tags,
            ..ReplyItems::default()
        };
        let result =
            client.reply_items(&target.message_id, &target.idempotency_key, target.words.as_deref(), &items);
        match result {
            Err(error) => {
                let refused =
                    error.downcast_ref::<HttpError>().is_some_and(|error| (400..500).contains(&error.status));
                inbox.send_failed(
                    index,
                    targets.get(index..).unwrap_or_default().to_vec(),
                    &format!("{error:#}"),
                    refused,
                );
                super::reread_thread(inbox, client);
                super::perform(inbox, client, Request::List);
                return;
            }
            Ok(result) => {
                if let Some(held) = &result.held {
                    inbox.send_failed(
                        index,
                        targets.get(index..).unwrap_or_default().to_vec(),
                        &format!("reply held — {}; delivery is not confirmed", held.description(None)),
                        false,
                    );
                    super::reread_thread(inbox, client);
                    return;
                }
                if result.nothing_to_send {
                    nothing += 1;
                }
                replayed += usize::from(result.replayed);
                if (target.clear_draft.is_some()
                    || ((target.words.is_some() || !target.uploads.is_empty())
                        && inbox.draft.as_ref().is_some_and(super::drafts::DraftState::is_remote)))
                    && let Some(draft) = inbox.draft.as_mut()
                {
                    // The reply has been accepted. A failed follow-up draft read must
                    // never turn that accepted reply into a new, duplicate send.
                    let refreshed = if result.nothing_to_send {
                        draft.release_send();
                        draft.refresh(client)
                    } else {
                        draft.after_send(client, &result).map(|_| ())
                    };
                    if let Err(error) = refreshed {
                        inbox.status = Some(format!("reply accepted; draft refresh failed: {error:#}"));
                    }
                }
                if let Some(thread) = inbox.thread.as_mut() {
                    thread.apply_questions(
                        result.message_id.as_deref().unwrap_or(&target.message_id),
                        result.questions,
                    );
                }
            }
        }
    }
    let followup = inbox.status.take();
    inbox.sent(targets.len() - nothing);
    if nothing == targets.len() {
        inbox.status = Some("server confirmed there was nothing new to send".to_owned());
    } else if followup.as_deref().is_some_and(|text| text.starts_with("reply accepted;")) {
        inbox.status = followup;
    } else if replayed > 0 {
        inbox.status = Some(format!(
            "Inbox confirmed {} replies, including {replayed} previously accepted; no duplicate replies created",
            targets.len() - nothing
        ));
    }
    super::reread_thread(inbox, client);
    super::perform(inbox, client, Request::List);
}
