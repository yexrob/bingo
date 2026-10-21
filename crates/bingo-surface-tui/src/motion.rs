//! Every row of `docs/design/tui.md` §6, frame by frame on an injected clock.
//!
//! A cue is a pure function of [`crate::clock::Now`] and state, so each row
//! below is a handful of samples at named instants rather than something to
//! watch for. What the samples assert is the *cue* — a glyph, a style, a row
//! that is there or is not — because that is what §6 promises; where the cue
//! is drawn is `screens.rs`'s business.
//!
//! The rule the whole file is here to hold: every motion reports a state
//! change, stillness is the default, and `BINGO_MOTION=off` stills all of it.

use std::time::Duration;

use bingo_sdk::{Event, ItemStatus, TurnStatus};
use ratatui::style::Style;

use crate::clock::{FRAME, Now};
use crate::test_support::*;
use crate::theme;
use crate::tree::Tree;

/// The style of the run of cells carrying `needle` on a drawn frame.
fn style_of(tree: &Tree, ui: &mut crate::ui::Ui, now: Now, needle: &str) -> Style {
    let painted = crate::painted::painted(80, 24, tree, ui, now);
    painted
        .row(needle)
        .into_iter()
        .find(|(text, _)| text.contains(needle))
        .map(|(_, style)| style)
        .unwrap_or_else(|| panic!("no run carries {needle:?}"))
}

fn composer_style(tree: &Tree, ui: &mut crate::ui::Ui, now: Now) -> Style {
    let backend = drawn(80, 24, tree, ui, now);
    backend.buffer()[(3, ui.shown.regions.composer.y + 1)].style()
}

/// A screen at one instant, as text.
fn screen(tree: &Tree, ui: &mut crate::ui::Ui, now: Now) -> String {
    draw_tree(80, 24, tree, ui, now)
}

/// The style of the first run of the row carrying `needle` — the sparkle on
/// the activity row, the border on the input box's own row.
fn leading_style(tree: &Tree, ui: &mut crate::ui::Ui, now: Now, needle: &str) -> Style {
    let painted = crate::painted::painted(80, 24, tree, ui, now);
    let runs = painted.row(needle);
    runs.iter()
        .find(|(text, _)| !text.trim().is_empty())
        .map(|(_, style)| *style)
        .unwrap_or_else(|| panic!("no row carries {needle:?}"))
}

/// One row of it, without the quotes a test backend prints around each.
fn row(tree: &Tree, ui: &mut crate::ui::Ui, now: Now, needle: &str) -> String {
    screen(tree, ui, now)
        .lines()
        .find(|line| line.contains(needle))
        .map(|line| line.trim_matches('"').trim_end().to_string())
        .unwrap_or_else(|| panic!("no row carries {needle:?}"))
}

/// What a block does as it lands: it takes its own room and nothing more, one
/// light crosses the name of a call that came back, and one that came back
/// wrong cools out of `bad`.
mod landing;

// ---- presence: the sparkle and the breath -------------------------------

/// A turn that has been running long enough to have a row of its own.
fn turning() -> Tree {
    solo(&folded(vec![frame(1, started("trn_1"))]))
}

/// A state whose one item is still arriving, and one whose one item is a
/// call the turn is waiting on.
fn answering() -> bingo_sdk::SessionState {
    let mut state = folded(vec![frame(1, started("trn_1"))]);
    state.apply(&frame(
        2,
        Event::ItemStarted {
            item: assistant("itm_1", "half a sen", ItemStatus::Running),
        },
    ));
    state
}

fn calling() -> bingo_sdk::SessionState {
    folded(running_bash())
}

/// The row is drawn on that period and not on a constant, which is the half
/// of the cue a unit test cannot see.
#[test]
fn arriving_words_hold_the_caret_steady_while_tools_breathe() {
    let (mut ui, now) = mid_turn();
    let at = later(now, 250);
    theme::with(crate::painted::truecolor(), || {
        let answer = screen(&solo(&answering()), &mut ui, at);
        assert!(answer.contains("half a sen▍"), "{answer}");
        assert!(!answer.contains("working"));
        let tool = screen(&solo(&calling()), &mut ui, at);
        assert!(!tool.contains("working"), "{tool}");
        let working = screen(&turning(), &mut ui, at);
        assert!(working.contains("working"), "{working}");
        assert_eq!(
            leading_style(&turning(), &mut ui, at, "working"),
            theme::as_drawn(theme::glow())
        );
    });
}

#[test]
fn the_composer_caret_respects_the_platform_motion_policy() {
    let (mut ui, now) = scene();
    theme::with(crate::painted::truecolor(), || {
        let idle = solo(&state());
        assert_eq!(
            composer_style(&idle, &mut ui, now),
            composer_style(&turning(), &mut ui, now)
        );
        let first = composer_style(&idle, &mut ui, now);
        let next = composer_style(&idle, &mut ui, later(now, 300));
        if cfg!(windows) {
            assert_eq!(
                first, next,
                "the Windows caret stays steady to avoid flashing"
            );
        } else {
            assert_ne!(first, next, "the caret animates while motion is enabled");
        }
        assert_eq!(
            composer_style(&idle, &mut ui, still(now)),
            composer_style(&idle, &mut ui, still(later(now, 300)))
        );
    });
}

#[test]
fn nothing_of_the_presence_is_on_screen_while_no_turn_runs() {
    let (mut ui, now) = mid_turn();
    let screen = screen(&solo(&state()), &mut ui, now);
    assert!(!screen.contains("working"), "{screen}");
    assert!(
        !screen.contains("bingo▍") && screen.contains("   ▍Ask anything"),
        "{screen}"
    );
}

// ---- streaming: the comet tail ------------------------------------------

/// An answer that is still arriving, with a tail long enough to ramp.
fn streaming() -> Tree {
    solo(&folded(vec![
        frame(1, started("trn_1")),
        frame(
            2,
            Event::ItemStarted {
                item: assistant("itm_1", "", ItemStatus::Running),
            },
        ),
        frame(
            3,
            Event::ItemDelta {
                item: bingo_sdk::ItemId::from_raw("itm_1"),
                n: 0,
                kind: bingo_sdk::DeltaKind::Text,
                data: "the last cells are still warm".into(),
            },
        ),
    ]))
}

/// The styles of the last eight cells of the answer, oldest first.
fn tail(ui: &mut crate::ui::Ui, now: Now, at: Now) -> Vec<Style> {
    let tree = streaming();
    // The block is rendered once at `now` so the cache dates its arrival
    // there, and again at `at` so the tail has aged.
    crate::painted::painted(80, 24, &tree, ui, now);
    let painted = crate::painted::painted(80, 24, &tree, ui, at);
    let mut runs: Vec<Style> = painted
        .row("still warm")
        .into_iter()
        .rev()
        .filter(|(text, _)| !text.trim().is_empty())
        .take(8)
        .map(|(_, style)| style)
        .collect();
    runs.reverse();
    runs
}

/// Arrival, not time, advances the tail; the former 180 ms settling check does not apply.
#[test]
fn an_answer_tail_blends_without_adding_its_own_pace() {
    let (mut ui, now) = mid_turn();
    crate::theme::with(crate::painted::truecolor(), || {
        let fresh = tail(&mut ui, now, now);
        assert_eq!(
            fresh.last().copied(),
            Some(theme::as_drawn(theme::comet(0.0))),
            "the cell that just landed wears the glow"
        );
        assert_ne!(fresh.first(), fresh.last(), "and the ramp is a ramp");

        let warm = tail(&mut ui, now, later(now, 75));
        assert_eq!(
            warm, fresh,
            "arrival, not a second clock, advances the tail"
        );
        let (mut reduced_ui, _) = scene();
        let reduced = crate::painted::painted(80, 24, &streaming(), &mut reduced_ui, still(now));
        let cells: Vec<_> = reduced
            .row("still warm")
            .into_iter()
            .flat_map(|(text, ink)| text.chars().map(move |c| (c, ink)).collect::<Vec<_>>())
            .filter(|(c, _)| *c != '▍')
            .collect();
        for (_, ink) in cells.iter().rev().take(10) {
            assert_eq!(*ink, theme::as_drawn(theme::text()));
        }
        let mut state = answering();
        state.apply(&frame(
            3,
            Event::ItemCompleted {
                item: assistant(
                    "itm_1",
                    "the last cells are still warm",
                    ItemStatus::Completed,
                ),
            },
        ));
        state.apply(&frame(4, completed("trn_1", TurnStatus::Completed)));
        let tree = solo(&state);
        let moving = crate::painted::painted(80, 24, &tree, &mut ui, now).row("still warm");
        let resting = crate::painted::painted(80, 24, &tree, &mut ui, still(now)).row("still warm");
        assert_eq!(moving, resting);
    });
}

#[test]
fn the_tail_is_style_and_never_text() {
    let (mut ui, now) = mid_turn();
    let tree = streaming();
    assert_eq!(
        screen(&tree, &mut ui, now),
        screen(&tree, &mut ui, later(now, 90)),
        "what a `--print` would see does not move"
    );
}

// ---- a tool running, and finishing --------------------------------------

fn running_bash() -> Vec<bingo_sdk::Frame> {
    vec![
        frame(1, started("trn_1")),
        started_tool(2, running_tool("itm_1", "Bash", "compiling…")),
    ]
}

/// The glyph a running step's row starts with, on the frame at `now`.
fn spinner_at(tree: &Tree, ui: &mut crate::ui::Ui, now: Now) -> String {
    let drawn = row(tree, ui, now, "bash    cargo test");
    drawn.trim_start().chars().take(1).collect()
}

/// A running step turns in presence, one frame a beat of the wall clock, so
/// two rows running side by side turn together.
#[test]
fn a_running_step_spins_in_presence_on_the_wall_clock() {
    let tree = solo(&folded(running_bash()));
    let (mut ui, now) = mid_turn();
    let beat = i64::try_from(crate::spinner::beat().as_millis()).unwrap_or(80);
    let frames = theme::glyphs().chrome.spinner.chars().count() as i64;
    let start = spinner_at(&tree, &mut ui, now);
    assert_ne!(
        start,
        spinner_at(&tree, &mut ui, later(now, beat)),
        "the next beat turns it"
    );
    assert_eq!(
        start,
        spinner_at(&tree, &mut ui, later(now, beat * frames)),
        "and a whole turn brings it back"
    );
    crate::theme::with(crate::painted::truecolor(), || {
        assert_eq!(
            style_of(&tree, &mut ui, now, &start),
            theme::as_drawn(theme::glow()),
            "in glow"
        );
    });
}

// ---- the activity row ---------------------------------------------------

#[test]
fn a_turn_that_answers_at_once_never_flashes_a_row() {
    let tree = turning();
    let (mut ui, now) = scene();
    for ms in [0i64, 100, 200, 499] {
        let screen = screen(&tree, &mut ui, later(now, ms));
        assert!(!screen.contains("working"), "at {ms} ms: {screen}");
    }
    assert!(
        screen(&tree, &mut ui, later(now, 500)).contains("working"),
        "and it appears at 500 ms"
    );
}

/// The flag is a fact about one keypress and one turn. The turn after it is
/// another turn, and reads as one.
#[test]
fn the_next_turn_is_not_the_one_that_was_stopped() {
    let state = folded(vec![frame(1, started("trn_2"))]);
    let (mut ui, now) = scene();
    ui.stop_asked = Some(bingo_sdk::TurnId::from_raw("trn_1"));
    let drawn = screen(&solo(&state), &mut ui, later(now, 4_000));
    assert!(drawn.contains("working"), "{drawn}");
    assert!(!drawn.contains("stopping"), "{drawn}");
}

// ---- a sheet ------------------------------------------------------------

#[test]
fn a_sheet_slides_up() {
    let (mut sheet, now) = scene();
    sheet
        .focus
        .show(crate::ui::Open::Rewind(Default::default()), now.instant);
    let plain = solo(&state());
    let frames: Vec<String> = (0..5)
        .map(|f| screen(&plain, &mut sheet, later(now, f * 33)))
        .collect();
    insta::assert_snapshot!("sheet_slide", frames.join("\n"));
}

#[test]
fn esc_runs_a_sheet_back_down_the_way_it_came() {
    let (mut ui, now) = scene();
    shown(&mut ui, crate::ui::Open::Rewind(Default::default()), now);
    ui.focus.close(now.instant);
    let tree = solo(&state());
    let closing: Vec<String> = (0..5)
        .map(|f| screen(&tree, &mut ui, later(now, f * 33)))
        .collect();
    assert_eq!(
        closing[4],
        screen(&tree, &mut crate::ui::Ui::new(Vec::new(), now.instant), now),
        "by the last frame it has gone"
    );
    insta::assert_snapshot!("sheet_close", closing.join("\n"));
}

// ---- notices ------------------------------------------------------------

#[test]
fn a_toast_keeps_its_colour_until_expiry() {
    let tree = solo(&state());
    let (mut ui, now) = scene();
    ui.notify(bingo_sdk::Level::Error, "unknown command: /x", now.instant);
    theme::with(crate::painted::truecolor(), || {
        let at =
            |ui: &mut crate::ui::Ui, ms| style_of(&tree, ui, later(now, ms), "unknown command");
        assert_ne!(at(&mut ui, 0), at(&mut ui, 160));
        assert_eq!(at(&mut ui, 160), at(&mut ui, 2399));
        assert_ne!(at(&mut ui, 2400), at(&mut ui, 2550));
    });
    assert!(!screen(&tree, &mut ui, later(now, 2700)).contains("unknown command"));
}

#[test]
fn a_refused_line_is_named_after_the_reason_that_refused_it() {
    let tree = solo(&state());
    let (mut ui, now) = scene();
    ui.notify_about(
        bingo_sdk::Level::Error,
        "unknown command: /x".into(),
        "/x the whole line".into(),
        now.instant,
    );
    let screen = screen(&tree, &mut ui, later(now, 66));
    assert!(
        screen.contains("unknown command: /x · /x the whole line"),
        "{screen}"
    );
    assert_eq!(
        style_of(&tree, &mut ui, later(now, 66), "/x the whole"),
        theme::as_drawn(theme::dim()),
        "what it was about is said quietly"
    );
}

// ---- what wants a person ------------------------------------------------

#[test]
fn the_footer_holds_attention_words_still() {
    let tree = folded_tree(vec![
        child_frame(1, announced("reviewer")),
        child_frame(2, opened(child_permission())),
    ]);
    let (ui, now) = scene();
    let first = crate::footer::draw(&tree, &ui, &mut Default::default(), 80, now)
        .lines
        .remove(0);
    assert_eq!(
        first,
        crate::footer::draw(&tree, &ui, &mut Default::default(), 80, later(now, 1000)).lines[0]
    );
    assert!(
        first.to_string().contains("◆ reviewer needs you"),
        "the needs-you lead names the waiting child"
    );
}

// ---- the context notice -------------------------------------------------

/// A session that has used `share` per cent of its compaction trigger.
fn context(share: u64) -> Tree {
    solo(&folded(vec![frame(
        1,
        Event::TurnUsage {
            turn: bingo_sdk::TurnId::from_raw("trn_1"),
            usage: Default::default(),
            context: bingo_sdk::ContextUsage {
                parts: None,
                used: share * 1_000,
                window: 200_000,
                trigger: 100_000,
            },
        },
    )]))
}

/// The notice is a notice until the way out joins it, and then it is bad — the
/// same in every look since M73, because what it warms *from* is the
/// terminal's own dim and no colour of ours can be mixed towards that. The
/// turn lands where the words do, at 90 % (§4).
#[test]
fn the_context_count_is_always_there_and_warms_across_the_last_fifth() {
    let (mut ui, now) = scene();
    assert!(!screen(&context(139), &mut ui, now).contains("context"));
    for look in [crate::painted::truecolor(), crate::painted::ascii()] {
        theme::with(look, || {
            assert_eq!(
                style_of(&context(140), &mut ui, now, "context 70%"),
                theme::as_drawn(theme::foreground("muted").unwrap())
            );
            assert_eq!(
                style_of(&context(160), &mut ui, now, "context 80%"),
                theme::as_drawn(theme::foreground("amber").unwrap())
            );
        });
    }
}

// ---- stepping into another session --------------------------------------

#[test]
fn the_transcript_crossfades_through_dim_into_the_session_stepped_into() {
    let mut tree = spawned_tree(busy_child("reviewer"));
    tree.show(&child_id());
    let (mut ui, now) = scene();
    let row = |ui: &mut crate::ui::Ui, at| {
        crate::painted::painted(80, 24, &tree, ui, at).row("read    src/lib.rs")
    };
    let plain = row(&mut ui, now);

    ui.switched = Some(now.instant);
    assert_ne!(
        row(&mut ui, now),
        plain,
        "everything recedes for the two frames of the crossfade"
    );
    assert_eq!(
        row(&mut ui, later(now, 66)),
        plain,
        "and comes back up as it was"
    );
}

// ---- reduced motion -----------------------------------------------------

#[test]
fn motion_off_holds_every_cue_at_its_resting_frame() {
    let tree = turning();
    let (mut ui, now) = mid_turn();
    let still = still(later(now, 600));
    assert_eq!(
        leading_style(&tree, &mut ui, still, "working"),
        theme::as_drawn(theme::glow()),
        "the still mark keeps its glow"
    );
    let drawn = row(&tree, &mut ui, still, "working");
    assert!(
        drawn.starts_with("   ◌"),
        "and holds its first glyph: {drawn}"
    );

    // A spinner stands as the waiting glyph, and a tail is absent rather
    // than frozen.
    let bash = solo(&folded(running_bash()));
    assert_eq!(
        spinner_at(&bash, &mut ui, still),
        theme::glyphs().chrome.steps[2],
        "a running step does not spin"
    );
    let tail = crate::painted::painted(80, 24, &streaming(), &mut ui, still);
    assert!(
        tail.coloured("still warm")
            .iter()
            .all(|text| text.trim() == "▍"),
        "and streaming text carries no tail"
    );
}

#[test]
fn motion_off_puts_a_layer_up_whole_and_takes_it_away_at_once() {
    let (mut ui, now) = scene();
    ui.focus
        .show(crate::ui::Open::Rewind(Default::default()), now.instant);
    let still = still(now);
    let tree = solo(&state());
    assert_eq!(
        screen(&tree, &mut ui, still),
        screen(&tree, &mut ui, later(now, 200)),
        "a sheet is whole on its first frame"
    );
    ui.focus.close(now.instant);
    assert!(
        ui.focus.drawn(still).is_none(),
        "and gone on the frame it closes"
    );
}

#[test]
fn a_still_notice_is_said_at_once_and_still_leaves() {
    let tree = solo(&state());
    let (mut ui, now) = scene();
    ui.notify(bingo_sdk::Level::Error, "unknown command: /x", now.instant);
    assert_eq!(
        style_of(&tree, &mut ui, still(now), "unknown command"),
        theme::as_drawn(theme::foreground("muted").unwrap()),
        "no fade, but the same muted words"
    );
    assert_eq!(
        style_of(&tree, &mut ui, still(now), theme::glyphs::get("times")),
        theme::as_drawn(theme::bad()),
        "only the mark carries the bad ink"
    );
    ui.expire(still(later(now, 4_132)));
    assert!(
        !screen(&tree, &mut ui, still(later(now, 4_132))).contains("unknown command"),
        "and it still goes when its time is up"
    );
}

// ---- what a completed turn leaves behind --------------------------------

#[test]
fn a_finished_turn_takes_the_whole_of_the_presence_with_it() {
    let state = folded(vec![
        frame(1, started("trn_1")),
        frame(2, completed("trn_1", TurnStatus::Completed)),
    ]);
    let (mut ui, now) = mid_turn();
    let screen = screen(&solo(&state), &mut ui, now);
    assert!(!screen.contains("working"), "{screen}");
    assert_eq!(
        composer_style(&solo(&state), &mut ui, still(now)),
        theme::as_drawn(theme::glow().patch(crate::dock::ground())),
        "the idle prompt keeps its glow"
    );
}

/// §6's budget: nothing moves when nothing is happening. The loop's own
/// counter proves it (`run::tests`); this proves the frame is identical, so
/// even a redraw would cost nothing.
#[test]
fn an_idle_frame_is_the_same_frame_a_second_later() {
    let tree = solo(&folded(vec![frame(
        1,
        Event::ItemCompleted {
            item: assistant("itm_1", "All green.", ItemStatus::Completed),
        },
    )]));
    let (mut ui, now) = scene();
    let first = screen(&tree, &mut ui, now);
    for ms in [33i64, 150, 1_000, 4_000] {
        assert_eq!(first, screen(&tree, &mut ui, later(now, ms)), "at {ms} ms");
    }
}

/// A duration named in §6 is named once, here, so a change of rhythm is a
/// change of one line rather than a hunt.
#[test]
fn the_rhythms_are_the_ones_the_design_names() {
    assert_eq!(FRAME, Duration::from_millis(33));
    assert_eq!(theme::PULSE, Duration::from_secs(1));
    assert_eq!(crate::ui::NOTICE, Duration::from_millis(2700));
    assert_eq!(crate::ui::SWITCH, FRAME * 2);
}
