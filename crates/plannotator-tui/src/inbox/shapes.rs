//! Shapes and System: a read-only opening, explicit writes, and honest graphic handoff.
//!
//! `run(client, terminal, thread_id)` uses the caller's existing terminal. Embedders may
//! use `Shapes::{default,refresh,draw,handle_key,paste,poll}`; `handle_key` returns true
//! only when the pane closes. `refresh` and drawing never mark boards seen. Set a
//! thread filter with `for_thread`. Refresh on Inbox `shapes`/`capabilities` events.
//!
//! Tab switches Shapes/System; j/k select, PgUp/PgDn scroll, 1–7 select detail pages,
//! Enter opens, o opens the genuine upstream graphical viewer, R refreshes, Esc backs
//! out. f toggles this thread/all boards, A includes finished boards. e requests a
//! change, a asks, c comments on the selected part, v marks the version read, x changes
//! active/finished. Mutations require a second explicit y confirmation. System e edits
//! its real setup JSON; upstream validates repos/docs/agent/model. No drawing files
//! are edited directly. References use server-scoped file GETs or a validated browser URL.
//!
//! Terminal limitations: no HTML/SVG rendering, pan/zoom, scenes/animation, box capture,
//! crop selection, or graphical editing. Those use the existing upstream browser
//! viewer, not a fabricated terminal canvas. Parts, example steps, refs, history,
//! notes, System sheets/parts/flows/revisions and PR touches remain browsable as text.
//! Upstream exposes no human drawing-write route: Change it is a real note to its
//! drawing agent, not a local graphic edit. System setup saves preferences only:
//! latest upstream `packages/inbox/system/firstDraw.ts` has no implemented first-draw
//! launcher, so this pane never labels setup Save as Draw or invents that endpoint.

mod api;
mod draw;
mod forms;
mod input;
mod model;
mod rows;
#[cfg(test)]
mod tests;

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyEventKind};

use super::client::Client;
use crate::app::compose::Compose;
use model::{Drawing, Loaded, Pulls, Stamp, Summary, SystemProject};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Tab {
    #[default]
    Shapes,
    System,
}

enum Opened {
    Shape { loaded: Box<Loaded>, history: Vec<Stamp> },
    System { drawing: Drawing, pulls: Pulls },
}

#[derive(Default)]
struct Row {
    label: String,
    detail: String,
    target: Target,
}

#[derive(Default)]
enum Target {
    #[default]
    None,
    Shape(String),
    System(String),
    Part {
        view: String,
        id: String,
        quote: String,
    },
    Version(u64),
    Reference(model::Reference),
    Sheet(String),
    Pull(String),
}

enum FormKind {
    Feedback { shape: String, input: serde_json::Value },
    Setup { project: String },
    Write { path: String, body: serde_json::Value },
}

struct Form {
    title: String,
    editor: Compose,
    kind: FormKind,
    confirm: bool,
    uncertain: bool,
}

#[derive(Default)]
pub(crate) struct Shapes {
    tab: Tab,
    boards: Vec<Summary>,
    systems: Vec<SystemProject>,
    thread: Option<String>,
    filter_thread: bool,
    finished: bool,
    opened: Option<Opened>,
    page: usize,
    rows: Vec<Row>,
    selected: usize,
    list_scroll: usize,
    detail_scroll: u16,
    file: Option<(String, String)>,
    form: Option<Form>,
    status: Option<String>,
    help: bool,
    worker: Option<Receiver<Result<String>>>,
}

impl Shapes {
    pub(crate) fn for_thread(thread_id: Option<&str>) -> Self {
        Self { thread: thread_id.map(str::to_owned), filter_thread: thread_id.is_some(), ..Self::default() }
    }

    pub(crate) fn refresh(&mut self, client: &Client) -> Result<()> {
        match &self.opened {
            Some(Opened::Shape { loaded, .. }) => {
                let id = loaded.shape.id.clone();
                let version = (loaded.version.n != loaded.shape.latest).then_some(loaded.version.n);
                self.open_shape(client, &id, version)?;
            }
            Some(Opened::System { drawing, .. }) => {
                let id = drawing.project.id.clone();
                self.open_system(client, &id)?;
            }
            None => {
                match self.tab {
                    Tab::Shapes => self.boards = api::list(client, self.finished)?,
                    Tab::System => self.systems = api::projects(client)?,
                }
                self.rebuild();
            }
        }
        Ok(())
    }

    fn open_shape(&mut self, client: &Client, id: &str, n: Option<u64>) -> Result<()> {
        let loaded = api::load(client, id, n)?;
        let history = api::history(client, id)?;
        self.opened = Some(Opened::Shape { loaded: Box::new(loaded), history });
        self.rebuild();
        Ok(())
    }

    fn open_system(&mut self, client: &Client, id: &str) -> Result<()> {
        let drawing = api::drawing(client, id)?;
        let pulls = api::pulls(client, id)?;
        self.opened = Some(Opened::System { drawing, pulls });
        self.rebuild();
        Ok(())
    }

    pub(crate) fn paste(&mut self, text: &str) {
        if let Some(form) = &mut self.form
            && !form.confirm
        {
            form.editor.insert_text(text);
        }
    }

    /// Read a completed live Ask. No network work or writes happen from drawing.
    pub(crate) fn poll(&mut self, client: &Client) {
        let answer = self.worker.as_ref().and_then(|worker| match worker.try_recv() {
            Ok(answer) => Some(answer),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Some(Err(anyhow::anyhow!("Shape agent worker stopped; the saved note still waits")))
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
        });
        if let Some(answer) = answer {
            self.worker = None;
            self.status = Some(match answer {
                Ok(message) => message,
                Err(error) => format!(
                    "Note saved, but live delivery failed: {error:#}. The agent can still read its notes."
                ),
            });
            if let Err(error) = self.refresh(client) {
                self.status = Some(format!("Refreshing saved feedback: {error:#}"));
            }
        }
    }
}

pub(crate) fn run(
    client: &Client,
    terminal: &mut ratatui::DefaultTerminal,
    thread_id: Option<&str>,
) -> Result<()> {
    let mut pane = Shapes::for_thread(thread_id);
    if let Err(error) = pane.refresh(client) {
        pane.status = Some(format!(
            "Shapes unavailable: {error:#}. Tab opens System; enable capabilities in Settings if off."
        ));
    }
    drive(client, terminal, pane)
}

/// Open a concrete board returned by Ask/Show me, never discard its identity.
pub(crate) fn run_at(
    client: &Client,
    terminal: &mut ratatui::DefaultTerminal,
    thread_id: Option<&str>,
    shape_id: &str,
) -> Result<()> {
    let mut pane = Shapes::for_thread(thread_id);
    pane.open_shape(client, shape_id, None)?;
    drive(client, terminal, pane)
}

fn drive(client: &Client, terminal: &mut ratatui::DefaultTerminal, mut pane: Shapes) -> Result<()> {
    let mut refresh_at = Instant::now() + Duration::from_secs(5);
    loop {
        pane.poll(client);
        terminal.draw(|frame| {
            let area = frame.area();
            pane.draw(frame, area);
        })?;
        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => match pane.handle_key(client, key) {
                    Ok(true) => return Ok(()),
                    Ok(false) => {}
                    Err(error) => pane.status = Some(format!("{error:#}")),
                },
                Event::Paste(text) => pane.paste(&text),
                _ => {}
            }
        }
        if Instant::now() >= refresh_at {
            // Keep live notes/drawings current without ever marking a version read.
            if pane.form.is_none()
                && let Err(error) = pane.refresh(client)
            {
                pane.status = Some(format!("Read failed: {error:#}; R retries"));
            }
            refresh_at = Instant::now() + Duration::from_secs(5);
        }
    }
}
