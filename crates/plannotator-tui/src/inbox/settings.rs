//! The config-backed part of Inbox Settings: a read-only GET followed by small, explicit POST patches.
//!
//! `SettingsPane` is a panel, not a second event loop. Its caller owns routing and requests: draw it in the chosen
//! region, pass keys to [`SettingsPane::handle_key`], perform [`SettingsAction::Save`] through [`write`], then give
//! the returned object to [`SettingsPane::saved`]. Rows unavailable on an older server are omitted; locked rows stay
//! visible with their reason. The reader and writer use only the documented `/api/inbox/settings` contract.

use anyhow::{Context as _, Result, ensure};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use serde_json::{Map, Value, json};

use super::client::Client;
use crate::theme::palette;

const ENDPOINT: &str = "/api/inbox/settings?store=0";

/// A configuration row's group. These are the config-backed Settings web sections, not the data/device panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsSection {
    General,
    Agents,
    Capabilities,
    Diff,
    Rows,
    Updates,
    Voice,
}

impl SettingsSection {
    const ALL: [Self; 7] =
        [Self::General, Self::Agents, Self::Capabilities, Self::Diff, Self::Rows, Self::Updates, Self::Voice];

    const fn title(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Agents => "Agents",
            Self::Capabilities => "Capabilities",
            Self::Diff => "Diff display",
            Self::Rows => "Rows",
            Self::Updates => "Updates",
            Self::Voice => "Voice",
        }
    }
}

/// What the panel asks its owner to do. A Save is one validated field or an atomic row-look update.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SettingsAction {
    None,
    Close,
    Refresh,
    Save(Value),
}

#[derive(Debug, Clone, Copy)]
enum Choices {
    Bool,
    Strings(&'static [&'static str]),
    Numbers(&'static [u64]),
    Row(&'static str),
}

#[derive(Debug, Clone, Copy)]
struct Setting {
    section: SettingsSection,
    /// The server's read field, except nested switches whose wire patch is formed explicitly below.
    path: &'static str,
    label: &'static str,
    choices: Choices,
}

const PALETTE_IDS: &[&str] = &[
    "plannotator",
    "plannotator-graphite",
    "plannotator-modern",
    "simple",
    "claude-plus",
    "adwaita",
    "andromeeda",
    "aurora-x",
    "ayu-dark",
    "caffeine",
    "catppuccin",
    "cursor-hc",
    "cursor",
    "colorblind",
    "dark-plus",
    "doom-64",
    "dracula",
    "everforest",
    "everforest-hard",
    "everforest-soft",
    "github",
    "gruvbox",
    "houston",
    "kanagawa-dragon",
    "kanagawa-lotus",
    "kanagawa-wave",
    "laserwave",
    "material",
    "cursor-midnight",
    "min",
    "monokai-pro",
    "night-owl",
    "nord",
    "one-dark-pro",
    "one-light",
    "paulmillr",
    "plastic",
    "poimandres",
    "quantum-rose",
    "red",
    "rose-pine",
    "slack",
    "snazzy-light",
    "soft-pop",
    "solar-dusk",
    "solarized",
    "synthwave-84",
    "terminal",
    "tinacious",
    "tokyo-night",
    "tokyo-night-moon",
    "vesper",
    "vitesse",
    "vitesse-black",
    "neutral",
];

const SETTINGS: &[Setting] = &[
    Setting {
        section: SettingsSection::General,
        path: "theme.light",
        label: "Light palette",
        choices: Choices::Strings(PALETTE_IDS),
    },
    Setting {
        section: SettingsSection::General,
        path: "theme.dark",
        label: "Dark palette",
        choices: Choices::Strings(PALETTE_IDS),
    },
    Setting {
        section: SettingsSection::General,
        path: "theme.mode",
        label: "Appearance mode",
        choices: Choices::Strings(&["system", "light", "dark"]),
    },
    Setting {
        section: SettingsSection::General,
        path: "text_size",
        label: "Text size (px)",
        choices: Choices::Numbers(&[12, 13, 14, 15]),
    },
    Setting {
        section: SettingsSection::General,
        path: "favicon",
        label: "Favicon",
        choices: Choices::Strings(&["glass", "yellow", "totman"]),
    },
    Setting {
        section: SettingsSection::General,
        path: "notifications.enabled",
        label: "Desktop notifications preference",
        choices: Choices::Bool,
    },
    Setting {
        section: SettingsSection::General,
        path: "sound.enabled",
        label: "Question sound",
        choices: Choices::Bool,
    },
    Setting {
        section: SettingsSection::General,
        path: "sound.volume",
        label: "Question sound volume",
        choices: Choices::Numbers(&[10, 20, 30, 40, 50, 60, 70, 80, 90, 100]),
    },
    Setting {
        section: SettingsSection::General,
        path: "cookie_host.on",
        label: "Open at plannotator.localhost",
        choices: Choices::Bool,
    },
    Setting {
        section: SettingsSection::General,
        path: "tailscale.on",
        label: "Serve over your tailnet",
        choices: Choices::Bool,
    },
    Setting {
        section: SettingsSection::General,
        path: "mark_read_warning.on",
        label: "Warn before Mark read sets questions aside",
        choices: Choices::Bool,
    },
    Setting {
        section: SettingsSection::Agents,
        path: "inbox_tool.hosts.claude-code",
        label: "Inbox tool · Claude Code",
        choices: Choices::Bool,
    },
    Setting {
        section: SettingsSection::Agents,
        path: "inbox_tool.hosts.pi",
        label: "Inbox tool · Pi",
        choices: Choices::Bool,
    },
    Setting {
        section: SettingsSection::Agents,
        path: "inbox_tool.hosts.opencode",
        label: "Inbox tool · OpenCode",
        choices: Choices::Bool,
    },
    Setting {
        section: SettingsSection::Agents,
        path: "inbox_tool.hosts.dsh",
        label: "Inbox tool · DeepSeek Harness",
        choices: Choices::Bool,
    },
    Setting {
        section: SettingsSection::Agents,
        path: "inbox_tool.hosts.cline",
        label: "Inbox tool · Cline",
        choices: Choices::Bool,
    },
    Setting {
        section: SettingsSection::Capabilities,
        path: "board.on",
        label: "Board",
        choices: Choices::Bool,
    },
    Setting {
        section: SettingsSection::Capabilities,
        path: "steps.on",
        label: "Checklist",
        choices: Choices::Bool,
    },
    Setting {
        section: SettingsSection::Capabilities,
        path: "bro.on",
        label: "Bro mode",
        choices: Choices::Bool,
    },
    Setting {
        section: SettingsSection::Capabilities,
        path: "float_drafts.on",
        label: "Float reply drafts",
        choices: Choices::Bool,
    },
    Setting {
        section: SettingsSection::Capabilities,
        path: "code_review.on",
        label: "Code review",
        choices: Choices::Bool,
    },
    Setting {
        section: SettingsSection::Capabilities,
        path: "shape.on",
        label: "Shape",
        choices: Choices::Bool,
    },
    Setting {
        section: SettingsSection::Capabilities,
        path: "system.on",
        label: "System",
        choices: Choices::Bool,
    },
    Setting {
        section: SettingsSection::Capabilities,
        path: "chapter_diagrams.on",
        label: "Chapter diagrams",
        choices: Choices::Bool,
    },
    Setting {
        section: SettingsSection::Capabilities,
        path: "experimental.decision_mode.on",
        label: "Experimental · Decision mode",
        choices: Choices::Bool,
    },
    Setting {
        section: SettingsSection::Diff,
        path: "diff_options.diff_style",
        label: "Layout",
        choices: Choices::Strings(&["split", "unified"]),
    },
    Setting {
        section: SettingsSection::Diff,
        path: "diff_options.overflow",
        label: "Long lines",
        choices: Choices::Strings(&["scroll", "wrap"]),
    },
    Setting {
        section: SettingsSection::Diff,
        path: "diff_options.indicators",
        label: "Change indicators",
        choices: Choices::Strings(&["bars", "classic", "none"]),
    },
    Setting {
        section: SettingsSection::Diff,
        path: "diff_options.line_diff_type",
        label: "Inline changes",
        choices: Choices::Strings(&["word-alt", "word", "char", "none"]),
    },
    Setting {
        section: SettingsSection::Diff,
        path: "diff_options.line_numbers",
        label: "Line numbers",
        choices: Choices::Bool,
    },
    Setting {
        section: SettingsSection::Diff,
        path: "diff_options.background",
        label: "Change background",
        choices: Choices::Bool,
    },
    Setting {
        section: SettingsSection::Diff,
        path: "diff_options.background_intensity",
        label: "Background intensity",
        choices: Choices::Strings(&["subtle", "normal", "strong"]),
    },
    Setting {
        section: SettingsSection::Diff,
        path: "diff_options.tab_size",
        label: "Tab size",
        choices: Choices::Numbers(&[1, 2, 3, 4, 5, 6, 7, 8]),
    },
    Setting {
        section: SettingsSection::Rows,
        path: "rows",
        label: "Highlight new activity",
        choices: Choices::Row("highlight"),
    },
    Setting {
        section: SettingsSection::Rows,
        path: "rows",
        label: "Compact rows",
        choices: Choices::Row("compact"),
    },
    Setting {
        section: SettingsSection::Rows,
        path: "rows",
        label: "Hide agent names",
        choices: Choices::Row("hideAgents"),
    },
    Setting {
        section: SettingsSection::Rows,
        path: "rows",
        label: "Smaller thread names",
        choices: Choices::Row("smallNames"),
    },
    Setting {
        section: SettingsSection::Rows,
        path: "rows",
        label: "Hide project names",
        choices: Choices::Row("hideProject"),
    },
    Setting {
        section: SettingsSection::Rows,
        path: "rows",
        label: "Hide previews",
        choices: Choices::Row("hidePreview"),
    },
    Setting {
        section: SettingsSection::Rows,
        path: "rows",
        label: "Roomy rows",
        choices: Choices::Row("roomy"),
    },
    Setting {
        section: SettingsSection::Rows,
        path: "rows",
        label: "Unread dot",
        choices: Choices::Row("unreadDot"),
    },
    Setting {
        section: SettingsSection::Rows,
        path: "rows",
        label: "Two-line previews",
        choices: Choices::Row("previewTwo"),
    },
    Setting {
        section: SettingsSection::Rows,
        path: "rows",
        label: "Relative time",
        choices: Choices::Row("relativeTime"),
    },
    Setting {
        section: SettingsSection::Rows,
        path: "rows",
        label: "Title first",
        choices: Choices::Row("titleFirst"),
    },
    Setting {
        section: SettingsSection::Rows,
        path: "rows",
        label: "Thread letter marks",
        choices: Choices::Row("marksThreads"),
    },
    Setting {
        section: SettingsSection::Rows,
        path: "rows",
        label: "Project letter marks",
        choices: Choices::Row("marksProjects"),
    },
    Setting {
        section: SettingsSection::Rows,
        path: "rows",
        label: "Inline letter marks",
        choices: Choices::Row("markInline"),
    },
    Setting {
        section: SettingsSection::Rows,
        path: "rows",
        label: "Dot letter marks",
        choices: Choices::Row("markDot"),
    },
    Setting {
        section: SettingsSection::Rows,
        path: "rows",
        label: "Repository logo",
        choices: Choices::Row("repoLogo"),
    },
    Setting {
        section: SettingsSection::Rows,
        path: "rows",
        label: "GitHub profile pictures",
        choices: Choices::Row("githubPictures"),
    },
    Setting {
        section: SettingsSection::Rows,
        path: "rows",
        label: "Time below preview",
        choices: Choices::Row("timeBelow"),
    },
    Setting {
        section: SettingsSection::Rows,
        path: "rows",
        label: "Row dividers",
        choices: Choices::Row("dividers"),
    },
    Setting {
        section: SettingsSection::Rows,
        path: "rows",
        label: "Status mark",
        choices: Choices::Row("statusMark"),
    },
    Setting {
        section: SettingsSection::Rows,
        path: "rows",
        label: "Bold thread names",
        choices: Choices::Row("boldNames"),
    },
    Setting {
        section: SettingsSection::Rows,
        path: "rows",
        label: "Hide pull request marks",
        choices: Choices::Row("hidePulls"),
    },
    Setting {
        section: SettingsSection::Updates,
        path: "updates.update_check.on",
        label: "Check for updates",
        choices: Choices::Bool,
    },
    Setting {
        section: SettingsSection::Updates,
        path: "updates.auto_update.on",
        label: "Install updates automatically",
        choices: Choices::Bool,
    },
    Setting {
        section: SettingsSection::Voice,
        path: "voice.on",
        label: "Voice dictation",
        choices: Choices::Bool,
    },
    Setting {
        section: SettingsSection::Voice,
        path: "voice.phones_only",
        label: "Voice on phones only",
        choices: Choices::Bool,
    },
    Setting {
        section: SettingsSection::Voice,
        path: "voice.engine",
        label: "Voice engine",
        choices: Choices::Strings(&["codex", "api"]),
    },
];

/// Read Settings without the Storage page's potentially expensive disk walk.
pub(crate) fn read(client: &Client) -> Result<Value> {
    let model: Value = client.get_json("GET", ENDPOINT).context("reading Inbox settings")?;
    ensure!(
        model.get("serverSession").and_then(Value::as_str) == Some(client.server_session()),
        "GET {}: Inbox serverSession changed; rediscover before continuing",
        client.url()
    );
    ensure!(model.is_object(), "GET {}: settings response was not an object", client.url());
    Ok(model)
}

/// POST one supported setting change. The server checks the same schema, ownership and environment locks as the web UI.
pub(crate) fn write(client: &Client, patch: &Value) -> Result<Value> {
    validate_patch(patch)?;
    let mut body = patch.as_object().cloned().context("settings patch must be an object")?;
    body.insert("serverSession".to_owned(), Value::String(client.server_session().to_owned()));
    let result: Value =
        client.post_json("/api/inbox/settings", &Value::Object(body)).context("saving Inbox settings")?;
    ensure!(result.is_object(), "POST /api/inbox/settings: response was not an object");
    Ok(result)
}

/// A stateful, embeddable Settings list. It intentionally contains no terminal ownership or event loop.
#[derive(Debug, Clone)]
pub(crate) struct SettingsPane {
    model: Value,
    section: usize,
    selected: usize,
    status: Option<String>,
}

impl SettingsPane {
    pub(crate) fn load(client: &Client) -> Result<Self> {
        Ok(Self::new(read(client)?))
    }

    pub(crate) fn new(model: Value) -> Self {
        Self { model, section: 0, selected: 0, status: None }
    }

    pub(crate) fn section(&self) -> SettingsSection {
        SettingsSection::ALL.get(self.section).copied().unwrap_or(SettingsSection::General)
    }

    /// Replace the read model after a successful settings write.
    pub(crate) fn replace(&mut self, response: Value) {
        self.saved(response);
    }

    /// Merge a successful settings write's response into the read model.
    pub(crate) fn saved(&mut self, response: Value) {
        if let (Some(current), Value::Object(next)) = (self.model.as_object_mut(), response) {
            for (key, value) in next {
                match (current.get_mut(&key), value) {
                    (Some(Value::Object(old)), Value::Object(new)) => old.extend(new),
                    (_, value) => {
                        current.insert(key, value);
                    }
                }
            }
        }
        self.status = Some("Saved".to_owned());
    }

    pub(crate) fn refresh(&mut self, client: &Client) -> Result<()> {
        self.model = read(client)?;
        self.selected = self.selected.min(self.rows().len().saturating_sub(1));
        self.status = Some("Settings refreshed".to_owned());
        Ok(())
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> SettingsAction {
        if key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::ALT) {
            return SettingsAction::None;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => SettingsAction::Close,
            KeyCode::Tab | KeyCode::Right | KeyCode::PageDown => {
                self.next_section(1);
                SettingsAction::None
            }
            KeyCode::BackTab | KeyCode::PageUp => {
                self.next_section(-1);
                SettingsAction::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                SettingsAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                SettingsAction::None
            }
            KeyCode::Char('r') => SettingsAction::Refresh,
            KeyCode::Enter | KeyCode::Char(' ') => self.change(1),
            KeyCode::Left => self.change(-1),
            _ => SettingsAction::None,
        }
    }

    pub(crate) fn draw(&self, frame: &mut Frame, area: Rect) {
        let [heading, body, footer] =
            Layout::vertical([Constraint::Length(2), Constraint::Min(1), Constraint::Length(1)]).areas(area);
        let section = self.section();
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("Settings  ", Style::new().fg(palette().accent).add_modifier(Modifier::BOLD)),
                Span::styled(section.title(), Style::new().add_modifier(Modifier::BOLD)),
                Span::raw("  ·  "),
                Span::styled(
                    format!("{}/{}", self.section + 1, SettingsSection::ALL.len()),
                    Style::new().dim(),
                ),
            ])),
            heading,
        );
        let rows = self.rows();
        if rows.is_empty() {
            frame.render_widget(
                Paragraph::new("This Inbox does not expose settings in this section.")
                    .style(Style::new().dim()),
                body,
            );
        } else {
            let selected = self.selected.min(rows.len().saturating_sub(1));
            let lines: Vec<Line> = rows
                .iter()
                .enumerate()
                .map(|(index, setting)| {
                    let (value, locked) = self.describe(setting);
                    let row_style = if locked.is_some() { Style::new().dim() } else { Style::default() };
                    let line = Line::from(vec![
                        Span::styled(
                            if index == selected { "› " } else { "  " },
                            Style::new().fg(palette().accent),
                        ),
                        Span::styled(
                            setting.label,
                            row_style.add_modifier(if index == selected {
                                Modifier::BOLD
                            } else {
                                Modifier::empty()
                            }),
                        ),
                        Span::raw("  "),
                        Span::styled(
                            value,
                            if locked.is_some() {
                                Style::new().dim()
                            } else {
                                Style::new().fg(palette().comment)
                            },
                        ),
                        locked.map_or_else(
                            || Span::raw(""),
                            |reason| Span::styled(format!("  ({reason})"), Style::new().fg(palette().delete)),
                        ),
                    ]);
                    if index == selected { line.patch_style(palette().selection) } else { line }
                })
                .collect();
            frame.render_widget(
                Paragraph::new(lines)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_type(ratatui::widgets::BorderType::Rounded),
                    )
                    .wrap(Wrap { trim: false }),
                body,
            );
        }
        let status =
            self.status.as_deref().unwrap_or("↑↓ move · ←/enter change · tab group · r refresh · esc close");
        frame.render_widget(Paragraph::new(status).style(Style::new().fg(palette().idle_fg)), footer);
    }

    fn rows(&self) -> Vec<Setting> {
        SETTINGS
            .iter()
            .copied()
            .filter(|setting| {
                setting.section == self.section()
                    && (setting.path == "rows" || self.value(setting.path).is_some())
            })
            .collect()
    }

    fn value(&self, path: &str) -> Option<&Value> {
        let mut cursor = &self.model;
        for part in path.split('.') {
            cursor = cursor.get(part)?;
        }
        Some(cursor)
    }

    fn describe(&self, setting: &Setting) -> (String, Option<&'static str>) {
        let value = self.setting_value(setting);
        let locked = self.lock_reason(setting);
        let shown = match &setting.choices {
            Choices::Bool => {
                if value.as_bool().unwrap_or(false) {
                    "On".to_owned()
                } else {
                    "Off".to_owned()
                }
            }
            Choices::Strings(choices) => value.as_str().map_or_else(
                || {
                    choices
                        .first()
                        .map_or_else(|| "Default".to_owned(), |value| choice_label(value).to_owned())
                },
                |s| choice_label(s).to_owned(),
            ),
            Choices::Numbers(_) => value.as_u64().map_or_else(|| "Default".to_owned(), |n| n.to_string()),
            Choices::Row(option) => {
                if self.rows_values().iter().any(|v| v == option) {
                    "On".to_owned()
                } else {
                    "Off".to_owned()
                }
            }
        };
        (shown, locked)
    }

    fn setting_value(&self, setting: &Setting) -> Value {
        if setting.path == "rows" {
            return Value::Null;
        }
        let path = if setting.path == "experimental.decision_mode.on" {
            "experimental.decision_mode.on"
        } else {
            setting.path
        };
        self.value(path).cloned().unwrap_or(Value::Null)
    }

    fn lock_reason(&self, setting: &Setting) -> Option<&'static str> {
        let env_bool = |path: &str| self.value(path).is_some_and(|value| !value.is_null());
        match setting.path {
            "inbox_tool.hosts.claude-code"
            | "inbox_tool.hosts.pi"
            | "inbox_tool.hosts.opencode"
            | "inbox_tool.hosts.dsh"
            | "inbox_tool.hosts.cline"
                if env_bool("inbox_tool.env") =>
            {
                Some("PLANNOTATOR_INBOX_TOOL decides")
            }
            "experimental.decision_mode.on" if env_bool("experimental.decision_mode.env") => {
                Some("PLANNOTATOR_EXPERIMENTAL decides")
            }
            "updates.update_check.on" if env_bool("updates.update_check.env") => Some("environment decides"),
            "updates.auto_update.on" if env_bool("updates.auto_update.env") => Some("environment decides"),
            "cookie_host.on" if env_bool("cookie_host.env") || env_bool("cookie_host.url_host") => {
                Some("environment/config decides")
            }
            "tailscale.on"
                if self
                    .value("tailscale.source")
                    .and_then(Value::as_str)
                    .is_some_and(|source| source == "flag" || source == "env") =>
            {
                Some("environment/flag decides")
            }
            _ if self.model.get("guest").and_then(Value::as_bool) == Some(true)
                && owner_only(setting.path) =>
            {
                Some("owner only")
            }
            _ => None,
        }
    }

    fn rows_values(&self) -> Vec<String> {
        let mut rows = self.model.get("rows").and_then(Value::as_array).map_or_else(
            || vec!["smallNames".to_owned()],
            |values| values.iter().filter_map(Value::as_str).map(str::to_owned).collect::<Vec<_>>(),
        );
        canonical_rows(&mut rows);
        rows
    }

    fn change(&mut self, direction: isize) -> SettingsAction {
        let rows = self.rows();
        let Some(setting) = rows.get(self.selected).copied() else {
            self.status = Some("No settings in this group".to_owned());
            return SettingsAction::None;
        };
        if let Some(reason) = self.lock_reason(&setting) {
            self.status = Some(format!("Locked: {reason}"));
            return SettingsAction::None;
        }
        let next = match setting.choices {
            Choices::Bool => json!(!self.setting_value(&setting).as_bool().unwrap_or(false)),
            Choices::Strings(values) => {
                let current_value = self.setting_value(&setting);
                let current = current_value.as_str();
                let index = values
                    .iter()
                    .position(|value| Some(*value) == current)
                    .unwrap_or(if direction > 0 { values.len() - 1 } else { 0 });
                let next_index = ((index as isize + direction).rem_euclid(values.len() as isize)) as usize;
                let Some(value) = values.get(next_index) else {
                    return SettingsAction::None;
                };
                json!(value)
            }
            Choices::Numbers(values) => {
                let current = self.setting_value(&setting).as_u64();
                let index = values
                    .iter()
                    .position(|value| Some(*value) == current)
                    .unwrap_or(if direction > 0 { 0 } else { values.len() - 1 });
                let next_index = ((index as isize + direction).rem_euclid(values.len() as isize)) as usize;
                let Some(value) = values.get(next_index) else {
                    return SettingsAction::None;
                };
                json!(value)
            }
            Choices::Row(option) => {
                let mut rows = self.rows_values();
                if rows.iter().any(|value| value == option) {
                    rows.retain(|value| value != option);
                } else {
                    rows.push(option.to_owned());
                }
                canonical_rows(&mut rows);
                json!({"rows": rows})
            }
        };
        let patch = if setting.path == "rows" { next } else { patch_for(&self.model, setting.path, next) };
        if let Err(message) = validate_patch(&patch) {
            self.status = Some(format!("Cannot save: {message}"));
            return SettingsAction::None;
        }
        SettingsAction::Save(patch)
    }

    fn move_selection(&mut self, direction: isize) {
        let count = self.rows().len();
        if count != 0 {
            self.selected = self.selected.saturating_add_signed(direction).min(count - 1);
        }
        self.status = None;
    }

    fn next_section(&mut self, direction: isize) {
        self.section =
            (self.section as isize + direction).rem_euclid(SettingsSection::ALL.len() as isize) as usize;
        self.selected = 0;
        self.status = None;
    }
}

fn choice_label(value: &str) -> &str {
    match value {
        "system" => "Match system",
        "light" => "Light",
        "dark" => "Dark",
        "totman" => "Face",
        "word-alt" => "Word (alternate)",
        "word" => "Word",
        "char" => "Character",
        "none" => "None",
        "bars" => "Bars",
        "classic" => "Classic",
        "subtle" => "Subtle",
        "normal" => "Normal",
        "strong" => "Strong",
        "scroll" => "Scroll",
        "wrap" => "Wrap",
        "codex" => "Codex login",
        "api" => "OpenAI API key",
        _ => value,
    }
}

fn owner_only(path: &str) -> bool {
    matches!(
        path,
        "inbox_tool.hosts.claude-code"
            | "inbox_tool.hosts.pi"
            | "inbox_tool.hosts.opencode"
            | "inbox_tool.hosts.dsh"
            | "inbox_tool.hosts.cline"
            | "board.on"
            | "steps.on"
            | "bro.on"
            | "float_drafts.on"
            | "mark_read_warning.on"
            | "code_review.on"
            | "shape.on"
            | "system.on"
            | "chapter_diagrams.on"
            | "cookie_host.on"
            | "tailscale.on"
            | "voice.on"
            | "voice.phones_only"
            | "voice.engine"
    )
}

fn patch_for(model: &Value, path: &str, value: Value) -> Value {
    if let Some(field) = path.strip_prefix("theme.") {
        let mut theme = model.get("theme").and_then(Value::as_object).cloned().unwrap_or_default();
        theme.insert(field.to_owned(), value);
        json!({ "theme": Value::Object(theme) })
    } else if path == "experimental.decision_mode.on" {
        json!({ "experimental": { "decision_mode": value } })
    } else if path == "tailscale.on" {
        json!({ "tailscale": { "on": value } })
    } else if path == "cookie_host.on" {
        json!({ "cookie_host": value })
    } else if path == "notifications.enabled" {
        json!({ "notifications": { "enabled": value } })
    } else if path.starts_with("sound.") {
        let field = path.strip_prefix("sound.").unwrap_or_default();
        json!({ "sound": { (field): value } })
    } else if path.starts_with("inbox_tool.hosts.") {
        let host = path.strip_prefix("inbox_tool.hosts.").unwrap_or_default();
        json!({ "inbox_tool": { (host): value } })
    } else if path.starts_with("diff_options.") {
        let field = path.strip_prefix("diff_options.").unwrap_or_default();
        json!({ "diff_options": { (field): value } })
    } else if path.starts_with("updates.") {
        let field = path.strip_prefix("updates.").and_then(|s| s.split('.').next()).unwrap_or_default();
        json!({ (field): value })
    } else if path.starts_with("voice.") {
        let field = path.strip_prefix("voice.").unwrap_or_default();
        json!({ "voice": { (field): value } })
    } else if let Some((field, _)) = path.split_once('.') {
        json!({ (field): { "on": value } })
    } else {
        let mut object = Map::new();
        object.insert(path.to_owned(), value);
        Value::Object(object)
    }
}

fn canonical_rows(rows: &mut Vec<String>) {
    rows.sort();
    rows.dedup();
    if rows.iter().any(|row| row == "compact") {
        rows.retain(|row| row != "roomy");
    }
    if rows.iter().any(|row| row == "hidePreview") {
        rows.retain(|row| row != "previewTwo");
    }
    if rows.iter().any(|row| row == "unreadDot") && !rows.iter().any(|row| row == "smallNames") {
        rows.push("smallNames".to_owned());
    }
    if rows.iter().any(|row| row == "markInline") {
        rows.retain(|row| row != "markDot");
    }
    if !rows.iter().any(|row| row == "marksThreads" || row == "marksProjects") {
        rows.retain(|row| row != "markInline" && row != "markDot");
    }
    if !rows.iter().any(|row| row == "marksProjects") {
        rows.retain(|row| row != "repoLogo" && row != "githubPictures");
    }
    rows.sort();
}

fn validate_patch(patch: &Value) -> Result<()> {
    let object = patch.as_object().context("settings patch must be an object")?;
    ensure!(!object.is_empty(), "settings patch is empty");
    for (key, value) in object {
        match key.as_str() {
            "theme" => {
                let theme = value.as_object().context("theme must be an object")?;
                ensure!(!theme.is_empty(), "theme patch is empty");
                for (field, setting) in theme {
                    match field.as_str() {
                        "mode" => ensure!(
                            matches!(setting.as_str(), Some("light" | "dark" | "system")),
                            "theme.mode must be light, dark or system"
                        ),
                        "light" | "dark" => ensure!(
                            setting.as_str().is_some_and(valid_theme_id),
                            "theme.{field} must be a known palette id"
                        ),
                        _ => anyhow::bail!("theme.{field} is not a supported setting"),
                    }
                }
            }
            "text_size" => {
                ensure!(matches!(value.as_u64(), Some(12..=15)), "text_size must be 12, 13, 14 or 15");
            }
            "favicon" => ensure!(
                matches!(value.as_str(), Some("glass" | "yellow" | "totman")),
                "favicon must be glass, yellow or totman"
            ),
            "notifications" => {
                let settings = value.as_object().context("notifications must be an object")?;
                ensure!(
                    settings.len() == 1 && settings.get("enabled").is_some_and(Value::is_boolean),
                    "notifications supports only {{ enabled: boolean }}"
                );
            }
            "sound" => {
                let settings = value.as_object().context("sound must be an object")?;
                ensure!(!settings.is_empty(), "sound patch is empty");
                for (field, setting) in settings {
                    match field.as_str() {
                        "enabled" => ensure!(setting.is_boolean(), "sound.enabled must be boolean"),
                        "volume" => ensure!(
                            matches!(setting.as_u64(), Some(10..=100)),
                            "sound.volume must be an integer from 10 to 100"
                        ),
                        _ => anyhow::bail!("sound.{field} is not a supported setting"),
                    }
                }
            }
            "cookie_host" => ensure!(value.is_boolean(), "cookie_host must be boolean"),
            "tailscale" => {
                let settings = value.as_object().context("tailscale must be an object")?;
                ensure!(
                    settings.len() == 1 && settings.get("on").is_some_and(Value::is_boolean),
                    "tailscale supports only {{ on: boolean }}"
                );
            }
            "mark_read_warning" | "board" | "steps" | "bro" | "float_drafts" | "code_review" | "shape"
            | "system" | "chapter_diagrams" => {
                let settings = value.as_object().context("capability setting must be an object")?;
                ensure!(
                    settings.len() == 1 && settings.get("on").is_some_and(Value::is_boolean),
                    "capability setting supports only {{ on: boolean }}"
                );
            }
            "inbox_tool" => {
                let hosts = value.as_object().context("inbox_tool must be an object")?;
                ensure!(!hosts.is_empty(), "inbox_tool patch is empty");
                for (host, on) in hosts {
                    ensure!(
                        matches!(host.as_str(), "claude-code" | "pi" | "opencode" | "dsh" | "cline"),
                        "unsupported Inbox tool host {host}"
                    );
                    ensure!(on.is_boolean(), "inbox_tool.{host} must be boolean");
                }
            }
            "experimental" => {
                let settings = value.as_object().context("experimental must be an object")?;
                ensure!(
                    settings.len() == 1 && settings.get("decision_mode").is_some_and(Value::is_boolean),
                    "experimental supports only {{ decision_mode: boolean }}"
                );
            }
            "diff_options" => validate_diff_options(value)?,
            "rows" => {
                let rows = value.as_array().context("rows must be a list")?;
                ensure!(
                    rows.iter().all(|row| row.as_str().is_some_and(valid_row_option)),
                    "rows includes an unsupported option"
                );
            }
            "update_check" | "auto_update" => ensure!(value.is_boolean(), "{key} must be boolean"),
            "voice" => {
                let settings = value.as_object().context("voice must be an object")?;
                ensure!(!settings.is_empty(), "voice patch is empty");
                for (field, setting) in settings {
                    match field.as_str() {
                        "on" | "phones_only" => {
                            ensure!(setting.is_boolean(), "voice.{field} must be boolean");
                        }
                        "engine" => ensure!(
                            matches!(setting.as_str(), Some("codex" | "api")),
                            "voice.engine must be codex or api"
                        ),
                        _ => anyhow::bail!("voice.{field} is not a supported setting"),
                    }
                }
            }
            _ => anyhow::bail!("{key} is not a supported Inbox setting"),
        }
    }
    Ok(())
}

fn validate_diff_options(value: &Value) -> Result<()> {
    let settings = value.as_object().context("diff_options must be an object")?;
    ensure!(!settings.is_empty(), "diff_options patch is empty");
    for (field, setting) in settings {
        let valid = match field.as_str() {
            "diff_style" => matches!(setting.as_str(), Some("split" | "unified")),
            "overflow" => matches!(setting.as_str(), Some("scroll" | "wrap")),
            "indicators" => matches!(setting.as_str(), Some("bars" | "classic" | "none")),
            "line_diff_type" => {
                matches!(setting.as_str(), Some("word-alt" | "word" | "char" | "none"))
            }
            "line_numbers" | "background" => setting.is_boolean(),
            "background_intensity" => {
                matches!(setting.as_str(), Some("subtle" | "normal" | "strong"))
            }
            "tab_size" => matches!(setting.as_u64(), Some(1..=8)),
            _ => anyhow::bail!("diff_options.{field} is not supported"),
        };
        ensure!(valid, "diff_options.{field} has an invalid value");
    }
    Ok(())
}

// Palette ids are deliberately restricted to the built-in ids the server's Settings UI can name without its
// live theme registry. Unknown custom palettes remain visible as read-only values rather than becoming blind writes.
fn valid_theme_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && value.as_bytes().first().is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}
fn valid_row_option(value: &str) -> bool {
    matches!(
        value,
        "highlight"
            | "compact"
            | "hideAgents"
            | "smallNames"
            | "hideProject"
            | "hidePreview"
            | "roomy"
            | "unreadDot"
            | "previewTwo"
            | "relativeTime"
            | "titleFirst"
            | "marksThreads"
            | "marksProjects"
            | "markInline"
            | "markDot"
            | "repoLogo"
            | "githubPictures"
            | "timeBelow"
            | "dividers"
            | "statusMark"
            | "boldNames"
            | "hidePulls"
    )
}
