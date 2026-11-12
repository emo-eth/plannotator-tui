//! One Send as the Inbox window makes it (packages/inbox/sendTargets.ts): one reply per
//! agent session the thread holds picks on, each answering that session's latest message
//! and naming the picks on its earlier messages by `message_id`; the person's words ride
//! the reply to the agent that wrote most recently. A session is never sent another
//! session's picks: the server refuses those, so they are grouped here first.

use std::collections::HashMap;

use super::actions::{ActionRef, ReplyTag};
use super::client::{Message, Pick, Thread};
use super::questions::State;

/// A permission prompt's own session (`perm:…`): its question takes only its own answer,
/// never the person's words.
const PERMISSION_SESSION: &str = "perm:";

/// One reply of a Send, with the idempotency key it was given when the Send was made. A
/// Send the server did not confirm is replayed reply for reply, keys included, so a reply
/// the server wrote but never answered for is answered with that reply, not written twice.
#[derive(Debug, Clone, Default)]
pub(crate) struct Target {
    pub(crate) message_id: String,
    pub(crate) questions: Vec<Pick>,
    pub(crate) words: Option<String>,
    pub(crate) idempotency_key: String,
    pub(crate) actions: Vec<ActionRef>,
    pub(crate) annotation_ids: Vec<String>,
    pub(crate) uploads: Vec<String>,
    pub(crate) feedback: Option<String>,
    pub(crate) to: Option<super::client::Addressee>,
    pub(crate) clear_draft: Option<String>,
    pub(crate) tags: Vec<ReplyTag>,
    /// Enriched once before the first POST; retries never read a different payload.
    pub(crate) prepared: bool,
}

/// A key no other Send of this process has used.
pub(crate) fn new_key() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("tui-{}-{nanos:x}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed))
}

fn is_agent(message: &Message) -> bool {
    message.author.kind == "agent"
}

fn is_person(message: &Message) -> bool {
    message.author.kind == "person"
}

pub(crate) fn is_permission(message: &Message) -> bool {
    message.author.session.as_deref().is_some_and(|s| s.starts_with(PERMISSION_SESSION))
}

/// The reply a message's questions ride: its session's, or its own when it has none.
fn group_key(message: &Message) -> String {
    match (&message.author.session, is_agent(message)) {
        (Some(session), true) => format!("s:{}\0{session}", message.author.host.as_deref().unwrap_or("")),
        _ => format!("m:{}", message.id),
    }
}

/// The person's latest reply that handed the thread to another session and was not taken
/// back; a later reply that took over from an ended session ends the hand-off.
fn active_handoff(messages: &[Message]) -> Option<usize> {
    for (index, message) in messages.iter().enumerate().rev() {
        if is_person(message)
            && message.to.is_some()
            && message.handoff.as_ref().is_some_and(|h| h.undone_at.is_none())
        {
            return Some(index);
        }
        if message.took_over {
            return None;
        }
    }
    None
}

/// The person's reply holding the thread because the session that asked ended and another
/// took its place, while no agent has written since: that session's latest reply.
fn took_over_reply(messages: &[Message]) -> Option<&Message> {
    if active_handoff(messages).is_some() {
        return None;
    }
    for (index, message) in messages.iter().enumerate().rev() {
        if is_agent(message) && !is_permission(message) {
            return None;
        }
        let Some(owner) = message.to.as_ref().filter(|_| message.took_over) else { continue };
        let later = messages.get(index + 1..).unwrap_or_default();
        return later
            .iter()
            .rev()
            .find(|m| m.to.as_ref().is_some_and(|to| to.session == owner.session))
            .or(Some(message));
    }
    None
}

/// The message the person's words answer: the latest agent message, except after a take-over
/// (that reply) or a hand-off (the new owner's latest message, else the hand-off itself).
pub(crate) fn words_message(thread: &Thread) -> Option<&Message> {
    let messages = &thread.messages;
    if let Some(reply) = took_over_reply(messages) {
        return Some(reply);
    }
    if let Some(index) = active_handoff(messages) {
        let handoff = messages.get(index)?;
        let owner = handoff.to.as_ref().map(|to| to.session.as_str());
        let later = messages.get(index + 1..).unwrap_or_default();
        return later
            .iter()
            .rev()
            .find(|m| is_agent(m) && !is_permission(m) && m.author.session.as_deref() == owner)
            .or(Some(handoff));
    }
    messages
        .iter()
        .rev()
        .find(|m| is_agent(m) && !is_permission(m))
        .or_else(|| messages.iter().find(|m| !is_permission(m)))
}

/// The replies one Send makes, in thread order, the one carrying `words` last, each with a
/// fresh key. Empty when the thread has no message at all. `words` blank and no pick:
/// nothing to send.
pub(crate) fn targets(thread: &Thread, words: &str) -> Vec<Target> {
    let mut groups: HashMap<String, Target> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for message in thread.messages.iter().filter(|m| is_agent(m) && !is_permission(m)) {
        let picks: Vec<Pick> = message
            .questions
            .iter()
            .filter(|q| q.state == State::Picked && q.kind != super::questions::Kind::Action)
            .map(|q| Pick {
                message_id: Some(message.id.clone()),
                key: q.key.clone(),
                revision: q.revision,
                answer: None,
            })
            .collect();
        let actions: Vec<ActionRef> = message
            .questions
            .iter()
            .filter(|question| {
                question.action_result.as_ref().is_some_and(|result| {
                    result.in_draft(question.state, question.revision, question.sent_revision)
                })
            })
            .map(|question| ActionRef { message_id: Some(message.id.clone()), key: question.key.clone() })
            .collect();
        if picks.is_empty() && actions.is_empty() {
            continue;
        }
        let key = group_key(message);
        let target = groups.entry(key.clone()).or_insert_with(|| {
            order.push(key);
            Target {
                message_id: message.id.clone(),
                questions: Vec::new(),
                words: None,
                idempotency_key: new_key(),
                ..Target::default()
            }
        });
        target.questions.extend(picks);
        target.actions.extend(actions);
        target.message_id.clone_from(&message.id);
    }
    let mut targets: Vec<Target> = order.into_iter().filter_map(|key| groups.remove(&key)).collect();
    for target in &mut targets {
        for pick in &mut target.questions {
            if pick.message_id.as_deref() == Some(target.message_id.as_str()) {
                pick.message_id = None;
            }
        }
        for action in &mut target.actions {
            if action.message_id.as_deref() == Some(target.message_id.as_str()) {
                action.message_id = None;
            }
        }
    }
    let Some(latest) = words_message(thread) else {
        return targets;
    };
    let latest_key = group_key(latest);
    let position = targets.iter().position(|t| {
        t.message_id == latest.id
            || thread.messages.iter().any(|m| m.id == t.message_id && group_key(m) == latest_key)
    });
    let words = (!words.trim().is_empty()).then(|| words.to_owned());
    match position {
        Some(index) => {
            let mut last = targets.remove(index);
            last.words = words;
            targets.push(last);
        }
        None if words.is_some() => {
            targets.push(Target {
                message_id: latest.id.clone(),
                questions: Vec::new(),
                words,
                idempotency_key: new_key(),
                ..Target::default()
            });
        }
        None => {}
    }
    targets
}
