//! The screens a shell line the person ran is read through (§5's shell line).
//!
//! Three things are being decided here and nowhere else: that the line sits
//! on the person's own bar under a `$` rather than a `>`, that what came back
//! hangs under a `⎿` and folds the way a result folds, and that a line which
//! failed says so in `bad` on a row the fold cannot take.

use super::*;

/// A line that worked, a long one folded back to its five rows, and one that
/// failed — read in the order a person would have run them.
pub(super) fn session() -> bingo_sdk::SessionState {
    folded(vec![
        item(1, user("itm_1", "which tests are failing?")),
        item(
            2,
            shell("itm_2", "git status --short", " M src/lib.rs\n", Some(0)),
        ),
        item(
            3,
            shell(
                "itm_3",
                "cargo test -p bingo-core 2>&1 | tail -20",
                &(1..=12)
                    .map(|n| format!("test session::case_{n} ... ok"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                Some(0),
            ),
        ),
        item(
            4,
            shell(
                "itm_4",
                "cargo fmt --check",
                "Diff in src/lib.rs at line 12:\n-fn a(){}\n+fn a() {}\n",
                Some(1),
            ),
        ),
        item(
            5,
            assistant(
                "itm_5",
                "The formatter wants one space.",
                ItemStatus::Completed,
            ),
        ),
    ])
}

#[test]
fn shell_lines() {
    let (ui, now) = scene();
    both("shell_lines", &solo(&session()), &ui, now);
}

/// The look a terminal that can draw no glyphs gets (§7): the prompt is a
/// `$` either way, and the connector falls back with everything else.
#[test]
fn shell_lines_in_ascii() {
    let (ui, now) = scene();
    without_glyphs("shell_lines_ascii", &solo(&session()), &ui, now);
}
