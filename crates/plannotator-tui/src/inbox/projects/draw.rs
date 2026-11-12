use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};

use super::{Modal, Projects};
use crate::app::compose::Compose;

impl Projects {
    pub(crate) fn draw(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let parts = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(7),
            Constraint::Length(5),
        ])
        .split(area);
        let Some((&header, rest)) = parts.split_first() else {
            return;
        };
        let [list, details, footer] = rest else {
            return;
        };
        let title = format!(
            " Projects — {}{} ",
            if self.show_archived { "including archived" } else { "open" },
            if self.query.is_empty() { String::new() } else { format!(" — find: {}", self.query) }
        );
        frame.render_widget(
            Paragraph::new(
                "Enter: filter Inbox  0: all projects  /: find  h: archived  F5: refresh  Esc: back",
            )
            .block(Block::default().borders(Borders::ALL).title(title)),
            header,
        );
        self.height = usize::from(list.height.saturating_sub(2)) / 2;
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        }
        if self.cursor >= self.scroll + self.height.max(1) {
            self.scroll = self.cursor.saturating_sub(self.height.max(1) - 1);
        }
        let items: Vec<_> = self
            .visible
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(self.height.max(1))
            .filter_map(|(position, index)| {
                let project = self.entries.get(*index)?;
                let marker = if self.filter.as_deref() == Some(&project.id) { "*" } else { " " };
                let listed = project.listed.map_or_else(|| "?".to_owned(), |count| count.to_string());
                let item = ListItem::new(vec![
                    Line::from(format!(
                        "{marker} {}  ·  {} unread  ·  {listed} listed  ·  {} archived  ·  {} total",
                        project.name, project.unread, project.archived, project.threads
                    )),
                    Line::styled(
                        format!("  {}  {}", project.id, project.root),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]);
                Some(if position == self.cursor {
                    item.style(Style::default().add_modifier(Modifier::REVERSED))
                } else {
                    item
                })
            })
            .collect();
        let list_block =
            Block::default().borders(Borders::ALL).title(format!(" {} projects ", self.visible.len()));
        if items.is_empty() {
            frame.render_widget(
                Paragraph::new(
                    "No matching open projects. h includes archived projects; / changes the search.",
                )
                .wrap(Wrap { trim: false })
                .block(list_block),
                *list,
            );
        } else {
            frame.render_widget(List::new(items).block(list_block), *list);
        }
        let detail = self.detail_lines();
        frame.render_widget(
            Paragraph::new(detail)
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).title(" Selected project ")),
            *details,
        );
        let pending = if self.pending.is_some() {
            "UNCONFIRMED: ! retry same request · x discard · F5 read current state"
        } else {
            "r rename · R reset name · c combine · U separate · m mark all read · A archive all · D delete · z Undo"
        };
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(pending),
                Line::styled(
                    self.status
                        .as_deref()
                        .unwrap_or("Opening and refreshing this pane only read the server."),
                    Style::default().fg(Color::Yellow),
                ),
            ])
            .wrap(Wrap { trim: false }),
            *footer,
        );
        if let Some(modal) = &self.modal {
            draw_modal(frame, Rect { height: area.height.saturating_sub(5), ..area }, modal);
        }
    }

    fn detail_lines(&self) -> Vec<Line<'static>> {
        let Some(project) = self.selected() else {
            return Vec::new();
        };
        let mut lines = vec![Line::from(format!("{} — {}", project.name, project.root))];
        let readable = self.eligibility.get(&project.id).map_or(0, |count| count.readable);
        lines.push(Line::from(if readable == 0 {
            "Mark all read unavailable: nothing waiting on you.".to_owned()
        } else {
            format!("Mark all read: {readable} threads waiting on you.")
        }));
        if project.listed == Some(0) && project.archived > 0 {
            lines.push(Line::from("Everything in this project is archived. Enter selects it; use Archived in the Inbox to read those threads."));
        }
        if let Some(name) = &project.display_name {
            lines.push(Line::from(format!("Custom name: {name} (R resets it)")));
        }
        if !project.combined.is_empty() {
            lines.push(Line::from(format!(
                "Combined folders: {} (U separates one)",
                project.combined.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
            )));
        }
        lines
    }
}

fn draw_modal(frame: &mut Frame<'_>, area: Rect, modal: &Modal) {
    let rect = Rect {
        x: area.x + area.width / 12,
        y: area.y + area.height / 10,
        width: area.width.saturating_sub(area.width / 6),
        height: area.height.saturating_sub(area.height / 5),
    };
    frame.render_widget(Clear, rect);
    let (title, description, input): (&str, String, Option<&Compose>) = match modal {
        Modal::Search(input) => (" Find projects ", "Names, paths and ids. Enter keeps the search; Esc clears it.".to_owned(), Some(input)),
        Modal::Rename { project, input } => (" Rename project ", format!("{}\n{}\nEnter saves; Esc cancels. A matching name offers Combine instead. At most 64 characters.", project.name, project.root), Some(input)),
        Modal::Confirm { mutation, warning, expected, input } => (" Confirm project action ", format!("{}\n\n{warning}\n\n{}", mutation.label,
            if expected.is_some() { "Type the project id, then Enter. Esc cancels." } else { "y confirms. n or Esc cancels. Enter does NOT confirm." }), expected.as_ref().map(|_| input)),
        Modal::Discard => (" Discard unconfirmed request? ", "The request may already have applied. Discarding loses its retained batch id and any not-yet-received Undo. It does not undo the server's changes.\n\ny discards locally; n or Esc keeps it.".to_owned(), None),
        Modal::Choose { project, choices, cursor, separate } => {
            let block = Block::default().borders(Borders::ALL).title(if *separate { " Separate a combined folder " } else { " Combine into project " });
            let inner = block.inner(rect);
            frame.render_widget(block, rect);
            let rows = Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).split(inner);
            let [hint, list] = rows.as_ref() else { return; };
            frame.render_widget(Paragraph::new(format!("{}\nUp/Down selects. Enter asks to confirm; Esc cancels.", project.name)).wrap(Wrap { trim: false }), *hint);
            let height = usize::from(list.height) / 2;
            let start = cursor.saturating_sub(height.saturating_sub(1));
            let items: Vec<_> = choices.iter().enumerate().skip(start).take(height.max(1)).map(|(index, choice)| {
                let item = ListItem::new(vec![Line::from(format!("{}  {}", choice.name, choice.id)), Line::from(choice.root.as_str())]);
                if index == *cursor { item.style(Style::default().add_modifier(Modifier::REVERSED)) } else { item }
            }).collect();
            frame.render_widget(List::new(items), *list);
            return;
        }
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    if let Some(input) = input {
        let rows = Layout::vertical([Constraint::Min(3), Constraint::Length(4)]).split(inner);
        let [description_area, input_area] = rows.as_ref() else {
            return;
        };
        frame.render_widget(Paragraph::new(description).wrap(Wrap { trim: false }), *description_area);
        let input_block = Block::default().borders(Borders::ALL).title(" Input ");
        let text_area = input_block.inner(*input_area);
        let (text, row, col) = input.wrapped(usize::from(text_area.width).max(1));
        let scroll = row.saturating_sub(usize::from(text_area.height).saturating_sub(1));
        frame.render_widget(
            Paragraph::new(text.into_iter().skip(scroll).map(Line::from).collect::<Vec<_>>())
                .block(input_block),
            *input_area,
        );
        if text_area.width > 0 && text_area.height > 0 {
            frame.set_cursor_position((
                text_area.x + u16::try_from(col).unwrap_or(0).min(text_area.width - 1),
                text_area.y + u16::try_from(row - scroll).unwrap_or(0).min(text_area.height - 1),
            ));
        }
    } else {
        frame.render_widget(
            Paragraph::new(vec![Line::from(Span::raw(description))]).wrap(Wrap { trim: false }),
            inner,
        );
    }
}
