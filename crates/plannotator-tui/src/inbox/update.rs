//! Update, restart, and agent-reload status for the Inbox.
//!
//! Opening this pane reads current state only. Checking, installing, and restarting each need
//! an explicit keypress; operations which change the machine additionally require confirmation.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::Result;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use serde::Deserialize;
use serde_json::{Value, json};

use super::client::Client;
use crate::theme::palette;

const REFRESH_MS: u64 = 250;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct UpdateDetails {
    update: Option<Update>,
    current: String,
    updated: Option<String>,
    latest: Option<Latest>,
    check: Check,
    auto_update: AutoUpdate,
    summary: String,
    command: String,
    plugin_command: Option<String>,
    sessions: Vec<UpdateSession>,
    failure: Option<Value>,
    log: String,
    checked: Option<Checked>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct AutoUpdate {
    on: bool,
    supported: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct Checked {
    ok: bool,
    error: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct Update {
    state: String,
    version: String,
    action: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct Latest {
    version: String,
    published_at: Option<String>,
    fetched_at: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct Check {
    on: bool,
    last_attempt_at: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct UpdateSession {
    mode: String,
    project: String,
    label: String,
    started_at: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct Agents {
    sessions: Option<BTreeMap<String, HostSessions>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct HostSessions {
    running: usize,
    outdated: usize,
    installed: Option<String>,
    update_command: Option<String>,
    sessions: Vec<AgentSession>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct AgentSession {
    project: Option<String>,
    folder: Option<String>,
    busy: Option<bool>,
    plugin: Option<String>,
    outdated: Option<Outdated>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct Outdated {
    loaded: Option<String>,
    installed: Option<String>,
    fix: String,
    update_command: Option<String>,
    reload_command: Option<String>,
    reload_where: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Confirm {
    Update,
    RestartInbox,
    RestartToUpdate,
}

#[derive(Debug, Default)]
struct Pane {
    update: Option<UpdateDetails>,
    agents: Option<Agents>,
    status: Option<String>,
    confirm: Option<Confirm>,
    busy: bool,
    restarting: bool,
    quit: bool,
    scroll: u16,
}

/// Run the update/status pane on the caller's terminal. The optional thread is context only;
/// opening the pane does not mark it read or write anything.
pub(crate) fn run(client: &Client, terminal: &mut DefaultTerminal, thread_id: Option<&str>) -> Result<()> {
    let mut pane = Pane::default();
    pane.status = pane.refresh(client);
    while !pane.quit {
        terminal.draw(|frame| pane.draw(frame, thread_id))?;
        if !event::poll(Duration::from_millis(REFRESH_MS))? {
            continue;
        }
        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            pane.key(client, key.code);
        }
    }
    Ok(())
}

impl Pane {
    fn refresh(&mut self, client: &Client) -> Option<String> {
        let mut errors = Vec::new();
        match client.get_json::<UpdateDetails>("GET", "/api/inbox/update") {
            Ok(update) => self.update = Some(update),
            Err(error) => errors.push(format!("Update details unavailable: {error:#}")),
        }
        match client.get_json::<Agents>("GET", "/api/inbox/agents") {
            Ok(agents) => self.agents = Some(agents),
            Err(error) => errors.push(format!("Agent status unavailable: {error:#}")),
        }
        (!errors.is_empty()).then(|| errors.join("; "))
    }

    fn key(&mut self, client: &Client, key: KeyCode) {
        if self.busy {
            if key == KeyCode::Esc {
                self.confirm = None;
            }
            return;
        }
        if let Some(confirm) = self.confirm {
            match key {
                KeyCode::Esc | KeyCode::Char('n' | 'N') => {
                    self.confirm = None;
                    self.status = Some("Cancelled; nothing changed.".to_owned());
                }
                KeyCode::Enter | KeyCode::Char('y' | 'Y') => self.perform(client, confirm),
                _ => {}
            }
            return;
        }
        match key {
            KeyCode::Esc | KeyCode::Char('q') => self.quit = true,
            KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll = self.scroll.saturating_add(1),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(10),
            KeyCode::Home => self.scroll = 0,
            KeyCode::Char('r') => {
                self.status = self.refresh(client).or_else(|| Some("Status refreshed.".to_owned()));
                self.scroll = 0;
            }
            KeyCode::Char('c') => self.check(client),
            KeyCode::Char('u') => self.request_update(),
            KeyCode::Char('R') => self.confirm = Some(Confirm::RestartInbox),
            KeyCode::Char('U') => self.request_restart_to_update(),
            _ => {}
        }
    }

    fn check(&mut self, client: &Client) {
        self.busy = true;
        let body = json!({"serverSession": client.server_session()});
        match client.post_json::<UpdateDetails>("/api/inbox/update/check", &body) {
            Ok(update) => {
                let checked = update.checked.as_ref();
                let message = match checked {
                    Some(result) if result.ok => "Update check completed.".to_owned(),
                    Some(result) => format!(
                        "Update check failed: {}",
                        result.error.as_deref().unwrap_or("the release check failed")
                    ),
                    None => "Update check returned no outcome.".to_owned(),
                };
                self.update = Some(update);
                self.status = Some(message);
            }
            Err(error) => self.status = Some(format!("Update check failed: {error:#}")),
        }
        self.busy = false;
    }

    fn request_update(&mut self) {
        let Some(update) = self.update.as_ref().and_then(|details| details.update.as_ref()) else {
            self.status = Some("No update is currently available. Press c to check now.".to_owned());
            return;
        };
        match (update.state.as_str(), update.action.as_str()) {
            ("available" | "failed", "script") => self.confirm = Some(Confirm::Update),
            ("ready", _) => self.confirm = Some(Confirm::RestartToUpdate),
            (_, "copy") => {
                self.status =
                    Some("This copy cannot update itself. Run the install command shown below.".to_owned());
            }
            ("updating", _) => self.status = Some("An update is already running.".to_owned()),
            _ => self.status = Some("This update cannot be applied from the Inbox.".to_owned()),
        }
    }

    fn request_restart_to_update(&mut self) {
        if self
            .update
            .as_ref()
            .and_then(|details| details.update.as_ref())
            .is_some_and(|update| update.state == "ready")
        {
            self.confirm = Some(Confirm::RestartToUpdate);
        } else {
            self.status = Some("No installed update is ready to restart into.".to_owned());
        }
    }

    fn perform(&mut self, client: &Client, confirm: Confirm) {
        self.busy = true;
        self.confirm = None;
        let body = json!({"serverSession": client.server_session()});
        match confirm {
            Confirm::Update => match client.post_json::<Value>("/api/inbox/update", &body) {
                Ok(answer) => {
                    let version =
                        answer.get("version").and_then(Value::as_str).unwrap_or("the available release");
                    let error = self.refresh(client);
                    self.status = Some(format!(
                        "Update started for {version}{}",
                        error.map_or(String::new(), |error| format!("; {error}"))
                    ));
                }
                Err(error) => self.status = Some(format!("Update could not start: {error:#}")),
            },
            Confirm::RestartInbox | Confirm::RestartToUpdate => {
                match client.post_json::<Value>("/api/inbox/restart", &body) {
                    Ok(_) => {
                        self.restarting = true;
                        self.status = Some(if confirm == Confirm::RestartToUpdate {
                            "Restart requested; the Inbox will start the installed update.".to_owned()
                        } else {
                            "Restart requested; the Inbox and its connected sessions will reconnect."
                                .to_owned()
                        });
                    }
                    Err(error) => self.status = Some(format!("Restart could not start: {error:#}")),
                }
            }
        }
        self.busy = false;
    }

    fn draw<'a>(&'a self, frame: &mut ratatui::Frame, thread_id: Option<&'a str>) {
        let area = frame.area();
        let title = if self.restarting {
            "Update and agent status · restart requested"
        } else {
            "Update and agent status"
        };
        let block = Block::default()
            .title(Span::styled(
                format!(" {title} "),
                Style::new().fg(palette().accent).add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let mut lines: Vec<Line<'a>> = vec![Line::from(vec![
            Span::styled("Inbox ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(self.update.as_ref().map_or("version unavailable", |u| u.current.as_str())),
        ])];
        if let Some(thread_id) = thread_id {
            lines.push(Line::from(vec![
                Span::styled("Context thread: ", Style::new().dim()),
                Span::raw(thread_id),
            ]));
        }
        lines.push(Line::from(""));
        self.update_lines(&mut lines);
        lines.push(Line::from(""));
        self.agent_lines(&mut lines);
        if let Some(status) = &self.status {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(status, Style::new().fg(palette().comment))));
        }
        if let Some(confirm) = self.confirm {
            lines.push(Line::from(""));
            let message = match confirm {
                Confirm::Update => {
                    "Install the selected release on this computer? Connected agent sessions may continue using the old plugin until reloaded."
                }
                Confirm::RestartInbox => {
                    "Restart the Inbox now? This affects every connected session; they reconnect automatically."
                }
                Confirm::RestartToUpdate => {
                    "Restart the Inbox now to run the installed update? Every connected session will reconnect."
                }
            };
            lines.push(Line::from(Span::styled(
                message,
                Style::new().fg(palette().delete).add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from("Press Enter or y to confirm · Esc or n to cancel"));
        }
        let footer = if self.busy {
            " Working… "
        } else if self.confirm.is_some() {
            " Confirm / cancel "
        } else {
            " c check · r refresh · u update · U restart to update · R restart Inbox · q close "
        };
        let [body, foot] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
        let content = Paragraph::new(Text::from(lines)).scroll((self.scroll, 0)).wrap(Wrap { trim: false });
        frame.render_widget(content, body);
        frame.render_widget(Paragraph::new(footer).style(Style::new().fg(palette().idle_fg)), foot);
    }

    fn update_lines<'a>(&'a self, lines: &mut Vec<Line<'a>>) {
        lines.push(Line::from(Span::styled(
            "Software update",
            Style::new().fg(palette().heading).add_modifier(Modifier::BOLD),
        )));
        let Some(details) = &self.update else {
            lines.push(Line::from("Update details could not be read."));
            return;
        };
        match &details.update {
            Some(update) => {
                lines.push(Line::from(format!(
                    "State: {} · release {} · {}",
                    update.state, update.version, update.action
                )));
                if update.state == "ready" {
                    lines.push(Line::from("An update is installed and ready; press U to restart into it."));
                } else if update.state == "available" {
                    lines.push(Line::from(
                        "A release is available; press u to review and confirm installation.",
                    ));
                } else if update.state == "updating" {
                    lines
                        .push(Line::from("Installation is running. Refresh with r to see when it is ready."));
                } else if update.state == "failed" {
                    lines.push(Line::from(
                        "The last update attempt failed; inspect the details below before retrying.",
                    ));
                }
            }
            None => lines.push(Line::from("No update is currently offered.")),
        }
        if let Some(latest) = &details.latest {
            lines.push(Line::from(format!(
                "Latest known: {} · checked {}",
                latest.version, latest.fetched_at
            )));
            if let Some(published) = &latest.published_at {
                lines.push(Line::from(format!("Release published: {published}")));
            }
        }
        if let Some(result) = &details.checked {
            lines.push(Line::from(if result.ok {
                "The last explicit check succeeded.".to_owned()
            } else {
                format!(
                    "The explicit check failed: {}",
                    result.error.as_deref().unwrap_or("unknown release-check error")
                )
            }));
        }
        if let Some(attempt) = &details.check.last_attempt_at {
            lines.push(Line::from(format!("Last automatic check attempt: {attempt}")));
        }
        if let Some(updated) = &details.updated {
            lines.push(Line::from(format!("This Inbox was updated to {updated}.")));
        }
        lines.push(Line::from(format!(
            "Automatic checks: {}{}",
            if details.check.on { "on" } else { "off" },
            details.check.error.as_deref().map_or(String::new(), |error| format!(" · {error}"))
        )));
        lines.push(Line::from(format!(
            "Automatic installation: {}",
            if details.auto_update.on {
                "on"
            } else if details.auto_update.supported {
                "off"
            } else {
                "not supported"
            }
        )));
        if !details.summary.is_empty() {
            lines.push(Line::from(details.summary.clone()));
        }
        if !details.command.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("Manual install: ", Style::new().dim()),
                Span::raw(details.command.clone()),
            ]));
        }
        if let Some(command) = &details.plugin_command {
            lines.push(Line::from(vec![
                Span::styled("Plugin install: ", Style::new().dim()),
                Span::raw(command.clone()),
            ]));
        }
        for session in &details.sessions {
            lines.push(Line::from(format!(
                "Open session: {} · {} · {} · started {}",
                session.mode, session.project, session.label, session.started_at
            )));
        }
    }

    fn agent_lines<'a>(&'a self, lines: &mut Vec<Line<'a>>) {
        lines.push(Line::from(Span::styled(
            "Agent sessions",
            Style::new().fg(palette().heading).add_modifier(Modifier::BOLD),
        )));
        let Some(agents) = &self.agents else {
            lines.push(Line::from("Agent status could not be read."));
            return;
        };
        let Some(sessions) = &agents.sessions else {
            lines.push(Line::from("This Inbox version does not report agent session status."));
            return;
        };
        let mut any = false;
        for (host, group) in sessions {
            if group.running == 0 && group.outdated == 0 {
                continue;
            }
            any = true;
            if let Some(installed) = &group.installed {
                lines.push(Line::from(format!("  Version installed on disk: {installed}")));
            }
            lines.push(Line::from(format!(
                "{host}: {} running · {} need attention",
                group.running, group.outdated
            )));
            if let Some(command) = &group.update_command {
                lines.push(Line::from(format!("  Plugin update first: {command}")));
            }
            for session in group.sessions.iter().filter(|session| session.outdated.is_some()) {
                let Some(outdated) = &session.outdated else {
                    continue;
                };
                let place =
                    session.project.as_deref().or(session.folder.as_deref()).unwrap_or("unknown project");
                let versions = format!(
                    "{} → {}",
                    outdated.loaded.as_deref().unwrap_or("unknown"),
                    outdated.installed.as_deref().unwrap_or("unknown")
                );
                lines.push(Line::from(format!("  {place}: {} ({versions})", outdated.fix)));
                if let Some(command) = &outdated.update_command {
                    lines.push(Line::from(format!("    Update plugin: {command}")));
                }
                if let Some(command) = &outdated.reload_command {
                    let where_ = outdated.reload_where.as_deref().unwrap_or("session");
                    lines.push(Line::from(format!("    Reload ({where_}): {command}")));
                } else if outdated.fix == "restart" {
                    lines.push(Line::from("    Reload Claude Code with /reload-plugins"));
                }
                if session.busy == Some(true) {
                    lines.push(Line::from("    Session is working; wait for a safe moment to reload."));
                }
            }
        }
        if !any {
            lines.push(Line::from("No connected agent sessions need a reload."));
        }
        lines.push(Line::from(Span::styled(
            "Reload commands are guidance only; run them in the named agent session.",
            Style::new().dim(),
        )));
    }
}
