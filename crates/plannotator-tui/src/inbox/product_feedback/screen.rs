use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

use crate::app::compose::{Compose, ComposeAction};
use crate::theme::palette;

use super::Pane;
use super::model::Draft;

pub(super) enum Selection {
    Item(usize),
    Back,
}

pub(super) fn choose(
    terminal: &mut DefaultTerminal,
    title: &str,
    detail: &str,
    labels: &[String],
) -> Result<Selection> {
    let mut list = ListState::default().with_selected(Some(0));
    let mut scroll = 0u16;
    let mut armed = Instant::now();
    loop {
        terminal.draw(|frame| {
            let [choices, body, footer] = Layout::vertical([
                Constraint::Length(
                    u16::try_from(labels.len() + 2).unwrap_or(u16::MAX).min((frame.area().height / 2).max(3)),
                ),
                Constraint::Min(1),
                Constraint::Length(2),
            ])
            .areas(frame.area());
            frame.render_stateful_widget(
                List::new(labels.iter().map(|label| ListItem::new(label.as_str())))
                    .block(Block::default().borders(Borders::ALL).title(title))
                    .highlight_style(palette().selection)
                    .highlight_symbol("> "),
                choices,
                &mut list,
            );
            frame.render_widget(
                Paragraph::new(detail)
                    .wrap(Wrap { trim: false })
                    .scroll((scroll, 0))
                    .block(Block::default().borders(Borders::ALL).title("Details — PgUp/PgDn")),
                body,
            );
            frame.render_widget(
                Paragraph::new(
                    "↑/↓ or j/k choose · Enter select · Esc back\nPgUp/PgDn scroll details · Home top",
                ),
                footer,
            );
        })?;
        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Release {
                continue;
            }
            match key.code {
                KeyCode::Esc => return Ok(Selection::Back),
                KeyCode::Up | KeyCode::Char('k') => {
                    list.select(Some(list.selected().unwrap_or(0).saturating_sub(1)));
                    armed = Instant::now();
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    list.select(Some((list.selected().unwrap_or(0) + 1).min(labels.len().saturating_sub(1))));
                    armed = Instant::now();
                }
                KeyCode::PageUp => scroll = scroll.saturating_sub(10),
                KeyCode::PageDown => scroll = scroll.saturating_add(10),
                KeyCode::Home => scroll = 0,
                KeyCode::Enter
                    if key.kind == KeyEventKind::Press
                        && armed.elapsed() >= Duration::from_millis(600)
                        && !labels.is_empty() =>
                {
                    return Ok(Selection::Item(list.selected().unwrap_or(0)));
                }
                _ => {}
            }
        }
    }
}

pub(super) fn notice(terminal: &mut DefaultTerminal, title: &str, detail: &str) -> Result<()> {
    choose(terminal, title, detail, &["Back".into()])?;
    Ok(())
}

pub(super) fn edit(terminal: &mut DefaultTerminal, initial: &str, contact: bool) -> Result<String> {
    let mut compose = Compose::with_text(initial);
    loop {
        terminal.draw(|frame| {
            let [field, footer] =
                Layout::vertical([Constraint::Min(1), Constraint::Length(3)]).areas(frame.area());
            let count = compose.value().trim().chars().count();
            let max = if contact { super::model::CONTACT_MAX } else { super::model::MESSAGE_MAX };
            let name = if contact { "Contact — optional email or handle" } else { "Your feedback" };
            let block = Block::default().borders(Borders::ALL).title(format!("{name} · {count}/{max}"));
            let inner = block.inner(field);
            let (lines, row, col) = compose.wrapped(usize::from(inner.width));
            let top = row.saturating_sub(usize::from(inner.height).saturating_sub(1));
            frame.render_widget(
                Paragraph::new(lines.into_iter().skip(top).map(Line::from).collect::<Vec<_>>()).block(block),
                field,
            );
            if inner.width > 0 && inner.height > 0 {
                frame.set_cursor_position((
                    inner.x + u16::try_from(col).unwrap_or(0).min(inner.width - 1),
                    inner.y + u16::try_from(row.saturating_sub(top)).unwrap_or(0).min(inner.height - 1),
                ));
            }
            let hint = if contact {
                "Enter/Esc finish editing · Ctrl+S finish · never submits"
            } else {
                "Enter newline · Esc or Ctrl+S finish editing · never submits"
            };
            frame.render_widget(
                Paragraph::new(format!(
                    "{hint}\nYour words stay in the draft until you explicitly review and send.\n{}",
                    if count > max { "Over the limit; shorten before sending." } else { "" }
                ))
                .wrap(Wrap { trim: false }),
                footer,
            );
        })?;
        match event::read()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if key.code == KeyCode::Esc
                    || (key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL))
                {
                    return Ok(compose.value());
                }
                if key.code == KeyCode::Enter {
                    if contact {
                        return Ok(compose.value());
                    }
                    compose.insert_text("\n");
                } else if !(contact
                    && matches!(key.code, KeyCode::Char('j' | 'J'))
                    && key.modifiers.contains(KeyModifiers::CONTROL))
                    && compose.handle_key(key) == ComposeAction::Cancel
                {
                    return Ok(compose.value());
                }
            }
            Event::Paste(text) => {
                if contact {
                    compose.insert_text(&text.replace(['\n', '\r'], " "));
                } else {
                    compose.insert_text(&text);
                }
            }
            _ => {}
        }
    }
}

pub(super) fn draft_detail(draft: &Draft, disclosure: &str) -> String {
    format!(
        "{disclosure}\n\nCategory: {}\nContact: {}\n\n{}",
        draft.category.label(),
        if draft.contact.trim().is_empty() { "Not provided" } else { draft.contact.trim() },
        draft.message.trim()
    )
}

pub(super) fn draw(frame: &mut Frame, pane: &mut Pane) {
    let [header, menu, body, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(8),
        Constraint::Min(1),
        Constraint::Length(3),
    ])
    .areas(frame.area());
    frame.render_widget(
        Paragraph::new("Product feedback · Message / Common questions")
            .style(Style::new().fg(palette().accent)),
        header,
    );
    let labels = [
        format!("Edit message · {} / 4,000 characters", pane.draft.message.trim().chars().count()),
        format!("Category · {}", pane.draft.category.label()),
        format!(
            "Contact · {}",
            if pane.draft.contact.is_empty() { "None (optional)" } else { &pane.draft.contact }
        ),
        format!("These might help · {} local matches", pane.related.len()),
        "Common questions".to_owned(),
        "Copy agent prompt (does not send)".to_owned(),
        "Review and send feedback".to_owned(),
        "Close — keep draft in this process".to_owned(),
    ];
    frame.render_stateful_widget(
        List::new(labels.into_iter().map(ListItem::new))
            .highlight_style(palette().selection)
            .highlight_symbol("> "),
        menu,
        &mut pane.list,
    );
    let mut detail = draft_detail(&pane.draft, &pane.disclosure);
    if !pane.related.is_empty() {
        detail.push_str("\n\nThese might help (local; no draft text is sent):\n");
        for hit in &pane.related {
            detail.push_str("• ");
            detail.push_str(&pane.help.label(*hit));
            detail.push('\n');
        }
    }
    detail.push_str("\nHelp contains bundled upstream common answers and curated features. Full upstream issue/PR/release-title search is not included. Browser-only preferences are identified, not changed here.");
    frame.render_widget(
        Paragraph::new(detail)
            .wrap(Wrap { trim: false })
            .scroll((pane.scroll, 0))
            .block(Block::default().borders(Borders::ALL).title("Draft and privacy — PgUp/PgDn")),
        body,
    );
    frame.render_widget(Paragraph::new(format!("↑/↓ j/k select · Enter open · e message · c category · @ contact · ? questions\nh related · p copy prompt · s review/send · Esc close · PgUp/PgDn preview\n{}", pane.status)).wrap(Wrap { trim: false }), footer);
}
