//! Copy/export for an Inbox thread. Server access is GET-only; local file writes happen
//! only after the person chooses an artifact, version, and destination.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use serde::Deserialize;
use serde_json::Value;
use time::{Month, OffsetDateTime, format_description::well_known::Rfc3339};
use url::Url;

use super::attachments::{Attachment, Version};
use super::client::{Author, Client, Project, path_id};
use crate::app::compose::{Compose, ComposeAction};
use crate::delivery::{Clipboard, Delivery as _};
use crate::theme::palette;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct Thread {
    #[serde(rename = "thread_id")]
    id: String,
    subject: Option<String>,
    #[serde(rename = "thread_name")]
    name: Option<String>,
    project: Project,
    messages: Vec<Message>,
}
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct Message {
    id: String,
    author: Author,
    subject: Option<String>,
    body: String,
    created_at: String,
    attachments: Vec<NamedAttachment>,
}
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct NamedAttachment {
    name: String,
}
#[derive(Debug, Deserialize)]
struct ThreadEnvelope {
    #[serde(rename = "serverSession")]
    server_session: String,
    thread: Thread,
}
#[derive(Debug, Deserialize)]
struct GuideResponse {
    message_id: String,
    snapshot: Value,
}
#[derive(Debug, Clone)]
struct GuideChoice {
    message_id: String,
    title: String,
}

/// Share/copy/export one thread. Called from the F2 feature menu, borrowing its terminal.
pub(crate) fn run(client: &Client, terminal: &mut DefaultTerminal, thread_id: Option<&str>) -> Result<()> {
    let Some(thread_id) = thread_id else {
        notice(
            terminal,
            "Share / export",
            "Select or open a thread first.\n\nCopying and exporting never changes the Inbox.",
        )?;
        return Ok(());
    };
    let thread_id = path_id(thread_id)?;
    let route = format!("/api/inbox/threads/{thread_id}");
    let read: ThreadEnvelope = client.get_json("GET", &route)?;
    ensure!(
        read.server_session == client.server_session(),
        "Inbox serverSession changed; reopen the Inbox before continuing"
    );
    let thread = read.thread;
    ensure!(thread.id == thread_id, "Inbox returned a different thread");
    let labels = [
        "Copy thread as Markdown",
        "Copy thread link",
        "Copy thread ID",
        "Copy one message as Markdown",
        "Copy link to one message",
        "Export attachment or image",
        "Export guided review snapshot",
        "Back",
    ]
    .map(str::to_owned);
    loop {
        let Some(index) = choose(
            terminal,
            "Share / export",
            &format!(
                "{}\n{} · {} messages\n\nCopies use terminal OSC 52. Exports are saved only to a path you choose. All Inbox access is GET-only.",
                thread.subject.as_deref().or(thread.name.as_deref()).unwrap_or("(no subject)"),
                thread.project.name,
                thread.messages.len()
            ),
            &labels,
        )?
        else {
            return Ok(());
        };
        match index {
            0 => copy_result(terminal, "Copy thread", &thread_markdown(&thread))?,
            1 => copy_result(terminal, "Copy thread link", &thread_link(client, thread_id, None)?)?,
            2 => copy_result(terminal, "Copy thread ID", thread_id)?,
            3 | 4 => {
                let choices: Vec<_> = thread
                    .messages
                    .iter()
                    .map(|message| {
                        format!(
                            "{} · {} · {}",
                            author(&message.author),
                            copy_time(&message.created_at),
                            one_line(message.subject.as_deref().unwrap_or_else(|| {
                                message.body.lines().next().unwrap_or("(empty message)")
                            }))
                        )
                    })
                    .collect();
                if let Some(selected) = choose(
                    terminal,
                    if index == 3 { "Copy message" } else { "Copy message link" },
                    "Select a message from the complete thread, including folded messages.",
                    &choices,
                )? && let Some(message) = thread.messages.get(selected)
                {
                    if index == 3 {
                        copy_result(terminal, "Copy message", &message_markdown(message))?;
                    } else {
                        copy_result(
                            terminal,
                            "Copy message link",
                            &thread_link(client, thread_id, Some(&message.id))?,
                        )?;
                    }
                }
            }
            5 => export_attachment(client, terminal, thread_id)?,
            6 => export_guide(client, terminal, &thread)?,
            _ => return Ok(()),
        }
    }
}

fn choose(
    terminal: &mut DefaultTerminal,
    title: &str,
    detail: &str,
    labels: &[String],
) -> Result<Option<usize>> {
    let mut selected = 0usize;
    let mut state = ListState::default().with_selected((!labels.is_empty()).then_some(0));
    loop {
        terminal.draw(|frame| {
            let area = frame.area();
            let height = u16::try_from(labels.len().saturating_add(2))
                .unwrap_or(u16::MAX)
                .min((area.height / 2).max(3));
            let [menu, info, help] =
                Layout::vertical([Constraint::Length(height), Constraint::Min(1), Constraint::Length(2)])
                    .areas(area);
            let rows: Vec<_> = labels.iter().map(|label| ListItem::new(label.as_str())).collect();
            frame.render_stateful_widget(
                List::new(rows)
                    .block(Block::default().borders(Borders::ALL).title(title))
                    .highlight_style(Style::default().fg(palette().accent).add_modifier(Modifier::BOLD))
                    .highlight_symbol("> "),
                menu,
                &mut state,
            );
            frame.render_widget(
                Paragraph::new(detail)
                    .wrap(Wrap { trim: false })
                    .block(Block::default().borders(Borders::ALL).title("Details")),
                info,
            );
            frame.render_widget(Paragraph::new("↑/↓ or j/k choose · Enter select · Esc back"), help);
        })?;
        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        let Event::Key(key) = event::read()? else { continue };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(None),
            KeyCode::Up | KeyCode::Char('k') => {
                selected = selected.saturating_sub(1);
                state.select((!labels.is_empty()).then_some(selected));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                selected = (selected + 1).min(labels.len().saturating_sub(1));
                state.select((!labels.is_empty()).then_some(selected));
            }
            KeyCode::Home => {
                selected = 0;
                state.select((!labels.is_empty()).then_some(selected));
            }
            KeyCode::End => {
                selected = labels.len().saturating_sub(1);
                state.select((!labels.is_empty()).then_some(selected));
            }
            KeyCode::Enter if key.kind == KeyEventKind::Press && !labels.is_empty() => {
                return Ok(Some(selected));
            }
            _ => {}
        }
    }
}

fn thread_link(client: &Client, thread_id: &str, message_id: Option<&str>) -> Result<String> {
    let mut url = Url::parse(client.url()).context("parsing Inbox URL for a share link")?;
    ensure!(
        matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
        "Inbox URL must be HTTP(S)"
    );
    ensure!(url.username().is_empty() && url.password().is_none(), "Inbox URL must not contain credentials");
    let mut fragment = format!("thread={thread_id}");
    if let Some(message_id) = message_id {
        fragment.push_str("&msg=");
        fragment.push_str(path_id(message_id)?);
    }
    url.set_fragment(Some(&fragment));
    Ok(url.into())
}

fn copy_result(terminal: &mut DefaultTerminal, title: &str, text: &str) -> Result<()> {
    let detail = match Clipboard.deliver(text) {
        Ok(()) => format!("Copied {} characters to the terminal clipboard.", text.chars().count()),
        Err(error) => format!("Clipboard copy failed: {error}"),
    };
    notice(terminal, title, &detail)
}

fn thread_markdown(thread: &Thread) -> String {
    let mut out = vec![
        format!(
            "# {}",
            one_line(thread.subject.as_deref().or(thread.name.as_deref()).unwrap_or("(no subject)"))
        ),
        String::new(),
        format!(
            "{} · {} message{}",
            one_line(&thread.project.name),
            thread.messages.len(),
            if thread.messages.len() == 1 { "" } else { "s" }
        ),
    ];
    for message in &thread.messages {
        out.extend([
            String::new(),
            "---".to_owned(),
            String::new(),
            format!("**{}** · {}", author(&message.author), copy_time(&message.created_at)),
        ]);
        let body = message_markdown(message);
        if !body.is_empty() {
            out.extend([String::new(), body]);
        }
    }
    out.join("\n")
}

fn message_markdown(message: &Message) -> String {
    // Preserve message Markdown byte-for-byte; do not reinterpret window-only directives.
    let names: Vec<_> = message.attachments.iter().map(|file| plain_markdown_text(&file.name)).collect();
    if names.is_empty() {
        message.body.clone()
    } else {
        format!("{}\n\nFiles: {}", message.body, names.join(", "))
    }
}
fn plain_markdown_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(ch, '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '|' | '~' | '&' | '!' | '#') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn author(author: &Author) -> String {
    if author.kind == "person" {
        "You".to_owned()
    } else {
        author
            .name
            .as_deref()
            .or(author.host.as_deref())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or("agent")
            .to_owned()
    }
}
fn copy_time(iso: &str) -> String {
    let Ok(at) = OffsetDateTime::parse(iso, &Rfc3339) else { return String::new() };
    let local = at.to_offset(time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC));
    let month = match local.month() {
        Month::January => "Jan",
        Month::February => "Feb",
        Month::March => "Mar",
        Month::April => "Apr",
        Month::May => "May",
        Month::June => "Jun",
        Month::July => "Jul",
        Month::August => "Aug",
        Month::September => "Sep",
        Month::October => "Oct",
        Month::November => "Nov",
        Month::December => "Dec",
    };
    let hour = (local.hour() % 12).max(1);
    format!(
        "{month} {}, {}, {}:{:02} {}",
        local.day(),
        local.year(),
        hour,
        local.minute(),
        if local.hour() < 12 { "AM" } else { "PM" }
    )
}

fn export_attachment(client: &Client, terminal: &mut DefaultTerminal, thread_id: &str) -> Result<()> {
    let model = client.attachments(thread_id)?;
    let files: Vec<Attachment> = model.files().cloned().collect();
    if files.is_empty() {
        notice(terminal, "Export artifact", "This thread has no attached files or captured images.")?;
        return Ok(());
    }
    let labels: Vec<_> = files
        .iter()
        .map(|file| {
            format!(
                "{} · {} bytes{}",
                file.name,
                file.size,
                if file.changed_since_sent { " · changed since sent" } else { "" }
            )
        })
        .collect();
    let Some(index) = choose(
        terminal,
        "Choose artifact to export",
        "Choose the current or immutable sent version, then a destination path.",
        &labels,
    )?
    else {
        return Ok(());
    };
    let Some(attachment) = files.get(index) else { return Ok(()) };
    let versions = [Version::Current, Version::Sent];
    let Some(version_index) = choose(
        terminal,
        "Choose artifact version",
        &attachment.name,
        &["Current file".to_owned(), "Sent copy".to_owned()],
    )?
    else {
        return Ok(());
    };
    let Some(version) = versions.get(version_index).copied() else { return Ok(()) };
    let Some(path) = input_path(terminal, "Export artifact", "Destination path.", &attachment.name)? else {
        return Ok(());
    };
    let route = format!(
        "/api/inbox/attachments/{}/download{}",
        super::attachments::id(&attachment.id)?,
        if version == Version::Sent { "?version=sent" } else { "" }
    );
    let bytes = client
        .get_bytes("GET", &route)
        .with_context(|| format!("reading {} from the Inbox", attachment.name))?;
    write_export(terminal, &path, &bytes)
}

fn export_guide(client: &Client, terminal: &mut DefaultTerminal, thread: &Thread) -> Result<()> {
    let raw: Value = client.get_json("GET", &format!("/api/inbox/threads/{}", path_id(&thread.id)?))?;
    ensure!(
        raw.get("serverSession").and_then(Value::as_str) == Some(client.server_session()),
        "Inbox serverSession changed; reopen the Inbox before continuing"
    );
    let guides: Vec<_> = raw
        .get("thread")
        .and_then(|value| value.get("messages"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|message| {
            let reference = message.get("guide")?;
            if reference.is_null() {
                return None;
            }
            Some(GuideChoice {
                message_id: message.get("id")?.as_str()?.to_owned(),
                title: reference.get("title").and_then(Value::as_str).unwrap_or("guided-review").to_owned(),
            })
        })
        .collect();
    if guides.is_empty() {
        notice(terminal, "Export guided review", "This thread has no guided review snapshot to export.")?;
        return Ok(());
    }
    let labels: Vec<_> =
        guides.iter().map(|guide| format!("{} · {}", guide.title, guide.message_id)).collect();
    let Some(index) = choose(
        terminal,
        "Choose guided review",
        "Exports the snapshot attached to the selected message.",
        &labels,
    )?
    else {
        return Ok(());
    };
    let Some(guide) = guides.get(index) else { return Ok(()) };
    let response: GuideResponse =
        client.get_json("GET", &format!("/api/inbox/messages/{}/guide", path_id(&guide.message_id)?))?;
    ensure!(
        response.message_id == guide.message_id,
        "Inbox returned a guided review for a different message"
    );
    let bytes = serde_json::to_vec_pretty(&response.snapshot).context("formatting guided review snapshot")?;
    let filename = format!("{}.json", safe_filename(&guide.title));
    let Some(path) =
        input_path(terminal, "Export guided review", "Destination path for the JSON snapshot.", &filename)?
    else {
        return Ok(());
    };
    write_export(terminal, &path, &bytes)
}

fn input_path(
    terminal: &mut DefaultTerminal,
    title: &str,
    detail: &str,
    initial: &str,
) -> Result<Option<PathBuf>> {
    let mut compose = Compose::with_text(initial);
    loop {
        terminal.draw(|frame| {
            let [info, field, help] =
                Layout::vertical([Constraint::Length(3), Constraint::Min(3), Constraint::Length(2)])
                    .areas(frame.area());
            frame.render_widget(Paragraph::new(detail).wrap(Wrap { trim: false }), info);
            let block = Block::default().borders(Borders::ALL).title(title);
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
            frame.render_widget(Paragraph::new("Enter save · Esc cancel · paste a path"), help);
        })?;
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match compose.handle_key(key) {
                ComposeAction::Save => {
                    let value = compose.value();
                    if value.trim().is_empty() {
                        notice(terminal, title, "Enter a destination path.")?;
                    } else {
                        return Ok(Some(PathBuf::from(value.trim())));
                    }
                }
                ComposeAction::Cancel => return Ok(None),
                ComposeAction::Edited => {}
            },
            Event::Paste(value) => compose.insert_text(&value),
            _ => {}
        }
    }
}

fn write_export(terminal: &mut DefaultTerminal, path: &PathBuf, bytes: &[u8]) -> Result<()> {
    let result = (|| -> Result<()> {
        if path.exists() {
            let metadata = fs::symlink_metadata(path)
                .with_context(|| format!("reading export destination {}", path.display()))?;
            ensure!(!metadata.file_type().is_symlink(), "refusing to overwrite symlink {}", path.display());
            ensure!(metadata.is_file(), "export destination is not a file: {}", path.display());
            let labels = vec!["Cancel".to_owned(), "Replace file".to_owned()];
            ensure!(
                choose(
                    terminal,
                    "Replace existing export?",
                    &format!("{} exists.", path.display()),
                    &labels
                )? == Some(1),
                "export cancelled"
            );
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .with_context(|| format!("opening export destination {}", path.display()))?;
        file.write_all(bytes).with_context(|| format!("writing export destination {}", path.display()))?;
        file.flush().with_context(|| format!("flushing export destination {}", path.display()))?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            notice(terminal, "Export complete", &format!("Wrote {} bytes to {}", bytes.len(), path.display()))
        }
        Err(error) if error.to_string() == "export cancelled" => Ok(()),
        Err(error) => notice(terminal, "Export failed", &format!("{error:#}")),
    }
}
fn safe_filename(text: &str) -> String {
    let name: String = text
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') { ch } else { '-' })
        .collect();
    let name = name.trim_matches(['-', '.']);
    if name.is_empty() { "guided-review".to_owned() } else { name.chars().take(100).collect() }
}
fn notice(terminal: &mut DefaultTerminal, title: &str, detail: &str) -> Result<()> {
    loop {
        terminal.draw(|frame| {
            let [body, help] =
                Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).areas(frame.area());
            frame.render_widget(
                Paragraph::new(detail)
                    .wrap(Wrap { trim: false })
                    .block(Block::default().borders(Borders::ALL).title(title)),
                body,
            );
            frame.render_widget(Paragraph::new("Enter / Esc back"), help);
        })?;
        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        if let Event::Key(key) = event::read()?
            && key.kind != KeyEventKind::Release
            && matches!(key.code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q'))
        {
            return Ok(());
        }
    }
}
