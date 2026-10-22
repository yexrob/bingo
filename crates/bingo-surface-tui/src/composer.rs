//! The prompt editor and its rows, independent of terminal geometry.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::pictures;

pub const CARET_COLUMNS: usize = 1;
pub const COLUMN: usize = crate::layout::grid::TX;
pub const CARET_REST: std::time::Duration = std::time::Duration::from_secs(10);

pub fn width(width: usize) -> usize {
    width
        .saturating_sub(COLUMN + crate::layout::grid::RM + CARET_COLUMNS)
        .max(1)
}

pub mod fill;

/// Where the editor's text sits on screen once it is folded into a box.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub lines: Vec<String>,
    pub images: Vec<u32>,
    /// The terminal columns selected in each visual line.
    pub selection: Vec<Option<std::ops::Range<usize>>>,
    /// Row and column of the caret within `lines`.
    pub cursor: (usize, usize),
}

impl Layout {
    pub fn images(mut self, held: &crate::pictures::Held, text: &str) -> Self {
        self.images = held.shown(text).into_iter().map(|(n, _)| n).collect();
        self
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Composer {
    text: String,
    /// Byte offset, always on a grapheme boundary.
    cursor: usize,
    /// The fixed end of a composer-local selection, when one is active.
    anchor: Option<usize>,
    /// A pointer press in the composer is still held down.
    dragging: bool,
}

impl Composer {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    #[cfg(test)]
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.clear_selection();
    }

    pub fn take(&mut self) -> String {
        self.cursor = 0;
        self.clear_selection();
        std::mem::take(&mut self.text)
    }

    /// Replace the whole buffer and park the caret at the end (history recall).
    pub fn set(&mut self, text: &str) {
        self.text = text.to_string();
        self.cursor = self.text.len();
        self.clear_selection();
    }

    /// Insert verbatim: a bracketed paste keeps its newlines and its spaces.
    pub fn insert(&mut self, text: &str) {
        self.delete_selection();
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    pub fn selected_text(&self) -> Option<&str> {
        let range = self.selection_range()?;
        (!range.is_empty()).then(|| &self.text[range])
    }

    pub fn clear_selection(&mut self) {
        self.anchor = None;
        self.dragging = false;
    }

    pub fn pointer_press(&mut self, offset: usize) {
        self.cursor = offset;
        self.anchor = Some(offset);
        self.dragging = true;
    }

    pub fn pointer_drag(&mut self, offset: usize) {
        if self.dragging {
            self.cursor = offset;
        }
    }

    pub fn pointer_release(&mut self) {
        self.dragging = false;
        if self.selected_text().is_none() {
            self.anchor = None;
        }
    }

    pub fn pointer_dragging(&self) -> bool {
        self.dragging
    }

    /// The byte boundary at a visual row and terminal column.
    pub fn offset_at(&self, width: usize, row: usize, column: usize) -> Option<usize> {
        let lines = self.visual_lines(width);
        let line = lines.get(row)?;
        Some(offset_in_line(&line.text, line.start, column))
    }

    pub fn picture(&mut self, number: u32, held: &pictures::Held) {
        self.delete_selection();
        let mut end = 0;
        for (range, token) in pictures::spans(&self.text) {
            if range.start != end || held.under(token).is_none() {
                break;
            }
            end = range.end;
            if self.text[end..].starts_with(' ') {
                end += 1;
            }
        }
        let chip = format!("{} ", pictures::placeholder(number));
        self.text.insert_str(end, &chip);
        self.cursor = self.cursor.max(end) + chip.len();
    }

    pub fn newline(&mut self) {
        self.insert("\n");
    }

    pub fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        if let Some(start) = self.prev() {
            self.text.replace_range(start..self.cursor, "");
            self.cursor = start;
        }
    }

    pub fn delete(&mut self) {
        if self.delete_selection() {
            return;
        }
        if let Some(end) = self.next() {
            self.text.replace_range(self.cursor..end, "");
        }
    }

    pub fn left(&mut self) {
        self.clear_selection();
        self.move_left();
    }

    pub fn right(&mut self) {
        self.clear_selection();
        self.move_right();
    }

    pub fn home(&mut self) {
        self.clear_selection();
        self.move_home();
    }

    pub fn end(&mut self) {
        self.clear_selection();
        self.move_end();
    }

    pub fn select_left(&mut self) {
        self.begin_selection();
        self.move_left();
    }

    pub fn select_right(&mut self) {
        self.begin_selection();
        self.move_right();
    }

    pub fn select_home(&mut self) {
        self.begin_selection();
        self.move_home();
    }

    pub fn select_end(&mut self) {
        self.begin_selection();
        self.move_end();
    }

    pub fn word_left(&mut self) {
        self.clear_selection();
        self.cursor = self.word_start();
    }

    pub fn word_right(&mut self) {
        self.clear_selection();
        self.cursor = self.word_end();
    }

    pub fn delete_word_left(&mut self) {
        if self.delete_selection() {
            return;
        }
        let start = self.word_start();
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    pub fn delete_to_line_start(&mut self) {
        if self.delete_selection() {
            return;
        }
        let start = self.line_start();
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    pub fn delete_to_line_end(&mut self) {
        if self.delete_selection() {
            return;
        }
        let end = self.line_end();
        self.text.replace_range(self.cursor..end, "");
    }

    /// Move one logical line up. `false` when there is none, which is what
    /// lets the caller walk the prompt history instead.
    pub fn up(&mut self) -> bool {
        self.clear_selection();
        self.move_up()
    }

    pub fn down(&mut self) -> bool {
        self.clear_selection();
        self.move_down()
    }

    pub fn select_up(&mut self) -> bool {
        self.begin_selection();
        self.move_up()
    }

    pub fn select_down(&mut self) -> bool {
        self.begin_selection();
        self.move_down()
    }

    /// The wrapped rows and the caret cell, for a box `width` columns wide.
    pub fn layout(&self, width: usize) -> Layout {
        crate::layout::count::band(0);
        let width = width.max(1);
        let mut lines = self.visual_lines(width);
        let mut cursor = self.cursor_cell(&lines, width);
        if cursor.1 >= width {
            cursor = (cursor.0 + 1, 0);
            if lines.len() <= cursor.0 {
                lines.push(VisualLine::empty(self.text.len()));
            }
        }
        let selection = lines
            .iter()
            .map(|line| self.selection_columns(line))
            .collect();
        Layout {
            lines: lines.into_iter().map(|line| line.text).collect(),
            cursor,
            images: Vec::new(),
            selection,
        }
    }

    fn begin_selection(&mut self) {
        self.anchor.get_or_insert(self.cursor);
    }

    fn delete_selection(&mut self) -> bool {
        let Some(range) = self.selection_range() else {
            return false;
        };
        self.text.replace_range(range.clone(), "");
        self.cursor = range.start;
        self.clear_selection();
        true
    }

    fn selection_range(&self) -> Option<std::ops::Range<usize>> {
        let anchor = self.anchor?;
        let start = anchor.min(self.cursor);
        let end = anchor.max(self.cursor);
        Some(start..end)
    }

    fn move_left(&mut self) {
        if let Some(start) = self.prev() {
            self.cursor = start;
        }
    }

    fn move_right(&mut self) {
        if let Some(end) = self.next() {
            self.cursor = end;
        }
    }

    fn move_home(&mut self) {
        self.cursor = self.line_start();
    }

    fn move_end(&mut self) {
        self.cursor = self.line_end();
    }

    fn move_up(&mut self) -> bool {
        let start = self.line_start();
        if start == 0 {
            return false;
        }
        let column = self.column();
        let previous = self.text[..start - 1].rfind('\n').map_or(0, |i| i + 1);
        self.cursor = seek(&self.text, previous, start - 1, column);
        true
    }

    fn move_down(&mut self) -> bool {
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

    fn visual_lines(&self, width: usize) -> Vec<VisualLine> {
        let width = width.max(1);
        let mut lines = vec![VisualLine::empty(0)];
        let mut column = 0;
        for (offset, grapheme) in self.text.grapheme_indices(true) {
            if grapheme == "\n" {
                lines.push(VisualLine::empty(offset + grapheme.len()));
                column = 0;
                continue;
            }
            if column + grapheme.width() > width {
                lines.push(VisualLine::empty(offset));
                column = 0;
            }
            if let Some(last) = lines.last_mut() {
                last.text.push_str(grapheme);
            }
            column += grapheme.width();
        }
        lines
    }

    fn cursor_cell(&self, lines: &[VisualLine], width: usize) -> (usize, usize) {
        for (row, line) in lines.iter().enumerate() {
            let end = line.start + line.text.len();
            if self.cursor >= line.start && self.cursor <= end {
                return (row, self.text[line.start..self.cursor].width());
            }
        }
        (lines.len().saturating_sub(1), width)
    }

    fn selection_columns(&self, line: &VisualLine) -> Option<std::ops::Range<usize>> {
        let selection = self.selection_range()?;
        let mut columns = 0;
        let mut first = None;
        let mut last = 0;
        for (offset, grapheme) in line.text.grapheme_indices(true) {
            let start = line.start + offset;
            let end = start + grapheme.len();
            if start < selection.end && end > selection.start {
                first.get_or_insert(columns);
                last = columns + grapheme.width();
            }
            columns += grapheme.width();
        }
        first.map(|first| first..last)
    }

    /// Where one step back lands: over a whole `[image N]` when the caret
    /// stands at its end, else one grapheme. The token is one thing in the
    /// line, so no motion or deletion ever opens it.
    fn prev(&self) -> Option<usize> {
        pictures::ending_at(&self.text, self.cursor).or_else(|| {
            self.text[..self.cursor]
                .graphemes(true)
                .next_back()
                .map(|g| self.cursor - g.len())
        })
    }

    fn next(&self) -> Option<usize> {
        pictures::starting_at(&self.text, self.cursor).or_else(|| {
            self.text[self.cursor..]
                .graphemes(true)
                .next()
                .map(|g| self.cursor + g.len())
        })
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

#[derive(Clone, Debug)]
struct VisualLine {
    start: usize,
    text: String,
}

impl VisualLine {
    fn empty(start: usize) -> Self {
        Self {
            start,
            text: String::new(),
        }
    }
}

/// The first grapheme boundary at or beyond a terminal column.
fn offset_in_line(text: &str, start: usize, column: usize) -> usize {
    let mut at = 0;
    for (offset, grapheme) in text.grapheme_indices(true) {
        if column <= at {
            return start + offset;
        }
        at += grapheme.width();
        if column < at {
            return start + offset + grapheme.len();
        }
    }
    start + text.len()
}

/// The offset `column` graphemes into `start..end`, clamped to the line's end.
fn seek(text: &str, start: usize, end: usize, column: usize) -> usize {
    text[start..end]
        .grapheme_indices(true)
        .nth(column)
        .map_or(end, |(i, _)| start + i)
}

mod drawing;
pub use drawing::draw as drawing;

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str, cursor: usize) -> Composer {
        Composer {
            text: text.into(),
            cursor,
            anchor: None,
            dragging: false,
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

    /// `[image 1]` is one thing: a backspace at its end takes all of it, a
    /// delete at its start does too, and `←`/`→` step over it whole. Words
    /// around it are still edited by the grapheme.
    #[test]
    fn a_picture_token_is_deleted_and_stepped_over_whole() {
        let mut c = Composer::default();
        c.insert("see ");
        c.insert(&pictures::placeholder(1));
        c.insert(" now");
        c.backspace();
        assert_eq!(c.text(), "see [image 1] no", "words go by the grapheme");
        c.set("see [image 1]");
        c.backspace();
        assert_eq!(c.text(), "see ", "the token goes whole");

        let mut c = at("see [image 12] now", 4);
        c.right();
        assert_eq!(c.cursor, 4 + "[image 12]".len(), "→ steps over it");
        c.left();
        assert_eq!(c.cursor, 4, "← steps back over it");
        c.delete();
        assert_eq!(c.text(), "see  now", "delete takes it whole");

        let mut c = at("[image x]", "[image x]".len());
        c.backspace();
        assert_eq!(
            c.text(),
            "[image x",
            "a token that is not a number is words"
        );
    }

    #[test]
    fn a_paste_lands_verbatim_at_the_caret() {
        let mut c = at("ab", 1);
        c.insert("x\ny");
        assert_eq!(c.text(), "ax\nyb");
    }

    #[test]
    fn pictures_precede_words_and_keep_the_text_caret() {
        let mut c = at("look here", 4);
        let mut held = pictures::Held::default();
        let first = held.hold(c.text(), "first.png".into());
        c.picture(first, &held);
        let second = held.hold(c.text(), "second.png".into());
        c.picture(second, &held);
        c.insert(" closely");
        assert_eq!(c.text(), "[image 1] [image 2] look closely here");
        assert_eq!(held.carried(c.text()).len(), 2);
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
                selection: vec![None, None],
                cursor: (1, 1),
                images: Vec::new(),
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
                selection: vec![None, None],
                cursor: (1, 2),
                images: Vec::new(),
            }
        );
    }

    #[test]
    fn layout_keeps_trailing_spaces_before_the_caret() {
        let c = at("word  ", 6);
        assert_eq!(
            c.layout(10),
            Layout {
                lines: vec!["word  ".into()],
                selection: vec![None],
                cursor: (0, 6),
                images: Vec::new(),
            }
        );
    }

    #[test]
    fn a_caret_at_the_right_edge_shows_on_the_next_row() {
        let c = at("abc", 3);
        assert_eq!(c.layout(3).cursor, (1, 0));
    }

    #[test]
    fn shifted_motion_marks_a_run_and_editing_replaces_it() {
        let mut c = at("one\ntwo", 7);
        c.select_home();
        c.select_up();
        assert_eq!(c.selected_text(), Some("one\ntwo"));
        assert_eq!(c.layout(10).selection, vec![Some(0..3), Some(0..3)]);
        c.insert("draft");
        assert_eq!(c.text(), "draft");
        assert_eq!(c.selected_text(), None);
    }

    #[test]
    fn pointer_columns_select_whole_wide_graphemes() {
        let mut c = at("a你好b\nnext", 0);
        c.pointer_press(c.offset_at(10, 0, 1).expect("first Chinese boundary"));
        c.pointer_drag(c.offset_at(10, 0, 4).expect("second Chinese boundary"));
        c.pointer_release();
        assert_eq!(c.selected_text(), Some("你好"));
        assert_eq!(c.layout(10).selection, vec![Some(1..5), None]);
    }
}
