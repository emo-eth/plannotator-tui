//! Word boundaries for word jumps and deletes, over Unicode (UAX #29) segments: a word is a
//! run of alphanumeric segments or a run of punctuation ones, and whitespace is skipped.
//! Positions are char indices, as the buffer's cursor is.

use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Space,
    Word,
    Punctuation,
}

impl Class {
    fn of(segment: &str) -> Self {
        if segment.chars().all(char::is_whitespace) {
            Self::Space
        } else if segment.chars().any(|c| c.is_alphanumeric() || c == '_') {
            Self::Word
        } else {
            Self::Punctuation
        }
    }
}

struct Run {
    start: usize,
    end: usize,
    class: Class,
}

/// Maximal runs of one class, in order.
fn runs(text: &str) -> impl Iterator<Item = Run> + '_ {
    let mut segments = text.split_word_bounds().peekable();
    let mut start = 0;
    std::iter::from_fn(move || {
        let first = segments.next()?;
        let class = Class::of(first);
        let mut end = start + first.chars().count();
        while let Some(more) = segments.next_if(|segment| Class::of(segment) == class) {
            end += more.chars().count();
        }
        let run = Run { start, end, class };
        start = end;
        Some(run)
    })
}

/// The end of the word the cursor is in or before; the text's end when there is none.
pub(super) fn next_boundary(text: &str, cursor: usize) -> usize {
    runs(text)
        .find(|run| run.end > cursor && run.class != Class::Space)
        .map_or_else(|| text.chars().count(), |run| run.end)
}

/// The start of the word the cursor is in or after; 0 when there is none.
pub(super) fn previous_boundary(text: &str, cursor: usize) -> usize {
    runs(text)
        .take_while(|run| run.start < cursor)
        .filter(|run| run.class != Class::Space)
        .last()
        .map_or(0, |run| run.start)
}
