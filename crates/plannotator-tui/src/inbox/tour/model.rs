use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};

use super::fixture::{DESIGN, FILM, Fixture, NEW};
use super::media::Circle;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Stage {
    Entry,
    Orient,
    Open,
    WarmPick,
    WarmSend,
    WarmReplay,
    Picture,
    Words,
    FileOpen,
    FileComment,
    Send,
    Replay,
    ImageOpen,
    ImageDraw,
    ImageComment,
    Artifacts,
    ArtifactOpen,
    FilmOpen,
    FilmPlay,
    FilmComment,
    FilmSend,
    FilmReplay,
    NewOpen,
    NewRecipient,
    NewCompose,
    NewSend,
    NewReplay,
    Loop,
    Also,
    Appearance,
    Connect,
}

impl Stage {
    pub(super) fn step(self) -> u8 {
        match self {
            Self::Entry | Self::Orient | Self::Open => 1,
            Self::WarmPick | Self::WarmSend | Self::WarmReplay => 2,
            Self::Picture => 3,
            Self::Words => 4,
            Self::FileOpen | Self::FileComment => 5,
            Self::Send => 6,
            Self::Replay
            | Self::ImageOpen
            | Self::ImageDraw
            | Self::ImageComment
            | Self::Artifacts
            | Self::ArtifactOpen => 7,
            Self::FilmOpen | Self::FilmPlay | Self::FilmComment | Self::FilmSend | Self::FilmReplay => 8,
            Self::NewOpen
            | Self::NewRecipient
            | Self::NewCompose
            | Self::NewSend
            | Self::NewReplay
            | Self::Loop
            | Self::Also => 9,
            Self::Appearance => 10,
            Self::Connect => 11,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Target {
    Message,
    File,
    Image,
    Film,
}

#[derive(Debug)]
pub(super) struct Comment {
    pub target: Target,
    pub quote: String,
    pub body: String,
    pub circle: Option<Circle>,
    pub at: Option<f64>,
    pub sent: bool,
}

pub(super) struct Replay {
    pub stage: Stage,
    pub warm: Option<String>,
    pub picked: Option<String>,
    pub comments: Vec<Comment>,
    pub new_body: String,
    pub recipient: bool,
    pub skipped: BTreeSet<u8>,
    pub delivered: bool,
    pub arrived: u8,
    pub started: Option<Instant>,
    pub moment: f64,
    pub circle: Circle,
    pub drawing: bool,
}

impl Default for Replay {
    fn default() -> Self {
        Self {
            stage: Stage::Entry,
            warm: None,
            picked: None,
            comments: Vec::new(),
            new_body: String::new(),
            recipient: false,
            skipped: BTreeSet::new(),
            delivered: false,
            arrived: 0,
            started: None,
            moment: 0.0,
            circle: Circle::default(),
            drawing: false,
        }
    }
}

impl Replay {
    pub(super) fn go(&mut self, stage: Stage) {
        self.stage = stage;
        self.started = None;
    }

    pub(super) fn send(&mut self) -> Result<()> {
        let next = match self.stage {
            Stage::WarmSend => {
                ensure!(self.warm.is_some(), "Pick an answer before sending");
                Stage::WarmReplay
            }
            Stage::Send => {
                ensure!(self.picked.is_some(), "Pick a direction before sending");
                for comment in &mut self.comments {
                    if matches!(comment.target, Target::Message | Target::File) {
                        comment.sent = true;
                    }
                }
                Stage::Replay
            }
            Stage::FilmSend => {
                ensure!(
                    self.comments.iter().any(|comment| comment.target == Target::Film),
                    "Comment on a film moment before sending"
                );
                for comment in &mut self.comments {
                    if comment.target == Target::Film {
                        comment.sent = true;
                    }
                }
                Stage::FilmReplay
            }
            Stage::NewSend => {
                ensure!(
                    self.recipient && !self.new_body.trim().is_empty(),
                    "Choose a recipient and write a message before sending"
                );
                Stage::NewReplay
            }
            _ => anyhow::bail!("Send is not available at this tour step"),
        };
        self.go(next);
        self.delivered = false;
        self.arrived = 0;
        self.started = Some(Instant::now());
        Ok(())
    }

    pub(super) fn tick(&mut self) -> bool {
        let Some(start) = self.started else { return false };
        self.advance_elapsed(start.elapsed())
    }

    fn advance_elapsed(&mut self, elapsed: Duration) -> bool {
        let before = (self.stage, self.delivered, self.arrived);
        self.delivered = elapsed >= Duration::from_millis(1100);
        match self.stage {
            Stage::WarmReplay if elapsed >= Duration::from_millis(2600) => self.go(Stage::Picture),
            Stage::Replay => {
                self.arrived = if elapsed >= Duration::from_millis(5600) {
                    2
                } else {
                    u8::from(elapsed >= Duration::from_millis(3000))
                };
                if self.arrived == 2 {
                    self.go(Stage::ImageOpen);
                }
            }
            Stage::FilmReplay if elapsed >= Duration::from_millis(2400) => {
                self.arrived = 1;
                self.started = None;
            }
            Stage::NewReplay if elapsed >= Duration::from_millis(3200) => {
                self.arrived = 1;
                self.go(Stage::Loop);
            }
            _ => {}
        }
        before != (self.stage, self.delivered, self.arrived)
    }

    pub(super) fn skip(&mut self, fixture: &Fixture) -> Result<()> {
        let step = self.stage.step();
        self.skipped.insert(step);
        let next = match step {
            1 => Stage::WarmPick,
            2 => Stage::Picture,
            3 => {
                self.picked = Some(fixture.thread(DESIGN)?.real_answer.clone());
                Stage::Words
            }
            4 => Stage::FileOpen,
            5 => Stage::Send,
            6 => Stage::ImageOpen,
            7 => Stage::FilmOpen,
            8 => Stage::NewOpen,
            9 => Stage::Appearance,
            _ => anyhow::bail!("Use Skip appearance to restore its settings, or Finish to leave"),
        };
        self.go(next);
        Ok(())
    }

    pub(super) fn back(&mut self) -> Result<()> {
        let next = match self.stage {
            Stage::WarmPick | Stage::WarmSend => Stage::Open,
            Stage::Words => Stage::Picture,
            Stage::FileOpen | Stage::FileComment => Stage::Words,
            Stage::FilmOpen | Stage::FilmPlay | Stage::FilmComment | Stage::FilmSend => Stage::Artifacts,
            _ => anyhow::bail!("Back is unavailable after this replay's Send; restart from the tour menu"),
        };
        self.go(next);
        Ok(())
    }

    pub(super) fn save_comment(&mut self, target: Target, quote: String, body: String) -> Result<()> {
        ensure!(!body.trim().is_empty(), "Write a comment before saving");
        self.comments.retain(|comment| comment.target != target || comment.sent);
        self.comments.push(Comment {
            target,
            quote,
            body,
            circle: (target == Target::Image).then_some(self.circle),
            at: (target == Target::Film).then_some(self.moment),
            sent: false,
        });
        self.go(match target {
            Target::Message => Stage::FileOpen,
            Target::File => Stage::Send,
            Target::Image => Stage::Artifacts,
            Target::Film => Stage::FilmSend,
        });
        Ok(())
    }

    pub(super) fn replay_text(fixture: &Fixture, thread: &str, count: u8) -> Result<String> {
        let mut text = String::new();
        for message in &fixture.thread(thread)?.messages {
            if message.replay != 0 && message.replay <= count {
                let _ = writeln!(
                    text,
                    "\n### Claude Code · {} · Recorded replay\n\n{}",
                    message.time, message.body
                );
            }
        }
        Ok(text)
    }

    pub(super) fn compose_suggestion(fixture: &Fixture) -> Result<&str> {
        fixture
            .thread(NEW)?
            .compose
            .as_ref()
            .map(|compose| compose.body.as_str())
            .context("Tour fixture omitted compose suggestion")
    }

    pub(super) fn film_reply(fixture: &Fixture) -> Result<String> {
        Self::replay_text(fixture, FILM, 1)
    }
}
