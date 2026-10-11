#![allow(clippy::all, clippy::pedantic)]

//! Plannotator Inbox TUI module.
//!
//! Subcommand `plannotator-tui inbox`:
//! - Left pane: thread list grouped by sections (new/waiting/sent/quiet…), live via SSE.
//! - Right pane: thread messages with Markdown rendering and interactive keyboard cards for questions.
//! - Attachments open in existing annotator; line comments route back as server replies.
//! - `r` free-text reply, `x` resolve, `?` help.

pub(crate) mod app;
pub(crate) mod client;
pub(crate) mod delivery;
pub(crate) mod discovery;
pub(crate) mod questions;
pub(crate) mod sse;
pub(crate) mod ui;

use std::time::Duration;

use anyhow::{Context, Result};
#[allow(unused_imports)]
pub(crate) use delivery::InboxDelivery;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{self, Event};

use self::app::InboxApp;
use self::discovery::discover_inbox;

/// Run the interactive Inbox TUI, or render a snapshot if specified.
pub(crate) fn run_inbox(url_override: Option<&str>, snapshot: Option<(u16, u16)>) -> Result<()> {
    let info = discover_inbox(url_override).context("discovering Plannotator Inbox server")?;

    if let Some((cols, rows)) = snapshot {
        let output = run_snapshot(&info, cols, rows)?;
        print!("{output}");
        return Ok(());
    }

    let mut app = InboxApp::new(info)?;
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app);
    ratatui::restore();
    result
}

/// Headless snapshot renderer for testing and proof captures.
pub(crate) fn run_snapshot(info: &discovery::InboxConnectionInfo, cols: u16, rows: u16) -> Result<String> {
    let app = InboxApp::new(info.clone())?;
    let mut terminal = ratatui::Terminal::new(TestBackend::new(cols, rows))?;
    terminal.draw(|frame| ui::draw(frame, &app))?;

    let buffer = terminal.backend().buffer();
    let mut lines = Vec::new();
    for y in 0..buffer.area.height {
        let mut line = String::new();
        for x in 0..buffer.area.width {
            if let Some(cell) = buffer.cell((x, y)) {
                line.push_str(cell.symbol());
            }
        }
        lines.push(line.trim_end().to_owned());
    }

    Ok(lines.join("\n") + "\n")
}

fn event_loop(terminal: &mut ratatui::DefaultTerminal, app: &mut InboxApp) -> Result<()> {
    let mut dirty = true;

    while !app.quit {
        app.pump_sse_events();

        if dirty {
            terminal.draw(|frame| ui::draw(frame, app))?;
            dirty = false;
        }

        if event::poll(Duration::from_millis(50))? {
            let mut handle_event = |ev: Event| -> Result<()> {
                match ev {
                    Event::Key(key) if key.kind != event::KeyEventKind::Release => {
                        app.handle_key(key)?;
                        dirty = true;
                    }
                    Event::Paste(text) => {
                        if app.reply_mode {
                            for c in text.chars() {
                                app.reply_buffer.insert(c);
                            }
                            dirty = true;
                        }
                    }
                    _ => {}
                }
                Ok(())
            };
            let first_ev = event::read()?;
            handle_event(first_ev)?;
            while event::poll(Duration::ZERO)? {
                handle_event(event::read()?)?;
            }
        }
    }

    Ok(())
}
