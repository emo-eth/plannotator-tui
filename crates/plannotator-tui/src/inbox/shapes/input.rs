use std::fmt::Write as _;

use anyhow::{Context, Result};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use serde_json::json;

use super::{Form, FormKind, Opened, Shapes, Tab, Target, api};
use crate::app::compose::Compose;
use crate::inbox::attachments::open_browser;
use crate::inbox::client::{Client, path_id};

impl Shapes {
    pub(crate) fn handle_key(&mut self, client: &Client, key: KeyEvent) -> Result<bool> {
        if self.form.is_some() {
            return self.form_key(client, key).map(|()| false);
        }
        if self.help {
            self.help = false;
            return Ok(false);
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                if self.file.take().is_some() {
                    self.detail_scroll = 0;
                } else if self.opened.take().is_some() {
                    self.page = 0;
                    self.selected = 0;
                    self.detail_scroll = 0;
                    self.refresh(client)?;
                } else {
                    return Ok(true);
                }
            }
            KeyCode::Tab => {
                self.tab = if self.tab == Tab::Shapes { Tab::System } else { Tab::Shapes };
                self.opened = None;
                self.file = None;
                self.page = 0;
                self.selected = 0;
                self.rows.clear();
                self.refresh(client)?;
            }
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char('R') => self.refresh(client)?,
            KeyCode::Up | KeyCode::Char('k') => self.select(self.selected.saturating_sub(1)),
            KeyCode::Down | KeyCode::Char('j') => {
                self.select((self.selected + 1).min(self.rows.len().saturating_sub(1)));
            }
            KeyCode::Home => self.select(0),
            KeyCode::End => self.select(self.rows.len().saturating_sub(1)),
            KeyCode::PageUp => self.detail_scroll = self.detail_scroll.saturating_sub(10),
            KeyCode::PageDown => self.detail_scroll = self.detail_scroll.saturating_add(10),
            KeyCode::Char(number @ '1'..='7') if self.opened.is_some() => {
                self.page = number as usize - '1' as usize;
                self.selected = 0;
                self.file = None;
                self.detail_scroll = 0;
                self.rebuild();
            }
            KeyCode::Char('f') if self.opened.is_none() && self.tab == Tab::Shapes => {
                self.filter_thread = !self.filter_thread && self.thread.is_some();
                self.selected = 0;
                self.rebuild();
            }
            KeyCode::Char('A') if self.opened.is_none() && self.tab == Tab::Shapes => {
                self.finished = !self.finished;
                self.refresh(client)?;
            }
            KeyCode::Enter => self.activate(client)?,
            KeyCode::Char('o') => self.open_graphic(client)?,
            KeyCode::Char('e' | 'c' | 'a') => {
                if self.tab == Tab::System {
                    if key.code == KeyCode::Char('e') {
                        self.edit_setup(client)?;
                    } else {
                        anyhow::bail!(
                            "System has no feedback endpoint; use its linked review or drawing agent. e edits setup; o opens the actual drawing."
                        );
                    }
                } else {
                    self.feedback(client, key.code == KeyCode::Char('a'))?;
                }
            }
            KeyCode::Char('v' | 'x') => self.state_form(key.code == KeyCode::Char('v'))?,
            _ => {}
        }
        Ok(false)
    }

    fn select(&mut self, index: usize) {
        if self.selected != index {
            self.selected = index;
            self.detail_scroll = 0;
            self.file = None;
        }
    }

    fn activate(&mut self, client: &Client) -> Result<()> {
        match self.rows.get(self.selected).map(|row| &row.target) {
            Some(Target::Shape(id)) => {
                let id = id.clone();
                self.page = 0;
                self.selected = 0;
                self.open_shape(client, &id, None)?;
            }
            Some(Target::System(id)) => {
                let id = id.clone();
                self.page = 0;
                self.selected = 0;
                self.open_system(client, &id)?;
            }
            Some(Target::Version(n)) => {
                let n = *n;
                if let Some(Opened::Shape { loaded, .. }) = &self.opened {
                    let id = loaded.shape.id.clone();
                    self.page = 0;
                    self.selected = 0;
                    self.open_shape(client, &id, Some(n))?;
                }
            }
            Some(Target::Reference(reference)) => {
                let reference = reference.clone();
                let Some(Opened::Shape { loaded, .. }) = &self.opened else {
                    return Ok(());
                };
                match reference.kind.as_str() {
                    "file" => {
                        let content = api::file(client, &loaded.shape.id, &reference.target)?;
                        self.file = Some((reference.target, content));
                        self.detail_scroll =
                            u16::try_from(reference.line.unwrap_or(1).saturating_sub(1)).unwrap_or(u16::MAX);
                    }
                    "image" if reference.target.starts_with("files/") => {
                        open_browser(&api::asset_url(client, &loaded.shape.id, &reference.target)?)?;
                    }
                    "link" | "pr" | "image" => open_browser(&reference.target)?,
                    _ => self.open_graphic(client)?,
                }
            }
            Some(Target::Sheet(_) | Target::Pull(_)) => self.open_graphic(client)?,
            _ => {}
        }
        Ok(())
    }

    fn open_graphic(&mut self, client: &Client) -> Result<()> {
        let url = match &self.opened {
            Some(Opened::Shape { loaded, .. }) => api::shape_url(client, loaded)?,
            Some(Opened::System { drawing, .. }) => {
                let mut fragment = format!("reviews&system={}&full=1", api::encode(&drawing.project.id));
                match self.rows.get(self.selected).map(|row| &row.target) {
                    Some(Target::Sheet(sheet)) => write!(fragment, "&sheet={}", api::encode(sheet))?,
                    Some(Target::Pull(pull)) => write!(fragment, "&stack={}", api::encode(pull))?,
                    _ => {}
                }
                api::browser_url(client, &fragment)?
            }
            None => match self.rows.get(self.selected).map(|row| &row.target) {
                Some(Target::Shape(id)) => {
                    api::browser_url(client, &format!("shapes&shape={}", api::encode(id)))?
                }
                Some(Target::System(id)) => {
                    api::browser_url(client, &format!("reviews&system={}&full=1", api::encode(id)))?
                }
                _ => anyhow::bail!("Select a board or System project first"),
            },
        };
        open_browser(&url)?;
        self.status = Some(format!("Opened upstream graphical viewer: {url}. R reads its changes."));
        Ok(())
    }

    fn feedback(&mut self, client: &Client, ask: bool) -> Result<()> {
        anyhow::ensure!(
            self.worker.is_none(),
            "The previous saved note is still being delivered to its agent"
        );
        if self.opened.is_none() {
            self.activate(client)?;
        }
        let Some(Opened::Shape { loaded, .. }) = &self.opened else {
            anyhow::bail!("Open a Shape board first");
        };
        anyhow::ensure!(
            loaded.shape.state == "active",
            "This board is finished; x reopens it before feedback"
        );
        let mut input = json!({"kind": if ask {"ask"} else {"change"}, "v": loaded.version.n, "view": null, "part": null, "quote": null});
        let mut scope = "whole board".to_owned();
        if let Some(Target::Part { view, id, quote }) = self.rows.get(self.selected).map(|row| &row.target)
            && view != "header"
        {
            let fields = input.as_object_mut().context("Shape feedback must be an object")?;
            fields.insert("view".into(), json!(view));
            fields.insert("part".into(), json!(id));
            fields.insert(
                "quote".into(),
                json!(
                    quote
                        .chars()
                        .scan(0, |units, ch| {
                            *units += ch.len_utf16();
                            (*units <= 600).then_some(ch)
                        })
                        .collect::<String>()
                ),
            );
            scope = format!("{view}/{id}");
        }
        self.form = Some(Form {
            title: format!(
                "{} · v{} · {scope}",
                if ask { "Ask the drawing agent" } else { "Request a change (not graphic editing)" },
                loaded.version.n
            ),
            editor: Compose::default(),
            kind: FormKind::Feedback { shape: loaded.shape.id.clone(), input },
            confirm: false,
            uncertain: false,
        });
        Ok(())
    }

    fn edit_setup(&mut self, client: &Client) -> Result<()> {
        let project = match &self.opened {
            Some(Opened::System { drawing, .. }) => drawing.project.id.clone(),
            _ => match self.rows.get(self.selected).map(|row| &row.target) {
                Some(Target::System(id)) => id.clone(),
                _ => anyhow::bail!("Select a System project first"),
            },
        };
        let project = api::setup(client, &project)?;
        let mut body = project.setup.unwrap_or_else(
            || json!({"repos": [], "docs": [], "agent": "claude", "model": "claude-opus-5-5"}),
        );
        let object = body.as_object_mut().context("System saved setup is not an object")?;
        object.remove("v");
        object.remove("thread_message");
        self.status = Some(format!(
            "{} · {} · drawing {} · repo {}. Save only; upstream cannot launch the first drawing yet.",
            project.name, project.root, project.drawing, project.repo
        ));
        self.form = Some(Form {
            title: format!("System setup · {} · repos/docs/agent/model JSON", project.name),
            editor: Compose::with_text(&serde_json::to_string_pretty(&body)?),
            kind: FormKind::Setup { project: project.id },
            confirm: false,
            uncertain: false,
        });
        Ok(())
    }

    fn state_form(&mut self, seen: bool) -> Result<()> {
        let Some(Opened::Shape { loaded, .. }) = &self.opened else {
            anyhow::bail!("Open a Shape board first");
        };
        let (label, sub, body) = if seen {
            (format!("Mark v{} seen", loaded.version.n), "seen", json!({"n": loaded.version.n}))
        } else {
            let state = if loaded.shape.state == "active" { "finished" } else { "active" };
            (format!("Set board state to {state}"), "state", json!({"state": state}))
        };
        self.form = Some(Form {
            title: label,
            editor: Compose::default(),
            kind: FormKind::Write { path: format!("/api/shapes/{}/{sub}", path_id(&loaded.shape.id)?), body },
            confirm: true,
            uncertain: false,
        });
        Ok(())
    }
}
