//! User-facing Inbox media: the Inbox's image/audio/video routes in the native browser, plus
//! an explicit, in-memory microphone recording sent to this Inbox's voice transcription route.

mod voice;

use std::fmt::Write as _;
use std::time::Duration;

use anyhow::Result;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use super::attachments::{Attachment, AttachmentKind, AttachmentsModel, Version, open_browser};
use super::client::Client;
use crate::theme::palette;

const MEDIA_KINDS: [AttachmentKind; 3] =
    [AttachmentKind::Image, AttachmentKind::Video, AttachmentKind::Audio];

/// Open the Inbox media surface in the Inbox's already-owned terminal. A transcript, when
/// requested explicitly, is returned to the caller for insertion into the active compose box.
pub(crate) fn run(
    client: &Client,
    terminal: &mut DefaultTerminal,
    thread_id: Option<&str>,
) -> Result<Option<String>> {
    let mut model = thread_id.map(|id| client.attachments(id)).transpose()?;
    let mut selected = 0;
    let mut status = String::new();
    loop {
        let files: Vec<&Attachment> = model
            .as_ref()
            .map_or_else(Vec::new, |m| m.files().filter(|file| MEDIA_KINDS.contains(&file.kind)).collect());
        selected = selected.min(files.len().saturating_sub(1));
        terminal.draw(|frame| draw(frame, model.as_ref(), &files, selected, &status, thread_id))?;
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let Event::Key(key) = event::read()? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(None),
            KeyCode::Up | KeyCode::Char('k') => {
                selected = selected.saturating_sub(1);
                status.clear();
            }
            KeyCode::Down | KeyCode::Char('j') => {
                selected = selected.saturating_add(1).min(files.len().saturating_sub(1));
                status.clear();
            }
            KeyCode::Char('r') => {
                if let Some(id) = thread_id {
                    match client.attachments(id) {
                        Ok(next) => {
                            model = Some(next);
                            "media refreshed".clone_into(&mut status);
                        }
                        Err(error) => status = format!("refresh failed: {error:#}"),
                    }
                } else {
                    "open a thread to refresh its media".clone_into(&mut status);
                }
            }
            KeyCode::Char('v') => match voice::transcribe(client, terminal) {
                Ok(text) => return Ok(Some(text)),
                Err(error) => status = format!("voice transcription failed: {error:#}"),
            },
            KeyCode::Enter | KeyCode::Char('c' | 's') => {
                let Some(file) = files.get(selected).copied() else {
                    "no image, audio, or video attachments in this thread".clone_into(&mut status);
                    continue;
                };
                let version = match key.code {
                    KeyCode::Char('c') => {
                        if file.current.is_none() {
                            "current file is unavailable; open the sent copy instead".clone_into(&mut status);
                            continue;
                        }
                        Version::Current
                    }
                    KeyCode::Char('s') => Version::Sent,
                    _ => file.readable_version(),
                };
                match client.attachment_url(file, version).and_then(|url| open_browser(url.as_str())) {
                    Ok(()) => {
                        status = format!("opened {} ({}) in the system browser", file.name, version.label());
                    }
                    Err(error) => status = format!("could not open {}: {error:#}", file.name),
                }
            }
            _ => {}
        }
    }
}

fn draw(
    frame: &mut ratatui::Frame<'_>,
    model: Option<&AttachmentsModel>,
    files: &[&Attachment],
    selected: usize,
    status: &str,
    thread_id: Option<&str>,
) {
    let [header, body, footer] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(1), Constraint::Length(3)])
            .areas(frame.area());
    let count = model.map_or(0, |data| data.files().count());
    let title = thread_id.map_or_else(
        || "Media · no thread selected".to_owned(),
        |id| format!("Media · {id} · {} media items / {count} files", files.len()),
    );
    frame.render_widget(Paragraph::new(title), header);
    let items: Vec<ListItem<'_>> = files
        .iter()
        .map(|file| {
            let kind = match file.kind {
                AttachmentKind::Image => "image",
                AttachmentKind::Video => "video",
                AttachmentKind::Audio => "audio",
                _ => "media",
            };
            let mut detail = format!("{kind} · {} · {}", file.name, human_size(file.size));
            if let Some(media_type) = file.media_type.as_deref() {
                detail.push_str(" · ");
                detail.push_str(media_type);
            }
            if let (Some(width), Some(height)) = (file.width, file.height) {
                let _ = write!(detail, " · {width}×{height}");
            }
            if let Some(duration) = file.duration_ms {
                detail.push_str(" · ");
                detail.push_str(&duration_label(duration));
            }
            if file.current.is_none() {
                detail.push_str(" · current unavailable");
            } else if file.changed_since_sent {
                detail.push_str(" · changed since sent");
            }
            if let Some(unavailable) = &file.unavailable {
                detail.push_str(" · ");
                detail.push_str(&unavailable.message);
            }
            ListItem::new(Line::from(detail))
        })
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Images · audio · video"))
        .highlight_symbol("› ")
        .highlight_style(Style::new().fg(palette().accent).add_modifier(Modifier::BOLD));
    let mut state = ListState::default().with_selected((!files.is_empty()).then_some(selected));
    frame.render_stateful_widget(list, body, &mut state);
    let help = if status.is_empty() {
        "↑/↓ or j/k choose · Enter open readable version · c current · s sent · r refresh · v transcribe · q back"
    } else {
        status
    };
    frame.render_widget(
        Paragraph::new(help).wrap(Wrap { trim: false }).block(Block::default().borders(Borders::ALL)),
        footer,
    );
}

fn human_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn duration_label(milliseconds: f64) -> String {
    let seconds = (milliseconds.max(0.0) / 1000.0).round() as u64;
    if seconds >= 3600 {
        format!("{}:{:02}:{:02}", seconds / 3600, (seconds / 60) % 60, seconds % 60)
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, reason = "tests assert by panicking")]
mod tests {
    use super::{duration_label, human_size};

    #[test]
    fn displays_media_sizes_and_durations() {
        assert_eq!(human_size(24), "24 B");
        assert_eq!(human_size(1024), "1.0 KiB");
        assert_eq!(duration_label(12_500.0), "0:13");
        assert_eq!(duration_label(3_723_000.0), "1:02:03");
    }
}
