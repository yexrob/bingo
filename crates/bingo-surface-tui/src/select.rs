//! What the transcript is holding: the block the pointer landed on, and the
//! run of cells being taken out of it.
//!
//! A run is two cells of the *rendered* transcript — the lines a person can
//! see, not the items behind them — so what is copied is what was read. The
//! clipboard is the terminal's, reached with OSC 52; a payload past
//! [`LIMIT`] is refused out loud rather than truncated, because half a
//! selection on the clipboard is worse than none.

use base64::Engine;
use bingo_sdk::ItemId;
use ratatui::layout::Rect;
use ratatui::{Frame, style::Style};
use unicode_width::UnicodeWidthChar;

use crate::theme;

/// The most a terminal is asked to take in one sequence. tmux's own default
/// is smaller still; past this the answer is a notice, not a truncation.
pub const LIMIT: usize = 100 * 1024;

/// A cell of the rendered transcript.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Cell {
    pub line: usize,
    pub column: usize,
}

/// A run of cells, from where it was started to where it reaches now. Either
/// end may be the earlier one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    pub anchor: Cell,
    pub head: Cell,
}

/// What the transcript is holding.
#[derive(Clone, Debug, Default)]
pub struct Select {
    /// The block a click or a key last landed on.
    pub block: Option<ItemId>,
    /// The run being taken out of it.
    pub run: Option<Run>,
}

impl Select {
    pub fn start(&mut self, at: Cell) {
        self.run = Some(Run {
            anchor: at,
            head: at,
        });
    }

    pub fn extend(&mut self, to: Cell) {
        if let Some(run) = self.run.as_mut() {
            run.head = to;
        }
    }

    /// Move the far end by lines and columns, within the transcript's shape.
    pub fn walk(&mut self, lines: isize, columns: isize, height: usize) {
        let Some(run) = self.run.as_mut() else {
            return;
        };
        run.head = Cell {
            line: run
                .head
                .line
                .saturating_add_signed(lines)
                .min(height.saturating_sub(1)),
            column: run.head.column.saturating_add_signed(columns),
        };
    }

    pub fn clear(&mut self) {
        self.run = None;
    }
}

impl Run {
    /// The two ends in reading order.
    pub fn span(&self) -> (Cell, Cell) {
        match self.anchor <= self.head {
            true => (self.anchor, self.head),
            false => (self.head, self.anchor),
        }
    }

    /// Whether a cell of the transcript is inside the run.
    pub fn holds(&self, line: usize, column: usize) -> bool {
        let (from, to) = self.span();
        let at = Cell { line, column };
        at >= from && at < to
    }

    /// What is inside it, as a person would paste it.
    pub fn text(&self, lines: &[String]) -> String {
        let (from, to) = self.span();
        (from.line..=to.line)
            .filter_map(|line| lines.get(line).map(|text| (line, text)))
            .map(|(line, text)| {
                let start = if line == from.line { from.column } else { 0 };
                let end = if line == to.line {
                    to.column
                } else {
                    usize::MAX
                };
                slice(text, start, end)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// The cells `[from, to)` of a line, measured as the terminal measures them.
fn slice(text: &str, from: usize, to: usize) -> String {
    let mut out = String::new();
    let mut column = 0;
    for c in text.chars() {
        let width = UnicodeWidthChar::width(c).unwrap_or(0);
        if column >= to {
            break;
        }
        if column >= from {
            out.push(c);
        }
        column += width;
    }
    out.trim_end().to_string()
}

/// The bytes that put `text` on the terminal's own clipboard, or nothing when
/// it is too much to ask (`OSC 52 ; c ; <base64> BEL`).
pub fn osc52(text: &str) -> Option<Vec<u8>> {
    let payload = base64::engine::general_purpose::STANDARD.encode(text);
    if payload.len() > LIMIT {
        return None;
    }
    let mut out = b"\x1b]52;c;".to_vec();
    out.extend_from_slice(payload.as_bytes());
    out.push(0x07);
    Some(out)
}

/// What a refusal says. It names the size, because the way out is to select
/// less — or, under tmux, to turn `set-clipboard on`.
pub fn refused(bytes: usize) -> String {
    format!(
        "{} KiB is more than the terminal will take — select less",
        bytes.div_ceil(1024)
    )
}

/// Tint the cells of the run that are on the screen.
pub fn mark(frame: &mut Frame, area: Rect, top: usize, run: &Run) {
    for row in 0..area.height {
        let line = top + row as usize;
        for column in 0..area.width {
            if run.holds(line, column as usize) {
                paint(
                    frame,
                    area.x + column,
                    area.y + row,
                    theme::raised().patch(theme::presence()),
                );
            }
        }
    }
}

fn paint(frame: &mut Frame, x: u16, y: u16, style: Style) {
    frame.buffer_mut()[(x, y)].set_style(style);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines() -> Vec<String> {
        vec![
            "the first line".to_string(),
            "the second one".to_string(),
            "and a third".to_string(),
        ]
    }

    fn run(from: (usize, usize), to: (usize, usize)) -> Run {
        Run {
            anchor: Cell {
                line: from.0,
                column: from.1,
            },
            head: Cell {
                line: to.0,
                column: to.1,
            },
        }
    }

    #[test]
    fn a_run_inside_one_line_is_the_cells_between_its_ends() {
        assert_eq!(run((0, 4), (0, 9)).text(&lines()), "first");
    }

    #[test]
    fn a_run_across_lines_keeps_the_line_breaks() {
        assert_eq!(
            run((0, 4), (2, 5)).text(&lines()),
            "first line\nthe second one\nand a"
        );
    }

    #[test]
    fn a_run_drawn_backwards_is_the_same_run() {
        assert_eq!(
            run((2, 5), (0, 4)).text(&lines()),
            run((0, 4), (2, 5)).text(&lines())
        );
        assert_eq!(run((2, 5), (0, 4)).span(), run((0, 4), (2, 5)).span());
    }

    #[test]
    fn the_cells_it_holds_are_the_ones_that_are_tinted() {
        let run = run((0, 4), (0, 9));
        assert!(!run.holds(0, 3));
        assert!(run.holds(0, 4));
        assert!(run.holds(0, 8));
        assert!(!run.holds(0, 9), "the far end is not inside it");
        assert!(!run.holds(1, 5));
    }

    #[test]
    fn walking_moves_the_far_end_and_stops_at_the_foot() {
        let mut select = Select::default();
        select.start(Cell { line: 1, column: 2 });
        select.walk(1, 3, 3);
        assert_eq!(
            select.run.map(|r| r.head),
            Some(Cell { line: 2, column: 5 })
        );
        select.walk(5, 0, 3);
        assert_eq!(select.run.map(|r| r.head.line), Some(2));
        select.walk(-9, -9, 3);
        assert_eq!(
            select.run.map(|r| r.head),
            Some(Cell { line: 0, column: 0 })
        );
    }

    #[test]
    fn a_selection_is_the_cells_that_were_read_not_the_bytes_behind_them() {
        let lines = vec!["✻ 你好 warm".to_string()];
        assert_eq!(run((0, 2), (0, 6)).text(&lines), "你好");
    }

    #[test]
    fn osc_52_carries_the_selection_as_base64() {
        assert_eq!(
            osc52("hi").expect("a short selection"),
            b"\x1b]52;c;aGk=\x07".to_vec()
        );
    }

    #[test]
    fn a_selection_too_large_for_the_terminal_is_refused_by_name() {
        let huge = "x".repeat(LIMIT);
        assert!(
            osc52(&huge).is_none(),
            "base64 of 100 KiB is over the limit"
        );
        assert_eq!(
            refused(huge.len()),
            "100 KiB is more than the terminal will take — select less"
        );
        assert!(osc52(&"x".repeat(1024)).is_some());
    }
}
