//! [`TextBox`]: the text of a text box and its cursor, with readline keys.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;

/// The text of a text box and the cursor in it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextBox {
    text: String,
    /// The byte offset of the cursor. A move or a delete leaves it on a
    /// grapheme boundary.
    cursor: usize,
    /// The text that the last run of cuts removed, for Ctrl+Y.
    cut: String,
    /// The text and the cursor before each change, for Ctrl+_.
    undo: Vec<(String, usize)>,
    /// The kind of the last edit.
    last: Last,
}

/// The kind of an edit that the next edit can join.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Last {
    #[default]
    Other,
    /// A typed character. A run of them is one undo step.
    Insert,
    /// A cut. A run of them makes one text for Ctrl+Y.
    Cut,
}

/// One edit of a text box.
#[derive(Debug, Clone, Copy)]
enum Edit {
    Move(usize),
    Delete(usize),
    Cut(usize),
    Paste,
    Undo,
    Insert(char),
}

impl From<String> for TextBox {
    /// A text box with the cursor at the end of `text`.
    fn from(text: String) -> Self {
        Self {
            cursor: text.len(),
            text,
            ..Self::new()
        }
    }
}

impl From<&str> for TextBox {
    fn from(text: &str) -> Self {
        Self::from(text.to_string())
    }
}

impl From<Box<str>> for TextBox {
    fn from(text: Box<str>) -> Self {
        Self::from(String::from(text))
    }
}

/// `true` if `grapheme` is part of a word for Alt+B, Alt+F, Alt+D, and
/// Alt+Backspace.
fn is_word(grapheme: &str) -> bool {
    grapheme.chars().next().is_some_and(char::is_alphanumeric)
}

/// `true` if `grapheme` is part of a word for Ctrl+W.
fn is_not_space(grapheme: &str) -> bool {
    !grapheme.chars().next().is_some_and(char::is_whitespace)
}

impl TextBox {
    /// An empty text box.
    pub const fn new() -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            cut: String::new(),
            undo: Vec::new(),
            last: Last::Other,
        }
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub const fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The text before the cursor.
    pub fn before_cursor(&self) -> &str {
        self.text.get(..self.cursor).unwrap_or_default()
    }

    /// The text after the cursor.
    fn after_cursor(&self) -> &str {
        self.text.get(self.cursor..).unwrap_or_default()
    }

    /// Take the text, and leave the box empty. The cut text stays.
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        self.undo.clear();
        self.last = Last::Other;
        std::mem::take(&mut self.text)
    }

    /// Type `c` at the cursor.
    pub fn insert(&mut self, c: char) {
        self.apply(Edit::Insert(c));
    }

    /// Type or edit with `key`. Returns `false` for a key that is not a
    /// text key.
    pub fn edit(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let only_ctrl = ctrl && !alt;
        let only_alt = alt && !ctrl;
        let edit = match key.code {
            KeyCode::Backspace if ctrl || alt => Edit::Cut(self.word_start(is_word)),
            KeyCode::Char('h') if only_ctrl => Edit::Delete(self.previous()),
            KeyCode::Backspace => Edit::Delete(self.previous()),
            KeyCode::Delete => Edit::Delete(self.next()),
            KeyCode::Char('d') if only_ctrl => Edit::Delete(self.next()),
            KeyCode::Char('d') if only_alt => Edit::Cut(self.word_end(is_word)),
            KeyCode::Char('w') if only_ctrl => Edit::Cut(self.word_start(is_not_space)),
            KeyCode::Char('u') if only_ctrl => Edit::Cut(self.line_start()),
            KeyCode::Char('k') if only_ctrl => Edit::Cut(self.line_end_or_next()),
            KeyCode::Char('y') if only_ctrl => Edit::Paste,
            // A terminal sends Ctrl+_ as Ctrl+7 or as Ctrl+Shift+-.
            KeyCode::Char('_' | '7') if only_ctrl => Edit::Undo,
            KeyCode::Char('-') if only_ctrl && key.modifiers.contains(KeyModifiers::SHIFT) => {
                Edit::Undo
            }
            KeyCode::Left if ctrl || alt => Edit::Move(self.word_start(is_word)),
            KeyCode::Right if ctrl || alt => Edit::Move(self.word_end(is_word)),
            KeyCode::Char('b') if only_alt => Edit::Move(self.word_start(is_word)),
            KeyCode::Char('f') if only_alt => Edit::Move(self.word_end(is_word)),
            KeyCode::Left => Edit::Move(self.previous()),
            KeyCode::Char('b') if only_ctrl => Edit::Move(self.previous()),
            KeyCode::Right => Edit::Move(self.next()),
            KeyCode::Char('f') if only_ctrl => Edit::Move(self.next()),
            KeyCode::Home => Edit::Move(self.line_start()),
            KeyCode::Char('a') if only_ctrl => Edit::Move(self.line_start()),
            KeyCode::End => Edit::Move(self.line_end()),
            KeyCode::Char('e') if only_ctrl => Edit::Move(self.line_end()),
            // AltGr on Windows sends Ctrl and Alt, and gives no ASCII letter or digit.
            KeyCode::Char(c) if !ctrl && !alt || ctrl && alt && !c.is_ascii_alphanumeric() => {
                Edit::Insert(c)
            }
            _ => return false,
        };
        self.apply(edit);
        true
    }

    /// Do `edit`, and record it for undo and for the cut text.
    fn apply(&mut self, edit: Edit) {
        let last = std::mem::take(&mut self.last);
        match edit {
            Edit::Move(to) => self.cursor = to,
            Edit::Delete(to) => {
                self.delete_to(to);
            }
            Edit::Cut(to) => {
                let back = to < self.cursor;
                let deleted = self.delete_to(to);
                if last != Last::Cut {
                    self.cut.clear();
                }
                if back {
                    self.cut.insert_str(0, &deleted);
                } else {
                    self.cut.push_str(&deleted);
                }
                self.last = Last::Cut;
            }
            Edit::Paste if !self.cut.is_empty() => {
                self.save();
                self.text.insert_str(self.cursor, &self.cut);
                self.cursor = self.cursor.saturating_add(self.cut.len());
            }
            Edit::Paste => {}
            Edit::Undo => {
                if let Some((text, cursor)) = self.undo.pop() {
                    self.text = text;
                    self.cursor = cursor;
                }
            }
            Edit::Insert(c) => {
                if last != Last::Insert {
                    self.save();
                }
                self.text.insert(self.cursor, c);
                self.cursor = self.cursor.saturating_add(c.len_utf8());
                self.last = Last::Insert;
            }
        }
    }

    /// Keep the text and the cursor for undo.
    fn save(&mut self) {
        self.undo.push((self.text.clone(), self.cursor));
    }

    /// Delete the text between the cursor and `to`, and put the cursor at
    /// the start of the grapheme that holds the start of the deleted text.
    /// Returns the deleted text.
    fn delete_to(&mut self, to: usize) -> String {
        let start = to.min(self.cursor);
        let end = to.max(self.cursor);
        if start < end {
            self.save();
        }
        let deleted = self.text.drain(start..end).collect();
        self.cursor = self
            .text
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .take_while(|&i| i <= start)
            .last()
            .filter(|_| start < self.text.len())
            .unwrap_or(start);
        deleted
    }

    /// The start of the grapheme before the cursor.
    fn previous(&self) -> usize {
        self.before_cursor()
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    /// The end of the grapheme after the cursor.
    fn next(&self) -> usize {
        self.after_cursor()
            .graphemes(true)
            .next()
            .map_or(self.cursor, |g| self.cursor + g.len())
    }

    /// The start of the word before the cursor. A word is a run of
    /// graphemes for which `in_word` is `true`.
    fn word_start(&self, in_word: fn(&str) -> bool) -> usize {
        let mut start = self.cursor;
        let mut seen = false;
        for (i, g) in self.before_cursor().grapheme_indices(true).rev() {
            if in_word(g) {
                seen = true;
            } else if seen {
                break;
            }
            start = i;
        }
        start
    }

    /// The end of the word after the cursor.
    fn word_end(&self, in_word: fn(&str) -> bool) -> usize {
        let mut end = self.cursor;
        let mut seen = false;
        for g in self.after_cursor().graphemes(true) {
            if in_word(g) {
                seen = true;
            } else if seen {
                break;
            }
            end += g.len();
        }
        end
    }

    /// The start of the line that holds the cursor.
    fn line_start(&self) -> usize {
        self.before_cursor().rfind('\n').map_or(0, |i| i + 1)
    }

    /// The end of the line that holds the cursor.
    fn line_end(&self) -> usize {
        self.after_cursor()
            .find('\n')
            .map_or(self.text.len(), |i| self.cursor + i)
    }

    /// The end of the line, or the line break after it when the cursor is
    /// at the end.
    fn line_end_or_next(&self) -> usize {
        match self.line_end() {
            end if end == self.cursor => self.next(),
            end => end,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn alt(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT)
    }

    /// A text box with `text`, and the cursor where `|` is.
    fn at(text: &str) -> TextBox {
        let cursor = text.find('|').unwrap();
        TextBox {
            text: text.replacen('|', "", 1),
            cursor,
            ..TextBox::new()
        }
    }

    /// The text of `text_box`, with `|` at the cursor.
    fn shown(text_box: &TextBox) -> String {
        let mut text = text_box.text.clone();
        text.insert(text_box.cursor, '|');
        text
    }

    fn after(text: &str, keys: &[KeyEvent]) -> String {
        let mut text_box = at(text);
        for &k in keys {
            assert!(text_box.edit(k), "{k:?}");
        }
        shown(&text_box)
    }

    #[test]
    fn typing_inserts_at_the_cursor() {
        assert_eq!(
            after("ac|", &[key(KeyCode::Left), key(KeyCode::Char('b'))]),
            "ab|c"
        );
    }

    #[test]
    fn left_and_backspace_step_over_a_whole_grapheme() {
        // An e with a combining acute accent, then a flag of two code points.
        let text = "ae\u{301}\u{1f1eb}\u{1f1f7}|";
        assert_eq!(
            after(text, &[key(KeyCode::Left)]),
            "ae\u{301}|\u{1f1eb}\u{1f1f7}"
        );
        assert_eq!(after(text, &[key(KeyCode::Backspace)]), "ae\u{301}|");
        let back = [key(KeyCode::Left), key(KeyCode::Left), key(KeyCode::Delete)];
        assert_eq!(after(text, &back), "a|\u{1f1eb}\u{1f1f7}");
    }

    #[test]
    fn home_end_ctrl_a_and_ctrl_e_stay_on_the_line() {
        assert_eq!(after("ab\ncd|e\nf", &[key(KeyCode::Home)]), "ab\n|cde\nf");
        assert_eq!(after("ab\nc|de\nf", &[ctrl('e')]), "ab\ncde|\nf");
        assert_eq!(after("ab|", &[ctrl('a')]), "|ab");
        assert_eq!(after("|ab", &[key(KeyCode::End)]), "ab|");
    }

    #[test]
    fn ctrl_b_and_ctrl_f_move_one_grapheme() {
        assert_eq!(after("ab|", &[ctrl('b'), ctrl('b'), ctrl('f')]), "a|b");
        assert_eq!(after("|a", &[ctrl('b')]), "|a");
        assert_eq!(after("a|", &[ctrl('f')]), "a|");
    }

    #[test]
    fn alt_b_and_alt_f_move_by_word() {
        assert_eq!(after("one, two|", &[alt('b')]), "one, |two");
        assert_eq!(after("one, |two", &[alt('b')]), "|one, two");
        assert_eq!(after("|one, two", &[alt('f')]), "one|, two");
        assert_eq!(after("one|, two", &[alt('f')]), "one, two|");
        let ctrl_left = KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL);
        assert_eq!(after("one two|", &[ctrl_left]), "one |two");
    }

    #[test]
    fn ctrl_w_deletes_to_a_space_and_alt_backspace_to_a_word() {
        assert_eq!(after("cd a/b.c|", &[ctrl('w')]), "cd |");
        let alt_backspace = KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT);
        assert_eq!(after("cd a/b.c|", &[alt_backspace]), "cd a/b.|");
        assert_eq!(after("ab c |d", &[ctrl('w')]), "ab |d");
    }

    #[test]
    fn alt_d_and_ctrl_d_delete_forward() {
        assert_eq!(after("a| bc d", &[alt('d')]), "a| d");
        assert_eq!(after("a|bc", &[ctrl('d')]), "a|c");
        assert_eq!(after("ab|", &[ctrl('d')]), "ab|");
    }

    #[test]
    fn ctrl_u_and_ctrl_k_delete_to_the_line_ends() {
        assert_eq!(after("ab\ncd|ef", &[ctrl('u')]), "ab\n|ef");
        assert_eq!(after("ab\ncd|ef", &[ctrl('k')]), "ab\ncd|");
        assert_eq!(after("ab\n|cd", &[ctrl('u')]), "ab\n|cd");
        assert_eq!(after("ab|\ncd", &[ctrl('k')]), "ab|cd");
    }

    #[test]
    fn ctrl_y_pastes_the_last_run_of_cuts() {
        assert_eq!(
            after("ab cd|", &[ctrl('w'), ctrl('w'), ctrl('y')]),
            "ab cd|"
        );
        assert_eq!(
            after("a|b\ncd", &[ctrl('k'), ctrl('k'), ctrl('y')]),
            "ab\n|cd"
        );
        let left = key(KeyCode::Left);
        assert_eq!(
            after("ab c|", &[ctrl('w'), left, ctrl('w'), ctrl('y')]),
            "ab| "
        );
        assert_eq!(after("ab|", &[ctrl('y')]), "ab|");
    }

    #[test]
    fn delete_does_not_change_the_cut_text() {
        let keys = [ctrl('w'), key(KeyCode::Backspace), ctrl('y'), ctrl('y')];
        assert_eq!(after("a bc|", &keys), "abcbc|");
    }

    #[test]
    fn ctrl_underscore_undoes_a_run_of_typing_then_each_edit() {
        let undo = ctrl('_');
        let mut text_box = at("a|");
        for c in "bc".chars() {
            text_box.edit(key(KeyCode::Char(c)));
        }
        text_box.edit(key(KeyCode::Backspace));
        text_box.edit(undo);
        assert_eq!(shown(&text_box), "abc|");
        text_box.edit(undo);
        assert_eq!(shown(&text_box), "a|");
        text_box.edit(undo);
        assert_eq!(shown(&text_box), "a|");
        let ctrl_7 = ctrl('7');
        let ctrl_shift_minus = KeyEvent::new(
            KeyCode::Char('-'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        assert_eq!(after("ab|", &[ctrl('w'), ctrl_7]), "ab|");
        assert_eq!(after("ab|", &[ctrl('u'), ctrl_shift_minus]), "ab|");
    }

    #[test]
    fn a_move_ends_a_run_of_typing() {
        let keys = [
            key(KeyCode::Char('b')),
            key(KeyCode::Left),
            key(KeyCode::Char('c')),
            ctrl('_'),
        ];
        assert_eq!(after("a|", &keys), "a|b");
    }

    #[test]
    fn a_delete_that_joins_two_graphemes_moves_to_their_start() {
        assert_eq!(after("🇫| 🇷🇩", &[key(KeyCode::Delete)]), "|🇫🇷🇩");
        assert_eq!(after("🇫 |🇷🇩", &[key(KeyCode::Backspace)]), "|🇫🇷🇩");
    }

    #[test]
    fn ctrl_d_and_ctrl_k_do_nothing_in_an_empty_box() {
        assert_eq!(after("|", &[ctrl('d'), ctrl('k')]), "|");
    }

    #[test]
    fn a_key_that_does_not_edit_returns_false() {
        let mut text_box = TextBox::from("a");
        assert!(!text_box.edit(key(KeyCode::Enter)));
        assert!(!text_box.edit(ctrl('p')));
        assert!(!text_box.edit(alt('a')));
        assert_eq!(text_box.before_cursor(), "a");
    }

    #[test]
    fn take_empties_the_box() {
        let mut text_box = TextBox::from("abc");
        assert_eq!(text_box.take(), "abc");
        assert!(text_box.is_empty());
        text_box.insert('x');
        assert_eq!(text_box.as_str(), "x");
    }
}
