//! Small modal screens inside the Inbox's existing terminal. Nothing here sends HTTP.

use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use crate::app::compose::{Compose, ComposeAction};

pub(super) enum Choice {
    Selected(usize),
    Refresh,
    Cancel,
}

/// Menus arm after focus changes; key-repeat can navigate but never activate an action.
/// PageUp/PageDown scroll the complete detail, including permission commands and diffs.
pub(super) fn choose(
    terminal: &mut DefaultTerminal,
    title: &str,
    detail: &str,
    labels: &[String],
    refresh: bool,
) -> Result<Choice> {
    let mut selected = 0usize;
    let mut state = ListState::default().with_selected(Some(0));
    let mut scroll = 0u16;
    let mut armed = Instant::now();
    let opened = Instant::now();
    loop {
        terminal.draw(|frame| {
            let area = frame.area();
            let list_height = u16::try_from(labels.len().saturating_add(2))
                .unwrap_or(u16::MAX)
                .min((area.height / 2).max(3));
            let [list, details, help] = Layout::vertical([
                Constraint::Length(list_height),
                Constraint::Min(1),
                Constraint::Length(2),
            ])
            .areas(area);
            let rows: Vec<ListItem<'_>> = labels.iter().map(|s| ListItem::new(s.as_str())).collect();
            frame.render_stateful_widget(
                List::new(rows)
                    .block(Block::default().borders(Borders::ALL).title(title))
                    .highlight_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
                    .highlight_symbol("> "),
                list,
                &mut state,
            );
            frame.render_widget(
                Paragraph::new(detail)
                    .wrap(Wrap { trim: false })
                    .scroll((scroll, 0))
                    .block(Block::default().borders(Borders::ALL).title("Details — PgUp/PgDn scroll")),
                details,
            );
            frame.render_widget(
                Paragraph::new("↑/↓ or j/k choose · Enter select · Esc back\nPgUp/PgDn details · r refresh"),
                help,
            );
        })?;
        if !event::poll(Duration::from_millis(100))? {
            if refresh && opened.elapsed() >= Duration::from_secs(5) {
                return Ok(Choice::Refresh);
            }
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Esc => return Ok(Choice::Cancel),
                KeyCode::Char('r') if refresh => return Ok(Choice::Refresh),
                KeyCode::Up | KeyCode::Char('k') => {
                    selected = selected.saturating_sub(1);
                    state.select(Some(selected));
                    armed = Instant::now();
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    selected = selected.saturating_add(1).min(labels.len().saturating_sub(1));
                    state.select(Some(selected));
                    armed = Instant::now();
                }
                KeyCode::PageUp => scroll = scroll.saturating_sub(10),
                KeyCode::PageDown => scroll = scroll.saturating_add(10),
                KeyCode::Home => {
                    scroll = 0;
                }
                KeyCode::Enter
                    if key.kind == KeyEventKind::Press
                        && armed.elapsed() >= Duration::from_millis(600)
                        && !labels.is_empty() =>
                {
                    return Ok(Choice::Selected(selected));
                }
                _ => {}
            },
            _ => {}
        }
    }
}

pub(super) fn confirm(
    terminal: &mut DefaultTerminal,
    title: &str,
    detail: &str,
    action: &str,
) -> Result<bool> {
    Ok(matches!(
        choose(terminal, title, detail, &["Cancel".into(), action.into()], false)?,
        Choice::Selected(1)
    ))
}

pub(super) fn notice(terminal: &mut DefaultTerminal, title: &str, detail: &str) -> Result<()> {
    choose(terminal, title, detail, &["Back".into()], false)?;
    Ok(())
}

pub(super) fn text(
    terminal: &mut DefaultTerminal,
    title: &str,
    detail: &str,
    initial: &str,
) -> Result<Option<String>> {
    let mut compose = Compose::with_text(initial);
    loop {
        terminal.draw(|frame| {
            let [info, field, help] = Layout::vertical([
                Constraint::Length(4), Constraint::Min(3), Constraint::Length(2),
            ]).areas(frame.area());
            frame.render_widget(Paragraph::new(detail).wrap(Wrap { trim: false }), info);
            let block = Block::default().borders(Borders::ALL).title(title);
            let inner = block.inner(field);
            let (lines, row, col) = compose.wrapped(usize::from(inner.width));
            let top = row.saturating_sub(usize::from(inner.height).saturating_sub(1));
            let text: Vec<Line<'_>> = lines.into_iter().skip(top).map(Line::from).collect();
            frame.render_widget(Paragraph::new(text).block(block), field);
            if inner.width > 0 && inner.height > 0 {
                frame.set_cursor_position((inner.x + u16::try_from(col).unwrap_or(0).min(inner.width - 1),
                    inner.y + u16::try_from(row.saturating_sub(top)).unwrap_or(0).min(inner.height - 1)));
            }
            frame.render_widget(Paragraph::new("Enter continue · Alt+Enter/Ctrl+J newline · Esc cancel\nPasted text is kept verbatim; nothing is sent yet."), help);
        })?;
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match compose.handle_key(key) {
                ComposeAction::Save => return Ok(Some(compose.value())),
                ComposeAction::Cancel => return Ok(None),
                ComposeAction::Edited => {}
            },
            Event::Paste(value) => compose.insert_text(&value),
            _ => {}
        }
    }
}
