//! The comment box: a small multi-line editor over `editing::TextBuffer`.
//!
//! Enter saves; Shift+Enter (where the terminal's keyboard protocol distinguishes it),
//! Alt+Enter and Ctrl+J insert a new line; Esc cancels; F1 or Ctrl+/ shows the editing
//! keys. The buffer is wrapped to the box width for display only — the saved body keeps
//! exactly the newlines the user typed.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::editing::TextBuffer;

/// What a keystroke did to the compose box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ComposeAction {
    Edited,
    Save,
    Cancel,
}

/// Ctrl+/ from a terminal without the kitty keyboard protocol is the byte 0x1f, which
/// crossterm reports as Ctrl+7.
const LEGACY_CTRL_SLASH: char = '7';

#[derive(Debug, Default)]
pub(super) struct Compose {
    buffer: TextBuffer,
    /// Whether the editing keys overlay is open.
    pub(super) show_help: bool,
}

impl Compose {
    pub(super) fn with_text(text: &str) -> Self {
        Self { buffer: TextBuffer::with_text(text), show_help: false }
    }

    pub(super) fn value(&self) -> String {
        self.buffer.text()
    }

    pub(super) fn handle_key(&mut self, key: KeyEvent) -> ComposeAction {
        match key.code {
            KeyCode::Esc => return ComposeAction::Cancel,
            KeyCode::Enter if key.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) => {
                self.buffer.insert('\n');
            }
            KeyCode::Enter => return ComposeAction::Save,
            KeyCode::Char('j' | 'J') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.buffer.insert('\n');
            }
            KeyCode::F(1) => self.show_help = !self.show_help,
            KeyCode::Char('/' | '?' | LEGACY_CTRL_SLASH) if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.show_help = !self.show_help;
            }
            _ => self.buffer.handle_key(key),
        }
        ComposeAction::Edited
    }

    /// Insert pasted text as typed characters, newlines included.
    pub(super) fn insert_text(&mut self, text: &str) {
        self.buffer.insert_text(text);
    }

    /// The buffer wrapped to `width` display cells; see `TextBuffer::wrapped`.
    pub(super) fn wrapped(&self, width: usize) -> (Vec<Vec<(String, bool)>>, usize, usize) {
        self.buffer.wrapped(width)
    }
}
