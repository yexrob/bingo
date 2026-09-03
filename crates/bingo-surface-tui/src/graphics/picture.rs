//! One picture a frame drew: where in the journal it came from, and how many
//! cells of the screen it took.
//!
//! It carries no bytes. The picture itself lives in the item the reducer
//! folded, and this says which one and how big it was drawn — so a frame that
//! places a picture costs a handful of integers rather than a copy of it.

use bingo_sdk::{ContentPart, Image, ItemBody, ItemId, SessionState};

use super::Cell;
use super::kitty::MAX_CELLS;

/// A picture the transcript placed, and the rectangle its placeholders cover.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    pub item: ItemId,
    /// Which of the item's pictures this is ([`pictures_of`]'s order).
    pub part: usize,
    pub cols: u16,
    pub rows: u16,
}

impl Picture {
    /// The number the terminal knows this picture by: a stable hash of where
    /// it sits in the journal, so a redraw asks for the picture already sent
    /// rather than sending it again. Twenty-four bits, because that is what a
    /// foreground colour carries, and never zero, which the protocol keeps
    /// for "no id".
    pub fn id(&self) -> u32 {
        id_of(&self.item, self.part)
    }

    /// The picture itself, read back out of the session that was drawn.
    pub fn image_in<'a>(&self, state: &'a SessionState) -> Option<&'a Image> {
        let item = state.items.iter().find(|item| item.id == self.item)?;
        pictures_of(&item.body).get(self.part).copied()
    }
}

/// The number the terminal knows a picture by, from where it sits in the
/// journal alone — so the row that draws it and the send that follows ask
/// about it under the same name, without either passing the other a number.
pub fn id_of(item: &ItemId, part: usize) -> u32 {
    let mut hash = FNV_OFFSET;
    for byte in item.as_str().as_bytes() {
        hash = fold(hash, *byte);
    }
    for byte in part.to_le_bytes() {
        hash = fold(hash, byte);
    }
    (hash & 0xff_ffff).max(1)
}

/// The pictures one item carries, in the order they were said: a person's own
/// parts, or what a tool answered with. One reading of where pictures live, so
/// the index the transcript drew and the one the sender resolves are the same
/// number.
pub fn pictures_of(body: &ItemBody) -> Vec<&Image> {
    match body {
        ItemBody::User { parts, .. } => images(parts),
        ItemBody::ToolCall {
            output: Some(output),
            ..
        } => images(&output.parts),
        _ => Vec::new(),
    }
}

fn images(parts: &[ContentPart]) -> Vec<&Image> {
    parts
        .iter()
        .filter_map(|part| match part {
            ContentPart::Image(image) => Some(image),
            _ => None,
        })
        .collect()
}

/// How many cells a picture of this many pixels takes, at most this many of
/// them, with its shape kept: the tighter of the two limits decides, and
/// nothing ever comes out as no cells at all.
pub fn fit(pixels: (u32, u32), cell: Cell, max_cols: u16, max_rows: u16) -> (u16, u16) {
    let max_cols = max_cols.clamp(1, MAX_CELLS);
    let max_rows = max_rows.clamp(1, MAX_CELLS);
    let cols = cells(pixels.0, cell.width);
    let rows = cells(pixels.1, cell.height);
    if cols <= u32::from(max_cols) && rows <= u32::from(max_rows) {
        return (cols as u16, rows as u16);
    }
    // Which edge runs out first: comparing the two ratios without dividing.
    match cols * u32::from(max_rows) > rows * u32::from(max_cols) {
        true => (max_cols, shrunk(rows, u32::from(max_cols), cols)),
        false => (shrunk(cols, u32::from(max_rows), rows), max_rows),
    }
}

/// A length in pixels as whole cells, rounded up: half a cell of picture
/// still needs a cell to be drawn in.
fn cells(pixels: u32, per_cell: u16) -> u32 {
    let per_cell = u32::from(per_cell.max(1));
    pixels.div_ceil(per_cell).max(1)
}

/// `length` scaled by `limit / whole`, never to nothing.
fn shrunk(length: u32, limit: u32, whole: u32) -> u16 {
    let scaled = length.saturating_mul(limit) / whole.max(1);
    scaled.clamp(1, u32::from(MAX_CELLS)) as u16
}

const FNV_OFFSET: u32 = 0x811c_9dc5;
const FNV_PRIME: u32 = 0x0100_0193;

fn fold(hash: u32, byte: u8) -> u32 {
    (hash ^ u32::from(byte)).wrapping_mul(FNV_PRIME)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_sdk::{ItemStatus, Origin, ToolOutput};

    fn picture(item: &str, part: usize) -> Picture {
        Picture {
            item: ItemId::from_raw(item),
            part,
            cols: 1,
            rows: 1,
        }
    }

    fn image(data: &str) -> Image {
        Image {
            media_type: "image/png".into(),
            data: data.into(),
        }
    }

    /// The id is a function of where the picture is and of nothing else: the
    /// same place always answers the same number, two places never the same
    /// one, and how big it was drawn does not enter into it.
    #[test]
    fn the_id_is_where_the_picture_is_and_nothing_else() {
        assert_eq!(picture("itm_1", 0).id(), picture("itm_1", 0).id());
        assert_ne!(picture("itm_1", 0).id(), picture("itm_1", 1).id());
        assert_ne!(picture("itm_1", 0).id(), picture("itm_2", 0).id());
        let bigger = Picture {
            cols: 40,
            rows: 12,
            ..picture("itm_1", 0)
        };
        assert_eq!(bigger.id(), picture("itm_1", 0).id());
    }

    #[test]
    fn an_id_fits_a_colour_and_is_never_the_protocols_none() {
        for i in 0..2000 {
            let id = picture(&format!("itm_{i}"), i).id();
            assert!(id > 0 && id <= 0xff_ffff, "{id:#x}");
        }
    }

    /// Where pictures live: a person's own parts and what a tool answered
    /// with, in order, and nowhere else.
    #[test]
    fn the_pictures_of_an_item_are_its_parts_in_order() {
        let said = ItemBody::User {
            parts: vec![
                ContentPart::text("look"),
                ContentPart::Image(image("one")),
                ContentPart::Image(image("two")),
            ],
            origin: Origin::surface("tui"),
        };
        assert_eq!(
            pictures_of(&said),
            vec![&image("one"), &image("two")],
            "the words are not pictures"
        );

        let read = ItemBody::ToolCall {
            call_id: "call_1".into(),
            name: "Read".into(),
            input: serde_json::json!({}),
            output: Some(ToolOutput {
                parts: vec![ContentPart::Image(image("shot"))],
                display: None,
                is_error: false,
            }),
            progress: None,
            duration_ms: None,
        };
        assert_eq!(pictures_of(&read), vec![&image("shot")]);

        assert!(
            pictures_of(&ItemBody::Assistant {
                text: "no picture".into()
            })
            .is_empty()
        );
        assert!(
            pictures_of(&ItemBody::ToolCall {
                call_id: "call_1".into(),
                name: "Read".into(),
                input: serde_json::json!({}),
                output: None,
                progress: None,
                duration_ms: None,
            })
            .is_empty(),
            "a call still running answered with nothing"
        );
    }

    #[test]
    fn a_picture_finds_itself_in_the_session_it_was_drawn_from() {
        let mut state = crate::test_support::folded(Vec::new());
        state.items.push(crate::test_support::item(
            "itm_9",
            ItemStatus::Completed,
            ItemBody::User {
                parts: vec![ContentPart::text("look"), ContentPart::Image(image("one"))],
                origin: Origin::surface("tui"),
            },
        ));
        assert_eq!(picture("itm_9", 0).image_in(&state), Some(&image("one")));
        assert_eq!(picture("itm_9", 1).image_in(&state), None, "no second part");
        assert_eq!(picture("itm_8", 0).image_in(&state), None, "no such item");
    }

    const CELL: Cell = Cell {
        width: 10,
        height: 20,
    };

    /// A picture small enough takes the cells it needs, rounded up.
    #[test]
    fn a_picture_that_fits_takes_the_cells_it_covers() {
        assert_eq!(fit((100, 200), CELL, 40, 12), (10, 10));
        assert_eq!(fit((95, 195), CELL, 40, 12), (10, 10), "rounded up");
        assert_eq!(fit((1, 1), CELL, 40, 12), (1, 1), "never nothing");
    }

    /// Too tall, too wide, and too much of both: the shape is kept and the
    /// tighter limit is the one that binds.
    #[test]
    fn a_picture_too_big_keeps_its_shape() {
        assert_eq!(fit((100, 800), CELL, 40, 12), (3, 12), "height binds");
        assert_eq!(fit((800, 200), CELL, 40, 12), (40, 5), "width binds");
        assert_eq!(fit((4000, 4000), CELL, 40, 12), (24, 12));
        assert_eq!(
            fit((10_000, 10), CELL, 40, 12),
            (40, 1),
            "and a sliver still draws a row"
        );
    }

    /// However wide the terminal is, no picture asks for a cell the protocol
    /// has no way to number.
    #[test]
    fn nothing_is_drawn_past_the_end_of_the_diacritics() {
        // Twice as wide as it is tall in cells, so the width is what the
        // table binds and the height follows it.
        assert_eq!(fit((100_000, 100_000), CELL, 1_000, 1_000), (128, 64));
        assert_eq!(fit((100, 100_000), CELL, 1_000, 1_000).1, MAX_CELLS);
    }
}
