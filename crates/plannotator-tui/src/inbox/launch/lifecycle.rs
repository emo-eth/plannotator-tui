//! The run line and session menu, including read-only permission history and attach commands.

use std::fmt::Write as _;

use anyhow::{Context, Result};
use ratatui::DefaultTerminal;
use serde_json::Value;

use crate::delivery::{Clipboard, Delivery};

use super::super::client::Client;
use super::{
    api,
    delivery::{self, Outcome},
    model::{LaunchRecord, Run, Settings, ThreadRead},
    permission,
    screen::{self, Choice},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Stop,
    Retry,
    Trust,
    Open,
    Delete,
    Copy,
    CloneCopy,
    New,
    Permission(usize),
}

fn can_open(settings: &Settings, machine: Option<&str>, launch: &LaunchRecord, run: Option<&Run>) -> bool {
    settings.local()
        && machine.is_none()
        && launch.deleted_at.is_none()
        && (launch.session_ref.is_some() || run.is_some_and(|r| r.trust_scope.untrusted))
}

fn actions(read: &ThreadRead, settings: &Settings, machine: Option<&str>) -> Vec<(String, Action)> {
    let mut actions = Vec::new();
    for (i, message) in read.thread.messages.iter().enumerate().filter(|(_, m)| permission::is_permission(m))
    {
        if let Some(question) = message.message.questions.first() {
            actions.push((
                format!(
                    "{}: {}",
                    if question.is_open() { "Answer permission" } else { "Permission record" },
                    question.prompt
                ),
                Action::Permission(i),
            ));
        }
    }
    if let Some(launch) =
        read.thread.messages.first().and_then(|m| m.launch.as_ref()).filter(|l| l.deleted_at.is_none())
    {
        let state = read.run.as_ref().map(|r| r.state.as_str());
        if !settings.guest() {
            if (launch.session_ref.is_some()
                && matches!(
                    state,
                    None | Some("starting" | "working" | "waiting_answer" | "waiting_terminal")
                ))
                || state == Some("cloning")
            {
                actions.push((
                    (if state == Some("cloning") { "Cancel clone" } else { "Stop session" }).into(),
                    Action::Stop,
                ));
            }
            if matches!(state, Some("failed" | "clone_failed" | "clone_cancelled")) {
                actions.push(("Try again".into(), Action::Retry));
            }
            if read.run.as_ref().is_some_and(|r| {
                r.trust_scope.untrusted
                    && r.trust.is_some()
                    && !r.trust_scope.home_folder
                    && !r.trust_scope.trust_covers_home
            }) {
                actions.push(("Trust folder and start".into(), Action::Trust));
            }
            actions.push(("Delete session…".into(), Action::Delete));
        }
        if can_open(settings, machine, launch, read.run.as_ref()) {
            actions.push(("Open in Terminal on this computer".into(), Action::Open));
        }
        if attach(read).is_some() {
            actions.push(("Copy attach command".into(), Action::Copy));
        }
        if read.run.as_ref().and_then(|r| r.clone.as_ref()).is_some() {
            actions.push(("Copy clone command".into(), Action::CloneCopy));
        }
    }
    actions.push(("New message/session…".into(), Action::New));
    actions
}

fn attach(read: &ThreadRead) -> Option<&str> {
    read.attach_command.as_deref().or_else(|| read.run.as_ref().and_then(|r| r.attach_command.as_deref()))
}

fn describe(read: &ThreadRead, machine: Option<&str>) -> String {
    let mut text =
        format!("Thread: {}\nMachine: {}\n", read.thread.thread_id, machine.unwrap_or("this computer"));
    if let Some(launch) = read.thread.messages.first().and_then(|m| m.launch.as_ref()) {
        let _ = writeln!(
            text,
            "Agent: {} · session: {}\nFolder: {}{}",
            launch.agent,
            launch.session_ref.as_deref().unwrap_or("not started"),
            launch.folder,
            if launch.scratch { " [scratch]" } else { "" }
        );
        if launch.full_permissions {
            text.push_str("Started with Full permissions.\n");
        }
        if launch.deleted_at.is_some() {
            text.push_str("Session deleted. This thread is kept.\n");
        } else if let Some(run) = &read.run {
            let _ = write!(text, "State: {}", run.state);
            if let Some(since) = &run.since {
                let _ = write!(text, " · since {since}");
            }
            text.push('\n');
            if let Some(cause) = &run.cause {
                text.push_str(cause);
                text.push('\n');
            }
            if run.state == "stopped" {
                text.push_str(if run.stopped_by_person { "You stopped this session. A new reply in the thread resumes it; background commands keep running.\n" } else { "This session stopped. A new reply in the thread continues it.\n" });
            }
            if run.state == "missing" {
                text.push_str("The agent is no longer installed. Install it again, or send the waiting reply to another session.\n");
            }
            if run.trust_scope.trust_covers_home {
                text.push_str("Trust would cover the home folder. Start inside a project with its own git repository.\n");
            } else if run.trust_scope.home_folder {
                text.push_str("Home itself cannot be trusted. Start inside a project folder.\n");
            } else if run.trust_scope.untrusted && run.trust.is_none() {
                text.push_str("Trust this folder in the agent's own terminal on that computer.\n");
            }
            if let Some(warning) = run.trust.as_ref().and_then(|t| t.warning.as_deref()) {
                text.push_str(warning);
                text.push('\n');
            }
            if let Some(folder) = &run.made_folder {
                let _ = writeln!(text, "Created folder {folder} stays even if launch failed.");
            }
            if let Some(clone) = &run.clone {
                let _ = writeln!(
                    text,
                    "Clone:\n{}",
                    serde_json::to_string_pretty(clone).unwrap_or_else(|_| clone.to_string())
                );
            }
        } else {
            text.push_str("Agent is resting (no run line). Reply in the thread to continue.\n");
        }
    } else {
        text.push_str(
            "This thread was not created by launching an agent. Permission history is still shown.\n",
        );
    }
    if let Some(command) = attach(read) {
        let _ = writeln!(text, "Attach command (run on the agent's computer):\n{command}");
    }
    text
}

pub(super) fn run(client: &Client, terminal: &mut DefaultTerminal, thread_id: &str) -> Result<()> {
    let (machine, mut read) = api::locate(client, thread_id)?;
    loop {
        let settings = api::settings(client)?;
        let choices = actions(&read, &settings, machine.as_deref());
        let labels: Vec<_> = choices.iter().map(|(label, _)| label.clone()).collect();
        let detail = describe(&read, machine.as_deref());
        let choice = screen::choose(terminal, "Session and permissions", &detail, &labels, true)?;
        match choice {
            Choice::Cancel => return Ok(()),
            Choice::Refresh => {}
            Choice::Selected(i) => {
                if let Some((_, action)) = choices.get(i)
                    && let Err(error) = act(client, terminal, machine.as_deref(), &read, &settings, *action)
                {
                    screen::notice(
                        terminal,
                        "Session action failed",
                        &format!(
                            "{error:#}\n\nThe next read refreshes the current state. A changed trust warning requires a new confirmation."
                        ),
                    )?;
                }
            }
        }
        read = api::thread(client, machine.as_deref(), thread_id)?;
    }
}

fn act(
    client: &Client,
    terminal: &mut DefaultTerminal,
    machine: Option<&str>,
    read: &ThreadRead,
    settings: &Settings,
    action: Action,
) -> Result<()> {
    if let Action::Permission(i) = action {
        let message = read.thread.messages.get(i).context("Permission message disappeared")?;
        let open = read
            .thread
            .messages
            .first()
            .and_then(|m| m.launch.as_ref())
            .is_some_and(|l| can_open(settings, machine, l, read.run.as_ref()));
        return permission::answer(client, terminal, machine, read, message, open);
    }
    if action == Action::New {
        let initial = read
            .run
            .as_ref()
            .filter(|r| r.state == "clone_failed")
            .and_then(|_| read.thread.messages.first())
            .map_or("", |m| m.message.body.as_str());
        super::compose_on(client, terminal, machine, initial, None, false)?;
        return Ok(());
    }
    if action == Action::Copy || action == Action::CloneCopy {
        let command = if action == Action::Copy {
            attach(read).context("No attach command is available")?.to_owned()
        } else {
            clone_command(read.run.as_ref().and_then(|r| r.clone.as_ref()).context("No clone command")?)?
        };
        Clipboard.deliver(&command).map_err(|e| anyhow::anyhow!("Clipboard: {e}"))?;
        return screen::notice(terminal, "Command sent to terminal clipboard", &command);
    }
    let launch =
        read.thread.messages.first().and_then(|m| m.launch.as_ref()).context("This thread has no launch")?;
    let (endpoint, warning, button) = match action {
        Action::Stop => ("stop", "Stop this session? Commands it started in the background keep running. Cancelling a clone removes its partially cloned folder.".to_owned(), "Stop session"),
        Action::Retry => ("retry", "Try this start or clone again? The kept message is sent when the agent starts.".to_owned(), "Try again"),
        Action::Trust => ("trust", format!("Trust {} and start the agent?\n{}", launch.folder, read.run.as_ref().and_then(|r| r.trust.as_ref()).and_then(|t| t.warning.as_deref()).unwrap_or("No additional warning was reported.")), "Trust and start"),
        Action::Open => ("open", "Open this session in Terminal on the Inbox's computer? Pi hands its session to Terminal so there is only one writer.".to_owned(), "Open in Terminal"),
        Action::Delete => ("delete", if launch.scratch { format!("Stop and delete this session, its session files and scratch folder {}, INCLUDING EVERYTHING IN IT? This thread stays.", launch.folder) } else { format!("Stop and delete this session and its session files? Project folder {} and this thread stay.", launch.folder) }, "Delete session"),
        _ => return Ok(()),
    };
    if !screen::confirm(terminal, "Confirm session action", &warning, button)? {
        return Ok(());
    }
    let mut body = api::keyed();
    if action == Action::Trust {
        api::set(
            &mut body,
            "warning",
            read.run
                .as_ref()
                .and_then(|r| r.trust.as_ref())
                .and_then(|t| t.warning.clone())
                .map_or(Value::Null, Value::String),
        )?;
    }
    let outcome = delivery::submit(
        client,
        terminal,
        machine,
        &format!("launch/{}/{}", api::id(&launch.id)?, endpoint),
        &body,
    )?;
    if let Outcome::Delivered(answer) = outcome {
        screen::notice(
            terminal,
            "Session action confirmed",
            if answer.get("scratch_removed") == Some(&Value::Bool(false)) {
                "Session deleted; its scratch folder was kept because it could not be safely removed."
            } else {
                "The server confirmed the action. The session state is refreshed next."
            },
        )?;
    }
    Ok(())
}

fn clone_command(clone: &Value) -> Result<String> {
    fn quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
    let url = clone.get("url").and_then(Value::as_str).context("Clone omitted its URL")?;
    let mut command = "git clone".to_owned();
    if let Some(branch) = clone.get("branch").and_then(Value::as_str) {
        let _ = write!(command, " --branch {}", quote(branch));
    }
    let _ = write!(command, " {}", quote(url));
    if let Some(folder) = clone.get("folder_shown").and_then(Value::as_str) {
        command.push(' ');
        if let Some(rest) = folder.strip_prefix("~/") {
            let _ = write!(command, "~/{}", quote(rest));
        } else {
            command.push_str(&quote(folder));
        }
    }
    Ok(command)
}
