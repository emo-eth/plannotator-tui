mod fixture;
mod input;
mod media;
mod model;
mod settings;
#[cfg(test)]
mod tests;
mod view;

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::text::Line;

use super::client::Client;
use crate::app::compose::Compose;
use fixture::Fixture;
use media::Assets;
use model::{Replay, Stage, Target};
use settings::Appearance;

enum Editor {
    Quote { source: String, target: Target, suggestion: String },
    Comment { target: Target, quote: String },
    New,
    Other { warm: bool },
}

struct Pane {
    fixture: Fixture,
    replay: Replay,
    appearance: Appearance,
    assets: Assets,
    cursor: usize,
    scroll: u16,
    viewer: Option<String>,
    editor: Option<Editor>,
    compose: Compose,
    status: String,
    body: Vec<Line<'static>>,
    rows: Vec<crate::wrap::Row>,
    width: u16,
    exit: bool,
    started: bool,
    playback: Option<(Instant, f64)>,
}

pub(crate) fn run(client: &Client, terminal: &mut DefaultTerminal) -> Result<()> {
    let fixture = Fixture::load(client)?;
    let appearance = Appearance::load(client)?;
    let mut pane = Pane {
        fixture,
        appearance,
        replay: Replay::default(),
        assets: Assets::default(),
        cursor: 0,
        scroll: 0,
        viewer: None,
        editor: None,
        compose: Compose::default(),
        status: String::new(),
        body: Vec::new(),
        rows: Vec::new(),
        width: 0,
        exit: false,
        started: false,
        playback: None,
    };
    pane.refresh_body()?;
    loop {
        if pane.replay.tick() {
            pane.refresh_body()?;
        }
        if let Some((start, at)) = pane.playback {
            let duration = pane.fixture.asset("film-clip.mp4")?.duration_ms as f64 / 1000.0;
            pane.replay.moment = (at + start.elapsed().as_secs_f64()).min(duration);
            if pane.replay.moment >= duration {
                pane.playback = None;
            }
        }
        terminal.draw(|frame| pane.draw(frame))?;
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let result = match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => pane.key(key, client, terminal),
            Event::Paste(text) if pane.editor.is_some() => {
                pane.compose.insert_text(&text);
                Ok(false)
            }
            _ => Ok(false),
        };
        match result {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(error) => pane.status = format!("{error:#}"),
        }
    }
}

impl Pane {
    fn go(&mut self, stage: Stage) -> Result<()> {
        self.replay.go(stage);
        self.cursor = 0;
        self.scroll = 0;
        self.viewer = None;
        self.playback = None;
        self.status.clear();
        self.refresh_body()
    }

    fn open(&mut self, client: &Client, name: &str) -> Result<()> {
        if Path::new(name).extension().is_some_and(|extension| extension == "md" || extension == "html") {
            self.assets.text(client, name)?;
        } else {
            self.assets.pixels(client, name)?;
        }
        self.viewer = Some(name.to_owned());
        self.scroll = 0;
        self.refresh_body()
    }
}
