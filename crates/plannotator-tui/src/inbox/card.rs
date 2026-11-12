//! An answer card: one question as the person answers it, drawn as boxed rows. The thread
//! view places the cards; this module only knows how one looks and which of its rows the
//! keyboard can land on.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::questions::{Answer, Kind, Question, ShowsKind, State};
use crate::theme::palette;
use crate::wrap::wrap_line;

/// Where a card sits in the thread, in thread rows.
#[derive(Debug, Clone)]
pub(crate) struct Card {
    pub(crate) message_id: String,
    pub(crate) key: String,
    pub(crate) first: usize,
    pub(crate) last: usize,
    /// The rows the choice highlight can land on: each choice, then Other; the text row of
    /// a free-text question.
    pub(crate) rows: Vec<usize>,
    pub(crate) answerable: bool,
}

/// What the card shows beyond the question itself.
pub(crate) struct Shown<'a> {
    pub(crate) number: usize,
    pub(crate) answer: &'a Answer,
    /// The answer is a draft the server does not hold yet (a pick that failed to save).
    pub(crate) drafted: bool,
    /// The highlighted selectable row, when this card is focused.
    pub(crate) focus: Option<usize>,
}

const PREFIX: &str = "│ ";
const CONTINUATION: &str = "    ";

pub(crate) fn answerable(question: &Question) -> bool {
    matches!(question.kind, Kind::Single | Kind::Multi | Kind::Text)
        && question.is_open()
        && !question.orphaned
}

fn kind_words(kind: Kind) -> &'static str {
    match kind {
        Kind::Single => "pick one",
        Kind::Multi => "pick any",
        Kind::Text => "free text",
        Kind::Action => "action item",
        Kind::Unknown => "question",
    }
}

fn state_words(state: State) -> &'static str {
    match state {
        State::Open => "open",
        State::Picked => "picked, not sent",
        State::Sent => "sent",
        State::Closed => "closed",
        State::Superseded => "replaced",
        State::Unknown => "",
    }
}

fn border_color(state: State) -> Color {
    match state {
        State::Open => palette().comment,
        State::Picked => palette().accent,
        State::Sent => palette().looks_good,
        State::Closed | State::Superseded | State::Unknown => palette().idle_fg,
    }
}

fn reference_words(prefix: &str, label: &str, target: &str) -> Option<String> {
    let label = label.trim();
    let target = target.trim();
    match (label.is_empty(), target.is_empty()) {
        (true, true) => None,
        (true, false) => Some(format!("{prefix}: {target}")),
        (false, true) => Some(format!("{prefix}: {label}")),
        (false, false) if label == target => Some(format!("{prefix}: {target}")),
        (false, false) => Some(format!("{prefix}: {label} — {target}")),
    }
}

/// `text` wrapped to `width` columns, one line per row.
fn wrapped(text: &str, style: Style, width: usize) -> Vec<Line<'static>> {
    let offsets = vec![None; text.chars().count()];
    let line = Line::from(vec![Span::styled(text.to_owned(), style)]);
    wrap_line(&line, &offsets, width.max(1)).into_iter().map(|row| row.line).collect()
}

/// A wrapped paragraph: the first row after `lead`, the rest indented under it.
fn lead_wrapped(lead: Span<'static>, text: &str, style: Style, width: usize) -> Vec<Line<'static>> {
    let lead_width = lead.width();
    let mut rows = wrapped(text, style, width.saturating_sub(lead_width).max(8)).into_iter();
    let mut out = Vec::new();
    if let Some(first) = rows.next() {
        let mut spans = vec![lead];
        spans.extend(first.spans);
        out.push(Line::from(spans));
    } else {
        out.push(Line::from(vec![lead]));
    }
    for row in rows {
        let mut spans = vec![Span::raw(" ".repeat(lead_width))];
        spans.extend(row.spans);
        out.push(Line::from(spans));
    }
    out
}

struct Builder {
    border: Style,
    width: usize,
    lines: Vec<Line<'static>>,
    rows: Vec<usize>,
}

impl Builder {
    fn push(&mut self, line: Line<'static>, highlight: bool) {
        let mut spans = vec![Span::styled(PREFIX, self.border)];
        for span in line.spans {
            spans.push(if highlight { span.patch_style(palette().selection) } else { span });
        }
        if highlight {
            let used: usize = spans.iter().map(Span::width).sum();
            spans.push(Span::styled(" ".repeat(self.width.saturating_sub(used)), palette().selection));
        }
        self.lines.push(Line::from(spans));
    }

    fn paragraph(&mut self, text: &str, style: Style) {
        for line in wrapped(text, style, self.width.saturating_sub(PREFIX.len())) {
            self.push(line, false);
        }
    }

    /// A selectable row: the first line of a wrapped entry, highlighted when focused.
    fn entry(&mut self, lead: Span<'static>, text: &str, style: Style, highlight: bool) {
        self.rows.push(self.lines.len());
        let lines = lead_wrapped(lead, text, style, self.width.saturating_sub(PREFIX.len()));
        for (index, line) in lines.into_iter().enumerate() {
            self.push(line, highlight && index == 0);
        }
    }

    fn aside(&mut self, text: &str) {
        for line in
            wrapped(text, Style::new().dim(), self.width.saturating_sub(PREFIX.len() + CONTINUATION.len()))
        {
            let mut spans = vec![Span::raw(CONTINUATION)];
            spans.extend(line.spans);
            self.push(Line::from(spans), false);
        }
    }
}

/// The card's rows and, relative to its first row, the rows the keyboard can land on.
pub(crate) fn lines(
    question: &Question,
    shown: &Shown<'_>,
    width: usize,
) -> (Vec<Line<'static>>, Vec<usize>) {
    let width = width.max(PREFIX.len() + 8);
    let color = border_color(question.state);
    let mut border = Style::new().fg(color);
    if shown.focus.is_some() {
        border = border.add_modifier(Modifier::BOLD);
    }
    let mut b = Builder { border, width, lines: Vec::new(), rows: Vec::new() };
    let mut title = format!("╭─ Q{} · {}", shown.number, kind_words(question.kind));
    let state = state_words(question.state);
    if !state.is_empty() {
        title.push_str(" · ");
        title.push_str(state);
    }
    if let Some(name) = question.attachment.as_ref().map(|a| a.name.trim()).filter(|n| !n.is_empty()) {
        title.push_str(" · in ");
        title.push_str(name);
    }
    if shown.drafted {
        title.push_str(" · not saved");
    }
    title.push(' ');
    let fill = "─".repeat(width.saturating_sub(title.chars().count()));
    b.lines.push(Line::from(vec![Span::styled(title, border), Span::styled(fill, border.dim())]));
    b.paragraph(&question.prompt, Style::new().add_modifier(Modifier::BOLD));
    if let Some(context) = question.context.as_deref().filter(|c| !c.trim().is_empty()) {
        b.paragraph(context, Style::new().dim());
    }
    if let Some(stakes) = question.stakes.as_deref().filter(|s| !s.trim().is_empty()) {
        b.paragraph(&format!("Stakes: {stakes}"), Style::new().dim());
    }
    if let Some(reversible) = question.reversible.as_ref().filter(|r| !r.value) {
        let warning = match reversible.why.as_deref().map(str::trim).filter(|why| !why.is_empty()) {
            Some(why) => format!("Can't be undone: {why}"),
            None => "Can't be undone".to_owned(),
        };
        b.paragraph(&warning, Style::new().fg(palette().delete));
    }
    if let Some(shows) = &question.shows {
        let prefix = match shows.kind {
            ShowsKind::Image => "Shows (image)",
            ShowsKind::Link => "Shows (file/link)",
            ShowsKind::Unknown => "Shows",
        };
        if let Some(words) = reference_words(prefix, &shows.label, &shows.src) {
            b.paragraph(&words, Style::new().dim());
        }
    }
    if let Some(prototype) = &question.prototype
        && let Some(words) =
            reference_words("Prototype", prototype.label.as_deref().unwrap_or_default(), &prototype.target)
    {
        b.paragraph(&words, Style::new().dim());
    }
    if let Some(attachment) = &question.attachment {
        let mut source = format!("Question in file: Q{}", question.position.saturating_add(1));
        if !attachment.key.is_empty() {
            source.push_str(" · ");
            source.push_str(&attachment.key);
        }
        b.paragraph(&source, Style::new().dim());
    }
    let answer = shown.answer;
    answer_lines(&mut b, question, shown, color);
    if let Some(why) = question.why.as_deref().map(str::trim).filter(|why| !why.is_empty()) {
        let mut words = String::from("Why");
        let mut first = true;
        for choice in question.choices.iter().filter(|choice| choice.recommended) {
            words.push_str(if first { " " } else { ", " });
            words.push_str(&choice.label);
            first = false;
        }
        words.push_str(": ");
        words.push_str(why);
        b.paragraph(&words, Style::new().dim());
    }
    if let Some(note) = answer.note.as_deref().filter(|n| !n.trim().is_empty()) {
        b.paragraph(&format!("Note: {note}"), Style::new().fg(palette().comment));
    }
    if question.kind != Kind::Action && !matches!(question.state, State::Open | State::Picked) {
        b.paragraph(&question.summary(), Style::new().fg(color));
    }
    if let Some(stopped) = question.stopped.as_deref().filter(|s| !s.trim().is_empty()) {
        b.paragraph(&format!("Stopped: {stopped}"), Style::new().fg(palette().delete));
    }
    if !question.holds_up.is_empty() {
        b.paragraph(
            &format!("Holds up: {}", question.holds_up.join(", ")),
            Style::new().fg(palette().delete).dim(),
        );
    }
    let bottom = "╰".to_owned() + &"─".repeat(width.saturating_sub(1));
    b.lines.push(Line::from(vec![Span::styled(bottom, border.dim())]));
    (b.lines, b.rows)
}

fn answer_lines(b: &mut Builder, question: &Question, shown: &Shown<'_>, color: Color) {
    let answer = shown.answer;
    match question.kind {
        Kind::Single | Kind::Multi => {
            b.push(Line::default(), false);
            let (on, off) = if question.kind == Kind::Multi { ("[x] ", "[ ] ") } else { ("(x) ", "( ) ") };
            for (index, choice) in question.choices.iter().enumerate() {
                let picked = answer.selected.iter().any(|s| s == &choice.label);
                let lead = Span::styled(if picked { on } else { off }, Style::new().fg(color));
                let mut label = choice.label.clone();
                if let Some(description) = choice.description.as_deref().filter(|d| !d.trim().is_empty()) {
                    label.push_str(" — ");
                    label.push_str(description);
                }
                if choice.recommended {
                    label.push_str(" ★");
                }
                let style = if choice.settled { Style::new().dim() } else { Style::new() };
                b.entry(lead, &label, style, shown.focus == Some(index));
                if let Some(image) = &choice.image
                    && let Some(words) = reference_words("Image", &image.alt, &image.src)
                {
                    b.aside(&words);
                }
                if let Some(prototype) = &choice.prototype
                    && let Some(words) = reference_words(
                        "Prototype",
                        prototype.label.as_deref().unwrap_or_default(),
                        &prototype.target,
                    )
                {
                    b.aside(&words);
                }
                for pro in &choice.pros {
                    b.aside(&format!("+ {pro}"));
                }
                for con in &choice.cons {
                    b.aside(&format!("− {con}"));
                }
            }
            let other_on = answer.other.as_deref().is_some_and(|o| !o.is_empty());
            let lead = Span::styled(if other_on { on } else { off }, Style::new().fg(color));
            let text = match answer.other.as_deref() {
                Some(other) if !other.is_empty() => format!("Other: {other}"),
                _ => "Other… (o)".to_owned(),
            };
            b.entry(lead, &text, Style::new().dim(), shown.focus == Some(question.choices.len()));
            if let Some(recommendation) = question.recommendation.as_deref().filter(|r| !r.trim().is_empty())
            {
                b.paragraph(&format!("Recommended: {recommendation}"), Style::new().dim());
            }
        }
        Kind::Text => {
            b.push(Line::default(), false);
            let lead = Span::styled("› ", Style::new().fg(color));
            if let Some(text) = answer.text.as_deref().filter(|t| !t.is_empty()) {
                b.entry(lead, text, Style::new(), shown.focus == Some(0));
            } else {
                let hint = match question.suggested_text.as_deref() {
                    Some(suggested) if !suggested.trim().is_empty() => {
                        format!("(t to answer; suggested: {suggested})")
                    }
                    _ => "(t to answer)".to_owned(),
                };
                b.entry(lead, &hint, Style::new().dim(), shown.focus == Some(0));
            }
        }
        Kind::Action => {
            if let Some(action) = &question.action {
                if let Some(words) = reference_words(
                    "Where",
                    action.where_.as_deref().unwrap_or_default(),
                    action.where_url.as_deref().unwrap_or_default(),
                ) {
                    b.paragraph(&words, Style::new().dim());
                }
                for (label, text) in [("Run", &action.run), ("Watching", &action.watching)] {
                    if let Some(text) = text.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
                        b.paragraph(&format!("{label}: {text}"), Style::new().dim());
                    }
                }
            }
            if let Some(result) = &question.action_result {
                let outcome = match &result.outcome {
                    super::actions::ActionOutcome::Done => "Done",
                    super::actions::ActionOutcome::Cant => "Can't do it",
                };
                let by = if result.by == "agent" { "agent" } else { "you" };
                let in_draft = result.in_draft(question.state, question.revision, question.sent_revision);
                let mut status = format!("{outcome} · {by}");
                if in_draft {
                    status.push_str(" · in reply draft");
                }
                b.paragraph(&status, Style::new().fg(color));
                if let Some(note) = result.note.as_deref().map(str::trim).filter(|note| !note.is_empty()) {
                    b.paragraph(&format!("Note: {note}"), Style::new().fg(palette().comment));
                }
                if result.by == "agent"
                    && let Some(note) = result.person.as_ref().and_then(|person| person.note.as_deref())
                    && !note.trim().is_empty()
                {
                    b.paragraph(&format!("Your note: {}", note.trim()), Style::new().fg(palette().comment));
                }
            }
        }
        Kind::Unknown => {}
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing, reason = "tests assert by panicking")]
mod tests {
    use super::{Shown, answerable, lines};
    use crate::inbox::questions::{Answer, Kind, Question, State};
    use ratatui::text::Line;
    use serde_json::json;

    fn row_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|span| span.content.as_ref()).collect()
    }

    fn rendered(
        question: &Question,
        answer: &Answer,
        focus: Option<usize>,
    ) -> (Vec<Line<'static>>, Vec<usize>) {
        lines(question, &Shown { number: 7, answer, drafted: false, focus }, 160)
    }

    fn detailed_question() -> Question {
        serde_json::from_value(json!({
            "key": "q-layout-2",
            "kind": "single",
            "state": "open",
            "position": 1,
            "prompt": "Which layout?",
            "stakes": "Checkout ships tomorrow.",
            "shows": {"kind": "image", "label": "Both layouts", "src": "both.png"},
            "why": "The total stays visible.",
            "reversible": {"value": false, "why": "The previous layout is removed."},
            "prototype": {"target": "shared.html", "label": "Compare layouts"},
            "attachment": {"id": "att_plan", "name": "plan.md", "key": "q-layout"},
            "choices": [{
                "label": "Pinned total",
                "recommended": true,
                "image": {"alt": "Pinned checkout", "src": "pinned.png"},
                "prototype": {"target": "http://localhost:3000/pinned", "label": "Try pinned"},
                "pros": ["Always visible"],
                "cons": ["Less space"]
            }, {
                "label": "Floating total",
                "prototype": {"target": "floating.html"}
            }]
        }))
        .expect("question fixture")
    }

    #[test]
    fn orphaned_question_is_not_answerable() {
        let question =
            Question { kind: Kind::Single, state: State::Open, orphaned: true, ..Question::default() };

        assert!(!answerable(&question));
    }

    #[test]
    fn renders_server_details_without_adding_answer_rows() {
        let question = detailed_question();
        let answer = Answer { selected: vec!["Pinned total".to_owned()], ..Answer::default() };
        let (rendered, rows) = rendered(&question, &answer, Some(1));
        let text = rendered.iter().map(row_text).collect::<Vec<_>>().join("\n");
        for detail in [
            "in plan.md",
            "Question in file: Q2 · q-layout",
            "Stakes: Checkout ships tomorrow.",
            "Shows (image): Both layouts — both.png",
            "Can't be undone: The previous layout is removed.",
            "Prototype: Compare layouts — shared.html",
            "Prototype: Try pinned — http://localhost:3000/pinned",
            "Prototype: floating.html",
            "Image: Pinned checkout — pinned.png",
            "+ Always visible",
            "− Less space",
            "Why Pinned total: The total stays visible.",
        ] {
            assert!(text.contains(detail), "missing {detail:?} from {text}");
        }
        assert_eq!(rows.len(), 3);
        assert!(row_text(&rendered[rows[0]]).contains("(x) Pinned total"));
        assert!(row_text(&rendered[rows[1]]).contains("( ) Floating total"));
        assert!(row_text(&rendered[rows[2]]).contains("Other… (o)"));
        assert!(answerable(&question));
    }

    #[test]
    fn renders_file_links_and_why_without_a_recommendation() {
        let question: Question = serde_json::from_value(json!({
            "kind": "text",
            "state": "open",
            "prompt": "What should change?",
            "shows": {"kind": "link", "label": "Design notes", "src": "design.md"},
            "why": "Keep checkout simple.",
            "reversible": {"value": false, "why": null},
            "prototype": {"target": "shared.html"}
        }))
        .expect("text question fixture");
        let answer = Answer { text: Some("Keep the total pinned".to_owned()), ..Answer::default() };
        let (rendered, rows) = rendered(&question, &answer, Some(0));
        let text = rendered.iter().map(row_text).collect::<Vec<_>>().join("\n");
        assert!(text.contains("Shows (file/link): Design notes — design.md"));
        assert!(text.contains("Why: Keep checkout simple."));
        assert!(text.contains("Can't be undone"));
        assert!(!text.contains("Can't be undone:"));
        assert!(text.contains("Prototype: shared.html"));
        assert_eq!(rows.len(), 1);
        assert!(row_text(&rendered[rows[0]]).contains("Keep the total pinned"));
        assert!(answerable(&question));
    }

    #[test]
    fn details_wrap_without_displacing_choice_focus() {
        let question = detailed_question();
        let answer = Answer::default();
        let shown = Shown { number: 7, answer: &answer, drafted: false, focus: Some(1) };
        let (rendered, rows) = lines(&question, &shown, 32);
        assert_eq!(rows.len(), 3);
        assert!(rows[0] < rows[1] && rows[1] < rows[2]);
        assert!(row_text(&rendered[rows[0]]).contains("Pinned total"));
        assert!(row_text(&rendered[rows[1]]).contains("Floating total"));
        let selection = crate::theme::palette().selection;
        let choice_style = ratatui::style::Style::new().fg(crate::theme::palette().comment);
        assert_eq!(rendered[rows[1]].spans[1].style, choice_style.patch(selection));
        assert_ne!(rendered[rows[1] + 1].spans[1].style, choice_style.patch(selection));
    }

    #[test]
    fn empty_optional_details_leave_older_cards_unchanged() {
        let question: Question = serde_json::from_value(json!({
            "kind": "single", "state": "open", "prompt": "Pick?",
            "choices": [{"label": "A"}]
        }))
        .expect("older question fixture");
        let with_empty: Question = serde_json::from_value(json!({
            "kind": "single", "state": "open", "prompt": "Pick?",
            "shows": {"kind": "image", "label": " ", "src": " "},
            "why": " ",
            "reversible": {"value": true, "why": "still reversible"},
            "prototype": {"target": " ", "label": " "},
            "choices": [{
                "label": "A",
                "image": {"alt": " ", "src": " "},
                "prototype": {"target": " ", "label": " "}
            }]
        }))
        .expect("empty optional details fixture");
        let answer = Answer::default();
        assert_eq!(rendered(&question, &answer, None), rendered(&with_empty, &answer, None));
    }

    #[test]
    fn unknown_reference_kinds_and_unlabelled_images_remain_visible() {
        let question: Question = serde_json::from_value(json!({
            "kind": "multi", "state": "sent", "prompt": "Pick?",
            "shows": {"kind": "future", "label": "Plan", "src": "plan.md"},
            "why": "Both fit.",
            "choices": [
                {"label": "A", "recommended": true, "image": {"alt": "", "src": "a.png"}},
                {"label": "B", "recommended": true, "prototype": {"label": "b.html", "target": "b.html"}}
            ],
            "answer": {"selected": ["A", "B"]}
        }))
        .expect("additive question fixture");
        let answer = question.answer.as_ref().expect("saved answer");
        let (rendered, rows) = rendered(&question, answer, None);
        let text = rendered.iter().map(row_text).collect::<Vec<_>>().join("\n");
        assert!(text.contains("Shows: Plan — plan.md"));
        assert!(text.contains("Image: a.png"));
        assert!(text.contains("Prototype: b.html"));
        assert!(!text.contains("b.html — b.html"));
        assert!(text.contains("Why A, B: Both fit."));
        assert!(text.contains("Answer: A, B"));
        assert_eq!(rows.len(), 3);
        assert!(!answerable(&question));
    }

    #[test]
    fn action_details_remain_read_only_and_preserve_staged_notes() {
        let question: Question = serde_json::from_value(json!({
            "kind": "action", "state": "picked", "prompt": "Approve the release",
            "revision": 2, "sent_revision": 0,
            "action": {
                "where": "Release dashboard", "where_url": "https://example.com/release",
                "run": "release inspect", "watching": "The approval status"
            },
            "action_result": {
                "outcome": "cant", "by": "person", "note": "Need access first", "staged": true,
                "at": "2026-11-11T12:00:00Z", "message_id": null, "person": null
            }
        }))
        .expect("staged action fixture");
        let (rendered, rows) = rendered(&question, &Answer::default(), None);
        let text = rendered.iter().map(row_text).collect::<Vec<_>>().join("\n");
        assert!(text.contains("Where: Release dashboard — https://example.com/release"));
        assert!(text.contains("Run: release inspect"));
        assert!(text.contains("Watching: The approval status"));
        assert!(text.contains("Can't do it · you · in reply draft"));
        assert!(text.contains("Note: Need access first"));
        assert!(rows.is_empty());
        assert!(!answerable(&question));
    }

    #[test]
    fn agent_completed_actions_show_the_preserved_person_note() {
        let question: Question = serde_json::from_value(json!({
            "kind": "action", "state": "closed", "prompt": "Approve the release",
            "revision": 2, "sent_revision": 0,
            "action_result": {
                "outcome": "done", "by": "agent", "note": null,
                "at": "2026-11-11T12:01:00Z", "message_id": "msg_done", "staged": false,
                "person": {"outcome": "cant", "note": "Need access first", "at": "2026-11-11T12:00:00Z"}
            }
        }))
        .expect("completed action fixture");
        let (rendered, rows) = rendered(&question, &Answer::default(), None);
        let text = rendered.iter().map(row_text).collect::<Vec<_>>().join("\n");
        assert!(text.contains("Done · agent · in reply draft"));
        assert!(text.contains("Your note: Need access first"));
        assert!(!text.contains("No answer"));
        assert!(rows.is_empty());
    }
}
