//! A band of thumbnails: a few pictures side by side, each small enough to
//! glance at, and a count where there were more than the band shows.
//!
//! One shape in two places. The composer's strip is the band of the pictures a
//! draft is carrying ([`crate::composer::strip`]); a person's own `>` block in
//! the transcript wears the same band above their words
//! ([`crate::transcript::pictured`]). What they saw before `⏎` and what the
//! record keeps of it are the same pictures at the same size, because they are
//! the same rows — written once, here.
//!
//! Nothing here reads a byte of a picture. A thumbnail is measured, never
//! decoded, and one whose pixels are not fitted yet is an empty slot this
//! frame and cells the next (M61).

use bingo_sdk::Image;
use ratatui::text::{Line, Span};

use super::picture::{self, Source};
use super::{Cell, Decoded, Picture, kitty};
use crate::theme;

/// How many thumbnails are drawn before the rest are counted.
pub const SHOWN: usize = 4;
/// How many rows the band is. Enough to see what a picture is of, few enough
/// that what stands under it is still the point of the screen.
pub const ROWS: u16 = 3;
/// How many columns one thumbnail may take.
pub const COLS: u16 = 12;
/// The blank column between two of them.
const GAP: u16 = 1;

/// The band as rows to draw and the pictures those rows stand for. The
/// pictures join the frame's placed list, so the terminal is sent them and
/// told to forget them by the one reconciler everything else goes through
/// ([`super::Stored`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Band {
    pub lines: Vec<Line<'static>>,
    pub pictures: Vec<Picture>,
}

impl Band {
    /// How many rows the band takes. Always the same rows when it is up at
    /// all: a band that changed height with the shortest picture in it would
    /// move whatever stands under it every time a picture arrived.
    pub fn height(&self) -> u16 {
        match self.pictures.is_empty() {
            true => 0,
            false => ROWS,
        }
    }
}

/// The band for these pictures, in this order, `width` columns wide: at most
/// [`SHOWN`] of them and no more than the width holds, then `+N` for the rest.
pub fn of(pictures: &[(Source, &Image)], cell: Cell, decoded: &Decoded, width: u16) -> Band {
    let thumbnails: Vec<Picture> = pictures
        .iter()
        .take(fitting(width))
        .filter_map(|(source, image)| thumbnail(source.clone(), image, cell, decoded))
        .collect();
    banded(thumbnails, pictures.len())
}

/// How many thumbnails a band this wide has room for — at least one, whatever
/// the width, so that whether there is a band at all is a question about the
/// pictures and not about the terminal's size. A narrow band cuts thumbnails,
/// it does not take the band away.
fn fitting(width: u16) -> usize {
    let each = COLS + GAP;
    usize::from(width.saturating_add(GAP) / each).clamp(1, SHOWN)
}

/// One picture, fitted into the band's own box. `None` where nothing has
/// measured it — what names the picture beside the band is what says it is
/// there, and a slot that drew a chip would say it twice.
///
/// Measured, never decoded: a picture's size is in its own header, so the
/// frame that asks is answered now, and only the pixels are late (M61).
fn thumbnail(source: Source, image: &Image, cell: Cell, decoded: &Decoded) -> Option<Picture> {
    let size = decoded.size(source.id(), image)?;
    let (cols, rows) = picture::fit(size, cell, COLS, ROWS);
    Some(Picture { source, cols, rows })
}

/// The thumbnails as [`ROWS`] rows, with the ones `of_many` left out counted
/// after the last. No thumbnail is no band: there is nothing to count beside.
fn banded(thumbnails: Vec<Picture>, of_many: usize) -> Band {
    if thumbnails.is_empty() {
        return Band::default();
    }
    let cut = of_many.saturating_sub(thumbnails.len());
    Band {
        lines: (0..ROWS)
            .map(|row| band_row(&thumbnails, row, cut))
            .collect(),
        pictures: thumbnails,
    }
}

/// One row of the band: every thumbnail's cells on it, a column apart.
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

/// One thumbnail's cells on one row of the band, blank above its top edge —
/// so every thumbnail stands on the band's last row, whatever its height, the
/// way a picture sits on a line of type.
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

    fn cell() -> Cell {
        let graphics::Graphics::Kitty { cell, .. } = graphics::drawing() else {
            unreachable!("`drawing` draws")
        };
        cell
    }

    /// A band of pictures of these sizes, each behind a draft token of its own.
    fn band(sizes: &[(u32, u32)], width: u16) -> Band {
        let images: Vec<Image> = sizes.iter().map(|(w, h)| png(*w, *h)).collect();
        let pictures: Vec<(Source, &Image)> = images
            .iter()
            .enumerate()
            .map(|(n, image)| {
                (
                    Source::Draft {
                        token: n as u32 + 1,
                    },
                    image,
                )
            })
            .collect();
        of(&pictures, cell(), &Decoded::default(), width)
    }

    fn width_of(line: &Line<'static>) -> usize {
        line.spans.iter().map(|span| span.content.width()).sum()
    }

    /// The band is its own three rows whatever is in it, and each picture is
    /// fitted into the band's box rather than the room around it.
    #[test]
    fn a_picture_is_three_rows_of_cells_in_the_bands_own_box() {
        let band = band(&[(100, 200)], 60);
        assert_eq!(band.height(), ROWS);
        assert_eq!(band.lines.len(), usize::from(ROWS));
        // 100×200 pixels of a 10×20 cell is 10 by 10 cells, cut to three rows.
        assert_eq!((band.pictures[0].cols, band.pictures[0].rows), (3, 3));
    }

    /// A wide picture takes one row of the band and stands on its floor, so
    /// its base is where every other thumbnail's is.
    #[test]
    fn a_short_thumbnail_stands_on_the_bands_last_row() {
        let band = band(&[(1200, 100)], 60);
        assert_eq!((band.pictures[0].cols, band.pictures[0].rows), (12, 1));
        let blank = |row: usize| {
            band.lines[row]
                .spans
                .iter()
                .all(|span| span.content.trim().is_empty())
        };
        assert!(blank(0) && blank(1), "the rows above it are air");
        assert!(!blank(2), "and the picture is on the last");
    }

    /// Four are shown side by side with a column between them; the fifth and
    /// everything after it is a count, on the floor row alone.
    #[test]
    fn four_are_shown_and_the_rest_are_counted() {
        let band = band(&[(100, 100); 6], 60);
        assert_eq!(band.pictures.len(), SHOWN);
        let last = &band.lines[usize::from(ROWS) - 1];
        assert_eq!(
            last.spans.last().map(|span| span.content.as_ref()),
            Some("+2")
        );
        assert!(
            band.lines[0]
                .spans
                .iter()
                .all(|span| !span.content.contains('+')),
            "and not above it: {:?}",
            band.lines[0]
        );
    }

    /// No picture is no band: nothing to draw and nothing to count beside.
    #[test]
    fn nothing_is_no_band() {
        assert_eq!(band(&[], 60), Band::default());
        assert_eq!(band(&[], 60).height(), 0);
    }

    /// A narrow band shows fewer thumbnails and still shows one: whether there
    /// is a band is a question about the pictures, so the rows the frame made
    /// room for are the rows that get drawn.
    #[test]
    fn a_narrow_band_cuts_thumbnails_and_never_the_band() {
        for (width, shown) in [(60u16, 4usize), (30, 2), (13, 1), (4, 1), (0, 1)] {
            let band = band(&[(100, 100); 4], width);
            assert_eq!(band.pictures.len(), shown, "{width} columns");
            assert_eq!(band.height(), ROWS, "{width} columns");
        }
    }

    /// Four thumbnails, their gaps and the count are within the width they
    /// were fitted to.
    #[test]
    fn four_thumbnails_and_a_count_fit_seventy_four_columns() {
        let band = band(&[(1200, 600); 5], 74);
        for line in &band.lines {
            assert!(width_of(line) <= 74, "{}", width_of(line));
        }
        assert_eq!(width_of(&band.lines[2]), 4 * 12 + 3 + 3, "and `+1` after");
    }

    /// The order is the caller's: the band draws the pictures it was handed,
    /// in the order it was handed them.
    #[test]
    fn the_band_keeps_the_order_it_was_given() {
        let wide = png(400, 100);
        let tall = png(100, 400);
        let pictures = vec![
            (Source::Draft { token: 2 }, &wide),
            (Source::Draft { token: 1 }, &tall),
        ];
        let band = of(&pictures, cell(), &Decoded::default(), 60);
        assert_eq!(
            band.pictures
                .iter()
                .map(|one| one.source.clone())
                .collect::<Vec<_>>(),
            vec![Source::Draft { token: 2 }, Source::Draft { token: 1 }]
        );
    }
}
