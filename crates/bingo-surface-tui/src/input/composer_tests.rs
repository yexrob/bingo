//! Composer gestures agree with the frame a terminal actually displays.

use super::*;
use crate::test_support::{render, scene, solo, state, still};
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Position;

fn composer_symbols(ui: &Ui, symbol: &str) -> Vec<Position> {
    let area = ui.shown.regions.composer;
    let mut found = Vec::new();
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if ui
                .shown
                .canvas
                .buffer
                .cell((x, y))
                .is_some_and(|cell| cell.symbol() == symbol)
            {
                found.push(Position::new(x, y));
            }
        }
    }
    found
}

#[test]
fn native_cursor_matches_visible_tail_caret() {
    let (mut ui, now) = scene();
    ui.composer.set(
        &(0..12)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    render(&state(), &mut ui, still(now));
    let carets = composer_symbols(&ui, crate::theme::glyphs::get("caret"));
    assert_eq!(
        carets.len(),
        1,
        "the drawn tail must contain exactly one caret"
    );
    assert_eq!(
        ui.shown.canvas.cursor,
        Some(carets[0]),
        "native IME anchor must match the visible caret"
    );
}

#[test]
fn hidden_caret_does_not_show_cursor_on_overflow_banner() {
    let (mut ui, now) = scene();
    ui.composer.set(
        &(0..12)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    ui.composer.home();
    for _ in 0..11 {
        ui.composer.up();
    }
    render(&state(), &mut ui, still(now));
    assert!(
        composer_symbols(&ui, crate::theme::glyphs::get("caret")).is_empty(),
        "the first line is scrolled out of view"
    );
    assert_eq!(
        ui.shown.canvas.cursor, None,
        "a hidden caret must not show a native cursor on another line"
    );
}

#[test]
fn click_visible_b_keeps_insertion_before_b() {
    let (mut ui, now) = scene();
    let state = state();
    ui.composer.set("abc");
    ui.composer.home();
    ui.composer.right();
    render(&state, &mut ui, still(now));
    let caret = composer_symbols(&ui, crate::theme::glyphs::get("caret"))[0];
    let b = composer_symbols(&ui, "b")
        .into_iter()
        .find(|p| p.y == caret.y)
        .expect("visible b on the input row");
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        crate::pointer::on_mouse(
            &mut ui,
            &solo(&state),
            MouseEvent {
                kind,
                column: b.x,
                row: b.y,
                modifiers: KeyModifiers::NONE,
            },
            now,
        );
    }
    on_paste(&mut ui, "X");
    assert_eq!(
        ui.composer.text(),
        "aXbc",
        "typing at the visible b must insert before b"
    );
}

#[test]
fn collapsed_selection_does_not_swallow_backspace() {
    let (mut ui, now) = scene();
    let tree = solo(&state());
    ui.composer.set("abc");
    for code in [KeyCode::Left, KeyCode::Right] {
        on_key(
            &mut ui,
            &tree,
            KeyEvent::new(code, KeyModifiers::SHIFT),
            now,
        );
    }
    on_key(
        &mut ui,
        &tree,
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        now,
    );
    assert_eq!(ui.composer.text(), "ab");
}

#[test]
fn shift_at_eof_does_not_swallow_word_delete() {
    let (mut ui, now) = scene();
    let tree = solo(&state());
    ui.composer.set("one two");
    on_key(
        &mut ui,
        &tree,
        KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT),
        now,
    );
    on_key(
        &mut ui,
        &tree,
        KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL),
        now,
    );
    assert_eq!(ui.composer.text(), "one ");
}

#[test]
fn native_cursor_tracks_visible_caret_in_compact_and_wrapped_drafts() {
    use crate::test_support::draw_sized;

    let draft = (0..12)
        .map(|n| format!("你好 e\u{301} line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    for width in [12, 80] {
        for height in [3, 5, 10, 24] {
            let (mut ui, now) = scene();
            ui.composer.set(&draft);
            for hidden in [false, true] {
                if hidden {
                    ui.composer.home();
                    for _ in 0..11 {
                        ui.composer.up();
                    }
                }
                draw_sized(width, height, &state(), &mut ui, still(now));
                let carets = composer_symbols(&ui, crate::theme::glyphs::get("caret"));
                assert!(carets.len() <= 1);
                assert_eq!(
                    ui.shown.canvas.cursor,
                    carets.first().copied(),
                    "{width}x{height}, hidden={hidden}"
                );
            }
        }
    }
}

#[test]
fn clicks_after_the_caret_preserve_unicode_grapheme_boundaries() {
    for (symbol, extra_column, expected) in [
        ("你", 0, "aX你e\u{301}好b"),
        ("你", 1, "a你Xe\u{301}好b"),
        ("e\u{301}", 0, "a你Xe\u{301}好b"),
        ("好", 0, "a你e\u{301}X好b"),
        ("b", 0, "a你e\u{301}好Xb"),
    ] {
        let (mut ui, now) = scene();
        ui.composer.set("a你e\u{301}好b");
        ui.composer.home();
        ui.composer.right();
        render(&state(), &mut ui, still(now));
        let at = composer_symbols(&ui, symbol)[0];
        mouse(
            &mut ui,
            MouseEventKind::Down(MouseButton::Left),
            Position::new(at.x + extra_column, at.y),
            now,
        );
        mouse(&mut ui, MouseEventKind::Up(MouseButton::Left), at, now);
        on_paste(&mut ui, "X");
        assert_eq!(ui.composer.text(), expected);
    }
}

#[test]
fn repeated_drag_events_use_the_last_drawn_caret() {
    let (mut ui, now) = scene();
    ui.composer.set("abcdef");
    render(&state(), &mut ui, still(now));
    let a = composer_symbols(&ui, "a")[0];
    let c = composer_symbols(&ui, "c")[0];
    let e = composer_symbols(&ui, "e")[0];
    mouse(&mut ui, MouseEventKind::Down(MouseButton::Left), a, now);
    mouse(&mut ui, MouseEventKind::Drag(MouseButton::Left), c, now);
    assert_eq!(ui.composer.selected_text(), Some("ab"));
    mouse(&mut ui, MouseEventKind::Drag(MouseButton::Left), e, now);
    assert_eq!(ui.composer.selected_text(), Some("abcd"));
    render(&state(), &mut ui, still(now));
    let f = composer_symbols(&ui, "f")[0];
    mouse(&mut ui, MouseEventKind::Drag(MouseButton::Left), f, now);
    mouse(&mut ui, MouseEventKind::Up(MouseButton::Left), f, now);
    assert_eq!(ui.composer.selected_text(), Some("abcde"));
}

fn mouse(ui: &mut Ui, kind: MouseEventKind, at: Position, now: Now) {
    crate::pointer::on_mouse(
        ui,
        &solo(&state()),
        MouseEvent {
            kind,
            column: at.x,
            row: at.y,
            modifiers: KeyModifiers::NONE,
        },
        now,
    );
}

#[test]
fn dragging_to_the_drawn_eof_row_survives_a_caret_move_before_redraw() {
    let (mut ui, now) = scene();
    let draft = "x".repeat(crate::composer::width(80));
    ui.composer.set(&draft);
    render(&state(), &mut ui, still(now));
    let start = composer_symbols(&ui, "x")[0];
    let end = composer_symbols(&ui, crate::theme::glyphs::get("caret"))[0];
    assert_eq!(end.y, start.y + 1);
    mouse(&mut ui, MouseEventKind::Down(MouseButton::Left), start, now);
    mouse(&mut ui, MouseEventKind::Drag(MouseButton::Left), end, now);
    mouse(&mut ui, MouseEventKind::Up(MouseButton::Left), end, now);
    assert_eq!(ui.composer.selected_text(), Some(draft.as_str()));
}

#[test]
fn a_reverse_drag_crosses_wide_text_using_the_drawn_caret() {
    let (mut ui, now) = scene();
    ui.composer.set("a你好bc");
    ui.composer.home();
    ui.composer.right();
    render(&state(), &mut ui, still(now));
    let b = composer_symbols(&ui, "b")[0];
    let hao = composer_symbols(&ui, "好")[0];
    mouse(&mut ui, MouseEventKind::Down(MouseButton::Left), b, now);
    mouse(&mut ui, MouseEventKind::Drag(MouseButton::Left), hao, now);
    mouse(&mut ui, MouseEventKind::Up(MouseButton::Left), hao, now);
    assert_eq!(ui.composer.selected_text(), Some("好"));
}
