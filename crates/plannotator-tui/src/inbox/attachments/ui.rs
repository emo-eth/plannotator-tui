use anyhow::Result;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use super::{Attachment, AttachmentKind, AttachmentView, AttachmentsModel, Version, open_browser};
use crate::config::Config;
use crate::gate::Decision;
use crate::inbox::client::Client;
use crate::theme::palette;

pub(crate) fn run(client: &Client, terminal: &mut DefaultTerminal, thread_id: &str) -> Result<()> {
    let mut model = client.attachments(thread_id)?;
    let mut selected = 0;
    let mut status = String::new();
    loop {
        let count = model.files().count();
        selected = selected.min(count.saturating_sub(1));
        terminal.draw(|frame| draw_list(frame, &model, selected, &status))?;
        let Event::Key(key) = event::read()? else { continue };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Ok(());
        }
        let result = match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(()),
            KeyCode::Down | KeyCode::Char('j') => {
                selected = (selected + 1).min(count.saturating_sub(1));
                continue;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                selected = selected.saturating_sub(1);
                continue;
            }
            KeyCode::Char('r') => client.attachments(thread_id).map(|next| model = next),
            KeyCode::Char('n') => pending_notes(client, terminal, &mut model, thread_id),
            KeyCode::Char('p') => {
                let text = serde_json::to_string_pretty(&model.earlier_annotations)?;
                read_text(terminal, "Earlier annotations · already sent · read only", &text)
            }
            KeyCode::Enter | KeyCode::Char('c' | 's' | 'o' | 'O') => {
                let Some(attachment) = model.files().nth(selected).cloned() else { continue };
                let version = match key.code {
                    KeyCode::Char('s' | 'O') => Version::Sent,
                    KeyCode::Char('c') => Version::Current,
                    _ => attachment.readable_version(),
                };
                let opened = if matches!(key.code, KeyCode::Char('o' | 'O')) {
                    client.attachment_url(&attachment, version).and_then(|url| open_browser(url.as_str()))
                } else {
                    open_attachment(client, terminal, &attachment, version)
                };
                if opened.is_ok() { client.attachments(thread_id).map(|next| model = next) } else { opened }
            }
            _ => continue,
        };
        status = result.err().map_or_else(String::new, |error| format!("{error:#}"));
    }
}

fn draw_list(frame: &mut ratatui::Frame<'_>, model: &AttachmentsModel, selected: usize, status: &str) {
    let [header, body, footer] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(1), Constraint::Length(3)])
            .areas(frame.area());
    frame.render_widget(
        Paragraph::new(format!(
            "Attachments · {} files · {} pending notes · {} earlier notes",
            model.files().count(),
            model.annotations.len(),
            model.earlier_annotations.len()
        )),
        header,
    );
    let items: Vec<_> = model
        .files()
        .map(|file| {
            let state = if let Some(unavailable) = &file.unavailable {
                format!("current unavailable: {} · Enter opens sent copy", unavailable.message)
            } else if file.changed_since_sent {
                "changed since sent".to_owned()
            } else {
                "unchanged".to_owned()
            };
            let inline = if file.image_ref.is_some() { " · message image" } else { "" };
            ListItem::new(format!("{} · {:?}{inline} · {} bytes\n  {state}", file.name, file.kind, file.size))
        })
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL))
        .highlight_symbol("› ")
        .highlight_style(Style::default().fg(palette().accent).add_modifier(Modifier::BOLD));
    let mut state = ListState::default().with_selected((model.files().count() > 0).then_some(selected));
    frame.render_stateful_widget(list, body, &mut state);
    let help = if status.is_empty() {
        "j/k choose · Enter read · c current · s sent · o/O external readable/sent · n pending · p earlier · r refresh · q back"
    } else {
        status
    };
    frame.render_widget(Paragraph::new(help).wrap(Wrap { trim: false }), footer);
}

pub(crate) fn open_attachment(
    client: &Client,
    terminal: &mut DefaultTerminal,
    attachment: &Attachment,
    version: Version,
) -> Result<()> {
    match attachment.kind {
        AttachmentKind::Markdown
        | AttachmentKind::Text
        | AttachmentKind::Code
        | AttachmentKind::Mermaid
        | AttachmentKind::Graphviz => {
            let mut view = client.attachment_view(&attachment.id, version)?;
            view.attachment.thread_id.clone_from(&attachment.thread_id);
            annotate(client, terminal, &view)
        }
        _ => {
            let url = client.attachment_url(attachment, version)?;
            open_browser(url.as_str())
        }
    }
}

fn annotate(client: &Client, terminal: &mut DefaultTerminal, view: &AttachmentView) -> Result<()> {
    let width = usize::from(terminal.size()?.width.saturating_sub(36).max(20));
    let mut app = view.open_app(width)?;
    let pending = if view.attachment.thread_id.is_empty() {
        Vec::new()
    } else {
        client
            .attachments(&view.attachment.thread_id)?
            .annotations
            .into_iter()
            .filter(|record| {
                record.is_file()
                    && record.pending()
                    && record.path == view.attachment.path
                    && record.version == view.version
            })
            .collect::<Vec<_>>()
    };
    app.replace_annotations(pending.iter().map(super::annotation::import).collect::<Result<_>>()?)?;
    let Some(notes) = edit_app(terminal, app)? else { return Ok(()) };
    save_with_retry(terminal, || {
        view.save_annotations(client, &notes)?;
        for record in &pending {
            if !notes.iter().any(|note| note.id == record.id) {
                client.remove_attachment_annotation(&record.id)?;
            }
        }
        Ok(())
    })
}

pub(super) fn edit_app(
    terminal: &mut DefaultTerminal,
    mut app: crate::app::App,
) -> Result<Option<Vec<plannotator_tui_schema::Annotation>>> {
    let config = Config::load().unwrap_or_default();
    app.keys = config.keys;
    app.ui = config.ui;
    app.clipboard = true;
    while !app.quit {
        terminal.draw(|frame| app.draw(frame))?;
        app.handle_event(&event::read()?)?;
        if app.take_editor_request().is_some() {
            app.set_status("This is the server's copy, not a local editable file.".to_owned());
        }
    }
    Ok(matches!(app.take_decision(), Some(Decision::Annotated { .. })).then(|| app.annotations()))
}

pub(super) fn save_with_retry(
    terminal: &mut DefaultTerminal,
    mut save: impl FnMut() -> Result<()>,
) -> Result<()> {
    loop {
        match save() {
            Ok(()) => return Ok(()),
            Err(error) => {
                let message = format!(
                    "Could not save all notes: {error:#}\n\nConfirmed saves remain pending in the Inbox.\nPress r to retry the same annotation IDs, or q to discard the unsaved notes and return."
                );
                loop {
                    terminal.draw(|frame| {
                        frame.render_widget(
                            Paragraph::new(message.as_str()).wrap(Wrap { trim: false }),
                            frame.area(),
                        );
                    })?;
                    let Event::Key(key) = event::read()? else { continue };
                    if key.kind == KeyEventKind::Release {
                        continue;
                    }
                    match key.code {
                        KeyCode::Char('r') => break,
                        KeyCode::Char('q') => return Err(error),
                        _ => {}
                    }
                }
            }
        }
    }
}

fn read_text(terminal: &mut DefaultTerminal, title: &str, text: &str) -> Result<()> {
    let mut scroll = 0_u16;
    loop {
        terminal.draw(|frame| {
            let [header, body, footer] =
                Layout::vertical([Constraint::Length(1), Constraint::Min(1), Constraint::Length(1)])
                    .areas(frame.area());
            frame.render_widget(Paragraph::new(title), header);
            frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }).scroll((scroll, 0)), body);
            frame.render_widget(Paragraph::new("j/k scroll · PgUp/PgDn page · Home top · q back"), footer);
        })?;
        let Event::Key(key) = event::read()? else { continue };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(()),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
            KeyCode::Down | KeyCode::Char('j') => scroll = scroll.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => scroll = scroll.saturating_sub(1),
            KeyCode::PageDown => scroll = scroll.saturating_add(terminal.size()?.height.saturating_sub(3)),
            KeyCode::PageUp => scroll = scroll.saturating_sub(terminal.size()?.height.saturating_sub(3)),
            KeyCode::Home => scroll = 0,
            _ => {}
        }
    }
}

fn pending_notes(
    client: &Client,
    terminal: &mut DefaultTerminal,
    model: &mut AttachmentsModel,
    thread_id: &str,
) -> Result<()> {
    let mut selected = 0_usize;
    loop {
        let records: Vec<_> = model.annotations.iter().filter(|record| record.is_file()).collect();
        selected = selected.min(records.len().saturating_sub(1));
        terminal.draw(|frame| {
            let [body, footer] = Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).areas(frame.area());
            let items: Vec<_> = records.iter().map(|record| {
                let text = record.annotation.get("text").and_then(serde_json::Value::as_str).unwrap_or("");
                let quote = record.annotation.get("originalText").and_then(serde_json::Value::as_str).unwrap_or("");
                let version = if record.version == "current" { "current" } else { "sent copy" };
                ListItem::new(format!("{} · {version}\n  {quote}\n  {text}", record.path))
            }).collect();
            let mut state = ListState::default().with_selected((!records.is_empty()).then_some(selected));
            frame.render_stateful_widget(List::new(items).highlight_symbol("› ").highlight_style(Style::default().fg(palette().accent)), body, &mut state);
            frame.render_widget(Paragraph::new("Pending notes · j/k choose · Enter read · e edit · d then y remove · q back\nSend the thread reply to deliver these notes."), footer);
        })?;
        let Event::Key(key) = event::read()? else { continue };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(()),
            KeyCode::Down | KeyCode::Char('j') => {
                selected = (selected + 1).min(records.len().saturating_sub(1));
            }
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Enter => {
                if let Some(record) = records.get(selected) {
                    let text = serde_json::to_string_pretty(record)?;
                    read_text(terminal, "Stored annotation · read only", &text)?;
                }
            }
            KeyCode::Char('e') => {
                let Some(record) = records.get(selected) else { continue };
                let body = record.annotation.get("text").and_then(serde_json::Value::as_str).unwrap_or("");
                if let Some(text) = edit_text(terminal, body)? {
                    client.edit_annotation_text(record, &text)?;
                    *model = client.attachments(thread_id)?;
                }
            }
            KeyCode::Char('d') => {
                let Some(record) = records.get(selected) else { continue };
                let record_id = record.id.clone();
                terminal.draw(|frame| {
                    frame.render_widget(
                        Paragraph::new("Remove this pending note? y remove · any other key cancel"),
                        frame.area(),
                    );
                })?;
                if let Event::Key(confirm) = event::read()?
                    && confirm.code == KeyCode::Char('y')
                    && confirm.kind != KeyEventKind::Release
                {
                    client.remove_attachment_annotation(&record_id)?;
                    *model = client.attachments(thread_id)?;
                }
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
            _ => {}
        }
    }
}

fn edit_text(terminal: &mut DefaultTerminal, text: &str) -> Result<Option<String>> {
    use crate::app::compose::{Compose, ComposeAction};
    let mut compose = Compose::with_text(text);
    loop {
        terminal.draw(|frame| {
            let [body, footer] =
                Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());
            let (lines, row, col) = compose.wrapped(usize::from(body.width.max(1)));
            frame.render_widget(Paragraph::new(lines.join("\n")), body);
            frame.set_cursor_position((
                body.x + u16::try_from(col).unwrap_or(u16::MAX).min(body.width.saturating_sub(1)),
                body.y + u16::try_from(row).unwrap_or(u16::MAX).min(body.height.saturating_sub(1)),
            ));
            frame.render_widget(Paragraph::new("Enter saves · Alt+Enter newline · Esc cancels"), footer);
        })?;
        match event::read()? {
            Event::Paste(text) => compose.insert_text(&text),
            Event::Key(key) if key.kind != KeyEventKind::Release => match compose.handle_key(key) {
                ComposeAction::Save => return Ok(Some(compose.value())),
                ComposeAction::Cancel => return Ok(None),
                ComposeAction::Edited => {}
            },
            _ => {}
        }
    }
}
