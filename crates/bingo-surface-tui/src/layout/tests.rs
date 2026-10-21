use super::Layout;

fn layout(
    previous: Layout,
    tree: &crate::tree::Tree,
    ui: &crate::ui::Ui,
    size: Rect,
    now: crate::clock::Now,
) -> Layout {
    super::layout(
        previous,
        tree,
        ui,
        size,
        now,
        &crate::render::Render {
            motion: now.motion,
            ..crate::render::current()
        },
    )
}
use crate::test_support::*;
use ratatui::layout::Rect;

const SIZE: Rect = Rect::new(0, 0, 120, 40);

#[test]
fn layout_is_pure_and_idle_has_no_deadline() {
    let (ui, now) = scene();
    let now = still(now);
    let tree = solo(&state());
    let a = layout(Layout::default(), &tree, &ui, SIZE, now);
    let b = layout(Layout::default(), &tree, &ui, SIZE, now);
    assert_eq!(a.canvas, b.canvas);
    assert_eq!(a.regions, b.regions);
    assert_eq!(a.cells, b.cells);
    assert_eq!(a.deadline(), None);
    assert_eq!(b.deadline(), None);
}

#[test]
fn layout_renders_each_band_once_and_reuses_unchanged_blocks() {
    let (ui, now) = scene();
    let mut state = state();
    state.transcript = (0..200)
        .map(|i| {
            assistant(
                &format!("itm_{i}"),
                "A finished answer.",
                bingo_sdk::ItemStatus::Completed,
            )
        })
        .collect();
    let tree = solo(&state);
    let first = layout(Layout::default(), &tree, &ui, SIZE, now);
    assert_eq!(first.blocks.renders(), 200);
    assert_eq!(first.renders, [1, 0, 1, 1]);
    let next = layout(first, &tree, &ui, SIZE, now);
    assert_eq!(next.blocks.renders(), 200);
    assert_eq!(next.renders, [1, 0, 1, 1]);
    assert_eq!(next.canvas.buffer.content.len(), 120 * 40);
}

#[test]
fn layout_schedules_exit_at_its_deadline() {
    let (mut ui, now) = scene();
    let now = still(now);
    ui.armed = Some(now.instant);
    let frame = layout(Layout::default(), &solo(&state()), &ui, SIZE, now);
    assert_eq!(frame.deadline(), Some(now.instant + crate::ui::EXIT_WINDOW));
}

#[test]
fn layout_schedules_a_scroll_sample() {
    let (mut ui, now) = scene();
    ui.scroll = crate::scroll::Scroll::Held {
        from: 30,
        to: 0,
        since: now.instant,
    };
    let frame = layout(Layout::default(), &solo(&state()), &ui, SIZE, now);
    assert_eq!(frame.deadline(), Some(now.instant + crate::clock::FRAME));
}

#[test]
fn sent_images_are_chips_without_thumbnail_targets() {
    let (mut ui, now) = scene();
    let mut picture = user("itm_picture", "A picture.");
    if let bingo_sdk::ItemBody::User { parts, .. } = &mut picture.body {
        parts.push(bingo_sdk::ContentPart::Image(bingo_pictures::testing::png(
            40, 40,
        )));
    }
    let state = folded(vec![frame(
        1,
        bingo_sdk::Event::ItemCompleted { item: picture },
    )]);
    let screen = draw_sized(120, 40, &state, &mut ui, now);
    assert!(screen.contains("[image 1] A picture."));
    assert!(ui.shown.cells.is_empty());
}

#[test]
fn layout_finds_the_foot_in_the_current_transcript() {
    let (mut ui, now) = scene();
    let mut state = state();
    state.transcript = (0..100)
        .map(|i| user(&format!("itm_{i}"), "hello"))
        .collect();
    ui.scroll = crate::scroll::Scroll::Held {
        from: 0,
        to: 0,
        since: now.instant,
    };
    let frame = layout(Layout::default(), &solo(&state), &ui, SIZE, now);
    let foot = frame.foot.expect("held transcript");
    assert_eq!(foot.y, frame.regions.transcript.bottom() - 1);
    assert!(
        frame.canvas.buffer[(foot.x + 1, foot.y)]
            .symbol()
            .contains('↓')
    );
    assert_eq!(frame.line_at(0), Some(0));
}

#[test]
fn layout_countdown_changes_at_the_deadline_it_reports() {
    let (ui, now) = scene();
    let now = still(now);
    let until = now.wall + jiff::SignedDuration::from_millis(5250);
    let state = folded(vec![frame(
        1,
        signalled(
            "fixture",
            "timer",
            bingo_sdk::slot::with(
                serde_json::json!({}),
                vec![bingo_sdk::slot::Slot::Status {
                    text: "wake".into(),
                    tone: bingo_sdk::Tone::Neutral,
                    until: Some(until),
                }],
            ),
        ),
    )]);
    let tree = solo(&state);
    let first = layout(Layout::default(), &tree, &ui, SIZE, now);
    let due = first.deadline().expect("countdown");
    assert_eq!(
        due,
        now.instant + std::time::Duration::from_millis(250) + std::time::Duration::from_nanos(1)
    );
    let before = later(now, 250);
    let same = layout(Layout::default(), &tree, &ui, SIZE, before);
    assert_eq!(same.canvas, first.canvas);
    let changed = crate::clock::Now {
        instant: due,
        wall: before.wall + jiff::SignedDuration::from_nanos(1),
        ..now
    };
    let next = layout(Layout::default(), &tree, &ui, SIZE, changed);
    assert_ne!(next.canvas, first.canvas);
}

#[test]
fn layout_notice_sleeps_through_its_hold_and_expires() {
    let (mut ui, now) = scene();
    let now = still(now);
    ui.notify(bingo_sdk::Level::Info, "hello", now.instant);
    let tree = solo(&state());
    let first = layout(Layout::default(), &tree, &ui, SIZE, now);
    assert_eq!(first.deadline(), Some(now.instant + crate::ui::NOTICE));
    let held = later(now, 100);
    let frame = layout(first, &tree, &ui, SIZE, held);
    assert_eq!(frame.deadline(), Some(now.instant + crate::ui::NOTICE));
    ui.expire(later(now, 5000));
    assert_eq!(
        layout(frame, &tree, &ui, SIZE, later(now, 5000)).deadline(),
        None
    );
}

#[test]
fn layout_running_turn_schedules_each_motion_sample() {
    let (ui, now) = mid_turn();
    let tree = solo(&folded(vec![frame(1, started("trn_1"))]));
    let first = layout(Layout::default(), &tree, &ui, SIZE, now);
    assert_eq!(first.deadline(), Some(now.instant + crate::clock::FRAME));
    let later_sample = later(now, 140);
    let next = layout(first, &tree, &ui, SIZE, later_sample);
    assert_eq!(
        next.deadline(),
        Some(crate::spinner::next(later_sample).min(later_sample.instant + crate::clock::FRAME))
    );
}

#[test]
fn layout_layer_and_drag_report_their_own_deadlines() {
    let (mut ui, now) = scene();
    let tree = solo(&state());
    ui.focus
        .show(crate::ui::Open::Rewind(Default::default()), now.instant);
    let first = layout(Layout::default(), &tree, &ui, SIZE, now);
    assert_eq!(first.deadline(), Some(now.instant + crate::clock::FRAME));
    let arrived = later(now, 200);
    let first = layout(first, &tree, &ui, SIZE, arrived);
    assert_eq!(first.deadline(), None);
    ui.start_selection(crate::select::Cell::default());
    ui.select.dragging = Some(crate::select::Dragging {
        edge: crate::select::Edge::Above(0),
        stepped: arrived.instant,
    });
    let first = layout(first, &tree, &ui, SIZE, arrived);
    assert_eq!(
        first.deadline(),
        Some(arrived.instant + crate::select::EDGE_PACE)
    );
    ui.focus = crate::focus::Focus::Search(crate::search::Search::open());
    assert!(
        layout(first, &tree, &ui, SIZE, arrived)
            .deadline()
            .is_some()
    );
    ui.drag_step(arrived);
    assert!(ui.select.dragging.is_none());
}

#[test]
fn idle_nonterminal_item_schedules_nothing() {
    let (ui, now) = scene();
    let mut unfinished = user("unfinished", "a resumed item");
    unfinished.status = bingo_sdk::ItemStatus::Running;
    let state = folded(vec![frame(
        1,
        bingo_sdk::Event::ItemStarted { item: unfinished },
    )]);
    assert!(!state.busy());
    for motion in [true, false] {
        let drawn = layout(
            Layout::default(),
            &solo(&state),
            &ui,
            SIZE,
            crate::clock::Now { motion, ..now },
        );
        assert!(drawn.blocks.height() > 0);
        assert_eq!(
            drawn.deadline(),
            (motion && !cfg!(windows)).then_some(now.instant + crate::clock::FRAME),
            "only an animated visible caret schedules, motion={motion}"
        );
    }
}
