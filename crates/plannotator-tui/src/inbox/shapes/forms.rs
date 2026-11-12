use anyhow::{Context, Result};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use serde_json::json;

use super::{FormKind, Opened, Shapes, api};
use crate::app::compose::ComposeAction;
use crate::inbox::client::{Client, HttpError};

impl Shapes {
    pub(super) fn form_key(&mut self, client: &Client, key: KeyEvent) -> Result<()> {
        let Some(form) = &mut self.form else {
            return Ok(());
        };
        if form.confirm {
            match key.code {
                KeyCode::Esc | KeyCode::Char('n') if matches!(&form.kind, FormKind::Write { .. }) => {
                    self.form = None;
                }
                KeyCode::Esc | KeyCode::Char('n') => form.confirm = false,
                KeyCode::Char('y') => return self.submit_form(client),
                _ => {}
            }
        } else {
            match form.editor.handle_key(key) {
                ComposeAction::Cancel => self.form = None,
                ComposeAction::Edited => {}
                ComposeAction::Save => {
                    let text = form.editor.value();
                    match &form.kind {
                        FormKind::Feedback { .. } => {
                            anyhow::ensure!(
                                !text.trim().is_empty(),
                                "Write a question or change request first"
                            );
                            anyhow::ensure!(
                                text.encode_utf16().count() <= 4000,
                                "Shape feedback is limited to 4000 characters by the upstream API"
                            );
                        }
                        FormKind::Setup { project } => {
                            api::setup_body(project, &text)?;
                        }
                        FormKind::Write { .. } => {}
                    }
                    form.confirm = true;
                }
            }
        }
        Ok(())
    }

    fn submit_form(&mut self, client: &Client) -> Result<()> {
        let form = self.form.as_ref().context("No Shape change is pending")?;
        let result = match &form.kind {
            FormKind::Feedback { shape, input } => {
                let mut input = input.clone();
                input
                    .as_object_mut()
                    .context("Shape feedback must be an object")?
                    .insert("text".into(), json!(form.editor.value().trim()));
                api::note(client, shape, input).map(Some)
            }
            FormKind::Setup { project } => {
                api::write(client, "/api/inbox/system/setup", api::setup_body(project, &form.editor.value())?)
                    .map(|_| None)
            }
            FormKind::Write { path, body } => api::write(client, path, body.clone()).map(|_| None),
        };
        let note = match result {
            Ok(note) => note,
            Err(error) => {
                let ambiguous = error.downcast_ref::<HttpError>().is_none_or(|http| http.status >= 500);
                if let Some(form) = &mut self.form {
                    form.uncertain |= ambiguous;
                }
                let suffix = if ambiguous {
                    "The server may have saved this. Shape notes have no idempotency key; another y can duplicate it. Esc edits; a second Esc discards this draft. Close the form, refresh Notes and inspect before sending again."
                } else {
                    "The server refused it. Text is retained; Esc edits it."
                };
                self.status = Some(format!("{error:#}. {suffix}"));
                return Ok(());
            }
        };
        let saved_setup = matches!(self.form.as_ref().map(|form| &form.kind), Some(FormKind::Setup { .. }));
        self.form = None;
        self.status = Some(if saved_setup {
            "System setup saved. This does not start a drawing: upstream first-draw launch is unavailable."
                .to_owned()
        } else if let Some(note) = &note {
            if let Some(Opened::Shape { loaded, .. }) = &self.opened {
                self.worker = api::deliver(client, loaded, note);
            }
            if self.worker.is_some() {
                "Feedback saved; contacting its live drawing agent. You can keep browsing.".to_owned()
            } else {
                "Feedback saved; it waits for the drawing agent's next `shape read --notes`.".to_owned()
            }
        } else {
            "Board change saved.".to_owned()
        });
        if let Err(error) = self.refresh(client) {
            self.status = Some(format!(
                "Saved successfully, but the next read failed: {error:#}. R refreshes; do not resend."
            ));
        }
        Ok(())
    }
}
