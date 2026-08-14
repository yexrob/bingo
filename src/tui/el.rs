//! Declarative element tree — the composition layer of the TUI.
//!
//! The shape borrows from Ink (React for terminals): views are trees of
//! elements built fresh every frame by plain functions ("components"), and a
//! single interpreter ([`render`]) turns a tree into the flat row list the
//! drivers consume. What is deliberately *not* borrowed is Ink's runtime — no
//! virtual DOM, no hooks, no retained component state: the iocraft era showed
//! that a React-style runtime in a terminal breeds render storms and diff
//! desync (research.md D25/D26). Here the tree is pure data and rendering is
//! one pre-order walk.
//!
//! What the tree buys over imperative `Vec<Row>` pushes:
//!
//! - **Measured, never predicted**: an element's height is the row count that
//!   falls out of rendering it — there is no second formula to drift.
//! - **Offset-free annotation**: click targets ([`El::Click`]) and the caret
//!   ([`El::Caret`]) are attached to the subtree they describe; absolute row
//!   numbers are computed by the walk, not hand-threaded through counters.
//!
//! Text layout (wrapping, truncation) intentionally stays in the components:
//! markdown and `wrap_words` already produce width-correct [`Line`]s, and the
//! leaves here are one-row-per-line by construction ([`Row::new`] sanitizes).

use ratatui::style::Color;

use crate::tui::line::Line;

/// One document row: exactly one terminal line plus row-level paint attributes.
#[derive(Debug, Clone)]
pub struct Row {
    pub line: Line,
    /// Full-row background.
    pub bg: Option<Color>,
    /// Right padding inside the row (CC user bubble paddingRight=1).
    pub padding_right: usize,
}

impl Row {
    /// Every row is exactly one canvas line: the constructor is the single
    /// choke point that enforces it (see [`crate::tui::line::sanitize`]).
    pub fn new(line: Line) -> Self {
        let mut line = line;
        line.sanitize();
        Self {
            line,
            bg: None,
            padding_right: 0,
        }
    }

    /// Bubble row with a full-row background (user messages; CC paddingRight=1).
    pub fn bubble(line: Line, bg: Color) -> Self {
        let mut row = Row::new(line);
        row.bg = Some(bg);
        row.padding_right = 1;
        row
    }
}

/// Click target of a document row.
#[derive(Debug, Clone)]
pub enum ClickTarget {
    /// Collapse-group row (collapses/expands the group).
    Group { message: usize, group: usize },
    /// Activity header row (collapses/expands the activity).
    Activity { message: usize, path: Vec<usize> },
    /// Permission option (confirm by index).
    AskOption(usize),
}

/// Document coordinate range of a clickable region.
#[derive(Debug, Clone)]
pub struct ClickRange {
    pub start: usize,
    pub end: usize,
    pub target: ClickTarget,
}

/// A click range local to a pre-laid-out leaf (offsets relative to the leaf's
/// first row; the render walk turns them absolute).
#[derive(Debug, Clone)]
pub struct LocalClick {
    pub start: usize,
    pub end: usize,
    pub target: ClickTarget,
}

/// A view element. Leaves carry rows; wrappers annotate the subtree they
/// contain. Conditional rendering is ordinary Rust producing [`El::None`].
#[derive(Debug, Clone)]
pub enum El {
    /// One empty row (block spacing).
    Blank,
    /// One row of styled text.
    Line(Line),
    /// One row with row-level attributes (bubble background / padding).
    Row(Row),
    /// A pre-laid-out block, one row per line.
    Lines(Vec<Line>),
    /// A pre-laid-out block of attributed rows.
    Rows(Vec<Row>),
    /// Vertical stack.
    Col(Vec<El>),
    /// The child's row span is clickable. Ranges are emitted pre-order
    /// (enclosing wrapper first), and click resolution picks the first match —
    /// an outer wrapper wins over annotations inside the same span.
    Click { target: ClickTarget, child: Box<El> },
    /// The caret sits on the child's first row at `col`. Last one wins if a
    /// tree declares several (a frame has one caret; components declare at
    /// most one).
    Caret { col: usize, child: Box<El> },
    /// A pre-laid-out leaf that carries its own click ranges (activity
    /// layouts): `clicks` are leaf-local and get offset by the walk.
    Annotated {
        rows: Vec<Row>,
        clicks: Vec<LocalClick>,
    },
    /// The child's rows sit to the right of a gutter: `cells[i]` prefixes the
    /// child's `i`-th row, and `blank` prefixes every row past the cells.
    ///
    /// A wrapper rather than a second row builder because the row *count* is
    /// unchanged: click ranges and the caret keep the offsets the walk already
    /// computed, and the message underneath is built by the same code that
    /// builds it without a gutter. The caller narrows the width it hands the
    /// child — the gutter takes cells, and nothing here can wrap text back.
    Gutter {
        cells: Vec<Line>,
        blank: Line,
        child: Box<El>,
    },
}

impl El {
    /// Vertical stack sugar.
    pub fn col(children: impl Into<Vec<El>>) -> Self {
        El::Col(children.into())
    }

    /// Clickable wrapper sugar.
    pub fn click(target: ClickTarget, child: El) -> Self {
        El::Click {
            target,
            child: Box::new(child),
        }
    }

    /// Caret wrapper sugar.
    pub fn caret(col: usize, child: El) -> Self {
        El::Caret {
            col,
            child: Box::new(child),
        }
    }

    /// Gutter wrapper sugar.
    pub fn gutter(cells: Vec<Line>, blank: Line, child: El) -> Self {
        El::Gutter {
            cells,
            blank,
            child: Box::new(child),
        }
    }

    /// The first row in this tree that has anything on it, as its line plus the
    /// columns that row reserves on its right ([`Row::padding_right`]).
    ///
    /// Blank spacing rows are skipped: a trailer hung on one would float above
    /// the block it belongs to instead of sitting beside it. Used to put a
    /// message's send stamp on its opening row (D93).
    pub fn first_content_line_mut(&mut self) -> Option<(&mut Line, usize)> {
        fn from_rows(rows: &mut [Row]) -> Option<(&mut Line, usize)> {
            rows.iter_mut()
                .find(|row| has_content(&row.line))
                .map(|row| {
                    let padding = row.padding_right;
                    (&mut row.line, padding)
                })
        }
        fn has_content(line: &Line) -> bool {
            line.image.is_some() || !line.plain_text().trim().is_empty()
        }
        match self {
            El::Blank => None,
            El::Line(line) => has_content(line).then_some((line, 0)),
            El::Row(row) => {
                let padding = row.padding_right;
                has_content(&row.line).then_some((&mut row.line, padding))
            }
            El::Lines(lines) => lines
                .iter_mut()
                .find(|line| has_content(line))
                .map(|l| (l, 0)),
            El::Rows(rows) | El::Annotated { rows, .. } => from_rows(rows),
            El::Col(children) => children
                .iter_mut()
                .find_map(|child| child.first_content_line_mut()),
            El::Click { child, .. } | El::Caret { child, .. } | El::Gutter { child, .. } => {
                child.first_content_line_mut()
            }
        }
    }
}

/// Prefix `rows` with a gutter: `cells[i]` on the `i`-th row, `blank` past the
/// cells. The single implementation both [`El::Gutter`] and the row-level
/// conversation builders use, so a message's indentation is decided once.
///
/// The row's own background is left alone. A user bubble spans the terminal by
/// design, and re-applying its colour segment by segment after the gutter
/// would make the bubble stop short of the right edge the moment it settles
/// into scrollback ([`crate::tui::view::history_line`] pads from `Row::bg`).
/// The avatar chip carries its own background, so it still reads as a chip.
pub fn gutter_rows(rows: &mut [Row], cells: &[Line], blank: &Line) {
    for (i, row) in rows.iter_mut().enumerate() {
        let cell = cells.get(i).unwrap_or(blank);
        row.line.segs.splice(0..0, cell.segs.iter().cloned());
        row.line.sanitize();
    }
}

/// The flat output of one render walk.
#[derive(Debug, Default)]
pub struct Rendered {
    pub rows: Vec<Row>,
    /// Absolute click ranges, in pre-order emission order (resolution order).
    pub clicks: Vec<ClickRange>,
    /// Absolute caret cell `(row, col)`, if any subtree declared one.
    pub caret: Option<(usize, usize)>,
}

/// Interpret a tree into rows. Consumes the element — trees are built fresh
/// each frame and their rows move straight into the output.
pub fn render(el: El) -> Rendered {
    let mut out = Rendered::default();
    walk(el, &mut out);
    out
}

/// Row count of a tree (renders and discards; chrome height is measured this
/// way so there is no second formula).
pub fn height(el: El) -> usize {
    render(el).rows.len()
}

fn walk(el: El, out: &mut Rendered) {
    match el {
        El::Blank => out.rows.push(Row::new(Line::empty())),
        El::Line(line) => out.rows.push(Row::new(line)),
        El::Row(row) => out.rows.push(row),
        El::Lines(lines) => out.rows.extend(lines.into_iter().map(Row::new)),
        El::Rows(rows) => out.rows.extend(rows),
        El::Col(children) => {
            for child in children {
                walk(child, out);
            }
        }
        El::Click { target, child } => {
            let start = out.rows.len();
            // Pre-order: reserve the slot so the enclosing range resolves
            // before any annotation inside the child.
            let slot = out.clicks.len();
            out.clicks.push(ClickRange {
                start,
                end: start,
                target,
            });
            walk(*child, out);
            out.clicks[slot].end = out.rows.len();
        }
        El::Caret { col, child } => {
            let row = out.rows.len();
            walk(*child, out);
            // The caret needs a row to sit on; an empty child declares none.
            if out.rows.len() > row {
                out.caret = Some((row, col));
            }
        }
        El::Annotated { rows, clicks } => {
            let base = out.rows.len();
            out.clicks.extend(clicks.into_iter().map(|c| ClickRange {
                start: base + c.start,
                end: base + c.end,
                target: c.target,
            }));
            out.rows.extend(rows);
        }
        El::Gutter {
            cells,
            blank,
            child,
        } => {
            // The child renders first and is indented afterwards: it produces
            // the same rows it would without a gutter, and the row count it
            // produced is the row count that comes out — which is what lets the
            // click ranges and the caret it declared stay correct.
            let start = out.rows.len();
            walk(*child, out);
            gutter_rows(&mut out.rows[start..], &cells, &blank);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::line::SegStyle;

    fn texts(rendered: &Rendered) -> Vec<String> {
        rendered.rows.iter().map(|r| r.line.plain_text()).collect()
    }

    /// A gutter indents without disturbing anything the walk computed: the row
    /// count is the same, so the click range and the caret it declared still
    /// point at the rows they were about. That invariant is the entire reason
    /// the gutter is a wrapper instead of a second row builder.
    #[test]
    fn a_gutter_indents_without_moving_clicks_or_the_caret() {
        let body = El::Lines(vec![
            Line::styled("first", SegStyle::plain()),
            Line::styled("second", SegStyle::plain()),
            Line::styled("third", SegStyle::plain()),
        ]);
        let bare = render(El::col(vec![
            El::Blank,
            El::click(ClickTarget::AskOption(1), body.clone()),
        ]));
        let cells = vec![
            Line::styled("AB  ", SegStyle::plain()),
            Line::styled("CD  ", SegStyle::plain()),
        ];
        let blank = Line::styled("    ", SegStyle::plain());
        let gutted = render(El::col(vec![
            El::Blank,
            El::gutter(
                cells,
                blank,
                El::caret(2, El::click(ClickTarget::AskOption(1), body)),
            ),
        ]));

        assert_eq!(bare.rows.len(), gutted.rows.len(), "same row count");
        assert_eq!(
            texts(&gutted)[1..],
            ["AB  first", "CD  second", "    third"],
            "cells on the portrait's rows, blank below"
        );
        assert_eq!(
            (bare.clicks[0].start, bare.clicks[0].end),
            (gutted.clicks[0].start, gutted.clicks[0].end),
            "the click range is where it was"
        );
        assert_eq!(gutted.caret, Some((1, 2)), "and so is the caret");
    }

    #[test]
    fn leaves_stack_in_order() {
        let el = El::col(vec![
            El::Line(Line::plain("a")),
            El::Col(Vec::new()),
            El::Blank,
            El::Lines(vec![Line::plain("b"), Line::plain("c")]),
            El::Rows(vec![Row::new(Line::plain("d"))]),
        ]);
        let out = render(el);
        assert_eq!(texts(&out), vec!["a", "", "b", "c", "d"]);
        assert!(out.clicks.is_empty());
        assert!(out.caret.is_none());
    }

    #[test]
    fn height_measures_by_rendering() {
        let el = El::col(vec![
            El::Blank,
            El::Col(Vec::new()),
            El::Line(Line::plain("x")),
        ]);
        assert_eq!(height(el), 2);
    }

    #[test]
    fn click_spans_cover_the_child_with_absolute_rows() {
        let el = El::col(vec![
            El::Line(Line::plain("above")),
            El::click(
                ClickTarget::AskOption(1),
                El::Lines(vec![Line::plain("opt"), Line::plain("desc")]),
            ),
        ]);
        let out = render(el);
        assert_eq!(out.clicks.len(), 1);
        assert_eq!((out.clicks[0].start, out.clicks[0].end), (1, 3));
    }

    #[test]
    fn nested_clicks_emit_outer_first() {
        // An outer Group wrapper around an annotated activity leaf: the group
        // range must resolve before the activity range on the same rows
        // (first match wins in doc_click).
        let leaf = El::Annotated {
            rows: vec![Row::new(Line::plain("head")), Row::new(Line::plain("out"))],
            clicks: vec![LocalClick {
                start: 0,
                end: 2,
                target: ClickTarget::Activity {
                    message: 0,
                    path: vec![3],
                },
            }],
        };
        let el = El::col(vec![
            El::Blank,
            El::click(
                ClickTarget::Group {
                    message: 0,
                    group: 0,
                },
                leaf,
            ),
        ]);
        let out = render(el);
        assert_eq!(out.clicks.len(), 2);
        assert!(matches!(out.clicks[0].target, ClickTarget::Group { .. }));
        assert_eq!((out.clicks[0].start, out.clicks[0].end), (1, 3));
        assert!(matches!(out.clicks[1].target, ClickTarget::Activity { .. }));
        assert_eq!((out.clicks[1].start, out.clicks[1].end), (1, 3));
    }

    #[test]
    fn caret_lands_on_the_childs_first_row() {
        let el = El::col(vec![
            El::Line(Line::plain("border")),
            El::caret(4, El::Line(Line::styled("input", SegStyle::plain()))),
            El::Line(Line::plain("border")),
        ]);
        let out = render(el);
        assert_eq!(out.caret, Some((1, 4)));
    }

    #[test]
    fn caret_on_an_empty_child_declares_nothing() {
        let out = render(El::caret(0, El::Rows(Vec::new())));
        assert!(out.caret.is_none());
    }
}
