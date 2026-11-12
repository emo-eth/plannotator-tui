//! Additive wire models for the upstream Inbox Board. Checklist rows are agent-owned,
//! deliberately read-only; cancelled cards and deleted initiatives remain browseable.
use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Column {
    #[default]
    Backlog,
    Todo,
    InProgress,
    InReview,
    Done,
    Dropped,
}

impl Column {
    pub(crate) const ALL: [Self; 6] =
        [Self::Backlog, Self::Todo, Self::InProgress, Self::InReview, Self::Done, Self::Dropped];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Backlog => "Backlog",
            Self::Todo => "Todo",
            Self::InProgress => "In progress",
            Self::InReview => "In review",
            Self::Done => "Done",
            Self::Dropped => "Cancelled",
        }
    }

    pub(crate) fn wire(self) -> &'static str {
        match self {
            Self::Backlog => "backlog",
            Self::Todo => "todo",
            Self::InProgress => "in_progress",
            Self::InReview => "in_review",
            Self::Done => "done",
            Self::Dropped => "dropped",
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Author {
    pub(crate) kind: String,
    pub(crate) host: Option<String>,
    pub(crate) session: Option<String>,
    pub(crate) name: Option<String>,
}

impl Author {
    pub(crate) fn label(&self) -> &str {
        if self.kind == "person" {
            "You"
        } else {
            self.name.as_deref().or(self.host.as_deref()).unwrap_or("Agent")
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Card {
    pub(crate) id: String,
    pub(crate) project_id: String,
    pub(crate) title: String,
    pub(crate) column: Column,
    pub(crate) you: Option<String>,
    pub(crate) said: Option<String>,
    pub(crate) notes: Option<String>,
    pub(crate) threads: Vec<String>,
    pub(crate) refs: Option<Vec<Reference>>,
    pub(crate) prs: Vec<Reference>,
    pub(crate) agent: Option<Author>,
    pub(crate) moved_at: String,
    pub(crate) heard_at: String,
    pub(crate) heard_by: Author,
    pub(crate) heard_seq: u64,
    pub(crate) dropped: Option<Drop>,
    pub(crate) sent: Option<Sent>,
    pub(crate) initiative: Option<String>,
}

impl Card {
    pub(crate) fn unread(&self, seen: Option<u64>) -> bool {
        self.heard_by.kind == "agent" && seen.is_none_or(|seen| self.heard_seq > seen)
    }

    pub(crate) fn references(&self) -> &[Reference] {
        self.refs.as_deref().unwrap_or(&self.prs)
    }

    pub(crate) fn sent_waiting(&self) -> Option<&Sent> {
        self.sent.as_ref().filter(|sent| self.column == Column::Todo && self.moved_at <= sent.at)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Drop {
    pub(crate) at: String,
    pub(crate) by: Author,
    pub(crate) reason: Option<String>,
    pub(crate) seen_at: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Sent {
    pub(crate) at: String,
    pub(crate) to: Author,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Reference {
    #[serde(rename = "ref")]
    pub(crate) label: String,
    pub(crate) url: String,
    pub(crate) kind: String,
    pub(crate) source: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Initiative {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) deleted_at: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Steps {
    pub(crate) agent: Author,
    #[serde(rename = "steps")]
    pub(crate) items: Vec<Step>,
    pub(crate) presence: String,
    pub(crate) updated_at: String,
}

impl Steps {
    pub(crate) fn progress(&self) -> (usize, usize) {
        let counted = self.items.iter().filter(|step| step.status != "dropped");
        counted.fold((0, 0), |(done, total), step| (done + usize::from(step.status == "done"), total + 1))
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Step {
    #[serde(rename = "step")]
    pub(crate) title: String,
    pub(crate) status: String,
    pub(crate) note: Option<String>,
    pub(crate) at: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct InitiativeThreads {
    pub(crate) threads: Vec<String>,
    pub(crate) total: usize,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct CardsModel {
    #[serde(rename = "serverSession")]
    pub(crate) server_session: String,
    pub(crate) cursor: u64,
    pub(crate) cards: Vec<Card>,
    pub(crate) dropped_unseen: usize,
    pub(crate) seen_seq: Option<u64>,
    pub(crate) initiatives: Vec<Initiative>,
    pub(crate) threads: BTreeMap<String, Value>,
    pub(crate) steps: BTreeMap<String, Vec<Steps>>,
    pub(crate) initiative_threads: BTreeMap<String, InitiativeThreads>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct CardDetail {
    pub(crate) card: Card,
    #[serde(default)]
    pub(crate) history: Vec<Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct ResolvedRef {
    #[serde(rename = "ref")]
    pub(crate) reference: Reference,
    pub(crate) result: Value,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Project {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) root: String,
}

#[derive(Debug, Clone)]
pub(crate) struct Target {
    pub(crate) host: String,
    pub(crate) session: String,
    pub(crate) label: String,
    pub(crate) project: String,
}

/// An absent/deleted initiative reads as no initiative, even after a stale list refresh.
pub(crate) fn initiative_of<'a>(card: &'a Card, initiatives: &[Initiative]) -> Option<&'a str> {
    card.initiative
        .as_deref()
        .filter(|id| initiatives.iter().any(|item| item.id == *id && item.deleted_at.is_none()))
}

/// Match NewCardDialog.splitRefs: commas in URLs are kept, separators elsewhere split.
pub(crate) fn split_refs(text: &str) -> Vec<String> {
    let mut refs = Vec::new();
    for word in text.split_whitespace() {
        let mut remaining = word;
        loop {
            let separator = remaining.match_indices(',').find_map(|(index, _)| {
                let after = remaining.get(index + 1..)?;
                (is_http(after)).then_some(index)
            });
            let (part, next) = separator.map_or((remaining, ""), |index| {
                (remaining.get(..index).unwrap_or(""), remaining.get(index + 1..).unwrap_or(""))
            });
            if is_http(part) {
                let part = part.trim_end_matches(',');
                if !part.is_empty() {
                    refs.push(part.to_owned());
                }
            } else {
                refs.extend(part.split(',').filter(|part| !part.is_empty()).map(str::to_owned));
            }
            if next.is_empty() {
                break;
            }
            remaining = next;
        }
    }
    refs
}

fn is_http(text: &str) -> bool {
    text.get(..7).is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"))
        || text.get(..8).is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
}
