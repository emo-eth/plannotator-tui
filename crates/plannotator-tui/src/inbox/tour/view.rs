use std::fmt::Write as _;
use std::path::Path;

use anyhow::Result;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use super::fixture::{DESIGN, FILM, FIRST_QUESTION, NEW};
use super::model::{Replay, Stage};
use super::{Editor, Pane};

impl Pane {
    pub(super) fn refresh_body(&mut self) -> Result<()> {
        let mut body = self.body_text()?;
        if let Some(name) = &self.viewer
            && let Some(text) = self.assets.texts.get(name)
        {
            body = if Path::new(name).extension().is_some_and(|extension| extension == "html") {
                format!(
                    "# prototype.html — source only\n\nThe actual HTML mock is loaded as inert source. Interactive HTML preview is unavailable: a separate browser request cannot be guaranteed to preserve a checked sandbox. No browser navigation is performed.\n\nPress Esc to return, or Enter to continue.\n\n```html\n{text}\n```"
                )
            } else {
                text.clone()
            };
        }
        let text = tui_markdown::from_str(&body);
        self.body = text
            .lines
            .into_iter()
            .map(|line| {
                Line::from(
                    line.spans
                        .into_iter()
                        .map(|span| Span::styled(span.content.into_owned(), span.style))
                        .collect::<Vec<_>>(),
                )
                .style(line.style)
            })
            .collect();
        self.width = 0;
        Ok(())
    }

    fn body_text(&self) -> Result<String> {
        let design = self.fixture.thread(DESIGN)?;
        let stage = self.replay.stage;
        let mut text = match stage {
            Stage::Entry => format!("# Two paths\n\nTake the guided tour, or connect your agents.\n\nReplay real threads from building {} with its own Inbox ({}), curated by hand. Picks, comments, circles, sends and new threads exist only in this pane; nothing reaches real threads.\n\n**Enter: start the tour and save its seen timestamp.**\n\n**a: connect agents and save skipped.**\n\nEsc leaves without writing anything. Image and film playback needs ffmpeg on PATH.\n\nOnly the explicitly labeled appearance and tour-status actions write real settings.", self.fixture.project, self.fixture.date),
            Stage::Orient => format!("# How it is organised\n\nProjects contain threads about one subject; a thread is a conversation with attached files.\n\n## {}\n\n- Waiting on you: {}\n- Quiet: {}\n\n{}\n\nEnter: show the first question.", self.fixture.project, design.subject, self.fixture.thread(FILM)?.subject, design.slice),
            Stage::Open => format!("# {}\n\n{} asked Claude Code to rethink a VS Code feature, then went back to other work.\n\n**Waiting on you**\n\n> {}\n\nEnter opens this fixture thread.", self.fixture.project, self.fixture.developer, design.subject),
            Stage::WarmPick | Stage::WarmSend | Stage::WarmReplay => {
                let message = self.fixture.question_message(true)?;
                format!("# {}\n\n### Claude Code · {}\n\n{}", design.subject, message.time, message.body.split(":::question").next().unwrap_or(&message.body))
            }
            Stage::Picture | Stage::Words | Stage::FileOpen | Stage::FileComment | Stage::Send => {
                let message = self.fixture.question_message(false)?;
                format!("# {}\n\n### Claude Code · {}\n\n{}", design.subject, message.time, message.body.split(":::question").next().unwrap_or(&message.body))
            }
            Stage::Replay => Replay::replay_text(&self.fixture, DESIGN, self.replay.arrived)?,
            Stage::ImageOpen | Stage::ImageDraw | Stage::ImageComment | Stage::Artifacts | Stage::ArtifactOpen => Replay::replay_text(&self.fixture, DESIGN, 2)?,
            Stage::FilmOpen | Stage::FilmPlay | Stage::FilmComment | Stage::FilmSend | Stage::FilmReplay => {
                let film = self.fixture.thread(FILM)?;
                let mut text = format!("# {}\n", film.subject);
                for message in film.messages.iter().filter(|message| message.replay == 0) { let _ = writeln!(text, "\n### Claude Code · {}\n\n{}", message.time, message.body); }
                let _ = writeln!(text, "\n{}", self.fixture.asset("film-clip.mp4")?.note);
                if stage == Stage::FilmReplay && self.replay.arrived > 0 { text.push_str(&Replay::film_reply(&self.fixture)?); }
                text
            }
            Stage::NewOpen => "# Start a thread yourself\n\nNew message writes to an agent session in any project. The tour offers only its fixture session, never a live session.\n\nEnter: New message.".to_owned(),
            Stage::NewRecipient => format!("# New message\n\nTo: **not chosen**\n\n> Claude Code · {} · tour-session\n\nEnter chooses this fixture recipient.", self.fixture.project),
            Stage::NewCompose | Stage::NewSend | Stage::NewReplay => format!("# New message\n\nTo: Claude Code · {} · **tour-session (fixture)**\n\n{}", self.fixture.project, self.replay.new_body),
            Stage::Loop => format!("# That is the loop\n\nAgents ask, you answer when you can, and they keep going.\n\nYour draft request:\n\n{}\n\nRecorded reply (not generated from your text):\n{}", self.replay.new_body, Replay::replay_text(&self.fixture, NEW, 1)?),
            Stage::Also => "# Also in your Inbox\n\n- Star a thread to keep it at the top.\n- Snooze puts a thread away until later and brings it back on time.\n- Looks good tells an agent when there is nothing to add.\n- Re-ask gathers open questions into one fresh message.\n- Send now puts your reply into a turn that is running.\n\nThese real-thread controls are outside the tour. Enter: appearance.".to_owned(),
            Stage::Appearance => format!("# Make it yours — REAL SETTINGS\n\nLeft/right saves the selected setting immediately to the real Inbox. Color sets choose **both light and dark halves**, as upstream's tour does. Text size affects the browser Inbox, not your terminal font.\n\n{} Theme: {}\n\n{} Colors (both halves): light {} / dark {}\n\n{} Browser text size: {} px\n\nEnter keeps your choices and continues. s skips and restores only this tour's changes; fields another window changed are preserved.\n\nThe terminal's own configured palette/font is unchanged.", self.mark(0), self.appearance.value("mode"), self.mark(1), self.appearance.value("light"), self.appearance.value("dark"), self.mark(2), self.appearance.value("size")),
            Stage::Connect => format!("# Connect agents next\n\nReplay ended. Your real threads are as you left them.\n\n**a: save finished and open real connection settings** (changes there have their own confirmation).\n\n**c: copy this prompt**, then paste it into a connected agent yourself:\n\n> {FIRST_QUESTION}\n\n**Enter: save finished and leave.** This also sets notify_again so the browser Inbox can ask about notifications. It never grants notification permission.\n\nSkipped interactions: {:?}. Skipped means not completed.", self.replay.skipped),
        };
        self.append_questions(&mut text, stage, design)?;
        Ok(text)
    }

    fn append_questions(
        &self,
        text: &mut String,
        stage: Stage,
        design: &super::fixture::Thread,
    ) -> Result<()> {
        use std::fmt::Write as _;
        if matches!(stage, Stage::WarmPick | Stage::Picture) {
            let question = self.fixture.question(stage == Stage::WarmPick)?;
            let _ = writeln!(text, "\n## {}", question.prompt);
            for (index, choice) in question.choices.iter().enumerate() {
                let _ = writeln!(
                    text,
                    "\n{} {}\n\n{}{}",
                    self.mark(index),
                    choice.label,
                    choice.description,
                    if choice.picture.is_some() { " [v: view picture]" } else { "" }
                );
            }
            text.push_str("\nOther: o · Skip question: x (still sent only when you press Send).\n");
        }
        if matches!(stage, Stage::WarmSend | Stage::WarmReplay) {
            let _ =
                writeln!(text, "\nYour draft answer: **{}**", self.replay.warm.as_deref().unwrap_or("none"));
        }
        if stage.step() >= 3
            && self.replay.warm.as_deref().is_some_and(|answer| answer != design.warmup_answer)
        {
            let _ = writeln!(
                text,
                "\n**Replay note:** You answered {}. This curated tour continues with '{}'; it is not the result of your answer.",
                self.replay.warm.as_deref().unwrap_or_default(),
                design.warmup_answer
            );
        }
        if stage.step() >= 7
            && self.replay.picked.as_deref().is_some_and(|answer| answer != design.real_answer)
        {
            let _ = writeln!(
                text,
                "\n**Replay note:** You picked {}. The real thread chose {}; the following messages are that recorded path.",
                self.replay.picked.as_deref().unwrap_or_default(),
                design.real_answer
            );
        }
        if stage == Stage::Words {
            let _ = write!(
                text,
                "\nSuggested words to select:\n\n> {}\n\nEnter opens exact-text selection; edit it to select any words in this message, then save your comment.",
                self.fixture.question_message(false)?.suggest.phrase
            );
        }
        if stage == Stage::FileOpen {
            text.push_str("\n## Attached files\n\nprototype.html · **EVALUATION.md** · d1-clean.png\n\nEnter opens EVALUATION.md.");
        }
        if stage == Stage::Artifacts {
            text.push_str("\n## Artifacts — all thread files, newest first\n");
            for (index, asset) in self.artifacts().iter().enumerate() {
                let _ = writeln!(
                    text,
                    "\n{} {} ({})",
                    self.mark(index),
                    asset.name,
                    if asset.kind.is_empty() { "image" } else { &asset.kind }
                );
            }
        }
        if matches!(stage, Stage::Send | Stage::FilmSend | Stage::Artifacts) {
            let _ = writeln!(
                text,
                "\n## Reply draft\n\nAnswer: {}",
                self.replay.picked.as_deref().unwrap_or("none")
            );
            for comment in &self.replay.comments {
                let _ = write!(
                    text,
                    "\n- {} **{}** — {}",
                    if comment.sent { "[sent in replay]" } else { "[draft]" },
                    comment.quote,
                    comment.body
                );
                if let Some(at) = comment.at {
                    let _ = write!(text, " [frame at {at:.1}s]");
                }
                let _ =
                    writeln!(text, "{}", if comment.circle.is_some() { " [circle attached]" } else { "" });
            }
        }
        if self.replay.started.is_some() {
            text.push_str(if self.replay.delivered {
                "\n**Delivered in replay. Waiting for recorded response.**\n"
            } else {
                "\n**Sending in replay only…**\n"
            });
        }
        Ok(())
    }

    fn mark(&self, index: usize) -> &'static str {
        if index == self.cursor { ">" } else { " " }
    }

    pub(super) fn draw(&mut self, frame: &mut Frame<'_>) {
        let areas = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(2),
            Constraint::Length(4),
            Constraint::Length(3),
        ])
        .split(frame.area());
        let [header, body, coach, status] = areas.as_ref() else { return };
        frame.render_widget(
            Paragraph::new(format!(
                "Guided Inbox tour · step {}/11 · fixture only · skipped {:?}",
                self.replay.stage.step(),
                self.replay.skipped
            ))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" q/Esc: exit · f: explicitly skip interaction · b: back "),
            ),
            *header,
        );
        if let Some((name, pixels)) =
            self.viewer.as_ref().and_then(|name| self.assets.pictures.get(name).map(|pixels| (name, pixels)))
        {
            let block = Block::default()
                .borders(Borders::ALL)
                .title(format!(" {name} · {:.1}s · Esc closes ", self.replay.moment));
            let inner = block.inner(*body);
            frame.render_widget(block, *body);
            pixels.draw(
                frame,
                inner,
                self.replay.moment,
                matches!(self.replay.stage, Stage::ImageDraw | Stage::ImageComment)
                    .then_some(self.replay.circle),
            );
        } else {
            let block = Block::default().borders(Borders::ALL).title(" PgUp/PgDn: read · arrows: choose ");
            let inner = block.inner(*body);
            frame.render_widget(block, *body);
            if self.width != inner.width {
                self.rows = self
                    .body
                    .iter()
                    .flat_map(|line| crate::wrap::wrap_line(line, &[], usize::from(inner.width).max(1)))
                    .collect();
                self.width = inner.width;
            }
            self.scroll = self.scroll.min(
                u16::try_from(self.rows.len().saturating_sub(usize::from(inner.height))).unwrap_or(u16::MAX),
            );
            for (index, row) in
                self.rows.iter().skip(usize::from(self.scroll)).take(usize::from(inner.height)).enumerate()
            {
                frame.render_widget(&row.line, Rect { y: inner.y + index as u16, height: 1, ..inner });
            }
        }
        frame.render_widget(
            Paragraph::new(self.coach())
                .wrap(Wrap { trim: false })
                .style(Style::default().fg(Color::Cyan))
                .block(Block::default().borders(Borders::ALL).title(" Your turn ")),
            *coach,
        );
        frame.render_widget(
            Paragraph::new(self.status.as_str())
                .wrap(Wrap { trim: false })
                .style(Style::default().fg(Color::Yellow)),
            *status,
        );
        if let Some(editor) = &self.editor {
            self.draw_editor(frame, *body, editor);
        }
        if self.exit {
            frame.render_widget(Clear, *body);
            frame.render_widget(Paragraph::new("Leave the tour?\n\ny: save tour status and leave (finished at step 10+, skipped earlier)\nx: leave without saving tour status\nn/Esc: keep touring\n\nAppearance picks already saved remain saved. To restore them, return to step 10 and choose s. All fixture drafts are discarded on exit.").wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).title(" Save decision explicitly ")), *body);
        }
    }

    fn coach(&self) -> &'static str {
        match self.replay.stage {
            Stage::Entry => {
                "Enter: start and save seen · a: connect and save skipped · Esc: leave without changes"
            }
            Stage::Orient | Stage::Open => "Enter: continue / open the fixture thread",
            Stage::WarmPick | Stage::Picture => {
                "Arrows choose · Enter picks · o other answer · x skip question · v view a picture"
            }
            Stage::WarmSend | Stage::Send | Stage::FilmSend | Stage::NewSend => {
                "Enter: Send the shown draft in this replay only. Nothing is sent to any live session."
            }
            Stage::Words => {
                "Enter: select exact words, then comment. The suggested real comment is editable."
            }
            Stage::FileOpen => "Enter: open the actual EVALUATION.md tour asset, beside the conversation.",
            Stage::FileComment => {
                "PgUp/PgDn: read · Enter: select exact text from this file, then add your comment."
            }
            Stage::ImageOpen => "Enter: open the first built screenshot and mark it up.",
            Stage::ImageDraw => {
                "Arrows move a real circle overlay · +/- resize · Enter saves the drawing with an editable comment."
            }
            Stage::ImageComment => "Enter: write a comment to save with your circle.",
            Stage::Artifacts => {
                "Arrows choose any attached file · Enter opens it. Your image comment remains a draft."
            }
            Stage::ArtifactOpen => {
                "Read the actual artifact · HTML is inert source only (interactive preview unavailable) · Enter continues."
            }
            Stage::FilmOpen => "Enter: open the film thread's real clip.",
            Stage::FilmPlay => {
                "Space plays/pauses · Left/right seek by 0.2s · Enter pins this frame/time and opens its comment."
            }
            Stage::FilmComment => {
                "Enter: comment on the pinned frame. The timestamp and actual decoded frame stay with it."
            }
            Stage::FilmReplay if self.replay.arrived > 0 => {
                "It knew exactly where. The recorded reply is above. Enter: start a new thread."
            }
            Stage::NewOpen => "Enter: New message",
            Stage::NewRecipient => {
                "Enter: choose Claude Code in the tour project. To starts empty, never a real recipient."
            }
            Stage::NewCompose => {
                "Enter: edit the developer's request. Saving prepares a draft; Send is a separate action."
            }
            Stage::Loop | Stage::Also => "Enter: continue",
            Stage::Appearance => {
                "Up/down chooses setting · Left/right SAVES REAL SETTINGS · s restores tour changes · Enter keeps them"
            }
            Stage::Connect => {
                "Enter saves finished · a saves finished and opens real connection settings · c copies first-question prompt"
            }
            _ => "Recorded replay running · f explicitly skips it. The real Inbox is untouched.",
        }
    }

    fn draw_editor(&self, frame: &mut Frame<'_>, area: Rect, editor: &Editor) {
        let rect = Rect { y: area.y + area.height / 2, height: area.height - area.height / 2, ..area };
        frame.render_widget(Clear, rect);
        let title = match editor {
            Editor::Quote { .. } => " Select exact text ",
            Editor::Comment { .. } => " Comment + captured target ",
            Editor::New => " New message draft ",
            Editor::Other { .. } => " Other answer ",
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .title_bottom(" Enter saves · Esc cancels · Alt+Enter newline ");
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        let (lines, row, column) = self.compose.wrapped(usize::from(inner.width).max(1));
        let scroll = row.saturating_sub(usize::from(inner.height).saturating_sub(1));
        frame.render_widget(
            Paragraph::new(lines.into_iter().skip(scroll).map(Line::from).collect::<Vec<_>>())
                .style(Style::default().add_modifier(Modifier::BOLD)),
            inner,
        );
        if inner.width > 0 && inner.height > 0 {
            frame.set_cursor_position((
                inner.x + (column as u16).min(inner.width - 1),
                inner.y + ((row - scroll) as u16).min(inner.height - 1),
            ));
        }
    }
}
