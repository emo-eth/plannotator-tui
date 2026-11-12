use anyhow::{Result, ensure};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};

use super::manage::OwnerAsk;
use super::{Client, DiffScope, Pull, PullDiff, PullState};

#[derive(Debug)]
pub(super) struct MergeConfirmation {
    pub(super) key: String,
    pub(super) head: String,
    pub(super) url: String,
}

impl MergeConfirmation {
    pub(super) fn prepare(pull: &Pull, diff: &PullDiff, has_draft: bool) -> Result<Self> {
        ensure!(!has_draft, "Post or discard the review draft before merging.");
        ensure!(pull.state == PullState::Open, "Only an open, non-draft pull request can be merged.");
        ensure!(
            diff.scope == DiffScope::Layer && diff.key == pull.key,
            "Read this pull request's layer before merging."
        );
        let head = diff.head_sha.as_deref().unwrap_or_default();
        ensure!(
            matches!(head.len(), 40 | 64) && head.bytes().all(|b| b.is_ascii_hexdigit()),
            "The diff's commit is unknown; refresh before merging."
        );
        Ok(Self { key: pull.key.clone(), head: head.to_owned(), url: pull.url.clone() })
    }

    pub(super) fn draw(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(Paragraph::new(format!("CONFIRM MERGE as your own forge account\n\n{}\nCommit: {}\n\nThe server rechecks mergeability and pins this exact commit. The repository chooses its merge method. This changes the forge.\n\nEnter/y merges once · Esc cancels", self.url, self.head)).wrap(Wrap { trim: false }), area);
    }
}

#[derive(Debug)]
struct Candidate {
    thread: String,
    project: String,
    label: String,
}

#[derive(Debug)]
pub(super) struct OwnerPane {
    keys: Vec<String>,
    candidates: Vec<Candidate>,
    selected: usize,
    confirmed: bool,
    ask: OwnerAsk,
    pub(super) status: String,
}

impl OwnerPane {
    pub(super) fn load(client: &Client, key: &str) -> Result<Self> {
        let list = client.list()?;
        let candidates = list
            .sections
            .into_iter()
            .flat_map(|section| section.threads)
            .filter(|row| row.machine.is_none())
            .map(|row| Candidate {
                label: format!(
                    "{} · {} · {}",
                    row.project.name,
                    row.thread_name.or(row.subject).unwrap_or_else(|| row.thread_id.clone()),
                    row.thread_id
                ),
                thread: row.thread_id,
                project: row.project.id,
            })
            .collect();
        Ok(Self {
            keys: vec![key.to_owned()],
            candidates,
            selected: 0,
            confirmed: false,
            ask: OwnerAsk::Custom,
            status: String::new(),
        })
    }

    pub(super) fn handle(&mut self, key: KeyEvent, client: &Client) -> Result<bool> {
        if key.kind != KeyEventKind::Press {
            return Ok(false);
        }
        if self.confirmed {
            match key.code {
                KeyCode::Esc => self.confirmed = false,
                KeyCode::Enter | KeyCode::Char('y') => {
                    let Some(candidate) = self.candidates.get(self.selected) else { return Ok(false) };
                    self.confirmed = false;
                    client.set_pull_owner(&self.keys, &candidate.thread, self.ask)?;
                    self.status = format!(
                        "Ownership linked to {}. No message was sent to the agent.",
                        candidate.thread
                    );
                    return Ok(true);
                }
                _ => {}
            }
            return Ok(false);
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(true),
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = self.selected.saturating_add(1).min(self.candidates.len().saturating_sub(1));
            }
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Char('a') => {
                self.ask = match self.ask {
                    OwnerAsk::Guide => OwnerAsk::Custom,
                    OwnerAsk::Custom => OwnerAsk::Guide,
                }
            }
            KeyCode::Enter => {
                if let Some(candidate) = self.candidates.get(self.selected) {
                    client.check_pull_owner(&self.keys, &candidate.project)?;
                    self.confirmed = true;
                }
            }
            _ => {}
        }
        Ok(false)
    }

    pub(super) fn draw(&self, frame: &mut Frame, area: Rect, error: Option<&str>) {
        let [header, body, footer] =
            Layout::vertical([Constraint::Length(3), Constraint::Min(1), Constraint::Length(3)]).areas(area);
        frame.render_widget(Paragraph::new(format!("Hand ownership to an existing thread · {:?}\nThis changes PR routing, but does NOT send a request or launch an agent.\nThe server checks that the thread belongs to this PR's repository.", self.ask)), header);
        let lines: Vec<_> = if self.confirmed {
            self.candidates
                .get(self.selected)
                .map(|candidate| {
                    vec![
                        Line::from(format!("Confirm ownership by {}", candidate.label)),
                        Line::from("Enter/y changes ownership · Esc returns to choices"),
                    ]
                })
                .unwrap_or_default()
        } else if self.candidates.is_empty() {
            vec![Line::from(
                "No local threads available. Create the agent thread first, then reopen this picker.",
            )]
        } else {
            self.candidates
                .iter()
                .enumerate()
                .map(|(index, candidate)| {
                    Line::from(format!(
                        "{} {}",
                        if index == self.selected { ">" } else { " " },
                        candidate.label
                    ))
                })
                .collect()
        };
        let scroll = if self.confirmed {
            0
        } else {
            self.selected.saturating_sub(usize::from(body.height).saturating_sub(2))
        };
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((scroll.min(usize::from(u16::MAX)) as u16, 0)),
            body,
        );
        frame.render_widget(
            Paragraph::new(format!(
                "j/k select · a guide/custom ownership · Enter validate then confirm\nEsc back\n{}",
                error.unwrap_or(&self.status)
            )),
            footer,
        );
    }
}
