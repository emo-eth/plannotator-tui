use super::{ReviewAction, ReviewPane, feedback, model};
use crate::{inbox::client::Client, theme::palette};
use anyhow::{Context, Result, ensure};
use ratatui::{
    DefaultTerminal,
    crossterm::event::{self, Event, KeyCode, KeyEventKind},
    layout::{Constraint, Layout},
    style::Style,
    text::Line,
    widgets::{Block, Borders, Paragraph},
};
use serde_json::Value;

thread_local! {
    static KEPT: std::cell::RefCell<std::collections::HashMap<String, ReviewPane>> = std::cell::RefCell::new(std::collections::HashMap::new());
}

#[derive(Debug)]
struct Entry {
    label: String,
    thread: Option<String>,
    message: Option<String>,
    pull: Option<String>,
}

/// Open all reviews, or every guide card in one thread. GETs only until the person acts.
pub(crate) fn run(client: &Client, terminal: &mut DefaultTerminal, thread_id: Option<&str>) -> Result<()> {
    let mut entries = entries_read(client, thread_id)?;
    let mut selected = 0usize;
    let mut status = String::new();
    loop {
        terminal.draw(|frame| {
            let bands = Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).split(frame.area());
            let [body, footer] = bands.as_ref() else { return };
            let height = usize::from(body.height.saturating_sub(2));
            let scroll = selected.saturating_sub(height.saturating_sub(1));
            let lines: Vec<_> = if entries.is_empty() {
                vec![Line::from("No guided reviews here yet.")]
            } else {
                entries
                    .iter()
                    .enumerate()
                    .skip(scroll)
                    .take(height)
                    .map(|(index, entry)| {
                        Line::styled(
                            format!("{} {}", if index == selected { ">" } else { " " }, entry.label),
                            if index == selected { palette().selection } else { Style::default() },
                        )
                    })
                    .collect()
            };
            frame.render_widget(
                Paragraph::new(lines).block(
                    Block::default()
                        .title(if thread_id.is_some() {
                            "Thread reviews and walkthroughs"
                        } else {
                            "Reviews"
                        })
                        .borders(Borders::ALL),
                ),
                *body,
            );
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from("j/k select · Enter open · R refresh · Esc back"),
                    Line::from(status.clone()),
                ]),
                *footer,
            );
        })?;
        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Release {
                continue;
            }
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => return Ok(()),
                KeyCode::Down | KeyCode::Char('j') => {
                    selected = selected.saturating_add(1).min(entries.len().saturating_sub(1));
                }
                KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
                KeyCode::Char('R') => match entries_read(client, thread_id) {
                    Ok(next) => {
                        entries = next;
                        selected = selected.min(entries.len().saturating_sub(1));
                        status.clear();
                    }
                    Err(error) => status = format!("{error:#}"),
                },
                KeyCode::Enter => {
                    if let Some(entry) = entries.get(selected) {
                        let result = match (&entry.thread, &entry.message, &entry.pull) {
                            (Some(thread), Some(message), _) => run_guide(client, terminal, thread, message),
                            (_, _, Some(pull)) => run_pull(client, terminal, pull),
                            (Some(thread), _, _) => run(client, terminal, Some(thread)),
                            _ => Err(anyhow::anyhow!("This review has no thread or pull request to open")),
                        };
                        if let Err(error) = result {
                            status = format!("{error:#}");
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn entries_read(client: &Client, thread_id: Option<&str>) -> Result<Vec<Entry>> {
    if let Some(thread) = thread_id {
        let response: Value =
            client.get_json("GET", &format!("/api/inbox/threads/{}", model::segment(thread)))?;
        ensure!(
            response.get("serverSession").and_then(Value::as_str) == Some(client.server_session()),
            "Inbox session changed; reopen Reviews"
        );
        return Ok(model::links(response.get("thread").context("Inbox omitted thread")?)?
            .into_iter()
            .rev()
            .map(|message| Entry {
                label: format!(
                    "{} · {}",
                    if message.guide.walkthrough {
                        "Walkthrough"
                    } else if message.guide.review {
                        "Review"
                    } else {
                        "Guided review"
                    },
                    message.guide.title
                ),
                thread: Some(thread.into()),
                message: Some(message.id),
                pull: None,
            })
            .collect());
    }
    let response: Value = client.get_json("GET", "/api/inbox/reviews")?;
    list_entries(&response)
}

fn list_entries(response: &Value) -> Result<Vec<Entry>> {
    let rows = response.get("rows").and_then(Value::as_array).context("Inbox omitted Reviews rows")?;
    let mut entries = Vec::with_capacity(rows.len());
    for (key, label) in [
        ("on_you", "On you"),
        ("agents", "With your agents"),
        ("ready", "Ready to land"),
        ("waiting", "Waiting on others"),
        ("not_reviewed", "Open, not reviewed"),
    ] {
        for row in rows.iter().filter(|row| feedback::field(row, "move") == key) {
            let review = row.get("review").unwrap_or(&Value::Null);
            let title = review
                .get("title")
                .and_then(Value::as_str)
                .or_else(|| row.pointer("/stacks/0/pulls/0/title").and_then(Value::as_str))
                .or_else(|| row.pointer("/started/title").and_then(Value::as_str))
                .unwrap_or_else(|| feedback::field(row, "id"));
            let reason = feedback::field(row, "reason").replace('_', " ");
            let pull = row
                .get("pull_keys")
                .and_then(Value::as_array)
                .and_then(|keys| keys.first())
                .and_then(Value::as_str)
                .map(str::to_owned);
            entries.push(Entry {
                label: format!("{label} · {title} · {reason}"),
                thread: row.get("thread_id").and_then(Value::as_str).map(str::to_owned),
                message: review.get("latest_review_id").and_then(Value::as_str).map(str::to_owned),
                pull,
            });
        }
    }
    Ok(entries)
}

/// Open an exact guide message (artifact wall and board links use this entrypoint).
pub(crate) fn run_guide(
    client: &Client,
    terminal: &mut DefaultTerminal,
    thread_id: &str,
    message_id: &str,
) -> Result<()> {
    let mut pane = KEPT
        .with(|kept| kept.borrow_mut().remove(message_id))
        .map_or_else(|| ReviewPane::load(client, thread_id, message_id), Ok)?;
    loop {
        terminal.draw(|frame| pane.draw(frame, frame.area()))?;
        match event::read()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => match pane.handle_key(key, client) {
                Ok(ReviewAction::Close) => {
                    KEPT.with(|kept| kept.borrow_mut().insert(message_id.to_owned(), pane));
                    return Ok(());
                }
                Ok(ReviewAction::Stay) => {}
                Ok(ReviewAction::PostForge) => {
                    let result =
                        crate::inbox::guide_forge::run(client, terminal, &pane.thread_id, &pane.message.id);
                    if let Err(error) = result {
                        pane.status = format!("{error:#}");
                    }
                    if let Err(error) = pane.refresh_comments(client) {
                        pane.status = format!("Refreshing guide comments: {error:#}");
                    }
                }
                Ok(ReviewAction::Ask { words, show_me }) => {
                    let result = open_ask(client, terminal, &mut pane, &words, show_me);
                    if let Err(error) = result {
                        pane.status = format!("{error:#}");
                    }
                }
                Err(error) => pane.status = format!("{error:#}"),
            },
            Event::Paste(text) => pane.paste(&text),
            _ => {}
        }
    }
}

fn open_ask(
    client: &Client,
    terminal: &mut DefaultTerminal,
    pane: &mut ReviewPane,
    words: &str,
    show_me: Option<Value>,
) -> Result<()> {
    use crate::inbox::ask;
    if let Some(target) = show_me {
        ask::seed_show_me(&pane.thread_id, words, target, None)?;
    } else {
        ask::seed(&pane.thread_id, words, Some(&pane.message.guide.title));
    }
    match ask::run(client, terminal, Some(&pane.thread_id))? {
        Some(ask::Action::OpenShape { thread_id, shape_id }) => {
            crate::inbox::shapes::run_at(client, terminal, Some(&thread_id), &shape_id)?;
        }
        Some(ask::Action::Reply { words, .. }) => {
            if !pane.note.is_empty() {
                pane.note.push_str("\n\n");
            }
            pane.note.push_str(&words);
            pane.mode = super::Mode::Send(super::SendKind::Feedback);
            pane.status = "Ask answer copied to a feedback note; Enter sends, n edits, Esc cancels".into();
        }
        _ => {}
    }
    Ok(())
}

fn run_pull(client: &Client, terminal: &mut DefaultTerminal, key: &str) -> Result<()> {
    use crate::inbox::pulls::{PullAction, ThreadPullsPane};
    let mut pane = ThreadPullsPane::load_pull(client, key)?;
    loop {
        terminal.draw(|frame| pane.draw(frame, frame.area()))?;
        match event::read()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => match pane.handle_key(key, client)? {
                PullAction::Close => return Ok(()),
                PullAction::Stay => {}
                PullAction::Guide { key } => crate::inbox::guide_forge::run_for_pull(client, terminal, &key)?,
            },
            Event::Paste(text) => pane.paste(&text),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn list_opens_latest_review_and_keeps_unreviewed_pull_rows() -> Result<()> {
        let entries = list_entries(&json!({"rows":[
            {"id":"p","move":"not_reviewed","reason":"not_reviewed","review":null,"pull_keys":["github.com/a/b#2"]},
            {"id":"t","thread_id":"t","move":"on_you","reason":"review_ready","review":{"title":"A","latest_review_id":"m2"}}
        ]}))?;
        assert_eq!(entries.first().and_then(|entry| entry.message.as_deref()), Some("m2"));
        assert_eq!(entries.last().and_then(|entry| entry.pull.as_deref()), Some("github.com/a/b#2"));
        Ok(())
    }
}
