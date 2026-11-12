use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use unicode_segmentation::UnicodeSegmentation;

const PROMPT_MAX_CHARS: usize = 400;
const SELECTED_MAX_CHARS: usize = 20_000;
const SELECTED_MAX: usize = 20;
const OTHER_MAX_CHARS: usize = 2_000;
const TEXT_MAX_CHARS: usize = 4_000;
const NOTE_MAX_CHARS: usize = 2_000;

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Kind {
    Single,
    Multi,
    Text,
    Action,
    #[default]
    #[serde(other)]
    Unknown,
}

impl Kind {
    fn as_wire(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::Multi => "multi",
            Self::Text => "text",
            Self::Action => "action",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum State {
    Open,
    Picked,
    Sent,
    Closed,
    Superseded,
    #[default]
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Question {
    #[serde(default)]
    pub(crate) key: String,
    #[serde(default)]
    pub(crate) position: u64,
    #[serde(default)]
    pub(crate) kind: Kind,
    #[serde(default)]
    pub(crate) prompt: String,
    #[serde(default)]
    pub(crate) context: Option<String>,
    #[serde(default)]
    pub(crate) choices: Vec<Choice>,
    #[serde(default)]
    pub(crate) recommendation: Option<String>,
    #[serde(default)]
    pub(crate) suggested_text: Option<String>,
    #[serde(default)]
    pub(crate) stopped: Option<String>,
    #[serde(default)]
    pub(crate) holds_up: Vec<String>,
    #[serde(default)]
    pub(crate) state: State,
    #[serde(default)]
    pub(crate) orphaned: bool,
    #[serde(default)]
    pub(crate) answer: Option<Answer>,
    #[serde(default)]
    pub(crate) revision: u64,
    #[serde(default)]
    pub(crate) sent_revision: u64,
    #[serde(default)]
    pub(crate) action: Option<super::actions::ActionFacts>,
    #[serde(default)]
    pub(crate) action_result: Option<super::actions::ActionResult>,
    #[serde(default)]
    pub(crate) stakes: Option<String>,
    #[serde(default)]
    pub(crate) shows: Option<Shows>,
    #[serde(default)]
    pub(crate) why: Option<String>,
    #[serde(default)]
    pub(crate) reversible: Option<Reversible>,
    #[serde(default)]
    pub(crate) prototype: Option<Prototype>,
    #[serde(default)]
    pub(crate) attachment: Option<Attachment>,
}

impl Question {
    pub(crate) fn is_open(&self) -> bool {
        matches!(self.state, State::Open | State::Picked)
    }

    pub(crate) fn summary(&self) -> String {
        self.answer.as_ref().map_or_else(|| "No answer".to_owned(), Answer::summary)
    }
}
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Attachment {
    #[serde(default)]
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) key: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Prototype {
    #[serde(default)]
    pub(crate) target: String,
    #[serde(default)]
    pub(crate) label: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ShowsKind {
    Image,
    Link,
    #[default]
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Shows {
    #[serde(default)]
    pub(crate) kind: ShowsKind,
    #[serde(default)]
    pub(crate) label: String,
    #[serde(default)]
    pub(crate) src: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Reversible {
    #[serde(default)]
    pub(crate) value: bool,
    #[serde(default)]
    pub(crate) why: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Image {
    #[serde(default)]
    pub(crate) alt: String,
    #[serde(default)]
    pub(crate) src: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Choice {
    #[serde(default)]
    pub(crate) label: String,
    #[serde(default)]
    pub(crate) description: Option<String>,
    #[serde(default)]
    pub(crate) recommended: bool,
    #[serde(default)]
    pub(crate) settled: bool,
    #[serde(default)]
    pub(crate) pros: Vec<String>,
    #[serde(default)]
    pub(crate) cons: Vec<String>,
    #[serde(default)]
    pub(crate) prototype: Option<Prototype>,
    #[serde(default)]
    pub(crate) image: Option<Image>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub(crate) struct Answer {
    #[serde(default)]
    pub(crate) selected: Vec<String>,
    #[serde(default)]
    pub(crate) other: Option<String>,
    #[serde(default)]
    pub(crate) text: Option<String>,
    #[serde(default)]
    pub(crate) note: Option<String>,
    #[serde(default)]
    pub(crate) skipped: bool,
}

impl Answer {
    pub(crate) fn is_empty(&self) -> bool {
        !self.skipped
            && self.selected.is_empty()
            && !has_words(self.other.as_deref())
            && !has_words(self.text.as_deref())
            && !has_words(self.note.as_deref())
    }

    pub(crate) fn serialize(&self, question: &Question) -> Value {
        let mut selected = Vec::with_capacity(self.selected.len().min(SELECTED_MAX));
        let mut seen = BTreeSet::new();
        if question.kind != Kind::Text && !self.skipped {
            for label in &self.selected {
                let label = take_chars(label.trim(), SELECTED_MAX_CHARS);
                if !label.is_empty() && seen.insert(label.clone()) {
                    selected.push(label);
                    if selected.len() == SELECTED_MAX {
                        break;
                    }
                }
            }
        }

        let mut object = Map::new();
        object.insert("v".to_owned(), json!(1));
        object.insert("key".to_owned(), json!(question.key));
        object.insert("kind".to_owned(), json!(question.kind.as_wire()));
        object.insert("prompt".to_owned(), json!(take_utf16_graphemes(&question.prompt, PROMPT_MAX_CHARS)));
        object.insert("selected".to_owned(), json!(selected));
        if self.skipped {
            object.insert("skipped".to_owned(), json!(true));
        } else if question.kind == Kind::Text {
            insert_optional(&mut object, "text", self.text.as_deref(), TEXT_MAX_CHARS);
        } else {
            insert_optional(&mut object, "other", self.other.as_deref(), OTHER_MAX_CHARS);
        }
        insert_optional(&mut object, "note", self.note.as_deref(), NOTE_MAX_CHARS);
        Value::Object(object)
    }

    fn summary(&self) -> String {
        let answer = if self.skipped {
            "Skipped".to_owned()
        } else if let Some(text) = nonempty(self.text.as_deref()) {
            format!("“{}”", text.trim())
        } else {
            let mut parts = Vec::with_capacity(self.selected.len() + usize::from(self.other.is_some()));
            parts.extend(
                self.selected.iter().filter(|label| !label.trim().is_empty()).map(|label| plain_label(label)),
            );
            if let Some(other) = nonempty(self.other.as_deref()) {
                parts.push(format!("Other: {}", other.trim()));
            }
            if parts.is_empty() {
                if nonempty(self.note.as_deref()).is_some() {
                    "A note".to_owned()
                } else {
                    "No answer".to_owned()
                }
            } else if self.selected.is_empty() {
                parts.join(", ")
            } else {
                format!("Answer: {}", parts.join(", "))
            }
        };
        match nonempty(self.note.as_deref()) {
            Some(note) if answer != "No answer" => format!("{answer} — note: {}", note.trim()),
            _ => answer,
        }
    }
}

fn has_words(value: Option<&str>) -> bool {
    value.is_some_and(|value| !value.trim().is_empty())
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

fn insert_optional(object: &mut Map<String, Value>, name: &str, value: Option<&str>, max: usize) {
    if let Some(value) = nonempty(value) {
        object.insert(name.to_owned(), json!(take_chars(value, max)));
    }
}

fn take_chars(value: &str, max_utf16_units: usize) -> String {
    let mut units = 0;
    let mut end = 0;
    for (index, ch) in value.char_indices() {
        let next = units + ch.len_utf16();
        if next > max_utf16_units {
            break;
        }
        units = next;
        end = index + ch.len_utf8();
    }
    value[..end].to_owned()
}

fn take_utf16_graphemes(value: &str, max_utf16_units: usize) -> String {
    let mut units = 0;
    let mut end = 0;
    for (index, grapheme) in value.grapheme_indices(true) {
        let next = units + grapheme.encode_utf16().count();
        if next > max_utf16_units {
            break;
        }
        units = next;
        end = index + grapheme.len();
    }
    value[..end].to_owned()
}

fn plain_label(label: &str) -> String {
    let mut plain = String::new();
    for event in pulldown_cmark::Parser::new(label) {
        match event {
            pulldown_cmark::Event::Text(text) | pulldown_cmark::Event::Code(text) => plain.push_str(&text),
            _ => {}
        }
    }
    plain
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing, reason = "tests assert by panicking")]
mod tests {
    use super::{Answer, Kind, Question, ShowsKind, State};
    use serde_json::json;

    fn question(kind: Kind) -> Question {
        Question {
            key: "q-choice".to_owned(),
            kind,
            prompt: "Choose a deployment strategy?".to_owned(),
            state: State::Open,
            ..Question::default()
        }
    }

    #[test]
    fn recorded_attachment_question_keeps_source_and_answer_identity() {
        let question: Question = serde_json::from_str(include_str!("fixtures/attachment-question.json"))
            .expect("recorded attachment question");
        let attachment = question.attachment.as_ref().expect("attachment source");
        assert_eq!(attachment.name, "attachment.md");
        assert_eq!(attachment.key, "q-c06ee0f2");
        assert_eq!(question.key, "q-c06ee0f2");
        assert_eq!(question.kind, Kind::Single);
        assert_eq!(question.choices[1].label, "Choices and selected state");
    }

    #[test]
    fn deserializes_upstream_choices_and_unknown_additive_kinds() {
        let decoded: Question = serde_json::from_value(json!({
            "key": "q-1",
            "kind": "multi",
            "prompt": "Which options?",
            "state": "open",
            "choices": [{"label":"Fast", "recommended":true, "settled":false, "pros":["quick"], "cons":["cost"]}],
            "future_field": true
        }))
        .expect("question wire");
        assert_eq!(decoded.kind, Kind::Multi);
        assert_eq!(decoded.choices[0].pros, ["quick"]);
        assert_eq!(decoded.state, State::Open);

        let future: Question = serde_json::from_value(json!({"kind":"future", "state":"future"}))
            .expect("unknown enum values are read-only");
        assert_eq!(future.kind, Kind::Unknown);
        assert_eq!(future.state, State::Unknown);
        assert!(!future.is_open());
    }

    #[test]
    fn deserializes_question_references_without_changing_answer_payloads() {
        let decoded: Question = serde_json::from_value(json!({
            "key": "q-layout-2",
            "position": 1,
            "kind": "single",
            "prompt": "Which layout?",
            "state": "picked",
            "stakes": "The checkout ships tomorrow.",
            "shows": {"kind": "image", "label": "Both layouts", "src": "screens/both.png"},
            "why": "The total stays visible.",
            "reversible": {"value": false, "why": "The previous layout is removed."},
            "prototype": {"target": "mock.html", "label": "Compare layouts"},
            "attachment": {"id": "att_plan", "name": "plan.md", "key": "q-layout"},
            "choices": [{
                "label": "Pinned total",
                "recommended": true,
                "image": {"alt": "Pinned checkout", "src": "screens/pinned.png"},
                "prototype": {"target": "http://localhost:3000/pinned#checkout", "label": "Try pinned"}
            }, {
                "label": "Floating total",
                "prototype": {"target": "floating.html"}
            }],
            "answer": {"selected": ["Pinned total"], "note": "Keep the total visible"}
        }))
        .expect("current question wire");
        assert_eq!(decoded.stakes.as_deref(), Some("The checkout ships tomorrow."));
        let shows = decoded.shows.as_ref().expect("shows");
        assert_eq!(shows.kind, ShowsKind::Image);
        assert_eq!(shows.label, "Both layouts");
        assert_eq!(shows.src, "screens/both.png");
        assert_eq!(decoded.why.as_deref(), Some("The total stays visible."));
        let reversible = decoded.reversible.as_ref().expect("one-way question");
        assert!(!reversible.value);
        assert_eq!(reversible.why.as_deref(), Some("The previous layout is removed."));
        let shared = decoded.prototype.as_ref().expect("shared prototype");
        assert_eq!(shared.target, "mock.html");
        assert_eq!(shared.label.as_deref(), Some("Compare layouts"));
        let own = decoded.choices[0].prototype.as_ref().expect("choice prototype");
        assert_eq!(own.target, "http://localhost:3000/pinned#checkout");
        assert_eq!(own.label.as_deref(), Some("Try pinned"));
        assert!(decoded.choices[1].prototype.as_ref().expect("unlabelled prototype").label.is_none());
        let image = decoded.choices[0].image.as_ref().expect("picture choice");
        assert_eq!(image.alt, "Pinned checkout");
        assert_eq!(image.src, "screens/pinned.png");
        let attachment = decoded.attachment.as_ref().expect("source attachment");
        assert_eq!(attachment.id, "att_plan");
        assert_eq!(attachment.name, "plan.md");
        assert_eq!(attachment.key, "q-layout");
        assert_ne!(attachment.key, decoded.key);
        assert_eq!(
            decoded.answer.as_ref().expect("saved answer").serialize(&decoded),
            json!({
                "v": 1,
                "key": "q-layout-2",
                "kind": "single",
                "prompt": "Which layout?",
                "selected": ["Pinned total"],
                "note": "Keep the total visible"
            })
        );
    }

    #[test]
    fn optional_question_details_accept_older_and_additive_records() {
        let older: Question =
            serde_json::from_value(json!({"choices": [{"label": "A"}]})).expect("older question wire");
        assert!(older.prototype.is_none());
        assert!(older.shows.is_none());
        assert!(older.reversible.is_none());
        assert!(older.why.is_none());
        assert!(older.attachment.is_none());
        assert!(older.action.is_none());
        assert!(older.action_result.is_none());
        assert_eq!(older.sent_revision, 0);
        assert!(older.choices[0].prototype.is_none());
        assert!(older.choices[0].image.is_none());

        for (kind, expected) in [("link", ShowsKind::Link), ("future", ShowsKind::Unknown)] {
            let decoded: Question = serde_json::from_value(json!({
                "shows": {"kind": kind, "label": "Plan", "src": "plan.md"},
                "reversible": {"value": false, "why": null},
                "prototype": null,
                "attachment": null,
                "action": null,
                "action_result": null
            }))
            .expect("optional question details");
            assert_eq!(decoded.shows.as_ref().expect("shows").kind, expected);
            assert!(decoded.reversible.as_ref().expect("one-way question").why.is_none());
            assert!(decoded.prototype.is_none());
            assert!(decoded.attachment.is_none());
            assert!(decoded.action.is_none());
            assert!(decoded.action_result.is_none());
        }
    }

    #[test]
    fn serializes_kind_specific_answers_and_caps_prompt_without_splitting_unicode() {
        let mut q = question(Kind::Single);
        q.prompt = format!("{}🧑‍🔬tail", "p".repeat(399));
        let answer = Answer {
            selected: vec!["Retry".into(), "Retry".into()],
            other: Some("  custom  ".into()),
            text: Some("not used by a choice question".into()),
            note: Some(" because it is safer ".into()),
            skipped: false,
        };
        let wire = answer.serialize(&q);
        assert_eq!(wire["prompt"].as_str().map(|prompt| prompt.encode_utf16().count()), Some(399));
        assert_eq!(wire["selected"], json!(["Retry"]));
        assert_eq!(wire["other"], "  custom  ");
        assert!(wire.get("text").is_none());
        assert_eq!(wire["note"], " because it is safer ");

        q.kind = Kind::Text;
        let text = Answer { text: Some("  two devices  ".into()), ..Answer::default() }.serialize(&q);
        assert_eq!(text["text"], "  two devices  ");
        assert!(text.get("other").is_none());
    }

    #[test]
    fn notes_keep_an_answer_nonempty_and_summary_states_remain_distinct() {
        let q = question(Kind::Single);
        assert!(Answer::default().is_empty());
        assert!(!Answer { note: Some("why".into()), ..Answer::default() }.is_empty());
        assert_eq!(
            Question {
                answer: Some(Answer { selected: vec!["Retry".into()], ..Answer::default() }),
                ..q.clone()
            }
            .summary(),
            "Answer: Retry"
        );
        assert_eq!(Answer { other: Some("Custom".into()), ..Answer::default() }.summary(), "Other: Custom");
        assert_eq!(
            Answer { text: Some("Two devices".into()), ..Answer::default() }.summary(),
            "“Two devices”"
        );
        assert_eq!(Answer { skipped: true, ..Answer::default() }.summary(), "Skipped");
    }
}
