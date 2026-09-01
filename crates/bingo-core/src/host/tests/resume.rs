//! A session read back from its store on another host (ADR-0005 §7): the
//! journal is replayed, the fold is the one reducer's, and what the store
//! knows of the children is said before anything new happens (M31).

use super::*;

#[tokio::test]
async fn a_stored_session_reopens_on_another_host_with_its_history() {
    let store = Arc::new(crate::journal::MemoryStore::new());
    let first = ScriptedProvider::new(vec![Script::Events(text("first answer"))]);
    let host_a = host_on(store.clone(), first).await;
    let mut a = host_a
        .open(
            SessionSelector::Create {
                spec: spec("/work"),
            },
            who(),
            OpenOptions::default(),
        )
        .await
        .unwrap();
    let ended_at = one_turn(&mut a, "hello").await;
    let id = a.session.clone();

    let second = ScriptedProvider::new(vec![Script::Events(text("second answer"))]);
    let host_b = host_on(store.clone(), second.clone()).await;
    let mut b = host_b
        .open(
            SessionSelector::ById { id: id.clone() },
            who(),
            OpenOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(b.session, id);
    assert!(
        b.snapshot.seq > ended_at,
        "a new head after the old journal"
    );
    assert!(
        b.snapshot
            .items
            .iter()
            .any(|i| matches!(&i.body, ItemBody::Assistant { text } if text == "first answer")),
        "the old items are in the snapshot"
    );
    assert!(!b.snapshot.busy());

    one_turn(&mut b, "again").await;
    let sent = &second.requests()[0].messages;
    assert!(
        sent.iter()
            .any(|m| m.parts.iter().any(|p| p.as_text() == Some("first answer"))),
        "the next request carries the old conversation: {sent:?}"
    );

    let listed = host_b
        .sessions(SessionFilter {
            cwd: Some("/work".into()),
            ..SessionFilter::default()
        })
        .await
        .unwrap();
    assert_eq!(listed.iter().map(|s| &s.id).collect::<Vec<_>>(), [&id]);
}

/// A child in the store that no host has opened: its head, and a turn it
/// either finished or was still inside when the last process ended.
async fn plant_child(
    store: &crate::journal::MemoryStore,
    root: &SessionId,
    id: &str,
    title: &str,
    driver: Driver,
    finished: bool,
) {
    let head = SessionSummary {
        title: Some(title.into()),
        parent: Some(ParentLink {
            session: root.clone(),
            item: None,
        }),
        cwd: "/work".into(),
        driver,
        ..summary(id)
    };
    let turn = TurnId::from_raw("trn_1");
    let mut frames = vec![
        Event::SessionUpdated {
            summary: head.clone(),
        },
        Event::TurnStarted {
            turn: turn.clone(),
            inputs: Vec::new(),
            origin: TurnOrigin::Submit,
        },
    ];
    if finished {
        frames.push(Event::TurnCompleted {
            turn,
            status: TurnStatus::Completed,
            usage: Usage::default(),
        });
    }
    store.create(&head).await.unwrap();
    for (n, event) in frames.into_iter().enumerate() {
        let frame = Frame {
            seq: Seq(n as u64 + 1),
            ts: head.created_at,
            session: head.id.clone(),
            cause: None,
            event,
        };
        store.append(&head.id, &frame).await.unwrap();
    }
}

/// M31: a child whose stored journal ends inside a turn was at work when the
/// last process ended, and the report it owed will not arrive. The session
/// that comes back is told once, in its own transcript; the child is neither
/// reopened nor rewritten, and `recover` still owns the turn it left open.
#[tokio::test]
async fn a_resumed_session_is_told_which_child_was_mid_turn() {
    let store = Arc::new(crate::journal::MemoryStore::new());
    let host_a = host_on(
        store.clone(),
        ScriptedProvider::new(vec![Script::Events(text("one"))]),
    )
    .await;
    let mut a = host_a
        .open(
            SessionSelector::Create {
                spec: spec("/work"),
            },
            who(),
            OpenOptions::default(),
        )
        .await
        .unwrap();
    one_turn(&mut a, "hello").await;
    let root = a.session.clone();

    let busy = SessionId::from_raw("ses_busy");
    plant_child(&store, &root, "ses_busy", "slow", Driver::Model, false).await;
    plant_child(&store, &root, "ses_done", "quick", Driver::Model, true).await;
    // A session no model answers never opened a turn, whatever is written in
    // it: it is skipped rather than replayed (ADR-0011 §1).
    plant_child(&store, &root, "ses_log", "#design", Driver::Log, false).await;
    let before = store.replay(&busy, Seq::ZERO).await.unwrap();

    let host_b = host_on(store.clone(), ScriptedProvider::new(vec![])).await;
    let b = host_b
        .open(
            SessionSelector::ById { id: root.clone() },
            who(),
            OpenOptions::default(),
        )
        .await
        .unwrap();

    let said: Vec<String> = b
        .snapshot
        .items
        .iter()
        .filter_map(|item| match &item.body {
            ItemBody::Notice { code, text, .. } if code == "CHILD_TURN_LOST" => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(said.len(), 1, "only the one that was working: {said:?}");
    assert!(
        said[0].contains("slow") && said[0].contains("ses_busy"),
        "{}",
        said[0]
    );
    assert!(
        !said[0].contains("resumed") && !said[0].contains("again from"),
        "it promises nothing that did not happen: {}",
        said[0]
    );

    assert_eq!(
        store.replay(&busy, Seq::ZERO).await.unwrap(),
        before,
        "the child's journal is not rewritten"
    );
    assert!(
        host_b.session_state(&busy).await.is_err(),
        "and the child is not reopened"
    );
}

/// A root with nothing behind it says nothing at all.
#[tokio::test]
async fn a_resumed_session_with_no_lost_child_says_nothing() {
    let store = Arc::new(crate::journal::MemoryStore::new());
    let host_a = host_on(
        store.clone(),
        ScriptedProvider::new(vec![Script::Events(text("one"))]),
    )
    .await;
    let mut a = host_a
        .open(
            SessionSelector::Create {
                spec: spec("/work"),
            },
            who(),
            OpenOptions::default(),
        )
        .await
        .unwrap();
    one_turn(&mut a, "hello").await;
    let root = a.session.clone();
    plant_child(&store, &root, "ses_done", "quick", Driver::Model, true).await;

    let host_b = host_on(store.clone(), ScriptedProvider::new(vec![])).await;
    let b = host_b
        .open(
            SessionSelector::ById { id: root },
            who(),
            OpenOptions::default(),
        )
        .await
        .unwrap();
    assert!(
        !b.snapshot.items.iter().any(
            |item| matches!(&item.body, ItemBody::Notice { code, .. } if code == "CHILD_TURN_LOST")
        ),
        "{:?}",
        b.snapshot.items
    );
}

#[tokio::test]
async fn latest_in_a_directory_comes_from_the_store_when_nothing_is_live() {
    let store = Arc::new(crate::journal::MemoryStore::new());
    let host_a = host_on(
        store.clone(),
        ScriptedProvider::new(vec![Script::Events(text("one"))]),
    )
    .await;
    let mut a = host_a
        .open(
            SessionSelector::Create {
                spec: spec("/work"),
            },
            who(),
            OpenOptions::default(),
        )
        .await
        .unwrap();
    one_turn(&mut a, "hello").await;

    let host_b = host_on(store.clone(), ScriptedProvider::new(vec![])).await;
    let b = host_b
        .open(
            SessionSelector::Latest {
                cwd: "/work".into(),
            },
            who(),
            OpenOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(b.session, a.session);
    assert_eq!(b.snapshot.summary.model.as_deref(), Some("m"));
    let missing = host_b
        .open(
            SessionSelector::Latest {
                cwd: "/elsewhere".into(),
            },
            who(),
            OpenOptions::default(),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(missing.code, ErrorCode::SessionNotFound);
    let unknown = host_b
        .open(
            SessionSelector::ById {
                id: SessionId::from_raw("ses_nope"),
            },
            who(),
            OpenOptions::default(),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(unknown.code, ErrorCode::SessionNotFound);
}

/// A `Log` session (ADR-0011 §1) resolves no model: a host with no provider
/// at all opens one, records what it is told, and refuses a model change.
#[tokio::test]
async fn a_log_session_needs_no_provider_and_answers_nothing() {
    let host = Host::build(vec![], HostConfig::new(env())).await.unwrap();
    let spec = SessionSpec {
        driver: Driver::Log,
        title: Some("#design".into()),
        ..spec("/work")
    };
    let mut journal = host
        .open(
            SessionSelector::Create { spec },
            who(),
            OpenOptions::default(),
        )
        .await
        .expect("no provider is needed");
    assert_eq!(journal.snapshot.summary.driver, Driver::Log);
    assert!(journal.snapshot.summary.model.is_none());

    journal.handle.submit(
        IntentId::mint(),
        Input::text("hello", Origin::surface("test")),
    );
    let mut recorded = false;
    while let Some(frame) = journal.events.next().await {
        match &frame.event {
            Event::ItemCompleted { .. } => recorded = true,
            Event::IntentAck {
                outcome: IntentOutcome::Applied { .. },
                ..
            } => break,
            Event::TurnStarted { .. } => panic!("a log opens no turn"),
            _ => {}
        }
    }
    assert!(recorded, "the input is the journal's");

    let err = host
        .reconfigure(
            &journal.session,
            Change::Model {
                provider: None,
                model: "m".into(),
            },
        )
        .await
        .expect_err("there is no model to change");
    assert_eq!(err.code, ErrorCode::InvalidInput);
}

/// `deliver` and `extend` reopen a session that is persisted but not live
/// (ADR-0011 §3), so a roster read from the store can be written to.
#[tokio::test]
async fn a_delivery_reaches_a_stored_session_that_is_not_live() {
    let store = Arc::new(crate::journal::MemoryStore::new());
    let first = ScriptedProvider::new(vec![Script::Events(text("first answer"))]);
    let host_a = host_on(store.clone(), first).await;
    let mut a = host_a
        .open(
            SessionSelector::Create {
                spec: spec("/work"),
            },
            who(),
            OpenOptions::default(),
        )
        .await
        .unwrap();
    one_turn(&mut a, "hello").await;
    let id = a.session.clone();
    drop(a);

    let host_b = host_on(store.clone(), ScriptedProvider::new(vec![])).await;
    assert!(host_b.live(&id).is_err(), "nothing of it is live here yet");
    let from_peer = Input::text(
        "are you there",
        Origin {
            surface: "agent".into(),
            principal: Some("scout".into()),
            conversation: None,
        },
    );
    host_b
        .deliver(&id, IntentId::mint(), from_peer, Delivery::Hold)
        .await
        .expect("reopened and delivered");
    assert!(host_b.live(&id).is_ok(), "the delivery reopened it");
    host_b
        .extend(&id, "bingo.test", "things", json!([1]))
        .await
        .expect("extended in place");

    let b = host_b
        .open(
            SessionSelector::ById { id: id.clone() },
            who(),
            OpenOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        b.snapshot.queue.len(),
        1,
        "held in the queue of an idle session"
    );
    assert_eq!(b.snapshot.extensions["bingo.test"]["things"], json!([1]));
}

/// `--continue` means the person's session: `Latest` prefers a root over a
/// child under it, live or in the store, though the child is newer.
#[tokio::test]
async fn latest_prefers_a_root_over_the_newer_child_under_it() {
    let store = Arc::new(crate::journal::MemoryStore::new());
    let host_a = host_on(store.clone(), ScriptedProvider::new(vec![])).await;
    let root = host_a
        .open(
            SessionSelector::Create {
                spec: spec("/work"),
            },
            who(),
            OpenOptions::default(),
        )
        .await
        .unwrap()
        .session;
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let child = SessionSpec {
        parent: Some(ParentLink {
            session: root.clone(),
            item: None,
        }),
        title: Some("reviewer".into()),
        ..spec("/work")
    };
    host_a
        .open(
            SessionSelector::Create { spec: child },
            who(),
            OpenOptions::default(),
        )
        .await
        .unwrap();
    let latest = SessionSelector::Latest {
        cwd: "/work".into(),
    };
    let live = host_a
        .open(latest.clone(), who(), OpenOptions::default())
        .await
        .unwrap();
    assert_eq!(live.session, root, "live: the root, not its newer child");

    let host_b = host_on(store, ScriptedProvider::new(vec![])).await;
    let stored = host_b
        .open(latest, who(), OpenOptions::default())
        .await
        .unwrap();
    assert_eq!(
        stored.session, root,
        "stored: the root, not its newer child"
    );
}

/// What a session was opened with comes back with it: its extra system
/// prompt and its tool set are in its summary, so a resume gives them back.
#[tokio::test]
async fn a_resumed_session_keeps_its_system_prompt_and_tool_set() {
    let store = Arc::new(crate::journal::MemoryStore::new());
    let host_a = host_on(store.clone(), ScriptedProvider::new(vec![])).await;
    let opened = SessionSpec {
        system_extra: Some("Be brief.".into()),
        tools: Some(vec!["Echo".into()]),
        ..spec("/work")
    };
    let id = host_a
        .open(
            SessionSelector::Create { spec: opened },
            who(),
            OpenOptions::default(),
        )
        .await
        .unwrap()
        .session;

    let second = ScriptedProvider::new(vec![Script::Events(text("ok"))]);
    let plugins = vec![
        TestPlugin::boxed(&PROVIDER, vec![Contribution::Provider(second.clone())]),
        TestPlugin::boxed(&STORE, vec![Contribution::Store(store)]),
        TestPlugin::boxed(
            &TOOLS,
            vec![Contribution::Tool(Arc::new(EchoTool { read_only: true }))],
        ),
    ];
    let config = HostConfig::new(env()).with_layer("cli", json!({"model": "m"}));
    let host_b = Host::build(plugins, config).await.unwrap();
    let mut b = host_b
        .open(SessionSelector::ById { id }, who(), OpenOptions::default())
        .await
        .unwrap();
    assert_eq!(
        b.snapshot.summary.system_extra.as_deref(),
        Some("Be brief.")
    );
    assert_eq!(b.snapshot.summary.tools, Some(vec!["Echo".to_string()]));
    one_turn(&mut b, "hello").await;
    let request = &second.requests()[0];
    assert!(
        request
            .system
            .iter()
            .any(|block| block.text.contains("Be brief.")),
        "the resumed turn's system prompt carries it"
    );
    assert_eq!(
        request
            .tools
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        ["Echo"],
        "the resumed turn is held to the tool set"
    );
}
