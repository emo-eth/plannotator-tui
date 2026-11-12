use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::theme::palette;

use super::Board;
use super::forms::{FieldValue, Modal};
use super::model::{Column, initiative_of};

impl Board {
    pub(crate) fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let [header, body, status, footer] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(2),
        ])
        .areas(area);
        self.height = usize::from(body.height);
        let title = self.opened.as_ref().map_or("Board", |detail| detail.card.title.as_str());
        let project = self.project.as_deref().map_or("Every project", |id| self.project_name(id));
        let initiative = self.initiative.as_deref().map_or("Every initiative", |id| {
            if id == "none" {
                "No initiative"
            } else {
                self.model
                    .initiatives
                    .iter()
                    .find(|item| item.id == id)
                    .map_or("Deleted initiative", |item| item.name.as_str())
            }
        });
        let mut heading = vec![Line::styled(
            format!(
                "{title} · {project} · {initiative} · {} unseen cancellations",
                self.model.dropped_unseen
            ),
            Style::new().fg(palette().accent).add_modifier(Modifier::BOLD),
        )];
        if self.opened.is_none() {
            let lanes = std::iter::once("0 All".to_owned())
                .chain(Column::ALL.iter().enumerate().map(|(index, column)| {
                    let count = self
                        .model
                        .cards
                        .iter()
                        .filter(|card| {
                            card.column == *column
                                && self.project.as_deref().is_none_or(|id| card.project_id == id)
                                && self.initiative.as_deref().is_none_or(|id| {
                                    if id == "none" {
                                        initiative_of(card, &self.model.initiatives).is_none()
                                    } else {
                                        initiative_of(card, &self.model.initiatives) == Some(id)
                                    }
                                })
                        })
                        .count();
                    format!(
                        "{} {}{} ({count})",
                        index + 1,
                        if self.column == Some(*column) { ">" } else { "" },
                        column.label()
                    )
                }))
                .collect::<Vec<_>>()
                .join(" · ");
            heading.push(Line::from(lanes));
        } else {
            heading.push(Line::from("↑↓ scroll · t threads · o links · w waiting step → thread"));
        }
        frame.render_widget(Paragraph::new(heading), header);
        if self.opened.is_some() {
            self.draw_detail(frame, body);
        } else {
            self.draw_cards(frame, body);
        }
        let warning = if self.pending.is_some() || self.pending_link.is_some() {
            "Unconfirmed agent action: ! retries without sending twice. "
        } else {
            ""
        };
        frame.render_widget(
            Paragraph::new(format!("{warning}{}", self.status.as_deref().unwrap_or("")))
                .style(Style::new().fg(palette().comment)),
            status,
        );
        frame.render_widget(Paragraph::new("Enter open · n new · m move · e title · N notes · d cancel · u restore · s send · a ask · b start\np project · f initiative · I manage · / find · T tidy · R refresh · ? all keys · Esc back").style(Style::new().fg(palette().idle_fg)), footer);
        if self.help {
            Self::draw_help(frame, area);
        }
        if self.modal.is_some() {
            self.draw_modal(frame, area);
        }
    }

    fn draw_cards(&mut self, frame: &mut Frame, area: Rect) {
        if self.visible.is_empty() {
            let text = if self.model.cards.is_empty() {
                "No cards yet. n creates a Backlog card; s sends it to an agent.\nAgents can fill the Board with the Plannotator card action.\nIf Board is disabled, turn it on in Inbox Settings → Capabilities."
            } else {
                "No cards match these filters. 0 shows every lane; p / f change filters; / clears search."
            };
            frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), area);
            return;
        }
        let rows = usize::from(area.height).max(1);
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        }
        if self.cursor >= self.scroll + rows {
            self.scroll = self.cursor + 1 - rows;
        }
        for (offset, index) in self.visible.iter().skip(self.scroll).take(rows).enumerate() {
            let Some(card) = self.model.cards.get(*index) else {
                continue;
            };
            let unread = if card.unread(self.model.seen_seq) { "*" } else { " " };
            let mut spans = vec![
                Span::styled(
                    format!("{unread} {:11} ", card.column.label()),
                    Style::new().fg(match card.column {
                        Column::InReview => palette().comment,
                        Column::Done => palette().looks_good,
                        Column::Dropped => palette().delete,
                        _ => palette().idle_fg,
                    }),
                ),
                Span::styled(&card.title, Style::new().add_modifier(Modifier::BOLD)),
            ];
            if let Some(sent) = card.sent_waiting() {
                spans.push(Span::raw(format!(" · Sent to {}", sent.to.label())));
            } else if card.column == Column::InReview {
                if let Some(you) = &card.you {
                    spans.push(Span::styled(format!(" · You: {you}"), Style::new().fg(palette().comment)));
                }
            } else if let Some(said) = &card.said {
                spans.push(Span::raw(format!(" · {said}")));
            }
            if self.project.is_none() {
                spans.push(Span::styled(
                    format!(" · {}", self.project_name(&card.project_id)),
                    Style::new().fg(palette().idle_fg),
                ));
            }
            if let Some(id) = initiative_of(card, &self.model.initiatives)
                && let Some(initiative) = self.model.initiatives.iter().find(|item| item.id == id)
            {
                spans.push(Span::raw(format!(" · #{}", initiative.name)));
            }
            if let Some(mark) = self.marks.get(&card.id)
                && let Some(state) = mark.get("state").and_then(serde_json::Value::as_str)
            {
                spans.push(Span::raw(format!(" · {state}")));
            }
            let mut done = 0;
            let mut total = 0;
            for list in card.threads.iter().filter_map(|thread| self.model.steps.get(thread)).flatten() {
                let count = list.progress();
                done += count.0;
                total += count.1;
            }
            if total > 0 {
                spans.push(Span::raw(format!(" · checklist {done}/{total}")));
            }
            if let Some(drop) = &card.dropped {
                spans.push(Span::raw(format!(
                    " · {}: {}{}",
                    drop.by.label(),
                    drop.reason.as_deref().unwrap_or("No reason"),
                    if drop.by.kind == "agent" && drop.seen_at.is_none() { " (unseen)" } else { "" }
                )));
            }
            let style = if self.scroll + offset == self.cursor { palette().selection } else { Style::new() };
            frame.render_widget(
                Paragraph::new(Line::from(spans)).style(style),
                Rect { y: area.y.saturating_add(offset as u16), height: 1, ..area },
            );
        }
    }

    fn draw_modal(&self, frame: &mut Frame, area: Rect) {
        let Some(modal) = &self.modal else {
            return;
        };
        let rect = centered(area, 90, area.height.saturating_sub(2).min(28));
        frame.render_widget(Clear, rect);
        let title = match modal {
            Modal::Form(form) => form.title.as_str(),
            Modal::Pick(picker) => picker.title.as_str(),
            Modal::Confirm { .. } => "Confirm",
        };
        let block =
            Block::default().borders(Borders::ALL).title(title).style(Style::new().bg(palette().toolbar_bg));
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        match modal {
            Modal::Confirm { prompt, .. } => {
                frame.render_widget(Paragraph::new(prompt.as_str()).wrap(Wrap { trim: false }), inner);
            }
            Modal::Pick(picker) => {
                let height = usize::from(inner.height.saturating_sub(1)).max(1);
                let start = picker.cursor.saturating_sub(height.saturating_sub(1));
                let mut lines: Vec<_> = picker
                    .items
                    .iter()
                    .enumerate()
                    .skip(start)
                    .take(height)
                    .map(|(index, (label, _))| {
                        Line::styled(
                            label.as_str(),
                            if index == picker.cursor { palette().selection } else { Style::new() },
                        )
                    })
                    .collect();
                if lines.is_empty() {
                    lines.push(Line::from("Nothing available. Esc closes."));
                } else {
                    lines.push(Line::from("↑↓ choose · Enter select · Esc cancel"));
                }
                frame.render_widget(Paragraph::new(lines), inner);
            }
            Modal::Form(form) => {
                let mut lines = Vec::new();
                let mut cursor = None;
                for (index, field) in form.fields.iter().enumerate() {
                    let active = index == form.cursor;
                    lines.push(Line::styled(
                        format!("{} {}", if active { ">" } else { " " }, field.label),
                        Style::new().fg(palette().accent),
                    ));
                    match &field.value {
                        FieldValue::Text(editor) => {
                            let (wrapped, row, col) = editor.wrapped(usize::from(inner.width.max(1)));
                            if active {
                                cursor = Some((lines.len() + row, col));
                            }
                            lines.extend(wrapped.into_iter().map(Line::from));
                        }
                        FieldValue::Choice { items, cursor: selected } => {
                            lines.push(Line::styled(
                                format!(
                                    "← {} →",
                                    items.get(*selected).map_or("No choices", |(label, _)| label.as_str())
                                ),
                                if active { palette().selection } else { Style::new() },
                            ));
                        }
                    }
                }
                lines.push(Line::from("Tab field · ←→ choose · Enter save · Alt+Enter newline · Esc cancel"));
                if let Some(status) = &self.status {
                    lines.push(Line::styled(status.as_str(), Style::new().fg(palette().comment)));
                }
                let offset = cursor
                    .map_or(0, |(row, _)| row.saturating_sub(usize::from(inner.height.saturating_sub(2))));
                frame.render_widget(
                    Paragraph::new(lines).scroll((offset.min(u16::MAX as usize) as u16, 0)),
                    inner,
                );
                if let Some((row, col)) = cursor
                    && row >= offset
                    && row - offset < usize::from(inner.height)
                {
                    frame.set_cursor_position((
                        inner.x.saturating_add(col as u16).min(inner.right().saturating_sub(1)),
                        inner.y.saturating_add((row - offset) as u16),
                    ));
                }
            }
        }
    }

    fn draw_help(frame: &mut Frame, area: Rect) {
        let rect = centered(area, 98, 22);
        frame.render_widget(Clear, rect);
        let text = "Board keys\n↑↓ / j k / PgUp PgDn: browse; Enter: card details; Esc: back\n0: all lanes; 1 Backlog; 2 Todo; 3 In progress; 4 In review; 5 Done; 6 Cancelled\np: project filter; f: initiative filter; /: find title or notes (empty clears)\nn: new card; m: move; d: cancel with reason and confirmation; u: restore\ne: rename; N: notes (empty clears); l: link issue/PR/URL; L: link thread id\ni: card initiative; I: create/rename/delete/restore initiatives, tagged threads\ns: send to live/new agent; c: create linked thread without moving card\nb: tell a live agent to start; a: ask for update; !: finish unconfirmed action\nt: linked threads; w: waiting checklist → thread; o: external links\nv: mark Board read through displayed cursor; V: mark shown cancellations seen\nM: refresh visible forge marks; may auto-move merged work to Done\nR: refresh cards/history/checklists; ?: help\n\nChecklists are agent-owned and read-only, exactly as in the web Inbox.\nEnded sessions retain dated steps, never imply a live wait.\nForms: Tab next field, arrows change choice, Enter saves, Esc cancels.\nErrors retain form text. Alt+Enter inserts a newline in notes.\n\nAny key closes help.";
        frame.render_widget(
            Paragraph::new(text).block(Block::default().borders(Borders::ALL)).wrap(Wrap { trim: false }),
            rect,
        );
    }
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}
