//! Inbox history and operational overview panes: archived threads, activity, decision data,
//! storage sizes, and Tidy. Reads are GET-only until an explicit, confirmed mutation key.

use std::collections::HashSet;
use std::time::Duration;

use anyhow::{Context as _, Result};
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style, Stylize as _};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use serde::Deserialize;
use serde_json::{Value, json};
use time::{Duration as TimeDuration, OffsetDateTime, format_description::well_known::Rfc3339};
use url::form_urlencoded;

use super::client::{Client, Row};
use crate::theme::palette;

const ARCHIVE_PAGE: usize = 50;
const ACTIVITY_RANGES: [u16; 5] = [1, 3, 7, 14, 30];
const DECISION_RANGES: [u16; 3] = [7, 30, 90];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Archived,
    Activity,
    Decisions,
    Storage,
    Tidy,
}

impl Tab {
    const ALL: [Self; 5] = [Self::Archived, Self::Activity, Self::Decisions, Self::Storage, Self::Tidy];

    fn label(self) -> &'static str {
        match self {
            Self::Archived => "Archived",
            Self::Activity => "Activity",
            Self::Decisions => "Decisions",
            Self::Storage => "Storage",
            Self::Tidy => "Tidy",
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct ArchiveProject {
    id: String,
    name: String,
    archived: usize,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct ArchivedModel {
    #[serde(rename = "serverSession")]
    server_session: String,
    total: usize,
    offset: usize,
    projects: Vec<ArchiveProject>,
    threads: Vec<Row>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProjectsModel {
    #[serde(rename = "serverSession")]
    server_session: String,
    projects: Vec<ProjectFolder>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProjectFolder {
    id: String,
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ActivityModel {
    from: String,
    to: String,
    hour_count: usize,
    threads: Vec<ActivityThread>,
}

#[derive(Debug, Clone, Deserialize)]
struct ActivityThread {
    thread_id: String,
    name: Option<String>,
    project_id: String,
    started_at: String,
    last_at: String,
    total: u64,
    hours: Vec<u64>,
}

#[derive(Debug, Clone, Deserialize)]
struct DecisionData {
    range: DecisionRange,
    counts: DecisionCounts,
    settled_in_words: u64,
    waiting: u64,
    pace: DecisionPace,
    stopped_wait: DecisionWait,
    recommendation: DecisionRecommendation,
    projects: Vec<DecisionGroup>,
    agents: Vec<DecisionGroup>,
    history: DecisionHistory,
    trends: Vec<DecisionTrend>,
    rate_min: u64,
    median_min: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct DecisionRange {
    since: Option<i64>,
    until: i64,
    earliest: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
struct DecisionCounts {
    answered: u64,
    skipped: u64,
    set_aside: u64,
    replaced: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct DecisionPace {
    fast: DecisionPacePart,
    slow: DecisionPacePart,
}

#[derive(Debug, Clone, Deserialize)]
struct DecisionPacePart {
    offered: u64,
    taken: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct DecisionWait {
    n: u64,
    median_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
struct DecisionRecommendation {
    offered: u64,
    taken: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct DecisionHistory {
    asked: u64,
    answered: u64,
    earliest: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
struct DecisionGroup {
    label: String,
    host: Option<String>,
    answered: u64,
    skipped: u64,
    set_aside: u64,
    replaced: u64,
    offered: u64,
    taken: u64,
    median_wait_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "key")]
enum DecisionTrend {
    #[serde(rename = "recommendations")]
    Recommendations { direction: String, now: u64, was: u64 },
    #[serde(rename = "skips")]
    Skips { direction: String, now: u64, was: u64 },
    #[serde(rename = "waits")]
    Waits { direction: String, now_ms: u64, was_ms: u64 },
}

#[derive(Debug, Clone, Deserialize)]
struct StorageModel {
    dir: String,
    bytes: u64,
    projects: Vec<StorageProject>,
}

#[derive(Debug, Clone, Deserialize)]
struct StorageProject {
    name: String,
    root: String,
    bytes: u64,
    threads: Vec<StorageThread>,
    #[serde(default)]
    combined_into: Option<String>,
    #[serde(default)]
    combined: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct StorageThread {
    thread_id: String,
    subject: Option<String>,
    bytes: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct TidyPlan {
    #[serde(rename = "serverSession")]
    server_session: String,
    count: usize,
    agents: Vec<TidyAgent>,
    run: Option<TidyRun>,
    #[serde(default)]
    combine: Vec<TidyCombine>,
    #[serde(default)]
    combine_loading: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct TidyAgent {
    session: String,
    name: String,
    reach: String,
    items: Vec<TidyItem>,
}

#[derive(Debug, Clone, Deserialize)]
struct TidyItem {
    subject: String,
    project: TidyProject,
    reasons: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct TidyProject {
    id: String,
    name: String,
    root: String,
}

#[derive(Debug, Clone, Deserialize)]
struct TidyCombine {
    into: TidyProject,
    from: TidyProject,
    why: String,
}

#[derive(Debug, Clone, Deserialize)]
struct TidyRun {
    id: String,
    at: String,
    agents: Vec<TidyRunAgent>,
    done: usize,
    updated: usize,
}

#[derive(Debug, Clone, Deserialize)]
struct TidyRunAgent {
    host: Option<String>,
    name: String,
    reach: String,
    state: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Confirmation {
    None,
    Unarchive,
    Tidy,
}

#[derive(Debug)]
struct History {
    tab: Tab,
    rows: Vec<Row>,
    archive_projects: Vec<ArchiveProject>,
    project_filter: Option<String>,
    total: usize,
    offset: usize,
    cursor: usize,
    activity_days: u16,
    activity_project: Option<String>,
    projects: Vec<(String, String)>,
    activity: Option<ActivityModel>,
    activity_cursor: usize,
    decisions_days: u16,
    decisions: Option<DecisionData>,
    storage: Option<StorageModel>,
    tidy: Option<TidyPlan>,
    tidy_cursor: usize,
    tidy_sessions: HashSet<String>,
    tidy_combines: HashSet<String>,
    confirmation: Confirmation,
    status: Option<String>,
    scroll: usize,
    return_thread: Option<String>,
}

/// Open history in the existing Inbox terminal. An unarchive returns its thread
/// id so the caller can open the restored thread in the Inbox view.
pub(crate) fn run(
    client: &Client,
    terminal: &mut ratatui::DefaultTerminal,
    selected_thread: Option<&str>,
) -> Result<Option<String>> {
    let mut history = History {
        tab: Tab::Archived,
        rows: Vec::new(),
        archive_projects: Vec::new(),
        project_filter: None,
        total: 0,
        offset: 0,
        cursor: 0,
        activity_days: 7,
        activity_project: None,
        projects: Vec::new(),
        activity: None,
        activity_cursor: 0,
        decisions_days: 30,
        decisions: None,
        storage: None,
        tidy: None,
        tidy_cursor: 0,
        tidy_sessions: HashSet::new(),
        tidy_combines: HashSet::new(),
        confirmation: Confirmation::None,
        status: None,
        scroll: 0,
        return_thread: None,
    };
    if let Err(error) = load_archive(client, &mut history) {
        history.status = Some(format!("{error:#}"));
    }
    if let Some(id) = selected_thread
        && let Some(index) = history.rows.iter().position(|row| row.thread_id == id)
    {
        history.cursor = index;
    }
    loop {
        terminal.draw(|frame| history.draw(frame)).context("drawing Inbox history")?;
        if !event::poll(Duration::from_millis(100)).context("waiting for Inbox history input")? {
            continue;
        }
        let Event::Key(key) = event::read().context("reading Inbox history input")? else { continue };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        if let Err(error) = history.key(client, key.code) {
            history.status = Some(format!("{error:#}"));
        }
        if let Some(id) = history.return_thread.take() {
            return Ok(Some(id));
        }
        if key.code == KeyCode::Esc && history.confirmation == Confirmation::None {
            return Ok(None);
        }
    }
}

impl History {
    fn draw(&self, frame: &mut Frame) {
        let [header, body, footer] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(1), Constraint::Length(1)])
                .areas(frame.area());
        let tabs = Tab::ALL
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let label = format!(" {}:{} ", index + 1, tab.label());
                if *tab == self.tab {
                    Span::styled(label, palette().selection.add_modifier(Modifier::BOLD))
                } else {
                    Span::styled(label, Style::new().dim())
                }
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(Line::from(tabs)), header);

        let mut lines = match self.tab {
            Tab::Archived => self.archive_lines(),
            Tab::Activity => self.activity_lines(),
            Tab::Decisions => self.decision_lines(),
            Tab::Storage => self.storage_lines(),
            Tab::Tidy => self.tidy_lines(),
        };
        if let Some(status) = &self.status {
            lines.push(Line::from(Span::styled(status.clone(), palette().comment)));
        }
        if self.confirmation != Confirmation::None {
            lines.push(Line::from(Span::styled(
                match self.confirmation {
                    Confirmation::Unarchive => "Confirm unarchive: y proceed · Esc cancel",
                    Confirmation::Tidy => "Confirm selected Tidy actions: y proceed · Esc cancel",
                    Confirmation::None => "",
                },
                Style::new().fg(palette().comment).add_modifier(Modifier::BOLD),
            )));
        }
        frame.render_widget(Paragraph::new(lines).scroll((self.scroll as u16, 0)), body);
        let help = match self.tab {
            Tab::Archived => "↑↓ select · n/p page · f project · u unarchive · 1-5 pane · esc back",
            Tab::Activity => "↑↓ select · enter open thread · [ ] range · f project · r refresh · esc back",
            Tab::Decisions => "[ ] range · r refresh · 1-5 pane · esc back",
            Tab::Storage => "r refresh · 1-5 pane · esc back",
            Tab::Tidy => "↑↓ select · space toggle · r refresh · t plan · 1-5 pane · esc back",
        };
        frame.render_widget(Paragraph::new(Line::from(Span::raw(help).dim())).right_aligned(), footer);
    }

    fn archive_lines(&self) -> Vec<Line<'static>> {
        let project_label = self
            .project_filter
            .as_deref()
            .and_then(|id| self.archive_projects.iter().find(|project| project.id == id))
            .map_or("All projects", |project| project.name.as_str());
        let mut lines = vec![Line::from(format!(
            "Archived · {}/{} · project: {project_label}",
            self.offset.saturating_add(self.rows.len()).min(self.total),
            self.total
        ))];
        if self.rows.is_empty() {
            lines.push(Line::from(
                Span::raw(if self.total == 0 {
                    "Nothing archived."
                } else {
                    "No archived threads on this page."
                })
                .dim(),
            ));
        }
        let grouped = self.archive_projects.iter().filter(|project| project.archived > 0).count() > 1
            && self.project_filter.is_none();
        let mut current_project = String::new();
        for (index, row) in self.rows.iter().enumerate() {
            if grouped && current_project != row.project.id {
                current_project.clone_from(&row.project.id);
                let count = self
                    .archive_projects
                    .iter()
                    .find(|project| project.id == row.project.id)
                    .map_or(0, |project| project.archived);
                lines.push(Line::from(Span::styled(
                    format!("{} · {count} archived", row.project.name),
                    Style::new().fg(palette().accent).add_modifier(Modifier::BOLD),
                )));
            }
            let subject = row.thread_name.as_deref().or(row.subject.as_deref()).unwrap_or("(no subject)");
            let archived = row.resolved_at.as_deref().unwrap_or("unknown date");
            let line = format!(
                "{} {} · {} · {} messages · archived {}",
                if index == self.cursor { "›" } else { " " },
                subject,
                row.project.name,
                row.message_count,
                archived
            );
            lines.push(if index == self.cursor {
                Line::from(Span::styled(line, palette().selection))
            } else {
                Line::from(line)
            });
        }
        lines
    }

    fn activity_lines(&self) -> Vec<Line<'static>> {
        let Some(model) = &self.activity else {
            return vec![Line::from("No activity loaded. Press r to read.")];
        };
        let project = self
            .activity_project
            .as_deref()
            .and_then(|id| self.projects.iter().find(|project| project.0 == id))
            .map_or("All projects", |project| project.1.as_str());
        let mut lines = vec![Line::from(format!(
            "Activity · {} days · {} threads · {} hours · {project}",
            self.activity_days,
            model.threads.len(),
            model.hour_count
        ))];
        if model.threads.is_empty() {
            lines.push(Line::from(Span::raw("No messages in this range.").dim()));
        }
        for (index, thread) in model.threads.iter().enumerate() {
            let name = thread.name.as_deref().unwrap_or("(no subject)");
            let hourly = thread
                .hours
                .as_chunks::<2>()
                .0
                .iter()
                .map(|[hour, count]| format!("{hour}:{count}"))
                .collect::<Vec<_>>()
                .join(" ");
            let label = format!(
                "{} {} · {} messages · project {} · {} → {}",
                if index == self.activity_cursor { "›" } else { " " },
                name,
                thread.total,
                thread.project_id,
                thread.started_at,
                thread.last_at
            );
            lines.push(if index == self.activity_cursor {
                Line::from(Span::styled(label, palette().selection))
            } else {
                Line::from(label)
            });
            lines.push(Line::from(format!("    {hourly}")));
        }
        lines.push(Line::from(format!(
            "{} → {} · f changes project · [ ] changes range",
            model.from, model.to
        )));
        lines
    }

    fn decision_lines(&self) -> Vec<Line<'static>> {
        let Some(data) = &self.decisions else {
            return vec![Line::from("No decision data loaded. Press r to read.")];
        };
        let mut lines = vec![Line::from(format!(
            "Decision data · {} days · {} asked · {} waiting",
            self.decisions_days, data.history.asked, data.waiting
        ))];
        lines.push(Line::from(format!(
            "Answered {} · skipped {} · set aside {} · replaced {} · own words {}",
            data.counts.answered,
            data.counts.skipped,
            data.counts.set_aside,
            data.counts.replaced,
            data.settled_in_words
        )));
        lines.push(Line::from(format!(
            "Recommendation {}/{} · fast {}/{} · slow {}/{} · stopped wait {} (median {})",
            data.recommendation.taken,
            data.recommendation.offered,
            data.pace.fast.taken,
            data.pace.fast.offered,
            data.pace.slow.taken,
            data.pace.slow.offered,
            data.stopped_wait.n,
            duration(data.stopped_wait.median_ms)
        )));
        lines.push(Line::from(format!(
            "Range {} → {} · earliest {} · rate minimum {} · median minimum {}",
            timestamp(data.range.since),
            timestamp(Some(data.range.until)),
            timestamp(data.range.earliest),
            data.rate_min,
            data.median_min
        )));
        lines.push(Line::from(format!(
            "History {} questions · {} answered · earliest {}",
            data.history.asked,
            data.history.answered,
            timestamp(data.history.earliest)
        )));
        lines.push(Line::from(Span::styled(
            "Trends (last 30 days vs. prior 30)",
            Style::new().fg(palette().accent).add_modifier(Modifier::BOLD),
        )));
        if data.trends.is_empty() {
            lines.push(Line::from("  Not enough history for a trend."));
        }
        for trend in &data.trends {
            lines.push(Line::from(format!("  {}", trend_label(trend))));
        }
        lines.push(Line::from(Span::styled(
            "Projects",
            Style::new().fg(palette().accent).add_modifier(Modifier::BOLD),
        )));
        for group in &data.projects {
            lines.push(Line::from(group_line(group)));
        }
        lines.push(Line::from(Span::styled(
            "Agents",
            Style::new().fg(palette().accent).add_modifier(Modifier::BOLD),
        )));
        for group in &data.agents {
            lines.push(Line::from(format!(
                "{}{}",
                group_line(group),
                group.host.as_deref().map_or(String::new(), |host| format!(" · {host}"))
            )));
        }
        lines
    }

    fn storage_lines(&self) -> Vec<Line<'static>> {
        let Some(storage) = &self.storage else {
            return vec![Line::from("No storage overview loaded. Press r to read.")];
        };
        let mut lines = vec![Line::from(format!(
            "Storage · {} · {} · {} projects",
            storage.dir,
            bytes(storage.bytes),
            storage.projects.len()
        ))];
        let mut projects = storage.projects.iter().collect::<Vec<_>>();
        projects.sort_by(|left, right| right.bytes.cmp(&left.bytes).then_with(|| left.name.cmp(&right.name)));
        for project in projects {
            lines.push(Line::from(format!(
                "{} · {} · {} threads · {}",
                project.name,
                bytes(project.bytes),
                project.threads.len(),
                project.root
            )));
            if let Some(into) = &project.combined_into {
                lines.push(Line::from(format!("  Combined into {into}")));
            }
            if !project.combined.is_empty() {
                lines.push(Line::from(format!("  Includes: {}", project.combined.join(", "))));
            }
            let mut threads = project.threads.iter().collect::<Vec<_>>();
            threads.sort_by(|left, right| {
                right.bytes.cmp(&left.bytes).then_with(|| left.thread_id.cmp(&right.thread_id))
            });
            for thread in threads.iter().take(5) {
                lines.push(Line::from(format!(
                    "  {} · {} · {}",
                    thread.subject.as_deref().unwrap_or("(no subject)"),
                    bytes(thread.bytes),
                    thread.thread_id
                )));
            }
        }
        lines
    }

    fn tidy_lines(&self) -> Vec<Line<'static>> {
        let Some(plan) = &self.tidy else { return vec![Line::from("No Tidy plan loaded. Press r to read.")] };
        let agents_selected =
            plan.agents.iter().filter(|agent| self.tidy_sessions.contains(&agent.session)).count();
        let combines_selected =
            plan.combine.iter().filter(|pair| self.tidy_combines.contains(&pair.from.id)).count();
        let mut lines = vec![Line::from(format!(
            "Tidy · {} items · {agents_selected} agents selected · {combines_selected} project combines selected{}",
            plan.count,
            if plan.combine_loading { " · project scan still running" } else { "" }
        ))];
        if let Some(run) = &plan.run {
            lines.push(Line::from(format!(
                "Last run {} · {} of {} done · {} threads updated · {}",
                run.id,
                run.done,
                run.agents.len(),
                run.updated,
                run.at
            )));
            for agent in &run.agents {
                lines.push(Line::from(format!(
                    "  {} · {} · {} · {}",
                    agent.name,
                    agent.state,
                    agent.reach,
                    agent.host.as_deref().unwrap_or("unknown host")
                )));
            }
        }
        if plan.agents.is_empty() && plan.combine.is_empty() {
            lines.push(Line::from(Span::raw("Nothing to ask.").dim()));
        }
        for (index, agent) in plan.agents.iter().enumerate() {
            let selected = self.tidy_sessions.contains(&agent.session);
            let label = format!(
                "{} [{}] {} · {} · {} items",
                if index == self.tidy_cursor { "›" } else { " " },
                if selected { "x" } else { " " },
                agent.name,
                agent.reach,
                agent.items.len()
            );
            lines.push(if index == self.tidy_cursor {
                Line::from(Span::styled(label, palette().selection))
            } else {
                Line::from(label)
            });
            for item in &agent.items {
                lines.push(Line::from(format!(
                    "    {} · {} · {}",
                    item.subject,
                    item.project.name,
                    item.reasons.join(", ")
                )));
            }
        }
        if !plan.combine.is_empty() {
            lines.push(Line::from(Span::styled(
                "Combine projects",
                Style::new().fg(palette().accent).add_modifier(Modifier::BOLD),
            )));
        }
        for (index, pair) in plan.combine.iter().enumerate() {
            let cursor = plan.agents.len() + index;
            let selected = self.tidy_combines.contains(&pair.from.id);
            let label = format!(
                "{} [{}] {} → {} · {}",
                if cursor == self.tidy_cursor { "›" } else { " " },
                if selected { "x" } else { " " },
                pair.from.name,
                pair.into.name,
                pair.why
            );
            lines.push(if cursor == self.tidy_cursor {
                Line::from(Span::styled(label, palette().selection))
            } else {
                Line::from(label)
            });
            lines.push(Line::from(format!("    {} → {}", pair.from.root, pair.into.root)));
        }
        lines.push(Line::from("Space selects; t reviews the selected action(s) before sending."));
        lines
    }

    fn key(&mut self, client: &Client, code: KeyCode) -> Result<()> {
        if self.confirmation != Confirmation::None {
            if code == KeyCode::Esc {
                self.confirmation = Confirmation::None;
                self.status = Some("Cancelled.".to_owned());
                return Ok(());
            }
            if matches!(code, KeyCode::Char('y' | 'Y')) {
                match self.confirmation {
                    Confirmation::Unarchive => self.unarchive(client)?,
                    Confirmation::Tidy => self.start_tidy(client)?,
                    Confirmation::None => {}
                }
            } else {
                self.confirmation = Confirmation::None;
                self.status = Some("Cancelled.".to_owned());
            }
            return Ok(());
        }
        if matches!(code, KeyCode::PageUp | KeyCode::PageDown) {
            self.scroll = if code == KeyCode::PageUp {
                self.scroll.saturating_sub(10)
            } else {
                self.scroll.saturating_add(10)
            };
            return Ok(());
        }
        if let KeyCode::Char(n @ '1'..='5') = code {
            let index = (n as usize).saturating_sub('1' as usize);
            if let Some(tab) = Tab::ALL.get(index).copied()
                && let Err(error) = self.switch_tab(client, tab)
            {
                self.status = Some(format!("{error:#}"));
            }
            return Ok(());
        }
        match self.tab {
            Tab::Archived => match code {
                KeyCode::Up | KeyCode::Char('k') => self.cursor = self.cursor.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    self.cursor = self.cursor.saturating_add(1).min(self.rows.len().saturating_sub(1));
                }
                KeyCode::Char('n') => self.archive_page(client, true)?,
                KeyCode::Char('p') => self.archive_page(client, false)?,
                KeyCode::Char('f') => self.cycle_project(client)?,
                KeyCode::Char('u') if !self.rows.is_empty() => {
                    self.confirmation = Confirmation::Unarchive;
                    self.status = Some("Unarchive selected thread?".to_owned());
                }
                KeyCode::Char('r') => load_archive(client, self)?,
                _ => {}
            },
            Tab::Activity => match code {
                KeyCode::Up | KeyCode::Char('k') => {
                    self.activity_cursor = self.activity_cursor.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.activity_cursor = self
                        .activity_cursor
                        .saturating_add(1)
                        .min(self.activity.as_ref().map_or(0, |model| model.threads.len().saturating_sub(1)));
                }
                KeyCode::Enter => {
                    if let Some(id) = self
                        .activity
                        .as_ref()
                        .and_then(|model| model.threads.get(self.activity_cursor))
                        .map(|thread| thread.thread_id.clone())
                    {
                        self.return_thread = Some(id);
                    }
                }
                KeyCode::Char('r') => load_activity(client, self)?,
                KeyCode::Char('f') => self.cycle_activity_project(client)?,
                KeyCode::Char('[') => {
                    self.activity_days = cycle_range(&ACTIVITY_RANGES, self.activity_days, false);
                    load_activity(client, self)?;
                }
                KeyCode::Char(']') => {
                    self.activity_days = cycle_range(&ACTIVITY_RANGES, self.activity_days, true);
                    load_activity(client, self)?;
                }
                _ => {}
            },
            Tab::Decisions => match code {
                KeyCode::Char('r') => load_decisions(client, self)?,
                KeyCode::Char('[') => {
                    self.decisions_days = cycle_range(&DECISION_RANGES, self.decisions_days, false);
                    load_decisions(client, self)?;
                }
                KeyCode::Char(']') => {
                    self.decisions_days = cycle_range(&DECISION_RANGES, self.decisions_days, true);
                    load_decisions(client, self)?;
                }
                _ => {}
            },
            Tab::Storage => {
                if code == KeyCode::Char('r') {
                    load_storage(client, self)?;
                }
            }
            Tab::Tidy => match code {
                KeyCode::Up | KeyCode::Char('k') => self.tidy_cursor = self.tidy_cursor.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    self.tidy_cursor =
                        self.tidy_cursor.saturating_add(1).min(self.tidy_item_count().saturating_sub(1));
                }
                KeyCode::Char(' ') => self.toggle_tidy_item(),
                KeyCode::Char('r') => load_tidy(client, self)?,
                KeyCode::Char('t') if self.tidy_sessions.is_empty() && self.tidy_combines.is_empty() => {
                    self.status = Some("Select at least one agent or combine suggestion.".to_owned());
                }
                KeyCode::Char('t') => {
                    self.confirmation = Confirmation::Tidy;
                    self.status = Some("Send selected Tidy requests and/or combine projects?".to_owned());
                }
                _ => {}
            },
        }
        Ok(())
    }

    fn switch_tab(&mut self, client: &Client, tab: Tab) -> Result<()> {
        self.tab = tab;
        self.cursor = 0;
        self.activity_cursor = 0;
        self.tidy_cursor = 0;
        self.scroll = 0;
        self.status = None;
        match tab {
            Tab::Archived => load_archive(client, self),
            Tab::Activity => load_activity(client, self),
            Tab::Decisions => load_decisions(client, self),
            Tab::Storage => load_storage(client, self),
            Tab::Tidy => load_tidy(client, self),
        }
    }

    fn archive_page(&mut self, client: &Client, next: bool) -> Result<()> {
        let end = self.offset.saturating_add(self.rows.len());
        let next_offset = if next {
            if end >= self.total {
                return Ok(());
            }
            end
        } else {
            self.offset.saturating_sub(ARCHIVE_PAGE)
        };
        if next_offset != self.offset {
            self.offset = next_offset;
            self.cursor = 0;
            load_archive(client, self)?;
        }
        Ok(())
    }

    fn cycle_project(&mut self, client: &Client) -> Result<()> {
        let mut choices = vec![None];
        choices.extend(
            self.archive_projects
                .iter()
                .filter(|project| project.archived > 0)
                .map(|project| Some(project.id.clone())),
        );
        let current = choices.iter().position(|choice| *choice == self.project_filter).unwrap_or(0);
        self.project_filter = choices.get((current + 1) % choices.len()).cloned().flatten();
        self.offset = 0;
        self.cursor = 0;
        load_archive(client, self)
    }

    fn cycle_activity_project(&mut self, client: &Client) -> Result<()> {
        let mut choices = vec![None];
        choices.extend(self.projects.iter().map(|(id, _)| Some(id.clone())));
        let current = choices.iter().position(|choice| *choice == self.activity_project).unwrap_or(0);
        self.activity_project = choices.get((current + 1) % choices.len()).cloned().flatten();
        self.activity_cursor = 0;
        load_activity(client, self)
    }

    fn tidy_item_count(&self) -> usize {
        self.tidy.as_ref().map_or(0, |plan| plan.agents.len() + plan.combine.len())
    }

    fn toggle_tidy_item(&mut self) {
        let Some(plan) = &self.tidy else { return };
        if self.tidy_cursor < plan.agents.len() {
            if let Some(agent) = plan.agents.get(self.tidy_cursor)
                && !self.tidy_sessions.insert(agent.session.clone())
            {
                self.tidy_sessions.remove(&agent.session);
            }
        } else if let Some(pair) = plan.combine.get(self.tidy_cursor - plan.agents.len()) {
            let id = pair.from.id.clone();
            if !self.tidy_combines.insert(id.clone()) {
                self.tidy_combines.remove(&id);
            }
        }
    }

    fn unarchive(&mut self, client: &Client) -> Result<()> {
        let Some(row) = self.rows.get(self.cursor) else { return Ok(()) };
        let thread_id = row.thread_id.clone();
        let path = format!("/api/inbox/messages/{}/resolve", super::client::path_id(&thread_id)?);
        let _: Value = client
            .post_json(&path, &json!({ "serverSession": client.server_session(), "resolved": false }))?;
        self.confirmation = Confirmation::None;
        self.return_thread = Some(thread_id);
        Ok(())
    }

    fn start_tidy(&mut self, client: &Client) -> Result<()> {
        let plan = self.tidy.as_ref().context("Tidy plan is not loaded")?;
        let selected_sessions = plan
            .agents
            .iter()
            .filter(|agent| self.tidy_sessions.contains(&agent.session))
            .map(|agent| agent.session.clone())
            .collect::<Vec<_>>();
        let selected_combines = plan
            .combine
            .iter()
            .filter(|pair| self.tidy_combines.contains(&pair.from.id))
            .map(|pair| {
                (pair.from.id.clone(), pair.from.name.clone(), pair.into.id.clone(), pair.into.name.clone())
            })
            .collect::<Vec<_>>();
        for (from, from_name, into, into_name) in selected_combines {
            let path = format!("/api/inbox/projects/{}/combine", super::client::path_id(&from)?);
            let _: Value = client
                .post_json(&path, &json!({ "serverSession": client.server_session(), "into": into }))
                .with_context(|| format!("combining project {from_name} into {into_name}"))?;
        }
        if !selected_sessions.is_empty() {
            let idempotency_key = format!("tui-tidy-{}", super::send::new_key());
            let _: Value = client
                .post_json(
                    "/api/inbox/tidy",
                    &json!({
                        "serverSession": client.server_session(),
                        "sessions": selected_sessions,
                        "idempotency_key": idempotency_key,
                        "project": Value::Null,
                        "surface": "list",
                    }),
                )
                .context("starting Tidy")?;
        }
        self.confirmation = Confirmation::None;
        self.tidy_sessions.clear();
        self.tidy_combines.clear();
        self.status = Some("Tidy action sent.".to_owned());
        load_tidy(client, self)
    }
}

fn load_archive(client: &Client, history: &mut History) -> Result<()> {
    let mut query = form_urlencoded::Serializer::new(String::new());
    query.append_pair("offset", &history.offset.to_string());
    query.append_pair("limit", &ARCHIVE_PAGE.to_string());
    if let Some(project) = &history.project_filter {
        query.append_pair("project", project);
    }
    let path = format!("/api/inbox/threads/archived?{}", query.finish());
    let model: ArchivedModel = client.get_json("GET", &path).context("reading archived Inbox threads")?;
    ensure_session(client, &model.server_session)?;
    history.total = model.total;
    history.archive_projects = model.projects;
    history.rows = model.threads;
    history.offset = model.offset;
    history.cursor = history.cursor.min(history.rows.len().saturating_sub(1));
    Ok(())
}

fn load_activity(client: &Client, history: &mut History) -> Result<()> {
    if history.projects.is_empty() {
        let projects: ProjectsModel = client
            .get_json("GET", "/api/inbox/projects")
            .context("reading Inbox projects for activity filter")?;
        ensure_session(client, &projects.server_session)?;
        history.projects = projects.projects.into_iter().map(|project| (project.id, project.name)).collect();
    }
    let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    let today = now.date();
    let from_day = today - TimeDuration::days(i64::from(history.activity_days.saturating_sub(1)));
    let tomorrow = today + TimeDuration::DAY;
    let from = from_day.with_hms(0, 0, 0)?.assume_offset(now.offset());
    let to = tomorrow.with_hms(0, 0, 0)?.assume_offset(now.offset());
    let mut query = form_urlencoded::Serializer::new(String::new());
    query.append_pair("from", &from.format(&Rfc3339)?);
    query.append_pair("to", &to.format(&Rfc3339)?);
    if let Some(project) = &history.activity_project {
        query.append_pair("project", project);
    }
    let path = format!("/api/inbox/activity?{}", query.finish());
    let model: ActivityModel = client.get_json("GET", &path).context("reading Inbox activity")?;
    history.activity_cursor = history.activity_cursor.min(model.threads.len().saturating_sub(1));
    history.activity = Some(model);
    Ok(())
}

fn load_decisions(client: &Client, history: &mut History) -> Result<()> {
    let tz = OffsetDateTime::now_local().map_or(0, |now| -now.offset().whole_minutes());
    let mut query = form_urlencoded::Serializer::new(String::new());
    query.append_pair("days", &history.decisions_days.to_string());
    query.append_pair("tz", &tz.to_string());
    let path = format!("/api/inbox/decision-data?{}", query.finish());
    history.decisions = Some(client.get_json("GET", &path).context("reading Inbox decision data")?);
    Ok(())
}

fn load_storage(client: &Client, history: &mut History) -> Result<()> {
    history.storage = Some(
        client.get_json("GET", "/api/inbox/settings/storage").context("reading Inbox storage overview")?,
    );
    Ok(())
}

fn load_tidy(client: &Client, history: &mut History) -> Result<()> {
    let plan: TidyPlan =
        client.get_json("GET", "/api/inbox/tidy?surface=list").context("reading Inbox Tidy plan")?;
    ensure_session(client, &plan.server_session)?;
    history.tidy_sessions.retain(|session| plan.agents.iter().any(|agent| &agent.session == session));
    history.tidy_combines.retain(|id| plan.combine.iter().any(|pair| &pair.from.id == id));
    history.tidy_cursor =
        history.tidy_cursor.min(plan.agents.len().saturating_add(plan.combine.len()).saturating_sub(1));
    history.tidy = Some(plan);
    Ok(())
}

fn ensure_session(client: &Client, session: &str) -> Result<()> {
    anyhow::ensure!(
        session == client.server_session(),
        "Inbox serverSession changed; rediscover before continuing"
    );
    Ok(())
}

fn cycle_range(ranges: &[u16], current: u16, forward: bool) -> u16 {
    let Some(index) = ranges.iter().position(|range| *range == current) else {
        return ranges.first().copied().unwrap_or(current);
    };
    let next = if forward { (index + 1) % ranges.len() } else { (index + ranges.len() - 1) % ranges.len() };
    ranges.get(next).copied().unwrap_or(current)
}

fn bytes(n: u64) -> String {
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64;
    let mut unit = "B";
    for next in ["KiB", "MiB", "GiB", "TiB"] {
        if value < 1024.0 {
            break;
        }
        value /= 1024.0;
        unit = next;
    }
    format!("{value:.1} {unit}")
}

fn timestamp(value: Option<i64>) -> String {
    value
        .and_then(|ms| OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000).ok())
        .map_or_else(
            || "not available".to_owned(),
            |time| time.format(&Rfc3339).unwrap_or_else(|_| "not available".to_owned()),
        )
}

fn duration(value: Option<u64>) -> String {
    let Some(ms) = value else { return "not available".to_owned() };
    if ms < 60_000 {
        format!("{} s", ms / 1000)
    } else if ms < 3_600_000 {
        format!("{} min", ms / 60_000)
    } else {
        format!("{} h {} min", ms / 3_600_000, (ms % 3_600_000) / 60_000)
    }
}

fn group_line(group: &DecisionGroup) -> String {
    format!(
        "{} · answered {} · skipped {} · set aside {} · replaced {} · recommendation {}/{} · median wait {}",
        group.label,
        group.answered,
        group.skipped,
        group.set_aside,
        group.replaced,
        group.taken,
        group.offered,
        duration(group.median_wait_ms)
    )
}

fn trend_label(trend: &DecisionTrend) -> String {
    match trend {
        DecisionTrend::Recommendations { direction, now, was } => {
            format!("Recommendations {direction}: {now} vs. {was}")
        }
        DecisionTrend::Skips { direction, now, was } => format!("Skips {direction}: {now} vs. {was}"),
        DecisionTrend::Waits { direction, now_ms, was_ms } => {
            format!("Waits {direction}: {} vs. {}", duration(Some(*now_ms)), duration(Some(*was_ms)))
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic, reason = "tests assert by panicking")]
mod tests {
    use super::*;

    #[test]
    fn pagination_and_project_query_are_encoded() {
        let mut query = form_urlencoded::Serializer::new(String::new());
        query.append_pair("offset", "50");
        query.append_pair("limit", "50");
        query.append_pair("project", "prj/a b");
        assert_eq!(query.finish(), "offset=50&limit=50&project=prj%2Fa+b");
    }

    #[test]
    fn activity_and_decision_ranges_cycle_in_both_directions() {
        assert_eq!(cycle_range(&ACTIVITY_RANGES, 7, true), 14);
        assert_eq!(cycle_range(&ACTIVITY_RANGES, 1, false), 30);
        assert_eq!(cycle_range(&DECISION_RANGES, 90, true), 7);
        assert_eq!(cycle_range(&DECISION_RANGES, 30, false), 7);
    }

    #[test]
    fn storage_sizes_and_decision_waits_are_readable() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1536), "1.5 KiB");
        assert_eq!(duration(Some(42_000)), "42 s");
        assert_eq!(duration(Some(90_000)), "1 min");
        assert_eq!(duration(None), "not available");
    }

    #[test]
    fn archive_and_tidy_wire_models_accept_current_responses() {
        let archive: ArchivedModel = serde_json::from_value(json!({
            "serverSession":"s", "cursor":7, "project":null, "projects":[], "total":0,
            "offset":0, "limit":50, "threads":[]
        }))
        .expect("archived response");
        assert_eq!(archive.server_session, "s");
        assert_eq!(archive.offset, 0);
        let tidy: TidyPlan =
            serde_json::from_value(json!({ "serverSession":"s", "count":0, "agents":[], "run":null }))
                .expect("tidy response");
        assert!(tidy.combine.is_empty());
        assert!(!tidy.combine_loading);
    }

    #[test]
    fn decision_trends_deserialize_discriminated_wire_shapes() {
        let trends: Vec<DecisionTrend> = serde_json::from_value(json!([
            {"key":"recommendations","direction":"up","now":8,"was":3},
            {"key":"skips","direction":"down","now":2,"was":5},
            {"key":"waits","direction":"up","now_ms":120_000,"was_ms":60_000}
        ]))
        .expect("trends");
        assert_eq!(trends.len(), 3);
        assert!(trend_label(&trends[0]).contains("Recommendations up"));
        assert!(trend_label(&trends[2]).contains("2 min"));
    }
}
