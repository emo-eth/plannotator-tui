//! Permission answers use the question's exact offered labels, never inferred tool grants.

use anyhow::{Context, Result, ensure};
use ratatui::DefaultTerminal;
use serde::Deserialize;
use serde_json::{Value, json};

use super::super::{
    client::Client,
    questions::{Answer, Kind, Question},
};
use super::{
    api,
    delivery::{self, Outcome},
    model::{LaunchMessage, ThreadRead},
    screen::{self, Choice},
};

const FULL: &str = "Allow and switch to Full permissions";
const TERMINAL: &str = "Approve in Terminal";

pub(super) fn is_permission(message: &LaunchMessage) -> bool {
    message.message.author.kind == "agent"
        && message.message.author.session.as_deref().is_some_and(|s| s.starts_with("perm:"))
}

/// Guard both the special card and its HTTP schema: generic question sends must never reach here.
pub(super) fn pick_body(
    message: &LaunchMessage,
    question: &Question,
    label: Option<&str>,
    reason: Option<&str>,
    confirm_full: bool,
) -> Result<Value> {
    ensure!(is_permission(message), "This message is not a permission prompt");
    ensure!(
        question.is_open() && question.kind == Kind::Single && !question.orphaned,
        "This permission prompt is no longer answerable"
    );
    if let Some(label) = label {
        ensure!(
            question.choices.iter().any(|c| c.label == label),
            "That permission is not offered by this prompt"
        );
        ensure!(label != FULL || confirm_full, "Full permissions require explicit confirmation");
    } else {
        ensure!(reason.is_some_and(|s| !s.trim().is_empty()), "Deny with a reason needs an explanation");
    }
    if let Some(reason) = reason {
        ensure!(reason.encode_utf16().count() <= 2_000, "A denial reason holds at most 2,000 UTF-16 units");
    }
    let answer = Answer {
        selected: label.into_iter().map(str::to_owned).collect(),
        other: reason.map(str::to_owned),
        ..Answer::default()
    }
    .serialize(question);
    let mut body = api::keyed();
    api::set(
        &mut body,
        "questions",
        json!([{"key": question.key, "revision": question.revision, "answer": answer}]),
    )?;
    api::set(&mut body, "unsent_only", Value::Bool(true))?;
    if label == Some(FULL) && confirm_full {
        api::set(&mut body, "confirm_full", Value::Bool(true))?;
    }
    Ok(body)
}

#[derive(Deserialize)]
struct Picked {
    questions: Vec<Question>,
}

pub(super) fn answer(
    client: &Client,
    terminal: &mut DefaultTerminal,
    machine: Option<&str>,
    read: &ThreadRead,
    message: &LaunchMessage,
    can_open: bool,
) -> Result<()> {
    let Some(question) = message.message.questions.first() else {
        return screen::notice(terminal, "Permission record", &message.message.body);
    };
    let detail = format!(
        "{}\n\n{}\n\nState: {:?}\n{}",
        question.prompt,
        message.message.body,
        question.state,
        question.summary()
    );
    if !question.is_open() {
        return screen::notice(terminal, "Permission record (read only)", &detail);
    }
    ensure!(
        question.kind == Kind::Single && !question.orphaned,
        "Unsupported or orphaned permission question; answer it in the agent's terminal"
    );
    // Remote prompts that only the host can approve must be answered on that machine's screen.
    let labels: Vec<String> = question
        .choices
        .iter()
        .filter(|c| machine.is_none() || c.label != TERMINAL)
        .map(|c| c.label.clone())
        .collect();
    let mut menu = labels.clone();
    menu.push("Deny with a reason…".into());
    let Choice::Selected(i) = screen::choose(terminal, "Agent permission request", &detail, &menu, false)?
    else {
        return Ok(());
    };
    let (label, reason) = if i == labels.len() {
        let Some(reason) = screen::text(
            terminal,
            "Deny with a reason",
            "The explanation is sent as a denial, never as permission to run a different command.",
            "",
        )?
        else {
            return Ok(());
        };
        (None, Some(reason))
    } else {
        (labels.get(i).map(String::as_str), None)
    };
    let full = label == Some(FULL);
    let warning = if full {
        "Switch this session to Full permissions? The agent will run commands, edit files and open pages without asking until this session ends.\n\n"
    } else {
        ""
    };
    let decision = label.unwrap_or("Deny with a reason");
    let confirm =
        format!("{warning}Chosen answer: {decision}\n{}\n\n{detail}", reason.as_deref().unwrap_or(""));
    if !screen::confirm(terminal, "Confirm permission answer", &confirm, decision)? {
        return Ok(());
    }
    let request = pick_body(message, question, label, reason.as_deref(), full)?;
    let path = format!("messages/{}/picks", api::id(&message.message.id)?);
    let saved = match delivery::submit(client, terminal, machine, &path, &request)? {
        Outcome::Delivered(value) => {
            serde_json::from_value::<Picked>(value).context("Reading saved permission pick")?
        }
        Outcome::Held => {
            screen::notice(
                terminal,
                "Permission pick is held",
                "The pick is not an answer yet. Once the machine confirms it, reopen this prompt and explicitly answer it.",
            )?;
            return Ok(());
        }
        Outcome::Cancelled => return Ok(()),
    };
    let saved = saved
        .questions
        .iter()
        .find(|q| q.key == question.key)
        .context("The server did not return this permission question; no reply was sent")?;
    ensure!(saved.is_open(), "This permission prompt already settled; no reply was sent");
    let mut request = api::keyed();
    api::set(&mut request, "words", json!(""))?;
    api::set(&mut request, "questions", json!([{"key": question.key, "revision": saved.revision}]))?;
    if machine.is_some() {
        let seen = read
            .thread
            .messages
            .iter()
            .filter_map(|m| m.message.seq)
            .max()
            .context("Remote thread omitted message sequences; refresh before replying")?;
        api::set(&mut request, "seen_through_seq", json!(seen))?;
    }
    match delivery::submit(
        client,
        terminal,
        machine,
        &format!("messages/{}/reply", api::id(&message.message.id)?),
        &request,
    )? {
        Outcome::Delivered(_) if label == Some(TERMINAL) && can_open => {
            if let Some(launch) = read.thread.messages.first().and_then(|m| m.launch.as_ref()) {
                // This is the explicitly confirmed Approve in Terminal hand-off, not an allow from here.
                delivery::submit(
                    client,
                    terminal,
                    None,
                    &format!("launch/{}/open", api::id(&launch.id)?),
                    &api::keyed(),
                )?;
            }
        }
        Outcome::Delivered(_) => screen::notice(
            terminal,
            "Permission answer sent",
            &format!("You chose {decision}. The run status reports what the agent does next."),
        )?,
        Outcome::Held | Outcome::Cancelled => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::super::{
        client::{Author, Message},
        questions::{Choice, State},
    };
    use super::*;

    fn prompt() -> (LaunchMessage, Question) {
        let message = LaunchMessage {
            message: Message {
                author: Author { kind: "agent".into(), session: Some("perm:s1".into()), ..Author::default() },
                ..Message::default()
            },
            launch: None,
        };
        let question = Question {
            key: "q1".into(),
            kind: Kind::Single,
            prompt: "Run a command".into(),
            state: State::Open,
            revision: 7,
            choices: vec![Choice { label: FULL.into(), ..Choice::default() }],
            ..Question::default()
        };
        (message, question)
    }

    #[test]
    fn full_permissions_require_explicit_confirmation_and_offered_label() {
        let (message, question) = prompt();
        assert!(pick_body(&message, &question, Some(FULL), None, false).is_err());
        assert!(pick_body(&message, &question, Some("Allow once"), None, true).is_err());
        let result = pick_body(&message, &question, Some(FULL), None, true);
        assert!(result.is_ok_and(|v| v.get("unsent_only") == Some(&Value::Bool(true))
            && v.get("confirm_full") == Some(&Value::Bool(true))));
    }

    #[test]
    fn sent_permissions_cannot_be_reopened() {
        let (message, mut question) = prompt();
        question.state = State::Sent;
        assert!(pick_body(&message, &question, Some(FULL), None, true).is_err());
    }
}
