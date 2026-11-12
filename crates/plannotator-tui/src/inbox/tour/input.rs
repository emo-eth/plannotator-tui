use anyhow::{Context, Result, ensure};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{KeyCode, KeyEvent};

use super::super::client::Client;
use super::fixture::{DESIGN, FIRST_QUESTION};
use super::model::{Replay, Stage, Target};
use super::{Editor, Pane};
use crate::app::compose::{Compose, ComposeAction};
use crate::delivery::{Clipboard, Delivery};

impl Pane {
    pub(super) fn key(
        &mut self,
        key: KeyEvent,
        client: &Client,
        terminal: &mut DefaultTerminal,
    ) -> Result<bool> {
        if self.editor.is_some() {
            self.edit(key)?;
            return Ok(false);
        }
        if self.exit {
            match key.code {
                KeyCode::Char('y') => {
                    self.appearance
                        .stamp(client, if self.replay.stage.step() >= 10 { "finished" } else { "skipped" })?;
                    return Ok(true);
                }
                KeyCode::Char('x') => return Ok(true),
                KeyCode::Esc | KeyCode::Char('n') => self.exit = false,
                _ => {}
            }
            return Ok(false);
        }
        if key.code == KeyCode::Esc && self.viewer.is_some() {
            self.viewer = None;
            self.playback = None;
            if self.replay.stage == Stage::ArtifactOpen {
                self.replay.go(Stage::Artifacts);
            }
            self.refresh_body()?;
            return Ok(false);
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                if !self.started {
                    return Ok(true);
                }
                self.exit = true;
            }
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(10),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::Char('b') => {
                self.replay.back()?;
                self.viewer = None;
                self.refresh_body()?;
            }
            KeyCode::Char('f') if self.started && self.replay.stage.step() < 10 => {
                self.replay.skip(&self.fixture)?;
                self.viewer = None;
                self.playback = None;
                self.cursor = 0;
                self.scroll = 0;
                self.refresh_body()?;
                "Interaction explicitly skipped, not completed. Recorded replay continues; skipped steps remain listed.".clone_into(&mut self.status);
            }
            KeyCode::Char('r') => self.retry_asset(client)?,
            KeyCode::Char('c') if self.replay.stage == Stage::Connect => {
                Clipboard.deliver(FIRST_QUESTION).map_err(|error| anyhow::anyhow!("{error}"))?;
                "First-question prompt sent to the terminal clipboard. Paste into your connected agent; nothing was sent to an agent here.".clone_into(&mut self.status);
            }
            KeyCode::Char('a') if matches!(self.replay.stage, Stage::Connect | Stage::Entry) => {
                if self.started || self.replay.stage == Stage::Entry {
                    self.appearance.stamp(client, if self.started { "finished" } else { "skipped" })?;
                }
                super::super::settings_extra::run(client, terminal)?;
                return Ok(true);
            }
            KeyCode::Char('s') if self.replay.stage == Stage::Appearance => {
                self.appearance.restore(client)?;
                self.go(Stage::Connect)?;
                "Restored only appearance fields changed by this tour. Other windows' changes were kept."
                    .clone_into(&mut self.status);
            }
            KeyCode::Char('o') if matches!(self.replay.stage, Stage::WarmPick | Stage::Picture) => {
                self.compose = Compose::default();
                self.editor = Some(Editor::Other { warm: self.replay.stage == Stage::WarmPick });
            }
            KeyCode::Char('x') if matches!(self.replay.stage, Stage::WarmPick | Stage::Picture) => {
                self.pick("Skipped".to_owned())?;
            }
            KeyCode::Char('v') if self.replay.stage == Stage::Picture => {
                let question = self.fixture.question(false)?;
                let name = question
                    .choices
                    .get(self.cursor)
                    .and_then(|choice| choice.picture.as_ref())
                    .context("This choice has no picture")?;
                self.open(client, &self.fixture.picture(name))?;
            }
            #[expect(
                clippy::case_sensitive_file_extension_comparisons,
                reason = "Fixture artifact names are case-sensitive."
            )]
            KeyCode::Char('v') if self.viewer.as_deref().is_some_and(|name| name.ends_with(".html")) => {
                "Interactive HTML preview is unavailable: a browser navigation cannot inherit an enforced local sandbox. Read the source here, or Esc and choose a picture/Markdown artifact.".clone_into(&mut self.status);
            }
            KeyCode::Char(' ') if self.replay.stage == Stage::FilmPlay && self.viewer.is_some() => {
                if self.playback.take().is_none() {
                    self.playback = Some((std::time::Instant::now(), self.replay.moment));
                }
            }
            KeyCode::Left
            | KeyCode::Right
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::Char('j' | 'k' | '+' | '-') => self.move_key(key, client)?,
            KeyCode::Enter => {
                if self.replay.stage == Stage::Connect {
                    self.appearance.stamp(client, "finished")?;
                    return Ok(true);
                }
                self.activate(client)?;
            }
            _ => {
                "Use the controls shown by the coach. Tour actions never reach a real thread."
                    .clone_into(&mut self.status);
            }
        }
        Ok(false)
    }

    fn retry_asset(&mut self, client: &Client) -> Result<()> {
        let name = self
            .viewer
            .clone()
            .or_else(|| match self.replay.stage {
                Stage::FileOpen | Stage::FileComment => Some("evaluation.md".to_owned()),
                Stage::ImageOpen | Stage::ImageDraw | Stage::ImageComment => Some("built-a1.webp".to_owned()),
                Stage::FilmOpen | Stage::FilmPlay | Stage::FilmComment => Some("film-clip.mp4".to_owned()),
                _ => None,
            })
            .context("Retry is for loading assets. Enter performs the current action.")?;
        self.open(client, &name)?;
        "Asset loaded. Enter continues the interaction.".clone_into(&mut self.status);
        Ok(())
    }

    fn move_key(&mut self, key: KeyEvent, client: &Client) -> Result<()> {
        let direction =
            if matches!(key.code, KeyCode::Up | KeyCode::Left | KeyCode::Char('k' | '-')) { -1 } else { 1 };
        match self.replay.stage {
            Stage::ImageDraw => {
                let circle = &mut self.replay.circle;
                match key.code {
                    KeyCode::Left | KeyCode::Right => {
                        circle.x = (circle.x + 0.025 * direction as f64).clamp(0.0, 1.0);
                    }
                    KeyCode::Char('+' | '-') => {
                        circle.radius = (circle.radius + 0.015 * direction as f64).clamp(0.02, 0.5);
                    }
                    _ => circle.y = (circle.y + 0.025 * direction as f64).clamp(0.0, 1.0),
                }
                self.replay.drawing = true;
            }
            Stage::FilmPlay if matches!(key.code, KeyCode::Left | KeyCode::Right) => {
                self.playback = None;
                let duration = self.fixture.asset("film-clip.mp4")?.duration_ms as f64 / 1000.0;
                self.replay.moment =
                    (self.replay.moment + direction as f64 / super::media::FPS).clamp(0.0, duration);
            }
            Stage::Appearance if matches!(key.code, KeyCode::Left | KeyCode::Right) => {
                let field = ["mode", "light", "size"]
                    .get(self.cursor)
                    .copied()
                    .context("Choose an appearance setting")?;
                self.appearance.choose(client, field, direction)?;
                "Saved to real Inbox appearance. s restores this tour's own changes; Enter keeps them."
                    .clone_into(&mut self.status);
                self.refresh_body()?;
            }
            Stage::WarmPick | Stage::Picture | Stage::Artifacts | Stage::Appearance => {
                let count = match self.replay.stage {
                    Stage::WarmPick => self.fixture.question(true)?.choices.len(),
                    Stage::Picture => self.fixture.question(false)?.choices.len(),
                    Stage::Appearance => 3,
                    _ => self.artifacts().len(),
                };
                self.cursor = self.cursor.saturating_add_signed(direction).min(count.saturating_sub(1));
                self.refresh_body()?;
            }
            _ => self.scroll = self.scroll.saturating_add_signed(direction as i16),
        }
        Ok(())
    }

    fn pick(&mut self, label: String) -> Result<()> {
        if self.replay.stage == Stage::WarmPick {
            self.replay.warm = Some(label);
            self.go(Stage::WarmSend)
        } else {
            self.replay.picked = Some(label);
            self.go(Stage::Words)
        }
    }

    fn activate(&mut self, client: &Client) -> Result<()> {
        match self.replay.stage {
            Stage::Entry => {
                self.appearance.stamp(client, "seen")?;
                self.started = true;
                self.go(Stage::Orient)?;
            }
            Stage::Orient => self.go(Stage::Open)?,
            Stage::Open => self.go(Stage::WarmPick)?,
            Stage::WarmPick | Stage::Picture => {
                let question = self.fixture.question(self.replay.stage == Stage::WarmPick)?;
                self.pick(question.choices.get(self.cursor).context("Choose an answer")?.label.clone())?;
            }
            Stage::WarmSend | Stage::Send | Stage::FilmSend | Stage::NewSend => {
                self.replay.send()?;
                self.viewer = None;
                self.refresh_body()?;
            }
            Stage::Words => {
                let message = self.fixture.question_message(false)?;
                self.compose = Compose::with_text(&message.suggest.phrase);
                self.editor = Some(Editor::Quote {
                    source: message.body.clone(),
                    target: Target::Message,
                    suggestion: message.suggest.comment.clone(),
                });
            }
            Stage::FileOpen => {
                self.open(client, "evaluation.md")?;
                self.replay.go(Stage::FileComment);
                self.refresh_body()?;
            }
            Stage::FileComment => {
                self.open(client, "evaluation.md")?;
                let asset = self.fixture.asset("evaluation.md")?;
                self.compose = Compose::with_text(&asset.suggest.phrase);
                self.editor = Some(Editor::Quote {
                    source: self
                        .assets
                        .texts
                        .get("evaluation.md")
                        .cloned()
                        .context("Evaluation was not loaded")?,
                    target: Target::File,
                    suggestion: asset.suggest.comment.clone(),
                });
            }
            Stage::ImageOpen => {
                self.open(client, "built-a1.webp")?;
                self.replay.go(Stage::ImageDraw);
                self.refresh_body()?;
            }
            Stage::ImageDraw => {
                ensure!(
                    self.replay.drawing,
                    "Move the circle with arrows or resize it with +/- before saving"
                );
                self.replay.go(Stage::ImageComment);
                self.compose = Compose::with_text(&self.fixture.asset("built-a1.webp")?.suggest.comment);
                self.editor = Some(Editor::Comment { target: Target::Image, quote: "a1.png".to_owned() });
            }
            Stage::ImageComment => {
                self.compose = Compose::with_text(&self.fixture.asset("built-a1.webp")?.suggest.comment);
                self.editor = Some(Editor::Comment { target: Target::Image, quote: "a1.png".to_owned() });
            }
            Stage::Artifacts => {
                let name = self.artifacts().get(self.cursor).context("Choose an artifact")?.key().to_owned();
                self.open(client, &name)?;
                self.replay.go(Stage::ArtifactOpen);
                self.refresh_body()?;
            }
            Stage::ArtifactOpen => self.go(Stage::FilmOpen)?,
            Stage::FilmOpen => {
                self.open(client, "film-clip.mp4")?;
                self.replay.go(Stage::FilmPlay);
                self.refresh_body()?;
            }
            Stage::FilmPlay | Stage::FilmComment => {
                self.open(client, "film-clip.mp4")?;
                self.playback = None;
                self.compose = Compose::with_text(&self.fixture.asset("film-clip.mp4")?.suggest.comment);
                self.editor = Some(Editor::Comment {
                    target: Target::Film,
                    quote: format!("guides-vscode.mp4 at {:.1}s", self.replay.moment),
                });
                self.replay.go(Stage::FilmComment);
            }
            Stage::FilmReplay if self.replay.arrived > 0 => self.go(Stage::NewOpen)?,
            Stage::NewOpen => self.go(Stage::NewRecipient)?,
            Stage::NewRecipient => {
                self.replay.recipient = true;
                self.go(Stage::NewCompose)?;
                self.activate(client)?;
            }
            Stage::NewCompose => {
                self.compose = Compose::with_text(Replay::compose_suggestion(&self.fixture)?);
                self.editor = Some(Editor::New);
            }
            Stage::Loop => self.go(Stage::Also)?,
            Stage::Also => self.go(Stage::Appearance)?,
            Stage::Appearance => self.go(Stage::Connect)?,
            _ => {
                "Waiting for the recorded replay. f explicitly skips this step.".clone_into(&mut self.status);
            }
        }
        Ok(())
    }

    fn edit(&mut self, key: KeyEvent) -> Result<()> {
        match self.compose.handle_key(key) {
            ComposeAction::Cancel => self.editor = None,
            ComposeAction::Edited => {}
            ComposeAction::Save => {
                let body = self.compose.value();
                ensure!(!body.trim().is_empty(), "Enter text before saving");
                if let Some(Editor::Quote { source, .. }) = &self.editor {
                    ensure!(
                        source.contains(body.trim()),
                        "Select exact words from the message or file; this text was not found"
                    );
                }
                match self.editor.take().context("No tour editor is open")? {
                    Editor::Quote { target, suggestion, .. } => {
                        self.editor = Some(Editor::Comment { target, quote: body.trim().to_owned() });
                        self.compose = Compose::with_text(&suggestion);
                    }
                    Editor::Comment { target, quote } => {
                        self.replay.save_comment(target, quote, body)?;
                        self.viewer = None;
                        self.refresh_body()?;
                    }
                    Editor::New => {
                        self.replay.new_body = body;
                        self.go(Stage::NewSend)?;
                    }
                    Editor::Other { warm } => {
                        if warm {
                            self.replay.warm = Some(body);
                            self.go(Stage::WarmSend)?;
                        } else {
                            self.replay.picked = Some(body);
                            self.go(Stage::Words)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub(super) fn artifacts(&self) -> Vec<&super::fixture::Asset> {
        self.fixture
            .threads
            .iter()
            .filter(|thread| thread.id == DESIGN)
            .flat_map(|thread| thread.messages.iter().rev())
            .flat_map(|message| message.inline_images.iter().chain(&message.attachments))
            .collect()
    }
}
