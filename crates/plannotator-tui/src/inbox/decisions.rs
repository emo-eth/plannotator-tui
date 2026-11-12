use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style, Stylize as _};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use serde::Deserialize;
use serde_json::json;

use super::client::{self, Author, Client, HttpError};
use super::format::author;
use super::pulls::ThreadPull;
use super::questions::{Question, State};
use crate::theme::palette;

const EXPORT_FORMATS: [ExportFormat; 3] = [ExportFormat::Markdown, ExportFormat::Html, ExportFormat::Json];

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct DecisionList {
    #[serde(rename = "serverSession")]
    pub(crate) server_session: String,
    pub(crate) cursor: u64,
    #[serde(default)]
    pub(crate) decisions: Vec<Decision>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Decision {
    pub(crate) id: String,
    pub(crate) question: Question,
    pub(crate) message: DecisionMessage,
    pub(crate) thread: DecisionThread,
    pub(crate) session: DecisionSession,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct DecisionMessage {
    pub(crate) id: String,
    pub(crate) created_at: String,
    pub(crate) author: Author,
    pub(crate) excerpt: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct DecisionThread {
    pub(crate) id: String,
    pub(crate) subject: Option<String>,
    pub(crate) thread_name: Option<String>,
    pub(crate) project: DecisionProject,
    pub(crate) section: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct DecisionProject {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) repo: Option<DecisionRepo>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct DecisionRepo {
    pub(crate) display: String,
    pub(crate) host: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct DecisionSession {
    pub(crate) key: String,
    pub(crate) host: Option<String>,
    pub(crate) session: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) wakes: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExportFormat {
    Markdown,
    Html,
    Json,
}

impl ExportFormat {
    fn route_value(self) -> &'static str {
        match self {
            Self::Markdown => "md",
            Self::Html => "html",
            Self::Json => "json",
        }
    }

    fn extension(self) -> &'static str {
        self.route_value()
    }

    fn label(self) -> &'static str {
        match self {
            Self::Markdown => "Markdown",
            Self::Html => "HTML",
            Self::Json => "JSON",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PostDestination {
    Description,
    Comment,
    Both,
}

impl PostDestination {
    fn label(self) -> &'static str {
        match self {
            Self::Description => "description",
            Self::Comment => "comment",
            Self::Both => "description and comment",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PostDecisionResult {
    pub(crate) ok: bool,
    pub(crate) action: Option<String>,
    pub(crate) destination: Option<PostDestination>,
    pub(crate) r#ref: Option<String>,
    pub(crate) url: Option<String>,
    pub(crate) fallback: Option<String>,
}

impl Client {
    pub(crate) fn decisions(&self) -> Result<DecisionList> {
        let list: DecisionList = self.get_json("GET", "/api/inbox/decisions")?;
        ensure!(
            list.server_session == self.server_session(),
            "Inbox serverSession changed; rediscover before continuing"
        );
        Ok(list)
    }

    pub(crate) fn export_decisions(&self, thread_id: &str, format: ExportFormat) -> Result<String> {
        let thread_id = client::path_id(thread_id)?;
        self.get_text(
            "GET",
            &format!("/api/inbox/threads/{thread_id}/decisions?format={}&tz=UTC", format.route_value()),
        )
    }

    pub(crate) fn post_decisions(&self, thread_id: &str, pull: &str) -> Result<PostDecisionResult> {
        let thread_id = client::path_id(thread_id)?;
        self.post_json(
            &format!("/api/inbox/threads/{thread_id}/decisions/post"),
            &json!({ "serverSession": self.server_session(), "pull": pull, "tz": "UTC" }),
        )
    }
}

pub(crate) fn decision_agent(decision: &Decision) -> String {
    decision
        .session
        .name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .map_or_else(|| author(&decision.message.author), str::to_owned)
}

pub(crate) fn decision_title(decision: &Decision) -> String {
    decision
        .thread
        .subject
        .as_deref()
        .or(decision.thread.thread_name.as_deref())
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or("(untitled thread)")
        .to_owned()
}

pub(crate) fn decision_state(decision: &Decision) -> &'static str {
    match decision.question.state {
        State::Open => "Open",
        State::Picked => "Picked, not sent",
        State::Sent => "Sent",
        State::Closed => "Set aside",
        State::Superseded => "Replaced",
        State::Unknown => "Unknown",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    List,
    Filter,
    Formats,
    Path,
    Overwrite,
    Pulls,
    ConfirmPost,
    PostResult,
}

#[derive(Debug)]
struct Panel {
    decisions: Vec<Decision>,
    filtered: Vec<usize>,
    query: String,
    cursor: usize,
    scroll: usize,
    view: View,
    status: Option<String>,
    export_format: ExportFormat,
    path: String,
    pulls: Vec<ThreadPull>,
    selected_pull: usize,
    post_result: Option<PostDecisionResult>,
    thread_context: Option<String>,
}

impl Panel {
    fn new(decisions: Vec<Decision>, thread_id: Option<String>) -> Self {
        let mut panel = Self {
            decisions,
            filtered: Vec::new(),
            query: thread_id.clone().unwrap_or_default(),
            cursor: 0,
            scroll: 0,
            view: View::List,
            status: None,
            export_format: ExportFormat::Markdown,
            path: String::new(),
            pulls: Vec::new(),
            selected_pull: 0,
            post_result: None,
            thread_context: thread_id,
        };
        panel.refilter();
        panel
    }

    fn refilter(&mut self) {
        let query = self.query.trim().to_lowercase();
        self.filtered = self
            .decisions
            .iter()
            .enumerate()
            .filter(|(_, decision)| {
                let haystack = format!(
                    "{} {} {} {} {} {} {}",
                    decision.id,
                    decision.thread.id,
                    decision_title(decision),
                    decision.thread.project.name,
                    decision_agent(decision),
                    decision.question.prompt,
                    decision_state(decision),
                )
                .to_lowercase();
                query.is_empty() || haystack.contains(&query)
            })
            .map(|(index, _)| index)
            .collect();
        self.cursor = self.cursor.min(self.filtered.len().saturating_sub(1));
        self.scroll = self.scroll.min(self.cursor);
    }

    fn selected(&self) -> Option<&Decision> {
        self.filtered.get(self.cursor).and_then(|index| self.decisions.get(*index))
    }

    fn selected_thread(&self) -> Option<String> {
        self.selected().map(|decision| decision.thread.id.clone()).or_else(|| self.thread_context.clone())
    }

    fn visible_lines(&mut self, height: usize) -> Vec<Line<'static>> {
        match self.view {
            View::List | View::Filter => self.list_lines(height),
            View::Formats => EXPORT_FORMATS
                .iter()
                .map(|format| {
                    let mark = if *format == self.export_format { ">" } else { " " };
                    Line::from(format!("{mark} Download as {}", format.label()))
                })
                .collect(),
            View::Path | View::Overwrite => vec![
                Line::from(format!("Save {} export to:", self.export_format.label())),
                Line::from(format!("  {}", self.path)),
                Line::from(""),
                Line::from(if self.view == View::Overwrite {
                    "That file already exists. Press y to replace it, n to choose another path."
                } else {
                    "Type a path, then press Enter to download and save."
                }),
            ],
            View::Pulls => self
                .pulls
                .iter()
                .enumerate()
                .map(|(index, pull)| {
                    let mark = if index == self.selected_pull { ">" } else { " " };
                    let title = pull.title.as_deref().unwrap_or("(untitled pull request)");
                    Line::from(format!("{mark} {} · {} · {}", pull.reference, pull.state.label(), title))
                })
                .collect(),
            View::ConfirmPost => {
                let pull = self.pulls.get(self.selected_pull);
                vec![
                    Line::from("Post this thread's decisions to the pull request?"),
                    Line::from(pull.map_or_else(
                        || "No pull request selected".to_owned(),
                        |pull| format!("  {} · {}", pull.reference, pull.url),
                    )),
                    Line::from(""),
                    Line::from("This writes to GitHub/GitLab. Press y to post or n to cancel."),
                ]
            }
            View::PostResult => self.post_lines(),
        }
    }

    fn list_lines(&mut self, height: usize) -> Vec<Line<'static>> {
        let start = self.scroll.min(self.filtered.len());
        let end = (start + height).min(self.filtered.len());
        self.filtered
            .get(start..end)
            .unwrap_or_default()
            .iter()
            .enumerate()
            .filter_map(|(offset, index)| {
                self.decisions.get(*index).map(|decision| {
                    let selected = start + offset == self.cursor;
                    let marker = if selected { ">" } else { " " };
                    Line::from(vec![
                        Span::raw(marker),
                        Span::styled(
                            format!(" {} ", decision_state(decision)),
                            if selected {
                                Style::new().fg(palette().accent).add_modifier(Modifier::BOLD)
                            } else {
                                Style::new().dim()
                            },
                        ),
                        Span::raw(format!(
                            "{} · {} · {}",
                            decision_agent(decision),
                            decision.thread.project.name,
                            decision_title(decision)
                        )),
                        Span::raw(format!(" — {}", decision.question.prompt)),
                    ])
                })
            })
            .collect()
    }

    fn post_lines(&self) -> Vec<Line<'static>> {
        let Some(answer) = &self.post_result else {
            return vec![Line::from("No post result.")];
        };
        if answer.ok {
            let action = answer.action.as_deref().unwrap_or("posted");
            let target = answer.r#ref.as_deref().unwrap_or("pull request");
            let destination = answer
                .destination
                .map_or(String::new(), |destination| format!(" in {}", destination.label()));
            let mut lines = vec![Line::from(format!("Decisions {action} on {target}{destination}."))];
            if let Some(fallback) = answer.fallback.as_deref() {
                lines.push(Line::from(format!(
                    "The description could not be used ({fallback}); posted as a comment instead."
                )));
            }
            if let Some(url) = answer.url.as_deref() {
                lines.push(Line::from(url.to_owned()));
            }
            lines
        } else {
            vec![
                Line::from("The Inbox did not confirm the post."),
                Line::from("Return to the list and check the pull request before retrying."),
            ]
        }
    }
}

pub(crate) fn run(
    client: &Client,
    terminal: &mut DefaultTerminal,
    thread_id: Option<&str>,
) -> Result<Option<String>> {
    let initial = match client.decisions() {
        Ok(list) => list,
        Err(error) => {
            let message = route_unavailable_message(&error)
                .unwrap_or_else(|| format!("Could not read decisions: {error:#}"));
            draw_message(terminal, &message)?;
            return Ok(None);
        }
    };
    let mut panel = Panel::new(initial.decisions, thread_id.map(str::to_owned));
    loop {
        terminal.draw(|frame| draw_panel(frame, &mut panel))?;
        if !event::poll(std::time::Duration::from_millis(250))? {
            continue;
        }
        let Event::Key(key) = event::read()? else { continue };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Ok(None);
        }
        if let Some(thread) = handle_key(client, &mut panel, key)? {
            return Ok(Some(thread));
        }
    }
}

fn handle_key(client: &Client, panel: &mut Panel, key: event::KeyEvent) -> Result<Option<String>> {
    if panel.view == View::Filter {
        filter_key(panel, key);
        return Ok(None);
    }
    if panel.view == View::Formats {
        match key.code {
            KeyCode::Esc => panel.view = View::List,
            KeyCode::Up | KeyCode::Char('k') => {
                panel.export_format = match panel.export_format {
                    ExportFormat::Markdown | ExportFormat::Html => ExportFormat::Markdown,
                    ExportFormat::Json => ExportFormat::Html,
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                panel.export_format = match panel.export_format {
                    ExportFormat::Markdown => ExportFormat::Html,
                    ExportFormat::Html | ExportFormat::Json => ExportFormat::Json,
                }
            }
            KeyCode::Enter => {
                let Some(thread_id) = panel.selected_thread() else { return Ok(None) };
                panel.path = format!("decisions-{thread_id}.{}", panel.export_format.extension());
                panel.view = View::Path;
            }
            _ => {}
        }
        return Ok(None);
    }
    if panel.view == View::Path {
        match key.code {
            KeyCode::Esc => panel.view = View::Formats,
            KeyCode::Enter => {
                if panel.path.trim().is_empty() {
                    panel.status = Some("Choose a file path before exporting.".to_owned());
                } else if PathBuf::from(&panel.path).exists() {
                    panel.view = View::Overwrite;
                } else {
                    save_export(client, panel)?;
                }
            }
            KeyCode::Backspace => {
                panel.path.pop();
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => panel.path.push(ch),
            _ => {}
        }
        return Ok(None);
    }
    if panel.view == View::Overwrite {
        match key.code {
            KeyCode::Char('y' | 'Y') => save_export(client, panel)?,
            KeyCode::Char('n' | 'N') | KeyCode::Esc => panel.view = View::Path,
            _ => {}
        }
        return Ok(None);
    }
    if panel.view == View::Pulls {
        match key.code {
            KeyCode::Esc => panel.view = View::List,
            KeyCode::Up | KeyCode::Char('k') => panel.selected_pull = panel.selected_pull.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                panel.selected_pull = (panel.selected_pull + 1).min(panel.pulls.len().saturating_sub(1));
            }
            KeyCode::Enter if !panel.pulls.is_empty() => panel.view = View::ConfirmPost,
            _ => {}
        }
        return Ok(None);
    }
    if panel.view == View::ConfirmPost {
        match key.code {
            KeyCode::Char('y' | 'Y') => post_selected(client, panel),
            KeyCode::Char('n' | 'N') | KeyCode::Esc => panel.view = View::Pulls,
            _ => {}
        }
        return Ok(None);
    }
    if panel.view == View::PostResult {
        if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
            panel.view = View::List;
        }
        return Ok(None);
    }
    Ok(list_key(client, panel, key.code))
}

fn list_key(client: &Client, panel: &mut Panel, code: KeyCode) -> Option<String> {
    match code {
        KeyCode::Esc | KeyCode::Char('q') => return None,
        KeyCode::Char('/') => panel.view = View::Filter,
        KeyCode::Char('a') => {
            panel.query.clear();
            panel.thread_context = None;
            panel.refilter();
        }
        KeyCode::Char('r') => match client.decisions() {
            Ok(list) => {
                panel.decisions = list.decisions;
                panel.refilter();
                panel.status = Some(format!("{} decisions · cursor {}", panel.decisions.len(), list.cursor));
            }
            Err(error) => {
                panel.status = Some(
                    route_unavailable_message(&error).unwrap_or_else(|| format!("Refresh failed: {error:#}")),
                );
            }
        },
        KeyCode::Up | KeyCode::Char('k') => panel.cursor = panel.cursor.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => {
            panel.cursor = (panel.cursor + 1).min(panel.filtered.len().saturating_sub(1));
        }
        KeyCode::Enter | KeyCode::Char('o') => return panel.selected_thread(),
        KeyCode::Char('e') => {
            if panel.selected_thread().is_some() {
                panel.export_format = ExportFormat::Markdown;
                panel.view = View::Formats;
            }
        }
        KeyCode::Char('p') => {
            if let Some(thread) = panel.selected_thread() {
                match client.thread_pulls(&thread) {
                    Ok(pulls) if !pulls.pulls.is_empty() => {
                        panel.pulls = pulls.pulls;
                        panel.selected_pull = 0;
                        panel.view = View::Pulls;
                    }
                    Ok(_) => {
                        panel.status = Some(
                            "This thread has no linked pull request. Add one in the thread first.".to_owned(),
                        );
                    }
                    Err(error) => {
                        panel.status = Some(
                            route_unavailable_message(&error)
                                .unwrap_or_else(|| format!("Could not read pull requests: {error:#}")),
                        );
                    }
                }
            }
        }
        _ => {}
    }
    None
}

fn filter_key(panel: &mut Panel, key: event::KeyEvent) {
    match key.code {
        KeyCode::Esc => {
            panel.query.clear();
            panel.thread_context = None;
            panel.refilter();
            panel.view = View::List;
        }
        KeyCode::Enter => panel.view = View::List,
        KeyCode::Backspace => {
            panel.query.pop();
            panel.thread_context = None;
            panel.refilter();
        }
        KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            panel.query.push(ch);
            panel.thread_context = None;
            panel.refilter();
        }
        _ => {}
    }
}

fn post_selected(client: &Client, panel: &mut Panel) {
    let Some(thread_id) = panel.selected_thread() else { return };
    let Some(pull) = panel.pulls.get(panel.selected_pull) else { return };
    match client.post_decisions(&thread_id, &pull.reference) {
        Ok(result) => panel.post_result = Some(result),
        Err(error) => {
            panel.status =
                Some(route_unavailable_message(&error).unwrap_or_else(|| format!("Post failed: {error:#}")));
        }
    }
    panel.view = View::PostResult;
}

fn save_export(client: &Client, panel: &mut Panel) -> Result<()> {
    let Some(thread_id) = panel.selected_thread() else {
        panel.view = View::List;
        return Ok(());
    };
    let format = panel.export_format;
    let content = match client.export_decisions(&thread_id, format) {
        Ok(content) => content,
        Err(error) => {
            panel.status = Some(
                route_unavailable_message(&error).unwrap_or_else(|| format!("Export failed: {error:#}")),
            );
            panel.view = View::List;
            return Ok(());
        }
    };
    if format == ExportFormat::Json {
        let value: serde_json::Value =
            serde_json::from_str(&content).context("decoding the Inbox decision export")?;
        ensure!(
            value.get("format").and_then(serde_json::Value::as_str) == Some("plannotator.decisions"),
            "unsupported Inbox decision export format"
        );
        ensure!(
            value.get("v").and_then(serde_json::Value::as_u64) == Some(1),
            "unsupported Inbox decision export version"
        );
    }
    let path = PathBuf::from(panel.path.trim());
    if path.exists() {
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("reading export destination {}", path.display()))?;
        ensure!(!metadata.file_type().is_symlink(), "refusing to overwrite symlink {}", path.display());
        ensure!(metadata.is_file(), "export destination is not a file: {}", path.display());
    }
    fs::write(&path, content.as_bytes())
        .with_context(|| format!("writing decision export to {}", path.display()))?;
    panel.status = Some(format!("Exported {} decisions to {}", format.label(), path.display()));
    panel.view = View::List;
    Ok(())
}

fn route_unavailable_message(error: &anyhow::Error) -> Option<String> {
    let http = error.downcast_ref::<HttpError>()?;
    if http.status != 404 {
        return None;
    }
    let detail = match http.code.as_deref() {
        Some("not_found") => "Decision mode may be disabled in Inbox settings.",
        _ => "This Inbox version does not provide that decisions route.",
    };
    Some(format!("{detail} Update Plannotator or enable Decision mode, then retry."))
}

fn draw_message(terminal: &mut DefaultTerminal, message: &str) -> Result<()> {
    loop {
        terminal.draw(|frame| {
            let [body, footer] =
                Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());
            frame.render_widget(
                Paragraph::new(message.to_owned()).wrap(ratatui::widgets::Wrap { trim: true }),
                body,
            );
            frame.render_widget(Paragraph::new("Esc to return"), footer);
        })?;
        if event::poll(std::time::Duration::from_millis(250))?
            && let Event::Key(key) = event::read()?
            && key.kind != KeyEventKind::Release
            && matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q'))
        {
            break;
        }
    }
    Ok(())
}

fn draw_panel(frame: &mut ratatui::Frame<'_>, panel: &mut Panel) {
    let area = frame.area();
    let [header, body, footer] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1), Constraint::Length(1)]).areas(area);
    let title = match panel.view {
        View::List | View::Filter => {
            format!(" Decisions · {} of {}", panel.filtered.len(), panel.decisions.len())
        }
        View::Formats => " Export decisions".to_owned(),
        View::Path | View::Overwrite => " Save decision export".to_owned(),
        View::Pulls | View::ConfirmPost => " Post decisions to a pull request".to_owned(),
        View::PostResult => " Post result".to_owned(),
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            title,
            Style::new().fg(palette().accent).add_modifier(Modifier::BOLD),
        ))),
        header,
    );
    let mut lines = panel.visible_lines(usize::from(body.height));
    if matches!(panel.view, View::List | View::Filter) && panel.decisions.is_empty() {
        lines.push(Line::from("No decisions are waiting for you."));
    } else if matches!(panel.view, View::List | View::Filter) && panel.filtered.is_empty() {
        lines.push(Line::from("No decisions match this filter."));
    }
    let mut paragraph = Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: true });
    if panel.view == View::List || panel.view == View::Filter {
        ensure_cursor(panel, usize::from(body.height));
        paragraph = paragraph.scroll((panel.scroll.min(u16::MAX as usize) as u16, 0));
    }
    frame.render_widget(paragraph, body);
    let keys = match panel.view {
        View::List => "↑/↓ move · Enter open thread · / filter · e export · p post · r refresh · q back",
        View::Filter => "Type to filter · Enter done · Esc clear",
        View::Formats => "↑/↓ choose · Enter · Esc back",
        View::Path => "Type path · Enter save · Esc back",
        View::Overwrite => "y overwrite · n choose another path",
        View::Pulls => "↑/↓ choose pull request · Enter continue · Esc back",
        View::ConfirmPost => "y confirm forge write · n cancel",
        View::PostResult => "Enter/Esc back",
    };
    let footer_text = panel.status.as_deref().unwrap_or(keys);
    frame.render_widget(Paragraph::new(footer_text.to_owned()).dim(), footer);
}

fn ensure_cursor(panel: &mut Panel, height: usize) {
    if height == 0 || panel.cursor < panel.scroll {
        panel.scroll = panel.cursor;
    } else if panel.cursor >= panel.scroll.saturating_add(height) {
        panel.scroll = panel.cursor.saturating_sub(height.saturating_sub(1));
    }
}
