use super::*;

#[tokio::test]
async fn preview_is_once_per_open_and_card_confirmation_is_consumed() {
    let fixture = Fixture::new();
    let (state, mut ui, now) = setup(&fixture);
    open(
        &mut ui,
        &solo(&state),
        Some(TurnId::from_raw("t2")),
        true,
        now,
    );
    let screen = fixture.paint(&mut ui, &state, now, 80, 24).await;
    assert!(screen.contains("undoes turn 2 ·"), "{screen}");
    assert!(!screen.contains("turns 2–2"));
    assert!(!screen.contains("\"\" 0/0"));
    assert!(!super::super::opened(&ui).unwrap().confirm);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    fixture.paint(&mut ui, &state, now, 80, 24).await;
    fixture
        .press(&mut ui, &state, key(KeyCode::Right), now)
        .await;
    let screen = fixture.paint(&mut ui, &state, now, 80, 24).await;
    assert!(screen.contains("files are kept"));
    assert!(!screen.contains("restored crates"));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    fixture.press(&mut ui, &state, key(KeyCode::Esc), now).await;
    let screen = fixture.paint(&mut ui, &state, now, 80, 24).await;
    assert!(!screen.contains("rewind to turn"));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    fixture
        .press(&mut ui, &state, key(KeyCode::Char('r')), now)
        .await;
    fixture.paint(&mut ui, &state, now, 80, 24).await;
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn pager_returns_to_query_and_position_and_tool_copy_is_readable() {
    let fixture = Fixture::new();
    let (state, mut ui, now) = setup(&fixture);
    open(&mut ui, &solo(&state), None, false, now);
    fixture.paint(&mut ui, &state, now, 80, 24).await;
    for c in "/hidden".chars() {
        fixture
            .press(&mut ui, &state, key(KeyCode::Char(c)), now)
            .await;
        fixture.paint(&mut ui, &state, now, 80, 24).await;
    }
    fixture
        .press(&mut ui, &state, key(KeyCode::Enter), now)
        .await;
    fixture.paint(&mut ui, &state, now, 80, 24).await;
    let before = current(&ui).unwrap();
    let copy = fixture
        .press(&mut ui, &state, key(KeyCode::Char('y')), now)
        .await;
    assert!(
        matches!(&copy[0], Effect::Copy(text) if text.contains("one\ntwo") && !text.contains("\\n"))
    );
    fixture
        .press(&mut ui, &state, key(KeyCode::Enter), now)
        .await;
    assert!(matches!(ui.focus.open(), Open::Pager(p) if p.return_to.is_some()));
    assert_eq!(request(&ui), before.action.args["handler"].as_str());
    crate::input::on_key(&mut ui, &solo(&state), key(KeyCode::Esc), now);
    assert!(active(&ui));
    fixture.paint(&mut ui, &state, now, 80, 24).await;
    let after = current(&ui).unwrap();
    assert_eq!(
        (&after.query, after.current, &after.selected),
        (&before.query, before.current, &before.selected)
    );
}

#[tokio::test]
async fn new_items_refresh_a_sheet_and_missing_turns_do_not_preview() {
    let fixture = Fixture::new();
    let (mut state, mut ui, now) = setup(&fixture);
    open(
        &mut ui,
        &solo(&state),
        Some(TurnId::from_raw("t2")),
        true,
        now,
    );
    fixture.paint(&mut ui, &state, now, 80, 24).await;
    state.transcript.apply(&Event::ItemCompleted {
        item: assistant("next", "another answer", ItemStatus::Completed),
    });
    fixture.paint(&mut ui, &state, now, 80, 24).await;
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
    state.transcript = Default::default();
    let screen = fixture.paint(&mut ui, &state, now, 80, 24).await;
    assert!(!screen.contains("rewind to turn"));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
}

#[test]
fn projection_cache_invalidates_on_length_query_and_generation() {
    let fixture = Fixture::new();
    let (mut state, mut ui, now) = setup(&fixture);
    open(&mut ui, &solo(&state), None, false, now);
    let mut props = props(&ui, &solo(&state)).unwrap();
    props.query = "fork".into();
    ui.placed.browse.fill(&state, &mut props);
    assert_eq!(props.count, 2);
    state.transcript.apply(&Event::ItemCompleted {
        item: assistant("next", "fork", ItemStatus::Completed),
    });
    ui.placed.browse.fill(&state, &mut props);
    assert_eq!(props.count, 3);
    props.query = "second line".into();
    ui.placed.browse.fill(&state, &mut props);
    assert_eq!(props.count, 1);
    state.transcript.apply(&Event::Rewound {
        generation: 1,
        to_turn: TurnId::from_raw("t1"),
        dropped: vec![ItemId::from_raw("i1")],
        files_restored: vec![],
    });
    ui.placed.browse.fill(&state, &mut props);
    assert_eq!(props.count, 0);
    let first = ui
        .placed
        .browse
        .accepted("test", 1, || Some(props.clone()))
        .unwrap();
    let same = ui
        .placed
        .browse
        .accepted("test", 1, || panic!("decoded twice"))
        .unwrap();
    assert!(Arc::ptr_eq(&first, &same));
    let next = ui
        .placed
        .browse
        .accepted("test", 2, || Some(props))
        .unwrap();
    assert!(!Arc::ptr_eq(&first, &next));
}

#[test]
fn projection_cache_sees_a_streamed_or_rewritten_item() {
    let fixture = Fixture::new();
    let (mut state, mut ui, now) = setup(&fixture);
    open(&mut ui, &solo(&state), None, false, now);
    let mut props = props(&ui, &solo(&state)).unwrap();
    props.query = "needle".into();
    state.transcript.apply(&Event::ItemStarted {
        item: assistant("live", "hay", ItemStatus::Running),
    });
    ui.placed.browse.fill(&state, &mut props);
    assert_eq!(props.count, 0);
    state.transcript.apply(&Event::ItemDelta {
        item: ItemId::from_raw("live"),
        n: 1,
        kind: bingo_sdk::DeltaKind::Text,
        data: " needle".into(),
    });
    ui.placed.browse.fill(&state, &mut props);
    assert_eq!(props.count, 1, "a delta grows the item in place");
    state.transcript.apply(&Event::ItemUpdated {
        item: assistant("live", "hay again", ItemStatus::Running),
    });
    ui.placed.browse.fill(&state, &mut props);
    assert_eq!(props.count, 0, "an update rewrites the item in place");
}

#[test]
fn ordinary_pagers_keep_their_existing_content_projection() {
    let state = state();
    let row = user("user", "already visible");
    let mut pager = crate::pager::Pager::open(row.id.clone());
    assert!(crate::pager::content(&state, &row, 80, &pager).is_empty());
    pager.return_to = Some(crate::pager::Return::Layer(Box::new(Open::Browse(
        Opened {
            session: state.summary.id.clone(),
            id: "test".into(),
            selected: None,
            confirm: false,
        },
    ))));
    assert_eq!(
        crate::pager::content(&state, &row, 80, &pager)[0].to_string(),
        "already visible"
    );
}

fn busy_notices(ui: &Ui) -> usize {
    ui.notice().iter().filter(|n| n.text == BUSY).count()
}

#[tokio::test]
async fn busy_card_refuses_preview_without_reading_files() {
    let fixture = Fixture::new();
    let (mut state, mut ui, now) = setup(&fixture);
    ui.catalogs.commands.push(crate::commands::entry(
        serde_json::from_value(
            json!({"name":"rewind","hint":"rewind","args":{"kind":"none"},"family":"session"}),
        )
        .unwrap(),
    ));
    state.apply(&frame(1, started("running")));
    ui.focus
        .show(Open::Rewind(crate::rewind::Rewind::default()), now.instant);
    crate::input::on_key(&mut ui, &solo(&state), key(KeyCode::Enter), now);
    assert_eq!(busy_notices(&ui), 1);
    let screen = fixture.paint(&mut ui, &state, now, 80, 24).await;
    assert!(!screen.contains("rewind to turn"));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    assert!(!super::super::opened(&ui).unwrap().confirm);
    assert_eq!(busy_notices(&ui), 1);
}

#[tokio::test]
async fn a_running_turn_under_an_open_sheet_says_busy_once_per_key() {
    let fixture = Fixture::new();
    let (mut state, mut ui, now) = setup(&fixture);
    open(
        &mut ui,
        &solo(&state),
        Some(TurnId::from_raw("t2")),
        true,
        now,
    );
    let screen = fixture.paint(&mut ui, &state, now, 80, 24).await;
    assert!(screen.contains("undoes turn 2"), "{screen}");
    state.apply(&frame(1, started("wake")));
    state.transcript.apply(&Event::ItemCompleted {
        item: assistant("woken", "a background job finished", ItemStatus::Completed),
    });
    for _ in 0..20 {
        fixture.paint(&mut ui, &state, now, 80, 24).await;
    }
    assert_eq!(busy_notices(&ui), 0);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    fixture
        .press(&mut ui, &state, key(KeyCode::Enter), now)
        .await;
    fixture.paint(&mut ui, &state, now, 80, 24).await;
    assert_eq!(busy_notices(&ui), 1);
}
