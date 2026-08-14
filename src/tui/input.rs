//! Prompt editing primitives: a cursor into a `String` plus the motions and
//! kills a readline-style prompt needs. Everything here is pure text work on
//! `(&mut String, &mut usize)`, so the chat state machine keeps owning the
//! buffer and every operation is unit testable without a terminal.
//!
//! The cursor is a **byte** index that always sits on a char boundary; motion
//! is per character (so CJK moves one glyph at a time) while rendering measures
//! display width.

use crate::tui::line::{char_width, text_width};

/// One rendered row of the prompt: the byte range it covers plus its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualLine {
    /// Byte offset of the first character.
    pub start: usize,
    /// Byte offset just past the last character (excludes the newline).
    pub end: usize,
    pub text: String,
}

/// Byte index of the char boundary before `cursor` (or 0).
pub fn prev_char(text: &str, cursor: usize) -> usize {
    text[..cursor.min(text.len())]
        .char_indices()
        .next_back()
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// Byte index of the char boundary after `cursor` (or the end).
pub fn next_char(text: &str, cursor: usize) -> usize {
    let cursor = cursor.min(text.len());
    text[cursor..]
        .chars()
        .next()
        .map(|c| cursor + c.len_utf8())
        .unwrap_or(text.len())
}

/// Start of the word before the cursor (skips trailing whitespace first).
pub fn word_left(text: &str, cursor: usize) -> usize {
    let mut i = cursor.min(text.len());
    while i > 0 {
        let p = prev_char(text, i);
        if !text[p..i].chars().all(char::is_whitespace) {
            break;
        }
        i = p;
    }
    while i > 0 {
        let p = prev_char(text, i);
        if text[p..i].chars().all(char::is_whitespace) {
            break;
        }
        i = p;
    }
    i
}

/// Start of the sub-word before the cursor: skip any run of non-alphanumeric
/// characters, then the alphanumeric run itself.
///
/// This is readline's `backward-word`, and the difference from [`word_left`]
/// (readline's *unix* word, whitespace only) is what makes it path-friendly:
/// `/`, `-`, `_` and `.` are boundaries, so `src/tui/chat_tail.rs` is six stops
/// rather than one. `alt+b`/`alt+f`/`alt+d`/`alt+backspace` use it; `ctrl+w`
/// keeps the whitespace word, exactly as a shell does.
pub fn subword_left(text: &str, cursor: usize) -> usize {
    let mut i = cursor.min(text.len());
    while i > 0 {
        let p = prev_char(text, i);
        if text[p..i].chars().all(char::is_alphanumeric) {
            break;
        }
        i = p;
    }
    while i > 0 {
        let p = prev_char(text, i);
        if !text[p..i].chars().all(char::is_alphanumeric) {
            break;
        }
        i = p;
    }
    i
}

/// End of the sub-word after the cursor: the mirror of [`subword_left`].
pub fn subword_right(text: &str, cursor: usize) -> usize {
    let mut i = cursor.min(text.len());
    while i < text.len() {
        let n = next_char(text, i);
        if text[i..n].chars().all(char::is_alphanumeric) {
            break;
        }
        i = n;
    }
    while i < text.len() {
        let n = next_char(text, i);
        if !text[i..n].chars().all(char::is_alphanumeric) {
            break;
        }
        i = n;
    }
    i
}

/// Start of the logical line (after the previous `\n`) holding the cursor.
pub fn line_start(text: &str, cursor: usize) -> usize {
    let cursor = cursor.min(text.len());
    text[..cursor].rfind('\n').map(|i| i + 1).unwrap_or(0)
}

/// End of the logical line (before the next `\n`) holding the cursor.
pub fn line_end(text: &str, cursor: usize) -> usize {
    let cursor = cursor.min(text.len());
    text[cursor..]
        .find('\n')
        .map(|i| cursor + i)
        .unwrap_or(text.len())
}

/// Insert `s` at the cursor and move past it.
pub fn insert(text: &mut String, cursor: &mut usize, s: &str) {
    let at = (*cursor).min(text.len());
    text.insert_str(at, s);
    *cursor = at + s.len();
}

/// Delete the character before the cursor. Returns whether anything went.
pub fn backspace(text: &mut String, cursor: &mut usize) -> bool {
    let at = (*cursor).min(text.len());
    if at == 0 {
        return false;
    }
    let start = prev_char(text, at);
    text.replace_range(start..at, "");
    *cursor = start;
    true
}

/// Delete the character under the cursor. Returns whether anything went.
pub fn delete(text: &mut String, cursor: &mut usize) -> bool {
    let at = (*cursor).min(text.len());
    if at >= text.len() {
        return false;
    }
    let end = next_char(text, at);
    text.replace_range(at..end, "");
    *cursor = at;
    true
}

/// Cut from the cursor to the end of the logical line (ctrl+k).
pub fn kill_to_end(text: &mut String, cursor: &mut usize) -> String {
    let at = (*cursor).min(text.len());
    let end = line_end(text, at);
    // On an empty tail, eat the newline itself so repeated ctrl+k joins lines.
    let end = if end == at { next_char(text, at) } else { end };
    let cut = text[at..end].to_string();
    text.replace_range(at..end, "");
    *cursor = at;
    cut
}

/// Cut from the start of the logical line to the cursor (ctrl+u).
pub fn kill_to_start(text: &mut String, cursor: &mut usize) -> String {
    let at = (*cursor).min(text.len());
    let start = line_start(text, at);
    let cut = text[start..at].to_string();
    text.replace_range(start..at, "");
    *cursor = start;
    cut
}

/// Cut the word before the cursor (ctrl+w).
pub fn kill_word(text: &mut String, cursor: &mut usize) -> String {
    let at = (*cursor).min(text.len());
    let start = word_left(text, at);
    let cut = text[start..at].to_string();
    text.replace_range(start..at, "");
    *cursor = start;
    cut
}

/// Cut the span a motion answered with and leave the cursor at its start.
/// Both ends are clamped, so a motion that ran off the text cuts what is there.
pub fn kill_between(text: &mut String, cursor: &mut usize, start: usize, end: usize) -> String {
    let start = start.min(text.len());
    let end = end.min(text.len()).max(start);
    let cut = text[start..end].to_string();
    text.replace_range(start..end, "");
    *cursor = start;
    cut
}

/// Split the prompt into rendered rows: newlines break, and anything longer
/// than `width` display columns hard-wraps. Hard wrapping (rather than word
/// wrapping) keeps the cursor mapping exact — every byte belongs to exactly
/// one row. Always returns at least one row.
pub fn visual_lines(text: &str, width: usize) -> Vec<VisualLine> {
    let width = width.max(1);
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut col = 0usize;
    let mut current = String::new();
    for (i, ch) in text.char_indices() {
        if ch == '\n' {
            out.push(VisualLine {
                start,
                end: i,
                text: std::mem::take(&mut current),
            });
            start = i + ch.len_utf8();
            col = 0;
            continue;
        }
        let w = char_width(ch);
        if col + w > width && !current.is_empty() {
            out.push(VisualLine {
                start,
                end: i,
                text: std::mem::take(&mut current),
            });
            start = i;
            col = 0;
        }
        current.push(ch);
        col += w;
    }
    out.push(VisualLine {
        start,
        end: text.len(),
        text: current,
    });
    out
}

/// Row and display column of `cursor` within `lines` (from [`visual_lines`]).
pub fn cursor_cell(text: &str, lines: &[VisualLine], cursor: usize) -> (usize, usize) {
    let cursor = cursor.min(text.len());
    let row = lines
        .iter()
        .rposition(|l| l.start <= cursor)
        .unwrap_or(0)
        .min(lines.len().saturating_sub(1));
    let line = &lines[row];
    let end = cursor.min(line.end).max(line.start);
    (row, text_width(&text[line.start..end]))
}

/// Move the cursor one rendered row up/down keeping the display column.
/// Returns `None` when already on the first/last row — the caller then treats
/// the key as history navigation (CC semantics).
pub fn move_row(text: &str, cursor: usize, width: usize, down: bool) -> Option<usize> {
    let lines = visual_lines(text, width);
    let (row, col) = cursor_cell(text, &lines, cursor);
    let target = if down {
        if row + 1 >= lines.len() {
            return None;
        }
        row + 1
    } else {
        if row == 0 {
            return None;
        }
        row - 1
    };
    let line = &lines[target];
    let mut at = line.start;
    let mut w = 0usize;
    for ch in line.text.chars() {
        if w >= col {
            break;
        }
        w += char_width(ch);
        at += ch.len_utf8();
    }
    Some(at)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn char_motion_is_cjk_aware() {
        let text = "aＡb";
        assert_eq!(next_char(text, 0), 1);
        assert_eq!(next_char(text, 1), 4, "Ａ is 3 bytes");
        assert_eq!(prev_char(text, 4), 1);
        assert_eq!(prev_char(text, 0), 0, "clamped at start");
        assert_eq!(next_char(text, text.len()), text.len(), "clamped at end");
    }

    /// The whitespace word (`ctrl+w`'s): punctuation is part of it.
    #[test]
    fn word_motion_skips_whitespace_runs() {
        let text = "hello  brave world";
        assert_eq!(word_left(text, text.len()), "hello  brave ".len());
        assert_eq!(word_left(text, "hello  ".len()), 0);
    }

    /// The sub-word (`alt+b`/`alt+f`'s): a path is a run of stops, so a
    /// mistyped segment can be fixed without retyping the whole path.
    #[test]
    fn subword_motion_stops_inside_a_path() {
        let text = "src/tui/chat_tail.rs";
        let mut at = text.len();
        let mut stops = Vec::new();
        while at > 0 {
            at = subword_left(text, at);
            stops.push(&text[at..]);
        }
        assert_eq!(
            stops,
            vec![
                "rs",
                "tail.rs",
                "chat_tail.rs",
                "tui/chat_tail.rs",
                "src/tui/chat_tail.rs",
            ],
            "`/`, `_` and `.` are all boundaries"
        );

        // Forward is the mirror.
        assert_eq!(subword_right(text, 0), "src".len());
        assert_eq!(subword_right(text, "src".len()), "src/tui".len());
        assert_eq!(subword_right(text, text.len()), text.len(), "clamped");
        assert_eq!(subword_left(text, 0), 0, "clamped");
    }

    /// CJK counts as alphanumeric, so a sentence of it is one stop rather
    /// than one per glyph.
    #[test]
    fn subword_motion_is_cjk_aware() {
        let text = "修 中文字";
        assert_eq!(subword_left(text, text.len()), "修 ".len());
        assert_eq!(subword_right(text, 0), "修".len());
    }

    #[test]
    fn kill_between_clamps_and_parks_the_cursor() {
        let mut text = String::from("hello world");
        let mut cursor = 11usize;
        assert_eq!(kill_between(&mut text, &mut cursor, 6, 99), "world");
        assert_eq!(text, "hello ");
        assert_eq!(cursor, 6);
        assert_eq!(kill_between(&mut text, &mut cursor, 4, 2), "", "inverted");
    }

    #[test]
    fn insert_and_delete_move_the_cursor() {
        let mut text = String::from("ac");
        let mut cursor = 1usize;
        insert(&mut text, &mut cursor, "b");
        assert_eq!((text.as_str(), cursor), ("abc", 2));
        assert!(backspace(&mut text, &mut cursor));
        assert_eq!((text.as_str(), cursor), ("ac", 1));
        assert!(delete(&mut text, &mut cursor));
        assert_eq!((text.as_str(), cursor), ("a", 1));
        assert!(!delete(&mut text, &mut cursor), "nothing after the cursor");
        assert!(backspace(&mut text, &mut cursor));
        assert!(!backspace(&mut text, &mut cursor), "empty buffer");
    }

    #[test]
    fn kills_cut_the_expected_span() {
        let mut text = String::from("one two three");
        let mut cursor = text.len();
        assert_eq!(kill_word(&mut text, &mut cursor), "three");
        assert_eq!(text, "one two ");
        assert_eq!(kill_to_start(&mut text, &mut cursor), "one two ");
        assert_eq!((text.as_str(), cursor), ("", 0));

        let mut text = String::from("keep|cut");
        let mut cursor = "keep".len();
        assert_eq!(kill_to_end(&mut text, &mut cursor), "|cut");
        assert_eq!(text, "keep");
    }

    #[test]
    fn kills_respect_logical_lines() {
        let mut text = String::from("first\nsecond");
        let mut cursor = "first\nse".len();
        assert_eq!(kill_to_start(&mut text, &mut cursor), "se");
        assert_eq!(text, "first\ncond");
        assert_eq!(cursor, "first\n".len());
        assert_eq!(kill_to_end(&mut text, &mut cursor), "cond");
        assert_eq!(text, "first\n");
        // Empty tail: ctrl+k eats the newline and joins the next line up.
        let mut text = String::from("a\nb");
        let mut cursor = 1usize;
        assert_eq!(kill_to_end(&mut text, &mut cursor), "\n");
        assert_eq!(text, "ab");
    }

    #[test]
    fn visual_lines_break_on_newlines_and_width() {
        let lines = visual_lines("ab\ncd", 10);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "ab");
        assert_eq!(
            lines[1],
            VisualLine {
                start: 3,
                end: 5,
                text: "cd".into()
            }
        );
        // Hard wrap by display width; wide glyphs count two columns.
        let lines = visual_lines("ＡＢＣＤ", 4);
        assert_eq!(
            lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(),
            vec!["ＡＢ", "ＣＤ"]
        );
        // Empty text still yields one row (the prompt always has a caret row).
        assert_eq!(visual_lines("", 10).len(), 1);
        // A trailing newline leaves an empty last row to put the caret on.
        assert_eq!(visual_lines("a\n", 10).len(), 2);
    }

    #[test]
    fn cursor_cell_maps_into_the_rendered_grid() {
        let text = "ab\ncd";
        let lines = visual_lines(text, 10);
        assert_eq!(cursor_cell(text, &lines, 0), (0, 0));
        assert_eq!(cursor_cell(text, &lines, 2), (0, 2), "end of first row");
        assert_eq!(cursor_cell(text, &lines, 3), (1, 0), "start of second row");
        assert_eq!(cursor_cell(text, &lines, 5), (1, 2));
        let text = "ＡＢ";
        let lines = visual_lines(text, 10);
        assert_eq!(
            cursor_cell(text, &lines, 3),
            (0, 2),
            "one wide glyph = 2 cols"
        );
    }

    #[test]
    fn row_motion_stops_at_the_edges() {
        let text = "one\ntwo\nthree";
        // Middle row moves both ways.
        let cursor = "one\ntw".len();
        assert_eq!(move_row(text, cursor, 20, false), Some("tw".len()));
        assert_eq!(move_row(text, cursor, 20, true), Some("one\ntwo\ntw".len()));
        // Edges hand the key back to history navigation.
        assert_eq!(move_row(text, 1, 20, false), None);
        assert_eq!(move_row(text, text.len(), 20, true), None);
        // Single-line input has no row motion at all.
        assert_eq!(move_row("plain", 2, 20, true), None);
        assert_eq!(move_row("plain", 2, 20, false), None);
    }

    #[test]
    fn row_motion_clamps_to_shorter_rows() {
        let text = "longer line\nab";
        let cursor = "longer line\na".len();
        // Up from column 1 lands on column 1 of the long row.
        assert_eq!(move_row(text, cursor, 20, false), Some(1));
        // Down from a far column clamps to the short row's end.
        assert_eq!(move_row(text, 9, 20, true), Some(text.len()));
    }
}
