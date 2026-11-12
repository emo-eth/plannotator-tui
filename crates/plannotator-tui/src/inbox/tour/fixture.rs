use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use serde::Deserialize;

use super::super::client::Client;

pub(super) const DESIGN: &str = "tour-vscode";
pub(super) const FILM: &str = "tour-film";
pub(super) const NEW: &str = "tour-new";
pub(super) const FIRST_QUESTION: &str = "Send me one real question in my Plannotator Inbox about what we’re working on now. If the choice is visual, show the options as pictures: screenshots you already have, or a small HTML mock you attach. Then end your turn, and act on my answer when it comes back.";

#[derive(Debug, Deserialize)]
pub(super) struct Fixture {
    pub v: u32,
    pub date: String,
    pub project: String,
    pub developer: String,
    #[serde(default)]
    pub full: BTreeMap<String, String>,
    pub threads: Vec<Thread>,
}

#[derive(Debug, Deserialize)]
pub(super) struct Thread {
    pub id: String,
    pub subject: String,
    #[serde(default)]
    pub slice: String,
    #[serde(default)]
    pub real_answer: String,
    #[serde(default)]
    pub warmup_answer: String,
    pub compose: Option<ComposeMessage>,
    pub messages: Vec<Message>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ComposeMessage {
    pub body: String,
    pub time: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct Message {
    pub id: String,
    pub who: String,
    pub time: String,
    pub body: String,
    #[serde(default)]
    pub warmup: bool,
    #[serde(default)]
    pub asks: bool,
    #[serde(default)]
    pub replay: u8,
    #[serde(default)]
    pub attachments: Vec<Asset>,
    #[serde(default)]
    pub inline_images: Vec<Asset>,
    #[serde(default)]
    pub suggest: Suggestion,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct Asset {
    pub name: String,
    #[serde(default, rename = "asset")]
    pub file: String,
    #[serde(default, rename = "ref")]
    pub reference: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub suggest: Suggestion,
}

impl Asset {
    pub(super) fn key(&self) -> &str {
        if self.file.is_empty() { &self.reference } else { &self.file }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct Suggestion {
    #[serde(default)]
    pub phrase: String,
    #[serde(default)]
    pub comment: String,
    #[serde(default, rename = "at")]
    _at: f64,
}

#[derive(Debug)]
pub(super) struct Question {
    pub prompt: String,
    pub choices: Vec<Choice>,
}

#[derive(Debug, Default)]
pub(super) struct Choice {
    pub label: String,
    pub description: String,
    pub picture: Option<String>,
}

impl Fixture {
    pub(super) fn load(client: &Client) -> Result<Self> {
        let bytes = super::media::read_route(client, "/api/inbox/tour/fixture.json", 512 * 1024)?;
        let fixture: Self = serde_json::from_slice(&bytes).context("Reading Inbox tour fixture")?;
        fixture.validate()?;
        Ok(fixture)
    }

    pub(super) fn validate(&self) -> Result<()> {
        ensure!(self.v == 2, "Inbox tour fixture version {} is unsupported", self.v);
        ensure!(
            !self.project.is_empty() && !self.developer.is_empty(),
            "Tour fixture has no project or developer"
        );
        let mut ids = BTreeSet::new();
        for thread in &self.threads {
            ensure!(ids.insert(&thread.id), "Duplicate tour thread {}", thread.id);
            for message in &thread.messages {
                ensure!(
                    matches!(message.who.as_str(), "developer" | "agent") && valid_time(&message.time),
                    "Invalid author or time in {}",
                    message.id
                );
                for asset in message.attachments.iter().chain(&message.inline_images) {
                    ensure!(valid_asset(asset.key()), "Not a tour asset: {}", asset.key());
                }
            }
        }
        let design = self.thread(DESIGN)?;
        ensure!(
            !design.warmup_answer.is_empty() && !design.real_answer.is_empty(),
            "Tour fixture omitted replay answers"
        );
        self.question(true)?;
        self.question(false)?;
        ensure!(
            !self.question_message(false)?.suggest.phrase.is_empty(),
            "Tour fixture omitted message annotation target"
        );
        ensure!(
            !self.asset("evaluation.md")?.suggest.phrase.is_empty(),
            "Tour fixture omitted file annotation target"
        );
        self.asset("built-a1.webp")?;
        ensure!(self.asset("film-clip.mp4")?.duration_ms > 0, "Tour film has no duration");
        self.thread(FILM)?;
        let compose = self.thread(NEW)?.compose.as_ref().context("Tour fixture omitted new-message text")?;
        ensure!(valid_time(&compose.time) && !compose.body.is_empty(), "Invalid new-message fixture");
        for (small, full) in &self.full {
            ensure!(valid_asset(small) && valid_asset(full), "Invalid tour picture mapping");
        }
        Ok(())
    }

    pub(super) fn thread(&self, id: &str) -> Result<&Thread> {
        self.threads
            .iter()
            .find(|thread| thread.id == id)
            .with_context(|| format!("Tour fixture omitted {id}"))
    }

    pub(super) fn question_message(&self, warm: bool) -> Result<&Message> {
        self.thread(DESIGN)?
            .messages
            .iter()
            .find(|message| if warm { message.warmup } else { message.asks })
            .context("Tour fixture omitted a question message")
    }

    pub(super) fn question(&self, warm: bool) -> Result<Question> {
        parse_question(&self.question_message(warm)?.body)
    }

    pub(super) fn asset(&self, key: &str) -> Result<&Asset> {
        self.threads
            .iter()
            .flat_map(|thread| &thread.messages)
            .flat_map(|message| message.attachments.iter().chain(&message.inline_images))
            .find(|asset| asset.key() == key)
            .with_context(|| format!("Tour fixture omitted {key}"))
    }

    pub(super) fn picture(&self, name: &str) -> String {
        self.full.get(name).map_or_else(|| name.to_owned(), Clone::clone)
    }
}

pub(super) fn valid_asset(name: &str) -> bool {
    !name.is_empty()
        && name.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
        && !name.contains("..")
        && [".webp", ".html", ".mp4", ".md"].iter().any(|extension| name.ends_with(extension))
}

pub(super) fn asset_path(name: &str) -> Result<String> {
    ensure!(valid_asset(name), "Not a tour asset: {name}");
    Ok(format!("/api/inbox/tour/assets/{name}"))
}

fn valid_time(time: &str) -> bool {
    let Some((hour, minute)) = time.split_once(':') else { return false };
    hour.len() == 2
        && minute.len() == 2
        && hour.parse::<u8>().is_ok_and(|n| n < 24)
        && minute.parse::<u8>().is_ok_and(|n| n < 60)
}

fn parse_question(body: &str) -> Result<Question> {
    let (_, rest) = body.split_once(":::question\n").context("Tour question has no question block")?;
    let (source, _) = rest.split_once(":::").context("Tour question block is not closed")?;
    let mut prompt = String::new();
    let mut prompt_done = false;
    let mut choices = Vec::new();
    let mut choice: Option<Choice> = None;
    let mut in_image = false;
    for event in Parser::new_ext(source, Options::ENABLE_TASKLISTS) {
        match event {
            Event::End(TagEnd::Paragraph) if choice.is_none() && !prompt.is_empty() => prompt_done = true,
            Event::Start(Tag::Item) => choice = Some(Choice::default()),
            Event::End(TagEnd::Item) => {
                if let Some(mut item) = choice.take() {
                    if let Some((label, detail)) = item.label.split_once(" — ") {
                        detail.trim().clone_into(&mut item.description);
                        item.label.truncate(label.trim_end().len());
                        let start = item.label.len() - item.label.trim_start().len();
                        item.label.replace_range(..start, "");
                    }
                    choices.push(item);
                }
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                if let Some(item) = &mut choice {
                    ensure!(valid_asset(&dest_url), "Question image is not a tour asset");
                    item.picture = Some(dest_url.into_string());
                }
                in_image = true;
            }
            Event::End(TagEnd::Image) => in_image = false,
            Event::Text(text) | Event::Code(text) if !in_image => {
                if let Some(item) = &mut choice {
                    item.label.push_str(&text);
                } else if !prompt_done {
                    prompt.push_str(&text);
                }
            }
            _ => {}
        }
    }
    ensure!(!prompt.is_empty() && !choices.is_empty(), "Tour question has no prompt or choices");
    Ok(Question { prompt, choices })
}
