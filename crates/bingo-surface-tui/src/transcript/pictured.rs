//! The picture the words carry: design §5's image row, as rows of the
//! transcript.
//!
//! A picture hangs under the block whose item carries it — under the `⎿` when
//! a tool answered with one, under a person's own line when they handed one
//! over ([`under_the_words`]) — or under its own chip line, where the words of
//! an answer named it themselves ([`in_the_words`]). On a terminal that draws
//! pictures it is the cells its placeholders take
//! ([`crate::graphics::kitty`]); on every other terminal it is the chip that
//! names it, which is the row's degrade and the only thing `--print` and a
//! chat channel ever had.
//!
//! Nothing here sends a byte, and nothing here reads one. What it answers with
//! is lines, a note of which pictures those lines stand for, and the
//! destinations that have still to be read in; the sending and the reading are
//! both `run.rs`'s, between frames.

use bingo_sdk::{Image, Item, ItemBody};
use ratatui::text::{Line, Span};

use super::{Block, Rows, returns, speaks_indent, under};
use crate::fold::Fold;
use crate::graphics::picture::{self, Source};
use crate::graphics::{self as graphics, Graphics, Picture, kitty};
use crate::{markdown, theme};

/// How many rows of a picture a block that is only peeked at shows — a
/// glance, not the picture. `ctrl+o` opens it to its whole height (§7).
const IMAGE_ROWS: u16 = 12;

/// The item's block with the pictures its own parts carry under it.
pub(super) fn under_the_words(item: &Item, mut block: Block, fold: Fold, rows: &Rows<'_>) -> Block {
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
    let source = Source::Journal {
        item: at.item.id.clone(),
        part: at.part,
    };
    match drawn(source, image, at.hangs.room(rows), height(fold), rows) {
        Some((picture, cells)) => {
            block.lines.extend(at.hangs.under(cells, rows));
            block.pictures.push(picture);
        }
        None => block.lines.extend(at.hangs.chip(image, rows)),
    }
}

/// The pictures an answer's own words named (M51): under each chip line the
/// markdown left, the cells of the one that has been read in — and after the
/// name, in dim, the reason there is none.
///
/// The chip stays where the picture draws. It is the line the picture hangs
/// from, and it is what a person reads when the fold shuts or the terminal
/// draws nothing.
pub(super) fn in_the_words(
    lines: Vec<Line<'static>>,
    images: &[markdown::Linked],
    fold: Fold,
    rows: &Rows<'_>,
) -> Block {
    let mut block = Block {
        lines,
        pictures: Vec::new(),
        wanted: images.iter().map(|image| image.dest.clone()).collect(),
    };
    // Backwards: rows put under one chip move every line after it, and a
    // destination's own line was measured before any of them were.
    for image in images.iter().rev() {
        named(&mut block, image, fold, rows);
    }
    block.pictures.reverse();
    block
}

/// One picture the words named, hanging from the chip that already stands for
/// it.
fn named(block: &mut Block, image: &markdown::Linked, fold: Fold, rows: &Rows<'_>) {
    let Some(read_in) = rows.linked.image(&image.dest) else {
        return note(block, image, rows);
    };
    let source = Source::Linked {
        dest: image.dest.clone(),
    };
    // The chip's own column, so the picture stands where the words it belongs
    // to stand rather than at the block's edge (M56).
    let hangs = Hangs::Said {
        indent: image.indent,
    };
    let Some((picture, cells)) = drawn(source, read_in, hangs.room(rows), height(fold), rows)
    else {
        return;
    };
    let under = (image.line + 1).min(block.lines.len());
    block
        .lines
        .splice(under..under, at_column(cells, image.indent));
    block.pictures.push(picture);
}

/// The same rows, moved right by `indent` columns — a picture among an
/// answer's words, whose lines are put in before the block is marked and
/// indented under its `⏺` ([`in_the_words`]).
fn at_column(cells: Vec<Line<'static>>, indent: usize) -> Vec<Line<'static>> {
    if indent == 0 {
        return cells;
    }
    cells
        .into_iter()
        .map(|line| {
            let mut spans = vec![Span::raw(" ".repeat(indent))];
            spans.extend(line.spans);
            Line::from(spans)
        })
        .collect()
}

/// Why a picture the words named draws none, in dim after its name. Nothing at
/// all while it is still being read in: a chip that said why the moment it was
/// written would be wrong for as long as the reading takes.
fn note(block: &mut Block, image: &markdown::Linked, rows: &Rows<'_>) {
    let Some(why) = rows.linked.failure(&image.dest) else {
        return;
    };
    if let Some(chip) = block.lines.get_mut(image.line) {
        chip.spans
            .push(Span::styled(format!(" ({why})"), theme::dim()));
    }
}

/// The cells one picture takes, and the note of which picture they stand for
/// — where this terminal draws pictures at all and a decoder read this one.
fn drawn(
    source: Source,
    image: &Image,
    room: u16,
    tall: u16,
    rows: &Rows<'_>,
) -> Option<(Picture, Vec<Line<'static>>)> {
    let Graphics::Kitty { cell, .. } = graphics::chosen() else {
        return None;
    };
    let id = source.id();
    // Measured, never decoded: how many cells a picture takes is where every
    // row under it goes, so a frame must have the answer now (M61).
    let (cols, tall) = picture::fit(rows.pictures.size(id, image)?, cell, room, tall);
    let cells = (0..tall)
        .map(|row| kitty::placeholder(id, row, cols))
        .collect();
    let picture = Picture {
        source,
        cols,
        rows: tall,
    };
    Some((picture, cells))
}

/// How tall a picture may be: a glance while the block is peeked at, and as
/// much as the protocol can number once it is opened.
fn height(fold: Fold) -> u16 {
    match fold {
        Fold::Open => kitty::MAX_CELLS,
        Fold::Peek | Fold::Shut => IMAGE_ROWS,
    }
}

/// Which mark a picture hangs from, and the column it stands in past that
/// mark — the two places one can be.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Hangs {
    /// Under a `⎿`: a tool answered with it, in the gutter's own column.
    Returned,
    /// Under the `⏺` indent, `indent` columns further in: the indent of the
    /// markdown construct an answer wrote it in — a list item's marker, a
    /// quote's bar — and nothing at all for a picture a person handed over,
    /// which stands where their words do (M56).
    Said { indent: usize },
}

impl Hangs {
    fn of(item: &Item) -> Self {
        match item.body {
            ItemBody::ToolCall { .. } => Hangs::Returned,
            _ => Hangs::Said { indent: 0 },
        }
    }

    /// How many columns the picture has to draw in: what is left of the
    /// measure once the mark and the column have taken theirs, so a picture is
    /// fitted to the room it has and never pushed past the right margin.
    fn room(self, rows: &Rows<'_>) -> u16 {
        let room = match self {
            Hangs::Returned => rows.result_width(),
            Hangs::Said { indent } => rows.measure().saturating_sub(speaks_indent() + indent),
        };
        u16::try_from(room).unwrap_or(u16::MAX).max(1)
    }

    /// The rows, under the mark they belong to.
    fn under(self, cells: Vec<Line<'static>>, rows: &Rows<'_>) -> Vec<Line<'static>> {
        match self {
            Hangs::Returned => returns(cells, rows),
            // No glyph: the line above it is the person's own, and the
            // picture is part of what they said rather than an answer to it.
            Hangs::Said { indent } => {
                let lead = speaks_indent() + indent;
                under(Span::raw(" ".repeat(lead)), cells, lead, rows.measure())
            }
        }
    }

    /// What a terminal that cannot draw the picture draws instead. A person's
    /// own line already carries the word for what they handed over — the
    /// `[image 1]` token or the path they typed (M45) — so nothing is added
    /// under it; a tool's answer has no such word, and says what was there.
    fn chip(self, image: &Image, rows: &Rows<'_>) -> Vec<Line<'static>> {
        match self {
            Hangs::Said { .. } => Vec::new(),
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
    use crate::graphics::linked::Answer;
    use crate::graphics::{Decoded, Linked, kitty::PLACEHOLDER};
    use crate::test_support::{folded, item, scene, solo};
    use crate::tree::Agents;
    use bingo_pictures::testing::{png, unreadable};
    use bingo_sdk::{ContentPart, ItemStatus, Origin, ToolOutput};
    use ratatui::style::{Color, Style};
    use unicode_width::UnicodeWidthStr;

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
        block_with(item, terminal, fold, &Linked::default())
    }

    /// The same, with a memo of the pictures its words named already primed —
    /// which is the state the run leaves between two frames (M51).
    fn block_with(item: &Item, terminal: Graphics, fold: Fold, linked: &Linked) -> Block {
        let state = folded(Vec::new());
        let pictures = Decoded::default();
        let folds = match fold {
            Fold::Peek => crate::fold::Folds::new(),
            other => [(item.id.clone(), other)].into_iter().collect(),
        };
        let now = scene().1;
        let rows = Rows::of(&state, 60, &folds, &[], &pictures, linked, now);
        let cue = crate::transcript::Cue {
            since: now.instant,
            flip: false,
        };
        graphics::with(terminal, || {
            super::super::item_block(item, None, &Agents::new(), &rows, cue)
        })
    }

    /// An answer whose own words name a picture (M51).
    fn wrote(text: &str) -> Item {
        item(
            "itm_1",
            ItemStatus::Completed,
            ItemBody::Assistant { text: text.into() },
        )
    }

    /// The memo as the run leaves it once a destination has been read in.
    fn memo(dest: &str, result: Result<bingo_sdk::Image, String>) -> Linked {
        let mut linked = Linked::default();
        assert!(linked.take(dest));
        linked.answered(Answer {
            dest: dest.into(),
            result,
        });
        linked
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
        let linked = Linked::default();
        let rows = Rows::of(&state, 60, &folds, &[], &pictures, &linked, scene().1);
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
        let linked = Linked::default();
        let rows = Rows::of(&state, 60, &folds, &[], &pictures, &linked, scene().1);
        graphics::with(graphics::drawing(), || {
            let mut blocks = Blocks::default();
            blocks.sync(&state, &Agents::new(), &rows, Vec::new());
            let first = blocks.renders();
            blocks.sync(&state, &Agents::new(), &rows, Vec::new());
            assert_eq!(blocks.renders(), first, "nothing was drawn again");
            assert_eq!(blocks.pictures().len(), 1, "and the picture is still named");
        });
    }

    // ---- the picture in the words (M51) ---------------------------------

    /// `![shot](docs/x.png)` in an answer draws where the words put it: the
    /// chip stays as the line the picture hangs from, and the cells go under
    /// it — inside the answer, not after it.
    #[test]
    fn a_picture_an_answer_named_draws_under_its_own_chip() {
        let linked = memo("docs/x.png", Ok(png(100, 200)));
        let block = block_with(
            &wrote("look:\n\n![shot](docs/x.png)\n\nand that is it"),
            graphics::drawing(),
            Fold::Peek,
            &linked,
        );
        let drawn = text(&block);
        assert!(drawn[0].contains("look:"), "{drawn:?}");
        assert!(drawn[2].contains("[image: shot]"), "the chip: {drawn:?}");
        assert_eq!(
            drawn[3].matches(PLACEHOLDER).count(),
            10,
            "ten cells under it: {drawn:?}"
        );
        assert_eq!(
            cells_at(&drawn[3]),
            Some(speaks_indent()),
            "at the words' own column: {drawn:?}"
        );
        assert!(
            drawn[14].contains("and that is it"),
            "and the words after it come after the picture: {drawn:?}"
        );
        assert_eq!(
            block.pictures,
            vec![Picture {
                source: Source::Linked {
                    dest: "docs/x.png".into()
                },
                cols: 10,
                rows: 10,
            }]
        );
    }

    /// A picture in a plain paragraph stands at the words' own column, which is
    /// the `⏺` indent and nothing more (M56).
    #[test]
    fn a_picture_in_a_paragraph_stands_at_the_words_column() {
        let linked = memo("docs/x.png", Ok(png(100, 200)));
        let block = block_with(
            &wrote("![shot](docs/x.png)"),
            graphics::drawing(),
            Fold::Peek,
            &linked,
        );
        let drawn = text(&block);
        assert_eq!(cells_at(&drawn[1]), Some(speaks_indent()), "{drawn:?}");
        assert_eq!(block.pictures[0].cols, 10, "and it has the whole measure");
    }

    /// A picture in a bulleted answer stands at the bullet's *text* column, not
    /// at the block's own edge (M56): the marker's width past the `⏺` indent,
    /// which is where its chip stands too — and the room it is fitted to is
    /// what is left of the measure, so it is never pushed past the margin.
    #[test]
    fn a_picture_in_a_bullet_stands_at_the_bullets_text_column() {
        let linked = memo("docs/x.png", Ok(png(1000, 100)));
        let block = block_with(
            &wrote("- ![shot](docs/x.png)"),
            graphics::drawing(),
            Fold::Peek,
            &linked,
        );
        let drawn = text(&block);
        assert!(drawn[0].contains("[image: shot]"), "the chip: {drawn:?}");
        assert_eq!(
            cells_at(&drawn[1]),
            Some(speaks_indent() + 2),
            "past the bullet: {drawn:?}"
        );
        // 60 columns of measure, less the `⏺` indent and the marker: 56.
        assert_eq!(block.pictures[0].cols, 56, "fitted to the room it has");
        assert!(
            drawn.iter().all(|line| line.width() <= 60),
            "and nothing past the margin: {drawn:?}"
        );
    }

    /// One indent further in a nested item, and past the bar in a quote: the
    /// column is the markdown's, whatever the construct.
    #[test]
    fn a_nested_item_and_a_quote_stand_one_construct_further_in() {
        let linked = memo("docs/x.png", Ok(png(100, 200)));
        let drawn = |text: &str| {
            let block = block_with(&wrote(text), graphics::drawing(), Fold::Peek, &linked);
            self::text(&block)
        };
        let nested = drawn("- one\n  - ![shot](docs/x.png)");
        assert_eq!(
            cells_at(&nested[2]),
            Some(speaks_indent() + 4),
            "{nested:?}"
        );
        let quoted = drawn("> ![shot](docs/x.png)");
        assert_eq!(
            cells_at(&quoted[1]),
            Some(speaks_indent() + 2),
            "{quoted:?}"
        );
    }

    /// Which column a row of placeholder cells starts in, or `None` when the
    /// row carries none.
    fn cells_at(line: &str) -> Option<usize> {
        let cells = line.find(PLACEHOLDER)?;
        Some(line[..cells].width())
    }

    /// The degrade of §5: the chip is the whole of it, and no picture is sent.
    #[test]
    fn a_terminal_that_draws_no_pictures_draws_the_chip_the_words_carry() {
        let linked = memo("docs/x.png", Ok(png(100, 200)));
        let block = block_with(
            &wrote("![shot](docs/x.png)"),
            Graphics::Off,
            Fold::Peek,
            &linked,
        );
        assert_eq!(text(&block), vec!["⏺ [image: shot]"]);
        assert!(block.pictures.is_empty());
    }

    /// A destination this session has not read in yet is the chip alone —
    /// and the block says which destination, so the run can go and get it.
    #[test]
    fn a_destination_not_yet_read_in_is_the_chip_and_a_word_to_the_run() {
        let block = block_with(
            &wrote("![shot](docs/x.png)"),
            graphics::drawing(),
            Fold::Peek,
            &Linked::default(),
        );
        assert_eq!(text(&block), vec!["⏺ [image: shot]"]);
        assert!(block.pictures.is_empty(), "nothing to send yet");
        assert_eq!(block.wanted, vec!["docs/x.png".to_string()]);
    }

    /// A picture that is not there says so, once, in dim after its name.
    #[test]
    fn a_destination_that_failed_says_why_after_the_name() {
        let linked = memo("docs/x.png", Err("not found".into()));
        let block = block_with(
            &wrote("![shot](docs/x.png)"),
            graphics::drawing(),
            Fold::Peek,
            &linked,
        );
        assert_eq!(text(&block), vec!["⏺ [image: shot] (not found)"]);
        let spans = &block.lines[0].spans;
        assert_eq!(
            spans.last().map(|s| s.style),
            Some(theme::dim()),
            "the note is dim"
        );
    }

    /// Two answers that name the same picture are one picture: it is read in
    /// once and the terminal is asked to hold it once ([`Source::Linked`]).
    #[test]
    fn the_same_destination_named_twice_is_one_picture() {
        let linked = memo("docs/x.png", Ok(png(100, 200)));
        let one = block_with(
            &wrote("![a](docs/x.png)"),
            graphics::drawing(),
            Fold::Peek,
            &linked,
        );
        let two = block_with(
            &wrote("![b](docs/x.png)"),
            graphics::drawing(),
            Fold::Peek,
            &linked,
        );
        assert_eq!(one.pictures, two.pictures);
    }

    /// Two pictures in one answer keep their order, each under its own chip.
    #[test]
    fn two_pictures_in_one_answer_hang_from_their_own_chips() {
        let mut linked = memo("a.png", Ok(png(20, 20)));
        assert!(linked.take("b.png"));
        linked.answered(Answer {
            dest: "b.png".into(),
            result: Ok(png(40, 20)),
        });
        let block = block_with(
            &wrote("![one](a.png)\n\n![two](b.png)"),
            graphics::drawing(),
            Fold::Peek,
            &linked,
        );
        assert_eq!(
            block.pictures.iter().map(|p| p.cols).collect::<Vec<_>>(),
            vec![2, 4],
            "in the order the words wrote them"
        );
        assert_eq!(block.wanted, vec!["a.png".to_string(), "b.png".to_string()]);
        let drawn = text(&block);
        assert!(drawn[0].contains("[image: one]"), "{drawn:?}");
        assert!(drawn[3].contains("[image: two]"), "{drawn:?}");
    }

    /// `ctrl+o` opens a picture the words named to its whole height, the way
    /// it opens one a tool answered with: it is the same fold.
    #[test]
    fn the_fold_opens_a_picture_the_words_named() {
        let linked = memo("tall.png", Ok(png(100, 400)));
        let peeked = block_with(
            &wrote("![tall](tall.png)"),
            graphics::drawing(),
            Fold::Peek,
            &linked,
        );
        assert_eq!(
            peeked.pictures.first().map(|p| (p.cols, p.rows)),
            Some((6, IMAGE_ROWS))
        );
        let opened = block_with(
            &wrote("![tall](tall.png)"),
            graphics::drawing(),
            Fold::Open,
            &linked,
        );
        assert_eq!(
            opened.pictures.first().map(|p| (p.cols, p.rows)),
            Some((10, 20))
        );
    }

    /// The whole way through, on a real buffer: an answer that names a
    /// picture the session has read in draws its cells on the screen.
    #[test]
    fn the_frame_draws_the_cells_of_a_picture_the_words_named() {
        let state = folded(vec![said(wrote("![shot](docs/x.png)"))]);
        let (mut ui, now) = scene();
        ui.linked = memo("docs/x.png", Ok(png(100, 200)));
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
            screen.1,
            vec![Picture {
                source: Source::Linked {
                    dest: "docs/x.png".into()
                },
                cols: 10,
                rows: 10,
            }],
            "and the frame knows what to send for them"
        );
    }
}
