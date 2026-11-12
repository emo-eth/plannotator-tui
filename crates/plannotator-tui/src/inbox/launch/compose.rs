//! New thread/session picker. Its final confirmation is the only point that starts an agent.

use std::fmt::Write as _;

use anyhow::{Context, Result, ensure};
use ratatui::DefaultTerminal;
use serde_json::{Value, json};

use super::super::client::Client;
use super::{
    ComposeOutcome, api,
    delivery::{self, Outcome},
    folders,
    model::Machines,
    screen::{self, Choice},
};

pub(super) enum Destination {
    Local,
    Remote(String),
}

pub(super) fn choose_machine(client: &Client, terminal: &mut DefaultTerminal) -> Result<Option<Destination>> {
    let settings = api::settings(client)?;
    if settings.guest() {
        return Ok(Some(Destination::Local));
    }
    let machines: Machines = api::get(client, None, "machines")?;
    let mut labels = vec!["This Inbox's computer".into()];
    let remote: Vec<_> = machines.machines.iter().filter(|m| !m.is_self).collect();
    labels.extend(remote.iter().map(|m| {
        format!(
            "{} · {}{}",
            m.name,
            m.state,
            match m.launch {
                Some(false) => " · new agents off",
                None => " · launch capability unknown",
                Some(true) => "",
            }
        )
    }));
    match screen::choose(
        terminal,
        "New message on…",
        "The agent and folder are on this machine, with its own settings and permissions.\nAn offline machine cannot supply a fresh session/folder catalog.",
        &labels,
        false,
    )? {
        Choice::Selected(0) => Ok(Some(Destination::Local)),
        Choice::Selected(i) => Ok(remote.get(i - 1).map(|m| Destination::Remote(m.id.clone()))),
        _ => Ok(None),
    }
}

fn choose_recipient(
    client: &Client,
    terminal: &mut DefaultTerminal,
    machine: Option<&str>,
) -> Result<Option<(Value, String)>> {
    let settings = api::settings(client)?;
    ensure!(!settings.guest() || machine.is_none(), "Guests cannot write to linked machines");
    let sessions = api::sessions(client, machine, None)?;
    let live: Vec<_> =
        sessions.projects.iter().flat_map(|p| p.sessions.iter().map(move |s| (p, s))).collect();
    let mut labels: Vec<String> = live
        .iter()
        .map(|(p, s)| {
            format!(
                "Message {} · {} · {} · {}",
                s.label.as_deref().unwrap_or(&s.host),
                p.project.name,
                s.started_at,
                if s.busy == Some(true) { "Working" } else { "Idle" }
            )
        })
        .collect();
    let live_count = labels.len();
    if !settings.guest()
        && let Some(catalog) = &sessions.launch
    {
        labels.extend(catalog.agents.iter().map(|a| format!("New {} session", a.name)));
    }
    let mut detail = format!("Home: {}\n", sessions.home);
    for project in &sessions.projects {
        let _ = writeln!(detail, "{} · {}", project.project.name, project.project.root);
    }
    if let Some(catalog) = &sessions.launch {
        for agent in &catalog.not_ready {
            let _ = writeln!(
                detail,
                "{} unavailable: {}{}",
                agent.name,
                agent.code,
                if agent.over_ssh { " (SSH can lock macOS sign-in)" } else { "" }
            );
        }
    } else {
        detail.push_str("Starting new agents is off or unsupported by this Inbox. Existing sessions can still be messaged.\n");
    }
    if settings.guest() {
        detail.push_str("Guest access: starting new agents is owner-only.\n");
    }
    let Choice::Selected(selected) = screen::choose(terminal, "Choose a recipient", &detail, &labels, false)?
    else {
        return Ok(None);
    };
    let mut trust_detail = String::new();
    let to = if selected < live_count {
        let (_, session) = live.get(selected).context("Selected session disappeared")?;
        json!({"host": session.host, "session": session.id})
    } else {
        ensure!(!settings.guest(), "Only this computer's owner can start agents");
        let catalog = sessions.launch.as_ref().context("Launch is unavailable")?;
        let agent = catalog.agents.get(selected - live_count).context("Selected agent disappeared")?;
        let Some((mut target, project)) =
            folders::choose(client, terminal, machine, catalog, &sessions.home, settings.local())?
        else {
            return Ok(None);
        };
        api::set(&mut target, "agent", Value::String(agent.id.clone()))?;
        if let Some(project) = project {
            // Only the selected folder's fresh facts may be consented to. No empty-facts fallback.
            let current = api::sessions(client, machine, Some((&agent.id, &project)))?;
            let available = current
                .launch
                .as_ref()
                .and_then(|l| l.agents.iter().find(|a| a.id == agent.id))
                .context("This agent is no longer available")?;
            if let Some(trust) = available.untrusted.get(&project) {
                let facts = trust
                    .facts
                    .as_ref()
                    .context("Trust details were not returned; refresh before starting")?;
                trust_detail = format!("Trust folder: {}\n{}\n", trust.folder, facts.join("\n"));
                api::set(&mut target, "trust", json!({"folder": trust.folder, "facts": facts}))?;
            } else if project == "scratch" && available.scratch_trusted == Some(false) {
                trust_detail = "The agent does not trust the scratch root. It may stop on trust; use the thread's Trust folder/Open in Terminal action afterwards.\n".into();
            }
        }
        json!({"launch": target})
    };
    Ok(Some((to, trust_detail)))
}

pub(super) fn on(
    client: &Client,
    terminal: &mut DefaultTerminal,
    machine: Option<&str>,
    initial: &str,
    thread_name: Option<&str>,
    board_request: bool,
) -> Result<ComposeOutcome> {
    let Some((to, trust_detail)) = choose_recipient(client, terminal, machine)? else {
        return Ok(ComposeOutcome::Cancelled);
    };
    let Some(mut body) = screen::text(
        terminal,
        "Message",
        "Write the first message. Enter reviews the send; it does not send yet.",
        initial,
    )?
    else {
        return Ok(ComposeOutcome::Cancelled);
    };
    let Some(name) = screen::text(
        terminal,
        "Thread name (optional)",
        "A typed name remains yours. Blank lets the Inbox derive the subject.",
        thread_name.unwrap_or(""),
    )?
    else {
        return Ok(ComposeOutcome::Cancelled);
    };
    loop {
        ensure!(!body.trim().is_empty(), "Write a message before sending");
        let target = serde_json::to_string_pretty(&to)?;
        let detail = format!(
            "Machine: {}\nRecipient:\n{target}\n{trust_detail}\nThread name: {}\nBoard tracking: {}\n\nMessage:\n{body}",
            machine.unwrap_or("this computer"),
            name.trim(),
            board_request
        );
        let action = if to.get("launch").is_some() {
            if trust_detail.starts_with("Trust folder:") { "Trust and start" } else { "Start agent and send" }
        } else {
            "Send message"
        };
        if !screen::confirm(terminal, "Confirm new thread", &detail, action)? {
            return Ok(ComposeOutcome::Cancelled);
        }
        let mut request = api::keyed();
        api::set(&mut request, "to", to.clone())?;
        api::set(&mut request, "body", Value::String(body.clone()))?;
        if !name.trim().is_empty() {
            api::set(&mut request, "thread_name", Value::String(name.trim().into()))?;
        }
        if board_request {
            api::set(&mut request, "board_request", Value::Bool(true))?;
        }
        let outcome = match delivery::submit(client, terminal, machine, "messages", &request) {
            Ok(outcome) => outcome,
            Err(error) => {
                // A failed Send never silently loses the person's words. Do not save an
                // uncertain duplicate addressed to the same agent: the draft is unaddressed.
                match super::super::machines::save_new_draft(client, &body) {
                    Ok(id) => screen::notice(
                        terminal,
                        "Message kept in Drafts",
                        &format!(
                            "Draft {id}\nThe send failed or is unconfirmed: {error:#}\nCheck the destination before sending the draft again."
                        ),
                    )?,
                    Err(save) => {
                        screen::notice(
                            terminal,
                            "Send and draft save failed",
                            &format!(
                                "Send: {error:#}\nDraft: {save:#}\nYour words remain below; copy them before leaving.\n\n{body}"
                            ),
                        )?;
                    }
                }
                return Err(error);
            }
        };
        match outcome {
            Outcome::Delivered(answer) => {
                return Ok(ComposeOutcome::Started(
                    serde_json::from_value(answer).context("Reading started thread")?,
                ));
            }
            Outcome::Held => return Ok(ComposeOutcome::Held),
            Outcome::Cancelled => match super::super::machines::save_new_draft(client, &body) {
                Ok(id) => {
                    screen::notice(
                        terminal,
                        "Cancelled — nothing was sent",
                        &format!("Your words were saved in Drafts ({id})."),
                    )?;
                    return Ok(ComposeOutcome::Cancelled);
                }
                Err(error) => {
                    screen::notice(
                        terminal,
                        "Cancelled; draft was not saved",
                        &format!("{error:#}\nYour words remain in the editor."),
                    )?;
                    let Some(edited) = screen::text(
                        terminal,
                        "Cancelled — your message is kept",
                        "Nothing was sent. Edit to try another send, or copy your words before leaving.",
                        &body,
                    )?
                    else {
                        return Ok(ComposeOutcome::Cancelled);
                    };
                    body = edited;
                }
            },
        }
    }
}
