//! The comment box: a multi-line editor powered by `TextBuffer`.
//!
//! Enter saves; Shift+Enter (where the terminal's keyboard protocol distinguishes it),
//! Alt+Enter and Ctrl+J insert a new line; Esc cancels. Full macOS-standard editing keys
//! (Alt+Left/Right word jump, Alt+Backspace / Ctrl+w word delete, Cmd+Left/Right line jump,
//! Ctrl+u/k kill line, Alt+Up/Down paragraph, Shift+Alt selection) are supported via
//! `TextBuffer`.

use super::editing::TextBuffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// What a keystroke did to the compose box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ComposeAction {
    Edited,
    Save,
    Cancel,
}

#[derive(Debug, Default)]
pub(super) struct Compose {
    pub(super) buffer: TextBuffer,
}

impl Compose {
    pub(super) fn with_text(text: &str) -> Self {
        Self { buffer: TextBuffer::with_text(text) }
    }

    pub(super) fn value(&self) -> String {
        self.buffer.text()
    }

    pub(super) fn handle_key(&mut self, key: KeyEvent) -> ComposeAction {
        match key.code {
            KeyCode::Esc => return ComposeAction::Cancel,
            KeyCode::Enter if key.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) => {
                self.buffer.insert('\n');
                return ComposeAction::Edited;
            }
            KeyCode::Enter => return ComposeAction::Save,
            KeyCode::Char('j' | 'J') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.buffer.insert('\n');
                return ComposeAction::Edited;
            }
            _ => {}
        }

        let _ = self.buffer.handle_key(key);
        ComposeAction::Edited
    }

    /// Insert pasted text as typed characters, newlines included.
    pub(super) fn insert_text(&mut self, text: &str) {
        self.buffer.insert_text(text);
    }

    /// The buffer wrapped to `width` display cells, with the cursor's (row, col) in that
    /// wrapping. Explicit newlines always break; a full row wraps to the next.
    #[allow(dead_code)]
    pub(super) fn wrapped(&self, width: usize) -> (Vec<String>, usize, usize) {
        self.buffer.wrapped(width)
    }

    /// The buffer wrapped to `width` cells with selection styling.
    pub(super) fn wrapped_with_selection(&self, width: usize) -> (Vec<Vec<(String, bool)>>, usize, usize) {
        self.buffer.wrapped_with_selection(width)
    }
}
