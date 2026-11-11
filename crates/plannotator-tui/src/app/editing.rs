//! The text buffer behind the comment box: a char vec with a cursor and, while Shift is
//! held through a move, a selection anchor. Everything here is a plain operation on the
//! text; the keys that reach them live in `keys`, word boundaries in `words`.

mod keys;
#[cfg(test)]
mod tests;
mod words;

use std::ops::Range;

use unicode_width::UnicodeWidthChar;

pub(super) use keys::{EDITING_HELP, EDITING_HINT};

#[derive(Debug, Default)]
pub(super) struct TextBuffer {
    chars: Vec<char>,
    cursor: usize,
    /// The far end of the selection; the cursor is the near end.
    anchor: Option<usize>,
}

fn width_of(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(0)
}

impl TextBuffer {
    pub(super) fn with_text(text: &str) -> Self {
        let chars: Vec<char> = text.chars().collect();
        let cursor = chars.len();
        Self { chars, cursor, anchor: None }
    }

    pub(super) fn text(&self) -> String {
        self.chars.iter().collect()
    }

    /// The selected char range, when it is not empty.
    pub(super) fn selection(&self) -> Option<Range<usize>> {
        let anchor = self.anchor?;
        (anchor != self.cursor).then(|| anchor.min(self.cursor)..anchor.max(self.cursor))
    }

    /// Type a character over the selection, or at the cursor.
    pub(super) fn insert(&mut self, c: char) {
        self.replace_selection(std::iter::once(c));
    }

    /// Paste text over the selection, or at the cursor; line endings become `\n`.
    pub(super) fn insert_text(&mut self, text: &str) {
        self.replace_selection(text.replace("\r\n", "\n").replace('\r', "\n").chars());
    }

    fn replace_selection(&mut self, text: impl IntoIterator<Item = char>) {
        self.delete(|buffer| buffer.cursor..buffer.cursor);
        for c in text {
            self.chars.insert(self.cursor, c);
            self.cursor += 1;
        }
    }

    /// Remove the selection, or the range `unselected` names when there is none.
    fn delete(&mut self, unselected: impl FnOnce(&Self) -> Range<usize>) {
        let range = self.selection().unwrap_or_else(|| unselected(self));
        self.chars.drain(range.clone());
        self.cursor = range.start;
        self.anchor = None;
    }

    fn delete_back(&mut self) {
        self.delete(|buffer| buffer.cursor.saturating_sub(1)..buffer.cursor);
    }

    fn delete_forward(&mut self) {
        self.delete(|buffer| buffer.cursor..(buffer.cursor + 1).min(buffer.chars.len()));
    }

    fn delete_word_back(&mut self) {
        self.delete(|buffer| words::previous_boundary(&buffer.text(), buffer.cursor)..buffer.cursor);
    }

    fn delete_word_forward(&mut self) {
        self.delete(|buffer| buffer.cursor..words::next_boundary(&buffer.text(), buffer.cursor));
    }

    fn kill_line_start(&mut self) {
        self.delete(|buffer| buffer.line_start()..buffer.cursor);
    }

    /// Delete to the end of the line; at the end already, join the next line on.
    fn kill_line_end(&mut self) {
        self.delete(|buffer| {
            let end = buffer.line_end();
            let newline = buffer.cursor..(end + 1).min(buffer.chars.len());
            if end > buffer.cursor { buffer.cursor..end } else { newline }
        });
    }

    /// Put the cursor at `target`; with `select`, the selection grows from where the
    /// cursor was, otherwise it is dropped.
    fn move_to(&mut self, target: usize, select: bool) {
        if select {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = target.min(self.chars.len());
    }

    /// One char left; a plain move out of a selection lands at its start.
    fn move_left(&mut self, select: bool) {
        let target = match self.selection() {
            Some(range) if !select => range.start,
            _ => self.cursor.saturating_sub(1),
        };
        self.move_to(target, select);
    }

    /// One char right; a plain move out of a selection lands at its end.
    fn move_right(&mut self, select: bool) {
        let target = match self.selection() {
            Some(range) if !select => range.end,
            _ => self.cursor + 1,
        };
        self.move_to(target, select);
    }

    fn move_word_left(&mut self, select: bool) {
        self.move_to(words::previous_boundary(&self.text(), self.cursor), select);
    }

    fn move_word_right(&mut self, select: bool) {
        self.move_to(words::next_boundary(&self.text(), self.cursor), select);
    }

    fn move_line_start(&mut self, select: bool) {
        self.move_to(self.line_start(), select);
    }

    fn move_line_end(&mut self, select: bool) {
        self.move_to(self.line_end(), select);
    }

    /// To the start of this paragraph, or of the one above when already there. Paragraphs
    /// are separated by blank lines.
    fn move_paragraph_prev(&mut self, select: bool) {
        let mut at = self.cursor;
        while at > 0 && self.newline_at(at - 1) {
            at -= 1;
        }
        let target = (2..=at).rev().find(|&i| self.newline_at(i - 1) && self.newline_at(i - 2)).unwrap_or(0);
        self.move_to(target, select);
    }

    /// To the start of the next paragraph, or the end of the text when there is none.
    fn move_paragraph_next(&mut self, select: bool) {
        let len = self.chars.len();
        let target = (self.cursor..len)
            .find(|&i| self.newline_at(i) && self.newline_at(i + 1))
            .map_or(len, |blank| (blank..len).find(|&i| !self.newline_at(i)).unwrap_or(len));
        self.move_to(target, select);
    }

    /// To the logical line above or below, landing on the nearest display column.
    fn move_vertical(&mut self, delta: isize, select: bool) {
        let text: String = self.chars.iter().take(self.cursor).collect();
        let row = text.split('\n').count().saturating_sub(1);
        let col: usize = text.rsplit('\n').next().unwrap_or("").chars().map(width_of).sum();
        let all = self.text();
        let lines: Vec<&str> = all.split('\n').collect();
        let target_row = row.saturating_add_signed(delta).min(lines.len().saturating_sub(1));
        let mut target: usize = lines.iter().take(target_row).map(|l| l.chars().count() + 1).sum();
        let mut used = 0;
        for c in lines.get(target_row).copied().unwrap_or_default().chars() {
            let w = width_of(c);
            if used + w > col {
                break;
            }
            used += w;
            target += 1;
        }
        self.move_to(target, select);
    }

    fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.chars.len();
    }

    fn newline_at(&self, i: usize) -> bool {
        self.chars.get(i) == Some(&'\n')
    }

    fn line_start(&self) -> usize {
        self.chars.iter().take(self.cursor).rposition(|&c| c == '\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self) -> usize {
        let after = self.chars.get(self.cursor..).unwrap_or_default();
        after.iter().position(|&c| c == '\n').map_or(self.chars.len(), |i| self.cursor + i)
    }

    /// The buffer wrapped to `width` display cells, each row as runs of `(text, selected)`,
    /// with the cursor's (row, col) in that wrapping. Explicit newlines always break; a
    /// full row wraps to the next.
    pub(super) fn wrapped(&self, width: usize) -> (Vec<Vec<(String, bool)>>, usize, usize) {
        let width = width.max(1);
        let selection = self.selection().unwrap_or(0..0);
        let mut lines: Vec<Vec<(String, bool)>> = vec![Vec::new()];
        let mut used = 0;
        let (mut cursor_row, mut cursor_col) = (0, 0);
        for (i, &c) in self.chars.iter().enumerate() {
            if i == self.cursor {
                (cursor_row, cursor_col) = (lines.len() - 1, used);
            }
            if c == '\n' {
                lines.push(Vec::new());
                used = 0;
                continue;
            }
            let w = width_of(c);
            if used + w > width {
                lines.push(Vec::new());
                used = 0;
            }
            let selected = selection.contains(&i);
            if let Some(line) = lines.last_mut() {
                match line.last_mut() {
                    Some((run, is_selected)) if *is_selected == selected => run.push(c),
                    _ => line.push((c.to_string(), selected)),
                }
            }
            used += w;
        }
        if self.cursor == self.chars.len() {
            (cursor_row, cursor_col) = (lines.len() - 1, used);
        }
        (lines, cursor_row, cursor_col)
    }
}
