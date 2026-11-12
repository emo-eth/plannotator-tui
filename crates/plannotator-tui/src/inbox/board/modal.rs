use anyhow::{Context, Result, ensure};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use serde_json::json;

use crate::app::compose::ComposeAction;

use super::super::client::Client;
use super::forms::{Choice, Form, FormKind, Modal, Submission};
use super::model::Column;
use super::{Board, BoardAction};

impl Board {
    pub(super) fn modal_key(&mut self, key: KeyEvent, client: &Client) -> Result<Option<BoardAction>> {
        let Some(mut modal) = self.modal.take() else {
            return Ok(None);
        };
        match &mut modal {
            Modal::Form(form) => match form.key(key) {
                ComposeAction::Cancel => {}
                ComposeAction::Edited => self.modal = Some(modal),
                ComposeAction::Save => {
                    let submission = match form.submit() {
                        Ok(submission) => submission,
                        Err(error) => {
                            self.modal = Some(modal);
                            return Err(error);
                        }
                    };
                    match submission {
                        Submission::Search(query) => {
                            self.query = query;
                            self.cursor = 0;
                            self.rebuild(None);
                        }
                        Submission::Confirm { prompt, path, body } => {
                            self.modal = Some(Modal::Confirm { prompt, path, body });
                        }
                        Submission::Post { path, body } => {
                            let response = match client.board_post(&path, body) {
                                Ok(response) => response,
                                Err(error) => {
                                    self.modal = Some(modal);
                                    return Err(error);
                                }
                            };
                            self.status = Some("Saved.".to_owned());
                            self.refresh(client)?;
                            if path == "/api/inbox/cards"
                                && let Some(id) =
                                    response.pointer("/card/id").and_then(serde_json::Value::as_str)
                            {
                                self.open_card(client, id)?;
                                self.detail_scroll = 0;
                            }
                        }
                    }
                }
            },
            Modal::Confirm { path, body, .. } => match key.code {
                KeyCode::Char('y' | 'Y') => {
                    let response = match client.board_post(path, body.clone()) {
                        Ok(response) => response,
                        Err(error) => {
                            self.modal = Some(modal);
                            return Err(error);
                        }
                    };
                    self.status = Some(if path == "/api/inbox/tidy" {
                        format!(
                            "Tidy sent: {}",
                            response.get("asked").map_or(String::new(), serde_json::Value::to_string)
                        )
                    } else {
                        "Saved; u restores a cancelled card, I restores a deleted initiative.".to_owned()
                    });
                    self.refresh(client)?;
                }
                KeyCode::Esc | KeyCode::Char('n' | 'N') => {}
                _ => self.modal = Some(modal),
            },
            Modal::Pick(picker) => match key.code {
                KeyCode::Esc => {}
                KeyCode::Up | KeyCode::Char('k') => {
                    picker.cursor = picker.cursor.saturating_sub(1);
                    self.modal = Some(modal);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    picker.cursor = (picker.cursor + 1).min(picker.items.len().saturating_sub(1));
                    self.modal = Some(modal);
                }
                KeyCode::Home => {
                    picker.cursor = 0;
                    self.modal = Some(modal);
                }
                KeyCode::End => {
                    picker.cursor = picker.items.len().saturating_sub(1);
                    self.modal = Some(modal);
                }
                KeyCode::Enter => {
                    if picker.cursor < picker.items.len() {
                        let (_, choice) = picker.items.remove(picker.cursor);
                        return self.choose(choice, client);
                    }
                    self.modal = Some(modal);
                }
                _ => self.modal = Some(modal),
            },
        }
        Ok(None)
    }

    fn choose(&mut self, choice: Choice, client: &Client) -> Result<Option<BoardAction>> {
        match choice {
            Choice::Project(project) => {
                self.project = project;
                self.cursor = 0;
                self.rebuild(None);
            }
            Choice::Filter(initiative) => {
                self.initiative = initiative;
                self.cursor = 0;
                self.rebuild(None);
            }
            Choice::Thread(id) => return Ok(Some(BoardAction::OpenThread(id))),
            Choice::Url(url) => {
                open_url(&url)?;
                self.status = Some(format!("Opened {url}"));
            }
            Choice::Move(card, Column::Dropped) => {
                self.modal = Some(Modal::Form(Form::single(
                    "Cancel card",
                    FormKind::Drop(card),
                    "Reason (optional)",
                    "",
                )));
            }
            Choice::Move(card, column) => {
                client.board_change(&card, &json!({"op":"move", "column":column.wire()}))?;
                self.refresh(client)?;
            }
            Choice::Assign(card, initiative) => {
                client.board_change(&card, &json!({"op":"initiative", "initiative":initiative}))?;
                self.refresh(client)?;
            }
            Choice::NewInitiative => {
                self.modal =
                    Some(Modal::Form(Form::single("New initiative", FormKind::InitiativeNew, "Name", "")));
            }
            Choice::Initiative(id) => {
                if let Some(initiative) = self.model.initiatives.iter().find(|item| item.id == id) {
                    if initiative.deleted_at.is_some() {
                        self.pick(
                            "Deleted initiative",
                            vec![(
                                "Restore initiative and its cards".to_owned(),
                                Choice::RestoreInitiative(id),
                            )],
                        );
                    } else {
                        let name = initiative.name.clone();
                        self.pick(
                            "Edit initiative",
                            vec![
                                ("Rename".to_owned(), Choice::RenameInitiative(id.clone(), name.clone())),
                                (
                                    "Delete (cards remain, unassigned)".to_owned(),
                                    Choice::DeleteInitiative(id.clone(), name),
                                ),
                                (
                                    "Show cards in this initiative".to_owned(),
                                    Choice::Filter(Some(id.clone())),
                                ),
                            ],
                        );
                        if let Some(Modal::Pick(picker)) = &mut self.modal
                            && let Some(tagged) = self.model.initiative_threads.get(&id)
                        {
                            picker.items.extend(tagged.threads.iter().map(|thread| {
                                let subject = self
                                    .model
                                    .threads
                                    .get(thread)
                                    .and_then(|row| row.get("subject"))
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or(thread);
                                (format!("Tagged thread: {subject}"), Choice::Thread(thread.clone()))
                            }));
                        }
                    }
                }
            }
            Choice::RenameInitiative(id, name) => {
                self.modal = Some(Modal::Form(Form::single(
                    "Rename initiative",
                    FormKind::InitiativeRename(id),
                    "Name",
                    &name,
                )));
            }
            Choice::DeleteInitiative(id, name) => {
                self.modal = Some(Modal::Confirm {
                    prompt: format!("Delete “{name}”? Cards remain unassigned. y confirms; n cancels"),
                    path: format!("/api/inbox/initiatives/{id}"),
                    body: json!({"op":"delete"}),
                });
            }
            Choice::RestoreInitiative(id) => {
                client.board_post(&format!("/api/inbox/initiatives/{id}"), json!({"op":"restore"}))?;
                self.refresh(client)?;
            }
            Choice::Target(target, start) => self.deliver(client, target, start)?,
        }
        Ok(None)
    }
}

fn open_url(url: &str) -> Result<()> {
    let parsed = url::Url::parse(url).context("Reading card link")?;
    ensure!(
        matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some(),
        "Only HTTP(S) card links can be opened"
    );
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("rundll32");
        command.arg("url.dll,FileProtocolHandler");
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = std::process::Command::new("xdg-open");
    let status = command
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .context("Opening card link in browser")?;
    ensure!(status.success(), "Browser opener failed for {url}");
    Ok(())
}
