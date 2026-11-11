//! The editing keys: macOS (Option words, Command lines) and Linux/Windows (Ctrl words)
//! conventions side by side, plus the readline Ctrl letters. Shift held through a move
//! extends the selection instead; typing or deleting then replaces it.
//!
//! Option reaches the app as Alt only where the terminal sends it that way (Terminal.app's
//! "Use Option as Meta", iTerm2's "Esc+"), and Command only through the kitty keyboard
//! protocol, which `cli` enables where the terminal has it.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::TextBuffer;

/// The compose box's bottom hint; the full list is behind F1.
pub(in crate::app) const EDITING_HINT: &str = " ⌥/^ ←→ word · ^u ^k kill · f1 all keys ";

/// The editing keys overlay, one row each.
pub(in crate::app) const EDITING_HELP: &[&str] = &[
    "macOS: ⌥ option, ⌘ command · Linux/Windows: ^ ctrl",
    "  ⌥← ⌥→ · ^← ^→ · ⌥b ⌥f       word back / forward",
    "  ⌥⌫ · ^⌫ · ^w                delete word back",
    "  ⌥⌦ · ^⌦ · ⌥d                delete word forward",
    "  ⌘← ⌘→ · home end · ^a ^e    line start / end",
    "  ⌘⌫ · ^u                     kill to line start",
    "  ⌘⌦ · ^k                     kill to line end",
    "  ⌥↑ ⌥↓ · ^↑ ^↓               paragraph back / forward",
    "  shift + any move            extend the selection",
    "  ⌘a                          select all",
    "Terminal.app needs 'Use Option as Meta'; iTerm2 needs Option as Esc+.",
    "⌘ keys need the kitty keyboard protocol, as in Ghostty.",
];

impl TextBuffer {
    pub(in crate::app) fn handle_key(&mut self, key: KeyEvent) {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let cmd = key.modifiers.contains(KeyModifiers::SUPER);
        let word = alt || ctrl;
        match key.code {
            KeyCode::Left if cmd => self.move_line_start(shift),
            KeyCode::Right if cmd => self.move_line_end(shift),
            KeyCode::Backspace if cmd => self.kill_line_start(),
            KeyCode::Delete if cmd => self.kill_line_end(),
            KeyCode::Char('a' | 'A') if cmd => self.select_all(),
            KeyCode::Left if word => self.move_word_left(shift),
            KeyCode::Right if word => self.move_word_right(shift),
            KeyCode::Up if word => self.move_paragraph_prev(shift),
            KeyCode::Down if word => self.move_paragraph_next(shift),
            KeyCode::Backspace if word => self.delete_word_back(),
            KeyCode::Delete if word => self.delete_word_forward(),
            KeyCode::Left => self.move_left(shift),
            KeyCode::Right => self.move_right(shift),
            KeyCode::Up => self.move_vertical(-1, shift),
            KeyCode::Down => self.move_vertical(1, shift),
            KeyCode::Home => self.move_line_start(shift),
            KeyCode::End => self.move_line_end(shift),
            KeyCode::Backspace => self.delete_back(),
            KeyCode::Delete => self.delete_forward(),
            KeyCode::Char('a' | 'A') if ctrl => self.move_line_start(shift),
            KeyCode::Char('e' | 'E') if ctrl => self.move_line_end(shift),
            KeyCode::Char('u' | 'U') if ctrl => self.kill_line_start(),
            KeyCode::Char('k' | 'K') if ctrl => self.kill_line_end(),
            KeyCode::Char('w' | 'W') if ctrl => self.delete_word_back(),
            KeyCode::Char('h' | 'H') if ctrl => self.delete_back(),
            KeyCode::Char('b' | 'B') if alt && !ctrl => self.move_word_left(shift),
            KeyCode::Char('f' | 'F') if alt && !ctrl => self.move_word_right(shift),
            KeyCode::Char('d' | 'D') if alt && !ctrl => self.delete_word_forward(),
            KeyCode::Char(c) if !ctrl && !alt && !cmd => self.insert(c),
            _ => {}
        }
    }
}
