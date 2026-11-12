//! The Inbox's artifacts wall, backed by GET /api/inbox/artifacts and the shared artifact pins.
//! Filter and cursor paging stay on the server; selecting a row opens its real file or guide pane.

use std::time::Duration;

use anyhow::{Context, Result, ensure};
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize as _};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use serde::Deserialize;
use serde_json::json;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::client::Client;
use crate::theme::palette;

const PAGE_SIZE: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TypeFilter {
    All,
    Images,
    Pages,
    Documents,
    Other,
}

impl TypeFilter {
    fn wire(self) -> Option<&'static str> {
        match self {
            Self::All => None,
            Self::Images => Some("images"),
            Self::Pages => Some("pages"),
            Self::Documents => Some("documents"),
            Self::Other => Some("other"),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::All => "All types",
            Self::Images => "Images",
            Self::Pages => "Pages",
            Self::Documents => "Documents",
            Self::Other => "Other files",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::All => Self::Images,
            Self::Images => Self::Pages,
            Self::Pages => Self::Documents,
            Self::Documents => Self::Other,
            Self::Other => Self::All,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct ArtifactPage {
    #[serde(rename = "serverSession")]
    server_session: String,
    #[serde(default)]
    artifacts: Vec<ArtifactRow>,
    next_cursor: Option<String>,
    #[serde(default)]
    counts: Counts,
    #[serde(default)]
    total: usize,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct Counts {
    pinned: usize,
}
#[derive(Debug, Clone, Deserialize)]
struct ArtifactRow {
    key: String,
    thread_id: String,
    thread_title: String,
    message_id: String,
    at: String,
    #[serde(rename = "type")]
    artifact_type: String,
    #[serde(default)]
    author: Author,
    #[serde(default)]
    attachment: Option<ArtifactAttachment>,
    #[serde(default)]
    guide: Option<Guide>,
    #[serde(default)]
    archived: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct Author {
    kind: String,
    name: Option<String>,
    host: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ArtifactAttachment {
    id: String,
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Guide {
    title: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Pins {
    #[serde(default)]
    pins: Vec<ArtifactPin>,
}

#[derive(Debug, Clone, Deserialize)]
struct ArtifactPin {
    key: String,
    thread_id: String,
}

#[derive(Debug)]
struct Wall {
    pinned_only: bool,
    include_archived: bool,
    kind: TypeFilter,
    rows: Vec<ArtifactRow>,
    next_cursor: Option<String>,
    counts: Counts,
    total: usize,
    pins: Vec<ArtifactPin>,
    cursor: usize,
    scroll: usize,
    viewport_height: usize,
    status: Option<String>,
}

impl Wall {
    fn new() -> Self {
        Self {
            pinned_only: false,
            include_archived: false,
            kind: TypeFilter::All,
            rows: Vec::new(),
            next_cursor: None,
            counts: Counts::default(),
            pins: Vec::new(),
            total: 0,
            cursor: 0,
            scroll: 0,
            viewport_height: 20,
            status: None,
        }
    }

    fn load_page(&self, client: &Client, cursor: Option<&str>) -> Result<ArtifactPage> {
        let mut query = format!(
            "archived={}&limit={PAGE_SIZE}",
            if self.include_archived { "include" } else { "exclude" }
        );
        if self.pinned_only {
            query.push_str("&pinned=1");
        }
        if let Some(kind) = self.kind.wire() {
            query.push_str("&type=");
            query.push_str(kind);
        }
        if let Some(cursor) = cursor {
            ensure!(
                !cursor.is_empty() && cursor.bytes().all(|byte| byte.is_ascii_digit()),
                "Inbox artifact cursor was not numeric"
            );
            query.push_str("&cursor=");
            query.push_str(cursor);
        }
        let page: ArtifactPage = client.get_json("GET", &format!("/api/inbox/artifacts?{query}"))?;
        ensure!(
            page.server_session == client.server_session(),
            "Inbox server session changed; reopen Artifacts"
        );
        Ok(page)
    }

    fn replace_page(&mut self, page: ArtifactPage) {
        self.rows = page.artifacts;
        self.next_cursor = page.next_cursor;
        self.counts = page.counts;
        self.total = page.total;
        self.cursor = 0;
        self.scroll = 0;
    }

    fn refresh(&mut self, client: &Client) -> Result<()> {
        let page = self.load_page(client, None)?;
        self.replace_page(page);
        self.status = None;
        Ok(())
    }

    fn refresh_pins(&mut self, client: &Client) -> Result<()> {
        let pins: Pins = client.get_json("GET", "/api/inbox/artifact-pins")?;
        self.pins = pins.pins;
        Ok(())
    }

    fn load_more(&mut self, client: &Client) -> Result<bool> {
        let Some(cursor) = self.next_cursor.clone() else { return Ok(false) };
        let page = self.load_page(client, Some(&cursor))?;
        let start = self.rows.len();
        self.rows.extend(page.artifacts);
        self.next_cursor = page.next_cursor;
        self.counts = page.counts;
        self.total = page.total;
        if self.rows.len() == start {
            return Ok(false);
        }
        self.cursor = start;
        Ok(true)
    }

    fn filters_changed(&mut self, client: &Client) {
        self.rows.clear();
        self.next_cursor = None;
        self.cursor = 0;
        self.scroll = 0;
        match self.refresh(client) {
            Ok(()) => self.status = None,
            Err(error) => self.status = Some(format!("Couldn't read artifacts: {error:#}")),
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        self.cursor = self.cursor.saturating_add_signed(delta).min(self.rows.len().saturating_sub(1));
    }

    fn selected(&self) -> Option<&ArtifactRow> {
        self.rows.get(self.cursor)
    }
    fn is_pinned(&self, thread_id: &str, key: &str) -> bool {
        self.pins.iter().any(|pin| pin.thread_id == thread_id && pin.key == key)
    }

    fn toggle_pin(&mut self, client: &Client) -> Result<()> {
        let Some(row) = self.selected() else {
            self.status = Some("Select an artifact first".to_owned());
            return Ok(());
        };
        let thread_id = row.thread_id.clone();
        let key = row.key.clone();
        let pin = self.is_pinned(&thread_id, &key);
        let pins: Pins = client.post_json(
            "/api/inbox/artifact-pins",
            &json!({
                "serverSession": client.server_session(),
                "thread_id": thread_id,
                "key": key,
                "pinned": !pin,
            }),
        )?;
        self.pins = pins.pins;
        let page = self.load_page(client, None)?;
        let key = self.rows.get(self.cursor).map(|selected| selected.key.clone());
        self.replace_page(page);
        if let Some(key) = key {
            self.cursor = self
                .rows
                .iter()
                .position(|row| row.key == key)
                .unwrap_or(self.cursor)
                .min(self.rows.len().saturating_sub(1));
        }
        self.status = Some(if pin { "Artifact unpinned" } else { "Artifact pinned" }.to_owned());
        Ok(())
    }

    fn handle_key(&mut self, client: &Client, key: KeyEvent) -> WallAction {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return WallAction::Close,
            KeyCode::Up | KeyCode::Char('k') => self.move_cursor(-1),
            KeyCode::Down | KeyCode::Char('j') => {
                if self.cursor + 1 >= self.rows.len() && self.next_cursor.is_some() {
                    match self.load_more(client) {
                        Ok(true) => self.status = None,
                        Ok(false) => self.status = Some("No more artifacts".to_owned()),
                        Err(error) => self.status = Some(format!("Couldn't load more artifacts: {error:#}")),
                    }
                } else {
                    self.move_cursor(1);
                }
            }
            KeyCode::PageUp => self.move_cursor(-(self.viewport_height.max(1) as isize)),
            KeyCode::PageDown => {
                let target = self.cursor.saturating_add(self.viewport_height.max(1));
                if target >= self.rows.len() && self.next_cursor.is_some() {
                    match self.load_more(client) {
                        Ok(true) => self.status = None,
                        Ok(false) => self.status = Some("No more artifacts".to_owned()),
                        Err(error) => self.status = Some(format!("Couldn't load more artifacts: {error:#}")),
                    }
                } else {
                    self.move_cursor(self.viewport_height.max(1) as isize);
                }
            }
            KeyCode::Home | KeyCode::Char('g') => self.cursor = 0,
            KeyCode::End | KeyCode::Char('G') => self.cursor = self.rows.len().saturating_sub(1),
            KeyCode::Char('p') => {
                self.pinned_only = !self.pinned_only;
                self.filters_changed(client);
            }
            KeyCode::Char('t') => {
                self.kind = self.kind.next();
                self.filters_changed(client);
            }
            KeyCode::Char('x') => {
                self.include_archived = !self.include_archived;
                self.filters_changed(client);
            }
            KeyCode::Char('s') => {
                if let Err(error) = self.toggle_pin(client) {
                    self.status = Some(format!("Couldn't change artifact pin: {error:#}"));
                }
            }
            KeyCode::Char('r') => {
                self.filters_changed(client);
                if let Err(error) = self.refresh_pins(client) {
                    self.status = Some(format!("Artifacts loaded; pins could not be read: {error:#}"));
                }
            }
            KeyCode::Char('m') => match self.load_more(client) {
                Ok(true) => self.status = None,
                Ok(false) => self.status = Some("No more artifacts".to_owned()),
                Err(error) => self.status = Some(format!("Couldn't load more artifacts: {error:#}")),
            },
            KeyCode::Enter | KeyCode::Right if self.selected().is_some() => {
                return WallAction::Open;
            }
            _ => {}
        }
        WallAction::Stay
    }

    fn ensure_visible(&mut self, height: usize) {
        let height = height.max(1);
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + height {
            self.scroll = self.cursor + 1 - height;
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let [header, body, footer] =
            Layout::vertical([Constraint::Length(2), Constraint::Min(1), Constraint::Length(1)]).areas(area);
        self.viewport_height = usize::from(body.height).max(1);
        self.draw_header(frame, header);
        self.draw_rows(frame, body);
        self.draw_footer(frame, footer);
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let heading = format!(
            " Artifacts · {} · {}",
            if self.pinned_only { "Pinned" } else { "All" },
            self.kind.label()
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(heading, Style::new().fg(palette().accent).bold()))),
            Rect { height: 1, ..area },
        );
        let selected_count = if self.pinned_only { self.counts.pinned } else { self.total };
        let pinned = self.counts.pinned;
        let archived = if self.include_archived { " · archived included" } else { " · archived hidden" };
        let summary =
            format!(" {} matches · {pinned} pinned · showing {}", selected_count, self.rows.len()) + archived;
        frame.render_widget(
            Paragraph::new(Line::from(Span::raw(summary).dim())),
            Rect { y: area.y.saturating_add(1), height: 1, ..area },
        );
    }

    fn draw_rows(&mut self, frame: &mut Frame, area: Rect) {
        let height = usize::from(area.height);
        self.ensure_visible(height);
        if self.rows.is_empty() {
            let words = if self.pinned_only {
                "No pinned artifacts match these filters."
            } else {
                "No artifacts match these filters."
            };
            frame.render_widget(
                Paragraph::new(Line::from(Span::raw(words).dim())).wrap(Wrap { trim: true }),
                area,
            );
            return;
        }
        for (offset, row) in self.rows.iter().skip(self.scroll).take(height).enumerate() {
            let absolute = self.scroll + offset;
            let selected = absolute == self.cursor;
            let name = row
                .attachment
                .as_ref()
                .map(|attachment| attachment.name.as_str())
                .or_else(|| row.guide.as_ref().map(|guide| guide.title.as_str()))
                .unwrap_or("Guided review");
            let who = if row.author.kind == "person" {
                "you"
            } else {
                row.author.name.as_deref().or(row.author.host.as_deref()).unwrap_or("agent")
            };
            let flags = format!(
                "{}{}",
                if self.is_pinned(&row.thread_id, &row.key) { " · pinned" } else { "" },
                if row.archived { " · archived" } else { "" }
            );
            let line = Line::from(vec![
                Span::styled(format!(" {:>3} ", absolute + 1), Style::new().dim()),
                Span::styled(name.to_owned(), Style::new().fg(palette().accent).add_modifier(Modifier::BOLD)),
                Span::raw(format!(
                    " · {} · {} · {} · {}{}",
                    row.artifact_type,
                    row.thread_title,
                    who,
                    ago(&row.at),
                    flags
                )),
            ]);
            let rect = Rect { y: area.y + offset as u16, height: 1, ..area };
            if selected {
                frame.render_widget(Paragraph::new("").style(palette().selection), rect);
                frame.render_widget(Paragraph::new(line.patch_style(palette().selection)), rect);
            } else {
                frame.render_widget(Paragraph::new(line), rect);
            }
        }
    }

    fn draw_footer(&self, frame: &mut Frame, area: Rect) {
        let text = self.status.as_deref().unwrap_or("↑↓ move · enter open · s pin · p pinned filter · t type · x archived · m more · r refresh · q back");
        let style = if self.status.is_some() {
            Style::new().fg(palette().comment)
        } else {
            Style::new().fg(palette().idle_fg)
        };
        frame.render_widget(Paragraph::new(Line::from(Span::raw(text).style(style))), area);
    }
}

#[derive(Debug)]
enum WallAction {
    Stay,
    Close,
    Open,
}

/// Run inside the existing Inbox terminal. Initial list and pins reads are GET-only.
pub(crate) fn run(
    client: &Client,
    terminal: &mut ratatui::DefaultTerminal,
    _thread_id: Option<&str>,
) -> Result<()> {
    let mut wall = Wall::new();
    if let Err(error) = wall.refresh(client) {
        wall.status = Some(format!("Couldn't read artifacts: {error:#}"));
    }
    if let Err(error) = wall.refresh_pins(client) {
        wall.status = Some(format!("Artifacts loaded; pins could not be read: {error:#}"));
    }
    loop {
        terminal.draw(|frame| wall.draw(frame))?;
        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        let Event::Key(key) = event::read()? else { continue };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        match wall.handle_key(client, key) {
            WallAction::Stay => {}
            WallAction::Close => return Ok(()),
            WallAction::Open => {
                if let Some(row) = wall.selected()
                    && let Err(error) = open_row(client, terminal, row)
                {
                    wall.status = Some(format!("Couldn't open {}: {error:#}", artifact_name(row)));
                }
            }
        }
    }
}

fn open_row(client: &Client, terminal: &mut ratatui::DefaultTerminal, row: &ArtifactRow) -> Result<()> {
    if let Some(artifact) = &row.attachment {
        let model = client.attachments(&row.thread_id)?;
        let attachment = model.files().find(|candidate| candidate.id == artifact.id).with_context(|| {
            format!("attachment {} is no longer in thread {}", artifact.id, row.thread_id)
        })?;
        let version = attachment.readable_version();
        super::attachments::open_attachment(client, terminal, attachment, version)
    } else if row.guide.is_some() {
        super::reviews::run_guide(client, terminal, &row.thread_id, &row.message_id)
    } else {
        anyhow::bail!("artifact {} has neither an attachment nor a guided review", row.key)
    }
}

fn artifact_name(row: &ArtifactRow) -> &str {
    row.attachment
        .as_ref()
        .map(|attachment| attachment.name.as_str())
        .or_else(|| row.guide.as_ref().map(|guide| guide.title.as_str()))
        .unwrap_or("guided review")
}

fn ago(iso: &str) -> String {
    let Ok(at) = OffsetDateTime::parse(iso, &Rfc3339) else {
        return iso.to_owned();
    };
    let seconds = (OffsetDateTime::now_utc() - at).whole_seconds();
    if seconds < 60 {
        "now".to_owned()
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3600)
    } else if seconds < 7 * 86_400 {
        format!("{}d", seconds / 86_400)
    } else {
        let date = at.date();
        format!("{:04}-{:02}-{:02}", date.year(), u8::from(date.month()), date.day())
    }
}
