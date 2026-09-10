//! What the transcript is holding: the block the pointer landed on, and the
//! run of cells being taken out of it.
//!
//! A run is two cells of the *rendered* transcript — the lines a person can
//! see, not the items behind them — so what is copied is what was read. The
//! clipboard is the terminal's, reached with OSC 52; a payload past
//! [`LIMIT`] is refused out loud rather than truncated, because half a
//! selection on the clipboard is worse than none.

use std::time::{Duration, Instant};

use base64::Engine;
use bingo_sdk::ItemId;
use ratatui::layout::Rect;
use ratatui::{Frame, style::Style};
use unicode_width::UnicodeWidthChar;

use crate::theme;

/// The most a terminal is asked to take in one sequence. tmux's own default
/// is smaller still; past this the answer is a notice, not a truncation.
pub const LIMIT: usize = 100 * 1024;

/// How long a drag held past the transcript's edge waits between lines: slow
/// enough to let go where a person meant to, quick enough to cross a
/// screenful in a second. A constant to tune by hand, not a setting.
pub const EDGE_PACE: Duration = Duration::from_millis(50);

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

/// How far past the rows of the transcript a screen row is, and on which
/// side. A pointer that has left the region is still pointing at the
/// transcript: above it are the lines that have scrolled off the top, below
/// it — over the composer, the status line, a rail card — the ones under its
/// foot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Above(usize),
    Below(usize),
}

/// A drag the hand is still holding past an edge: which way it pulls, and
/// when it last took a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dragging {
    pub edge: Edge,
    pub stepped: Instant,
}

/// What the transcript is holding.
#[derive(Clone, Debug, Default)]
pub struct Select {
    /// The block a click or a key last landed on.
    pub block: Option<ItemId>,
    /// The run being taken out of it.
    pub run: Option<Run>,
    /// The hand is past an edge of the transcript and the view is walking
    /// towards it, a line every [`EDGE_PACE`].
    pub dragging: Option<Dragging>,
}

impl Select {
    pub fn start(&mut self, at: Cell) {
        self.run = Some(Run {
            anchor: at,
            head: at,
        });
        self.dragging = None;
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
        self.dragging = None;
    }
}

impl Edge {
    /// Which side of a region `rows` tall a row falls past, and by how many
    /// rows; `None` while it is on it. The row is counted from the region's
    /// first, and a pointer above it counts backwards.
    pub fn of(row: i32, rows: usize) -> Option<Self> {
        let last = i32::try_from(rows).ok()? - 1;
        match row {
            _ if row < 0 => usize::try_from(row.unsigned_abs()).ok().map(Edge::Above),
            _ if row > last => usize::try_from(row - last).ok().map(Edge::Below),
            _ => None,
        }
    }

    /// The lines to scroll to bring what it points at into view: towards the
    /// head of the transcript above, towards its foot below.
    pub fn lines(self) -> isize {
        match self {
            Edge::Above(rows) => signed(rows),
            Edge::Below(rows) => -signed(rows),
        }
    }

    /// The line drawn at the boundary row of a view `rows` tall parked at
    /// `top`, in a transcript `height` lines long: the first line on the
    /// screen above, the last one below. The overshoot says how far to
    /// scroll, and this says what the pointer has reached once it has.
    pub fn line(self, top: usize, rows: usize, height: usize) -> usize {
        match self {
            Edge::Above(_) => top,
            Edge::Below(_) => (top + rows).min(height).saturating_sub(1),
        }
    }

    /// Whether there is any transcript left on this side of a line: a hand
    /// held past the last line asks for nothing more, and the frames another
    /// step would cost are not owed.
    pub fn beyond(self, line: usize, height: usize) -> bool {
        match self {
            Edge::Above(_) => line > 0,
            Edge::Below(_) => line + 1 < height,
        }
    }

    /// The one-row step a drag held past it takes each pace.
    pub fn step(self) -> Self {
        match self {
            Edge::Above(_) => Edge::Above(1),
            Edge::Below(_) => Edge::Below(1),
        }
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

    /// Whether it reaches nowhere: a press with no drag after it, which is a
    /// click and means what a click has always meant.
    pub fn empty(&self) -> bool {
        self.anchor == self.head
    }

    /// How many lines of the transcript it spans — what a copy says it took.
    pub fn lines(&self) -> usize {
        let (from, to) = self.span();
        to.line - from.line + 1
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

/// A count of rows as a move: a screen has fewer rows than an `isize` holds.
fn signed(rows: usize) -> isize {
    isize::try_from(rows).unwrap_or(isize::MAX)
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
///
/// `area`'s first row is line `top`: it is the rows carrying lines, which is
/// not the whole region when a short transcript hangs from the composer.
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
    fn a_run_that_reaches_nowhere_is_a_click() {
        let mut select = Select::default();
        select.start(Cell { line: 4, column: 2 });
        assert!(select.run.is_some_and(|run| run.empty()));
        assert_eq!(select.run.map(|run| run.lines()), Some(1));
        select.extend(Cell { line: 6, column: 0 });
        assert!(select.run.is_some_and(|run| !run.empty()));
        assert_eq!(
            select.run.map(|run| run.lines()),
            Some(3),
            "three lines, counted the way a person reads them"
        );
    }

    #[test]
    fn a_row_on_the_region_is_past_no_edge() {
        assert_eq!(Edge::of(0, 10), None);
        assert_eq!(Edge::of(9, 10), None);
    }

    #[test]
    fn a_row_off_the_region_is_the_rows_it_is_past_it_by() {
        assert_eq!(Edge::of(-1, 10), Some(Edge::Above(1)));
        assert_eq!(Edge::of(-4, 10), Some(Edge::Above(4)));
        assert_eq!(Edge::of(10, 10), Some(Edge::Below(1)));
        assert_eq!(Edge::of(13, 10), Some(Edge::Below(4)));
    }

    /// The overshoot is how far to scroll, and the sign is which way.
    #[test]
    fn an_edge_scrolls_towards_what_it_points_at() {
        assert_eq!(Edge::Above(4).lines(), 4);
        assert_eq!(Edge::Below(4).lines(), -4);
        assert_eq!(Edge::Above(4).step(), Edge::Above(1));
        assert_eq!(Edge::Below(4).step(), Edge::Below(1));
    }

    /// Lines 30..=49 are on a twenty-row view parked at 30.
    #[test]
    fn the_line_at_an_edge_is_the_first_or_the_last_one_drawn() {
        assert_eq!(Edge::Above(3).line(30, 20, 100), 30);
        assert_eq!(Edge::Below(3).line(30, 20, 100), 49);
    }

    /// A transcript shorter than its region hangs from the composer: the last
    /// line it drew is its own last, not the row that far down the pane.
    #[test]
    fn the_line_below_a_short_transcript_is_its_last() {
        assert_eq!(Edge::Below(3).line(0, 20, 4), 3);
    }

    #[test]
    fn an_edge_at_the_end_of_the_transcript_has_nowhere_further_to_go() {
        assert!(Edge::Above(1).beyond(1, 100));
        assert!(!Edge::Above(1).beyond(0, 100));
        assert!(Edge::Below(1).beyond(98, 100));
        assert!(!Edge::Below(1).beyond(99, 100));
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
