//! Agent icon sets and uploaded pictures, including the animation-request hand-off.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, ensure};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use serde::Deserialize;
use serde_json::{Value, json};

use super::agents::{Agent, AgentFacesWire, AgentGroup, AgentIcon, parse_agent_groups};
use super::client::{Client, HttpError};
use super::send::new_key;
use crate::app::compose::{Compose, ComposeAction};

const ICON_SETS: &str = "/api/inbox/icon-sets";
const AGENT_ICONS: &str = "/api/inbox/agent-icons";
const ICON_ANIMATE: &str = "/api/inbox/icon-animate";
const ICON_MAX_BYTES: usize = 512 * 1024;
const TICK: Duration = Duration::from_millis(150);

/// An uploaded picture or set icon selected for an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AgentFace {
    Own(String),
    Set { set: String, icon: String },
}

impl AgentFace {
    fn wire(&self) -> String {
        match self {
            Self::Own(id) => format!("own:{id}"),
            Self::Set { set, icon } => format!("set:{set}/{icon}"),
        }
    }
}

fn valid_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_key(value: &str, max: usize) -> bool {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else { return false };
    value.len() <= max
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct IconSet {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) count: usize,
    pub(crate) kind: String,
    pub(crate) pixelated: bool,
    pub(crate) size: Option<u32>,
    pub(crate) mono: bool,
    pub(crate) zoom: Option<f32>,
    pub(crate) credit: Option<String>,
    pub(crate) credits: bool,
    pub(crate) licence: Option<String>,
    pub(crate) get: Option<SetDownload>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct SetDownload {
    pub(crate) state: String,
    pub(crate) have: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct IconSetIcon {
    pub(crate) id: String,
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) words: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct IconSetListing {
    #[serde(flatten)]
    pub(crate) set: IconSet,
    #[serde(default)]
    pub(crate) icons: Vec<IconSetIcon>,
    #[serde(default)]
    pub(crate) have: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SetsAnswer {
    #[serde(default)]
    sets: Vec<IconSet>,
}
#[derive(Debug, Deserialize)]
struct SetAnswer {
    set: Option<IconSet>,
}
#[derive(Debug, Deserialize)]
struct IconsAnswer {
    icon: Option<AgentIcon>,
    #[serde(flatten)]
    faces: AgentFacesWire,
}
#[derive(Debug, Deserialize)]
struct AnimateRequestAnswer {
    request: Option<String>,
}
#[derive(Debug, Deserialize)]
struct AnimateResultsAnswer {
    #[serde(default)]
    results: Vec<AnimateResult>,
}
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct AnimateResult {
    pub(crate) icon: String,
    pub(crate) result: String,
}
#[derive(Debug, Deserialize)]
struct UploadFile {
    upload_id: String,
}
#[derive(Debug, Deserialize)]
struct SessionsAnswer {
    #[serde(default)]
    projects: Vec<SessionProject>,
}
#[derive(Debug, Deserialize)]
struct SessionProject {
    project: SessionProjectInfo,
    #[serde(default)]
    sessions: Vec<LiveSession>,
}
#[derive(Debug, Deserialize)]
struct SessionProjectInfo {
    name: String,
}
#[derive(Debug, Deserialize)]
struct LiveSession {
    session: String,
    host: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    busy: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Agents,
    Pictures,
    Sets,
    Originals,
}
impl Tab {
    fn title(self) -> &'static str {
        match self {
            Self::Agents => "Agents",
            Self::Pictures => "Your pictures",
            Self::Sets => "Icon sets",
            Self::Originals => "Animation originals",
        }
    }
    fn next(self) -> Self {
        match self {
            Self::Agents => Self::Pictures,
            Self::Pictures => Self::Sets,
            Self::Sets => Self::Originals,
            Self::Originals => Self::Agents,
        }
    }
}

#[derive(Debug, Clone)]
struct SelectedAgent {
    id: String,
    name: String,
    session: String,
    continues: Vec<String>,
}
#[derive(Debug, Clone)]
struct RowItem {
    label: String,
    detail: String,
    action: ItemAction,
}
#[derive(Debug, Clone)]
enum ItemAction {
    Agent(SelectedAgent),
    Picture(AgentIcon),
    Set(IconSet),
    SetIcon(IconSetIcon),
    Original(AnimateResult),
}
#[derive(Debug, Default)]
struct Pane {
    tab: Option<Tab>,
    cursor: usize,
    agent: Option<SelectedAgent>,
    groups: Vec<AgentGroup>,
    faces: Option<AgentFacesWire>,
    sets: Vec<IconSet>,
    listing: Option<IconSetListing>,
    originals: Vec<AnimateResult>,
    status: Option<String>,
    quit: bool,
}

impl Pane {
    fn refresh(&mut self, client: &Client) {
        self.status = None;
        match client.get_json::<Value>("GET", "/api/inbox/threads") {
            Ok(list) => {
                let raw = list.get("agents").and_then(Value::as_array).cloned().unwrap_or_default();
                match parse_agent_groups(&raw) {
                    Ok(groups) => self.groups = groups,
                    Err(error) => self.status = Some(format!("Agent list could not be read: {error:#}")),
                }
            }
            Err(error) => self.status = Some(format!("Agent list unavailable: {error:#}")),
        }
        match client.agent_faces() {
            Ok(wire) => self.faces = Some(wire),
            Err(error) => self.status = Some(compat_message("Agent faces", &error)),
        }
        match client.agent_icon_sets(true) {
            Ok(sets) => self.sets = sets,
            Err(error) => self.status = Some(compat_message("Icon sets", &error)),
        }
        match client.agent_icon_animation_results() {
            Ok(results) => self.originals = results,
            Err(error) if !is_missing_route(&error) => {
                self.status = Some(compat_message("Icon animation", &error));
            }
            Err(_) => {}
        }
        self.clamp_cursor();
    }

    fn items(&self) -> Vec<RowItem> {
        if let Some(listing) = &self.listing {
            return listing
                .icons
                .iter()
                .cloned()
                .map(|icon| {
                    let available =
                        listing.set.kind != "fetchable" || listing.have.iter().any(|id| id == &icon.id);
                    RowItem {
                        label: format!("{} · {}", icon.id, icon.name),
                        detail: if available {
                            icon.words.join(", ")
                        } else {
                            "Download this set first".to_owned()
                        },
                        action: ItemAction::SetIcon(icon),
                    }
                })
                .collect();
        }
        match self.tab.unwrap_or(Tab::Agents) {
            Tab::Agents => self
                .groups
                .iter()
                .flat_map(|group| group.agents.iter())
                .map(|agent| {
                    let name =
                        agent.label.as_deref().filter(|name| !name.trim().is_empty()).unwrap_or_else(|| {
                            agent.project.as_ref().map_or("Agent", |project| project.name.as_str())
                        });
                    let current = self.agent.as_ref().is_some_and(|selected| selected.id == agent.id);
                    RowItem {
                        label: format!("{name} · {}", agent.id),
                        detail: format!(
                            "{} · {} · {}{}",
                            host_name(agent.host.as_deref()),
                            state_label(agent),
                            agent.session,
                            if current { " · selected" } else { "" }
                        ),
                        action: ItemAction::Agent(SelectedAgent {
                            id: agent.id.clone(),
                            name: name.to_owned(),
                            session: agent.session.clone(),
                            continues: agent.continues.clone(),
                        }),
                    }
                })
                .collect(),
            Tab::Pictures => self
                .faces
                .as_ref()
                .and_then(|wire| wire.icons.as_ref())
                .into_iter()
                .flatten()
                .cloned()
                .map(|icon| {
                    let selected = self
                        .agent
                        .as_ref()
                        .and_then(|agent| face_for(self.faces.as_ref(), &agent.id))
                        .is_some_and(|face| face == format!("own:{}", icon.id));
                    RowItem {
                        label: format!("{} · {}", icon.id, icon.icon_type),
                        detail: format!(
                            "{} · added {}",
                            if selected { "On selected agent" } else { "Uploaded picture" },
                            icon.at
                        ),
                        action: ItemAction::Picture(icon),
                    }
                })
                .collect(),
            Tab::Sets => self
                .sets
                .iter()
                .cloned()
                .map(|set| {
                    let download = set
                        .get
                        .as_ref()
                        .map(|get| format!(" · {} ({}/{})", get.state, get.have, set.count))
                        .unwrap_or_default();
                    RowItem {
                        label: format!("{} · {}", set.name, set.id),
                        detail: format!(
                            "{} · {} icons · {}{}",
                            set.kind,
                            set.count,
                            if set.mono {
                                "monochrome"
                            } else if set.pixelated {
                                "pixel art"
                            } else {
                                "full colour"
                            },
                            download
                        ),
                        action: ItemAction::Set(set),
                    }
                })
                .collect(),
            Tab::Originals => self
                .originals
                .iter()
                .cloned()
                .map(|result| RowItem {
                    label: format!("Original {}…", result.icon.chars().take(12).collect::<String>()),
                    detail: format!(
                        "Animation result {}… · Enter restores this original on the selected agent",
                        result.result.chars().take(12).collect::<String>()
                    ),
                    action: ItemAction::Original(result),
                })
                .collect(),
        }
    }

    fn clamp_cursor(&mut self) {
        self.cursor = self.cursor.min(self.items().len().saturating_sub(1));
    }
    fn status(&mut self, text: impl Into<String>) {
        self.status = Some(text.into());
    }
}

impl Client {
    pub(crate) fn agent_icon_sets(&self, remote: bool) -> Result<Vec<IconSet>> {
        let path = if remote { format!("{ICON_SETS}?remote=1") } else { ICON_SETS.to_owned() };
        Ok(self.get_json::<SetsAnswer>("GET", &path)?.sets)
    }
    pub(crate) fn agent_icon_set(&self, id: &str) -> Result<IconSetListing> {
        ensure!(valid_key(id, 40), "invalid icon-set id {id:?}");
        self.get_json("GET", &format!("{ICON_SETS}/{id}"))
    }
    pub(crate) fn get_agent_icon_set(&self, id: &str) -> Result<IconSet> {
        ensure!(valid_key(id, 40), "invalid icon-set id {id:?}");
        let response: SetAnswer =
            self.post_json(ICON_SETS, &json!({"serverSession": self.server_session(), "get": id}))?;
        response.set.ok_or_else(|| anyhow!("Inbox did not return the downloaded icon set"))
    }
    pub(crate) fn upload_agent_icon(&self, bytes: &[u8]) -> Result<(AgentIcon, AgentFacesWire)> {
        ensure!(bytes.len() <= ICON_MAX_BYTES, "Picture: at most 512 KB.");
        let picture = format!("data:application/octet-stream;base64,{}", crate::base64::encode(bytes));
        let response: IconsAnswer = self
            .post_json(AGENT_ICONS, &json!({"serverSession": self.server_session(), "picture": picture}))?;
        Ok((response.icon.ok_or_else(|| anyhow!("Inbox did not return the saved picture"))?, response.faces))
    }
    pub(crate) fn remove_agent_icon(&self, id: &str) -> Result<AgentFacesWire> {
        ensure!(valid_hash(id), "invalid uploaded picture id");
        self.post_json(AGENT_ICONS, &json!({"serverSession": self.server_session(), "remove": id}))
    }
    fn save_agent_face(&self, agent: &SelectedAgent, face: &AgentFace) -> Result<AgentFacesWire> {
        let sessions: Vec<_> = std::iter::once(&agent.session)
            .chain(agent.continues.iter())
            .filter(|id| *id != &agent.id)
            .cloned()
            .collect();
        self.post_json("/api/inbox/agent-faces", &json!({"serverSession": self.server_session(), "agent": agent.id, "sessions": sessions, "face": face.wire()}))
    }
    pub(crate) fn agent_icon_animation_results(&self) -> Result<Vec<AnimateResult>> {
        Ok(self.get_json::<AnimateResultsAnswer>("GET", ICON_ANIMATE)?.results)
    }
    fn ask_agent_to_animate(
        &self,
        agent: &SelectedAgent,
        icon: &AgentIcon,
        target: &LiveSession,
    ) -> Result<String> {
        let sessions: Vec<_> = std::iter::once(&agent.session)
            .chain(agent.continues.iter())
            .filter(|id| *id != &agent.id)
            .cloned()
            .collect();
        let answer: AnimateRequestAnswer = self.post_json(ICON_ANIMATE, &json!({"serverSession": self.server_session(), "animate": {"icon": icon.id, "agent": agent.id, "sessions": sessions}}))?;
        let request = answer.request.context("Inbox did not return an animation request id")?;
        let bytes = self
            .get_bytes("GET", &format!("{AGENT_ICONS}/{}", icon.id))
            .context("reading the original uploaded agent picture")?;
        let filename = format!("agent-icon.{}", extension_for_type(&icon.icon_type));
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query.append_pair("name", &filename).append_pair("serverSession", self.server_session());
        let response = self.post_bytes(
            &format!("/api/inbox/uploads?{}", query.finish()),
            "application/octet-stream",
            &bytes,
        )?;
        let upload: UploadFile = serde_json::from_value(
            response
                .get("upload")
                .cloned()
                .ok_or_else(|| anyhow!("Inbox did not return the animation source upload"))?,
        )
        .context("decoding uploaded animation source")?;
        let result: Value = self.post_json("/api/inbox/messages", &json!({
            "serverSession": self.server_session(), "to": {"host": target.host, "session": target.session},
            "body": animate_prompt(&request), "uploads": [upload.upload_id], "thread_name": "Animate an agent icon", "idempotency_key": new_key(),
        }))?;
        let thread = result.get("thread_id").and_then(Value::as_str).unwrap_or("new thread");
        Ok(format!(
            "Animation request sent to {} in thread {thread}.",
            target.label.as_deref().unwrap_or(&target.session)
        ))
    }
}

fn face_for(wire: Option<&AgentFacesWire>, agent: &str) -> Option<String> {
    wire?.faces.get(agent).map(|entry| entry.face.clone())
}
fn host_name(host: Option<&str>) -> &str {
    super::agents::agent_host_name(host)
}
fn state_label(agent: &Agent) -> &'static str {
    match agent.state {
        super::agents::AgentState::Waiting => "waiting",
        super::agents::AgentState::Working => "working",
        super::agents::AgentState::Idle => "idle",
        super::agents::AgentState::Ended => "ended",
    }
}
fn extension_for_type(kind: &str) -> &'static str {
    match kind {
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        _ => "png",
    }
}
fn is_missing_route(error: &anyhow::Error) -> bool {
    error.downcast_ref::<HttpError>().is_some_and(|http| http.status == 404 && http.code.is_none())
}
fn compat_message(feature: &str, error: &anyhow::Error) -> String {
    if is_missing_route(error) {
        format!("{feature} are not supported by this Inbox release; update Plannotator to use them.")
    } else {
        format!("{feature} unavailable: {error:#}")
    }
}

pub(crate) fn run(client: &Client, terminal: &mut DefaultTerminal) -> Result<()> {
    let mut pane = Pane { tab: Some(Tab::Agents), ..Pane::default() };
    pane.refresh(client);
    while !pane.quit {
        draw(terminal, &pane)?;
        if !event::poll(TICK)? {
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                handle_key(client, terminal, &mut pane, key.code)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn draw(terminal: &mut DefaultTerminal, pane: &Pane) -> Result<()> {
    let items = pane.items();
    let tab = pane.tab.unwrap_or(Tab::Agents);
    terminal.draw(|frame| {
        let [header, list, detail, footer] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(3),
            Constraint::Length(2),
        ])
        .areas(frame.area());
        let agent = pane.agent.as_ref().map_or("No agent selected", |selected| selected.name.as_str());
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled(
                        "Agent icons",
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" · "),
                    Span::raw(agent),
                ]),
                Line::from("1 Agents · 2 Your pictures · 3 Icon sets · 4 Animation originals"),
            ])
            .block(Block::default().borders(Borders::ALL)),
            header,
        );
        let rows: Vec<ListItem<'_>> = items.iter().map(|item| ListItem::new(item.label.as_str())).collect();
        let mut state = ListState::default().with_selected((!items.is_empty()).then_some(pane.cursor));
        let title = if pane.listing.is_some() { "Icon set contents" } else { tab.title() };
        frame.render_stateful_widget(
            List::new(rows)
                .block(Block::default().borders(Borders::ALL).title(title))
                .highlight_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
                .highlight_symbol("> "),
            list,
            &mut state,
        );
        let details = items
            .get(pane.cursor)
            .map_or_else(|| "No entries are available.".to_owned(), |item| item.detail.clone());
        frame.render_widget(
            Paragraph::new(details)
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).title("Selected")),
            detail,
        );
        let mut help = match (pane.listing.is_some(), tab) {
            (true, _) => "Enter apply icon · Backspace/Esc return · ↑/↓ select".to_owned(),
            (false, Tab::Agents) => "Enter select agent · 1–4 switch page · r refresh · q close".to_owned(),
            (false, Tab::Pictures) => {
                "Enter apply picture · p upload · d remove · m animate · 1–4 switch · q close".to_owned()
            }
            (false, Tab::Sets) => {
                "Enter browse set · g download fetchable set · r refresh · 1–4 switch · q close".to_owned()
            }
            (false, Tab::Originals) => {
                "Enter use original on selected agent · r refresh · 1–4 switch · q close".to_owned()
            }
        };
        if let Some(status) = &pane.status {
            help = format!("{status}\n{help}");
        }
        frame.render_widget(
            Paragraph::new(help)
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).title("Actions / status")),
            footer,
        );
    })?;
    Ok(())
}

fn handle_key(client: &Client, terminal: &mut DefaultTerminal, pane: &mut Pane, code: KeyCode) -> Result<()> {
    if let Some(listing) = pane.listing.clone() {
        match code {
            KeyCode::Esc | KeyCode::Backspace => {
                pane.listing = None;
                pane.cursor = 0;
            }
            KeyCode::Up | KeyCode::Char('k') => pane.cursor = pane.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                pane.cursor = (pane.cursor + 1).min(listing.icons.len().saturating_sub(1));
            }
            KeyCode::Enter => {
                let Some(ItemAction::SetIcon(icon)) =
                    pane.items().get(pane.cursor).map(|item| item.action.clone())
                else {
                    return Ok(());
                };
                if listing.set.kind == "fetchable" && !listing.have.iter().any(|id| id == &icon.id) {
                    pane.status("Download this set before assigning one of its icons.");
                } else if let Some(agent) = pane.agent.as_ref() {
                    let face = AgentFace::Set { set: listing.set.id.clone(), icon: icon.id };
                    match client.save_agent_face(agent, &face) {
                        Ok(wire) => {
                            pane.faces = Some(wire);
                            pane.status(format!("{} now shows {}.", agent.name, face.wire()));
                        }
                        Err(error) => pane.status(compat_message("Saving agent faces", &error)),
                    }
                } else {
                    pane.status("Select an agent in the Agents page first.");
                }
            }
            _ => {}
        }
        return Ok(());
    }
    match code {
        KeyCode::Esc | KeyCode::Char('q') => pane.quit = true,
        KeyCode::Char('1') => set_tab(pane, Tab::Agents),
        KeyCode::Char('2') => set_tab(pane, Tab::Pictures),
        KeyCode::Char('3') => set_tab(pane, Tab::Sets),
        KeyCode::Char('4') => set_tab(pane, Tab::Originals),
        KeyCode::Tab => set_tab(pane, pane.tab.unwrap_or(Tab::Agents).next()),
        KeyCode::Up | KeyCode::Char('k') => pane.cursor = pane.cursor.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => {
            pane.cursor = (pane.cursor + 1).min(pane.items().len().saturating_sub(1));
        }
        KeyCode::Home => pane.cursor = 0,
        KeyCode::End => pane.cursor = pane.items().len().saturating_sub(1),
        KeyCode::Char('r') => pane.refresh(client),
        KeyCode::Enter => activate(client, pane),
        KeyCode::Char('p') if pane.tab == Some(Tab::Pictures) => upload_picture(client, terminal, pane)?,
        KeyCode::Char('d') if pane.tab == Some(Tab::Pictures) => remove_picture(client, terminal, pane)?,
        KeyCode::Char('m') if pane.tab == Some(Tab::Pictures) => animate_picture(client, terminal, pane)?,
        KeyCode::Char('g') if pane.tab == Some(Tab::Sets) => download_set(client, pane),
        _ => {}
    }
    Ok(())
}

fn set_tab(pane: &mut Pane, tab: Tab) {
    pane.tab = Some(tab);
    pane.cursor = 0;
    pane.status = None;
}

fn activate(client: &Client, pane: &mut Pane) {
    match pane.items().get(pane.cursor).map(|item| item.action.clone()) {
        Some(ItemAction::Agent(agent)) => {
            pane.status(format!("Selected {} · {}.", agent.name, agent.session));
            pane.agent = Some(agent);
        }
        Some(ItemAction::Picture(icon)) => {
            if let Some(agent) = pane.agent.as_ref() {
                let face = AgentFace::Own(icon.id);
                match client.save_agent_face(agent, &face) {
                    Ok(wire) => {
                        pane.faces = Some(wire);
                        pane.status(format!("{} now shows {}.", agent.name, face.wire()));
                    }
                    Err(error) => pane.status(compat_message("Saving agent faces", &error)),
                }
            } else {
                pane.status("Select an agent in the Agents page first.");
            }
        }
        Some(ItemAction::Set(set)) => {
            if set.kind != "fetchable" || set.get.as_ref().is_some_and(|get| get.state == "done") {
                match client.agent_icon_set(&set.id) {
                    Ok(listing) => {
                        pane.listing = Some(listing);
                        pane.cursor = 0;
                        pane.status(format!("{} · {} icons", set.name, set.count));
                    }
                    Err(error) => pane.status(compat_message("Icon set", &error)),
                }
            } else {
                pane.status("Download this fetchable set first with g.");
            }
        }
        Some(ItemAction::Original(result)) => {
            if let Some(agent) = pane.agent.as_ref() {
                let face = AgentFace::Own(result.icon);
                match client.save_agent_face(agent, &face) {
                    Ok(wire) => {
                        pane.faces = Some(wire);
                        pane.status(format!("Restored the original picture on {}.", agent.name));
                    }
                    Err(error) => pane.status(compat_message("Restoring the original picture", &error)),
                }
            } else {
                pane.status("Select an agent in the Agents page first.");
            }
        }
        None | Some(ItemAction::SetIcon(_)) => {}
    }
    pane.clamp_cursor();
}

fn upload_picture(client: &Client, terminal: &mut DefaultTerminal, pane: &mut Pane) -> Result<()> {
    let Some(path) = input(
        terminal,
        "Upload agent picture",
        "Enter a local image path. PNG, JPEG, GIF, WebP or SVG, up to 512 KB.",
    )?
    else {
        return Ok(());
    };
    let bytes = match std::fs::read(path.trim()) {
        Ok(bytes) => bytes,
        Err(error) => {
            pane.status(format!("Could not read picture: {error}"));
            return Ok(());
        }
    };
    match client.upload_agent_icon(&bytes) {
        Ok((icon, wire)) => {
            pane.faces = Some(wire);
            pane.status(format!("Uploaded picture {}. Enter applies it to the selected agent.", icon.id));
            set_tab(pane, Tab::Pictures);
            pane.cursor = pane
                .faces
                .as_ref()
                .and_then(|wire| wire.icons.as_ref())
                .and_then(|icons| icons.iter().position(|item| item.id == icon.id))
                .unwrap_or(0);
        }
        Err(error) => pane.status(compat_message("Uploading agent pictures", &error)),
    }
    Ok(())
}

fn remove_picture(client: &Client, terminal: &mut DefaultTerminal, pane: &mut Pane) -> Result<()> {
    let Some(ItemAction::Picture(icon)) = pane.items().get(pane.cursor).map(|item| item.action.clone())
    else {
        pane.status("Select an uploaded picture to remove.");
        return Ok(());
    };
    if !confirm(
        terminal,
        "Remove picture",
        &format!("Remove {}? Any agent showing it returns to its own mark.", icon.id),
    )? {
        return Ok(());
    }
    match client.remove_agent_icon(&icon.id) {
        Ok(wire) => {
            pane.faces = Some(wire);
            pane.status("Picture removed; agents that used it return to their own mark.");
        }
        Err(error) => pane.status(compat_message("Removing uploaded pictures", &error)),
    }
    pane.clamp_cursor();
    Ok(())
}

fn download_set(client: &Client, pane: &mut Pane) {
    let Some(ItemAction::Set(set)) = pane.items().get(pane.cursor).map(|item| item.action.clone()) else {
        pane.status("Select a fetchable icon set to download.");
        return;
    };
    if set.kind != "fetchable" {
        pane.status("Built-in icon sets are already available.");
        return;
    }
    match client.get_agent_icon_set(&set.id) {
        Ok(set) => {
            pane.status(format!("Downloaded {} ({} icons).", set.name, set.count));
            if let Ok(sets) = client.agent_icon_sets(true) {
                pane.sets = sets;
            }
        }
        Err(error) => pane.status(compat_message("Downloading icon sets", &error)),
    }
}

fn animate_picture(client: &Client, terminal: &mut DefaultTerminal, pane: &mut Pane) -> Result<()> {
    let Some(agent) = pane.agent.clone() else {
        pane.status("Select an agent first.");
        return Ok(());
    };
    let Some(ItemAction::Picture(icon)) = pane.items().get(pane.cursor).map(|item| item.action.clone())
    else {
        pane.status("Select an uploaded picture to animate.");
        return Ok(());
    };
    let sessions = match client.get_json::<SessionsAnswer>("GET", "/api/inbox/sessions") {
        Ok(answer) => answer,
        Err(error) => {
            pane.status(compat_message("Live agent sessions", &error));
            return Ok(());
        }
    };
    let live: Vec<(String, LiveSession)> = sessions
        .projects
        .into_iter()
        .flat_map(|project| {
            let project_name = project.project.name;
            project.sessions.into_iter().map(move |session| {
                let label = session.label.clone().unwrap_or_else(|| session.session.clone());
                let display = format!(
                    "{project_name} · {} · {label}{}",
                    host_name(Some(&session.host)),
                    if session.busy == Some(true) { " · busy" } else { "" }
                );
                (display, session)
            })
        })
        .collect();
    if live.is_empty() {
        pane.status("No live agent session can receive the animation request.");
        return Ok(());
    }
    let labels: Vec<String> = live.iter().map(|(label, _)| label.clone()).collect();
    let Some(index) = choose(
        terminal,
        "Send animation request to a live agent",
        "This creates a new Inbox thread with the source picture attached.",
        &labels,
    )?
    else {
        return Ok(());
    };
    let Some((_, target)) = live.get(index) else { return Ok(()) };
    if !confirm(
        terminal,
        "Animate this picture",
        &format!(
            "Ask {} to animate {}? This sends the original picture and a new message.",
            target.label.as_deref().unwrap_or(&target.session),
            icon.id
        ),
    )? {
        return Ok(());
    }
    match client.ask_agent_to_animate(&agent, &icon, target) {
        Ok(message) => {
            pane.status(message);
            pane.refresh(client);
        }
        Err(error) => pane.status(compat_message("Animation request", &error)),
    }
    Ok(())
}

fn input(terminal: &mut DefaultTerminal, title: &str, detail: &str) -> Result<Option<String>> {
    let mut compose = Compose::default();
    loop {
        terminal.draw(|frame| {
            let [info, field, help] =
                Layout::vertical([Constraint::Length(4), Constraint::Min(3), Constraint::Length(2)])
                    .areas(frame.area());
            frame.render_widget(Paragraph::new(detail).wrap(Wrap { trim: false }), info);
            let block = Block::default().borders(Borders::ALL).title(title);
            let inner = block.inner(field);
            let (lines, row, col) = compose.wrapped(usize::from(inner.width));
            frame.render_widget(
                Paragraph::new(lines.into_iter().map(Line::from).collect::<Vec<_>>()).block(block),
                field,
            );
            if inner.width > 0 && inner.height > 0 {
                frame.set_cursor_position((
                    inner.x + u16::try_from(col).unwrap_or(0).min(inner.width - 1),
                    inner.y + u16::try_from(row).unwrap_or(0).min(inner.height - 1),
                ));
            }
            frame.render_widget(Paragraph::new("Enter accept · Esc cancel · Paste a path"), help);
        })?;
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match compose.handle_key(key) {
                ComposeAction::Save => return Ok(Some(compose.value())),
                ComposeAction::Cancel => return Ok(None),
                ComposeAction::Edited => {}
            },
            Event::Paste(text) => compose.insert_text(&text),
            _ => {}
        }
    }
}

fn choose(
    terminal: &mut DefaultTerminal,
    title: &str,
    detail: &str,
    labels: &[String],
) -> Result<Option<usize>> {
    let mut selected = 0usize;
    loop {
        terminal.draw(|frame| {
            let [list, details, help] =
                Layout::vertical([Constraint::Percentage(55), Constraint::Min(3), Constraint::Length(2)])
                    .areas(frame.area());
            let rows: Vec<ListItem<'_>> = labels.iter().map(|label| ListItem::new(label.as_str())).collect();
            let mut state = ListState::default().with_selected((!labels.is_empty()).then_some(selected));
            frame.render_stateful_widget(
                List::new(rows)
                    .block(Block::default().borders(Borders::ALL).title(title))
                    .highlight_style(Style::default().fg(Color::Cyan))
                    .highlight_symbol("> "),
                list,
                &mut state,
            );
            frame.render_widget(
                Paragraph::new(detail)
                    .wrap(Wrap { trim: false })
                    .block(Block::default().borders(Borders::ALL)),
                details,
            );
            frame.render_widget(Paragraph::new("↑/↓ or j/k choose · Enter select · Esc cancel"), help);
        })?;
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Esc => return Ok(None),
                KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    selected = (selected + 1).min(labels.len().saturating_sub(1));
                }
                KeyCode::Enter if !labels.is_empty() => return Ok(Some(selected)),
                _ => {}
            },
            _ => {}
        }
    }
}
fn confirm(terminal: &mut DefaultTerminal, title: &str, detail: &str) -> Result<bool> {
    Ok(choose(terminal, title, detail, &["Cancel".to_owned(), "Confirm".to_owned()])?
        .is_some_and(|index| index == 1))
}

fn animate_prompt(request: &str) -> String {
    format!(
        "Please animate the picture attached to this message. It is the icon of one of my agents in my Plannotator Inbox.\n\nMake one animated SVG of it that is clearly alive, with personality, like a gentle idle animation in a game.\n\nPixel art: keep the original pixels and colours, split into 2 to 8 moving parts, and move them in whole pixels without gaps. Smooth pictures: never redraw or restyle the original; embed it unchanged as a data URL and move the whole picture, with at most three cut-out parts.\n\nUse CSS animations only (transform and opacity), with a visible idle loop of 1.2 to 2.5 seconds and distinct gentle gestures every few seconds. The first frame with animation disabled must be the exact original. No flashing, spinning, hover response, scripts, event attributes, foreign objects, external resources, SMIL, or web fonts. One square-viewBox SVG, under 512 KB, legible at 16 and 24 px on light and dark backgrounds. Watch multiple frames at icon size and compare against the original.\n\nHand it back with `plannotator inbox icon set <file.svg> --request {request}`. It becomes this agent's icon; the original can be restored in the Inbox. Then say in one line what moves and how."
    )
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, reason = "tests assert by panicking")]
mod tests {
    use super::*;
    #[test]
    fn serializes_selected_face_identifiers() {
        assert_eq!(AgentFace::Own("a".repeat(64)).wire(), format!("own:{}", "a".repeat(64)));
        assert_eq!(
            AgentFace::Set { set: "animals".to_owned(), icon: "cat".to_owned() }.wire(),
            "set:animals/cat"
        );
    }

    #[test]
    fn validates_icon_request_identifiers() {
        assert!(valid_hash(&"a".repeat(64)));
        assert!(!valid_hash("bad"));
        assert!(valid_key("animals", 40));
        assert!(!valid_key("../cat", 40));
    }
}
