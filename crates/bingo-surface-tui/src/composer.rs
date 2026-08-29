//! The prompt editor: a `String`, a grapheme-aligned cursor, and the motions a
//! readline user expects. It owns no terminal, so every one of its rules is
//! testable as a function of text and offset.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Where the editor's text sits on screen once it is folded into a box.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub lines: Vec<String>,
    /// Row and column of the caret within `lines`.
    pub cursor: (usize, usize),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Composer {
    text: String,
    /// Byte offset, always on a grapheme boundary.
    cursor: usize,
}

impl Composer {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    /// Replace the whole buffer and park the caret at the end (history recall).
    pub fn set(&mut self, text: &str) {
        self.text = text.to_string();
        self.cursor = self.text.len();
    }

    /// Insert verbatim: a bracketed paste keeps its newlines and its spaces.
    pub fn insert(&mut self, text: &str) {
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    pub fn newline(&mut self) {
        self.insert("\n");
    }

    pub fn backspace(&mut self) {
        if let Some(start) = self.prev() {
            self.text.replace_range(start..self.cursor, "");
            self.cursor = start;
        }
    }

    pub fn delete(&mut self) {
        if let Some(end) = self.next() {
            self.text.replace_range(self.cursor..end, "");
        }
    }

    pub fn left(&mut self) {
        if let Some(start) = self.prev() {
            self.cursor = start;
        }
    }

    pub fn right(&mut self) {
        if let Some(end) = self.next() {
            self.cursor = end;
        }
    }

    pub fn home(&mut self) {
        self.cursor = self.line_start();
    }

    pub fn end(&mut self) {
        self.cursor = self.line_end();
    }

    pub fn word_left(&mut self) {
        self.cursor = self.word_start();
    }

    pub fn word_right(&mut self) {
        self.cursor = self.word_end();
    }

    pub fn delete_word_left(&mut self) {
        let start = self.word_start();
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    pub fn delete_to_line_start(&mut self) {
        let start = self.line_start();
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    pub fn delete_to_line_end(&mut self) {
        let end = self.line_end();
        self.text.replace_range(self.cursor..end, "");
    }

    /// Move one logical line up. `false` when there is none, which is what
    /// lets the caller walk the prompt history instead.
    pub fn up(&mut self) -> bool {
        let start = self.line_start();
        if start == 0 {
            return false;
        }
        let column = self.column();
        let previous = self.text[..start - 1].rfind('\n').map_or(0, |i| i + 1);
        self.cursor = seek(&self.text, previous, start - 1, column);
        true
    }

    pub fn down(&mut self) -> bool {
        let end = self.line_end();
        if end == self.text.len() {
            return false;
        }
        let column = self.column();
        let start = end + 1;
        let next_end = self.text[start..]
            .find('\n')
            .map_or(self.text.len(), |i| start + i);
        self.cursor = seek(&self.text, start, next_end, column);
        true
    }

    /// The wrapped rows and the caret cell, for a box `width` columns wide.
    pub fn layout(&self, width: usize) -> Layout {
        let width = width.max(1);
        let mut lines = vec![String::new()];
        let mut column = 0usize;
        let mut cursor = (0usize, 0usize);
        for (offset, grapheme) in self.text.grapheme_indices(true) {
            if offset == self.cursor {
                cursor = (lines.len() - 1, column);
            }
            if grapheme == "\n" {
                lines.push(String::new());
                column = 0;
                continue;
            }
            if column + grapheme.width() > width {
                lines.push(String::new());
                column = 0;
            }
            if let Some(last) = lines.last_mut() {
                last.push_str(grapheme);
            }
            column += grapheme.width();
        }
        if self.cursor >= self.text.len() {
            cursor = (lines.len() - 1, column);
        }
        if cursor.1 >= width {
            cursor = (cursor.0 + 1, 0);
            if lines.len() <= cursor.0 {
                lines.push(String::new());
            }
        }
        Layout { lines, cursor }
    }

    fn prev(&self) -> Option<usize> {
        self.text[..self.cursor]
            .graphemes(true)
            .next_back()
            .map(|g| self.cursor - g.len())
    }

    fn next(&self) -> Option<usize> {
        self.text[self.cursor..]
            .graphemes(true)
            .next()
            .map(|g| self.cursor + g.len())
    }

    fn line_start(&self) -> usize {
        self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self) -> usize {
        self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |i| self.cursor + i)
    }

    /// The caret's grapheme offset within its logical line.
    fn column(&self) -> usize {
        self.text[self.line_start()..self.cursor]
            .graphemes(true)
            .count()
    }

    /// The start of the word before the caret: whitespace, then the word.
    fn word_start(&self) -> usize {
        let head = &self.text[..self.cursor];
        let trimmed = head.trim_end();
        match trimmed.rfind(char::is_whitespace) {
            Some(i) => i + 1,
            None => 0,
        }
    }

    fn word_end(&self) -> usize {
        let tail = &self.text[self.cursor..];
        let skipped = tail.len() - tail.trim_start().len();
        match tail[skipped..].find(char::is_whitespace) {
            Some(i) => self.cursor + skipped + i,
            None => self.text.len(),
        }
    }
}

/// The offset `column` graphemes into `start..end`, clamped to the line's end.
fn seek(text: &str, start: usize, end: usize, column: usize) -> usize {
    text[start..end]
        .grapheme_indices(true)
        .nth(column)
        .map_or(end, |(i, _)| start + i)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str, cursor: usize) -> Composer {
        Composer {
            text: text.into(),
            cursor,
        }
    }

    #[test]
    fn insert_and_backspace_move_by_grapheme() {
        let mut c = Composer::default();
        c.insert("héllo");
        c.backspace();
        assert_eq!(c.text(), "héll");
        c.left();
        c.left();
        c.backspace();
        assert_eq!(c.text(), "hll", "the two-byte é is one backspace");
    }

    #[test]
    fn a_paste_lands_verbatim_at_the_caret() {
        let mut c = at("ab", 1);
        c.insert("x\ny");
        assert_eq!(c.text(), "ax\nyb");
    }

    #[test]
    fn home_and_end_are_line_wise() {
        let mut c = at("one\ntwo", 5);
        c.home();
        assert_eq!(c.cursor, 4);
        c.end();
        assert_eq!(c.cursor, 7);
    }

    #[test]
    fn word_motions_stop_at_whitespace() {
        let mut c = at("alpha beta gamma", 16);
        c.word_left();
        assert_eq!(c.cursor, 11);
        c.word_left();
        assert_eq!(c.cursor, 6);
        c.word_right();
        assert_eq!(c.cursor, 10);
    }

    #[test]
    fn the_three_kill_chords_cut_word_line_start_and_line_end() {
        let mut c = at("alpha beta", 10);
        c.delete_word_left();
        assert_eq!(c.text(), "alpha ");

        let mut c = at("one\ntwo three", 8);
        c.delete_to_line_start();
        assert_eq!(c.text(), "one\nthree");

        let mut c = at("one\ntwo three", 8);
        c.delete_to_line_end();
        assert_eq!(c.text(), "one\ntwo ");
    }

    #[test]
    fn vertical_motion_reports_when_there_is_no_line_to_go_to() {
        let mut c = at("one\ntwo", 1);
        assert!(!c.up(), "the first line has nothing above it");
        assert!(c.down());
        assert_eq!(c.cursor, 5, "the column is kept");
        assert!(!c.down(), "the last line has nothing below it");
    }

    #[test]
    fn a_short_line_clamps_the_kept_column() {
        let mut c = at("ab\nlonger", 9);
        assert!(c.up());
        assert_eq!(c.cursor, 2, "column 6 does not exist on a two-column line");
    }

    #[test]
    fn layout_wraps_at_the_box_width_and_places_the_caret() {
        let c = at("abcdef", 4);
        assert_eq!(
            c.layout(3),
            Layout {
                lines: vec!["abc".into(), "def".into()],
                cursor: (1, 1),
            }
        );
    }

    #[test]
    fn layout_keeps_explicit_newlines() {
        let c = at("ab\ncd", 5);
        assert_eq!(
            c.layout(10),
            Layout {
                lines: vec!["ab".into(), "cd".into()],
                cursor: (1, 2),
            }
        );
    }

    #[test]
    fn a_caret_at_the_right_edge_shows_on_the_next_row() {
        let c = at("abc", 3);
        assert_eq!(c.layout(3).cursor, (1, 0));
    }
}
