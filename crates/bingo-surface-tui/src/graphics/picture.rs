//! One picture a frame drew: where it came from, and how many cells of the
//! screen it took.
//!
//! It carries no bytes. The picture itself lives where it already lived — an
//! item the reducer folded, or the composer's own held pictures — and this
//! says which one and how big it was drawn, so a frame that places a picture
//! costs a handful of integers rather than a copy of it.

use bingo_sdk::{ContentPart, Image, ItemBody, ItemId, SessionState};

use super::Cell;
use super::kitty::MAX_CELLS;
use crate::pictures::Held;

/// Where a picture on the screen came from: the two places this surface has
/// one to draw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// The journal: which item, and which of its pictures
    /// ([`pictures_of`]'s order).
    Journal { item: ItemId, part: usize },
    /// The draft: a pasted picture behind the composer's line, under the
    /// `[image N]` that names it (M45).
    Draft { token: u32 },
}

/// A picture a frame placed, and the rectangle its placeholders cover.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    pub source: Source,
    pub cols: u16,
    pub rows: u16,
}

impl Source {
    /// The number the terminal knows this picture by: a stable hash of where
    /// it is, so a redraw asks for the picture already sent rather than
    /// sending it again, and neither the row that draws it nor the send that
    /// follows has to pass the other a number.
    ///
    /// Twenty-four bits, because that is what a foreground colour carries;
    /// never zero, which the protocol keeps for "no id"; and the top of them
    /// says which kind of picture it is, so a draft's number can never be a
    /// journal picture's whatever the hash does.
    pub fn id(&self) -> u32 {
        match self {
            Source::Journal { item, part } => hashed(item.as_str().as_bytes(), &part.to_le_bytes()),
            Source::Draft { token } => DRAFT | hashed(b"draft", &token.to_le_bytes()),
        }
    }
}

impl Picture {
    pub fn id(&self) -> u32 {
        self.source.id()
    }

    /// How many pixels the cells this picture was drawn into hold: the size
    /// it is worth sending at, since nothing past it reaches the screen.
    pub fn pixels(&self, cell: Cell) -> (u32, u32) {
        (
            u32::from(self.cols) * u32::from(cell.width),
            u32::from(self.rows) * u32::from(cell.height),
        )
    }

    /// The picture itself, read back out of where it came from — one lookup
    /// that knows both places, so the send needs no second one.
    pub fn image_in<'a>(&self, state: &'a SessionState, held: &'a Held) -> Option<&'a Image> {
        match &self.source {
            Source::Journal { item, part } => {
                let found = state.items.iter().find(|kept| kept.id == *item)?;
                pictures_of(&found.body).get(*part).copied()
            }
            Source::Draft { token } => held.under(*token),
        }
    }
}

/// The bit that says a number is a draft's, and the bits the hash gets.
const DRAFT: u32 = 0x80_0000;
const MASK: u32 = 0x7f_ffff;

/// Twenty-three bits of FNV-1a over a name and a number, never zero.
fn hashed(name: &[u8], number: &[u8]) -> u32 {
    let mut hash = FNV_OFFSET;
    for byte in name.iter().chain(number.iter()) {
        hash = fold(hash, *byte);
    }
    (hash & MASK).max(1)
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
            source: Source::Journal {
                item: ItemId::from_raw(item),
                part,
            },
            cols: 1,
            rows: 1,
        }
    }

    fn draft(token: u32) -> Picture {
        Picture {
            source: Source::Draft { token },
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
        assert_eq!(draft(3).id(), draft(3).id());
        assert_ne!(draft(3).id(), draft(4).id());
    }

    #[test]
    fn an_id_fits_a_colour_and_is_never_the_protocols_none() {
        for i in 0..2000 {
            for id in [picture(&format!("itm_{i}"), i).id(), draft(i as u32).id()] {
                assert!(id > 0 && id <= 0xff_ffff, "{id:#x}");
            }
        }
    }

    /// A draft's number cannot be a journal picture's — not because a hash is
    /// lucky, but because the two halves of the range are disjoint. A number
    /// that collided would put the wrong picture on the screen.
    #[test]
    fn a_drafts_number_is_never_a_journal_pictures() {
        let journal: std::collections::BTreeSet<u32> = (0..4000)
            .map(|i| picture(&format!("itm_{i}"), i % 7).id())
            .collect();
        for token in 0..4000u32 {
            assert!(!journal.contains(&draft(token).id()), "{token}");
        }
    }

    /// The two places a picture may be, through one lookup.
    #[test]
    fn a_draft_is_read_out_of_what_the_composer_is_holding() {
        let state = crate::test_support::folded(Vec::new());
        let mut held = Held::default();
        let token = held.hold("", image("pasted"));
        assert_eq!(draft(token).image_in(&state, &held), Some(&image("pasted")));
        assert_eq!(draft(token + 1).image_in(&state, &held), None);
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
        let held = Held::default();
        assert_eq!(
            picture("itm_9", 0).image_in(&state, &held),
            Some(&image("one"))
        );
        assert_eq!(
            picture("itm_9", 1).image_in(&state, &held),
            None,
            "no second part"
        );
        assert_eq!(
            picture("itm_8", 0).image_in(&state, &held),
            None,
            "no such item"
        );
    }

    /// What a rectangle of cells holds, which is all that is worth sending.
    #[test]
    fn a_pictures_pixels_are_the_pixels_of_the_cells_it_covers() {
        let block = Picture {
            cols: 40,
            rows: 12,
            ..picture("itm_1", 0)
        };
        assert_eq!(block.pixels(CELL), (400, 240));
        assert_eq!(picture("itm_1", 0).pixels(CELL), (10, 20));
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
