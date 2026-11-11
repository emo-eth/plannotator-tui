//! An answer card: one question as the person answers it, drawn as boxed rows. The thread
//! view places the cards; this module only knows how one looks and which of its rows the
//! keyboard can land on.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::questions::{Answer, Kind, Question, State};
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
        Kind::Action | Kind::Unknown => {}
    }
    if let Some(note) = answer.note.as_deref().filter(|n| !n.trim().is_empty()) {
        b.paragraph(&format!("Note: {note}"), Style::new().fg(palette().comment));
    }
    if !matches!(question.state, State::Open | State::Picked) {
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
#[cfg(test)]
mod tests {
    use super::answerable;
    use crate::inbox::questions::{Kind, Question, State};

    #[test]
    fn orphaned_question_is_not_answerable() {
        let question =
            Question { kind: Kind::Single, state: State::Open, orphaned: true, ..Question::default() };

        assert!(!answerable(&question));
    }
}
