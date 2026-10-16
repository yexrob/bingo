use super::*;
use bingo_services::schedule::Row;

fn press(
    cursor: &mut Cursor,
    rows: &[Row],
    shown: Option<&Row>,
    confirming: Option<&Confirmation>,
    key: &str,
) -> Value {
    let rows = rows
        .iter()
        .cloned()
        .map(crate::rows::Row::Schedule)
        .collect::<Vec<_>>();
    let shown = shown.cloned().map(crate::rows::Row::Schedule);
    super::press(cursor, &rows, shown.as_ref(), confirming, key)
}
fn row() -> Row {
    Row {
        id: "abcd".into(),
        spec: "every 2h".into(),
        next_fire: None,
        enabled: true,
        by: "you".into(),
        mode: None,
        text: "check".into(),
        session: None,
    }
}
#[test]
fn keys_map_the_selected_row() {
    let mut cursor = Cursor::default();
    let row = row();
    for (key, expected) in [
        ("space", json!({"run":"/schedule pause abcd"})),
        ("r", json!({"run":"/schedule run abcd"})),
        ("right", json!({"message":"has not fired yet"})),
    ] {
        assert_eq!(
            press(
                &mut cursor,
                std::slice::from_ref(&row),
                Some(&row),
                None,
                key
            ),
            expected
        );
    }
}
#[test]
fn forget_requires_the_current_confirmation() {
    let mut cursor = Cursor::default();
    let row = row();
    press(
        &mut cursor,
        std::slice::from_ref(&row),
        Some(&row),
        None,
        "d",
    );
    assert_eq!(
        press(
            &mut cursor,
            std::slice::from_ref(&row),
            Some(&row),
            None,
            "enter"
        ),
        json!({})
    );
    assert_eq!(
        press(
            &mut cursor,
            std::slice::from_ref(&row),
            Some(&row),
            Some(&Confirmation::Forget(row.clone())),
            "enter"
        ),
        json!({"run":"/schedule forget abcd"})
    );
    assert_eq!(
        press(
            &mut cursor,
            &[],
            Some(&row),
            Some(&Confirmation::Forget(row.clone())),
            "enter"
        ),
        json!({"message":"the list changed; look again"})
    );
}
#[test]
fn disabled_and_fired_rows_offer_their_actions() {
    let mut cursor = Cursor::default();
    let row = row();
    let disabled = Row {
        enabled: false,
        ..row.clone()
    };
    assert_eq!(
        press(
            &mut cursor,
            std::slice::from_ref(&disabled),
            Some(&disabled),
            None,
            "space"
        ),
        json!({"run":"/schedule resume abcd"})
    );
    let fired = Row {
        session: Some(SessionId::from_raw("ses_fired")),
        ..row
    };
    assert_eq!(
        press(
            &mut cursor,
            std::slice::from_ref(&fired),
            Some(&fired),
            None,
            "right"
        ),
        json!({"view":"ses_fired"})
    );
}

#[test]
fn ownership_confirmation_pins_the_mode_and_only_confirms_once() {
    for by in ["unconfirmed", "model"] {
        let row = Row {
            by: by.into(),
            mode: Some("acceptEdits".into()),
            ..row()
        };
        let mut cursor = Cursor::default();
        let rows = std::slice::from_ref(&row);
        assert_eq!(press(&mut cursor, rows, Some(&row), None, "c"), json!({}));
        let shown = cursor.confirming.clone().unwrap();
        assert_eq!(
            press(&mut cursor, rows, Some(&row), None, "enter"),
            json!({})
        );
        let changed = Row {
            mode: Some("bypassPermissions".into()),
            ..row.clone()
        };
        assert!(press(&mut cursor, &[changed], Some(&row), Some(&shown), "enter")["run"].is_null());
        press(&mut cursor, rows, Some(&row), None, "c");
        let shown = cursor.confirming.clone().unwrap();
        assert_eq!(
            press(&mut cursor, rows, Some(&row), Some(&shown), "enter"),
            json!({"run":format!("/schedule confirm abcd {}",
                bingo_services::schedule::confirmation_digest(&row.text, row.mode.as_deref()))})
        );
        assert!(press(&mut cursor, rows, Some(&row), Some(&shown), "enter")["run"].is_null());
    }
    let row = row();
    let mut cursor = Cursor::default();
    press(
        &mut cursor,
        std::slice::from_ref(&row),
        Some(&row),
        None,
        "c",
    );
    assert!(
        cursor.confirming.is_none(),
        "a person's entry needs no confirmation"
    );
}
#[test]
fn an_id_no_command_line_can_carry_runs_nothing() {
    for id in ["abcd 1234", "a\nb", "x;y", "", &"a".repeat(65)] {
        let row = Row {
            id: id.into(),
            ..row()
        };
        let mut cursor = Cursor::default();
        let pressed = press(
            &mut cursor,
            std::slice::from_ref(&row),
            Some(&row),
            None,
            "r",
        );
        assert!(pressed.get("run").is_none(), "{id:?}: {pressed}");
        assert!(
            pressed["message"]
                .as_str()
                .is_some_and(|m| m.contains("rename"))
        );
    }
    assert_eq!(
        run("run", "01hz-x_9"),
        json!({"run":"/schedule run 01hz-x_9"})
    );
}

fn wake() -> crate::rows::Row {
    crate::rows::Row::Wake(bingo_services::schedule::Pending {
        at: "2026-10-16T12:05:00Z".into(),
        note: "check the build".into(),
    })
}

#[test]
fn a_wake_only_offers_ending_the_currently_shown_wake() {
    let row = wake();
    let rows = std::slice::from_ref(&row);
    let mut cursor = Cursor {
        selected: Some(crate::rows::Selection::Wake),
        ..Default::default()
    };
    assert_eq!(
        super::press(&mut cursor, rows, Some(&row), None, "d"),
        json!({"run":"/wake off"})
    );
    for key in ["space", "r", "c", "right", "enter"] {
        assert_eq!(
            super::press(&mut cursor, rows, Some(&row), None, key),
            json!({})
        );
    }
    let changed = crate::rows::Row::Wake(bingo_services::schedule::Pending {
        at: "2026-10-16T12:10:00Z".into(),
        note: "another wake".into(),
    });
    assert_eq!(
        super::press(&mut cursor, &[changed], Some(&row), None, "d"),
        json!({"message":"the list changed; look again"})
    );
    assert_eq!(
        super::press(&mut cursor, &[], Some(&row), None, "d"),
        json!({"message":"the list changed; look again"})
    );
}

#[test]
fn navigation_distinguishes_a_schedule_named_wake_from_the_pending_wake() {
    let schedule = crate::rows::Row::Schedule(Row {
        id: "wake".into(),
        ..row()
    });
    let wake = wake();
    let rows = [schedule.clone(), wake.clone()];
    let mut cursor = Cursor::default();
    super::press(&mut cursor, &rows, Some(&schedule), None, "down");
    assert_eq!(cursor.selected, Some(crate::rows::Selection::Wake));
    assert_eq!(
        super::press(&mut cursor, &rows, Some(&wake), None, "d"),
        json!({"run":"/wake off"})
    );
    super::press(&mut cursor, &rows, Some(&wake), None, "up");
    assert_eq!(
        cursor.selected,
        Some(crate::rows::Selection::Schedule("wake".into()))
    );
    assert_eq!(
        super::press(&mut cursor, &rows, Some(&schedule), None, "r"),
        json!({"run":"/schedule run wake"})
    );
}
