//! Inbox projects: select a thread-list filter, manage names/groups, and operate on batches.
//!
//! `run` uses the caller's terminal without changing its mode. Opening/refreshing only GETs.
//! A `ProjectAction::Filter` must be applied to the parent thread list. For embedded panes,
//! use `refresh`, `set_filter`, `handle_key`, `paste`, and `draw` instead.
//!
//! Keys: arrows/j/k select, Enter filters the Inbox, 0 clears that filter, / finds projects,
//! h includes archived projects, r renames, R resets a custom name, c combines, U separates
//! a combined folder, m marks all read, A archives all, z undoes the last batch/combine,
//! D deletes from the Inbox, F5 refreshes, Esc closes. Destructive actions require a second
//! explicit confirmation; Delete requires the exact project id. Delete is irreversible in
//! both current upstream servers: code stays on disk, but Inbox messages/files are removed.
//! No delete-undo endpoint is invented. Batch Undo uses only ids returned by the server,
//! including `looked`, and retains partial-success Undo. `!` retries an unconfirmed request
//! with the same batch id; `x` explicitly discards it. Errors keep their request context.

mod api;
mod draw;
mod eligibility;
mod input;
mod model;
#[cfg(test)]
mod tests;

use std::fmt::Write as _;

use anyhow::{Context, Result};
use ratatui::crossterm::event::{self, Event, KeyEventKind};

use super::client::Client;
use crate::app::compose::Compose;
use api::{Effect, Mutation};
use model::Project;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ProjectAction {
    Close,
    Filter(Option<String>),
}

enum Modal {
    Search(Compose),
    Rename { project: Project, input: Compose },
    Choose { project: Project, choices: Vec<model::Member>, cursor: usize, separate: bool },
    Confirm { mutation: Mutation, warning: String, expected: Option<String>, input: Compose },
    Discard,
}

#[derive(Default)]
pub(crate) struct Projects {
    entries: Vec<Project>,
    eligibility: std::collections::HashMap<String, eligibility::Counts>,
    visible: Vec<usize>,
    cursor: usize,
    scroll: usize,
    height: usize,
    filter: Option<String>,
    filter_changed: bool,
    query: String,
    show_archived: bool,
    modal: Option<Modal>,
    pending: Option<Mutation>,
    undo: Option<Mutation>,
    undo_filter: Option<String>,
    status: Option<String>,
}

impl Projects {
    pub(crate) fn refresh(&mut self, client: &Client) -> Result<()> {
        let list = api::read(client)?;
        let eligibility = eligibility::read(client)?;
        let keep = self.selected().map(|project| project.id.clone());
        self.entries = list.projects;
        self.eligibility = eligibility;
        self.rebuild(keep.as_deref());
        Ok(())
    }

    pub(crate) fn set_filter(&mut self, project: Option<String>) {
        self.filter = project;
        let keep = self.filter.clone();
        self.rebuild(keep.as_deref());
    }

    pub(crate) fn paste(&mut self, text: &str) {
        match self.modal.as_mut() {
            Some(Modal::Search(input)) => {
                input.insert_text(text);
                self.query = input.value();
                self.rebuild(None);
            }
            Some(Modal::Rename { input, .. } | Modal::Confirm { input, .. }) => input.insert_text(text),
            _ => {}
        }
    }

    fn selected(&self) -> Option<&Project> {
        self.visible.get(self.cursor).and_then(|index| self.entries.get(*index))
    }

    fn rebuild(&mut self, keep: Option<&str>) {
        let query = model::name_key(&self.query);
        self.visible = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, project)| {
                (self.show_archived
                    || project.listed != Some(0)
                    || self.filter.as_deref() == Some(&project.id))
                    && (query.is_empty()
                        || model::name_key(&project.name).contains(&query)
                        || model::name_key(&project.root).contains(&query)
                        || project.id.contains(&query))
            })
            .map(|(index, _)| index)
            .collect();
        self.cursor = keep
            .and_then(|id| {
                self.visible
                    .iter()
                    .position(|index| self.entries.get(*index).is_some_and(|project| project.id == id))
            })
            .unwrap_or(self.cursor)
            .min(self.visible.len().saturating_sub(1));
        self.scroll = self.scroll.min(self.cursor);
    }

    fn submit(&mut self, client: &Client, mutation: Mutation) -> Result<()> {
        self.pending = Some(mutation);
        self.retry(client)
    }

    fn retry(&mut self, client: &Client) -> Result<()> {
        let Some(pending) = self.pending.as_ref() else {
            return Ok(());
        };
        // Preserve the request and its batch id on every error, including a lost response.
        let answer = pending.apply(client)?;
        let undo = pending.inverse(&answer);
        let mut status = match &pending.effect {
            Effect::Batch { archive } => format!(
                "{} {} threads.",
                if *archive { "Archived" } else { "Marked read" },
                answer.batch.len()
            ),
            Effect::Delete => {
                "Deleted from Inbox. Your code was not touched; this cannot be undone.".to_owned()
            }
            _ => format!("{} completed.", pending.label),
        };
        if let Some(error) = &answer.error {
            let _ = write!(status, " {}: {}", error.code, error.message);
        }
        if !answer.skipped.is_empty() {
            let _ = write!(status, " Skipped {}: {}.", answer.skipped.len(), answer.skipped.join(", "));
        }
        if !matches!(pending.effect, Effect::Undo) {
            self.undo_filter = None;
        }
        match &pending.effect {
            Effect::Undo => {
                if let Some(filter) = self.undo_filter.take() {
                    self.filter = Some(filter);
                    self.filter_changed = true;
                }
            }
            Effect::Combine { into: Some(into) } if self.filter.as_deref() == Some(&pending.project) => {
                self.undo_filter.clone_from(&self.filter);
                self.filter = Some(into.clone());
                self.filter_changed = true;
            }
            Effect::Delete if self.filter.as_deref() == Some(&pending.project) => {
                self.filter = None;
                self.filter_changed = true;
            }
            _ => {}
        }
        self.undo = undo;
        if self.undo.is_some() {
            status.push_str(" z: Undo.");
        }
        let keep = answer.project.as_ref().map(|project| project.id.clone());
        self.pending = None;
        self.modal = None;
        self.status = Some(status);
        // A read failure must not turn a confirmed mutation into a request to send again.
        if let Err(error) = self.refresh(client) {
            let status = self.status.get_or_insert_with(String::new);
            let _ = write!(status, " Applied, but refresh failed: {error:#}. F5 retries the read.");
        } else {
            self.rebuild(keep.as_deref());
        }
        Ok(())
    }
}

pub(crate) fn run(
    client: &Client,
    terminal: &mut ratatui::DefaultTerminal,
    thread_id: Option<&str>,
    project_filter: Option<&str>,
) -> Result<Option<ProjectAction>> {
    let mut pane = Projects::default();
    pane.set_filter(project_filter.map(str::to_owned));
    pane.refresh(client)?;
    if let Some(id) = thread_id {
        let thread = client.thread(id).context("Finding the open thread's project")?;
        pane.rebuild(Some(&thread.thread.project.id));
    }
    loop {
        terminal.draw(|frame| pane.draw(frame, frame.area()))?;
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match pane.handle_key(key, client) {
                Ok(Some(ProjectAction::Close)) => {
                    return Ok(pane.filter_changed.then_some(ProjectAction::Filter(pane.filter)));
                }
                Ok(Some(action)) => return Ok(Some(action)),
                Ok(None) => {}
                Err(error) => pane.status = Some(format!("{error:#}")),
            },
            Event::Paste(text) => pane.paste(&text),
            _ => {}
        }
    }
}
