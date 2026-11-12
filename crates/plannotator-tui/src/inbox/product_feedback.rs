//! Product feedback: local composition/help, then an explicit confirmed POST to `/api/inbox/feedback`.
//! Drafts live only for this process and Inbox URL; closing, cancelling or a failed send keeps them.
//! The server adds version/OS/agent names. The client sends only message/category/contact/serverSession,
//! never retries, and clears the draft only after `{ ok: true }`. Enter inside the message adds a newline.
//!
//! Help wording and matching are adapted from backnotprop/plannotator's `packages/inbox/feedbackKnown.ts`,
//! `feedbackFaq.ts`, `feedbackPrompt.ts` and the curated features in `feedbackSearch.data.json`, snapshot
//! 2026-11-11. Upstream is MIT licensed; its notice is retained in `product_feedback/LICENSE-MIT`.
//! Synchronize these two JSON bundles and matching rules when upstream feedback help changes. No remote
//! search uses the draft. The large issue/PR/release-title corpus is intentionally not bundled; this
//! limitation and separate browser and terminal list preferences are visible in the pane.

mod actions;
mod help;
mod model;
mod prompt;
mod screen;
#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::collections::HashMap;

use anyhow::Result;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::widgets::ListState;
use serde_json::Value;

use super::client::Client;
use crate::delivery::{Clipboard, Delivery as _};
use help::{Help, Hit};
use model::{Category, Draft};
use screen::Selection;

thread_local! {
    static UNSENT: RefCell<HashMap<String, Draft>> = RefCell::new(HashMap::new());
}

struct Pane {
    draft: Draft,
    help: Help,
    related: Vec<Hit>,
    settings: Option<Value>,
    disclosure: String,
    status: String,
    list: ListState,
    scroll: u16,
}

impl Pane {
    fn load(client: &Client) -> Result<Self> {
        let draft = UNSENT.with(|drafts| drafts.borrow().get(client.url()).cloned().unwrap_or_default());
        let (settings, status) = match super::settings::read(client) {
            Ok(settings) => (Some(settings), String::new()),
            Err(error) => (None, format!("Settings unavailable: {error:#}. Feedback can still be composed.")),
        };
        let help = Help::load()?;
        let related = help.related(&draft.message, settings.as_ref());
        let disclosure = model::disclosure(settings.as_ref());
        Ok(Self {
            draft,
            help,
            related,
            settings,
            disclosure,
            status,
            list: ListState::default().with_selected(Some(0)),
            scroll: 0,
        })
    }

    fn remember(&self, client: &Client) {
        UNSENT.with(|drafts| {
            let mut drafts = drafts.borrow_mut();
            if self.draft == Draft::default() {
                drafts.remove(client.url());
            } else {
                drafts.insert(client.url().to_owned(), self.draft.clone());
            }
        });
    }

    fn activate(&mut self, index: usize, client: &Client, terminal: &mut DefaultTerminal) -> Result<bool> {
        match index {
            0 => {
                self.draft.message = screen::edit(terminal, &self.draft.message, false)?;
                self.related = self.help.related(&self.draft.message, self.settings.as_ref());
                self.scroll = 0;
                self.status = "Draft kept locally; nothing sent.".into();
            }
            1 => {
                let labels = model::CATEGORIES.iter().map(|label| (*label).to_owned()).collect::<Vec<_>>();
                if let Selection::Item(index) = screen::choose(terminal, "Category (optional)", "Choose None to clear the category. The category does not change your message.", &labels)? {
                    self.draft.category = Category::from_index(index);
                }
            }
            2 => {
                self.draft.contact = screen::edit(terminal, &self.draft.contact, true)?;
                self.status = "Contact stays in this draft until you send.".into();
            }
            3 => actions::related(client, terminal, &self.help, &self.related)?,
            4 => actions::questions(client, terminal, &self.help)?,
            5 => {
                match Clipboard.deliver(&prompt::text(&self.draft, self.settings.as_ref())) {
                    Ok(()) => self.status = "Prompt copied to terminal clipboard. Paste into your agent; it must show the exact text and ask before sending. No feedback submitted.".into(),
                    Err(error) => self.status = format!("Could not copy prompt: {error}. Nothing sent."),
                }
            }
            6 => {
                if let Err(error) = self.draft.body() {
                    self.status = error.to_string();
                    return Ok(false);
                }
                let detail = screen::draft_detail(&self.draft, &self.disclosure);
                if matches!(screen::choose(terminal, "Send this exact feedback?", &detail, &["Keep editing (do not send)".into(), "Send to Plannotator maintainers".into()])?, Selection::Item(1)) {
                    self.status = "Sending once; please wait…".into();
                    terminal.draw(|frame| screen::draw(frame, self))?;
                    match model::submit(client, &self.draft) {
                        Ok(()) => {
                            self.draft = Draft::default();
                            self.remember(client);
                            screen::notice(terminal, "Thanks, it's sent", "Your feedback went to the Plannotator maintainers. The draft has been cleared.")?;
                            return Ok(true);
                        }
                        Err(error) => {
                            self.status = model::failure(&error);
                            screen::notice(terminal, "Feedback not confirmed", &self.status)?;
                        }
                    }
                }
            }
            7 => return Ok(true),
            _ => {}
        }
        self.remember(client);
        Ok(false)
    }
}

pub(crate) fn run(client: &Client, terminal: &mut DefaultTerminal) -> Result<()> {
    let mut pane = Pane::load(client)?;
    let result = run_loop(client, terminal, &mut pane);
    pane.remember(client);
    result
}

fn run_loop(client: &Client, terminal: &mut DefaultTerminal, pane: &mut Pane) -> Result<()> {
    loop {
        terminal.draw(|frame| screen::draw(frame, pane))?;
        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Release {
                continue;
            }
            let action = match key.code {
                KeyCode::Esc | KeyCode::Char('q') => return Ok(()),
                KeyCode::Up | KeyCode::Char('k') => {
                    pane.list.select(Some(pane.list.selected().unwrap_or(0).saturating_sub(1)));
                    None
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    pane.list.select(Some((pane.list.selected().unwrap_or(0) + 1).min(7)));
                    None
                }
                KeyCode::PageUp => {
                    pane.scroll = pane.scroll.saturating_sub(10);
                    None
                }
                KeyCode::PageDown => {
                    pane.scroll = pane.scroll.saturating_add(10);
                    None
                }
                KeyCode::Home => {
                    pane.scroll = 0;
                    None
                }
                KeyCode::Enter => pane.list.selected(),
                KeyCode::Char('e') => Some(0),
                KeyCode::Char('c') => Some(1),
                KeyCode::Char('@') => Some(2),
                KeyCode::Char('h') => Some(3),
                KeyCode::Char('?') => Some(4),
                KeyCode::Char('p') => Some(5),
                KeyCode::Char('s') => Some(6),
                _ => None,
            };
            if key.kind == KeyEventKind::Press
                && let Some(action) = action
            {
                match pane.activate(action, client, terminal) {
                    Ok(true) => return Ok(()),
                    Ok(false) => {}
                    Err(error) => pane.status = format!("{error:#}"),
                }
            }
        }
    }
}
