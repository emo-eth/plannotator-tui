//! Server-backed search and agent navigation. These views never mark threads seen.

use anyhow::{Context as _, Result};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::widgets::{Block, Paragraph};

use super::client::Client;
use super::search::{
    SearchAnswer, SearchFrom, SearchGroup, SearchQuery, SearchRow, SearchSort, SearchStatus,
};
use super::ui::{Inbox, Request};
use crate::app::compose::{Compose, ComposeAction};
use crate::theme::palette;

const GROUPS: [SearchGroup; 4] =
    [SearchGroup::Threads, SearchGroup::Messages, SearchGroup::Files, SearchGroup::Projects];

pub(crate) fn search(client: &Client, terminal: &mut DefaultTerminal, inbox: &mut Inbox) -> Result<()> {
    let mut query = SearchQuery { project: inbox.project_filter.clone(), ..SearchQuery::default() };
    let mut answer = client.search(&query)?;
    let mut group = 0;
    let mut cursor: usize = 0;
    let mut edit: Option<Compose> = Some(Compose::default());
    let mut status = String::new();
    let projects = search_projects(inbox);
    loop {
        let selected_group = GROUPS.get(group).copied().context("search group is unavailable")?;
        let rows = search_rows(&answer, selected_group);
        cursor = cursor.min(rows.len().saturating_sub(1));
        terminal.draw(|frame| {
            let [header, body, footer] = Layout::vertical([
                Constraint::Length(3), Constraint::Min(1), Constraint::Length(3),
            ]).areas(frame.area());
            let words = edit.as_ref().map_or_else(|| query.q.clone(), Compose::value);
            frame.render_widget(Paragraph::new(format!(
                "Search: {words}\n{} · status {:?} · from {:?} · sort {:?} · project {}{}",
                selected_group.as_str(), query.status, query.from, query.sort,
                query.project.as_deref().unwrap_or("all"), if answer.indexing { " · index building; r refresh" } else { "" }
            )), header);
            let start = cursor.saturating_sub(usize::from(body.height).saturating_sub(1));
            for (index, row) in rows.iter().enumerate().skip(start).take(usize::from(body.height)) {
                let style = if index == cursor { palette().selection } else { Style::default() };
                frame.render_widget(Paragraph::new(search_label(row)).style(style),
                    Rect::new(body.x, body.y + (index - start) as u16, body.width, 1));
            }
            let help = if edit.is_some() { "Type query · Enter search · Esc browse · paste supported" }
                else { "/ edit · Tab group · ↑↓ move · Enter open · m more · s status · f author · o sort · p project\n0 clear filters · h recent search · C clear history · r refresh · Esc close" };
            frame.render_widget(Paragraph::new(format!("{status}\n{help}")), footer);
        })?;
        let input = event::read()?;
        if let Some(editor) = edit.as_mut() {
            match input {
                Event::Paste(text) => editor.insert_text(&text),
                Event::Key(key) if key.kind != KeyEventKind::Release => match editor.handle_key(key) {
                    ComposeAction::Save => {
                        query.q = editor.value();
                        edit = None;
                        refresh_search(client, &query, &mut answer, &mut cursor, &mut status);
                    }
                    ComposeAction::Cancel => edit = None,
                    ComposeAction::Edited => {}
                },
                _ => {}
            }
            continue;
        }
        let Event::Key(key) = input else { continue };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        let mut refresh = false;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => cursor = cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => cursor = (cursor + 1).min(rows.len().saturating_sub(1)),
            KeyCode::PageDown => cursor = (cursor + 10).min(rows.len().saturating_sub(1)),
            KeyCode::PageUp => cursor = cursor.saturating_sub(10),
            KeyCode::Tab => {
                group = (group + 1) % GROUPS.len();
                cursor = 0;
            }
            KeyCode::BackTab => {
                group = (group + GROUPS.len() - 1) % GROUPS.len();
                cursor = 0;
            }
            KeyCode::Char('/') => edit = Some(Compose::with_text(&query.q)),
            KeyCode::Char('r') => refresh = true,
            _ if change_search_filter(&mut query, &projects, key.code) => refresh = true,
            KeyCode::Char('h') => {
                if let Some(recent) = &answer.recent_searches {
                    let next = recent
                        .iter()
                        .position(|saved| saved == &query.q)
                        .map_or(0, |index| (index + 1) % recent.len().max(1));
                    if let Some(saved) = recent.get(next) {
                        edit = Some(Compose::with_text(saved));
                    }
                }
            }
            KeyCode::Char('C') => match client.clear_recent_searches() {
                Ok(saved) => {
                    answer.recent_searches = Some(saved);
                    "Search history cleared".clone_into(&mut status);
                }
                Err(error) => status = format!("{error:#}"),
            },
            KeyCode::Char('m') => {
                let group = selected_group;
                if answer.can_load_more(group) {
                    match client.search(&query.page(group, answer.next_offset(group))) {
                        Ok(page) => {
                            answer.append_page(group, page);
                        }
                        Err(error) => status = format!("{error:#}"),
                    }
                } else {
                    "No more results in this group".clone_into(&mut status);
                }
            }
            KeyCode::Enter => {
                if let Some(row) = rows.get(cursor) {
                    if !query.q.trim().is_empty()
                        && let Err(error) = client.remember_search(&query.q)
                    {
                        status = format!("search history not saved: {error:#}");
                        continue;
                    }
                    open_search_row(client, terminal, inbox, row)?;
                    return Ok(());
                }
            }
            _ => {}
        }
        if refresh {
            refresh_search(client, &query, &mut answer, &mut cursor, &mut status);
        }
    }
}

fn refresh_search(
    client: &Client,
    query: &SearchQuery,
    answer: &mut SearchAnswer,
    cursor: &mut usize,
    status: &mut String,
) {
    match client.search(query) {
        Ok(found) => {
            *answer = found;
            *cursor = 0;
            status.clear();
        }
        Err(error) => *status = format!("{error:#}"),
    }
}

fn change_search_filter(query: &mut SearchQuery, projects: &[(String, String)], code: KeyCode) -> bool {
    match code {
        KeyCode::Char('s') => {
            query.status = match query.status {
                None => Some(SearchStatus::Waiting),
                Some(SearchStatus::Waiting) => Some(SearchStatus::New),
                Some(SearchStatus::New) => Some(SearchStatus::Sent),
                Some(SearchStatus::Sent) => Some(SearchStatus::Quiet),
                Some(SearchStatus::Quiet) => Some(SearchStatus::Archived),
                Some(SearchStatus::Archived) => None,
            };
        }
        KeyCode::Char('f') => {
            query.from = match query.from {
                None => Some(SearchFrom::Agent),
                Some(SearchFrom::Agent) => Some(SearchFrom::Me),
                Some(SearchFrom::Me) => None,
            };
        }
        KeyCode::Char('o') => {
            query.sort = if query.sort == SearchSort::Best { SearchSort::Newest } else { SearchSort::Best };
        }
        KeyCode::Char('p') => {
            let next = query
                .project
                .as_ref()
                .and_then(|id| projects.iter().position(|(candidate, _)| candidate == id))
                .map_or(0, |index| index + 1);
            query.project = projects.get(next).map(|(id, _)| id.clone());
        }
        KeyCode::Char('0') => {
            query.status = None;
            query.from = None;
            query.project = None;
        }
        _ => return false,
    }
    true
}

fn search_projects(inbox: &Inbox) -> Vec<(String, String)> {
    inbox.list.iter().flat_map(|list| &list.sections).flat_map(|section| &section.threads).fold(
        Vec::new(),
        |mut projects, row| {
            if !projects.iter().any(|(id, _)| id == &row.project.id) {
                projects.push((row.project.id.clone(), row.project.name.clone()));
            }
            projects
        },
    )
}

fn open_search_row(
    client: &Client,
    terminal: &mut DefaultTerminal,
    inbox: &mut Inbox,
    row: &SearchRow,
) -> Result<()> {
    match row {
        SearchRow::Project(project) => inbox.set_project_filter(Some(project.project.id.clone())),
        SearchRow::Thread(thread) => super::perform(inbox, client, Request::Thread(thread.thread_id.clone())),
        SearchRow::Message(message) => {
            inbox.opened_thread(client.thread(&message.thread_id)?);
            if let Some(thread) = inbox.thread.as_mut() {
                thread.jump_message(&message.id);
            }
        }
        SearchRow::File(file) => {
            let model = client.attachments(&file.thread_id)?;
            let attachment = model
                .files()
                .find(|attachment| {
                    attachment.id == file.id
                        || (attachment.message_id == file.message_id && attachment.name == file.name)
                })
                .context("the indexed attachment is no longer available")?;
            super::attachments::open_attachment(
                client,
                terminal,
                attachment,
                super::attachments::Version::Current,
            )?;
        }
        SearchRow::Guide(file) => {
            super::reviews::run_guide(client, terminal, &file.thread_id, &file.message_id)?;
        }
    }
    Ok(())
}

fn search_rows(answer: &SearchAnswer, group: SearchGroup) -> Vec<SearchRow> {
    if group == SearchGroup::Threads && answer.groups.is_none() {
        answer.recent.iter().flatten().cloned().map(SearchRow::Thread).collect()
    } else {
        answer.rows(group).to_vec()
    }
}

fn search_label(row: &SearchRow) -> String {
    match row {
        SearchRow::Thread(row) => {
            format!("{} · {}{}", row.title, row.project.name, if row.archived { " · archived" } else { "" })
        }
        SearchRow::Message(row) => {
            format!("{} · {}: {}", row.title, row.author.name, row.snippet.text.replace('\n', " "))
        }
        SearchRow::File(row) | SearchRow::Guide(row) => {
            format!("{} · {} · {}", row.name, row.kind, row.project.name)
        }
        SearchRow::Project(row) => format!("{} · {}", row.name, row.root),
    }
}

pub(crate) fn agents(client: &Client, terminal: &mut DefaultTerminal, inbox: &mut Inbox) -> Result<()> {
    let mut list = client.list()?;
    let mut groups = super::agents::parse_agent_groups(&list.agents)?;
    let mut faces = client.agent_faces()?;
    let mut cursor: usize = 0;
    loop {
        let rows: Vec<_> = list.sections.iter().flat_map(|section| section.threads.iter().cloned()).collect();
        let entries: Vec<_> = super::agents::agent_groups(&groups, &rows, list.self_machine.as_deref())
            .into_iter()
            .flat_map(|group| group.agents)
            .collect();
        cursor = cursor.min(entries.len().saturating_sub(1));
        terminal.draw(|frame| {
            let [body, footer] = Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).areas(frame.area());
            let block = Block::bordered().title(" Agents · presence, faces and threads ");
            let inner = block.inner(body);
            frame.render_widget(block, body);
            let start = cursor.saturating_sub(usize::from(inner.height).saturating_sub(1));
            for (index, entry) in entries.iter().enumerate().skip(start).take(usize::from(inner.height)) {
                let face = faces.face_for_agent(&entry.agent).unwrap_or("");
                let text = format!("{} · {:?} · {} · {} · {}{}", entry.name, entry.agent.state,
                    entry.machine.as_deref().unwrap_or("this machine"),
                    super::agents::agent_host_name(entry.agent.host.as_deref()),
                    entry.thread.as_ref().map_or("no thread", |thread| thread.subject.as_str()),
                    if face.is_empty() { String::new() } else { format!(" · face {face}") });
                let style = if index == cursor { palette().selection } else { Style::default() };
                frame.render_widget(Paragraph::new(text).style(style),
                    Rect::new(inner.x, inner.y + (index - start) as u16, inner.width, 1));
            }
            frame.render_widget(Paragraph::new("↑↓ move · Enter thread · f filter this agent · 0 clear agent filter\nL session controls · i icon sets · r refresh · Esc close"), footer);
        })?;
        let Event::Key(key) = event::read()? else { continue };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => cursor = cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => cursor = (cursor + 1).min(entries.len().saturating_sub(1)),
            KeyCode::Char('0') => {
                inbox.set_agent_filter(None);
                return Ok(());
            }
            KeyCode::Char('f') => {
                if let Some(entry) = entries.get(cursor) {
                    inbox.set_agent_filter(Some(super::agents::agent_list_filter(
                        &entry.agent.id,
                        &groups,
                        &rows,
                        list.self_machine.as_deref(),
                    )));
                    return Ok(());
                }
            }
            KeyCode::Enter => {
                if let Some(thread) = entries.get(cursor).and_then(|entry| entry.thread.as_ref()) {
                    inbox.opened_thread(client.thread(&thread.id)?);
                    return Ok(());
                }
            }
            KeyCode::Char('L') => {
                let thread = entries
                    .get(cursor)
                    .and_then(|entry| entry.thread.as_ref())
                    .map(|thread| thread.id.as_str());
                super::launch::run(client, terminal, thread)?;
            }
            KeyCode::Char('i') => {
                super::main_delta::run(client, terminal)?;
                faces = client.agent_faces()?;
            }
            KeyCode::Char('r') => {
                list = client.list()?;
                groups = super::agents::parse_agent_groups(&list.agents)?;
                faces = client.agent_faces()?;
            }
            _ => {}
        }
    }
}

pub(crate) fn message_notes(client: &Client, terminal: &mut DefaultTerminal, thread_id: &str) -> Result<()> {
    let thread = client.thread(thread_id)?;
    let messages: Vec<_> = thread
        .thread
        .messages
        .iter()
        .filter(|message| message.author.kind == "agent" && !super::send::is_permission(message))
        .collect();
    let mut cursor = messages.len().saturating_sub(1);
    loop {
        terminal.draw(|frame| {
            let area = frame.area();
            let block = Block::bordered().title(" Annotate message · ↑↓ select · Enter annotate · Esc back ");
            let inner = block.inner(area);
            frame.render_widget(block, area);
            let start = cursor.saturating_sub(usize::from(inner.height).saturating_sub(1));
            for (index, message) in messages.iter().enumerate().skip(start).take(usize::from(inner.height)) {
                let label = format!(
                    "{} · {} · {}",
                    super::format::author(&message.author),
                    message.created_at,
                    message.body.lines().find(|line| !line.trim().is_empty()).unwrap_or("(empty message)")
                );
                let style = if index == cursor { palette().selection } else { Style::default() };
                frame.render_widget(
                    Paragraph::new(label).style(style),
                    Rect::new(inner.x, inner.y + (index - start) as u16, inner.width, 1),
                );
            }
        })?;
        let Event::Key(key) = event::read()? else { continue };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => cursor = cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => cursor = (cursor + 1).min(messages.len().saturating_sub(1)),
            KeyCode::Enter => {
                if let Some(message) = messages.get(cursor) {
                    super::attachments::annotate_message(
                        client,
                        terminal,
                        thread_id,
                        &message.id,
                        &message.body,
                    )?;
                }
            }
            _ => {}
        }
    }
}

pub(crate) fn question_reference(
    client: &Client,
    terminal: &mut DefaultTerminal,
    inbox: &Inbox,
) -> Result<()> {
    let thread = inbox
        .thread
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("open a thread and focus a question with Tab"))?;
    let (message, question) =
        thread.focused().ok_or_else(|| anyhow::anyhow!("focus a question with Tab first"))?;
    let choice = thread.focus().and_then(|focus| question.choices.get(focus.choice));
    let model = client.attachments(thread.id())?;
    let attachments: Vec<_> = model.files().filter(|file| file.message_id == message.id).cloned().collect();
    if let Some(prototype) =
        choice.and_then(|choice| choice.prototype.as_ref()).or(question.prototype.as_ref())
    {
        let raw: serde_json::Value = client
            .get_json("GET", &format!("/api/inbox/threads/{}", super::client::path_id(thread.id())?))?;
        let sites: Vec<(String, String)> = raw
            .get("thread")
            .and_then(|value| value.get("messages"))
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter(|value| value.get("id").and_then(serde_json::Value::as_str) == Some(message.id.as_str()))
            .flat_map(|value| {
                value.get("live_sites").and_then(serde_json::Value::as_array).into_iter().flatten()
            })
            .filter_map(|site| {
                Some((site.get("id")?.as_str()?.to_owned(), site.get("url")?.as_str()?.to_owned()))
            })
            .collect();
        let root = thread.model.thread.project.root.as_deref().unwrap_or("");
        let url = super::attachments::prototype_url(
            client,
            &prototype.target,
            &attachments,
            root,
            None,
            thread.id(),
            &sites,
        )?
        .ok_or_else(|| {
            anyhow::anyhow!("prototype target is not a checked live site or attached HTML file")
        })?;
        return super::attachments::open_browser(url.as_str());
    }
    if let Some(reference) = &question.attachment
        && let Some(file) = attachments.iter().find(|file| file.id == reference.id)
    {
        return super::attachments::open_attachment(
            client,
            terminal,
            file,
            super::attachments::Version::Current,
        );
    }
    let source = choice
        .and_then(|choice| choice.image.as_ref())
        .map(|image| image.src.as_str())
        .or_else(|| question.shows.as_ref().map(|shows| shows.src.as_str()))
        .ok_or_else(|| anyhow::anyhow!("this question has no prototype, example or attached source"))?;
    if let Some(file) = attachments
        .iter()
        .find(|file| file.image_ref.as_deref() == Some(source) || file.path == source || file.name == source)
    {
        super::attachments::open_attachment(client, terminal, file, super::attachments::Version::Current)
    } else {
        super::attachments::open_browser(source)
    }
}
