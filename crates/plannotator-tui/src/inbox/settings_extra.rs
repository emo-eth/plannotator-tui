use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use serde_json::{Value, json};

use super::client::Client;
use crate::app::compose::{Compose, ComposeAction};

const TABS: &[&str] = &["Prompts", "Agents", "Sidebar", "Reactions", "Voice", "Tour", "Accounts", "Identity"];
const PROMPT_HOSTS: &[(&str, &str)] =
    &[("claude-code", "Claude Code"), ("pi", "Pi"), ("opencode", "OpenCode")];
const SIDEBAR_ITEMS: &[(&str, &str, &str)] = &[
    ("logo", "Logo", "brand"),
    ("search", "Search", "top"),
    ("new-message", "New message", "top"),
    ("inbox", "Inbox", "top"),
    ("board", "Board", "top"),
    ("shape", "Shape", "top"),
    ("artifacts", "Artifacts", "top"),
    ("reviews", "Reviews", "top"),
    ("drafts", "Drafts", "top"),
    ("agents", "Agents", "middle"),
    ("copy-prompt", "Copy prompt", "bottom"),
    ("feedback", "Feedback", "bottom"),
    ("usage", "Usage", "bottom"),
    ("settings", "Settings", "bottom"),
    ("shortcuts", "Keyboard shortcuts", "settings"),
    ("theme", "Light and dark", "settings"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Prompts,
    Agents,
    Sidebar,
    Reactions,
    Voice,
    Tour,
    Accounts,
    Identity,
}
impl Tab {
    fn title(self) -> &'static str {
        match self {
            Self::Prompts => "Prompts",
            Self::Agents => "Agents",
            Self::Sidebar => "Sidebar",
            Self::Reactions => "Reactions",
            Self::Voice => "Voice",
            Self::Tour => "Tour",
            Self::Accounts => "Accounts",
            Self::Identity => "Identity",
        }
    }

    fn next(self, backwards: bool) -> Self {
        match (self, backwards) {
            (Self::Prompts, false) | (Self::Sidebar, true) => Self::Agents,
            (Self::Agents, false) | (Self::Reactions, true) => Self::Sidebar,
            (Self::Sidebar, false) | (Self::Voice, true) => Self::Reactions,
            (Self::Reactions, false) | (Self::Tour, true) => Self::Voice,
            (Self::Voice, false) | (Self::Accounts, true) => Self::Tour,
            (Self::Tour, false) | (Self::Identity, true) => Self::Accounts,
            (Self::Accounts, false) | (Self::Prompts, true) => Self::Identity,
            (Self::Identity, false) | (Self::Agents, true) => Self::Prompts,
        }
    }
}

#[derive(Debug, Clone)]
struct Sidebar {
    top: Vec<String>,
    bottom: Vec<String>,
    hidden: Vec<String>,
}

#[derive(Debug)]
struct Pane {
    settings: Value,
    agents: Option<Value>,
    voice_status: Option<Value>,
    accounts: BTreeMap<String, Value>,
    machines: Option<Value>,
    sidebar: Option<Sidebar>,
    sidebar_dirty: bool,
    tab: Tab,
    selected: usize,
    status: String,
}

pub(super) fn run(client: &Client, terminal: &mut DefaultTerminal) -> Result<()> {
    let settings = super::settings::read(client)?;
    let mut pane = Pane {
        sidebar: parse_sidebar(settings.get("sidebar")),
        settings,
        agents: None,
        voice_status: None,
        accounts: BTreeMap::new(),
        machines: None,
        sidebar_dirty: false,
        tab: Tab::Prompts,
        selected: 0,
        status: "Settings are read-only until an explicit action is chosen.".into(),
    };
    loop {
        draw(terminal, &pane)?;
        let Event::Key(key) = event::read()? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(()),
            KeyCode::Tab | KeyCode::Right | KeyCode::BackTab | KeyCode::Left => {
                pane.tab = pane.tab.next(matches!(key.code, KeyCode::BackTab | KeyCode::Left));
                pane.selected = 0;
                pane.status.clear();
            }
            KeyCode::Up | KeyCode::Char('k') => pane.selected = pane.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => pane.selected = pane.selected.saturating_add(1),
            _ => {
                if let Err(error) = pane.handle_key(client, terminal, key.code) {
                    pane.status = format!("Action not completed: {error:#}");
                }
            }
        }
        pane.selected = pane.selected.min(row_count(&pane).saturating_sub(1));
    }
}

impl Pane {
    fn refresh(&mut self, client: &Client) -> Result<()> {
        let settings = super::settings::read(client)?;
        if !self.sidebar_dirty {
            self.sidebar = parse_sidebar(settings.get("sidebar"));
        }
        self.settings = settings;
        self.voice_status = None;
        self.status = if self.sidebar_dirty {
            "Settings refreshed; unsaved sidebar edits retained"
        } else {
            "Settings refreshed"
        }
        .into();
        Ok(())
    }

    fn handle_key(&mut self, client: &Client, terminal: &mut DefaultTerminal, key: KeyCode) -> Result<()> {
        if key == KeyCode::Char('r') && !matches!(self.tab, Tab::Agents | Tab::Voice | Tab::Accounts) {
            return self.refresh(client);
        }
        match self.tab {
            Tab::Prompts => match key {
                KeyCode::Enter | KeyCode::Char('e') => {
                    self.status = edit_prompt(client, terminal, &mut self.settings, self.selected)?.into();
                }
                KeyCode::Char('d') => {
                    self.status = reset_prompt(client, terminal, &mut self.settings, self.selected)?.into();
                }
                _ => {}
            },
            Tab::Agents => self.agent_key(client, terminal, key)?,
            Tab::Sidebar => self.sidebar_key(client, terminal, key)?,
            Tab::Reactions => match key {
                KeyCode::Char('a' | '+') => self.status = add_reaction(client, terminal, &mut self.settings)?,
                KeyCode::Enter | KeyCode::Char('e' | 'c') => {
                    self.status = edit_reaction(
                        client,
                        terminal,
                        &mut self.settings,
                        self.selected,
                        key == KeyCode::Char('c'),
                    )?
                    .into();
                }
                KeyCode::Char('d') => {
                    self.status = remove_reaction(client, terminal, &mut self.settings, self.selected)?;
                }
                _ => {}
            },
            Tab::Voice => match key {
                KeyCode::Char('r') => {
                    self.voice_status = Some(client.get_json("GET", "/api/inbox/voice/status?fresh=1")?);
                    self.status = "Voice status refreshed".into();
                }
                KeyCode::Enter => {
                    self.status = voice_action(
                        client,
                        terminal,
                        &mut self.settings,
                        self.selected,
                        &mut self.voice_status,
                    )?
                    .into();
                }
                _ => {}
            },
            Tab::Tour => self.tour_key(client, key)?,
            Tab::Accounts => self.accounts_key(client, terminal, key)?,
            Tab::Identity => {
                if matches!(key, KeyCode::Enter | KeyCode::Char('e')) {
                    self.status = "This Inbox has no supported display-name write route.".into();
                }
            }
        }
        Ok(())
    }

    fn agent_key(&mut self, client: &Client, terminal: &mut DefaultTerminal, key: KeyCode) -> Result<()> {
        if key == KeyCode::Char('r')
            || (self.agents.is_none() && matches!(key, KeyCode::Enter | KeyCode::Char('g' | 'm' | 'n')))
        {
            self.agents = Some(client.get_json("GET", "/api/inbox/agents")?);
            self.status = "Agent settings refreshed".into();
        } else if matches!(key, KeyCode::Char('g' | 'm' | 'n')) {
            self.status = change_agent(
                client,
                terminal,
                self.agents.as_mut().context("read agent settings first")?,
                self.selected,
                key,
            )?
            .into();
        }
        Ok(())
    }

    fn sidebar_key(&mut self, client: &Client, terminal: &mut DefaultTerminal, key: KeyCode) -> Result<()> {
        match key {
            KeyCode::Char('h' | ' ') => {
                sidebar_toggle(
                    self.sidebar.as_mut().context("sidebar layout is unavailable")?,
                    self.selected,
                )?;
                self.sidebar_dirty = true;
                self.status = "Layout changed locally; press s to save".into();
            }
            KeyCode::Char('[' | ']') => {
                sidebar_move(
                    self.sidebar.as_mut().context("sidebar layout is unavailable")?,
                    self.selected,
                    if key == KeyCode::Char('[') { -1 } else { 1 },
                )?;
                self.sidebar_dirty = true;
                self.status = "Layout changed locally; press s to save".into();
            }
            KeyCode::Char('s') => {
                ensure!(self.sidebar_dirty, "sidebar has no unsaved changes");
                let layout = self.sidebar.as_ref().context("sidebar layout is unavailable")?;
                let value = save_extra(
                    client,
                    &json!({"sidebar":{"top":layout.top,"bottom":layout.bottom,"hidden":layout.hidden}}),
                )?;
                let sidebar = parse_sidebar(value.get("sidebar"))
                    .context("sidebar save response omitted a usable layout; local edits retained")?;
                merge(&mut self.settings, &value);
                self.sidebar = Some(sidebar);
                self.sidebar_dirty = false;
                self.status = "Sidebar layout saved".into();
            }
            KeyCode::Char('d') => {
                ensure!(self.sidebar.is_some(), "sidebar layout is unavailable");
                if !confirm(
                    terminal,
                    "Reset sidebar layout?",
                    "This replaces the saved sidebar order and hidden items with Plannotator's default.",
                )? {
                    self.status = "Sidebar reset cancelled".into();
                    return Ok(());
                }
                let layout = default_sidebar();
                let value = save_extra(
                    client,
                    &json!({"sidebar":{"top":layout.top,"bottom":layout.bottom,"hidden":layout.hidden}}),
                )?;
                let sidebar = parse_sidebar(value.get("sidebar"))
                    .context("sidebar reset response omitted a usable layout")?;
                merge(&mut self.settings, &value);
                self.sidebar = Some(sidebar);
                self.sidebar_dirty = false;
                self.status = "Sidebar reset to default".into();
            }
            _ => {}
        }
        Ok(())
    }

    fn tour_key(&mut self, client: &Client, key: KeyCode) -> Result<()> {
        if matches!(key, KeyCode::Enter | KeyCode::Char('n')) {
            ensure!(
                self.selected == 3,
                "onboarding timestamps are read-only; select the notification offer row"
            );
            let current = self
                .settings
                .pointer("/tour/notify_again")
                .and_then(Value::as_bool)
                .context("tour preferences are unavailable")?;
            let value = save_extra(client, &json!({"tour":{"notify_again":!current}}))?;
            ensure!(
                value.pointer("/tour/notify_again").and_then(Value::as_bool) == Some(!current),
                "server did not confirm the tour preference"
            );
            merge(&mut self.settings, &value);
            self.status = if current {
                "First-thread notification offer disabled"
            } else {
                "The first real thread will offer notifications again"
            }
            .into();
        }
        Ok(())
    }

    fn accounts_key(&mut self, client: &Client, terminal: &mut DefaultTerminal, key: KeyCode) -> Result<()> {
        if !matches!(key, KeyCode::Enter | KeyCode::Char('r')) {
            return Ok(());
        }
        if self.machines.is_none() || key == KeyCode::Char('r') {
            self.machines = Some(client.get_json("GET", "/api/inbox/machines")?);
            self.selected = self.selected.min(row_count(self).saturating_sub(1));
            self.status = "Linked machines refreshed; Enter reads selected account usage".into();
        }
        if key == KeyCode::Enter {
            let (cache_key, endpoint, name) = account_target(self.selected, self.machines.as_ref())
                .context("no account source selected")?;
            if !confirm(
                terminal,
                "Read account usage?",
                &format!(
                    "Reading {name} may ask claude-swap to refresh an expired login. No account will be switched."
                ),
            )? {
                self.status = "Account read cancelled".into();
                return Ok(());
            }
            let value: Value = client.get_json("GET", &endpoint)?;
            self.status = format!(
                "{name}: {}",
                value.get("state").and_then(Value::as_str).unwrap_or("unrecognized account response")
            );
            self.accounts.insert(cache_key, value);
        }
        Ok(())
    }
}
fn save_extra(client: &Client, patch: &Value) -> Result<Value> {
    let mut body = patch.as_object().context("settings patch must be an object")?.clone();
    body.insert("serverSession".into(), Value::String(client.server_session().to_owned()));
    client.post_json("/api/inbox/settings", &Value::Object(body))
}

fn draw(terminal: &mut DefaultTerminal, pane: &Pane) -> Result<()> {
    let Pane { tab, selected, status, .. } = pane;
    let rows = rows(pane);
    let detail = detail(pane);
    terminal.draw(|frame| {
        let [title, list, detail_area, foot] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(4),
            Constraint::Length(5),
            Constraint::Length(2),
        ])
        .areas(frame.area());
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("Inbox settings  ", Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(tab.title(), Style::default().add_modifier(Modifier::BOLD)),
                Span::raw("  ·  "),
                Span::raw(TABS.join("  ")),
            ])),
            title,
        );
        let items = rows.iter().map(|line| ListItem::new(line.as_str())).collect::<Vec<_>>();
        let mut state = ListState::default()
            .with_selected((!items.is_empty()).then_some((*selected).min(items.len().saturating_sub(1))));
        frame.render_stateful_widget(
            List::new(items)
                .block(Block::default().borders(Borders::ALL).title(tab.title()))
                .highlight_symbol("› ")
                .highlight_style(Style::default().add_modifier(Modifier::BOLD)),
            list,
            &mut state,
        );
        frame.render_widget(
            Paragraph::new(detail)
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).title("Details")),
            detail_area,
        );
        frame.render_widget(
            Paragraph::new(format!("{status}\n↑↓ move · Tab group · Enter action · r refresh · Esc close"))
                .wrap(Wrap { trim: false }),
            foot,
        );
    })?;
    Ok(())
}

fn row_count(pane: &Pane) -> usize {
    match pane.tab {
        Tab::Prompts => prompts(&pane.settings).len(),
        Tab::Agents => pane
            .agents
            .as_ref()
            .and_then(|model| model.get("agents"))
            .and_then(Value::as_array)
            .map_or(1, Vec::len),
        Tab::Sidebar => SIDEBAR_ITEMS.len(),
        Tab::Reactions => reactions(&pane.settings).len().max(1),
        Tab::Voice => 5,
        Tab::Tour => 4,
        Tab::Accounts => {
            1 + pane
                .machines
                .as_ref()
                .and_then(|model| model.get("machines"))
                .and_then(Value::as_array)
                .map_or(0, Vec::len)
        }
        Tab::Identity => 1,
    }
}

fn rows(pane: &Pane) -> Vec<String> {
    let settings = &pane.settings;
    let sidebar = pane.sidebar.as_ref();
    let voice_status = pane.voice_status.as_ref();
    match pane.tab {
        Tab::Prompts => prompts(settings)
            .into_iter()
            .map(|(_, label, text, custom)| {
                format!(
                    "{}  {label}{}",
                    if custom { "Custom" } else { "Default" },
                    if text.is_empty() { " · empty" } else { "" }
                )
            })
            .collect(),
        Tab::Agents => {
            pane.agents.as_ref().and_then(|model| model.get("agents")).and_then(Value::as_array).map_or_else(
                || vec!["Press r to read agent settings".into()],
                |agents| {
                    agents
                        .iter()
                        .map(|agent| {
                            format!(
                                "{} · {} · reminder {}",
                                agent.get("label").and_then(Value::as_str).unwrap_or("Unnamed agent"),
                                agent
                                    .pointer("/reach/state")
                                    .and_then(Value::as_str)
                                    .unwrap_or("not reported"),
                                bool_label(agent.pointer("/reminder/on"))
                            )
                        })
                        .collect()
                },
            )
        }
        Tab::Sidebar => SIDEBAR_ITEMS
            .iter()
            .map(|(id, label, place)| {
                let on = sidebar.is_some_and(|s| !s.hidden.iter().any(|item| item == id));
                let stack = sidebar.and_then(|s| {
                    if s.top.iter().any(|item| item == id) {
                        Some(("top", s.top.iter().position(|item| item == id)))
                    } else if s.bottom.iter().any(|item| item == id) {
                        Some(("bottom", s.bottom.iter().position(|item| item == id)))
                    } else {
                        None
                    }
                });
                let location = stack.map_or_else(
                    || (*place).to_owned(),
                    |(name, index)| format!("{name} #{}", index.unwrap_or(0) + 1),
                );
                format!(
                    "[{}] {label} · {location}",
                    if sidebar.is_none() {
                        "?"
                    } else if on {
                        "x"
                    } else {
                        " "
                    }
                )
            })
            .collect(),
        Tab::Reactions => {
            let mut out = reactions(settings)
                .iter()
                .map(|r| {
                    format!(
                        "{}  {}",
                        r.get("emoji").and_then(Value::as_str).unwrap_or("?"),
                        r.get("label").and_then(Value::as_str).unwrap_or("")
                    )
                })
                .collect::<Vec<_>>();
            if out.is_empty() {
                out.push("No saved reactions · a add one".into());
            }
            out
        }
        Tab::Voice => vec![
            format!("Voice dictation  ·  {}", bool_label(settings.pointer("/voice/on"))),
            format!("Phones only  ·  {}", bool_label(settings.pointer("/voice/phones_only"))),
            format!(
                "Engine  ·  {}",
                settings.pointer("/voice/engine").and_then(Value::as_str).unwrap_or("not reported")
            ),
            format!("Credential  ·  {}", voice_key_label(settings)),
            format!(
                "Readiness  ·  {}",
                voice_status
                    .and_then(|v| v.get("state"))
                    .and_then(Value::as_str)
                    .unwrap_or("not read · press r")
            ),
        ],
        Tab::Tour => {
            let t = settings.get("tour");
            vec![
                format!("Started  ·  {}", stamp(t.and_then(|v| v.get("seen_at")))),
                format!("Finished  ·  {}", stamp(t.and_then(|v| v.get("finished_at")))),
                format!("Skipped  ·  {}", stamp(t.and_then(|v| v.get("skipped_at")))),
                format!(
                    "Offer notifications on first thread again  ·  {}",
                    bool_label(t.and_then(|v| v.get("notify_again")))
                ),
            ]
        }
        Tab::Accounts => account_rows(&pane.accounts, pane.machines.as_ref()),
        Tab::Identity => vec![format!(
            "Name on comments  ·  {}",
            settings.get("display_name").and_then(Value::as_str).unwrap_or("not set")
        )],
    }
}

fn detail(pane: &Pane) -> String {
    let settings = &pane.settings;
    let agents = pane.agents.as_ref();
    let voice_status = pane.voice_status.as_ref();
    let sidebar = pane.sidebar.as_ref();
    let selected = pane.selected;
    match pane.tab {
        Tab::Prompts => prompts(settings).get(selected).map_or("Prompts are not exposed by this Inbox version.".into(), |(_,label,text,_)|format!("{label}\n\n{text}\n\nEnter/e edit · d reset a custom prompt (confirmed). Agent-specific reminders take precedence over the shared reminder.")),
        Tab::Agents => {
            let row = agents.and_then(|m|m.get("agents")).and_then(Value::as_array).and_then(|a|a.get(selected));
            row.map_or_else(||"Press r to read local agent settings. Reads inspect detected agent configuration; they do not change it.".into(),|agent|format!("{}\nReach: {} · {}\nReminder: {}\nNote: {}\n{}\n\ng toggles reach · m toggles reminder · n toggles note; reach/note changes ask for confirmation.",agent.get("label").and_then(Value::as_str).unwrap_or("Agent"),agent.pointer("/reach/state").and_then(Value::as_str).unwrap_or("unknown"),agent.pointer("/reach/path").and_then(Value::as_str).unwrap_or("no file path"),bool_label(agent.pointer("/reminder/on")),agent.pointer("/note/state").and_then(Value::as_str).unwrap_or("not supported"),agent.pointer("/reach/reason").and_then(Value::as_str).unwrap_or("")))
        },
        Tab::Sidebar => format!("{}\n{}\nChanges are local until saved. h/Space hide or show; [ and ] move within the current stack; d reset (confirmed); s save.",if sidebar.is_some(){"Sidebar visibility and top/bottom order."}else{"Sidebar layout is unavailable on this Inbox version."}, if pane.sidebar_dirty {"Unsaved local changes"} else {"No local changes"}),
        Tab::Reactions => reactions(settings).get(selected).map_or("Saved reactions appear in the reaction tray. + add · Enter/e name · c change emoji · d remove (confirmed). 👍 is built in and cannot be saved.".into(),|r|format!("{} · {}\n\n+ add · Enter/e name · c change emoji · d remove (confirmed).",r.get("emoji").and_then(Value::as_str).unwrap_or("?"),r.get("label").and_then(Value::as_str).unwrap_or("unnamed"))),
        Tab::Voice => format!("Voice dictation uses {}.\nStatus: {}{}\nCredential: {}\n\nEnter changes the selected switch/engine or edits the API key. Saving or removing a credential requires confirmation; its value is never displayed.",settings.pointer("/voice/engine").and_then(Value::as_str).unwrap_or("the configured engine"),voice_status.and_then(|v|v.get("state")).and_then(Value::as_str).unwrap_or("not checked"),voice_status.and_then(|v|v.get("plan")).and_then(Value::as_str).map_or(String::new(),|p|format!(" · {p}")),voice_key_label(settings)),
        Tab::Tour => format!("Onboarding timestamps are read-only. The TUI has no interactive tour runner. Enter on the final row toggles the first-thread notification offer again.\nStarted: {} · finished: {} · skipped: {}",stamp(settings.pointer("/tour/seen_at")),stamp(settings.pointer("/tour/finished_at")),stamp(settings.pointer("/tour/skipped_at"))),
        Tab::Accounts => account_target(selected, pane.machines.as_ref()).and_then(|(key,_,_)|pane.accounts.get(&key)).map_or_else(|| "Account usage is read only here. Reading may refresh an expired login; each read asks first. Account switching is disabled in the server contract.".into(),account_detail),
        Tab::Identity => "The display name is read from Inbox settings and is used on comments. The Inbox settings API has no supported way to change it; no write is attempted.".into(),
    }
}

fn prompts(settings: &Value) -> Vec<(String, String, String, bool)> {
    let Some(model) = settings.get("prompts") else { return Vec::new() };
    let mut out = Vec::new();
    let reminder = model.get("reminder");
    let shared = reminder.and_then(|v| v.pointer("/shared")).and_then(Value::as_str).unwrap_or("");
    out.push((
        "shared".into(),
        "Shared Inbox reminder".into(),
        if shared.is_empty() {
            reminder
                .and_then(|v| v.pointer("/defaults/wake"))
                .and_then(Value::as_str)
                .unwrap_or("Default reminder unavailable")
                .to_owned()
        } else {
            shared.to_owned()
        },
        !shared.is_empty(),
    ));
    for (host, label) in PROMPT_HOSTS {
        let text = reminder.and_then(|v| v.pointer(&format!("/runtimes/{host}"))).and_then(Value::as_str);
        out.push((
            (*host).into(),
            format!("{label} reminder"),
            text.or_else(|| reminder.and_then(|v| v.pointer("/shared")).and_then(Value::as_str))
                .unwrap_or_else(|| {
                    reminder
                        .and_then(|v| v.pointer("/defaults/wake"))
                        .and_then(Value::as_str)
                        .unwrap_or("Default reminder unavailable")
                })
                .to_owned(),
            text.is_some(),
        ));
    }
    if let Some(guide) = model.get("guide") {
        let text = guide.get("text").and_then(Value::as_str).unwrap_or("");
        out.push(("guide".into(), "Guided review instructions".into(), text.into(), !text.is_empty()));
    }
    if let Some(others) = model.get("others").and_then(Value::as_array) {
        for p in others {
            out.push((
                "readonly".into(),
                format!(
                    "{} · {}",
                    p.get("group_label").and_then(Value::as_str).unwrap_or("Prompt"),
                    p.get("label").and_then(Value::as_str).unwrap_or("")
                ),
                p.get("text").and_then(Value::as_str).unwrap_or("").to_owned(),
                false,
            ));
        }
    }
    out
}

fn edit_prompt(
    client: &Client,
    terminal: &mut DefaultTerminal,
    settings: &mut Value,
    index: usize,
) -> Result<&'static str> {
    let model = settings.get("prompts").context("prompts are unavailable")?;
    let (slot, label, _, custom) = prompts(settings).into_iter().nth(index).context("no prompt selected")?;
    ensure!(slot != "readonly", "this prompt is read-only");
    ensure!(
        model.pointer("/reminder/editable").and_then(Value::as_bool) != Some(false),
        "prompts are read-only for this Inbox user"
    );
    let max = if slot == "guide" {
        model.pointer("/guide/max").and_then(Value::as_u64).unwrap_or(2000) as usize
    } else {
        model.pointer("/reminder/max").and_then(Value::as_u64).unwrap_or(4128) as usize
    };
    let initial = if slot == "shared" {
        model.pointer("/reminder/shared").and_then(Value::as_str).unwrap_or("")
    } else if slot == "guide" {
        model.pointer("/guide/text").and_then(Value::as_str).unwrap_or("")
    } else {
        model.pointer(&format!("/reminder/runtimes/{slot}")).and_then(Value::as_str).unwrap_or("")
    };
    let mut compose = Compose::with_text(initial);
    let mut error = String::new();
    loop {
        draw_editor(terminal, &label, &compose.value(), false, max, &error)?;
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match compose.handle_key(key) {
                ComposeAction::Cancel => return Ok("Prompt edit cancelled"),
                ComposeAction::Save => {
                    let text = compose.value();
                    let length = text.encode_utf16().count();
                    if length > max {
                        error =
                            format!("Text is {length} UTF-16 units; limit is {max}. Your text remains here.");
                        continue;
                    }
                    let text = if text.trim().is_empty() { Value::Null } else { Value::String(text) };
                    if text.is_null()
                        && custom
                        && !confirm(
                            terminal,
                            "Reset this prompt?",
                            "This deletes your custom text and restores Plannotator's default.",
                        )?
                    {
                        continue;
                    }
                    let patch = if slot == "guide" {
                        json!({"prompts":{"guide":text}})
                    } else if slot == "shared" {
                        json!({"prompts":{"inbox":{"reminder":text}}})
                    } else {
                        json!({"prompts":{"inbox":{"runtimes":{(slot.as_str()):{"reminder":text}}}}})
                    };
                    match save_extra(client, &patch) {
                        Ok(value) => {
                            merge(settings, &value);
                            return Ok("Prompt saved");
                        }
                        Err(cause) => {
                            error = format!(
                                "Not saved: {cause:#}. Your text remains here; edit or press Enter to retry."
                            );
                        }
                    }
                }
                ComposeAction::Edited => {}
            },
            Event::Paste(text) => compose.insert_text(&text),
            _ => {}
        }
    }
}
fn reset_prompt(
    client: &Client,
    terminal: &mut DefaultTerminal,
    settings: &mut Value,
    index: usize,
) -> Result<&'static str> {
    let (slot, label, _, custom) = prompts(settings).into_iter().nth(index).context("no prompt selected")?;
    ensure!(slot != "readonly" && custom, "selected prompt has no custom text to reset");
    ensure!(
        settings.pointer("/prompts/reminder/editable").and_then(Value::as_bool) != Some(false),
        "prompts are read-only for this Inbox user"
    );
    if !confirm(
        terminal,
        &format!("Reset {label}?"),
        "This deletes the custom text. Plannotator's default will be used again.",
    )? {
        return Ok("Prompt reset cancelled");
    }
    let patch = if slot == "guide" {
        json!({"prompts":{"guide":null}})
    } else if slot == "shared" {
        json!({"prompts":{"inbox":{"reminder":null}}})
    } else {
        json!({"prompts":{"inbox":{"runtimes":{(slot):{"reminder":null}}}}})
    };
    let response = save_extra(client, &patch)?;
    merge(settings, &response);
    Ok("Prompt reset to default")
}

fn change_agent(
    client: &Client,
    terminal: &mut DefaultTerminal,
    model: &mut Value,
    index: usize,
    key: KeyCode,
) -> Result<&'static str> {
    let row = model
        .get("agents")
        .and_then(Value::as_array)
        .and_then(|rows| rows.get(index))
        .context("read agent settings first")?;
    let id = row.get("id").and_then(Value::as_str).context("agent response omitted id")?;
    let field = match key {
        KeyCode::Char('g') => "reach",
        KeyCode::Char('m') => "reminder",
        KeyCode::Char('n') => "note",
        _ => anyhow::bail!("no agent action selected"),
    };
    let control = row
        .get(field)
        .filter(|control| control.is_object())
        .context("this agent does not expose that control")?;
    ensure!(
        control.get("locked").and_then(Value::as_bool) != Some(true),
        "this control is locked by the environment"
    );
    let current = if field == "note" {
        match control.get("state").and_then(Value::as_str) {
            Some("added" | "edited") => true,
            Some("absent") => false,
            _ => anyhow::bail!(
                "{}",
                control
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("this agent's note cannot be changed automatically")
            ),
        }
    } else {
        control.get("on").and_then(Value::as_bool).context("this agent does not expose that control")?
    };
    if field == "reach" || field == "note" {
        let verb = if current { "turn off" } else { "turn on" };
        let detail = if field == "reach" {
            "This adds or removes Plannotator's Inbox connection in the agent's configuration. A foreign entry is never replaced."
        } else {
            "This writes or removes Plannotator's marked Inbox note in the agent's instruction file."
        };
        if !confirm(
            terminal,
            &format!("{verb} {} {field}?", row.get("label").and_then(Value::as_str).unwrap_or(id)),
            detail,
        )? {
            return Ok("Agent change cancelled");
        }
    }
    let response: Value = client.post_json(
        "/api/inbox/agents",
        &json!({"agent":id,(field):!current,"serverSession":client.server_session()}),
    )?;
    *model = response;
    if let Some(error) = model.get("error").and_then(Value::as_str).filter(|error| !error.is_empty()) {
        anyhow::bail!("{error}");
    }
    Ok("Agent setting saved")
}
fn default_sidebar() -> Sidebar {
    Sidebar {
        top: SIDEBAR_ITEMS
            .iter()
            .filter(|(_, _, place)| *place == "top")
            .map(|(id, _, _)| (*id).to_owned())
            .collect(),
        bottom: SIDEBAR_ITEMS
            .iter()
            .filter(|(_, _, place)| *place == "bottom")
            .map(|(id, _, _)| (*id).to_owned())
            .collect(),
        hidden: Vec::new(),
    }
}

fn parse_sidebar(value: Option<&Value>) -> Option<Sidebar> {
    let value = value?;
    if value.is_null() {
        return Some(default_sidebar());
    }
    let ids = SIDEBAR_ITEMS.iter().map(|(id, _, _)| *id).collect::<Vec<_>>();
    let list = |key: &str| -> Option<Vec<String>> {
        let values = value.get(key)?.as_array()?;
        let mut out = Vec::new();
        for item in values {
            let id = item.as_str()?;
            if !ids.contains(&id) {
                return None;
            }
            out.push(id.to_owned());
        }
        Some(out)
    };
    Some(Sidebar { top: list("top")?, bottom: list("bottom")?, hidden: list("hidden")? })
}
fn sidebar_toggle(layout: &mut Sidebar, index: usize) -> Result<()> {
    let (id, _, _) = SIDEBAR_ITEMS.get(index).context("no sidebar item selected")?;
    ensure!(!matches!(*id, "inbox" | "settings"), "Inbox and Settings must remain visible");
    if layout.hidden.iter().any(|item| item == id) {
        layout.hidden.retain(|item| item != id);
    } else {
        layout.hidden.push((*id).into());
    }
    Ok(())
}
fn sidebar_move(layout: &mut Sidebar, index: usize, by: isize) -> Result<()> {
    let (id, _, _) = SIDEBAR_ITEMS.get(index).context("no sidebar item selected")?;
    let list = if layout.top.iter().any(|item| item == id) { &mut layout.top } else { &mut layout.bottom };
    let at = list.iter().position(|item| item == id).context("this item is not in a movable stack")?;
    let next = at
        .checked_add_signed(by)
        .filter(|next| *next < list.len())
        .context("item is already at the end of its stack")?;
    list.swap(at, next);
    Ok(())
}

fn reactions(settings: &Value) -> &[Value] {
    settings.get("reactions").and_then(Value::as_array).map_or(&[], Vec::as_slice)
}
fn reaction_change(client: &Client, settings: &mut Value, change: &Value) -> Result<()> {
    let response = save_extra(client, &json!({"reaction_change":change}))?;
    let list = response.get("reactions").context("reaction save response omitted reactions")?;
    ensure!(list.is_array(), "reaction save response was invalid");
    merge(settings, &response);
    Ok(())
}
fn add_reaction(client: &Client, terminal: &mut DefaultTerminal, settings: &mut Value) -> Result<String> {
    let emoji = edit_text(
        terminal,
        "Add reaction",
        "Enter one emoji. 👍 is built in and cannot be saved.",
        "",
        false,
        None,
    )?;
    let Some(emoji) = emoji else { return Ok("No reaction added".into()) };
    if emoji == "👍" {
        return Ok("👍 is already built in".into());
    }
    reaction_change(client, settings, &json!({"op":"add","emoji":emoji}))?;
    Ok("Reaction added".into())
}
fn edit_reaction(
    client: &Client,
    terminal: &mut DefaultTerminal,
    settings: &mut Value,
    index: usize,
    change: bool,
) -> Result<&'static str> {
    let reaction = reactions(settings).get(index).context("no reaction selected")?;
    let emoji = reaction.get("emoji").and_then(Value::as_str).context("reaction omitted emoji")?;
    let (title, initial) = if change {
        ("Change reaction", "")
    } else {
        ("Name reaction", reaction.get("label").and_then(Value::as_str).unwrap_or(""))
    };
    let Some(text) = edit_text(
        terminal,
        title,
        if change {
            "Enter one replacement emoji."
        } else {
            "Optional short label (blank removes the label)."
        },
        initial,
        false,
        if change { None } else { Some(24) },
    )?
    else {
        return Ok("Reaction edit cancelled");
    };
    let patch = if change {
        json!({"op":"change","emoji":emoji,"to":text})
    } else {
        json!({"op":"name","emoji":emoji,"label":text})
    };
    reaction_change(client, settings, &patch)?;
    Ok("Reaction saved")
}
fn remove_reaction(
    client: &Client,
    terminal: &mut DefaultTerminal,
    settings: &mut Value,
    index: usize,
) -> Result<String> {
    let Some(r) = reactions(settings).get(index).cloned() else {
        return Ok("No saved reaction selected".into());
    };
    let emoji = r.get("emoji").and_then(Value::as_str).context("reaction omitted emoji")?;
    if !confirm(terminal, "Remove saved reaction?", &format!("Remove {emoji} from the reaction tray?"))? {
        return Ok("Reaction kept".into());
    }
    reaction_change(client, settings, &json!({"op":"remove","emoji":emoji}))?;
    Ok("Reaction removed".into())
}

fn voice_action(
    client: &Client,
    terminal: &mut DefaultTerminal,
    settings: &mut Value,
    index: usize,
    status: &mut Option<Value>,
) -> Result<&'static str> {
    match index {
        0..=2 => {
            let field = match index {
                0 => "on",
                1 => "phones_only",
                _ => "engine",
            };
            let current = settings
                .get("voice")
                .and_then(|voice| voice.get(field))
                .context("voice setting is unavailable")?;
            let value = if field == "engine" {
                json!(match current.as_str() {
                    Some("api") => "codex",
                    Some("codex") => "api",
                    _ => anyhow::bail!("unknown voice engine"),
                })
            } else {
                json!(!current.as_bool().context("voice switch is unavailable")?)
            };
            let saved = save_extra(client, &json!({"voice":{(field):value}}))?;
            merge(settings, &saved);
            *status = None;
            Ok("Voice setting saved")
        }
        3 => {
            let message = voice_credential(client, terminal, settings)?;
            *status = None;
            Ok(message)
        }
        4 => {
            *status = Some(client.get_json("GET", "/api/inbox/voice/status?fresh=1")?);
            Ok("Voice status refreshed")
        }
        _ => anyhow::bail!("no voice setting selected"),
    }
}

fn voice_credential(
    client: &Client,
    terminal: &mut DefaultTerminal,
    settings: &mut Value,
) -> Result<&'static str> {
    let voice =
        settings.get_mut("voice").and_then(Value::as_object_mut).context("voice settings are unavailable")?;
    let source = voice.get("key").and_then(|key| key.get("source")).and_then(Value::as_str);
    ensure!(
        source != Some("env"),
        "the voice credential comes from the environment and cannot be removed here"
    );
    if source == Some("saved") {
        if !confirm(
            terminal,
            "Remove voice API key?",
            "This deletes the Inbox's saved voice credential. An environment-provided key is not changed.",
        )? {
            return Ok("Voice credential removal cancelled");
        }
        let response: Value = client.post_json(
            "/api/inbox/voice/key",
            &json!({"key":null,"serverSession":client.server_session()}),
        )?;
        let state = response.get("key").cloned().context("voice key response omitted key state")?;
        voice.insert("key".into(), state);
        return Ok("Saved voice credential removed");
    }
    let mut draft = String::new();
    let mut error = String::new();
    loop {
        let Some(key) = edit_text(
            terminal,
            "Save voice API key",
            &format!("The credential is sent only to this Inbox and never displayed. {error}"),
            &draft,
            true,
            None,
        )?
        else {
            return Ok("Voice credential edit cancelled");
        };
        if !confirm(
            terminal,
            "Save voice API key?",
            "The Inbox will store this credential for voice dictation. Confirm only if you intended to save it.",
        )? {
            draft = key;
            error = "Not saved. Confirm to retry or Esc to cancel.".into();
            continue;
        }
        match client.post_json::<Value>(
            "/api/inbox/voice/key",
            &json!({"key":key,"serverSession":client.server_session()}),
        ) {
            Ok(response) => {
                let state = response.get("key").cloned().context("voice key response omitted key state")?;
                voice.insert("key".into(), state);
                return Ok("Voice credential saved");
            }
            Err(cause) => {
                draft = key;
                error = format!(
                    "Not saved: {cause:#}. The value remains in the masked editor; press Enter to retry."
                );
            }
        }
    }
}

fn edit_text(
    terminal: &mut DefaultTerminal,
    title: &str,
    detail: &str,
    initial: &str,
    secret: bool,
    max: Option<usize>,
) -> Result<Option<String>> {
    let mut compose = Compose::with_text(initial);
    let mut error = String::new();
    loop {
        draw_editor(
            terminal,
            title,
            &compose.value(),
            secret,
            max.unwrap_or(usize::MAX),
            &format!("{detail} {error}"),
        )?;
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match compose.handle_key(key) {
                ComposeAction::Cancel => return Ok(None),
                ComposeAction::Save => {
                    let value = compose.value();
                    if value.chars().count() > max.unwrap_or(usize::MAX) {
                        error =
                            format!("{} characters exceeds the limit; text retained.", value.chars().count());
                        continue;
                    }
                    return Ok(Some(value));
                }
                ComposeAction::Edited => {}
            },
            Event::Paste(text) => compose.insert_text(&text),
            _ => {}
        }
    }
}
fn draw_editor(
    terminal: &mut DefaultTerminal,
    title: &str,
    text: &str,
    secret: bool,
    max: usize,
    error: &str,
) -> Result<()> {
    terminal.draw(|frame| {
        let [detail, field, help] =
            Layout::vertical([Constraint::Length(4), Constraint::Min(3), Constraint::Length(2)])
                .areas(frame.area());
        frame.render_widget(Paragraph::new(error).wrap(Wrap { trim: false }), detail);
        let shown = if secret { "•".repeat(text.chars().count()) } else { text.to_owned() };
        let block = Block::default().borders(Borders::ALL).title(format!(
            "{title} · {}/{}",
            text.chars().count(),
            if max == usize::MAX { "—".into() } else { max.to_string() }
        ));
        let inner = block.inner(field);
        let (lines, row, col) = Compose::with_text(&shown).wrapped(usize::from(inner.width).max(1));
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
        frame.render_widget(
            Paragraph::new("Enter save · Shift+Enter newline · Esc cancel · Paste supported"),
            help,
        );
    })?;
    Ok(())
}
fn confirm(terminal: &mut DefaultTerminal, title: &str, detail: &str) -> Result<bool> {
    loop {
        terminal.draw(|frame| {
            frame.render_widget(
                Paragraph::new(format!("{detail}\n\nEnter or y confirms; Esc or n cancels."))
                    .wrap(Wrap { trim: false })
                    .block(Block::default().borders(Borders::ALL).title(title)),
                frame.area(),
            );
        })?;
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Enter | KeyCode::Char('y') => return Ok(true),
                KeyCode::Esc | KeyCode::Char('n') => return Ok(false),
                _ => {}
            },
            _ => {}
        }
    }
}

fn account_target(index: usize, machines: Option<&Value>) -> Option<(String, String, String)> {
    if index == 0 {
        return Some(("self".into(), "/api/inbox/accounts".into(), "this computer".into()));
    }
    let machine = machines?.get("machines")?.as_array()?.get(index.checked_sub(1)?)?;
    let id = super::client::path_id(machine.get("id")?.as_str()?).ok()?;
    let name = machine.get("name").and_then(Value::as_str).unwrap_or(id);
    Some((id.into(), format!("/api/inbox/accounts/{id}"), name.into()))
}
fn account_rows(accounts: &BTreeMap<String, Value>, machines: Option<&Value>) -> Vec<String> {
    let mut out = vec![format!(
        "This computer  ·  {}",
        accounts.get("self").map_or("not read", |v| v.get("state").and_then(Value::as_str).unwrap_or("read"))
    )];
    if let Some(list) = machines.and_then(|v| v.get("machines")).and_then(Value::as_array) {
        for m in list {
            let id = m.get("id").and_then(Value::as_str).unwrap_or("");
            let name = m.get("name").and_then(Value::as_str).unwrap_or(id);
            out.push(format!(
                "{name}  ·  {}",
                accounts
                    .get(id)
                    .map_or("not read", |v| v.get("state").and_then(Value::as_str).unwrap_or("read"))
            ));
        }
    }
    out
}
fn account_detail(value: &Value) -> String {
    let Some(list) = value.get("accounts").and_then(Value::as_array) else {
        return format!(
            "Account read state: {}\nNo account switch is available in this server.",
            value.get("state").and_then(Value::as_str).unwrap_or("unknown")
        );
    };
    if list.is_empty() {
        return "No accounts returned.".into();
    }
    list.iter()
        .map(|a| {
            format!(
                "{}{}{}  ·  {}",
                if a.get("active").and_then(Value::as_bool) == Some(true) { "● " } else { "  " },
                a.get("alias").and_then(Value::as_str).unwrap_or(""),
                a.get("email").and_then(Value::as_str).map_or(String::new(), |e| format!(" ({e})")),
                a.pointer("/usage/fiveHour/pct")
                    .and_then(Value::as_f64)
                    .map_or("usage unavailable".into(), |n| format!("{n:.0}% five-hour usage"))
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}
fn voice_key_label(settings: &Value) -> String {
    let key = settings.pointer("/voice/key");
    match key.and_then(|v| v.get("source")).and_then(Value::as_str) {
        Some("saved") => key
            .and_then(|v| v.get("last4"))
            .and_then(Value::as_str)
            .map_or_else(|| "saved".into(), |suffix| format!("saved · ending {suffix}")),
        Some("env") => "environment (read-only)".into(),
        _ => "not configured".into(),
    }
}
fn bool_label(value: Option<&Value>) -> &'static str {
    if value.and_then(Value::as_bool) == Some(true) {
        "On"
    } else if value.and_then(Value::as_bool) == Some(false) {
        "Off"
    } else {
        "not reported"
    }
}
fn stamp(value: Option<&Value>) -> &str {
    value.and_then(Value::as_str).unwrap_or("not recorded")
}
fn merge(current: &mut Value, next: &Value) {
    if let (Some(dst), Some(src)) = (current.as_object_mut(), next.as_object()) {
        for (k, v) in src {
            if let (Some(old), Some(new)) = (dst.get_mut(k), v.as_object())
                && let Some(old) = old.as_object_mut()
            {
                old.extend(new.iter().map(|(k, v)| (k.clone(), v.clone())));
                continue;
            }
            dst.insert(k.clone(), v.clone());
        }
    }
}
