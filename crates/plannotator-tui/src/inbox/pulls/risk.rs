use anyhow::{Result, ensure};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};

use super::manage::{PullRisk, RiskLevel, RiskModels, RiskSettings};
use super::{Client, PullOptions};
use crate::app::compose::{Compose, ComposeAction};

#[derive(Debug)]
pub(super) struct RiskPane {
    answer: PullRisk,
    draft: RiskSettings,
    models: RiskModels,
    editor: Option<Compose>,
    scroll: usize,
    status: String,
}

impl RiskPane {
    pub(super) fn load(client: &Client) -> Result<Self> {
        let answer = client.pull_risk()?;
        let models = answer
            .provider
            .as_deref()
            .map(|provider| client.pull_risk_models(provider))
            .transpose()?
            .unwrap_or_default();
        let draft = RiskSettings {
            rules: answer.rules.clone(),
            provider: answer.provider.clone(),
            model: answer.model.clone(),
        };
        Ok(Self { answer, draft, models, editor: None, scroll: 0, status: String::new() })
    }

    fn dirty(&self) -> bool {
        self.draft.rules != self.answer.rules
            || self.draft.provider != self.answer.provider
            || self.draft.model != self.answer.model
    }

    pub(super) fn paste(&mut self, text: &str) {
        if let Some(editor) = &mut self.editor {
            editor.insert_text(text);
        }
    }

    pub(super) fn handle(&mut self, key: KeyEvent, client: &Client) -> Result<bool> {
        if let Some(editor) = &mut self.editor {
            match editor.handle_key(key) {
                ComposeAction::Save => {
                    self.draft.rules = editor.value();
                    self.editor = None;
                }
                ComposeAction::Cancel => self.editor = None,
                ComposeAction::Edited => {}
            }
            return Ok(false);
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(true),
            KeyCode::Char('e') => self.editor = Some(Compose::with_text(&self.draft.rules)),
            KeyCode::Char('p') => {
                let provider = match self
                    .draft
                    .provider
                    .as_ref()
                    .and_then(|id| self.answer.agents.iter().position(|agent| &agent.id == id))
                {
                    Some(index) => self.answer.agents.get(index + 1),
                    None => self.answer.agents.first(),
                }
                .map(|agent| agent.id.clone());
                let models = provider
                    .as_deref()
                    .map(|id| client.pull_risk_models(id))
                    .transpose()?
                    .unwrap_or_default();
                self.draft.provider = provider;
                self.draft.model.clone_from(&models.default_model);
                self.models = models;
            }
            KeyCode::Char('m') => {
                ensure!(self.draft.provider.is_some(), "Choose a risk agent with p first.");
                self.draft.model = match self
                    .draft
                    .model
                    .as_ref()
                    .and_then(|id| self.models.models.iter().position(|model| &model.id == id))
                {
                    Some(index) => self.models.models.get(index + 1),
                    None => self.models.models.first(),
                }
                .map(|model| model.id.clone());
            }
            KeyCode::Char('s') => {
                self.answer = client.save_pull_risk(&self.draft)?;
                self.draft = RiskSettings {
                    rules: self.answer.rules.clone(),
                    provider: self.answer.provider.clone(),
                    model: self.answer.model.clone(),
                };
                self.status =
                    "Settings saved. r refreshes rankings; empty rules or no provider disable ranking."
                        .into();
            }
            KeyCode::Char('r') => {
                ensure!(
                    !self.dirty(),
                    "Save your risk settings with s, or close to discard them before refreshing."
                );
                client.pulls(PullOptions::default())?;
                self.answer = client.pull_risk()?;
                self.status =
                    "Rankings refreshed. Pending and failed judgments are never treated as Low.".into();
            }
            KeyCode::Down | KeyCode::Char('j') => self.scroll = self.scroll.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(20),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(20),
            KeyCode::Home => self.scroll = 0,
            _ => {}
        }
        Ok(false)
    }

    pub(super) fn draw(&self, frame: &mut Frame, area: Rect, error: Option<&str>) {
        let [header, body, footer] =
            Layout::vertical([Constraint::Length(3), Constraint::Min(1), Constraint::Length(3)]).areas(area);
        let provider = self.draft.provider.as_deref().unwrap_or("None (ranking disabled)");
        let model = self.draft.model.as_deref().unwrap_or("Agent default");
        let model_label = self
            .models
            .models
            .iter()
            .find(|candidate| Some(&candidate.id) == self.draft.model.as_ref())
            .map_or(model, |m| m.label.as_str());
        let agent_label = self
            .answer
            .agents
            .iter()
            .find(|agent| agent.id == provider)
            .map_or(provider, |a| a.label.as_str());
        frame.render_widget(Paragraph::new(format!("PR risk rankings{} · {} pending\nAgent: {agent_label} [{provider}]\nModel: {model_label} [{model}]", if self.dirty() { " (unsaved settings)" } else { "" }, self.answer.pending)), header);
        let mut lines = vec![
            Line::from("Rules (empty disables ranking):"),
            Line::from(self.draft.rules.clone()),
            Line::from(""),
        ];
        if let Some(failure) = &self.answer.last_failure {
            lines.push(Line::from(format!("Last failure: {failure}")));
        }
        let mut ranked: Vec<_> = self.answer.judgments.iter().collect();
        ranked.sort_by_key(|(_, judgment)| match judgment.level {
            RiskLevel::High => 0,
            RiskLevel::Medium => 1,
            RiskLevel::Low => 2,
        });
        for (key, judgment) in ranked {
            lines.push(Line::from(format!("{key} · {:?}: {}", judgment.level, judgment.why)));
            for place in &judgment.places {
                let [start, end] = place.lines;
                lines.push(Line::from(format!(
                    "  {}:{start}-{end} ({}) · {}",
                    place.file, place.side, place.why
                )));
                if let Some(rule) = &place.rule {
                    lines.push(Line::from(format!("  Rule: {rule}")));
                }
            }
        }
        for (key, reason) in &self.answer.failed {
            lines.push(Line::from(format!("{key} · Could not rank: {reason}")));
        }
        if self.answer.judgments.is_empty() && self.answer.failed.is_empty() {
            lines.push(Line::from(
                "No judgments yet. r asks the Inbox to rank its listed PRs and reads current results.",
            ));
        }
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((self.scroll.min(usize::from(u16::MAX)) as u16, 0)),
            body,
        );
        frame.render_widget(Paragraph::new(format!("e edit rules · p next agent/None · m next model/default · s SAVE settings\nr refresh rankings · j/k/PgUp/PgDn scroll · Esc discard unsaved settings/back\n{}", error.unwrap_or(&self.status))), footer);
        if let Some(editor) = &self.editor {
            super::pane::draw_compose(
                frame,
                area,
                editor,
                "Risk rules · Enter keep draft · Alt+Enter newline · Esc cancel",
            );
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing, reason = "tests assert by panicking")]
mod tests {
    use super::super::tests::fixture;
    use super::*;
    use ratatui::crossterm::event::KeyModifiers;
    use serde_json::json;

    #[test]
    fn provider_model_and_rules_are_drafts_until_explicit_save() {
        let answer = json!({"serverSession":"srv_test","rules":"","provider":null,"model":null,"agents":[{"id":"pi-sdk","label":"Pi"}],"judgments":{},"failed":{},"pending":0,"last_failure":null});
        let saved = json!({"serverSession":"srv_test","rules":"Protect migrations","provider":"pi-sdk","model":"vendor/model","agents":[{"id":"pi-sdk","label":"Pi"}],"judgments":{},"failed":{},"pending":0,"last_failure":null});
        let (client, server) = fixture(vec![
            (200, answer),
            (
                200,
                json!({"serverSession":"srv_test","models":[{"id":"vendor/model","label":"Model"}],"default_model":null}),
            ),
            (200, saved),
        ]);
        let mut pane = RiskPane::load(&client).expect("risk");
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        pane.handle(key('p'), &client).expect("provider reads models only");
        pane.handle(key('m'), &client).expect("model draft");
        pane.handle(key('e'), &client).expect("edit rules");
        pane.paste("Protect migrations");
        pane.handle(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &client).expect("keep draft");
        assert!(pane.dirty());
        assert!(pane.answer.rules.is_empty());
        assert!(pane.handle(key('r'), &client).is_err());
        pane.handle(key('s'), &client).expect("explicit save");
        assert!(!pane.dirty());
        let requests = server.join().expect("server");
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[1].0, "GET /api/inbox/pulls/risk HTTP/1.1\r\n");
        assert_eq!(requests[2].0, "GET /api/inbox/pulls/risk/models?provider=pi-sdk HTTP/1.1\r\n");
        assert_eq!(requests[3].0, "POST /api/inbox/pulls/risk HTTP/1.1\r\n");
        assert_eq!(
            requests[3].1,
            json!({"serverSession":"srv_test","rules":"Protect migrations","provider":"pi-sdk","model":"vendor/model"})
        );
    }
}
