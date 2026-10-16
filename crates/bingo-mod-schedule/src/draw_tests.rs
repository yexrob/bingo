use super::*;

#[test]
fn a_pending_wake_is_a_selectable_row_with_only_its_own_keys() {
    let listing = Listing::default();
    let wake = Pending {
        at: "2026-10-16T12:05:00Z".into(),
        note: "check the build\nthen report".into(),
    };
    let view = tree_with_wake(
        &listing,
        Some(&wake),
        0,
        None,
        80,
        18,
        "2026-10-16T12:00:00Z".parse().unwrap(),
    );
    testing::assert_separate_hints(&view);
    let shown = view.fold();
    assert!(
        shown.contains("❯ wake  in 5m  check the build  (+1 line)"),
        "{shown}"
    );
    assert!(!shown.contains("no schedules yet"));
    let View::Box { layout, .. } = view else {
        panic!("panel")
    };
    let hints = serde_json::to_value(layout.hints.unwrap()).unwrap();
    let hints = hints.to_string();
    assert!(hints.contains("end wake"));
    assert!(!hints.contains("pause/enable"));
    assert_eq!(
        layout.on_key.unwrap().args["row"],
        json!(rows::Row::Wake(wake))
    );
}

#[test]
fn a_wake_that_disappeared_has_no_drawn_row() {
    let view = tree(
        &Listing::default(),
        0,
        None,
        80,
        18,
        jiff::Timestamp::UNIX_EPOCH,
    );
    assert!(view.fold().contains("no schedules yet"));
    let View::Box { layout, .. } = view else {
        panic!("panel")
    };
    assert!(layout.on_key.unwrap().args["row"].is_null());
}
