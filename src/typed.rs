//! What's typed in a box (a search, an address, a name): the line and its
//! cursor, edited a key at a time as a text field is, and drawn by the UI as
//! a block on the letter the cursor is before.
//!
//! The same in tuinews and tuitube.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation};

use crate::text;

/// Whether `key` types a character: one with neither Ctrl nor Alt, or AltGr
/// on Windows.
pub fn types(key: &KeyEvent) -> bool {
    types_on(key, cfg!(windows))
}

/// [`types`], on Windows or not.
fn types_on(key: &KeyEvent, windows: bool) -> bool {
    let either = KeyModifiers::CONTROL | KeyModifiers::ALT;
    matches!(key.code, KeyCode::Char(_))
        && (!key.modifiers.intersects(either) || altgr(key, windows))
}

/// Whether `key` is AltGr with a character: Ctrl and Alt both, as Windows
/// says it. Elsewhere those are Esc then a Ctrl key, as a terminal sends Alt
/// with one: Alt and Ctrl-h are Option-Backspace where Backspace is Ctrl-h.
fn altgr(key: &KeyEvent, windows: bool) -> bool {
    windows
        && key
            .modifiers
            .contains(KeyModifiers::CONTROL | KeyModifiers::ALT)
}

/// A line being typed, and where its cursor is: always between two letters
/// as drawn, since only [`Typed::edit`] and [`Typed::paste`] change them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Typed {
    text: String,
    /// Where typing goes: a byte offset in `text`.
    cursor: usize,
}

impl Typed {
    /// `text`, the cursor at its end.
    pub fn new(text: String) -> Self {
        let cursor = text.len();
        Self { text, cursor }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn into_text(self) -> String {
        self.text
    }

    /// Where typing goes: a byte offset in [`Typed::text`].
    pub fn at(&self) -> usize {
        self.cursor
    }

    /// Edits with `key`, at the cursor (moved as it goes). Backspace, or
    /// Ctrl-h, which some terminals send for it, deletes the letter before
    /// it (an accent written apart first, as an input method that retypes a
    /// letter expects), Delete the letter after it, Ctrl-w, Alt-Backspace or
    /// Ctrl-Backspace the word before it, and Ctrl-u all before it (what Cmd-Backspace
    /// sends). ← and → move it a letter; with Alt (or Alt-b and Alt-f, what
    /// macOS's Option-← and Option-→ send), a word; Home and End, or Ctrl-a
    /// and Ctrl-e (Cmd-← and Cmd-→), to either end; Ctrl with an arrow is
    /// the panes'. A character goes in at it ([`types`], not
    /// [`text::is_hidden`]) while the line holds fewer than `max`. Whether
    /// the line changed.
    pub fn edit(&mut self, key: &KeyEvent, max: usize) -> bool {
        self.edit_on(key, max, cfg!(windows))
    }

    /// [`Typed::edit`], on Windows or not.
    fn edit_on(&mut self, key: &KeyEvent, max: usize, windows: bool) -> bool {
        let altgr = altgr(key, windows);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL) && !altgr;
        let alt = key.modifiers.contains(KeyModifiers::ALT) && !altgr;
        let at = self.cursor;
        let backspace = key.code == KeyCode::Backspace || (ctrl && key.code == KeyCode::Char('h'));
        // What's deleted, and where the cursor goes.
        let (gone, cursor) = match key.code {
            _ if (backspace && alt)
                || (ctrl && matches!(key.code, KeyCode::Char('w') | KeyCode::Backspace)) =>
            {
                let start = self.word_before();
                (start..at, start)
            }
            _ if backspace => {
                // Accents written apart are Unicode's combining marks for
                // the Latin alphabet; an emoji's selectors and joiners go
                // with it.
                let start = match self.text[..at].chars().next_back() {
                    Some(c @ '\u{300}'..='\u{36F}') => at - c.len_utf8(),
                    _ => self.letter_before(),
                };
                (start..at, start)
            }
            KeyCode::Char('u') if ctrl => (0..at, 0),
            KeyCode::Char('a') if ctrl => (at..at, 0),
            KeyCode::Char('e') if ctrl => (at..at, self.text.len()),
            _ if ctrl => return false,
            KeyCode::Delete => (at..self.letter_after(), at),
            KeyCode::Left | KeyCode::Char('b') if alt => (at..at, self.word_before()),
            KeyCode::Right | KeyCode::Char('f') if alt => (at..at, self.word_after()),
            KeyCode::Left => (at..at, self.letter_before()),
            KeyCode::Right => (at..at, self.letter_after()),
            KeyCode::Home => (at..at, 0),
            KeyCode::End => (at..at, self.text.len()),
            KeyCode::Char(c)
                if types_on(key, windows)
                    && !text::is_hidden(c)
                    && self.text.chars().count() < max =>
            {
                self.text.insert(at, c);
                self.cursor = at + c.len_utf8();
                self.settle();
                return true;
            }
            _ => return false,
        };
        let changed = !gone.is_empty();
        self.text.replace_range(gone, "");
        self.cursor = cursor;
        self.settle();
        changed
    }

    /// The cursor after the letter it's in, when what went in or out made
    /// one letter of two around it (half a flag before the other half).
    fn settle(&mut self) {
        let mut letters = GraphemeCursor::new(self.cursor, self.text.len(), true);
        if matches!(letters.is_boundary(&self.text, 0), Ok(false))
            && let Ok(Some(end)) = letters.next_boundary(&self.text, 0)
        {
            self.cursor = end;
        }
    }

    /// Where the letter before the cursor starts.
    fn letter_before(&self) -> usize {
        let at = self.cursor;
        self.text[..at]
            .grapheme_indices(true)
            .next_back()
            .map_or(at, |(i, _)| i)
    }

    /// Where the letter after the cursor ends.
    fn letter_after(&self) -> usize {
        let at = self.cursor;
        self.text[at..]
            .graphemes(true)
            .next()
            .map_or(at, |g| at + g.len())
    }

    /// Where the word before the cursor starts.
    fn word_before(&self) -> usize {
        self.text[..self.cursor]
            .trim_end()
            .rfind(' ')
            .map_or(0, |i| i + 1)
    }

    /// Where the word after the cursor ends.
    fn word_after(&self) -> usize {
        let rest = &self.text[self.cursor..];
        let start = rest.len() - rest.trim_start().len();
        self.cursor + rest[start..].find(' ').map_or(rest.len(), |i| start + i)
    }

    /// Puts `pasted` (a line, cleaned) in at the cursor, as much as fits in a
    /// line of `max` characters, and the cursor after it. `spaced`: whether
    /// it had a space before it and after it, kept (one) where a word would
    /// run into it otherwise (one beside it, or after it what's typed next),
    /// if there's room for them as well as for it.
    pub fn paste(&mut self, pasted: &str, spaced: (bool, bool), max: usize) {
        if pasted.is_empty() {
            return;
        }
        let room = max.saturating_sub(self.text.chars().count());
        let n = pasted.chars().count();
        let (before, after) = self.text.split_at(self.cursor);
        let lead = spaced.0 && room > n && before.ends_with(|c: char| !c.is_whitespace());
        let trail =
            spaced.1 && room > n + usize::from(lead) && !after.starts_with(char::is_whitespace);
        let space = |kept: bool| if kept { " " } else { "" };
        let line = format!("{}{pasted}{}", space(lead), space(trail));
        let cut = text::first_chars(&line, room);
        self.text.insert_str(self.cursor, cut);
        self.cursor += cut.len();
        self.settle();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn with(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    /// What's after the cursor.
    fn after(typed: &Typed) -> &str {
        &typed.text()[typed.at()..]
    }

    #[test]
    fn keys_edit_where_the_cursor_is() {
        let mut line = Typed::new("ha noi".into());
        assert!(!line.edit(&key(KeyCode::Left), 50), "a move");
        line.edit(&key(KeyCode::Left), 50);
        assert!(line.edit(&key(KeyCode::Char('x')), 50));
        assert_eq!(line.text(), "ha nxoi");
        line.edit(&key(KeyCode::Backspace), 50);
        line.edit(&key(KeyCode::Delete), 50);
        assert_eq!(line.text(), "ha ni");
        line.edit(&key(KeyCode::Home), 50);
        assert_eq!(line.at(), 0);
        line.edit(&key(KeyCode::End), 50);
        line.edit(&with(KeyCode::Char('w'), KeyModifiers::CONTROL), 50);
        assert_eq!(line.text(), "ha ");
        // Full: nothing more goes in.
        assert!(!line.edit(&key(KeyCode::Char('y')), 3));
        assert_eq!(line.text(), "ha ");
    }

    #[test]
    fn a_letter_is_one_however_its_written() {
        // ← steps over an e and its accent written apart, Delete takes them
        // both; Backspace takes the accent first.
        let mut line = Typed::new("cafe\u{301}!".into());
        line.edit(&key(KeyCode::Left), 50);
        line.edit(&key(KeyCode::Left), 50);
        assert_eq!(after(&line), "e\u{301}!");
        line.edit(&key(KeyCode::Delete), 50);
        assert_eq!(line.text(), "caf!");
        let mut line = Typed::new("cafe\u{301}".into());
        line.edit(&key(KeyCode::Backspace), 50);
        assert_eq!(line.text(), "cafe");
        // An emoji goes whole, its selector and all; a flag is two
        // characters, one letter.
        for emoji in ["❤\u{fe0f}", "1\u{fe0f}\u{20e3}", "🇻🇳"] {
            let mut line = Typed::new(format!("a{emoji}"));
            line.edit(&key(KeyCode::Backspace), 50);
            assert_eq!(line.text(), "a", "{emoji}");
        }
        // Half a flag typed before a flag makes a flag of the first two: the
        // cursor goes after it, not into it.
        let mut flags = Typed::new("🇺🇸".into());
        flags.edit(&key(KeyCode::Home), 50);
        flags.edit(&key(KeyCode::Char('🇻')), 50);
        assert_eq!((flags.text(), after(&flags)), ("🇻🇺🇸", "🇸"));
    }

    #[test]
    fn option_and_cmd_keys_move_as_on_a_mac() {
        let alt = |code| with(code, KeyModifiers::ALT);
        let ctrl = |c| with(KeyCode::Char(c), KeyModifiers::CONTROL);
        let mut line = Typed::new("tin ha noi".into());
        line.edit(&alt(KeyCode::Char('b')), 50);
        assert_eq!(after(&line), "noi");
        line.edit(&alt(KeyCode::Left), 50);
        assert_eq!(after(&line), "ha noi");
        line.edit(&alt(KeyCode::Char('f')), 50);
        assert_eq!(after(&line), " noi");
        // An Alt letter isn't typed, nor does Ctrl-← move.
        line.edit(&alt(KeyCode::Char('x')), 50);
        line.edit(&with(KeyCode::Left, KeyModifiers::CONTROL), 50);
        assert_eq!((line.text(), after(&line)), ("tin ha noi", " noi"));
        // Cmd-← and Cmd-→ (Ctrl-a, Ctrl-e).
        line.edit(&ctrl('a'), 50);
        assert_eq!(line.at(), 0);
        line.edit(&ctrl('e'), 50);
        assert_eq!(after(&line), "");
        // Option-Backspace a word (Ctrl-Backspace too, as on Windows),
        // Cmd-Backspace (Ctrl-u) all before.
        line.edit(&alt(KeyCode::Backspace), 50);
        assert_eq!(line.text(), "tin ha ");
        let mut other = Typed::new("tin ha".into());
        other.edit(&with(KeyCode::Backspace, KeyModifiers::CONTROL), 50);
        assert_eq!(other.text(), "tin ");
        line.edit(&key(KeyCode::Left), 50);
        line.edit(&ctrl('u'), 50);
        assert_eq!((line.text(), line.at()), (" ", 0));
    }

    #[test]
    fn ctrl_and_alt_are_altgr_on_windows_only() {
        let both = KeyModifiers::CONTROL | KeyModifiers::ALT;
        let mut line = Typed::new("z".into());
        assert!(line.edit_on(&with(KeyCode::Char('ą'), both), 50, true));
        assert_eq!(line.text(), "zą");
        assert!(types_on(&with(KeyCode::Char('@'), both), true));
        // Elsewhere they're Esc then a Ctrl key: with Ctrl-h, Alt-Backspace.
        assert!(!types_on(&with(KeyCode::Char('@'), both), false));
        let mut line = Typed::new("tin ha".into());
        line.edit_on(&with(KeyCode::Char('h'), both), 50, false);
        assert_eq!(line.text(), "tin ");
        assert!(!types(&with(KeyCode::Char('a'), KeyModifiers::CONTROL)));
        assert!(!types(&with(KeyCode::Char('a'), KeyModifiers::ALT)));
        assert!(types(&with(KeyCode::Char('A'), KeyModifiers::SHIFT)));
    }

    #[test]
    fn a_paste_goes_in_at_the_cursor_apart_from_the_words_beside_it() {
        let mut line = Typed::new("Hàmưa".into());
        line.edit(&key(KeyCode::Left), 50);
        line.edit(&key(KeyCode::Left), 50);
        line.edit(&key(KeyCode::Left), 50);
        line.paste("Nội", (true, true), 50);
        assert_eq!(line.text(), "Hà Nội mưa");
        assert_eq!(after(&line), "mưa");
        // No space twice, none at the line's start; one it ended with is
        // kept at the end, for what's typed next (keys come in two runs).
        let mut line = Typed::new("tin ".into());
        line.paste("ha", (true, true), 50);
        line.paste("noi", (false, false), 50);
        assert_eq!(line.text(), "tin ha noi");
        let mut line = Typed::default();
        line.paste("ha", (true, false), 50);
        assert_eq!(line.text(), "ha");
        // As much as fits, the spaces given up first.
        let mut line = Typed::new("ab".into());
        line.paste("cdef", (false, false), 4);
        assert_eq!(line.text(), "abcd");
        let mut line = Typed::new("ab".into());
        line.paste("x", (true, true), 3);
        assert_eq!(line.text(), "abx");
    }
}
