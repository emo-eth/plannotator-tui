//! Text editing state machine with OS-level (macOS) keybindings.
//!
//! Provides word navigation/deletion via Unicode Standard Annex #29 (`unicode-segmentation`),
//! line and paragraph navigation, kill ring operations, and text selection.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

/// Actions resulting from key handling in `TextBuffer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditOutcome {
    /// Text or cursor changed.
    Changed,
    /// Key was not handled by the text editor (e.g. Enter, Esc, Tab).
    Unhandled,
}

/// A text buffer supporting macOS-standard editing keys and selection.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct TextBuffer {
    chars: Vec<char>,
    cursor: usize,
    selection_anchor: Option<usize>,
    show_help: bool,
}

fn width_of(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(0)
}

impl TextBuffer {
    #[allow(dead_code)]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn with_text(text: &str) -> Self {
        let chars: Vec<char> = text.chars().collect();
        let cursor = chars.len();
        Self { chars, cursor, selection_anchor: None, show_help: false }
    }

    pub(crate) fn text(&self) -> String {
        self.chars.iter().collect()
    }

    #[allow(dead_code)]
    pub(crate) fn chars(&self) -> &[char] {
        &self.chars
    }

    #[allow(dead_code)]
    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    #[allow(dead_code)]
    pub(crate) fn set_cursor(&mut self, cursor: usize) {
        self.cursor = cursor.min(self.chars.len());
        self.selection_anchor = None;
    }

    #[allow(dead_code)]
    pub(crate) fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    #[allow(dead_code)]
    pub(crate) fn len(&self) -> usize {
        self.chars.len()
    }

    pub(crate) fn show_help(&self) -> bool {
        self.show_help
    }

    pub(crate) fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
    }

    pub(crate) fn selection_range(&self) -> Option<(usize, usize)> {
        let anchor = self.selection_anchor?;
        if anchor == self.cursor { None } else { Some((anchor.min(self.cursor), anchor.max(self.cursor))) }
    }

    pub(crate) fn has_selection(&self) -> bool {
        self.selection_range().is_some()
    }

    #[allow(dead_code)]
    pub(crate) fn clear_selection(&mut self) {
        self.selection_anchor = None;
    }

    pub(crate) fn select_all(&mut self) {
        if self.chars.is_empty() {
            return;
        }
        self.selection_anchor = Some(0);
        self.cursor = self.chars.len();
    }

    /// Delete selected text if a selection exists. Returns true if text was deleted.
    pub(crate) fn delete_selection(&mut self) -> bool {
        if let Some((start, end)) = self.selection_range() {
            self.chars.drain(start..end);
            self.cursor = start;
            self.selection_anchor = None;
            true
        } else {
            false
        }
    }

    /// Insert a character at cursor, replacing any active selection.
    pub(crate) fn insert(&mut self, c: char) {
        self.delete_selection();
        self.chars.insert(self.cursor, c);
        self.cursor += 1;
    }

    /// Insert text at cursor, replacing any active selection.
    pub(crate) fn insert_text(&mut self, text: &str) {
        self.delete_selection();
        for c in text.replace("\r\n", "\n").replace('\r', "\n").chars() {
            self.chars.insert(self.cursor, c);
            self.cursor += 1;
        }
    }

    /// Backspace: delete selection or delete preceding character.
    pub(crate) fn delete_back(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.cursor > 0 {
            self.cursor -= 1;
            self.chars.remove(self.cursor);
        }
    }

    /// Forward delete: delete selection or delete character at cursor.
    pub(crate) fn delete_forward(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.cursor < self.chars.len() {
            self.chars.remove(self.cursor);
        }
    }

    fn update_selection(&mut self, select: bool, prev_cursor: usize) {
        if select {
            if self.selection_anchor.is_none() {
                self.selection_anchor = Some(prev_cursor);
            }
        } else {
            self.selection_anchor = None;
        }
    }

    /// Move cursor left by one character.
    pub(crate) fn move_left(&mut self, select: bool) {
        let prev = self.cursor;
        if !select
            && self.has_selection()
            && let Some((start, _)) = self.selection_range()
        {
            self.cursor = start;
            self.selection_anchor = None;
            return;
        }
        self.cursor = self.cursor.saturating_sub(1);
        self.update_selection(select, prev);
    }

    /// Move cursor right by one character.
    pub(crate) fn move_right(&mut self, select: bool) {
        let prev = self.cursor;
        if !select
            && self.has_selection()
            && let Some((_, end)) = self.selection_range()
        {
            self.cursor = end;
            self.selection_anchor = None;
            return;
        }
        self.cursor = (self.cursor + 1).min(self.chars.len());
        self.update_selection(select, prev);
    }

    /// Move cursor left by one word boundary using Unicode segmentation.
    pub(crate) fn move_word_left(&mut self, select: bool) {
        let prev = self.cursor;
        self.cursor = prev_word_boundary(&self.chars, self.cursor);
        self.update_selection(select, prev);
    }

    /// Move cursor right by one word boundary using Unicode segmentation.
    pub(crate) fn move_word_right(&mut self, select: bool) {
        let prev = self.cursor;
        self.cursor = next_word_boundary(&self.chars, self.cursor);
        self.update_selection(select, prev);
    }

    /// Delete backward to the previous word boundary (Alt+Backspace, Ctrl+w).
    pub(crate) fn delete_word_back(&mut self) {
        if self.delete_selection() {
            return;
        }
        let target = prev_word_boundary(&self.chars, self.cursor);
        if target < self.cursor {
            self.chars.drain(target..self.cursor);
            self.cursor = target;
        }
    }

    /// Delete forward to the next word boundary (Alt+Delete, Alt+d).
    pub(crate) fn delete_word_forward(&mut self) {
        if self.delete_selection() {
            return;
        }
        let target = next_word_boundary(&self.chars, self.cursor);
        if target > self.cursor {
            self.chars.drain(self.cursor..target);
        }
    }

    /// Move cursor to line start (Cmd+Left, Ctrl+a, Home).
    pub(crate) fn move_line_start(&mut self, select: bool) {
        let prev = self.cursor;
        let mut target = self.cursor;
        while target > 0 && self.chars.get(target - 1) != Some(&'\n') {
            target -= 1;
        }
        self.cursor = target;
        self.update_selection(select, prev);
    }

    /// Move cursor to line end (Cmd+Right, Ctrl+e, End).
    pub(crate) fn move_line_end(&mut self, select: bool) {
        let prev = self.cursor;
        let mut target = self.cursor;
        while target < self.chars.len() && self.chars.get(target) != Some(&'\n') {
            target += 1;
        }
        self.cursor = target;
        self.update_selection(select, prev);
    }

    /// Kill from line start to cursor (Ctrl+u).
    pub(crate) fn kill_line_start(&mut self) {
        if self.delete_selection() {
            return;
        }
        let mut start = self.cursor;
        while start > 0 && self.chars.get(start - 1) != Some(&'\n') {
            start -= 1;
        }
        if start < self.cursor {
            self.chars.drain(start..self.cursor);
            self.cursor = start;
        }
    }

    /// Kill from cursor to line end (Ctrl+k). If at line end, deletes the newline.
    pub(crate) fn kill_line_end(&mut self) {
        if self.delete_selection() {
            return;
        }
        let mut end = self.cursor;
        while end < self.chars.len() && self.chars.get(end) != Some(&'\n') {
            end += 1;
        }
        if end > self.cursor {
            self.chars.drain(self.cursor..end);
        } else if end == self.cursor && end < self.chars.len() && self.chars.get(end) == Some(&'\n') {
            self.chars.remove(self.cursor);
        }
    }

    /// Move to the beginning of the current or previous paragraph (Alt+Up).
    pub(crate) fn move_paragraph_prev(&mut self, select: bool) {
        let prev = self.cursor;
        let mut target = self.cursor;
        // Skip any newlines immediately preceding target
        while target > 0 && self.chars.get(target - 1) == Some(&'\n') {
            target -= 1;
        }
        while target > 0 {
            if self.chars.get(target - 1) == Some(&'\n') {
                if target >= 2 && self.chars.get(target - 2) == Some(&'\n') {
                    break;
                }
                if target == 1 {
                    target = 0;
                    break;
                }
            }
            target -= 1;
        }
        self.cursor = target;
        self.update_selection(select, prev);
    }

    /// Move to the beginning of the next paragraph (Alt+Down).
    pub(crate) fn move_paragraph_next(&mut self, select: bool) {
        let prev = self.cursor;
        let len = self.chars.len();
        let mut target = self.cursor;
        while target < len {
            if self.chars.get(target) == Some(&'\n') {
                target += 1;
                if target < len && self.chars.get(target) == Some(&'\n') {
                    while target < len && self.chars.get(target) == Some(&'\n') {
                        target += 1;
                    }
                    break;
                }
            } else {
                target += 1;
            }
        }
        self.cursor = target;
        self.update_selection(select, prev);
    }

    /// Move cursor vertically by `delta` lines, maintaining display column.
    pub(crate) fn move_vertical(&mut self, delta: isize, select: bool) {
        let prev = self.cursor;
        let text: String = self.chars.iter().take(self.cursor).collect();
        let row = text.split('\n').count().saturating_sub(1);
        let col: usize = text.rsplit('\n').next().unwrap_or("").chars().map(width_of).sum();
        let all: String = self.chars.iter().collect();
        let lines: Vec<&str> = all.split('\n').collect();
        let target_row = row.saturating_add_signed(delta).min(lines.len().saturating_sub(1));
        let mut cursor: usize = lines.iter().take(target_row).map(|l| l.chars().count() + 1).sum();
        let mut used = 0;
        for c in lines.get(target_row).copied().unwrap_or_default().chars() {
            let w = width_of(c);
            if used + w > col {
                break;
            }
            used += w;
            cursor += 1;
        }
        self.cursor = cursor;
        self.update_selection(select, prev);
    }

    /// Handle OS-level editing keys. Returns `EditOutcome::Changed` if handled,
    /// or `EditOutcome::Unhandled` if the key should be processed by the caller
    /// (e.g. Enter, Esc, Tab).
    #[allow(clippy::too_many_lines)]
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> EditOutcome {
        let has_shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let has_alt = key.modifiers.contains(KeyModifiers::ALT);
        let has_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let has_cmd = key.modifiers.contains(KeyModifiers::SUPER);

        match key.code {
            // Help toggle: F1 or Ctrl+/
            KeyCode::F(1) => {
                self.toggle_help();
                EditOutcome::Changed
            }
            KeyCode::Char('/' | '?') if has_ctrl => {
                self.toggle_help();
                EditOutcome::Changed
            }

            // Word jump left: Alt+Left, Ctrl+Left, or Alt+b / Alt+B (ESC b)
            KeyCode::Left if has_alt || (has_ctrl && !has_cmd) => {
                self.move_word_left(has_shift);
                EditOutcome::Changed
            }
            KeyCode::Char('b' | 'B') if has_alt && !has_ctrl => {
                self.move_word_left(has_shift);
                EditOutcome::Changed
            }

            // Word jump right: Alt+Right, Ctrl+Right, or Alt+f / Alt+F (ESC f)
            KeyCode::Right if has_alt || (has_ctrl && !has_cmd) => {
                self.move_word_right(has_shift);
                EditOutcome::Changed
            }
            KeyCode::Char('f' | 'F') if has_alt && !has_ctrl => {
                self.move_word_right(has_shift);
                EditOutcome::Changed
            }

            // Line start: Cmd+Left, Home, Ctrl+a
            KeyCode::Left if has_cmd => {
                self.move_line_start(has_shift);
                EditOutcome::Changed
            }
            KeyCode::Home => {
                self.move_line_start(has_shift);
                EditOutcome::Changed
            }
            KeyCode::Char('a' | 'A') if has_ctrl => {
                self.move_line_start(has_shift);
                EditOutcome::Changed
            }

            // Line end: Cmd+Right, End, Ctrl+e
            KeyCode::Right if has_cmd => {
                self.move_line_end(has_shift);
                EditOutcome::Changed
            }
            KeyCode::End => {
                self.move_line_end(has_shift);
                EditOutcome::Changed
            }
            KeyCode::Char('e' | 'E') if has_ctrl => {
                self.move_line_end(has_shift);
                EditOutcome::Changed
            }

            // Select all: Cmd+a
            KeyCode::Char('a' | 'A') if has_cmd => {
                self.select_all();
                EditOutcome::Changed
            }

            // Paragraph up/down: Alt+Up / Alt+Down / Ctrl+Up / Ctrl+Down
            KeyCode::Up if has_alt || (has_ctrl && !has_cmd) => {
                self.move_paragraph_prev(has_shift);
                EditOutcome::Changed
            }
            KeyCode::Down if has_alt || (has_ctrl && !has_cmd) => {
                self.move_paragraph_next(has_shift);
                EditOutcome::Changed
            }

            // Character movement: Left / Right
            KeyCode::Left => {
                self.move_left(has_shift);
                EditOutcome::Changed
            }
            KeyCode::Right => {
                self.move_right(has_shift);
                EditOutcome::Changed
            }

            // Vertical movement: Up / Down
            KeyCode::Up => {
                self.move_vertical(-1, has_shift);
                EditOutcome::Changed
            }
            KeyCode::Down => {
                self.move_vertical(1, has_shift);
                EditOutcome::Changed
            }

            // Delete word back: Alt+Backspace, Ctrl+Backspace, Ctrl+w
            KeyCode::Backspace if (has_alt || has_ctrl) && !has_cmd => {
                self.delete_word_back();
                EditOutcome::Changed
            }
            KeyCode::Char('w' | 'W') if has_ctrl => {
                self.delete_word_back();
                EditOutcome::Changed
            }

            // Delete word forward: Alt+Delete, Ctrl+Delete, Alt+d
            KeyCode::Delete if (has_alt || has_ctrl) && !has_cmd => {
                self.delete_word_forward();
                EditOutcome::Changed
            }
            KeyCode::Char('d' | 'D') if has_alt && !has_ctrl => {
                self.delete_word_forward();
                EditOutcome::Changed
            }

            // Kill line start/end: Ctrl+u, Ctrl+k, Cmd+Backspace, Cmd+Delete
            KeyCode::Char('u' | 'U') if has_ctrl => {
                self.kill_line_start();
                EditOutcome::Changed
            }
            KeyCode::Backspace if has_cmd => {
                self.kill_line_start();
                EditOutcome::Changed
            }
            KeyCode::Char('k' | 'K') if has_ctrl => {
                self.kill_line_end();
                EditOutcome::Changed
            }
            KeyCode::Delete if has_cmd => {
                self.kill_line_end();
                EditOutcome::Changed
            }

            // Single char backspace / delete
            KeyCode::Backspace | KeyCode::Char('h') if key.code == KeyCode::Backspace || has_ctrl => {
                self.delete_back();
                EditOutcome::Changed
            }
            KeyCode::Delete => {
                self.delete_forward();
                EditOutcome::Changed
            }

            // Printable character insertion (when no control/alt modifier, shift is ok)
            KeyCode::Char(c) if !has_ctrl && (!has_alt || has_cmd) => {
                if has_cmd {
                    EditOutcome::Unhandled
                } else {
                    self.insert(c);
                    EditOutcome::Changed
                }
            }

            _ => EditOutcome::Unhandled,
        }
    }

    /// Wrap text to `width` display cells. Returns `(lines, cursor_row, cursor_col)`.
    #[allow(dead_code)]
    pub(crate) fn wrapped(&self, width: usize) -> (Vec<String>, usize, usize) {
        let width = width.max(1);
        let mut lines = vec![String::new()];
        let mut used = 0;
        let (mut cursor_row, mut cursor_col) = (0, 0);
        for (i, &c) in self.chars.iter().enumerate() {
            if i == self.cursor {
                (cursor_row, cursor_col) = (lines.len() - 1, used);
            }
            if c == '\n' {
                lines.push(String::new());
                used = 0;
                continue;
            }
            let w = width_of(c);
            if used + w > width {
                lines.push(String::new());
                used = 0;
            }
            if let Some(last) = lines.last_mut() {
                last.push(c);
            }
            used += w;
        }
        if self.cursor == self.chars.len() {
            (cursor_row, cursor_col) = (lines.len() - 1, used);
        }
        (lines, cursor_row, cursor_col)
    }

    /// Wrap text to `width` display cells with selection highlighting.
    /// Each element in the outer vector is a line of `(text_slice, is_selected)`.
    pub(crate) fn wrapped_with_selection(&self, width: usize) -> (Vec<Vec<(String, bool)>>, usize, usize) {
        let width = width.max(1);
        let selection = self.selection_range();
        let mut lines: Vec<Vec<(String, bool)>> = vec![vec![]];
        let mut current_line_str = String::new();
        let mut used = 0;
        let (mut cursor_row, mut cursor_col) = (0, 0);

        let push_char = |lines: &mut Vec<Vec<(String, bool)>>, c: char, is_sel: bool| {
            if let Some(line) = lines.last_mut() {
                if let Some((text, sel)) = line.last_mut()
                    && *sel == is_sel
                {
                    text.push(c);
                    return;
                }
                line.push((c.to_string(), is_sel));
            }
        };

        for (i, &c) in self.chars.iter().enumerate() {
            if i == self.cursor {
                (cursor_row, cursor_col) = (lines.len() - 1, used);
            }
            let is_sel = selection.is_some_and(|(s, e)| i >= s && i < e);
            if c == '\n' {
                lines.push(vec![]);
                current_line_str.clear();
                used = 0;
                continue;
            }
            let w = width_of(c);
            if used + w > width {
                lines.push(vec![]);
                current_line_str.clear();
                used = 0;
            }
            push_char(&mut lines, c, is_sel);
            current_line_str.push(c);
            used += w;
        }
        if self.cursor == self.chars.len() {
            (cursor_row, cursor_col) = (lines.len() - 1, used);
        }
        (lines, cursor_row, cursor_col)
    }
}

/// Help text listing the editing keybindings.
pub(crate) const EDITING_HELP_SHORT: &str =
    " ⌥/Ctrl←/→ word · ⌥/Ctrl⌫ del word · ⌘←/→ or Home/End line · ^u/^k kill · ⌥/Ctrl↑/↓ para ";

pub(crate) const EDITING_HELP_FULL: &[&str] = &[
    "Editing Keybindings (macOS: Alt/Cmd · Linux/Windows: Ctrl):",
    "  ⌥← / ⌥→ / ^← / ^→ (Alt/Ctrl): Jump word backward / forward",
    "  ⌥⌫ / ^⌫ / ^w               : Delete word backward",
    "  ⌥Delete / ^Delete / Alt+d  : Delete word forward",
    "  ⌘← / Home / ^a             : Jump to line start",
    "  ⌘→ / End / ^e              : Jump to line end",
    "  ^u                         : Kill to line start",
    "  ^k                         : Kill to line end",
    "  ⌥↑ / ⌥↓ / ^↑ / ^↓          : Jump paragraph backward / forward",
    "  Shift + ⌥/^ + Arrows       : Extend selection by word / paragraph",
    "  Shift + Arrows             : Extend selection by character / line",
    "  ⌘a                         : Select all",
    "Terminal Note: Terminal.app needs 'Use Option as Meta'.",
    "iTerm2 needs Option Key -> Esc+. Ghostty works natively.",
];

/// Find the previous word boundary char index before `cursor`.
/// Uses Unicode Standard Annex #29 word segmentation.
pub(crate) fn prev_word_boundary(chars: &[char], cursor: usize) -> usize {
    if cursor == 0 || chars.is_empty() {
        return 0;
    }
    let text: String = chars.iter().collect();
    let byte_pos = text.char_indices().nth(cursor).map_or(text.len(), |(b, _)| b);

    // Segment text into UAX #29 word bounds
    let bounds: Vec<(usize, &str)> = text.split_word_bound_indices().collect();
    if bounds.is_empty() {
        return 0;
    }

    // Find the segment containing or immediately preceding byte_pos
    let mut target_byte = 0;
    for (i, &(b_idx, chunk)) in bounds.iter().enumerate().rev() {
        if b_idx >= byte_pos {
            continue;
        }
        // If cursor is within this chunk, jump to its start
        let chunk_end = b_idx + chunk.len();
        if byte_pos > b_idx && byte_pos < chunk_end {
            target_byte = b_idx;
            break;
        }
        // If this chunk is whitespace, skip backward over it
        if chunk.chars().all(char::is_whitespace) {
            continue;
        }
        // Found preceding non-whitespace chunk
        target_byte = b_idx;
        // If it's punctuation, check if earlier consecutive punctuation should be grouped
        if !chunk.chars().any(|c| c.is_alphanumeric() || c == '_') {
            let mut start = b_idx;
            for &(prev_idx, prev_chunk) in bounds.get(..i).unwrap_or_default().iter().rev() {
                if !prev_chunk.chars().any(|c| c.is_alphanumeric() || c == '_')
                    && !prev_chunk.chars().all(char::is_whitespace)
                {
                    start = prev_idx;
                } else {
                    break;
                }
            }
            target_byte = start;
        }
        break;
    }

    // Convert byte offset back to char index
    text[..target_byte].chars().count()
}

/// Find the next word boundary char index after `cursor`.
/// Uses Unicode Standard Annex #29 word segmentation.
pub(crate) fn next_word_boundary(chars: &[char], cursor: usize) -> usize {
    let len = chars.len();
    if cursor >= len || chars.is_empty() {
        return len;
    }
    let text: String = chars.iter().collect();
    let byte_pos = text.char_indices().nth(cursor).map_or(text.len(), |(b, _)| b);

    let bounds: Vec<(usize, &str)> = text.split_word_bound_indices().collect();
    if bounds.is_empty() {
        return len;
    }

    let mut target_byte = text.len();
    for (i, &(b_idx, chunk)) in bounds.iter().enumerate() {
        let chunk_end = b_idx + chunk.len();
        if chunk_end <= byte_pos {
            continue;
        }
        // If cursor is inside a chunk, jump to the end of this chunk
        if byte_pos >= b_idx && byte_pos < chunk_end {
            if !chunk.chars().all(char::is_whitespace) {
                target_byte = chunk_end;
                break;
            }
            // In whitespace: continue to find the next non-whitespace chunk
            continue;
        }
        // If chunk is whitespace, skip it
        if chunk.chars().all(char::is_whitespace) {
            continue;
        }
        // Non-whitespace chunk: jump to its end
        target_byte = chunk_end;
        // If punctuation, group consecutive punctuation
        if !chunk.chars().any(|c| c.is_alphanumeric() || c == '_') {
            for &(_, next_chunk) in bounds.iter().skip(i + 1) {
                if !next_chunk.chars().any(|c| c.is_alphanumeric() || c == '_')
                    && !next_chunk.chars().all(char::is_whitespace)
                {
                    target_byte += next_chunk.len();
                } else {
                    break;
                }
            }
        }
        break;
    }

    text[..target_byte].chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_word_boundaries_ascii() {
        let chars: Vec<char> = "hello, world! 123".chars().collect();
        assert_eq!(next_word_boundary(&chars, 0), 5); // "hello" -> end of "hello"
        assert_eq!(next_word_boundary(&chars, 5), 6); // "," -> end of ","
        assert_eq!(next_word_boundary(&chars, 6), 12); // " world" -> end of "world"
        assert_eq!(next_word_boundary(&chars, 12), 13); // "!" -> end of "!"
        assert_eq!(next_word_boundary(&chars, 13), 17); // " 123" -> end of "123"

        assert_eq!(prev_word_boundary(&chars, 17), 14); // before "123"
        assert_eq!(prev_word_boundary(&chars, 14), 12); // before "!"
        assert_eq!(prev_word_boundary(&chars, 12), 7); // before "world"
        assert_eq!(prev_word_boundary(&chars, 7), 5); // before ","
        assert_eq!(prev_word_boundary(&chars, 5), 0); // before "hello"
    }

    #[test]
    fn test_word_boundaries_unicode() {
        let chars: Vec<char> = "Café au lait — schön! 🚀 rocket".chars().collect();
        let end_cafe = next_word_boundary(&chars, 0);
        let s: String = chars.get(..end_cafe).map(|c| c.iter().collect()).unwrap_or_default();
        assert_eq!(s, "Café");

        let prev = prev_word_boundary(&chars, end_cafe);
        assert_eq!(prev, 0);
    }

    #[test]
    fn test_delete_word_back() {
        let mut buf = TextBuffer::with_text("foo bar baz");
        buf.delete_word_back();
        assert_eq!(buf.text(), "foo bar ");
        buf.delete_word_back();
        assert_eq!(buf.text(), "foo ");
        buf.delete_word_back();
        assert_eq!(buf.text(), "");
    }

    #[test]
    fn test_delete_word_forward() {
        let mut buf = TextBuffer::with_text("foo bar baz");
        buf.set_cursor(0);
        buf.delete_word_forward();
        assert_eq!(buf.text(), " bar baz");
    }

    #[test]
    fn test_kill_line() {
        let mut buf = TextBuffer::with_text("first line\nsecond line\nthird line");
        buf.set_cursor(16);
        buf.kill_line_start();
        assert_eq!(buf.text(), "first line\nd line\nthird line");

        buf.kill_line_end();
        assert_eq!(buf.text(), "first line\n\nthird line");
    }

    #[test]
    fn test_selection_replacement() {
        let mut buf = TextBuffer::with_text("hello world");
        buf.set_cursor(0);
        buf.move_word_right(true);
        assert_eq!(buf.selection_range(), Some((0, 5)));

        buf.insert('H');
        assert_eq!(buf.text(), "H world");
        assert_eq!(buf.cursor(), 1);
        assert!(!buf.has_selection());
    }

    #[test]
    fn test_cmd_arrows() {
        let mut buf = TextBuffer::with_text("one two three\nfour five six");
        buf.set_cursor(18);
        buf.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::SUPER));
        assert_eq!(buf.cursor(), 14);

        buf.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SUPER));
        assert_eq!(buf.cursor(), 27);
    }

    #[test]
    fn test_ctrl_w_and_alt_backspace() {
        let mut buf = TextBuffer::with_text("alpha beta");
        buf.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(buf.text(), "alpha ");

        buf.insert_text("gamma");
        buf.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT));
        assert_eq!(buf.text(), "alpha ");
    }

    #[test]
    fn test_paragraph_movement() {
        let mut buf = TextBuffer::with_text("para 1\nline 2\n\npara 2\nline 4");
        buf.set_cursor(0);
        buf.move_paragraph_next(false);
        assert_eq!(buf.cursor(), 15);

        buf.move_paragraph_prev(false);
        assert_eq!(buf.cursor(), 0);
    }

    #[test]
    fn test_ctrl_arrows_word_jump() {
        let mut buf = TextBuffer::with_text("hello world again");
        buf.set_cursor(0);
        buf.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL));
        assert_eq!(buf.cursor(), 5);
        buf.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL));
        assert_eq!(buf.cursor(), 11);
        buf.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL));
        assert_eq!(buf.cursor(), 6);
    }

    #[test]
    fn test_ctrl_backspace_and_ctrl_delete() {
        let mut buf = TextBuffer::with_text("alpha beta gamma");
        buf.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL));
        assert_eq!(buf.text(), "alpha beta ");

        buf.set_cursor(0);
        buf.handle_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::CONTROL));
        assert_eq!(buf.text(), " beta ");
    }

    #[test]
    fn test_shift_ctrl_arrows_extend_selection() {
        let mut buf = TextBuffer::with_text("hello world");
        buf.set_cursor(0);
        let mut mods = KeyModifiers::CONTROL;
        mods.insert(KeyModifiers::SHIFT);
        buf.handle_key(KeyEvent::new(KeyCode::Right, mods));
        assert_eq!(buf.selection_range(), Some((0, 5)));
        assert_eq!(buf.cursor(), 5);

        buf.insert('H');
        assert_eq!(buf.text(), "H world");
        assert_eq!(buf.cursor(), 1);
        assert!(!buf.has_selection());
    }

    #[test]
    fn test_ctrl_paragraph_movement() {
        let mut buf = TextBuffer::with_text("para 1\nline 2\n\npara 2\nline 4");
        buf.set_cursor(0);
        buf.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL));
        assert_eq!(buf.cursor(), 15);

        buf.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL));
        assert_eq!(buf.cursor(), 0);
    }
}
