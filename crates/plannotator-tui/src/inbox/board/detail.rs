use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use serde_json::Value;

use crate::theme::palette;

use super::Board;
use super::model::{Author, Card, Column, Initiative};

impl Board {
    pub(super) fn draw_detail(&mut self, frame: &mut Frame, area: Rect) {
        if self.detail_width != area.width || self.detail_rows.is_empty() {
            self.detail_width = area.width;
            self.detail_rows = self
                .detail_lines()
                .iter()
                .flat_map(|line| {
                    let offsets =
                        vec![None; line.spans.iter().map(|span| span.content.chars().count()).sum()];
                    crate::wrap::wrap_line(line, &offsets, usize::from(area.width.max(1)))
                        .into_iter()
                        .map(|row| row.line)
                })
                .collect();
            self.detail_width = area.width;
        }
        let max_scroll = self.detail_rows.len().saturating_sub(usize::from(area.height));
        self.detail_scroll = self.detail_scroll.min(max_scroll.min(u16::MAX as usize) as u16);
        for (offset, line) in self
            .detail_rows
            .iter()
            .skip(usize::from(self.detail_scroll))
            .take(usize::from(area.height))
            .enumerate()
        {
            frame.render_widget(
                Paragraph::new(line.clone()),
                Rect { y: area.y.saturating_add(offset as u16), height: 1, ..area },
            );
        }
    }

    pub(super) fn detail_lines(&self) -> Vec<Line<'static>> {
        let Some(detail) = &self.opened else {
            return Vec::new();
        };
        let card = &detail.card;
        let mut lines = vec![Line::from(format!(
            "{} · {} · {}",
            card.column.label(),
            self.project_name(&card.project_id),
            card.id
        ))];
        if let Some(agent) = &card.agent {
            lines.push(Line::from(format!(
                "Agent: {} · {}",
                agent.label(),
                agent.session.as_deref().unwrap_or("")
            )));
        }
        if let Some(sent) = card.sent_waiting() {
            lines.push(Line::from(format!("Sent to {} at {}", sent.to.label(), sent.at)));
        }
        if let Some(initiative) = card.initiative.as_deref().and_then(|id| {
            self.model.initiatives.iter().find(|item| item.id == id && item.deleted_at.is_none())
        }) {
            lines.push(Line::from(format!("Initiative: {}", initiative.name)));
            if let Some(tagged) = self.model.initiative_threads.get(&initiative.id) {
                lines.push(Line::from(format!(
                    "{} other tagged threads (I → initiative opens the latest {})",
                    tagged.total,
                    tagged.threads.len()
                )));
            }
        }
        lines.push(Line::from(format!("Last update: {}", card.heard_at)));
        if card.column == Column::InReview
            && let Some(you) = &card.you
        {
            lines.push(Line::styled(format!("You: {you}"), Style::new().fg(palette().comment)));
        }
        if let Some(said) = &card.said {
            lines.push(Line::from(format!("Agent: {said}")));
        }
        if let Some(drop) = &card.dropped {
            lines.push(Line::styled(
                format!(
                    "Cancelled by {} at {}: {}",
                    drop.by.label(),
                    drop.at,
                    drop.reason.as_deref().unwrap_or("No reason")
                ),
                Style::new().fg(palette().delete),
            ));
        }
        lines.push(heading("Notes · N edits, empty clears"));
        let notes = crate::doc::Document::parse(card.notes.as_deref().unwrap_or("No notes.").to_owned());
        let notes_layout = crate::layout::DocLayout::build(&notes, usize::from(self.detail_width.max(1)));
        for block in &notes_layout.blocks {
            lines.extend(block.rows.iter().map(|row| row.line.clone()));
            lines.push(Line::default());
        }
        self.append_threads(&mut lines, card);
        lines.push(heading("Links · o opens, l adds"));
        if card.references().is_empty() {
            lines.push(Line::from("No links."));
        }
        for reference in card.references() {
            let resolved = self.refs.iter().find(|item| item.reference.url == reference.url);
            let data = resolved.and_then(|item| item.result.get("data"));
            let title =
                data.and_then(|data| data.get("title")).and_then(Value::as_str).unwrap_or(&reference.label);
            let state = data.and_then(|data| data.get("state")).and_then(Value::as_str).unwrap_or("link");
            let closed = data.and_then(|data| data.get("closedAt")).and_then(Value::as_str).unwrap_or("");
            lines.push(Line::from(format!(
                "{} {} · {title} · {state} {closed}",
                reference.source, reference.kind
            )));
            if let Some(checks) = data.and_then(|data| data.pointer("/pull/checks")) {
                lines.push(Line::from(format!("Checks: {checks}")));
            }
            lines.push(Line::from(reference.url.clone()));
            if let Some(error) = resolved.filter(|item| item.result.get("ok") == Some(&Value::Bool(false))) {
                lines.push(Line::from(format!("Link state unavailable: {}", error.result)));
            }
        }
        lines.push(heading("History · newest first"));
        for entry in &detail.history {
            lines.push(Line::from(history_line(entry, &self.model.initiatives)));
        }
        lines
    }

    fn append_threads(&self, lines: &mut Vec<Line<'static>>, card: &Card) {
        lines.push(heading("Threads · t opens"));
        if card.threads.is_empty() {
            lines.push(Line::from("None. c creates a conversation; L links an existing thread."));
        }
        for thread in &card.threads {
            let row = self.model.threads.get(thread);
            let subject = row.and_then(|row| row.get("subject")).and_then(Value::as_str).unwrap_or(thread);
            let section = row.and_then(|row| row.get("section")).and_then(Value::as_str).unwrap_or("");
            lines.push(Line::from(format!("{subject} · {section} · {thread}")));
            for list in
                self.model.steps.get(thread).into_iter().flatten().filter(|list| !list.items.is_empty())
            {
                let (done, total) = list.progress();
                let as_of = if list.presence == "ended" {
                    format!(" · as of {} (session ended)", list.updated_at)
                } else {
                    format!(" · {}", list.presence)
                };
                lines.push(heading(&format!("Checklist · {} · {done}/{total}{as_of}", list.agent.label())));
                let waits = list.presence != "ended"
                    && row.and_then(|row| row.get("steps_waiting")).and_then(Value::as_bool) == Some(true);
                for step in &list.items {
                    let (glyph, status) = match step.status.as_str() {
                        "done" => ("[x]", "Done"),
                        "doing" => ("[>]", "In progress"),
                        "waiting" if waits => ("[!]", "Waiting on you · w opens thread"),
                        "waiting" if list.presence == "ended" => ("[ ]", "Was waiting (session ended)"),
                        "waiting" => ("[ ]", "Waiting step (not currently waiting on you)"),
                        "dropped" => ("[-]", "Cancelled"),
                        _ => ("[ ]", "Todo"),
                    };
                    let note = step.note.as_deref().map_or(String::new(), |note| format!(" · {note}"));
                    let at = if step.status == "done" { format!(" · {}", step.at) } else { String::new() };
                    lines.push(Line::from(format!("{glyph} {} · {status}{note}{at}", step.title)));
                }
            }
        }
    }
}

fn heading(text: &str) -> Line<'static> {
    Line::styled(text.to_owned(), Style::new().fg(palette().accent))
}

pub(super) fn history_line(entry: &Value, initiatives: &[Initiative]) -> String {
    let text = |key: &str| entry.get(key).and_then(Value::as_str).unwrap_or("");
    let by = entry
        .get("by")
        .cloned()
        .and_then(|value| serde_json::from_value::<Author>(value).ok())
        .unwrap_or_default();
    let column = entry
        .get("column")
        .cloned()
        .and_then(|value| serde_json::from_value::<Column>(value).ok())
        .map_or("", Column::label);
    let words = match text("kind") {
        "created" => format!("Created in {column} by {}", by.label()),
        "moved" => format!("Moved to {column} by {} {}", by.label(), text("you")),
        "restored" => format!("Restored to {column} by {}", by.label()),
        "dropped" => format!("Cancelled by {}: {}", by.label(), text("reason")),
        "said" => format!("{}: {}", by.label(), text("text")),
        "you" => format!("{} asked you: {}", by.label(), text("text")),
        "linked_thread" => format!("Linked thread {} by {}", text("thread_id"), by.label()),
        "linked_ref" => format!("Linked {} by {}", text("ref"), by.label()),
        "renamed" => format!("Renamed to {} by {}", text("title"), by.label()),
        "notes" => format!("Notes edited by {}", by.label()),
        "initiative" => {
            let id = text("initiative");
            let name = initiatives
                .iter()
                .find(|item| item.id == id)
                .map_or(if id.is_empty() { "None" } else { "Deleted initiative" }, |item| item.name.as_str());
            format!("Initiative: {name} by {}", by.label())
        }
        "sent" => {
            let to = entry
                .get("to")
                .cloned()
                .and_then(|value| serde_json::from_value::<Author>(value).ok())
                .unwrap_or_default();
            format!(
                "Sent to {} in {} thread",
                to.label(),
                if entry.get("existing_thread") == Some(&Value::Bool(true)) { "existing" } else { "new" }
            )
        }
        "auto_shipped" => {
            format!("Done automatically: merged {}", entry.get("prs").map_or(String::new(), Value::to_string))
        }
        other => format!("{other}: {entry}"),
    };
    format!("{} · {words}", text("at"))
}
