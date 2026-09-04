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

/// A root, a sub-agent and a room, all in the store, on a host that is about
/// to be forgotten; the root's id.
async fn a_tree_on(store: &Arc<crate::journal::MemoryStore>) -> (SessionId, SessionId, SessionId) {
    let host = host_on(
        store.clone(),
        ScriptedProvider::new(vec![Script::Events(text("one"))]),
    )
    .await;
    let mut root = host
        .open(
            SessionSelector::Create {
                spec: spec("/work"),
            },
            who(),
            OpenOptions::default(),
        )
        .await
        .unwrap();
    one_turn(&mut root, "hello").await;
    let reviewer = born(&host, &root.session, "reviewer", Driver::Model).await;
    let journal = born(&host, &root.session, "#design", Driver::Log).await;
    (root.session.clone(), reviewer, journal)
}

/// One child of `parent`, opened and left in the store.
async fn born(host: &Arc<Host>, parent: &SessionId, title: &str, driver: Driver) -> SessionId {
    let spec = SessionSpec {
        driver,
        title: Some(title.into()),
        parent: Some(ParentLink {
            session: parent.clone(),
            item: Some(ItemId::mint()),
        }),
        ..spec("/work")
    };
    host.open(
        SessionSelector::Create { spec },
        who(),
        OpenOptions::default(),
    )
    .await
    .unwrap()
    .session
}

/// Read the tree's stream until every session named has been heard from.
async fn frames_until(attachment: &mut Attachment, want: &[&SessionId]) -> Vec<Frame> {
    let mut seen: Vec<Frame> = Vec::new();
    while let Some(frame) = attachment.events.next().await {
        seen.push(frame);
        if want.iter().all(|id| seen.iter().any(|f| &&f.session == id)) {
            break;
        }
    }
    seen
}

/// The frames one session contributed, in the order the client saw them.
fn of<'a>(frames: &'a [Frame], session: &SessionId) -> Vec<&'a Frame> {
    frames.iter().filter(|f| &f.session == session).collect()
}

/// A resume revives the root alone (ADR-0005), so a tree attachment answers
/// from both authorities: the descendants this host runs are followed live,
/// and the ones only the store knows are replayed onto the same stream. The
/// client folds every row and every view of the tree from frames either way.
#[tokio::test]
async fn a_resumed_tree_replays_the_descendants_only_the_store_has() {
    let store = Arc::new(crate::journal::MemoryStore::new());
    let (root, reviewer, journal) = a_tree_on(&store).await;
    // A grandchild goes deeper than a live host mints, and the walk still
    // reaches it: the store is the map, not the live table.
    plant_child(
        &store,
        &reviewer,
        "ses_helper",
        "helper",
        Driver::Model,
        true,
    )
    .await;
    let helper = SessionId::from_raw("ses_helper");

    let host_b = host_on(store.clone(), ScriptedProvider::new(vec![])).await;
    let mut b = host_b
        .open(
            SessionSelector::ById { id: root.clone() },
            who(),
            OpenOptions::with_children(),
        )
        .await
        .unwrap();
    for id in [&reviewer, &journal, &helper] {
        assert!(
            host_b.live(id).is_err(),
            "the resume revived the root alone"
        );
    }

    let seen = frames_until(&mut b, &[&reviewer, &journal, &helper]).await;
    for (id, title) in [
        (&reviewer, "reviewer"),
        (&journal, "#design"),
        (&helper, "helper"),
    ] {
        let frames = of(&seen, id);
        let head = frames.first().expect("a head");
        assert_eq!(head.seq, Seq(1), "replayed from the head");
        let Event::SessionUpdated { summary } = &head.event else {
            panic!("a session announces itself: {:?}", head.event)
        };
        assert_eq!(summary.title.as_deref(), Some(title));
        assert!(frames.windows(2).all(|w| w[0].seq < w[1].seq));
    }
}

/// A replayed child that wakes on this host carries on from where its replay
/// stopped: the client is given what it has not seen and no frame twice.
#[tokio::test]
async fn a_replayed_child_that_wakes_repeats_no_frame() {
    let store = Arc::new(crate::journal::MemoryStore::new());
    let (root, reviewer, journal) = a_tree_on(&store).await;
    let stored = store.replay(&reviewer, Seq::ZERO).await.unwrap();
    let replayed_to = stored.last().expect("a journal").seq;

    let host_b = host_on(store.clone(), ScriptedProvider::new(vec![])).await;
    let mut b = host_b
        .open(
            SessionSelector::ById { id: root },
            who(),
            OpenOptions::with_children(),
        )
        .await
        .unwrap();
    let replay = frames_until(&mut b, &[&reviewer, &journal]).await;

    host_b
        .deliver(
            &reviewer,
            IntentId::mint(),
            Input::text("are you there", Origin::surface("agent")),
            Delivery::Hold,
        )
        .await
        .expect("the stored child wakes");
    let mut seqs: Vec<Seq> = of(&replay, &reviewer).iter().map(|f| f.seq).collect();
    while let Some(frame) = b.events.next().await {
        if frame.session != reviewer {
            continue;
        }
        seqs.push(frame.seq);
        if matches!(frame.event, Event::QueueChanged { .. }) {
            break;
        }
    }
    assert!(
        seqs.iter().any(|seq| *seq > replayed_to),
        "the live stream carried on past the replay: {seqs:?}"
    );
    assert!(
        seqs.windows(2).all(|w| w[0] < w[1]),
        "no seq reached the client twice: {seqs:?}"
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

/// A session that was moved to another model with `/model` comes back on
/// that model, and says so (user-reported: "resume 后模型展示和实际使用的模型
/// 不对"). Two halves of one fact: the spec a resume rebuilds is what the
/// session *last* was, not what it first was — the journal's first frame
/// predates the `/model` that rewrote it (ADR-0008 §4) — and the head of the
/// new segment is stamped from the choice that was actually resolved, so no
/// surface can be shown a model that is not the one answering.
#[tokio::test]
async fn a_resumed_session_comes_back_on_the_model_it_was_moved_to() {
    let store = Arc::new(crate::journal::MemoryStore::new());
    let host_a = host_on(store.clone(), ScriptedProvider::new(vec![])).await;
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
    assert_eq!(a.snapshot.summary.model.as_deref(), Some("m"));
    let id = a.session.clone();

    a.handle.submit(
        IntentId::mint(),
        Input::text("/model m2", Origin::surface("test")),
    );
    while let Some(frame) = a.events.next().await {
        a.snapshot.apply(&frame);
        if matches!(frame.event, Event::IntentAck { .. }) {
            break;
        }
    }
    assert_eq!(a.snapshot.summary.model.as_deref(), Some("m2"));

    let answers = ScriptedProvider::new(vec![Script::Events(text("back"))]);
    let host_b = host_on(store.clone(), answers.clone()).await;
    let mut b = host_b
        .open(
            SessionSelector::ById { id: id.clone() },
            who(),
            OpenOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        b.snapshot.summary.model.as_deref(),
        Some("m2"),
        "the resumed session says the model it was left on"
    );

    one_turn(&mut b, "hello").await;
    assert_eq!(
        answers.requests()[0].model,
        "m2",
        "and it is the model the turn asked for"
    );
}

/// The level a session was thinking at is in its own config view, and a
/// resume reads the fold — so it comes back thinking as hard as it was left
/// (user-reported: "thinking 没有记住, 现在默认貌似是 off"). The settings say
/// nothing about thinking here, which is what would have made it `off`.
#[tokio::test]
async fn a_resumed_session_thinks_as_hard_as_it_was_left() {
    let store = Arc::new(crate::journal::MemoryStore::new());
    let host_a = reasoning_host_on(store.clone(), ScriptedProvider::new(vec![])).await;
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
    let id = a.session.clone();
    a.handle.submit(
        IntentId::mint(),
        Input::text("/think high", Origin::surface("test")),
    );
    while let Some(frame) = a.events.next().await {
        a.snapshot.apply(&frame);
        if matches!(frame.event, Event::IntentAck { .. }) {
            break;
        }
    }
    assert_eq!(a.snapshot.config.kernel["thinking"], json!("high"));

    let answers = ScriptedProvider::new(vec![Script::Events(text("back"))]);
    let host_b = reasoning_host_on(store.clone(), answers.clone()).await;
    let mut b = host_b
        .open(SessionSelector::ById { id }, who(), OpenOptions::default())
        .await
        .unwrap();
    assert_eq!(
        b.snapshot.config.kernel["thinking"],
        json!("high"),
        "the resumed session says the level it was left at"
    );
    one_turn(&mut b, "hello").await;
    assert_eq!(
        answers.requests()[0].reasoning,
        Some(Effort::High),
        "and it is the level the turn asked for"
    );
}

/// [`host_on`] with a model that declares reasoning, and no thinking level
/// in the settings — what would make a resumed session `off`.
async fn reasoning_host_on(
    store: Arc<crate::journal::MemoryStore>,
    provider: Arc<ScriptedProvider>,
) -> Arc<Host> {
    let plugins = vec![
        TestPlugin::boxed(&PROVIDER, vec![Contribution::Provider(provider)]),
        TestPlugin::boxed(&STORE, vec![Contribution::Store(store)]),
    ];
    let config = HostConfig::new(env()).with_layer(
        "cli",
        json!({"model": "m", "models": {"scripted/m": {"reasoning": true}}}),
    );
    Host::build(plugins, config).await.unwrap()
}

/// A name outlives the process that gave it: `/rename` goes onto the summary,
/// the summary is the journal's, and a resume reads the journal's fold — so
/// the session comes back called what it was called.
#[tokio::test]
async fn a_renamed_session_comes_back_under_its_name() {
    let store = Arc::new(crate::journal::MemoryStore::new());
    let host_a = host_on(store.clone(), ScriptedProvider::new(vec![])).await;
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
    let id = a.session.clone();

    a.handle.submit(
        IntentId::mint(),
        Input::text("/rename the release", Origin::surface("test")),
    );
    while let Some(frame) = a.events.next().await {
        a.snapshot.apply(&frame);
        if matches!(frame.event, Event::IntentAck { .. }) {
            break;
        }
    }
    assert_eq!(a.snapshot.summary.title.as_deref(), Some("the release"));

    let host_b = host_on(store.clone(), ScriptedProvider::new(vec![])).await;
    let b = host_b
        .open(
            SessionSelector::ById { id: id.clone() },
            who(),
            OpenOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(b.snapshot.summary.title.as_deref(), Some("the release"));
    assert_eq!(
        host_b
            .sessions(SessionFilter::default())
            .await
            .unwrap()
            .iter()
            .find(|s| s.id == id)
            .and_then(|s| s.title.clone())
            .as_deref(),
        Some("the release"),
        "and every list that names it says the same"
    );
}
