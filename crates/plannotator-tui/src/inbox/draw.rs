//! Drawing the Inbox: a one-line header, the list or the open thread, a one-line footer, and
//! the compose box floating over the thread. Pure over [`Inbox`] except for recording the
//! body's size, which paging and the thread's layout need.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize as _};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use time::OffsetDateTime;
use unicode_width::UnicodeWidthStr as _;

use super::client::Row;
use super::format::{ago, author, title};
use super::ui::{ComposeFor, Entry, Inbox, LiveState, Mode, Overlay};
use crate::theme::palette;

const COMPOSE_WIDTH: u16 = 64;
/// One column of air on each side of the body.
const MARGIN: u16 = 1;

fn section_color(id: &str) -> Color {
    match id {
        "stopped" | "holding" => palette().delete,
        "waiting" => palette().comment,
        "new" => palette().accent,
        _ => palette().idle_fg,
    }
}

/// What a row says about its questions: open ones first, then picks waiting for a Send.
fn badge(row: &Row) -> Option<String> {
    let q = &row.questions;
    if q.open > 0 {
        let noun = if q.open == 1 { "question" } else { "questions" };
        let stopped = if q.stopped { " · stopped" } else { "" };
        Some(format!("{} {noun}{stopped}", q.open))
    } else if row.answered_not_sent || q.picked > 0 {
        Some("ready to send".to_owned())
    } else {
        None
    }
}

impl Inbox {
    pub(crate) fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let [header, body, footer] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(1), Constraint::Length(1)]).areas(area);
        let inner = Rect {
            x: body.x + MARGIN,
            y: body.y,
            width: body.width.saturating_sub(2 * MARGIN),
            height: body.height,
        };
        self.body_height = usize::from(inner.height);
        self.body_width = usize::from(inner.width.max(1));
        self.draw_header(frame, header);
        let height = self.body_height;
        let width = self.body_width;
        match self.thread.as_mut().filter(|_| self.mode != Mode::List) {
            Some(thread) => {
                thread.reflow(width);
                thread.ensure_visible(height);
                let lines = thread.lines();
                for (i, line) in lines.iter().skip(thread.scroll).take(height).enumerate() {
                    let row = Rect { y: inner.y + i as u16, height: 1, ..inner };
                    frame.render_widget(Paragraph::new(line.clone()), row);
                }
            }
            None => self.draw_list(frame, inner),
        }
        self.draw_footer(frame, footer);
        if let Mode::Compose(purpose) = self.mode.clone() {
            self.draw_compose(frame, &purpose);
        }
        match self.overlay {
            Overlay::Closed => {}
            Overlay::Features(cursor) => self.draw_features(frame, cursor),
            Overlay::Help => Self::draw_help(frame),
        }
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let live = match &self.live {
            LiveState::Snapshot => Span::styled(" snapshot · read only ", Style::new().dim()),
            LiveState::Connecting => Span::styled(" connecting… ", Style::new().fg(palette().comment)),
            LiveState::Live => Span::styled(" live ", Style::new().fg(palette().looks_good)),
            LiveState::Lost(_) => Span::styled(" live updates lost ", Style::new().fg(palette().delete)),
        };
        let title = Span::styled(
            format!(" {}", self.title()),
            Style::new().fg(palette().accent).add_modifier(Modifier::BOLD),
        );
        let live_width = live.width() as u16;
        let [left, right] =
            Layout::horizontal([Constraint::Min(1), Constraint::Length(live_width)]).areas(area);
        frame.render_widget(Paragraph::new(Line::from(title)), left);
        frame.render_widget(Paragraph::new(Line::from(live)), right);
    }

    fn draw_list(&mut self, frame: &mut Frame, area: Rect) {
        let height = usize::from(area.height);
        self.ensure_cursor_visible(height);
        let Some(list) = &self.list else {
            frame.render_widget(Paragraph::new(Line::from(Span::raw("Loading…").dim())), area);
            return;
        };
        if self.entries.is_empty() {
            let words = list.notice.clone().unwrap_or_else(|| "No threads.".to_owned());
            frame.render_widget(Paragraph::new(Line::from(Span::raw(words).dim())), area);
            return;
        }
        let now = OffsetDateTime::now_utc();
        let width = usize::from(area.width);
        for (i, entry) in self.entries.iter().skip(self.list_scroll).take(height).enumerate() {
            let selected = self.list_scroll + i == self.cursor;
            let line = match *entry {
                Entry::Header(s) => {
                    let Some(section) = list.sections.get(s) else { continue };
                    let fold = if self.is_collapsed(&section.id) { "▸" } else { "▾" };
                    let text = format!("{fold} {} ({})", section.label, section.threads.len());
                    Line::from(Span::styled(
                        text,
                        Style::new().fg(section_color(&section.id)).add_modifier(Modifier::BOLD),
                    ))
                }
                Entry::Row(s, r) => {
                    let Some(row) = list.sections.get(s).and_then(|section| section.threads.get(r)) else {
                        continue;
                    };
                    row_line(row, now, width)
                }
            };
            let line = if selected { line.patch_style(palette().selection) } else { line };
            let rect = Rect { y: area.y + i as u16, height: 1, ..area };
            if selected {
                frame.render_widget(Paragraph::new("").style(palette().selection), rect);
            }
            frame.render_widget(Paragraph::new(line), rect);
        }
    }

    /// The key help for where the person is, in the footer's right half.
    fn help(&self) -> &'static str {
        match &self.mode {
            Mode::List => "↑↓ move · enter open · / search · F2 features · ? help · q quit",
            Mode::Compose(_) if self.shift_enter => {
                "enter submits · shift+enter new line · F2 draft · esc cancels"
            }
            Mode::Compose(_) => "enter submits · alt+enter new line · F2 draft · esc cancels",
            Mode::Thread if self.thread.as_ref().is_some_and(|t| t.focus().is_some()) => {
                "↑↓ choice · enter pick · 1-9 pick · o other · t text · N note · x clear · tab next · esc done"
            }
            Mode::Thread => "tab question · r reply · s send · F2 features · ? help · esc back",
        }
    }

    fn draw_footer(&self, frame: &mut Frame, area: Rect) {
        let mut parts: Vec<String> = self.status.iter().cloned().collect();
        if let Some(thread) = self.thread.as_ref().filter(|_| self.mode != Mode::List) {
            let total = thread.lines().len();
            let end = (thread.scroll + self.body_height).min(total);
            parts.push(format!("{}-{end}/{total}", (thread.scroll + 1).min(total)));
            let open = thread.open_questions();
            if open > 0 {
                parts.push(format!("{open} open question{}", if open == 1 { "" } else { "s" }));
            }
        }
        let help = format!("{} ", self.help());
        let status_width = self.status.as_ref().map_or(0, |s| s.width() + 1) as u16;
        let help_width = (help.width() as u16).min(area.width.saturating_sub(status_width.max(10)));
        let [left, right] =
            Layout::horizontal([Constraint::Min(10), Constraint::Length(help_width)]).areas(area);
        let status_style =
            if self.status.is_some() { Style::new().fg(palette().comment) } else { Style::new().dim() };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(format!(" {}", parts.join(" · ")), status_style))),
            left,
        );
        frame.render_widget(Paragraph::new(Line::from(Span::raw(help).dim()).right_aligned()), right);
    }

    fn draw_compose(&self, frame: &mut Frame, purpose: &ComposeFor) {
        let area = frame.area();
        let width = COMPOSE_WIDTH.min(area.width.saturating_sub(2)).max(10).min(area.width);
        let wrap_width = usize::from(width.saturating_sub(3));
        let (lines, cursor_row, cursor_col) = self.compose.wrapped(wrap_width);
        let content_rows = lines.len().clamp(1, 8).min(usize::from(area.height.saturating_sub(3)).max(1));
        let height = content_rows as u16 + 2;
        let rect = Rect {
            x: area.x + (area.width.saturating_sub(width)) / 2,
            y: area.bottom().saturating_sub(height + 1).max(area.y),
            width,
            height,
        }
        .clamp(area);
        if rect.height < 3 || rect.width < 4 {
            return;
        }
        let verb = match purpose {
            ComposeFor::Reply => "reply · enter sends",
            ComposeFor::Text { .. } => "answer · enter saves",
            ComposeFor::Other { .. } => "other · enter saves",
            ComposeFor::Note { .. } => "note · enter saves",
        };
        let newline = if self.shift_enter { "shift+enter new line" } else { "alt+enter new line" };
        let title = format!(" {verb} · {newline} · esc cancels ");
        frame.render_widget(Clear, rect);
        let boxed = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(palette().comment))
            .title(Span::styled(title, Style::new().dim()));
        let inner = boxed.inner(rect);
        frame.render_widget(boxed, rect);
        let content_rows = usize::from(inner.height).max(1);
        let scroll = cursor_row.saturating_sub(content_rows - 1);
        for (i, line) in lines.iter().skip(scroll).take(content_rows).enumerate() {
            let row = Rect {
                x: inner.x + 1,
                y: inner.y + i as u16,
                width: inner.width.saturating_sub(1),
                height: 1,
            };
            frame.render_widget(Paragraph::new(Line::from(line.clone())), row);
        }
        let cursor_x = inner.x + 1 + cursor_col as u16;
        let cursor_y = inner.y + (cursor_row - scroll) as u16;
        frame.set_cursor_position((cursor_x.min(inner.right().saturating_sub(1)), cursor_y));
    }

    fn draw_features(&self, frame: &mut Frame, cursor: usize) {
        let area = frame.area();
        let width = area.width.saturating_sub(4).min(84);
        let height = area.height.saturating_sub(2).min(super::features::ALL.len() as u16 + 2);
        let rect = Rect::new(
            area.x + area.width.saturating_sub(width) / 2,
            area.y + area.height.saturating_sub(height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, rect);
        let block = Block::bordered().title(" Inbox features · ↑↓ enter · esc back ");
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        let start = cursor.saturating_sub(usize::from(inner.height).saturating_sub(1));
        for (index, feature) in
            super::features::ALL.iter().enumerate().skip(start).take(usize::from(inner.height))
        {
            let unavailable = feature.needs_thread() && self.context_thread_id().is_none();
            let style = if index == cursor {
                palette().selection
            } else if unavailable {
                Style::new().dim()
            } else {
                Style::default()
            };
            let text = format!(" {}{}", feature.label(), if unavailable { " (select a thread)" } else { "" });
            frame.render_widget(
                Paragraph::new(text).style(style),
                Rect::new(inner.x, inner.y + (index - start) as u16, inner.width, 1),
            );
        }
    }

    fn draw_help(frame: &mut Frame) {
        let area = frame.area();
        frame.render_widget(Clear, area);
        let lines = vec![
            "Inbox · keyboard help",
            "",
            "F2  All features: board, sessions, attachments, PRs, reviews, actions,",
            "    search, decisions, agents, projects, Ask AI, drafts, artifacts,",
            "    Shapes/System, media, machines, history, sharing, settings and updates.",
            "/   Search                         L   Sessions / permission requests",
            "",
            "List: ↑↓ or j/k move; Enter opens / folds; ← folds; r refreshes.",
            "Thread: ↑↓ scroll; PgUp/PgDn page; Home/End; R refreshes; Esc back.",
            "Questions: Tab / Shift+Tab next / previous; ↑↓ choice; Enter or 1–9 pick;",
            "           o Other; t text; N note; x clear; Esc leaves the card.",
            "r writes a reply; s sends saved picks, staged actions and annotations.",
            "s excludes saved reply words/files. r opens them; Enter sends visible reply.",
            "Reply drafts, files, recipients and conflicts: F2 → Reply draft (also F2 in compose).",
            "Linked-machine drafts survive closing/reopening a thread, but not quitting.",
            "Permission requests must use L; generic picks never grant permissions.",
            "Action item Done / Can't / notes / undo are under F2 → Thread actions.",
            "Compose: Enter submits; Shift+Enter (or Alt+Enter) adds a line; Esc cancels.",
            "",
            "An unconfirmed send is frozen: s retries its original key and payload.",
            "D twice discards it (it may already have arrived); do not resend as new.",
            "Feature panels stay closed while a send is unconfirmed.",
            "--snapshot is GET-only: no event stream, picks, replies or seen writes.",
            "",
            "Esc / ? / F1 closes this help. Ctrl+C quits.",
        ];
        frame.render_widget(
            Paragraph::new(lines.into_iter().map(Line::from).collect::<Vec<_>>())
                .block(Block::bordered().title(" Help "))
                .wrap(ratatui::widgets::Wrap { trim: false }),
            area,
        );
    }
}

/// One thread of the list: its title, bold while unread, and on the right what waits in it,
/// its project and when it last moved.
fn row_line(row: &Row, now: OffsetDateTime, width: usize) -> Line<'static> {
    let mut meta: Vec<String> = Vec::new();
    meta.extend(badge(row));
    meta.push(row.project.name.clone());
    meta.push(ago(&row.last_at, now));
    let meta = meta.join(" · ");
    let marker = if row.unread { "● " } else { "  " };
    let mut name = title(row.subject.as_deref(), row.thread_name.as_deref(), &row.thread_id);
    let who = author(&row.author);
    if who != "you" {
        name = format!("{who}: {name}");
    }
    let room = width.saturating_sub(2 + marker.width() + meta.width() + 2);
    let name = clip(&name, room);
    let pad = width.saturating_sub(2 + marker.width() + name.width() + meta.width());
    let title_style = if row.unread { Style::new().add_modifier(Modifier::BOLD) } else { Style::new() };
    let badge_style = if row.questions.stopped {
        Style::new().fg(palette().delete)
    } else if row.questions.open > 0 {
        Style::new().fg(palette().comment)
    } else {
        Style::new().dim()
    };
    Line::from(vec![
        Span::raw("  "),
        Span::styled(marker, Style::new().fg(palette().accent)),
        Span::styled(name, title_style),
        Span::raw(" ".repeat(pad)),
        Span::styled(meta, badge_style),
    ])
}

/// `text` cut to `width` columns, ending in an ellipsis when it was longer.
fn clip(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}
