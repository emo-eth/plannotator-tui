//! The editing keys as crossterm delivers them: `ESC [1;3D` is Alt+Left, `ESC [1;5D`
//! Ctrl+Left, `ESC b` Alt+b, `ESC DEL` Alt+Backspace, `ESC [3;5~` Ctrl+Delete, 0x17
//! Ctrl+w, and kitty `CSI 97;9u` Cmd+a.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::TextBuffer;
use super::words::{next_boundary, previous_boundary};

fn at(text: &str, cursor: usize) -> TextBuffer {
    let mut buffer = TextBuffer::with_text(text);
    buffer.cursor = cursor;
    buffer
}

fn press(buffer: &mut TextBuffer, code: KeyCode, modifiers: KeyModifiers) {
    buffer.handle_key(KeyEvent::new(code, modifiers));
}

const ALT: KeyModifiers = KeyModifiers::ALT;
const CTRL: KeyModifiers = KeyModifiers::CONTROL;
const CMD: KeyModifiers = KeyModifiers::SUPER;
const SHIFT: KeyModifiers = KeyModifiers::SHIFT;
const NONE: KeyModifiers = KeyModifiers::NONE;

#[test]
fn word_jumps_stop_at_words_and_punctuation_and_skip_spaces() {
    let text = "hello, world! 123";
    assert_eq!([0, 5, 6, 12, 13, 17].map(|cursor| next_boundary(text, cursor)), [5, 6, 12, 13, 17, 17]);
    assert_eq!([17, 14, 12, 7, 5, 0].map(|cursor| previous_boundary(text, cursor)), [14, 12, 7, 5, 0, 0]);
}

#[test]
fn word_boundaries_follow_unicode_segmentation() {
    let text = "Café au lait — schön! 🚀 rocket";
    assert_eq!(next_boundary(text, 0), 4, "an accented word is one segment");
    assert_eq!(next_boundary(text, 12), 14, "an em dash is a word of its own");
    assert_eq!(next_boundary(text, 14), 20);
    assert_eq!(next_boundary(text, 21), 23, "so is an emoji");
    assert_eq!(previous_boundary(text, 30), 24);
    assert_eq!(previous_boundary(text, 24), 22);
    assert_eq!(previous_boundary(text, 20), 15);
    assert_eq!(next_boundary("cafe\u{301} x", 0), 5, "a combining mark stays with its letter");
    assert_eq!(next_boundary("don't stop", 0), 5, "an apostrophe does not split a word");
    assert_eq!(next_boundary("3.14 pi", 0), 4, "nor a decimal point a number");
    assert_eq!(next_boundary("日本語 text", 0), 3, "ideographs run together");
    assert_eq!(previous_boundary("a...b", 4), 1, "punctuation runs together");
}

#[test]
fn word_deletes_remove_to_the_boundary() {
    let mut buffer = at("foo bar baz", 11);
    for expected in ["foo bar ", "foo ", "", ""] {
        buffer.delete_word_back();
        assert_eq!(buffer.text(), expected);
    }
    let mut buffer = at("foo bar baz", 0);
    buffer.delete_word_forward();
    assert_eq!(buffer.text(), " bar baz");
    assert_eq!(buffer.cursor, 0);
}

#[test]
fn mac_and_linux_keys_jump_words_alike() {
    for (code, modifiers) in [(KeyCode::Left, ALT), (KeyCode::Left, CTRL), (KeyCode::Char('b'), ALT)] {
        let mut buffer = at("alpha beta gamma", 16);
        press(&mut buffer, code, modifiers);
        assert_eq!(buffer.cursor, 11, "{code:?} {modifiers:?}");
    }
    for (code, modifiers) in [(KeyCode::Right, ALT), (KeyCode::Right, CTRL), (KeyCode::Char('f'), ALT)] {
        let mut buffer = at("alpha beta gamma", 11);
        press(&mut buffer, code, modifiers);
        assert_eq!(buffer.cursor, 16, "{code:?} {modifiers:?}");
    }
}

#[test]
fn mac_linux_and_readline_keys_delete_words_alike() {
    for (code, modifiers) in
        [(KeyCode::Backspace, ALT), (KeyCode::Backspace, CTRL), (KeyCode::Char('w'), CTRL)]
    {
        let mut buffer = at("alpha beta gamma", 16);
        press(&mut buffer, code, modifiers);
        assert_eq!(buffer.text(), "alpha beta ", "{code:?} {modifiers:?}");
    }
    for (code, modifiers) in [(KeyCode::Delete, ALT), (KeyCode::Delete, CTRL), (KeyCode::Char('d'), ALT)] {
        let mut buffer = at("alpha beta gamma", 0);
        press(&mut buffer, code, modifiers);
        assert_eq!(buffer.text(), " beta gamma", "{code:?} {modifiers:?}");
    }
}

#[test]
fn cmd_arrows_home_end_and_readline_letters_reach_the_line_ends() {
    for (code, modifiers) in [(KeyCode::Left, CMD), (KeyCode::Home, NONE), (KeyCode::Char('a'), CTRL)] {
        let mut buffer = at("one two three\nfour five six", 18);
        press(&mut buffer, code, modifiers);
        assert_eq!(buffer.cursor, 14, "{code:?} {modifiers:?}");
    }
    for (code, modifiers) in [(KeyCode::Right, CMD), (KeyCode::End, NONE), (KeyCode::Char('e'), CTRL)] {
        let mut buffer = at("one two three\nfour five six", 18);
        press(&mut buffer, code, modifiers);
        assert_eq!(buffer.cursor, 27, "{code:?} {modifiers:?}");
    }
}

#[test]
fn kill_keys_cut_to_the_line_ends_and_join_lines_at_the_end() {
    for (code, modifiers) in [(KeyCode::Char('u'), CTRL), (KeyCode::Backspace, CMD)] {
        let mut buffer = at("first line\nsecond line\nthird line", 16);
        press(&mut buffer, code, modifiers);
        assert_eq!(buffer.text(), "first line\nd line\nthird line", "{code:?} {modifiers:?}");
        assert_eq!(buffer.cursor, 11);
    }
    for (code, modifiers) in [(KeyCode::Char('k'), CTRL), (KeyCode::Delete, CMD)] {
        let mut buffer = at("first line\nsecond line\nthird line", 16);
        press(&mut buffer, code, modifiers);
        assert_eq!(buffer.text(), "first line\nsecon\nthird line", "{code:?} {modifiers:?}");
        press(&mut buffer, code, modifiers);
        assert_eq!(buffer.text(), "first line\nseconthird line", "at the line end, the newline goes");
        assert_eq!(buffer.cursor, 16);
    }
}

#[test]
fn paragraph_keys_move_between_blank_lines() {
    for modifiers in [ALT, CTRL] {
        let mut buffer = at("para 1\nline 2\n\npara 2\nline 4", 0);
        press(&mut buffer, KeyCode::Down, modifiers);
        assert_eq!(buffer.cursor, 15, "{modifiers:?}");
        press(&mut buffer, KeyCode::Down, modifiers);
        assert_eq!(buffer.cursor, 28, "the last paragraph ends at the text's end");
        press(&mut buffer, KeyCode::Up, modifiers);
        assert_eq!(buffer.cursor, 15);
        press(&mut buffer, KeyCode::Up, modifiers);
        assert_eq!(buffer.cursor, 0);
    }
    let mut buffer = at("para 1\nline 2\n\npara 2\nline 4", 10);
    press(&mut buffer, KeyCode::Up, ALT);
    assert_eq!(buffer.cursor, 0, "from inside a paragraph, up goes to its start");
    let mut buffer = at("a\n\n\n\nb", 3);
    press(&mut buffer, KeyCode::Down, ALT);
    assert_eq!(buffer.cursor, 5, "from inside a blank run, down skips it");
}

#[test]
fn shift_extends_the_selection_by_char_word_line_and_paragraph() {
    let mut buffer = at("hello world\n\nnext", 0);
    press(&mut buffer, KeyCode::Right, SHIFT);
    assert_eq!(buffer.selection(), Some(0..1));
    press(&mut buffer, KeyCode::Right, SHIFT | ALT);
    assert_eq!(buffer.selection(), Some(0..5));
    press(&mut buffer, KeyCode::End, SHIFT);
    assert_eq!(buffer.selection(), Some(0..11));
    press(&mut buffer, KeyCode::Down, SHIFT | ALT);
    assert_eq!(buffer.selection(), Some(0..13));
    press(&mut buffer, KeyCode::Right, NONE);
    assert_eq!((buffer.selection(), buffer.cursor), (None, 13), "a plain move collapses to the far end");
    press(&mut buffer, KeyCode::Left, SHIFT);
    assert_eq!(buffer.selection(), Some(12..13));
    press(&mut buffer, KeyCode::Left, NONE);
    assert_eq!((buffer.selection(), buffer.cursor), (None, 12), "or to the near end");
    press(&mut buffer, KeyCode::Up, SHIFT);
    assert_eq!(buffer.selection(), Some(0..12));

    let mut buffer = at("hello world", 0);
    press(&mut buffer, KeyCode::Right, SHIFT | CTRL);
    assert_eq!(buffer.selection(), Some(0..5), "Ctrl extends by word too");
}

#[test]
fn typing_pasting_and_deleting_replace_the_selection() {
    let mut buffer = at("hello world", 0);
    press(&mut buffer, KeyCode::Right, SHIFT | ALT);
    press(&mut buffer, KeyCode::Char('H'), SHIFT);
    assert_eq!((buffer.text(), buffer.cursor, buffer.selection()), ("H world".to_owned(), 1, None));

    press(&mut buffer, KeyCode::Char('a'), CMD);
    assert_eq!(buffer.selection(), Some(0..7));
    buffer.insert_text("x\r\ny");
    assert_eq!((buffer.text(), buffer.cursor), ("x\ny".to_owned(), 3));

    press(&mut buffer, KeyCode::Left, SHIFT | CMD);
    press(&mut buffer, KeyCode::Backspace, NONE);
    assert_eq!(buffer.text(), "x\n");
    press(&mut buffer, KeyCode::Char('a'), CMD);
    press(&mut buffer, KeyCode::Char('k'), CTRL);
    assert_eq!(buffer.text(), "", "a kill removes just the selection");
}

#[test]
fn plain_moves_and_deletes_keep_their_old_behaviour() {
    let mut buffer = at("ab\ncd", 5);
    press(&mut buffer, KeyCode::Left, NONE);
    press(&mut buffer, KeyCode::Backspace, NONE);
    assert_eq!((buffer.text(), buffer.cursor), ("ab\nd".to_owned(), 3));
    press(&mut buffer, KeyCode::Delete, NONE);
    assert_eq!(buffer.text(), "ab\n");
    press(&mut buffer, KeyCode::Char('h'), CTRL);
    assert_eq!(buffer.text(), "ab", "Ctrl+h is a backspace");
    press(&mut buffer, KeyCode::Right, NONE);
    press(&mut buffer, KeyCode::Delete, NONE);
    assert_eq!((buffer.text(), buffer.cursor), ("ab".to_owned(), 2), "nothing past the end");
    press(&mut buffer, KeyCode::Char('x'), CMD);
    press(&mut buffer, KeyCode::Char('x'), ALT);
    press(&mut buffer, KeyCode::Char('x'), CTRL);
    assert_eq!(buffer.text(), "ab", "modified letters are keys, not text");
}

#[test]
fn up_and_down_land_on_the_nearest_column() {
    let mut buffer = at("abcd\nxy\nefgh", 3);
    press(&mut buffer, KeyCode::Down, NONE);
    assert_eq!(buffer.cursor, 7, "a short line ends early");
    press(&mut buffer, KeyCode::Down, NONE);
    assert_eq!(buffer.cursor, 10);
    press(&mut buffer, KeyCode::Up, NONE);
    assert_eq!(buffer.cursor, 7);
    press(&mut buffer, KeyCode::Up, NONE);
    assert_eq!(buffer.cursor, 2);
}

#[test]
fn wrapping_marks_the_selected_runs_and_places_the_cursor() {
    let mut buffer = at("hello world", 2);
    press(&mut buffer, KeyCode::Right, SHIFT | ALT);
    press(&mut buffer, KeyCode::Right, SHIFT);
    press(&mut buffer, KeyCode::Right, SHIFT);
    assert_eq!(buffer.selection(), Some(2..7));
    let (lines, row, col) = buffer.wrapped(6);
    assert_eq!(
        lines,
        [
            vec![("he".to_owned(), false), ("llo ".to_owned(), true)],
            vec![("w".to_owned(), true), ("orld".to_owned(), false)],
        ]
    );
    assert_eq!((row, col), (1, 1));
}

#[test]
fn wrapping_breaks_on_newlines_and_keeps_wide_chars_whole() {
    let buffer = at("ab日c\n", 5);
    let (lines, row, col) = buffer.wrapped(3);
    assert_eq!(lines, [vec![("ab".to_owned(), false)], vec![("日c".to_owned(), false)], vec![]]);
    assert_eq!((row, col), (2, 0));
}
