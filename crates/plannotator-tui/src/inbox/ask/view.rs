use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr as _;

use super::model::shown_question;
use super::{ChatState, Confirm, Pane, newest_answer};

impl Pane {
    pub(crate) fn draw(&mut self, frame: &mut Frame, area: Rect) {
        frame.render_widget(Clear, area);
        let [header, body, footer_area] =
            Layout::vertical([Constraint::Length(2), Constraint::Min(3), Constraint::Length(3)]).areas(area);
        let title = self
            .current
            .as_ref()
            .and_then(|id| self.states.get(id))
            .and_then(|s| s.model.as_ref())
            .map_or("Ask AI", |model| model.subject.as_deref().unwrap_or("Ask AI · no subject"));
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("Ask AI · ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(title),
            ]))
            .block(Block::default().borders(Borders::BOTTOM)),
            header,
        );
        if self.history {
            self.draw_history(frame, body);
        } else {
            self.draw_chat(frame, body);
        }
        let footer = if let Some(confirm) = self.confirm {
            match confirm {
                Confirm::End => {
                    "End this conversation permanently, stopping its answer? No undo. y confirms; any other key cancels."
                }
                Confirm::Interrupt => {
                    "Interrupt the agent's current work and ask this question now? y confirms; any other key cancels."
                }
                Confirm::Discard => {
                    "Forget the retained send id? It may already have been saved. Words stay in the box. y confirms."
                }
            }
        } else if self.editing {
            "Enter send · Alt+Enter/Ctrl+J newline · Esc retain draft and leave editor"
        } else if self.history {
            "j/k select · Enter open chat · h return · r refresh · Esc close (drafts retained)"
        } else {
            "i compose · h history · [/] turn · j/k scroll · a retry · s stop · R reply · o board\nw wait if busy · I interrupt · y/n permission · m mark seen · X end · D discard pending · r refresh · Esc close"
        };
        frame.render_widget(
            Paragraph::new(footer).wrap(Wrap { trim: false }).block(Block::default().borders(Borders::TOP)),
            footer_area,
        );
        if let Some(notice) = &self.notice {
            let warning = Rect { x: header.x, y: header.y.saturating_add(1), width: header.width, height: 1 };
            frame.render_widget(
                Paragraph::new(notice.as_str()).style(Style::default().fg(Color::Yellow)),
                warning,
            );
        }
    }

    fn draw_history(&self, frame: &mut Frame, area: Rect) {
        let mut lines = vec![Line::styled(
            "Chat history — newest activity first",
            Style::default().add_modifier(Modifier::BOLD),
        )];
        if self.chats.is_empty() {
            lines.push(Line::raw(if self.list_read.active {
                "Reading chats…"
            } else {
                "No saved chats. Open Ask from a thread to start one."
            }));
        }
        let visible = usize::from(area.height.saturating_sub(2));
        let start = self.history_index.saturating_sub(visible.saturating_sub(1));
        for (index, chat) in self.chats.iter().enumerate().skip(start).take(visible) {
            let mark = if self.current.as_deref() == Some(&chat.thread_id) {
                "current"
            } else if chat.answering {
                "answering"
            } else if chat.unread {
                "new answer"
            } else {
                ""
            };
            let style = if index == self.history_index {
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            lines.push(Line::styled(
                format!(
                    "{} {} · {} · {} · {} · {mark}",
                    if index == self.history_index { ">" } else { " " },
                    chat.subject.as_deref().unwrap_or("(no subject)"),
                    chat.project,
                    chat.host.as_deref().unwrap_or("agent"),
                    chat.at
                ),
                style,
            ));
        }
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
    }

    fn draw_chat(&mut self, frame: &mut Frame, area: Rect) {
        let Some(id) = self.current.as_ref() else {
            frame.render_widget(
                Paragraph::new("Choose a chat in history (h), or open Ask from a thread."),
                area,
            );
            return;
        };
        let Some(state) = self.states.get_mut(id) else { return };
        let composer_height = 6.min(area.height.saturating_sub(1));
        let [conversation, composer] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(composer_height)]).areas(area);
        let mut lines = conversation_lines(state);
        if let Some(live) = &state.live {
            for permission in &live.permissions {
                if permission.decided.is_some() {
                    continue;
                }
                lines.push(Line::styled(
                    format!("Permission: {}", permission.title.as_deref().unwrap_or(&permission.tool_name)),
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                ));
                if let Some(description) = &permission.description {
                    append(&mut lines, description, Style::default());
                }
                append(&mut lines, &permission.tool_input.to_string(), Style::default());
                lines.push(Line::raw(if permission.pending {
                    "Sending decision…"
                } else {
                    "y allows · n denies (first undecided permission)"
                }));
            }
        }
        if let Some(pending) = &state.pending {
            lines.push(Line::styled(
                "Retained send · a sends/retries this exact request",
                Style::default().fg(Color::Yellow),
            ));
            append(&mut lines, &pending.text, Style::default());
        }
        if state.posting {
            lines.push(Line::raw(
                "Sending question… (original words and turn id retained until acknowledged)",
            ));
        }
        if state.stopping {
            lines.push(Line::raw("Stopping this turn…"));
        }
        if let Some(problem) = &state.problem {
            lines.push(Line::styled(problem.clone(), Style::default().fg(Color::Yellow)));
        }
        let width = usize::from(conversation.width.max(1));
        let height = lines
            .iter()
            .map(|line| {
                let columns: usize = line.spans.iter().map(|span| span.content.width()).sum();
                columns.max(1).div_ceil(width)
            })
            .sum::<usize>();
        let max_scroll =
            height.saturating_sub(usize::from(conversation.height)).min(usize::from(u16::MAX)) as u16;
        state.scroll = state.scroll.min(max_scroll);
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((state.scroll, 0)),
            conversation,
        );
        let inner = Block::default().borders(Borders::ALL).title(if self.editing {
            "Question · Enter sends"
        } else {
            "Draft retained · i edits"
        });
        let box_area = inner.inner(composer);
        let (wrapped, cursor_row, cursor_col) = state.compose.wrapped(usize::from(box_area.width.max(1)));
        let first = cursor_row.saturating_sub(usize::from(box_area.height.saturating_sub(1)));
        frame.render_widget(inner, composer);
        frame.render_widget(
            Paragraph::new(wrapped.into_iter().skip(first).map(Line::raw).collect::<Vec<_>>()),
            box_area,
        );
        if self.editing && box_area.width > 0 && box_area.height > 0 {
            frame.set_cursor_position((
                box_area
                    .x
                    .saturating_add(cursor_col.min(usize::from(box_area.width.saturating_sub(1))) as u16),
                box_area.y.saturating_add(
                    cursor_row.saturating_sub(first).min(usize::from(box_area.height.saturating_sub(1)))
                        as u16,
                ),
            ));
        }
    }
}

fn conversation_lines(state: &ChatState) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(model) = &state.model {
        let agent = if model.agent.name.is_empty() { "The agent" } else { &model.agent.name };
        let unseen = newest_answer(model.conversation.as_ref()).is_some_and(|at| {
            model.conversation.as_ref().and_then(|c| c.seen_at.as_deref()).is_none_or(|seen| at > seen)
        });
        lines.push(Line::raw(format!(
            "{} · {} ({}) · {}{}",
            model.project,
            agent,
            model.agent.host.as_deref().unwrap_or("unknown host"),
            if model.can_ask { "available" } else { "no live connection" },
            if unseen { " · new answer (m marks seen)" } else { "" }
        )));
        if model.damaged {
            lines.push(Line::styled("Conversation file is damaged. The next question sets it aside and reports where; your words remain retained on refusal.", Style::default().fg(Color::Yellow)));
        }
        if state.turns().is_empty() {
            lines.push(Line::raw(format!("Ask {agent} about this thread. Press i to write.")));
        }
        for (index, turn) in state.turns().iter().enumerate() {
            lines.push(Line::raw(""));
            let selected = index == state.selected;
            lines.push(Line::styled(
                format!("{} You · {} · {}", if selected { ">" } else { " " }, index + 1, turn.at),
                Style::default()
                    .fg(if selected { Color::Cyan } else { Color::White })
                    .add_modifier(Modifier::BOLD),
            ));
            append(&mut lines, &shown_question(turn), Style::default());
            if let Some(passage) = &turn.passage {
                append(&mut lines, passage, Style::default().fg(Color::DarkGray));
            }
            let live = state
                .live
                .as_ref()
                .filter(|l| l.turn_id == turn.id)
                .or_else(|| state.retained_answers.get(&turn.id));
            let text = turn
                .answer
                .as_deref()
                .or_else(|| live.filter(|l| !l.text.is_empty()).map(|l| l.text.as_str()));
            if let Some(text) = text {
                lines.push(Line::styled(
                    format!("{} · {}", agent, turn.host.as_deref().unwrap_or("agent")),
                    Style::default().fg(Color::Green),
                ));
                append(&mut lines, text, Style::default());
            }
            if let Some(shape) = &turn.shape {
                lines.push(Line::styled(
                    format!("Shape {shape} · o opens the selected turn's board"),
                    Style::default().fg(Color::Cyan),
                ));
            }
            if turn.stopped {
                lines.push(Line::raw("Stopped."));
            }
            if let Some(failure) = &turn.failure {
                let note = match failure.as_str() {
                    "gone" => "The agent went away before finishing.",
                    "taken_over" => "Another message took over the session; this partial answer remains.",
                    _ => "The agent could not finish this answer.",
                };
                lines.push(Line::styled(note, Style::default().fg(Color::Yellow)));
            }
            if let Some(error) = live.and_then(|l| l.error.as_deref()) {
                lines.push(Line::styled(error.to_owned(), Style::default().fg(Color::Yellow)));
                if live.is_some_and(|l| l.error_code.as_deref() == Some("agent_busy")) {
                    lines.push(Line::raw(
                        "Agent busy: w asks when it finishes; I asks to interrupt; R replies in the thread.",
                    ));
                }
            }
            if state.running() == Some(turn.id.as_str()) {
                let status = live.and_then(|l| l.status.as_deref()).unwrap_or("Answering…");
                lines.push(Line::styled(status.to_owned(), Style::default().fg(Color::Cyan)));
            } else if turn.answer.is_none() && !turn.stopped {
                lines.push(Line::raw("No completed answer. Select this turn with [/] then a to ask again, or R to reply in the thread."));
            }
        }
    } else {
        lines.push(Line::raw(if state.read.active {
            "Reading conversation…"
        } else {
            "Conversation unavailable. Press r to retry; no text has been discarded."
        }));
    }
    lines
}

fn append(lines: &mut Vec<Line<'static>>, text: &str, style: Style) {
    lines.extend(text.lines().map(|line| Line::styled(line.to_owned(), style)));
}
