//! The screens a line somebody sent with pictures is read through (§5's image
//! row, M62): the band of thumbnails above their own words, one, three and six
//! of them — and the same line on a terminal that draws no picture at all,
//! where the tokens in the line are the whole of it.

use bingo_sdk::{Origin, SessionState};

use super::*;

/// The shapes the fixtures cycle through, in pixels: a wide screenshot, a tall
/// phone shot, a square — so the band reads as pictures rather than as four
/// copies of one.
const SIZES: [(u32, u32); 3] = [(1200, 800), (600, 900), (800, 800)];

/// A person's line with `count` pictures behind its tokens (M45), and the
/// answer to it.
fn sent(count: u32) -> SessionState {
    folded(vec![
        item(1, handed_over(count)),
        item(
            2,
            assistant(
                "itm_2",
                "The second one: 24px, where the rest are 16px.",
                ItemStatus::Completed,
            ),
        ),
    ])
}

/// The line itself: the words, one `[image N]` for each picture, and the
/// pictures behind them.
fn handed_over(count: u32) -> bingo_sdk::Item {
    let tokens: Vec<String> = (1..=count).map(crate::pictures::placeholder).collect();
    let mut parts = vec![ContentPart::text(format!(
        "which of these has the right margin? {}",
        tokens.join(" ")
    ))];
    parts.extend((0..count as usize).map(|n| {
        let (width, height) = SIZES[n % SIZES.len()];
        ContentPart::Image(bingo_pictures::testing::png(width, height))
    }));
    crate::test_support::item(
        "itm_1",
        ItemStatus::Completed,
        ItemBody::User {
            parts,
            origin: Origin::surface("tui"),
        },
    )
}

/// One picture: three rows of band above the line, in the line's own column.
#[test]
fn one_picture_sent() {
    let (ui, now) = scene();
    crate::graphics::with(crate::graphics::drawing(), || {
        both("sent_one_picture", &solo(&sent(1)), &ui, now);
    });
}

/// Three of them, side by side with a column between — a row, not a wall.
#[test]
fn three_pictures_sent() {
    let (ui, now) = scene();
    crate::graphics::with(crate::graphics::drawing(), || {
        both("sent_three_pictures", &solo(&sent(3)), &ui, now);
    });
}

/// Six: four are shown and the rest are counted, as the composer counted them.
#[test]
fn six_pictures_sent() {
    let (ui, now) = scene();
    crate::graphics::with(crate::graphics::drawing(), || {
        both("sent_six_pictures", &solo(&sent(6)), &ui, now);
    });
}

/// The same line where nothing can be drawn: the block is exactly the row it
/// has always been, because the tokens in it already say what is attached
/// (M45). This is the `--print` shape and a chat channel's.
#[test]
fn pictures_on_a_terminal_that_draws_none() {
    let (ui, now) = scene();
    both("sent_pictures_undrawn", &solo(&sent(3)), &ui, now);
}
