#![allow(clippy::all, clippy::pedantic)]

//! Question card interactive state machine.
//!
//! Renders single-choice, multi-choice, and free-text question cards with keyboard controls.
//! Generates canonical `QuestionAnswer` payloads for server sync via `save_picks`.

use std::collections::BTreeSet;

use plannotator_tui_schema::inbox::{InboxQuestion, QuestionAnswer, QuestionKind};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::editing::TextBuffer;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum QuestionCardAction {
    /// Focus moved or answer changed locally (needs server `save_picks`).
    Changed,
    /// Card requested to advance focus to next card or message.
    NextCard,
    /// Card requested to return focus to previous card.
    PrevCard,
    /// Key not handled by this card.
    Unhandled,
}

/// Interactive state for one question card.
#[derive(Debug, Clone)]
pub(crate) struct QuestionCard {
    pub(crate) question: InboxQuestion,
    pub(crate) focused_choice: usize,
    pub(crate) selected_labels: BTreeSet<String>,
    pub(crate) other_active: bool,
    pub(crate) other_text: TextBuffer,
    pub(crate) is_dirty: bool,
}

impl QuestionCard {
    pub(crate) fn new(question: InboxQuestion) -> Self {
        let mut selected_labels = BTreeSet::new();
        let mut other_active = false;
        let mut other_content = String::new();

        if let Some(ans) = &question.answer {
            for sel in &ans.selected {
                selected_labels.insert(sel.clone());
            }
            if let Some(other) = &ans.other {
                other_active = true;
                other_content = other.clone();
            }
            if let Some(text) = &ans.text {
                other_active = true;
                other_content = text.clone();
            }
        }

        // Default focus to recommended choice if any and not yet answered
        let mut focused_choice = 0;
        if selected_labels.is_empty() && !other_active {
            if let Some(rec_idx) = question.choices.iter().position(|c| c.recommended) {
                focused_choice = rec_idx;
            }
        } else if let Some(first_sel) = selected_labels.iter().next() {
            if let Some(idx) = question.choices.iter().position(|c| &c.label == first_sel) {
                focused_choice = idx;
            }
        }

        Self {
            question,
            focused_choice,
            selected_labels,
            other_active,
            other_text: TextBuffer::with_text(&other_content),
            is_dirty: false,
        }
    }

    /// Total number of interactive focus targets (choices + "Other..." on choice cards, or 1 for text).
    pub(crate) fn total_targets(&self) -> usize {
        match self.question.kind {
            QuestionKind::Single | QuestionKind::Multi => self.question.choices.len() + 1, // +1 for "Other…"
            QuestionKind::Text => 1,
        }
    }

    pub(crate) fn is_other_focused(&self) -> bool {
        match self.question.kind {
            QuestionKind::Single | QuestionKind::Multi => self.focused_choice == self.question.choices.len(),
            QuestionKind::Text => true,
        }
    }

    pub(crate) fn is_choice_selected(&self, label: &str) -> bool {
        self.selected_labels.contains(label)
    }

    #[allow(dead_code)]
    pub(crate) fn is_answered(&self) -> bool {
        !self.selected_labels.is_empty() || (self.other_active && !self.other_text.text().trim().is_empty())
    }

    /// Build the canonical `QuestionAnswer` struct to send to the server.
    pub(crate) fn build_answer(&self) -> QuestionAnswer {
        let other_val = if self.other_active {
            let t = self.other_text.text().trim().to_owned();
            if t.is_empty() { None } else { Some(t) }
        } else {
            None
        };

        let (text_val, final_other) = match self.question.kind {
            QuestionKind::Text => (other_val, None),
            _ => (None, other_val),
        };

        QuestionAnswer {
            v: 1,
            key: self.question.key.clone(),
            kind: self.question.kind,
            prompt: self.question.prompt.clone(),
            selected: self.selected_labels.iter().cloned().collect(),
            other: final_other,
            text: text_val,
            note: None,
            skipped: None,
            source_line: None,
        }
    }

    /// Handle key input on this question card.
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> QuestionCardAction {
        let targets = self.total_targets();

        // If editing "Other…" or text input, allow text editing keys unless moving out
        if self.is_other_focused() && self.other_active {
            match key.code {
                KeyCode::Esc => {
                    self.other_active = false;
                    self.is_dirty = true;
                    return QuestionCardAction::Changed;
                }
                KeyCode::Up if self.focused_choice > 0 => {
                    self.focused_choice -= 1;
                    return QuestionCardAction::Changed;
                }
                KeyCode::Tab if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                    return QuestionCardAction::NextCard;
                }
                KeyCode::BackTab | KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    return QuestionCardAction::PrevCard;
                }
                _ => {
                    let outcome = self.other_text.handle_key(key);
                    if outcome == crate::app::editing::EditOutcome::Changed {
                        self.is_dirty = true;
                        return QuestionCardAction::Changed;
                    }
                }
            }
        }

        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                if self.focused_choice + 1 < targets {
                    self.focused_choice += 1;
                    QuestionCardAction::Changed
                } else {
                    QuestionCardAction::NextCard
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.focused_choice > 0 {
                    self.focused_choice -= 1;
                    QuestionCardAction::Changed
                } else {
                    QuestionCardAction::PrevCard
                }
            }
            KeyCode::Char(' ') | KeyCode::Enter => {
                if self.is_other_focused() {
                    self.other_active = true;
                    if self.question.kind == QuestionKind::Single {
                        self.selected_labels.clear();
                    }
                    self.is_dirty = true;
                    QuestionCardAction::Changed
                } else if let Some(choice) = self.question.choices.get(self.focused_choice) {
                    let label = choice.label.clone();
                    match self.question.kind {
                        QuestionKind::Single => {
                            self.selected_labels.clear();
                            self.selected_labels.insert(label);
                            self.other_active = false;
                            self.is_dirty = true;
                        }
                        QuestionKind::Multi => {
                            if self.selected_labels.contains(&label) {
                                self.selected_labels.remove(&label);
                            } else {
                                self.selected_labels.insert(label);
                            }
                            self.is_dirty = true;
                        }
                        QuestionKind::Text => {}
                    }
                    QuestionCardAction::Changed
                } else {
                    QuestionCardAction::Unhandled
                }
            }
            _ => QuestionCardAction::Unhandled,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plannotator_tui_schema::inbox::{InboxQuestionChoice, InboxQuestionState};

    fn make_test_question(kind: QuestionKind, labels: &[&str]) -> InboxQuestion {
        let choices = labels
            .iter()
            .map(|&l| InboxQuestionChoice {
                label: l.to_owned(),
                description: None,
                recommended: l.contains("Rec"),
                settled: false,
            })
            .collect();

        InboxQuestion {
            key: "q-test".to_owned(),
            position: 0,
            kind,
            prompt: "Test question?".to_owned(),
            context: None,
            choices,
            recommendation: None,
            suggested_text: None,
            decision_on_answer: false,
            stopped: None,
            holds_up: vec![],
            asked_by_agent_id: None,
            orphaned: false,
            state: InboxQuestionState::Open,
            answer: None,
            revision: 1,
            sent_revision: 0,
            picked_by: None,
            picked_at: None,
            sent_reply_id: None,
            decision_id: None,
            message_id: "msg_test".to_owned(),
            decision_recording: false,
            decision_draft: None,
            superseded_by: None,
        }
    }

    #[test]
    fn test_single_choice_selection() {
        let q = make_test_question(QuestionKind::Single, &["Option A", "Option B (Rec)", "Option C"]);
        let mut card = QuestionCard::new(q);

        // Initially focuses recommended choice: index 1 ("Option B (Rec)")
        assert_eq!(card.focused_choice, 1);
        assert!(!card.is_answered());

        // Press Space to select
        let act = card.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert_eq!(act, QuestionCardAction::Changed);
        assert!(card.is_choice_selected("Option B (Rec)"));
        assert!(card.is_answered());

        // Navigate Down to Option C and select
        card.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(card.focused_choice, 2);
        card.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(card.is_choice_selected("Option C"));
        assert!(!card.is_choice_selected("Option B (Rec)")); // single choice replaced
    }

    #[test]
    fn test_multi_choice_toggle() {
        let q = make_test_question(QuestionKind::Multi, &["First", "Second", "Third"]);
        let mut card = QuestionCard::new(q);

        // Select first
        card.focused_choice = 0;
        card.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(card.is_choice_selected("First"));

        // Select second
        card.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        card.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(card.is_choice_selected("First"));
        assert!(card.is_choice_selected("Second"));

        // Toggle first off
        card.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        card.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(!card.is_choice_selected("First"));
        assert!(card.is_choice_selected("Second"));
    }

    #[test]
    fn test_build_answer_format() {
        let q = make_test_question(QuestionKind::Single, &["Approve", "Reject"]);
        let mut card = QuestionCard::new(q);
        card.focused_choice = 0;
        card.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));

        let ans = card.build_answer();
        assert_eq!(ans.v, 1);
        assert_eq!(ans.key, "q-test");
        assert_eq!(ans.selected, vec!["Approve"]);
        assert!(ans.other.is_none());
    }
}
