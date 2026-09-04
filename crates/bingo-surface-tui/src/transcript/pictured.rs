//! The picture under the words: design §5's image row, as rows of the
//! transcript.
//!
//! A picture hangs under the block whose item carries it — under the `⎿` when
//! a tool answered with one, under a person's own line when they handed one
//! over. On a terminal that draws pictures it is the cells its placeholders
//! take ([`crate::graphics::kitty`]); on every other terminal it is the chip
//! that names it, which is the row's degrade and the only thing `--print` and
//! a chat channel ever had.
//!
//! Nothing here sends a byte. What it answers with is lines and a note of
//! which pictures those lines stand for; the sending is `run.rs`'s, between
//! frames.

use bingo_sdk::{Image, Item, ItemBody};
use ratatui::text::{Line, Span};

use super::{Block, Rows, returns, speaks_indent, under};
use crate::fold::Fold;
use crate::graphics::picture::{self, Source};
use crate::graphics::{self as graphics, Graphics, Picture, kitty};
use crate::theme;

/// How many rows of a picture a block that is only peeked at shows — a
/// glance, not the picture. `ctrl+o` opens it to its whole height (§7).
const IMAGE_ROWS: u16 = 12;

/// The item's block with its pictures under it.
pub(super) fn under_the_words(
    item: &Item,
    lines: Vec<Line<'static>>,
    fold: Fold,
    rows: &Rows<'_>,
) -> Block {
    let mut block = Block {
        lines,
        pictures: Vec::new(),
    };
    // A shut block shows nothing of what came back, and a picture is what
    // came back.
    if fold == Fold::Shut {
        return block;
    }
    let hangs = Hangs::of(item);
    for (part, image) in picture::pictures_of(&item.body).into_iter().enumerate() {
        one(&mut block, Where { item, part, hangs }, image, fold, rows);
    }
    block
}

/// Where one picture of an item is: which item, which of its pictures, and
/// under which mark.
#[derive(Clone, Copy)]
struct Where<'a> {
    item: &'a Item,
    part: usize,
    hangs: Hangs,
}

/// One picture: its cells where the terminal can draw it, the chip that names
/// it where it cannot.
fn one(block: &mut Block, at: Where<'_>, image: &Image, fold: Fold, rows: &Rows<'_>) {
    match drawn(at, image, fold, rows) {
        Some((picture, lines)) => {
            block.lines.extend(lines);
            block.pictures.push(picture);
        }
        None => block.lines.extend(at.hangs.chip(image, rows)),
    }
}

/// The cells one picture takes, and the note of which picture they stand for
/// — where this terminal draws pictures at all and a decoder read this one.
fn drawn(
    at: Where<'_>,
    image: &Image,
    fold: Fold,
    rows: &Rows<'_>,
) -> Option<(Picture, Vec<Line<'static>>)> {
    let Graphics::Kitty { cell, .. } = graphics::chosen() else {
        return None;
    };
    let source = Source::Journal {
        item: at.item.id.clone(),
        part: at.part,
    };
    let id = source.id();
    let png = rows.pictures.png(id, image)?;
    let size = (png.width, png.height);
    let (cols, tall) = picture::fit(size, cell, at.hangs.room(rows), height(fold));
    let cells = (0..tall)
        .map(|row| kitty::placeholder(id, row, cols))
        .collect();
    let picture = Picture {
        source,
        cols,
        rows: tall,
    };
    Some((picture, at.hangs.under(cells, rows)))
}

/// How tall a picture may be: a glance while the block is peeked at, and as
/// much as the protocol can number once it is opened.
fn height(fold: Fold) -> u16 {
    match fold {
        Fold::Open => kitty::MAX_CELLS,
        Fold::Peek | Fold::Shut => IMAGE_ROWS,
    }
}

/// Which mark a picture hangs from — the two places one can be.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Hangs {
    /// Under a `⎿`: a tool answered with it.
    Returned,
    /// Under a person's own line: they handed it over.
    Said,
}

impl Hangs {
    fn of(item: &Item) -> Self {
        match item.body {
            ItemBody::ToolCall { .. } => Hangs::Returned,
            _ => Hangs::Said,
        }
    }

    /// How many columns the picture has to draw in.
    fn room(self, rows: &Rows<'_>) -> u16 {
        let room = match self {
            Hangs::Returned => rows.result_width(),
            Hangs::Said => rows.measure().saturating_sub(speaks_indent()),
        };
        u16::try_from(room).unwrap_or(u16::MAX).max(1)
    }

    /// The rows, under the mark they belong to.
    fn under(self, cells: Vec<Line<'static>>, rows: &Rows<'_>) -> Vec<Line<'static>> {
        match self {
            Hangs::Returned => returns(cells, rows),
            // No glyph: the line above it is the person's own, and the
            // picture is part of what they said rather than an answer to it.
            Hangs::Said => under(
                Span::raw(" ".repeat(speaks_indent())),
                cells,
                speaks_indent(),
                rows.measure(),
            ),
        }
    }

    /// What a terminal that cannot draw the picture draws instead. A person's
    /// own line already carries the word for what they handed over — the
    /// `[image 1]` token or the path they typed (M45) — so nothing is added
    /// under it; a tool's answer has no such word, and says what was there.
    fn chip(self, image: &Image, rows: &Rows<'_>) -> Vec<Line<'static>> {
        match self {
            Hangs::Said => Vec::new(),
            Hangs::Returned => returns(
                vec![Line::from(Span::styled(
                    format!("[image: {}]", image.media_type),
                    theme::dim(),
                ))],
                rows,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::Blocks;
    use crate::graphics::{Decoded, kitty::PLACEHOLDER};
    use crate::test_support::{folded, item, scene, solo};
    use crate::tree::Agents;
    use bingo_pictures::testing::{png, unreadable};
    use bingo_sdk::{ContentPart, ItemStatus, Origin, ToolOutput};
    use ratatui::style::{Color, Style};

    /// A `Read` that answered with a picture and nothing else, which is what
    /// the fs tool does (ADR-0040 §1).
    fn read(image: &bingo_sdk::Image) -> Item {
        item(
            "itm_1",
            ItemStatus::Completed,
            ItemBody::ToolCall {
                call_id: "call_1".into(),
                name: "Read".into(),
                input: serde_json::json!({ "file_path": "shot.png" }),
                output: Some(ToolOutput {
                    parts: vec![ContentPart::Image(image.clone())],
                    display: None,
                    is_error: false,
                }),
                progress: None,
                duration_ms: Some(3),
            },
        )
    }

    /// A person's line with a picture behind its token (M45).
    fn pasted(image: &bingo_sdk::Image) -> Item {
        item(
            "itm_1",
            ItemStatus::Completed,
            ItemBody::User {
                parts: vec![
                    ContentPart::text("what is this? [image 1]"),
                    ContentPart::Image(image.clone()),
                ],
                origin: Origin::surface("tui"),
            },
        )
    }

    /// One item's block, on a terminal of this kind and at this fold.
    fn block(item: &Item, terminal: Graphics, fold: Fold) -> Block {
        let state = folded(Vec::new());
        let pictures = Decoded::default();
        let folds = match fold {
            Fold::Peek => crate::fold::Folds::new(),
            other => [(item.id.clone(), other)].into_iter().collect(),
        };
        let now = scene().1;
        let rows = Rows::of(&state, 60, &folds, &[], &pictures, now);
        let cue = crate::transcript::Cue {
            since: now.instant,
            flip: false,
        };
        graphics::with(terminal, || {
            super::super::item_block(item, None, &Agents::new(), &rows, cue)
        })
    }

    /// One finished item, as the frame the reducer folds.
    fn said(item: Item) -> bingo_sdk::Frame {
        crate::test_support::frame(1, bingo_sdk::Event::ItemCompleted { item })
    }

    fn text(block: &Block) -> Vec<String> {
        block.lines.iter().map(ToString::to_string).collect()
    }

    /// The cells of a picture, drawn under the `⎿` a tool's answer hangs from,
    /// and the note that says which picture they stand for.
    #[test]
    fn a_tool_that_answered_with_a_picture_draws_its_cells() {
        let block = block(&read(&png(100, 200)), graphics::drawing(), Fold::Peek);
        assert_eq!(
            block.pictures,
            vec![Picture {
                source: Source::Journal {
                    item: bingo_sdk::ItemId::from_raw("itm_1"),
                    part: 0,
                },
                cols: 10,
                rows: 10,
            }],
            "ten cells across and ten down, at 10×20 pixels a cell"
        );
        let drawn = text(&block);
        assert_eq!(drawn.len(), 11, "the row, then ten rows of picture");
        assert!(drawn[1].starts_with("  ⎿  "), "under the mark: {drawn:?}");
        assert_eq!(
            drawn[1].matches(PLACEHOLDER).count(),
            10,
            "and ten cells wide: {drawn:?}"
        );
        assert!(
            drawn[2].starts_with("     "),
            "the rows under it line up: {drawn:?}"
        );
    }

    /// Every cell of a picture carries the id it belongs to, as the colour
    /// the protocol reads it out of.
    #[test]
    fn every_cell_carries_the_picture_it_belongs_to() {
        let block = block(&read(&png(100, 200)), graphics::drawing(), Fold::Peek);
        let id = Source::Journal {
            item: bingo_sdk::ItemId::from_raw("itm_1"),
            part: 0,
        }
        .id();
        let colour = Style::new().fg(Color::Rgb(
            ((id >> 16) & 0xff) as u8,
            ((id >> 8) & 0xff) as u8,
            (id & 0xff) as u8,
        ));
        for line in &block.lines[1..] {
            let cells = line.spans.last().expect("a row of cells");
            assert_eq!(cells.style, colour, "{line:?}");
        }
    }

    /// The degrade of §5, on a terminal that draws no pictures.
    #[test]
    fn a_terminal_that_draws_no_pictures_draws_the_chip() {
        let block = block(&read(&png(100, 200)), Graphics::Off, Fold::Peek);
        assert_eq!(text(&block)[1], "  ⎿  [image: image/png]");
        assert!(block.pictures.is_empty(), "and nothing is sent");
    }

    /// A payload no decoder reads is the chip too, however good the terminal
    /// is: what cannot be drawn is named.
    #[test]
    fn a_picture_no_decoder_reads_is_the_chip_on_any_terminal() {
        let block = block(&read(&unreadable()), graphics::drawing(), Fold::Peek);
        assert_eq!(text(&block)[1], "  ⎿  [image: image/png]");
        assert!(block.pictures.is_empty());
    }

    /// A picture a person handed over draws under their own line, at the
    /// indent their words are at, with no `⎿`: it is part of what they said.
    #[test]
    fn a_picture_a_person_pasted_draws_under_their_line() {
        let block = block(&pasted(&png(100, 200)), graphics::drawing(), Fold::Peek);
        assert_eq!(block.pictures.len(), 1);
        let drawn = text(&block);
        assert!(drawn[0].contains("what is this? [image 1]"));
        assert!(!drawn[1].contains('⎿'), "no answer's mark: {drawn:?}");
        assert_eq!(drawn[1].matches(PLACEHOLDER).count(), 10);
    }

    /// And on a terminal that cannot draw it, nothing at all: the line above
    /// already carries the word for it (M45).
    #[test]
    fn a_picture_a_person_pasted_adds_no_chip() {
        let block = block(&pasted(&png(100, 200)), Graphics::Off, Fold::Peek);
        assert_eq!(text(&block).len(), 1, "the line they typed, and no more");
    }

    /// The fold decides how much of a picture is on the screen, as it decides
    /// how much of everything else is: a glance, the whole, or nothing.
    #[test]
    fn the_fold_says_how_much_of_a_picture_is_shown() {
        let tall = read(&png(100, 400));
        let peeked = block(&tall, graphics::drawing(), Fold::Peek);
        assert_eq!(
            peeked.pictures.first().map(|p| (p.cols, p.rows)),
            Some((6, IMAGE_ROWS)),
            "cut to a glance, and still the shape it is"
        );
        let opened = block(&tall, graphics::drawing(), Fold::Open);
        assert_eq!(
            opened.pictures.first().map(|p| (p.cols, p.rows)),
            Some((10, 20)),
            "the whole of it"
        );
        let shut = block(&tall, graphics::drawing(), Fold::Shut);
        assert!(shut.pictures.is_empty(), "and none of it");
        assert_eq!(text(&shut).len(), 1);
    }

    /// The whole way through, on a real buffer: a picture drawn into the
    /// transcript is cells of `U+10EEEE` on the screen, and the frame knows
    /// which picture they stand for.
    #[test]
    fn the_frame_draws_the_cells_and_names_the_picture() {
        let state = folded(vec![said(read(&png(100, 200)))]);
        let (ui, now) = scene();
        let screen = graphics::with(graphics::drawing(), || {
            let drawn = crate::test_support::draw_tree(80, 24, &solo(&state), &ui, now);
            let pictures = ui.painted.borrow().blocks.pictures();
            (drawn, pictures)
        });
        assert!(
            screen.0.contains(PLACEHOLDER),
            "the cells are on the screen:\n{}",
            screen.0
        );
        assert_eq!(
            screen
                .1
                .iter()
                .map(|p| (p.cols, p.rows))
                .collect::<Vec<_>>(),
            vec![(10, 10)],
            "and the frame knows what to send for them"
        );
    }

    /// What the shell is left with when the surface goes: the words, and not
    /// a row of cells standing for a picture the terminal was told to forget.
    #[test]
    fn a_pictures_cells_are_not_printed_back_into_the_shell() {
        let state = folded(vec![said(read(&png(100, 200)))]);
        let pictures = Decoded::default();
        let folds = crate::fold::Folds::new();
        let rows = Rows::of(&state, 60, &folds, &[], &pictures, scene().1);
        let tail = graphics::with(graphics::drawing(), || {
            let mut blocks = Blocks::default();
            let height = blocks.sync(&state, &Agents::new(), &rows, Vec::new());
            assert!(height > 1, "the picture was drawn in the first place");
            blocks.tail(height)
        });
        assert!(
            tail.iter().any(|line| line.contains("Read(shot.png)")),
            "the row survives: {tail:?}"
        );
        assert!(
            !tail.iter().any(|line| line.contains(PLACEHOLDER)),
            "and its cells do not: {tail:?}"
        );
    }

    /// A block is drawn once and cloned ever after; its picture has to
    /// survive that, or the second frame would forget what its own cells are
    /// standing for and the terminal would be told to let the picture go.
    #[test]
    fn a_block_kept_between_frames_keeps_its_picture() {
        let state = folded(vec![said(read(&png(100, 200)))]);
        let pictures = Decoded::default();
        let folds = crate::fold::Folds::new();
        let rows = Rows::of(&state, 60, &folds, &[], &pictures, scene().1);
        graphics::with(graphics::drawing(), || {
            let mut blocks = Blocks::default();
            blocks.sync(&state, &Agents::new(), &rows, Vec::new());
            let first = blocks.renders();
            blocks.sync(&state, &Agents::new(), &rows, Vec::new());
            assert_eq!(blocks.renders(), first, "nothing was drawn again");
            assert_eq!(blocks.pictures().len(), 1, "and the picture is still named");
        });
    }
}
