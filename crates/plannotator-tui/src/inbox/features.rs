//! Discoverable entry points for Inbox surfaces. All runners borrow the Inbox's terminal;
//! opening the menu itself is pure, and a panel is entered only after an explicit key.
//! Extended settings expose prompts, agents, sidebar, reactions, voice, tour and accounts
//! without requiring an open thread. Display-name writes, an interactive terminal tour and
//! account switching remain unsupported; the panel reports those limits without claiming success.

use anyhow::Result;
use ratatui::DefaultTerminal;

use super::client::Client;
use super::ui::{Inbox, Request};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Feature {
    Board,
    Launch,
    Attachments,
    MessageNotes,
    QuestionReference,
    Pulls,
    Reviews,
    Actions,
    Search,
    Decisions,
    Agents,
    Projects,
    Ask,
    Drafts,
    Artifacts,
    Shapes,
    Media,
    Machines,
    History,
    Sharing,
    Settings,
    SettingsExtra,
    Icons,
    Update,
}

pub(crate) const ALL: &[Feature] = &[
    Feature::Board,
    Feature::Launch,
    Feature::Attachments,
    Feature::Pulls,
    Feature::MessageNotes,
    Feature::QuestionReference,
    Feature::Reviews,
    Feature::Actions,
    Feature::Search,
    Feature::Decisions,
    Feature::Agents,
    Feature::Projects,
    Feature::Ask,
    Feature::Drafts,
    Feature::Artifacts,
    Feature::Shapes,
    Feature::Media,
    Feature::Machines,
    Feature::History,
    Feature::Sharing,
    Feature::Settings,
    Feature::SettingsExtra,
    Feature::Icons,
    Feature::Update,
];

impl Feature {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Board => "Board · cards, columns, assignments and launch",
            Self::Launch => "Sessions · launch, resume, stop, permissions and terminals",
            Self::Attachments => "Attachments · read, annotate, sent versions and prototypes",
            Self::MessageNotes => "Message notes · annotate agent text and reply to its author",
            Self::QuestionReference => "Question reference · prototype, example or attached source",
            Self::Pulls => "Pull requests · link, inspect, check and review",
            Self::Reviews => "Guided reviews · walkthroughs, comments and verdicts",
            Self::Actions => "Thread actions · read, pin, snooze, resolve, react and skip",
            Self::Search => "Search · threads, messages and filters",
            Self::Decisions => "Decisions · browse, export and post",
            Self::Agents => "Agents · presence, faces and associated threads",
            Self::Projects => "Projects · filter, rename, combine and manage",
            Self::Ask => "Ask AI · conversations and reply suggestions",
            Self::Drafts => "Reply draft · saved text, files, recipients and conflicts",
            Self::Artifacts => "Artifacts · project files, guides and pins",
            Self::Shapes => "Shapes / System · designs and change requests",
            Self::Media => "Media · images, voice and video",
            Self::Machines => "Machines · peers and connection controls",
            Self::History => "History · archive, activity, storage and tidy",
            Self::Sharing => "Share / export · copy Markdown, links and artifacts",
            Self::Settings => "Settings · preferences, appearance and integrations",
            Self::SettingsExtra => "More settings · prompts, agents, sidebar, voice and accounts",
            Self::Icons => "Agent icons · sets, uploads and animation",
            Self::Update => "Updates · check, install and restart",
        }
    }

    pub(crate) fn needs_thread(self) -> bool {
        matches!(
            self,
            Self::Attachments
                | Self::MessageNotes
                | Self::QuestionReference
                | Self::Pulls
                | Self::Actions
                | Self::Drafts
                | Self::Media
                | Self::Sharing
        )
    }
}

/// A panel may navigate back into a thread; reads after returning reconcile every surface.
pub(crate) fn run(
    feature: Feature,
    client: &Client,
    terminal: &mut DefaultTerminal,
    inbox: &mut Inbox,
) -> Result<()> {
    let selected = inbox.context_thread_id();
    let thread = selected.as_deref();
    match feature {
        Feature::Board => board(client, terminal, inbox)?,
        Feature::Launch => super::launch::run(client, terminal, thread)?,
        Feature::Attachments => super::attachments::run(client, terminal, required(thread)?)?,
        Feature::MessageNotes => super::navigation::message_notes(client, terminal, required(thread)?)?,
        Feature::QuestionReference => super::navigation::question_reference(client, terminal, inbox)?,
        Feature::Pulls => pulls(client, terminal, required(thread)?)?,
        Feature::Reviews => super::reviews::run(client, terminal, thread)?,
        Feature::Actions => super::actions::run(client, terminal, required(thread)?)?,
        Feature::Search => super::navigation::search(client, terminal, inbox)?,
        Feature::Decisions => {
            if let Some(id) = super::decisions::run(client, terminal, thread)? {
                super::perform(inbox, client, Request::Thread(id));
            }
        }
        Feature::Agents => super::navigation::agents(client, terminal, inbox)?,
        Feature::Projects => {
            if let Some(super::projects::ProjectAction::Filter(project)) =
                super::projects::run(client, terminal, thread, inbox.project_filter.as_deref())?
            {
                inbox.set_project_filter(project);
            }
        }
        Feature::Ask => match super::ask::run(client, terminal, thread)? {
            Some(super::ask::Action::Reply { thread_id, words }) => {
                inbox.opened_thread(client.thread(&thread_id)?);
                super::perform(inbox, client, Request::ComposeReply);
                if matches!(inbox.mode, super::ui::Mode::Compose(super::ui::ComposeFor::Reply)) {
                    inbox.compose.insert_text(&words);
                } else {
                    inbox.compose_reply(&words);
                }
            }
            Some(super::ask::Action::OpenShape { thread_id, shape_id }) => {
                super::shapes::run_at(client, terminal, Some(&thread_id), &shape_id)?;
            }
            _ => {}
        },
        Feature::Drafts => {
            let id = required(thread)?;
            if inbox.thread.as_ref().is_none_or(|open| open.id() != id) {
                inbox.opened_thread(client.thread(id)?);
            }
            if inbox.draft.is_none() {
                inbox.draft = Some(super::drafts::DraftState::load(client, required(thread)?)?);
                if let (Some(draft), Some(thread)) = (inbox.draft.as_mut(), inbox.thread.as_ref()) {
                    draft.pin_recipient(&thread.model.thread)?;
                }
            }
            let composing = matches!(inbox.mode, super::ui::Mode::Compose(super::ui::ComposeFor::Reply));
            if let Some(draft) = inbox.draft.as_mut() {
                super::drafts::edit_draft(client, terminal, draft)?;
            }
            if composing {
                let words = inbox.draft.as_ref().map_or("", |draft| draft.body()).to_owned();
                inbox.compose_reply(&words);
            }
        }
        Feature::Artifacts => super::artifacts::run(client, terminal, thread)?,
        Feature::Shapes => super::shapes::run(client, terminal, thread)?,
        Feature::Media => {
            if let Some(words) = super::media::run(client, terminal, thread)? {
                let id = required(thread)?;
                if inbox.thread.as_ref().is_none_or(|open| open.id() != id) {
                    inbox.opened_thread(client.thread(id)?);
                }
                if !matches!(inbox.mode, super::ui::Mode::Compose(_)) {
                    super::perform(inbox, client, Request::ComposeReply);
                }
                inbox.compose.insert_text(&words);
            }
        }
        Feature::Machines => super::machines::run(client, terminal)?,
        Feature::History => {
            if let Some(id) = super::history::run(client, terminal, thread)? {
                super::perform(inbox, client, Request::Thread(id));
            }
        }
        Feature::Sharing => super::sharing::run(client, terminal, thread)?,
        Feature::Settings => settings(client, terminal)?,
        Feature::SettingsExtra => super::settings_extra::run(client, terminal)?,
        Feature::Icons => super::main_delta::run(client, terminal)?,
        Feature::Update => super::update::run(client, terminal, thread)?,
    }
    Ok(())
}

fn required(thread: Option<&str>) -> Result<&str> {
    thread.ok_or_else(|| anyhow::anyhow!("select or open a thread first"))
}

fn board(client: &Client, terminal: &mut DefaultTerminal, inbox: &mut Inbox) -> Result<()> {
    use super::board::{Board, BoardAction};
    use ratatui::crossterm::event::{self, Event, KeyEventKind};
    let mut board = Board::default();
    board.refresh(client)?;
    let live = client.subscribe(board.cursor());
    loop {
        terminal.draw(|frame| board.draw(frame, frame.area()))?;
        let mut changed = false;
        while let Ok(event) = live.try_recv() {
            match event {
                super::client::Live::Record { .. } | super::client::Live::Refresh => changed = true,
                super::client::Live::Restarted { .. } => return Ok(()),
                _ => {}
            }
        }
        if changed && let Err(error) = board.refresh(client) {
            board.failed(&format!("{error:#}"));
        }
        if !event::poll(std::time::Duration::from_millis(250))? {
            continue;
        }
        match event::read()? {
            Event::Paste(text) => board.paste(&text),
            Event::Key(key) if key.kind != KeyEventKind::Release => match board.handle_key(key, client) {
                Ok(Some(BoardAction::Close)) => return Ok(()),
                Ok(Some(BoardAction::OpenThread(id))) => match client.thread(&id) {
                    Ok(thread) => {
                        inbox.opened_thread(thread);
                        return Ok(());
                    }
                    Err(error) => board.failed(&format!("{error:#}")),
                },
                Ok(Some(BoardAction::Compose { card_id, body, send })) => {
                    match super::launch::compose(client, terminal, &body, false) {
                        Ok(Some(started)) => {
                            if let Err(error) = board.composed(client, &card_id, &started.thread_id, send) {
                                board.failed(&format!("{error:#}"));
                            }
                        }
                        Ok(None) => {}
                        Err(error) => board.failed(&format!("{error:#}")),
                    }
                }
                Ok(None) => {}
                Err(error) => board.failed(&format!("{error:#}")),
            },
            _ => {}
        }
    }
}

fn pulls(client: &Client, terminal: &mut DefaultTerminal, thread: &str) -> Result<()> {
    use super::pulls::{PullAction, ThreadPullsPane};
    use ratatui::crossterm::event::{self, Event, KeyEventKind};
    let mut pane = ThreadPullsPane::load(client, thread)?;
    let live = client.subscribe(client.list()?.cursor);
    let mut status = String::new();
    loop {
        terminal.draw(|frame| {
            let [body, footer] = ratatui::layout::Layout::vertical([
                ratatui::layout::Constraint::Min(1),
                ratatui::layout::Constraint::Length(1),
            ])
            .areas(frame.area());
            pane.draw(frame, body);
            frame.render_widget(ratatui::widgets::Paragraph::new(status.as_str()), footer);
        })?;
        let mut changed = false;
        while let Ok(event) = live.try_recv() {
            match event {
                super::client::Live::Record { .. } | super::client::Live::Refresh => changed = true,
                super::client::Live::Restarted { .. } => return Ok(()),
                _ => {}
            }
        }
        if changed && let Err(error) = pane.refresh(client) {
            status = format!("{error:#}");
        }
        if !event::poll(std::time::Duration::from_millis(250))? {
            continue;
        }
        match event::read()? {
            Event::Paste(text) => pane.paste(&text),
            Event::Key(key) if key.kind != KeyEventKind::Release => match pane.handle_key(key, client) {
                Ok(PullAction::Close) => return Ok(()),
                Ok(PullAction::Stay) => {}
                Ok(PullAction::Guide { key }) => {
                    if let Err(error) = super::guide_forge::run_for_pull(client, terminal, &key) {
                        status = format!("{error:#}");
                    }
                }
                Err(error) => status = format!("{error:#}"),
            },
            _ => {}
        }
    }
}

fn settings(client: &Client, terminal: &mut DefaultTerminal) -> Result<()> {
    use super::settings::{SettingsAction, SettingsPane};
    use ratatui::crossterm::event::{self, Event, KeyEventKind};
    use ratatui::layout::{Constraint, Layout};
    use ratatui::widgets::Paragraph;
    let mut pane = SettingsPane::load(client)?;
    let mut status = String::new();
    loop {
        terminal.draw(|frame| {
            let [body, footer] =
                Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());
            pane.draw(frame, body);
            frame.render_widget(Paragraph::new(status.as_str()), footer);
        })?;
        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Release {
                continue;
            }
            match pane.handle_key(key) {
                SettingsAction::Close => return Ok(()),
                SettingsAction::Refresh => {
                    if let Err(error) = pane.refresh(client) {
                        status = format!("{error:#}");
                    }
                }
                SettingsAction::Save(patch) => match super::settings::write(client, &patch) {
                    Ok(value) => {
                        pane.replace(value);
                        "Settings saved".clone_into(&mut status);
                    }
                    Err(error) => status = format!("Settings not saved: {error:#}"),
                },
                SettingsAction::None => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ALL, Feature};

    #[test]
    fn extended_settings_are_available_without_a_thread() {
        assert!(ALL.contains(&Feature::SettingsExtra));
        assert!(!Feature::SettingsExtra.needs_thread());
    }
}
