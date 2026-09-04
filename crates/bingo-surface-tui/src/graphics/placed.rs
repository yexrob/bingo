//! Where a frame drew a picture, read back out of the cells it drew.
//!
//! A picture reaches the screen as cells of `U+10EEEE` carrying its own number
//! in their colour ([`super::kitty`]), so the drawn lines *are* the record of
//! where each picture is. This reads that record back: no rectangle is
//! remembered beside cells that already say whose they are, and none can drift
//! from what is on the screen.
//!
//! Pure over the lines and the rectangle they were drawn in. It is what a
//! click on a picture is answered against ([`crate::ui::Painted::picture_at`]).

use ratatui::layout::Rect;
use ratatui::text::Line;
use unicode_width::UnicodeWidthStr;

use super::kitty;

/// One picture's cells, as a rectangle of the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placed {
    /// The number the terminal knows the picture by
    /// ([`super::picture::Source::id`]).
    pub id: u32,
    pub area: Rect,
}

/// Every picture whose cells are in these lines, drawn one line to a row from
/// `area`'s top left corner. The rows and columns are the screen's own, so a
/// click is answered by [`Rect::contains`] and nothing else.
pub fn cells(lines: &[Line<'_>], area: Rect) -> Vec<Placed> {
    let mut placed: Vec<Placed> = Vec::new();
    for (row, line) in lines.iter().enumerate() {
        let Some(row) = u16::try_from(row).ok().filter(|row| *row < area.height) else {
            break;
        };
        for (id, from, wide) in runs(line) {
            join(&mut placed, id, cell_row(area, row, from, wide));
        }
    }
    placed
}

/// One run of cells as a rectangle of one row, cut to the room the region has.
fn cell_row(area: Rect, row: u16, from: u16, wide: u16) -> Rect {
    let from = from.min(area.width);
    Rect {
        x: area.x + from,
        y: area.y + row,
        width: wide.min(area.width - from),
        height: 1,
    }
}

/// One more row of a picture's cells: the row directly under the last
/// rectangle of the same picture, in the same columns, grows it; anywhere else
/// starts another. Two drawings of one picture are two places, however alike
/// their numbers — a rectangle spanning the rows between them would answer for
/// the words there.
fn join(placed: &mut Vec<Placed>, id: u32, area: Rect) {
    if area.width == 0 {
        return;
    }
    if let Some(last) = placed.iter_mut().rev().find(|one| one.id == id)
        && last.area.x == area.x
        && last.area.width == area.width
        && last.area.bottom() == area.y
    {
        last.area.height += area.height;
        return;
    }
    placed.push(Placed { id, area });
}

/// The runs of placeholder cells on one line: which picture each is of, the
/// column of the line it starts in and how many columns it covers.
fn runs(line: &Line<'_>) -> Vec<(u32, u16, u16)> {
    let mut out: Vec<(u32, u16, u16)> = Vec::new();
    let mut column: u16 = 0;
    for span in &line.spans {
        let wide = u16::try_from(span.content.width()).unwrap_or(u16::MAX);
        if let Some(id) = kitty::pictured(span) {
            // A run the wrapper split into two spans is one run of cells.
            match out.last_mut() {
                Some((was, from, run)) if *was == id && *from + *run == column => *run += wide,
                _ => out.push((id, column, wide)),
            }
        }
        column = column.saturating_add(wide);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::text::Span;

    const AREA: Rect = Rect {
        x: 4,
        y: 2,
        width: 20,
        height: 6,
    };

    /// A row of `cols` cells of picture `id`, `at` columns in.
    fn row(id: u32, at: usize, cols: u16) -> Line<'static> {
        let mut spans = vec![Span::raw(" ".repeat(at))];
        spans.extend(kitty::placeholder(id, 0, cols).spans);
        Line::from(spans)
    }

    fn words(text: &str) -> Line<'static> {
        Line::from(Span::raw(text.to_string()))
    }

    /// A picture is one rectangle, in the screen's own rows and columns: the
    /// rows its cells were drawn on and the columns they stand in.
    #[test]
    fn a_pictures_rows_are_one_rectangle_of_the_screen() {
        let drawn = vec![words("⏺ [image: shot]"), row(7, 2, 10), row(7, 2, 10)];
        assert_eq!(
            cells(&drawn, AREA),
            vec![Placed {
                id: 7,
                area: Rect {
                    x: 6,
                    y: 3,
                    width: 10,
                    height: 2
                },
            }]
        );
    }

    /// Lines with no cells in them place nothing, whatever else they carry.
    #[test]
    fn words_place_no_picture() {
        assert!(cells(&[words("just words"), Line::default()], AREA).is_empty());
        assert!(cells(&[], AREA).is_empty());
    }

    /// Two pictures on one row are two rectangles — the strip's thumbnails,
    /// side by side with a column between them.
    #[test]
    fn two_pictures_on_one_row_are_two_rectangles() {
        let mut spans = kitty::placeholder(1, 0, 3).spans;
        spans.push(Span::raw(" ".to_string()));
        spans.extend(kitty::placeholder(2, 0, 4).spans);
        let placed = cells(&[Line::from(spans)], AREA);
        assert_eq!(
            placed
                .iter()
                .map(|one| (one.id, one.area.x, one.area.width))
                .collect::<Vec<_>>(),
            vec![(1u32, 4u16, 3u16), (2, 8, 4)],
        );
    }

    /// The same picture drawn twice is two places, not one rectangle over the
    /// rows between them: a click on the words in between is the words'.
    #[test]
    fn one_picture_drawn_twice_is_two_places() {
        let drawn = vec![row(7, 0, 4), words("and again"), row(7, 0, 4)];
        let placed = cells(&drawn, AREA);
        assert_eq!(placed.len(), 2, "{placed:?}");
        assert_eq!(placed[0].area.y, 2);
        assert_eq!(placed[1].area.y, 4);
        assert!(placed.iter().all(|one| one.area.height == 1));
    }

    /// A run that moved column between two rows is two rectangles too: only
    /// the row directly under the same columns grows one.
    #[test]
    fn a_run_that_moved_column_is_not_one_rectangle() {
        let placed = cells(&[row(7, 0, 4), row(7, 2, 4)], AREA);
        assert_eq!(placed.len(), 2, "{placed:?}");
    }

    /// Nothing is placed outside the region the lines were drawn in: rows past
    /// its height were never on the screen, and a run past its width is cut to
    /// what the terminal showed.
    #[test]
    fn nothing_is_placed_outside_the_region_it_was_drawn_in() {
        let short = Rect { height: 2, ..AREA };
        let placed = cells(&[row(7, 0, 4), row(7, 0, 4), row(7, 0, 4)], short);
        assert_eq!(placed.len(), 1);
        assert_eq!(placed[0].area.height, 2, "and only the rows it showed");

        let narrow = Rect { width: 6, ..AREA };
        let placed = cells(&[row(9, 2, 10)], narrow);
        assert_eq!(placed[0].area.width, 4, "cut to the region: {placed:?}");
        let past = cells(&[row(9, 8, 10)], narrow);
        assert!(past.is_empty(), "and a run wholly past it: {past:?}");
    }

    /// A run the wrapper split into two spans is one run of cells: what makes
    /// a rectangle is the picture and the columns, never the spans.
    #[test]
    fn a_split_run_of_cells_is_one_rectangle() {
        let mut spans = kitty::placeholder(3, 0, 2).spans;
        spans.extend(kitty::placeholder(3, 0, 3).spans);
        let placed = cells(&[Line::from(spans)], AREA);
        assert_eq!(placed.len(), 1, "{placed:?}");
        assert_eq!(placed[0].area.width, 5);
    }
}
