//! Inbox launch/session/permission surface, matching upstream v0.28.61 and main.
//!
//! `L` opens [`run`] in the Inbox's existing terminal; Escape returns without closing it.
//! With a thread selected it reads the run state every five seconds and offers only the
//! actions appropriate to that state and owner. Without a thread it opens New message.
//! Up/down chooses, Enter selects, PageUp/PageDown scrolls full details, and `r` refreshes.
//! Every mutation has a separate confirmation; permission controls arm for 600 ms and
//! ignore held Enter keys. Full permissions receives its own explicit scope warning.
//!
//! New message selects a linked machine, a live session or an available launch agent,
//! then a project, scratch folder, browsed/created folder or cloned repository. Trust
//! consent carries exactly the fresh folder/facts or warning shown to the person. The
//! server performs all launches, filesystem changes and agent-specific permission work.
//! Terminal opens only on the local Inbox; attach/clone commands are copied, never run.
//! Stop does not stop background commands; Delete warns about scratch-folder removal.
//!
//! Remote writes retain their idempotency keys, distinguish queued from delivered, and
//! offer cancellation while queued. A machine must provide a fresh catalog before a
//! new recipient/folder can be chosen; an offline machine with no catalog cannot be
//! guessed. Existing permission questions remain answerable when new launches are off.
//! Reads, including snapshot mode elsewhere, never invoke this module's mutations.

mod api;
mod compose;
mod delivery;
mod folders;
mod lifecycle;
mod model;
mod permission;
mod screen;

use anyhow::Result;
use ratatui::DefaultTerminal;

use super::client::Client;
pub(crate) use model::Started;

/// A queued remote write is not a cancellation and has not created a confirmed thread.
#[derive(Debug)]
pub(crate) enum ComposeOutcome {
    Started(Started),
    Held,
    Cancelled,
}

/// Main Inbox integration: uses the caller's raw/alternate-screen lifetime.
pub(crate) fn run(client: &Client, terminal: &mut DefaultTerminal, thread_id: Option<&str>) -> Result<()> {
    if let Some(thread_id) = thread_id {
        lifecycle::run(client, terminal, thread_id)
    } else {
        if let Some(started) = compose(client, terminal, "", false)? {
            screen::notice(
                terminal,
                "Thread created",
                &format!("Thread: {}\nThe Inbox list will refresh on return.", started.thread_id),
            )?;
        }
        Ok(())
    }
}

/// Board and New-message integration. A held remote launch is never a completed thread.
pub(crate) fn compose(
    client: &Client,
    terminal: &mut DefaultTerminal,
    body: &str,
    board_request: bool,
) -> Result<Option<Started>> {
    let Some(machine) = compose::choose_machine(client, terminal)? else { return Ok(None) };
    let machine_id = match &machine {
        compose::Destination::Local => None,
        compose::Destination::Remote(id) => Some(id.as_str()),
    };
    compose_on(client, terminal, machine_id, body, None, board_request)
}

/// Machines/update integration: fixes the destination machine while keeping the real
/// session/agent picker, editable message, and explicit final confirmation.
pub(crate) fn compose_on(
    client: &Client,
    terminal: &mut DefaultTerminal,
    machine: Option<&str>,
    body: &str,
    thread_name: Option<&str>,
    board_request: bool,
) -> Result<Option<Started>> {
    Ok(match compose_outcome_on(client, terminal, machine, body, thread_name, board_request)? {
        ComposeOutcome::Started(started) => Some(started),
        ComposeOutcome::Held | ComposeOutcome::Cancelled => None,
    })
}

/// For callers which must record a queued request separately from user cancellation.
pub(crate) fn compose_outcome_on(
    client: &Client,
    terminal: &mut DefaultTerminal,
    machine: Option<&str>,
    body: &str,
    thread_name: Option<&str>,
    board_request: bool,
) -> Result<ComposeOutcome> {
    compose::on(client, terminal, machine, body, thread_name, board_request)
}
