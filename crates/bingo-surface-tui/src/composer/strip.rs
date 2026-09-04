//! The thumbnails of the pictures the draft is carrying, standing on the
//! input box's top border in rows of their own (outside the box since 2026-09-04,
//! at the user's word: the box keeps its height whatever is pasted).
//!
//! `[image 2]` stays in the words — it is the record, and the anchor in the
//! sentence, so deleting it takes its picture with it (M45). The strip is
//! that record made visible: the same pictures, in the same order, small
//! enough to glance at. It is derived from the line and from nothing else, so
//! there is nothing to keep in step.
//!
//! Only a terminal that draws pictures gets one. Everywhere else the box is
//! exactly what it was: the tokens in the line already say what is attached.

use bingo_sdk::Image;
use ratatui::text::{Line, Span};

use crate::graphics::picture::{self, Source};
use crate::graphics::{Cell, Decoded, Graphics, Picture, kitty};
use crate::pictures::Held;
use crate::theme;

/// How many thumbnails are drawn before the rest are counted.
pub const SHOWN: usize = 4;
/// How many rows the strip is. Enough to see what a picture is of, few enough
/// that the box under it is still the point of the screen's bottom.
pub const ROWS: u16 = 3;
/// How many columns one thumbnail may take.
pub const COLS: u16 = 12;
/// The blank column between two of them.
const GAP: u16 = 1;

/// The strip as rows to draw and the pictures those rows stand for. The
/// pictures join the frame's placed list, so the terminal is sent them and
/// told to forget them by the one reconciler everything else goes through
/// ([`crate::graphics::Stored`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Strip {
    pub lines: Vec<Line<'static>>,
    pub pictures: Vec<Picture>,
}

impl Strip {
    /// How many rows of the box the strip takes. Always the same rows when it
    /// is up at all: a band that changed height with the shortest picture in
    /// it would move the draft above it every time a picture was pasted.
    pub fn height(&self) -> u16 {
        match self.pictures.is_empty() {
            true => 0,
            false => ROWS,
        }
    }
}

/// The strip for a line, `width` columns wide.
pub fn rows(held: &Held, line: &str, graphics: Graphics, decoded: &Decoded, width: u16) -> Strip {
    let Graphics::Kitty { cell, .. } = graphics else {
        return Strip::default();
    };
    let carried = held.shown(line);
    let pictures: Vec<Picture> = carried
        .iter()
        .take(fitting(width))
        .filter_map(|(token, image)| thumbnail(*token, image, cell, decoded))
        .collect();
    if pictures.is_empty() {
        return Strip::default();
    }
    Strip {
        lines: banded(&pictures, carried.len() - pictures.len()),
        pictures,
    }
}

/// How many thumbnails a box this wide has room for — at least one, whatever
/// the width, so that whether there is a strip at all is a question about the
/// line and not about the terminal's size. A narrow box cuts thumbnails, it
/// does not take the band away.
fn fitting(width: u16) -> usize {
    let each = COLS + GAP;
    usize::from(width.saturating_add(GAP) / each).clamp(1, SHOWN)
}

/// One held picture, fitted into the strip's own box. `None` where no decoder
/// read it — the token in the line is what says it is there, and a chip in
/// the box would say it twice.
fn thumbnail(token: u32, image: &Image, cell: Cell, decoded: &Decoded) -> Option<Picture> {
    let source = Source::Draft { token };
    let png = decoded.png(source.id(), image)?;
    let (cols, rows) = picture::fit((png.width, png.height), cell, COLS, ROWS);
    Some(Picture { source, cols, rows })
}

/// The band: [`ROWS`] rows with the thumbnails standing on the last of them,
/// so they sit on the prompt row the way a picture sits on a line of type.
fn banded(thumbnails: &[Picture], cut: usize) -> Vec<Line<'static>> {
    (0..ROWS)
        .map(|row| band_row(thumbnails, row, cut))
        .collect()
}

fn band_row(thumbnails: &[Picture], row: u16, cut: usize) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for one in thumbnails {
        if !spans.is_empty() {
            spans.push(Span::raw(" ".repeat(usize::from(GAP))));
        }
        spans.extend(cells(one, row));
    }
    if cut > 0 && row + 1 == ROWS {
        spans.push(Span::raw(" ".repeat(usize::from(GAP))));
        spans.push(Span::styled(format!("+{cut}"), theme::dim()));
    }
    Line::from(spans)
}

/// One thumbnail's cells on one row of the band, blank above its top edge.
fn cells(picture: &Picture, row: u16) -> Vec<Span<'static>> {
    match row.checked_sub(ROWS - picture.rows) {
        Some(of) => kitty::placeholder(picture.id(), of, picture.cols).spans,
        None => vec![Span::raw(" ".repeat(usize::from(picture.cols)))],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics;
    use bingo_pictures::testing::png;
    use unicode_width::UnicodeWidthStr;

    /// The pictures of a line, held under the tokens it names.
    fn holding(pictures: &[(u32, u32)]) -> (Held, String) {
        let mut held = Held::default();
        let mut line = String::new();
        for (width, height) in pictures {
            let token = held.hold(&line, png(*width, *height));
            line.push_str(&crate::pictures::placeholder(token));
        }
        (held, line)
    }

    fn strip(pictures: &[(u32, u32)], width: u16) -> Strip {
        let (held, line) = holding(pictures);
        rows(
            &held,
            &line,
            graphics::drawing(),
            &Decoded::default(),
            width,
        )
    }

    fn width_of(line: &Line<'static>) -> usize {
        line.spans.iter().map(|span| span.content.width()).sum()
    }

    /// The band is the strip's own three rows whatever is in it, and each
    /// picture is fitted into the strip's box rather than the block's.
    #[test]
    fn a_carried_picture_is_three_rows_of_cells_in_the_box() {
        let strip = strip(&[(100, 200)], 60);
        assert_eq!(strip.height(), ROWS);
        assert_eq!(strip.lines.len(), usize::from(ROWS));
        assert_eq!(strip.pictures.len(), 1);
        // 100×200 pixels of a 10×20 cell is 10 by 10 cells, cut to three rows.
        assert_eq!((strip.pictures[0].cols, strip.pictures[0].rows), (3, 3));
        assert!(matches!(
            strip.pictures[0].source,
            Source::Draft { token: 1 }
        ));
    }

    /// A wide picture takes one row of the band and stands on its floor, so
    /// its base is where every other thumbnail's is.
    #[test]
    fn a_short_thumbnail_stands_on_the_bands_last_row() {
        let strip = strip(&[(1200, 100)], 60);
        assert_eq!((strip.pictures[0].cols, strip.pictures[0].rows), (12, 1));
        let blank = |row: usize| {
            strip.lines[row]
                .spans
                .iter()
                .all(|span| span.content.trim().is_empty())
        };
        assert!(blank(0) && blank(1), "the rows above it are air");
        assert!(!blank(2), "and the picture is on the last");
    }

    /// Four are shown side by side with a column between them; the fifth and
    /// everything after it is a count.
    #[test]
    fn four_are_shown_and_the_rest_are_counted() {
        let strip = strip(&[(100, 100); 6], 60);
        assert_eq!(strip.pictures.len(), SHOWN);
        let last = &strip.lines[usize::from(ROWS) - 1];
        assert_eq!(
            last.spans.last().map(|span| span.content.as_ref()),
            Some("+2")
        );
        assert!(
            strip.lines[0]
                .spans
                .iter()
                .all(|span| !span.content.contains('+')),
            "and the count is on the floor row alone: {:?}",
            strip.lines[0]
        );
    }

    /// The token order is the line's, not the paste order: a picture moved in
    /// the sentence moves in the strip.
    #[test]
    fn the_strip_is_in_the_lines_own_order() {
        let mut held = Held::default();
        held.hold("", png(100, 100));
        held.hold("[image 1]", png(200, 200));
        let drawn = rows(
            &held,
            "[image 2] then [image 1]",
            graphics::drawing(),
            &Decoded::default(),
            60,
        );
        let tokens: Vec<&Source> = drawn.pictures.iter().map(|one| &one.source).collect();
        assert_eq!(
            tokens,
            vec![&Source::Draft { token: 2 }, &Source::Draft { token: 1 }]
        );
    }

    /// A line that names nothing held has no strip, and neither has one on a
    /// terminal that draws no pictures.
    #[test]
    fn a_line_with_no_picture_and_a_terminal_with_none_have_no_strip() {
        assert_eq!(strip(&[], 60), Strip::default());
        assert_eq!(strip(&[], 60).height(), 0);
        let (held, line) = holding(&[(100, 100)]);
        let off = rows(&held, &line, Graphics::Off, &Decoded::default(), 60);
        assert_eq!(off, Strip::default());
        let typed = rows(
            &Held::default(),
            "[image 1]",
            graphics::drawing(),
            &Decoded::default(),
            60,
        );
        assert_eq!(typed, Strip::default(), "a token typed by hand is words");
    }

    /// A narrow box shows fewer of them and still shows one: whether there is
    /// a band is a question about the line, so the rows the frame made room
    /// for are the rows that get drawn.
    #[test]
    fn a_narrow_box_cuts_thumbnails_and_never_the_band() {
        for (width, shown) in [(60u16, 4usize), (30, 2), (13, 1), (4, 1), (0, 1)] {
            let strip = strip(&[(100, 100); 4], width);
            assert_eq!(strip.pictures.len(), shown, "{width} columns");
            assert_eq!(strip.height(), ROWS, "{width} columns");
        }
    }

    /// Four thumbnails and their gaps are within the box a terminal of eighty
    /// columns gives the composer.
    #[test]
    fn four_thumbnails_fit_the_box_of_an_eighty_column_terminal() {
        let strip = strip(&[(1200, 600); 5], 74);
        for line in &strip.lines {
            assert!(width_of(line) <= 74, "{}", width_of(line));
        }
        assert_eq!(width_of(&strip.lines[2]), 4 * 12 + 3 + 3, "and `+1` after");
    }
}
