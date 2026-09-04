//! What a block does as it lands (design §6), on the same injected clock as
//! every other row: the light that crosses the name of a call that came back,
//! the cooling of one that came back wrong, and the room a new block takes —
//! which is none but its own.

use bingo_sdk::{Event, ItemStatus};
use ratatui::style::Style;

use super::{running_bash, screen, style_of};
use crate::clock::Now;
use crate::test_support::*;
use crate::theme;
use crate::tree::Tree;

/// A tool call that has just come back, with the cache warm on the frame
/// before — which is what makes it a completion a person watched, rather than
/// a row that was already there.
fn just_landed(ok: bool, ui: &crate::ui::Ui, now: Now) -> Tree {
    let mut state = folded(running_bash());
    crate::painted::painted(80, 24, &solo(&state), ui, now);
    let output = match ok {
        true => bingo_sdk::ToolOutput::text("ok"),
        false => bingo_sdk::ToolOutput {
            is_error: true,
            ..bingo_sdk::ToolOutput::text("exit 1")
        },
    };
    state.apply(&frame(
        3,
        Event::ItemCompleted {
            item: tool(
                "itm_1",
                "Bash",
                serde_json::json!({"command": "cargo test"}),
                Some(output),
                match ok {
                    true => ItemStatus::Completed,
                    false => ItemStatus::Failed,
                },
            ),
        },
    ));
    solo(&state)
}

/// The runs of the row a call landed on, so the light crossing its name can
/// be told from the one style it rests in.
fn landed_runs(tree: &Tree, ui: &crate::ui::Ui, now: Now) -> Vec<(String, Style)> {
    crate::painted::painted(80, 24, tree, ui, now).row("Bash")
}

/// The one bold frame a completion used to wear lasted 33 ms, which is below
/// the threshold at which a person reads it as *something happening*. One
/// light crosses the row's name instead, over six frames, and the row is at
/// rest on the seventh — where "at rest" is exactly the row it always was.
#[test]
fn a_completion_sweeps_its_name_for_six_frames_and_rests_on_the_seventh() {
    let (ui, now) = mid_turn();
    let done = just_landed(true, &ui, now);
    crate::theme::with(crate::painted::truecolor(), || {
        let sweeping = landed_runs(&done, &ui, now);
        assert!(
            sweeping.len() > 3,
            "the name is drawn cell by cell while the light is on it: {sweeping:#?}"
        );
        let bullet = sweeping.first().map(|(_, style)| *style);
        assert_eq!(
            bullet,
            Some(theme::as_drawn(theme::good())),
            "and the bullet only says it finished, with no frame of weight"
        );
        assert_ne!(
            landed_runs(&done, &ui, later(now, 99)),
            sweeping,
            "the light has moved along halfway through"
        );
        assert_eq!(
            landed_runs(&done, &ui, later(now, 198)),
            vec![
                ("⏺ ".to_string(), theme::as_drawn(theme::good())),
                ("Bash".to_string(), theme::as_drawn(theme::bold())),
                ("(cargo test)".to_string(), theme::as_drawn(theme::text())),
            ],
            "and on the seventh frame it is the row it always was"
        );
    });
    assert_eq!(
        landed_runs(&done, &ui, still(now)),
        landed_runs(&done, &ui, still(later(now, 198))),
        "a still surface draws the settled row from the first frame"
    );
}

/// The one moment §6 had no cue for at all. A failure flares and cools into
/// the words behind it over twelve frames — never a shake: §3's "nothing
/// jumps" outranks it, and the rise was withdrawn on 2026-09-02 for it.
#[test]
fn a_failure_flares_and_cools_into_the_words_behind_it() {
    let (ui, now) = mid_turn();
    let failed = just_landed(false, &ui, now);
    crate::theme::with(crate::painted::truecolor(), || {
        let name = |ms| {
            landed_runs(&failed, &ui, later(now, ms))
                .into_iter()
                .find(|(text, _)| text.contains("Bash"))
                .map(|(_, style)| style)
                .expect("the row's name")
        };
        assert_eq!(
            name(0),
            theme::as_drawn(theme::cooling(0.0).patch(theme::bold())),
            "it lands in `bad`"
        );
        assert_ne!(name(198), name(0), "and cools out of it");
        assert_eq!(
            name(396),
            theme::as_drawn(theme::bold()),
            "into the row's own words, twelve frames on"
        );
        assert_eq!(
            style_of(&failed, &ui, later(now, 396), "⏺"),
            theme::as_drawn(theme::bad()),
            "the bullet stays `bad`: what cooled is how fresh it is"
        );
    });
    assert_eq!(
        landed_runs(&failed, &ui, still(now)),
        landed_runs(&failed, &ui, still(later(now, 396))),
        "and a still surface draws the settled row from the first frame"
    );
}

// ---- a block arriving ---------------------------------------------------

/// Which row of a screen carries `needle`.
fn row_of(screen: &str, needle: &str) -> usize {
    screen
        .lines()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("no row carries {needle:?}"))
}

/// §3's "nothing jumps" outranks §6's cue: a block arriving takes exactly its
/// own room and the screen holds still from the frame it lands on. The rise
/// was withdrawn on 2026-09-02 (§10) — it put its two blank rows under the
/// newest block, which a bottom-anchored viewport turns into the whole
/// transcript jumping up two rows and walking back over three frames, once
/// per block.
#[test]
fn a_new_block_takes_its_own_room_and_walks_nowhere_after_it() {
    let (ui, now) = mid_turn();
    let mut state = folded(vec![frame(
        1,
        Event::ItemCompleted {
            item: user("itm_1", "first"),
        },
    )]);
    // The cache is warm after this draw, so the next block is one a person
    // watches arrive.
    let settled = screen(&solo(&state), &ui, now);
    let was = row_of(&settled, "first");

    state.apply(&frame(
        2,
        Event::ItemCompleted {
            item: assistant("itm_2", "second", ItemStatus::Completed),
        },
    ));
    let arriving = solo(&state);
    let frames: Vec<String> = [0i64, 33, 66, 99]
        .iter()
        .map(|ms| screen(&arriving, &ui, later(now, *ms)))
        .collect();
    assert_eq!(
        row_of(&frames[0], "first"),
        was - 2,
        "the row above moves up by the new block and its blank row, no further"
    );
    for (at, drawn) in frames.iter().enumerate().skip(1) {
        assert_eq!(
            drawn, &frames[0],
            "frame {at} of the arrival draws the same screen"
        );
    }
}
