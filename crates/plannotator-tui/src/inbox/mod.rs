//! `plannotator-tui inbox`: the Plannotator Inbox in the terminal. The list of threads
//! grouped as the window groups them, each thread with its messages and answer cards,
//! picks and replies sent as the window sends them, and the server's event stream keeping
//! both current.
//!
//! `--snapshot` draws one frame headlessly and prints it. It reads the list and, when a
//! thread id is given, that thread, through `GET` only: it never opens the event stream,
//! never saves a pick, never replies, never marks anything seen. (The server's own thread
//! read may capture images of the thread's pages in the background; that is the server's
//! doing on every read, the window's included, and writes nothing to the thread.)

mod card;
mod client;
mod draw;
mod format;
mod questions;
mod send;
mod thread;
mod ui;

use std::io::{IsTerminal as _, stdout};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;

use self::client::{Client, HttpError, Live, Pick};
use self::ui::{Inbox, Request};

pub(crate) const USAGE: &str = "usage:
  plannotator-tui inbox [--url <http://127.0.0.1:PORT>]
  plannotator-tui inbox --snapshot [cols rows] [thread-id] [--url <url>]";

/// How long a failed first read waits before the list is asked for again.
const RETRY: Duration = Duration::from_secs(5);

#[derive(Debug, Default)]
struct Args {
    url: Option<String>,
    snapshot: Option<Snapshot>,
}

#[derive(Debug)]
struct Snapshot {
    cols: u16,
    rows: u16,
    thread: Option<String>,
}

/// The command line read: the run asked for, or `--help`.
#[derive(Debug)]
enum Parsed {
    Help,
    Args(Args),
}

fn parse(args: &[String]) -> Result<Parsed> {
    let mut parsed = Args::default();
    let mut sizes: Vec<u16> = Vec::new();
    let mut thread = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--url" => parsed.url = Some(rest.next().context("--url takes the Inbox's address")?.clone()),
            "--snapshot" => parsed.snapshot = Some(Snapshot { cols: 140, rows: 40, thread: None }),
            "--help" | "-h" => return Ok(Parsed::Help),
            value if value.starts_with("--url=") => {
                parsed.url = Some(value.trim_start_matches("--url=").to_owned());
            }
            flag if flag.starts_with("--") => anyhow::bail!("unknown flag {flag}\n{USAGE}"),
            value => match value.parse::<u16>() {
                Ok(size) if parsed.snapshot.is_some() && sizes.len() < 2 => sizes.push(size),
                _ if parsed.snapshot.is_some() && thread.is_none() => thread = Some(value.to_owned()),
                _ => anyhow::bail!("unexpected argument {value}\n{USAGE}"),
            },
        }
    }
    if let Some(snapshot) = parsed.snapshot.as_mut() {
        if let Some(cols) = sizes.first() {
            snapshot.cols = (*cols).max(20);
        }
        if let Some(rows) = sizes.get(1) {
            snapshot.rows = (*rows).max(5);
        }
        snapshot.thread = thread;
    }
    Ok(Parsed::Args(parsed))
}

/// `plannotator-tui inbox …`: the arguments after `inbox`.
pub(crate) fn run(args: &[String]) -> Result<()> {
    let args = match parse(args)? {
        Parsed::Help => {
            #[allow(clippy::print_stdout, reason = "help is the output")]
            {
                println!("{USAGE}");
            }
            return Ok(());
        }
        Parsed::Args(args) => args,
    };
    let client = match args.url {
        Some(url) => Client::from_url(&url)?,
        None => Client::discover()?.context(
            "the Plannotator Inbox is not running: start it with `plannotator inbox --background`, or pass --url",
        )?,
    };
    match args.snapshot {
        Some(snapshot) => draw_snapshot(&client, &snapshot),
        None => interactive(&client),
    }
}

/// One frame into an in-memory backend, printed as text. Reads only.
#[allow(clippy::print_stdout, reason = "the snapshot is the output")]
fn draw_snapshot(client: &Client, snapshot: &Snapshot) -> Result<()> {
    use ratatui::backend::TestBackend;
    let mut inbox = Inbox::new(client.url().to_owned());
    inbox.live = ui::LiveState::Snapshot;
    inbox.loaded_list(client.list().context("reading the Inbox's list")?);
    if let Some(id) = &snapshot.thread {
        inbox.opened_thread(client.thread(id).with_context(|| format!("reading thread {id}"))?);
    }
    let mut terminal = ratatui::Terminal::new(TestBackend::new(snapshot.cols, snapshot.rows))?;
    // The first draw records the body's size; the second lays the thread out for it.
    terminal.draw(|frame| inbox.draw(frame))?;
    terminal.draw(|frame| inbox.draw(frame))?;
    let buffer = terminal.backend().buffer();
    for y in 0..buffer.area.height {
        let mut line = String::new();
        for x in 0..buffer.area.width {
            if let Some(cell) = buffer.cell((x, y)) {
                line.push_str(cell.symbol());
            }
        }
        println!("{}", line.trim_end());
    }
    Ok(())
}

/// The terminal while the Inbox owns it: the alternate screen and raw mode (`ratatui::init`,
/// which also restores on a panic), bracketed paste, and the kitty keyboard flags where
/// the terminal has them. Given back in reverse order when dropped, on every path out.
struct Screen {
    terminal: ratatui::DefaultTerminal,
    shift_enter: bool,
}

impl Screen {
    fn claim() -> Self {
        let terminal = ratatui::init();
        #[cfg(unix)]
        crate::hangup::watch();
        let shift_enter = ratatui::crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
        let _ = execute!(stdout(), EnableBracketedPaste);
        if shift_enter {
            let _ = execute!(
                stdout(),
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            );
        }
        Self { terminal, shift_enter }
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        if self.shift_enter {
            let _ = execute!(stdout(), PopKeyboardEnhancementFlags);
        }
        let _ = execute!(stdout(), DisableBracketedPaste);
        ratatui::restore();
    }
}

fn interactive(client: &Client) -> Result<()> {
    anyhow::ensure!(stdout().is_terminal(), "the Inbox needs a terminal; use --snapshot to print one frame");
    let config = crate::config::Config::load().unwrap_or_default();
    crate::theme::install(
        crate::theme::resolve(|key| std::env::var(key).ok(), config.ui.theme, crate::theme::detect)?,
        &config.colors,
    );
    let mut screen = Screen::claim();
    let mut inbox = Inbox::new(client.url().to_owned());
    inbox.shift_enter = screen.shift_enter;
    event_loop(&mut screen.terminal, client, &mut inbox)
}

fn event_loop(terminal: &mut ratatui::DefaultTerminal, client: &Client, inbox: &mut Inbox) -> Result<()> {
    perform(inbox, client, Request::List);
    let mut live: Option<Receiver<Live>> = None;
    // The stream is opened once the list has been read, and opened again, after a pause,
    // should its thread ever end; the list itself is asked for again until it is read.
    let mut retry_at = Instant::now();
    let mut dirty = true;
    while !inbox.quit {
        if dirty {
            terminal.draw(|frame| inbox.draw(frame))?;
            dirty = false;
        }
        if event::poll(Duration::from_millis(250))? {
            let mut requests = handle(inbox, &event::read()?);
            // Coalesce a burst (a paste, a held key) into one redraw.
            while event::poll(Duration::ZERO)? {
                requests.extend(handle(inbox, &event::read()?));
            }
            for request in dedup(requests) {
                perform(inbox, client, request);
            }
            dirty = true;
        }
        if let Some(receiver) = &live {
            let mut requests = Vec::new();
            loop {
                match receiver.try_recv() {
                    Ok(event) => {
                        requests.extend(inbox.live(event));
                        dirty = true;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        requests.extend(inbox.live(Live::Lost("the event stream ended".to_owned())));
                        live = None;
                        retry_at = Instant::now() + RETRY;
                        dirty = true;
                        break;
                    }
                }
            }
            // One read per tick however many events arrived: the burst is coalesced.
            for request in dedup(requests) {
                perform(inbox, client, request);
            }
        }
        if Instant::now() >= retry_at {
            match inbox.list.as_ref().map(|l| l.cursor) {
                Some(cursor) if live.is_none() => live = Some(client.subscribe(cursor)),
                Some(_) => {}
                None => {
                    perform(inbox, client, Request::List);
                    dirty = true;
                }
            }
            retry_at = Instant::now() + RETRY;
        }
    }
    Ok(())
}

fn handle(inbox: &mut Inbox, event: &Event) -> Vec<Request> {
    match event {
        Event::Key(key) if key.kind != KeyEventKind::Release => inbox.handle_key(*key),
        Event::Paste(text) => {
            inbox.paste(text);
            Vec::new()
        }
        _ => Vec::new(),
    }
}

/// The same read asked for twice in one burst is read once; writes always go.
fn dedup(requests: Vec<Request>) -> Vec<Request> {
    let mut out: Vec<Request> = Vec::new();
    for request in requests {
        let seen = match &request {
            Request::List => out.iter().any(|o| matches!(o, Request::List)),
            Request::Thread(id) => out.iter().any(|o| matches!(o, Request::Thread(other) if other == id)),
            Request::Pick { .. } | Request::Send(_) => false,
        };
        if !seen {
            out.push(request);
        }
    }
    out
}

/// Do what the state asked of the server, and hand it the answer. Every failure lands in
/// the status line; a refused or unanswered write also reads the thread again, so the
/// cards show what the server holds next to what was not saved. A reply the server refused
/// (4xx) was not written; one it never answered for may have been, and stays to be sent
/// again as it was.
fn perform(inbox: &mut Inbox, client: &Client, request: Request) {
    match request {
        Request::List => match client.list() {
            Ok(list) => inbox.loaded_list(list),
            Err(error) => inbox.failed(&format!("{error:#}")),
        },
        Request::Thread(id) => match client.thread(&id) {
            Ok(model) => inbox.opened_thread(model),
            Err(error) => inbox.failed(&format!("{error:#}")),
        },
        Request::Pick { message_id, key, revision, answer } => {
            let pick = Pick { message_id: None, key, revision, answer: Some(answer) };
            match client.pick(&message_id, &send::new_key(), &[pick]) {
                Ok(questions) => inbox.picked(&message_id, questions),
                Err(error) => {
                    inbox.failed(&format!("pick not confirmed: {error:#}"));
                    reread_thread(inbox, client);
                }
            }
        }
        Request::Send(targets) => {
            for (index, target) in targets.iter().enumerate() {
                let sent = client.reply(
                    &target.message_id,
                    &target.idempotency_key,
                    target.words.as_deref(),
                    &target.questions,
                    &[],
                );
                if let Err(error) = sent {
                    let refused =
                        error.downcast_ref::<HttpError>().is_some_and(|e| (400..500).contains(&e.status));
                    let remaining = targets.get(index..).unwrap_or_default().to_vec();
                    inbox.send_failed(index, remaining, &format!("{error:#}"), refused);
                    reread_thread(inbox, client);
                    perform(inbox, client, Request::List);
                    return;
                }
            }
            inbox.sent(targets.len());
            reread_thread(inbox, client);
            perform(inbox, client, Request::List);
        }
    }
}

/// Read the open thread again, keeping the status line's words about what just happened.
fn reread_thread(inbox: &mut Inbox, client: &Client) {
    let Some(id) = inbox.thread.as_ref().map(|t| t.id().to_owned()) else { return };
    let status = inbox.status.take();
    perform(inbox, client, Request::Thread(id));
    if status.is_some() {
        inbox.status = status;
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, reason = "tests assert by panicking")]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    fn parsed(list: &[&str]) -> Args {
        match parse(&args(list)).expect("parses") {
            Parsed::Args(args) => args,
            Parsed::Help => panic!("help"),
        }
    }

    #[test]
    fn snapshot_takes_a_size_then_a_thread_id() {
        let parsed = parsed(&["--snapshot", "100", "30", "msg_1", "--url", "http://127.0.0.1:1"]);
        let snapshot = parsed.snapshot.expect("snapshot");
        assert_eq!((snapshot.cols, snapshot.rows), (100, 30));
        assert_eq!(snapshot.thread.as_deref(), Some("msg_1"));
        assert_eq!(parsed.url.as_deref(), Some("http://127.0.0.1:1"));
    }

    #[test]
    fn a_thread_id_outside_a_snapshot_is_refused() {
        assert!(parse(&args(&["msg_1"])).is_err());
        assert!(parse(&args(&["--bogus"])).is_err());
    }
}
