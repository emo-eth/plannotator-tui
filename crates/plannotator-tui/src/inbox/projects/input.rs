use anyhow::{Result, ensure};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serde_json::json;

use super::api::{Effect, Mutation, batch_id};
use super::model::{Member, Project, checked_name, matches_name, name_key};
use super::{Modal, ProjectAction, Projects};
use crate::app::compose::{Compose, ComposeAction};
use crate::inbox::client::Client;

impl Projects {
    pub(crate) fn handle_key(&mut self, key: KeyEvent, client: &Client) -> Result<Option<ProjectAction>> {
        if key.kind != KeyEventKind::Press {
            return Ok(None);
        }
        if self.pending.is_some() && !matches!(self.modal, Some(Modal::Discard)) {
            match key.code {
                KeyCode::Char('!') => self.retry(client)?,
                KeyCode::Char('x') => self.modal = Some(Modal::Discard),
                KeyCode::F(5) => self.refresh(client)?,
                _ => self.status = Some("Request unconfirmed. ! retries the same request; x asks to discard. F5 reads current projects.".to_owned()),
            }
            return Ok(None);
        }
        if let Some(mut modal) = self.modal.take() {
            let result = self.modal_key(&mut modal, key, client);
            if !matches!(&result, Ok(true))
                && self.modal.is_none()
                && !(self.pending.is_some() && matches!(modal, Modal::Discard))
            {
                self.modal = Some(modal);
            }
            return result.map(|_| None);
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(Some(ProjectAction::Close)),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Ok(Some(ProjectAction::Close));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.cursor = (self.cursor + 1).min(self.visible.len().saturating_sub(1));
            }
            KeyCode::Up | KeyCode::Char('k') => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::PageDown => {
                self.cursor = (self.cursor + self.height.max(1)).min(self.visible.len().saturating_sub(1));
            }
            KeyCode::PageUp => self.cursor = self.cursor.saturating_sub(self.height.max(1)),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.visible.len().saturating_sub(1),
            KeyCode::Enter => {
                if let Some(project) = self.selected() {
                    return Ok(Some(ProjectAction::Filter(Some(project.id.clone()))));
                }
            }
            KeyCode::Char('0') => return Ok(Some(ProjectAction::Filter(None))),
            KeyCode::Char('/') => self.modal = Some(Modal::Search(Compose::with_text(&self.query))),
            KeyCode::Char('h') => {
                self.show_archived = !self.show_archived;
                self.rebuild(None);
            }
            KeyCode::F(5) => self.refresh(client)?,
            KeyCode::Char('z' | 'Z') => {
                if let Some(undo) = self.undo.take() {
                    self.submit(client, undo)?;
                }
            }
            KeyCode::Char('r' | 'R' | 'c' | 'U' | 'm' | 'A' | 'D') => {
                if let Some(project) = self.selected().cloned() {
                    self.project_key(key.code, project, client)?;
                }
            }
            _ => {}
        }
        Ok(None)
    }

    fn project_key(&mut self, code: KeyCode, project: Project, client: &Client) -> Result<()> {
        match code {
            KeyCode::Char('r') => {
                let input = Compose::with_text(&project.name);
                self.modal = Some(Modal::Rename { project, input });
            }
            KeyCode::Char('R') => {
                ensure!(project.display_name.is_some(), "{} already uses its folder name.", project.name);
                let mutation = Mutation::new(
                    client,
                    &project.id,
                    format!("Reset name of {}", project.name),
                    "name",
                    json!({"name": null}),
                    Effect::Name,
                );
                self.confirm(mutation, "Reset the custom name to the folder's name?".to_owned(), None);
            }
            KeyCode::Char('c') => {
                let choices = self.entries.iter().filter(|p| p.id != project.id).map(member).collect();
                self.choose(project, choices, false)?;
            }
            KeyCode::Char('U') => {
                let choices = project.combined.clone();
                self.choose(project, choices, true)?;
            }
            KeyCode::Char('m' | 'A') => {
                let archive = code == KeyCode::Char('A');
                if archive {
                    ensure!(project.listed != Some(0), "{} has no listed threads to archive.", project.name);
                }
                let counts = self.eligibility.get(&project.id);
                if !archive {
                    ensure!(
                        counts.is_some_and(|count| count.readable > 0),
                        "{} has no threads waiting to be marked read.",
                        project.name
                    );
                }
                let body = if archive {
                    json!({"batch_id": batch_id(), "resolved": true})
                } else {
                    json!({"batch_id": batch_id()})
                };
                let label =
                    format!("{} all in {}", if archive { "Archive" } else { "Mark read" }, project.name);
                let mutation = Mutation::new(
                    client,
                    &project.id,
                    label,
                    if archive { "resolve" } else { "read" },
                    body,
                    Effect::Batch { archive },
                );
                let impact = counts.map_or_else(String::new, |count| {
                    format!(
                        "Currently {} questions, {} stopped agents, and {} held-up things. ",
                        count.questions, count.stopped, count.holds
                    )
                });
                self.confirm(mutation, format!("{impact}Waiting questions close unanswered. Undo reverses only changed threads; undo within 10 seconds to prevent set-aside notes reaching agents."), None);
            }
            KeyCode::Char('D') => {
                let mutation = Mutation::new(
                    client,
                    &project.id,
                    format!("Delete {} from Inbox", project.name),
                    "delete",
                    json!({}),
                    Effect::Delete,
                );
                let members = if project.combined.is_empty() {
                    String::new()
                } else {
                    format!(
                        " Combined folders remain as their own projects: {}.",
                        project.combined.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
                    )
                };
                self.confirm(mutation, format!(
                    "Permanently removes this folder's Inbox messages and files, NOT your code. Cannot be undone.{members} Type the exact project id to confirm: {}", project.id), Some(project.id));
            }
            _ => {}
        }
        Ok(())
    }

    fn choose(&mut self, project: Project, choices: Vec<Member>, separate: bool) -> Result<()> {
        ensure!(
            !choices.is_empty(),
            "{}",
            if separate {
                "This project has no combined folders."
            } else {
                "There is no other project to combine into."
            }
        );
        self.modal = Some(Modal::Choose { project, choices, cursor: 0, separate });
        Ok(())
    }

    fn confirm(&mut self, mutation: Mutation, warning: String, expected: Option<String>) {
        self.modal = Some(Modal::Confirm { mutation, warning, expected, input: Compose::default() });
    }

    fn combine(&mut self, source: &Member, target: &Project, separate: bool, client: &Client) {
        let into = if separate { None } else { Some(target.id.clone()) };
        let label = if separate {
            format!("Separate {} from {}", source.name, target.name)
        } else {
            format!("Combine {} into {}", source.name, target.name)
        };
        let mutation = Mutation::new(
            client,
            &source.id,
            label,
            "combine",
            json!({"into": into}),
            Effect::Combine { into },
        );
        self.confirm(
            mutation,
            "No messages or files move. Threads, cards and future sends follow the resulting project group."
                .to_owned(),
            None,
        );
    }

    fn modal_key(&mut self, modal: &mut Modal, key: KeyEvent, client: &Client) -> Result<bool> {
        match modal {
            Modal::Search(input) => {
                let action = input.handle_key(key);
                if action == ComposeAction::Cancel {
                    self.query.clear();
                } else {
                    self.query = input.value();
                }
                self.rebuild(None);
                Ok(action != ComposeAction::Edited)
            }
            Modal::Rename { project, input } => match input.handle_key(key) {
                ComposeAction::Cancel => Ok(true),
                ComposeAction::Edited => Ok(false),
                ComposeAction::Save => {
                    let name = checked_name(&input.value())?;
                    if name == project.name {
                        return Ok(true);
                    }
                    let matches = if name_key(&name) == name_key(&project.name) {
                        Vec::new()
                    } else {
                        matches_name(&name, &project.id, &self.entries)
                    };
                    if !matches.is_empty() {
                        let choices = matches.into_iter().map(member).collect();
                        self.choose(project.clone(), choices, false)?;
                        return Ok(true);
                    }
                    let mutation = Mutation::new(
                        client,
                        &project.id,
                        format!("Rename {} to {name}", project.name),
                        "name",
                        json!({"name": name}),
                        Effect::Name,
                    );
                    self.submit(client, mutation)?;
                    Ok(true)
                }
            },
            Modal::Choose { project, choices, cursor, separate } => {
                match key.code {
                    KeyCode::Esc => return Ok(true),
                    KeyCode::Up | KeyCode::Char('k') => *cursor = cursor.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => {
                        *cursor = (*cursor + 1).min(choices.len().saturating_sub(1));
                    }
                    KeyCode::Enter => {
                        if let Some(choice) = choices.get(*cursor) {
                            if *separate {
                                self.combine(choice, project, true, client);
                            } else if let Some(target) =
                                self.entries.iter().find(|p| p.id == choice.id).cloned()
                            {
                                self.combine(&member(project), &target, false, client);
                            }
                            return Ok(true);
                        }
                    }
                    _ => {}
                }
                Ok(false)
            }
            Modal::Confirm { expected, input, .. } => {
                if key.code == KeyCode::Esc || (expected.is_none() && key.code == KeyCode::Char('n')) {
                    return Ok(true);
                }
                let confirmed = if let Some(expected) = expected {
                    match input.handle_key(key) {
                        ComposeAction::Save => {
                            ensure!(
                                input.value() == *expected,
                                "The project id must match exactly. Nothing was deleted."
                            );
                            true
                        }
                        _ => false,
                    }
                } else {
                    key.code == KeyCode::Char('y')
                };
                if !confirmed {
                    return Ok(false);
                }
                // Move the already-confirmed request into pending before any network call.
                let taken = std::mem::replace(modal, Modal::Discard);
                if let Modal::Confirm { mutation, .. } = taken {
                    self.submit(client, mutation)?;
                }
                Ok(true)
            }
            Modal::Discard => {
                if key.code == KeyCode::Char('y') {
                    self.pending = None;
                    self.status = Some("Unconfirmed request discarded locally; the server may already have applied it. F5 reads current projects.".to_owned());
                    Ok(true)
                } else {
                    Ok(matches!(key.code, KeyCode::Esc | KeyCode::Char('n')))
                }
            }
        }
    }
}

fn member(project: &Project) -> Member {
    Member { id: project.id.clone(), name: project.name.clone(), root: project.root.clone() }
}
