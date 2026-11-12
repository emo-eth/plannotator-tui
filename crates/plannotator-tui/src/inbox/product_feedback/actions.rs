use anyhow::{Result, bail};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::widgets::Paragraph;

use super::super::client::Client;
use super::super::settings::{SettingsAction, SettingsPane, SettingsSection};
use super::help::{Help, Hit};
use super::screen::{self, Selection};

pub(super) fn questions(client: &Client, terminal: &mut DefaultTerminal, help: &Help) -> Result<()> {
    let items: Vec<_> = help
        .known
        .iter()
        .enumerate()
        .filter_map(|(index, known)| known.faq.as_ref().map(|faq| (index, faq.question.clone())))
        .collect();
    let labels = items.iter().map(|(_, question)| question.clone()).collect::<Vec<_>>();
    loop {
        match screen::choose(
            terminal,
            "Common questions",
            "Bundled upstream help. Answers describe the browser Inbox where indicated. Esc returns to your unsent message.",
            &labels,
        )? {
            Selection::Item(index) => {
                if let Some((index, _)) = items.get(index) {
                    show(client, terminal, help, Hit::Known(*index))?;
                }
            }
            Selection::Back => return Ok(()),
        }
    }
}

pub(super) fn related(
    client: &Client,
    terminal: &mut DefaultTerminal,
    help: &Help,
    hits: &[Hit],
) -> Result<()> {
    if hits.is_empty() {
        return screen::notice(
            terminal,
            "These might help",
            "No matching bundled answer or feature. You can still submit your message. Matching is local; the full upstream issue/PR/release-title search is not included.",
        );
    }
    let labels = hits.iter().map(|hit| help.label(*hit)).collect::<Vec<_>>();
    loop {
        match screen::choose(
            terminal,
            "These might help",
            "Local common answers and curated features. Select one for details, a settings action or a public documentation link. Your message is not sent.",
            &labels,
        )? {
            Selection::Item(index) => {
                if let Some(hit) = hits.get(index) {
                    show(client, terminal, help, *hit)?;
                }
            }
            Selection::Back => return Ok(()),
        }
    }
}

fn show(client: &Client, terminal: &mut DefaultTerminal, help: &Help, hit: Hit) -> Result<()> {
    let (title, mut detail, action, link) = match hit {
        Hit::Known(index) => {
            let Some(known) = help.known.get(index) else { return Ok(()) };
            (
                known.label(),
                known.faq.as_ref().map_or_else(|| known.label(), |faq| faq.answer.clone()),
                known.action.as_ref(),
                known.link.as_ref().map(|link| (link.label.as_str(), link.href.as_str())),
            )
        }
        Hit::Feature(index) => {
            let Some(feature) = help.features.get(index) else { return Ok(()) };
            (
                feature.title.clone(),
                feature.definition.clone(),
                None,
                Some(("Documentation", feature.url.as_str())),
            )
        }
    };
    if let Some((_, url)) = link {
        detail.push_str("\n\n");
        detail.push_str(url);
    }
    let mut labels = vec!["Back to feedback".to_owned()];
    if let Some(action) = action {
        if action.kind == "simple-list" {
            labels.push("How to use Simple list".into());
            detail.push_str(
                "\n\nThe terminal's Simple list is session-local; the browser keeps a separate preference.",
            );
        } else {
            labels.push(action.label.clone());
        }
    }
    if let Some((label, _)) = link {
        labels.push(format!("Open {label} in browser"));
    }
    loop {
        match screen::choose(terminal, &title, &detail, &labels)? {
            Selection::Back | Selection::Item(0) => return Ok(()),
            Selection::Item(index) if index == 1 && action.is_some() => {
                if let Some(action) = action
                    && let Err(error) = perform(client, terminal, &action.kind)
                {
                    screen::notice(terminal, "Could not open help action", &format!("{error:#}"))?;
                }
            }
            Selection::Item(_) => {
                if let Some((_, url)) = link
                    && let Err(error) = super::super::attachments::open_browser(url)
                {
                    screen::notice(
                        terminal,
                        "Could not open documentation",
                        &format!("{error:#}\n\nOpen manually: {url}"),
                    )?;
                }
            }
        }
    }
}

fn perform(client: &Client, terminal: &mut DefaultTerminal, kind: &str) -> Result<()> {
    match kind {
        "simple-list" => screen::notice(
            terminal,
            "Simple list",
            "Press v in the terminal Inbox list to switch between Smart groups and Simple list. This choice lasts for the current session. The browser has its own separate preference under View → Simple list.",
        ),
        "settings-devices" | "settings-machines" => super::super::machines::run(client, terminal),
        "settings-prompts" => super::super::settings_extra::run(client, terminal),
        "settings-about" => super::super::update::run(client, terminal, None),
        "settings-general" => settings(client, terminal, SettingsSection::General),
        "settings-rows" => settings(client, terminal, SettingsSection::Rows),
        "settings-agents" => settings(client, terminal, SettingsSection::Agents),
        "settings-voice" => settings(client, terminal, SettingsSection::Voice),
        "settings-capabilities" | "settings-experimental" => {
            settings(client, terminal, SettingsSection::Capabilities)
        }
        _ => bail!("Unknown feedback help action: {kind}"),
    }
}

fn settings(client: &Client, terminal: &mut DefaultTerminal, section: SettingsSection) -> Result<()> {
    let mut pane = SettingsPane::load(client)?;
    for _ in 0..7 {
        if pane.section() == section {
            break;
        }
        pane.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    }
    let mut status = String::new();
    loop {
        terminal.draw(|frame| {
            let [body, footer] =
                Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).areas(frame.area());
            pane.draw(frame, body);
            frame.render_widget(
                Paragraph::new(format!(
                    "Esc returns to feedback. Changes require a settings action.\n{status}"
                )),
                footer,
            );
        })?;
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match pane.handle_key(key) {
                SettingsAction::Close => return Ok(()),
                SettingsAction::Refresh => {
                    if let Err(error) = pane.refresh(client) {
                        status = format!("{error:#}");
                    }
                }
                SettingsAction::Save(patch) => match super::super::settings::write(client, &patch) {
                    Ok(response) => {
                        pane.saved(response);
                        status.clear();
                    }
                    Err(error) => status = format!("Settings not saved: {error:#}"),
                },
                SettingsAction::None => {}
            }
        }
    }
}
