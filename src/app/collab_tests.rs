//! The collaboration domain, driven through the actor.
//!
//! One conversation model, two frontends: what these assert is what a room, a
//! direct message and an absorbed prompt *become* in the core, because that is
//! the only thing both the terminal front end and a GUI will read.

use std::sync::Arc;

use crate::app::command::{AppCommand, AppQuery};
use crate::app::conversation::ConvKey;
use crate::app::event::{AppEvent, AppEventPayload};
use crate::app::ids::{ConversationId, ItemId};
use crate::app::snapshot::{
    ConversationSummary, DeliveryState, Item, ItemBody, ItemCursor, TurnOrigin,
};
use crate::app::{
    AppCore, AppError, AppFrame, AppLink, AppReply, AppRequest, AttachRequest, RequestId,
    SessionSetup,
};
use crate::channels::{ChannelMode, MAIN_NAME, USER_NAME};
use crate::engine::events::EngineEvent;
use crate::query::Session;

fn test_session(core: &AppCore) -> Arc<Session> {
    Arc::new(Session {
        client: crate::api::client::Client::new("k".into(), "http://x".into()),
        runtime: crate::query::Runtime::new("m".into(), None, Default::default()),
        permission_mode: crate::permission::PermissionMode::Default,
        settings: crate::settings::Settings::default(),
        system: Vec::new(),
        depth: 1,
        cwd: Arc::new(std::sync::Mutex::new(std::env::temp_dir())),
        home: std::env::temp_dir(),
        user_config_dir: std::env::temp_dir().join(".config"),
        quiet: true,
        compact_failures: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        watch: core.watch(),
        tasks: Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "t")),
        expand_tasks: tokio::sync::watch::channel(false).0,
        agents: core.agents(),
        channels: core.channels(),
        turns: core.turns(),
        queue: core.queue(),
        submit: core.submit(),
        interactions: core.interactions(),
        mail: core.mail(),
        operations: core.operations(),
        instance: None,
        attachments: crate::api::image::Attachments::new(),
    })
}

/// Attach and take the cut every attachment starts from.
async fn attached(core: &AppCore) -> AppLink {
    let mut link = core
        .attach(AttachRequest::new("collab"))
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    link.request(AppRequest::Query {
        id: RequestId(1),
        query: AppQuery::ReadSession,
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
    match link.recv().await {
        Some(AppFrame::Reply { .. }) => link,
        other => panic!("expected the session cut, got {other:?}"),
    }
}

/// Everything the core said until it went quiet.
async fn drain(link: &mut AppLink) -> Vec<AppEvent> {
    let mut seen = Vec::new();
    while let Ok(Some(frame)) =
        tokio::time::timeout(std::time::Duration::from_millis(200), link.recv()).await
    {
        if let AppFrame::Event(event) = frame {
            seen.push(*event);
        }
    }
    seen
}

fn items(events: &[AppEvent]) -> Vec<&Item> {
    events
        .iter()
        .filter_map(|event| match &event.payload {
            AppEventPayload::ItemCompleted(changed) => Some(&changed.item),
            _ => None,
        })
        .collect()
}

fn summaries(events: &[AppEvent]) -> Vec<&ConversationSummary> {
    events
        .iter()
        .filter_map(|event| match &event.payload {
            AppEventPayload::ConversationCreated(changed)
            | AppEventPayload::ConversationUpdated(changed) => Some(&changed.conversation),
            _ => None,
        })
        .collect()
}

async fn read_conversation(
    link: &mut AppLink,
    id: &ConversationId,
    cursor: Option<ItemCursor>,
) -> Result<crate::app::snapshot::ConversationSnapshot, AppError> {
    link.request(AppRequest::Query {
        id: RequestId(77),
        query: AppQuery::ReadConversation {
            conversation_id: id.clone(),
            cursor,
            limit: None,
        },
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
    loop {
        match link.recv().await {
            Some(AppFrame::Reply {
                id: RequestId(77),
                result,
            }) => {
                return match result {
                    Ok(AppReply::Conversation(snapshot)) => Ok(*snapshot),
                    Ok(other) => panic!("expected a conversation, got {other:?}"),
                    Err(error) => Err(error),
                };
            }
            Some(_) => {}
            None => panic!("the core closed"),
        }
    }
}

/// Send one command and wait for its own reply.
async fn command(link: &mut AppLink, id: u64, command: AppCommand) -> Result<AppReply, AppError> {
    link.request(AppRequest::Command {
        id: RequestId(id),
        command,
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
    loop {
        match link.recv().await {
            Some(AppFrame::Reply { id: seen, result }) if seen == RequestId(id) => return result,
            Some(_) => {}
            None => panic!("the core closed"),
        }
    }
}

/// The conversation the newest summary named for this key.
fn conversation_of(events: &[AppEvent], title: &str) -> ConversationId {
    summaries(events)
        .into_iter()
        .find(|summary| summary.title == title)
        .map(|summary| summary.id.clone())
        .unwrap_or_else(|| panic!("no summary for {title}"))
}

/// A room post is a completed message item with no turn — and so is a join.
/// There is no synthetic room turn, and no second representation of either.
#[tokio::test]
async fn a_room_post_is_a_message_item_with_no_turn() {
    let core = AppCore::start(SessionSetup::default());
    let channels = core.channels();
    let mut link = attached(&core).await;
    channels
        .create(
            "build",
            vec![MAIN_NAME.to_string(), "scout".to_string()],
            ChannelMode::Free,
        )
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    channels
        .invite("build", USER_NAME)
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    channels
        .post("scout", "build", "@user the suite is green")
        .await
        .unwrap_or_else(|error| panic!("{error}"));

    let events = drain(&mut link).await;
    let posts: Vec<&Item> = items(&events)
        .into_iter()
        .filter(|item| matches!(item.body, ItemBody::RoomMessage { .. }))
        .collect();
    match posts.as_slice() {
        [join, said] => {
            assert!(
                join.turn_id.is_none() && said.turn_id.is_none(),
                "a room receives posts and never owns a turn"
            );
            match &join.body {
                ItemBody::RoomMessage { from, text, .. } => {
                    assert_eq!((from.as_str(), text.as_str()), (USER_NAME, "joined"),);
                }
                other => panic!("expected the membership entry, got {other:?}"),
            }
            match &said.body {
                ItemBody::RoomMessage {
                    from,
                    text,
                    room_seq,
                    mentions,
                    ..
                } => {
                    assert_eq!(from, "scout");
                    assert_eq!(text, "@user the suite is green");
                    assert_eq!(*room_seq, 2, "the room's own sequence travels with it");
                    assert_eq!(mentions, &vec!["user".to_string()]);
                }
                other => panic!("expected the post, got {other:?}"),
            }
        }
        other => panic!("expected the join and the post, got {other:?}"),
    }

    // The room's own summary carries the attention a badge is drawn from.
    let room = summaries(&events)
        .into_iter()
        .rfind(|summary| summary.title == "#build")
        .unwrap_or_else(|| panic!("the room has a summary"))
        .clone();
    assert_eq!(room.unread, 1, "the join is roster news, not a message");
    assert_eq!(room.mentions, 1);
    assert!(room.is_member, "the user joined");
    assert_eq!(
        room.run_state,
        crate::app::snapshot::ConversationRunState::Passive
    );
}

/// A direct message becomes an item in the receiver's conversation at the moment
/// it is delivered (D135), with the delivery record it belongs to.
#[tokio::test]
async fn a_direct_message_becomes_an_item_when_it_is_delivered() {
    let core = AppCore::start(SessionSetup::default());
    let agents = core.agents();
    let session = test_session(&core);
    agents
        .insert(
            "scout",
            crate::agents::AgentKind::Crew,
            None,
            "the scout".to_string(),
            session,
        )
        .await;
    let mut link = attached(&core).await;
    agents
        .deliver("scout", USER_NAME, "look at the lexer", Vec::new(), None)
        .await
        .unwrap_or_else(|error| panic!("{error}"));

    let events = drain(&mut link).await;
    match items(&events).as_slice() {
        [item] => match &item.body {
            ItemBody::PeerMessage {
                from,
                to,
                text,
                delivery_id,
            } => {
                assert_eq!(from, USER_NAME);
                assert_eq!(to.as_deref(), Some("scout"));
                assert_eq!(
                    text, "look at the lexer",
                    "the item keeps what was sent, not the ack's excerpt"
                );
                assert!(delivery_id.is_some(), "it names the record it belongs to");
                assert!(item.turn_id.is_none());
            }
            other => panic!("expected the message, got {other:?}"),
        },
        other => panic!("expected exactly one item, got {other:?}"),
    }

    let delivery = events
        .iter()
        .find_map(|event| match &event.payload {
            AppEventPayload::DeliveryChanged(changed) => Some(&changed.delivery),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the delivery is reported"));
    assert_eq!(delivery.from, USER_NAME);
    assert_eq!(delivery.to, "scout");
    assert!(delivery.private);
    assert_eq!(
        delivery.state,
        DeliveryState::Delivered,
        "it is in the inbox and nobody has read it"
    );
    assert_eq!(delivery.follow_ups, 0);
}

/// D137, as the core reports it: a colleague's turn prose settles nothing, and
/// only a message back moves the record to `answered`.
#[tokio::test]
async fn a_peers_turn_prose_does_not_settle_the_message_it_was_sent() {
    let core = AppCore::start(SessionSetup::default());
    let agents = core.agents();
    for name in ["dev", "qa"] {
        agents
            .insert(
                name,
                crate::agents::AgentKind::Crew,
                None,
                name.to_string(),
                test_session(&core),
            )
            .await;
    }
    agents
        .deliver("qa", "dev", "is the suite green?", Vec::new(), None)
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    // qa reads it, then produces a whole turn of prose.
    let _ = agents.take_running("qa", 0).await;
    let mut link = attached(&core).await;
    let _ = agents.finish("qa", Vec::new(), 500).await;
    let events = drain(&mut link).await;
    let state = events
        .iter()
        .rev()
        .find_map(|event| match &event.payload {
            AppEventPayload::DeliveryChanged(changed) => Some(changed.delivery.state),
            _ => None,
        })
        .unwrap_or(DeliveryState::Read);
    assert_ne!(
        state,
        DeliveryState::Answered,
        "turn text goes to main; the colleague who asked cannot read it"
    );

    agents
        .deliver("dev", "qa", "green as of now", Vec::new(), None)
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    let events = drain(&mut link).await;
    let answered = events.iter().any(|event| match &event.payload {
        AppEventPayload::DeliveryChanged(changed) => {
            changed.delivery.from == "dev"
                && changed.delivery.to == "qa"
                && changed.delivery.state == DeliveryState::Answered
        }
        _ => false,
    });
    assert!(answered, "a message back is what settles it");
}

/// An absorbed prompt is read by the one walker. The direct message it repeats
/// is dropped — it already became an item when it was delivered — and the
/// scaffolding nobody typed becomes a notice rather than somebody's words.
#[tokio::test]
async fn an_absorbed_prompt_is_read_by_the_one_walker() {
    let core = AppCore::start(SessionSetup::default());
    let turns = core.turns();
    let mut link = attached(&core).await;
    let turn = turns
        .open(
            ConvKey::Agent("scout".to_string()),
            TurnOrigin::Peer,
            Vec::new(),
        )
        .await
        .unwrap_or_else(|| panic!("the instance was idle"));
    turns.report_event(
        turn.clone(),
        EngineEvent::Inbound(format!(
            "{}\nlook at the lexer",
            crate::tool::agent::DM_FROM_USER_MARKER
        )),
    );
    turns.report_event(
        turn.clone(),
        EngineEvent::Inbound("[#build msg #4] qa: the suite is red".to_string()),
    );
    turns.close(turn, crate::app::snapshot::TurnStatus::Completed, None);

    let events = drain(&mut link).await;
    let notices: Vec<String> = items(&events)
        .into_iter()
        .filter_map(|item| match &item.body {
            ItemBody::Notice { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(
        notices
            .iter()
            .all(|text| !text.contains("look at the lexer")),
        "the direct message already arrived when it was delivered (D135): {notices:?}"
    );
    assert!(
        notices.iter().any(|text| text.contains("the suite is red")),
        "a room relay never passed through a delivery, so it lands here: {notices:?}"
    );
}

/// A warning is feedback with a stable code, not turn state and not prose.
#[tokio::test]
async fn a_warning_from_a_run_is_feedback_with_a_stable_code() {
    let core = AppCore::start(SessionSetup::default());
    let turns = core.turns();
    let mut link = attached(&core).await;
    let turn = turns
        .open(ConvKey::Main, TurnOrigin::User, Vec::new())
        .await
        .unwrap_or_else(|| panic!("main was idle"));
    turns.report_event(
        turn.clone(),
        EngineEvent::Warning("the MCP server went away".to_string()),
    );
    turns.close(turn, crate::app::snapshot::TurnStatus::Completed, None);

    let events = drain(&mut link).await;
    let raised = events
        .iter()
        .find_map(|event| match &event.payload {
            AppEventPayload::FeedbackRaised(raised) => Some(&raised.feedback),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the warning is raised"));
    assert_eq!(raised.code, crate::error::RUNTIME_WARNING);
    assert_eq!(raised.message, "the MCP server went away");
    assert_eq!(raised.level, crate::app::snapshot::NoticeLevel::Warning);
    assert!(raised.conversation_id.is_some());
}

/// A room's history is really pageable, and a cursor is bound to the generation
/// it was issued under.
#[tokio::test]
async fn a_room_conversation_pages_its_own_history() {
    let core = AppCore::start(SessionSetup::default());
    let channels = core.channels();
    let mut link = attached(&core).await;
    channels
        .create(
            "build",
            vec![MAIN_NAME.to_string(), "scout".to_string()],
            ChannelMode::Free,
        )
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    for n in 1..=4 {
        channels
            .post("scout", "build", &format!("line {n}"))
            .await
            .unwrap_or_else(|error| panic!("{error}"));
    }
    let events = drain(&mut link).await;
    let room = conversation_of(&events, "#build");

    let page = read_conversation(&mut link, &room, None)
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(page.items.items.len(), 4);
    assert_eq!(page.history_generation, 1);
    let cursor = ItemCursor {
        history_generation: page.history_generation,
        after: page.items.items[1].id.clone(),
    };
    let rest = read_conversation(&mut link, &room, Some(cursor))
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(rest.items.items.len(), 2, "the page after the cursor");

    let stale = read_conversation(
        &mut link,
        &room,
        Some(ItemCursor {
            history_generation: 99,
            after: ItemId::new("item_1"),
        }),
    )
    .await;
    assert_eq!(
        stale,
        Err(AppError::Refused(
            crate::app_server::protocol::error::ProtocolErrorKind::StalePage
        )),
        "a continuation from a generation that no longer exists is refused"
    );
}

/// Reading has no attention side effect (spec invariant #14); marking read is
/// the only thing that clears a badge, and it names the revision it saw.
#[tokio::test]
async fn reading_a_room_never_marks_it_read() {
    let core = AppCore::start(SessionSetup::default());
    let channels = core.channels();
    let mut link = attached(&core).await;
    channels
        .create(
            "build",
            vec![
                MAIN_NAME.to_string(),
                "scout".to_string(),
                "user".to_string(),
            ],
            ChannelMode::Free,
        )
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    channels
        .post("scout", "build", "the suite is green")
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    let events = drain(&mut link).await;
    let room = conversation_of(&events, "#build");

    let before = read_conversation(&mut link, &room, None)
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(before.conversation.unread, 1);
    let again = read_conversation(&mut link, &room, None)
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        again.conversation.unread, 1,
        "reading the page twice is still reading"
    );

    let last = again
        .items
        .items
        .last()
        .map(|item| item.id.clone())
        .unwrap_or_else(|| panic!("the room has a post"));
    assert_eq!(
        command(
            &mut link,
            9,
            AppCommand::MarkRead {
                conversation_id: room.clone(),
                last_item_id: Some(last),
                last_room_seq: None,
                expected_revision: again.conversation.revision,
            },
        )
        .await,
        Ok(AppReply::Accepted)
    );
    let _ = drain(&mut link).await;
    let after = read_conversation(&mut link, &room, None)
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(after.conversation.unread, 0);
    assert_eq!(after.conversation.mentions, 0);
}

/// The golden one (Amendment #6): a session's rooms and the user's place in them
/// come back after a restart. The sidecar is the only thing that crossed.
#[tokio::test]
async fn a_resumed_session_comes_back_to_its_rooms_and_its_unread_marks() {
    let home = std::env::temp_dir().join(format!(
        "bingo-resume-{}-{}",
        std::process::id(),
        crate::app::ids::now_millis()
    ));
    let sidecar = crate::app::roomlog::path(&home, "notes-1");

    // --- the first session -------------------------------------------------
    {
        let core = AppCore::start(SessionSetup::default());
        let channels = core.channels();
        let mut link = attached(&core).await;
        channels.attach_sidecar(sidecar.clone());
        channels
            .create(
                "build",
                vec![MAIN_NAME.to_string(), "scout".to_string(), "qa".to_string()],
                ChannelMode::Free,
            )
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        channels
            .invite("build", USER_NAME)
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        for line in ["one", "two", "@user three"] {
            channels
                .post("scout", "build", line)
                .await
                .unwrap_or_else(|error| panic!("{error}"));
        }
        let events = drain(&mut link).await;
        let room = conversation_of(&events, "#build");
        let snapshot = read_conversation(&mut link, &room, None)
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(snapshot.conversation.unread, 3);
        assert_eq!(snapshot.conversation.mentions, 1);

        // The user reads the first two posts. The join took sequence 1, so the
        // second post is sequence 3 — a room's own sequence counts roster
        // changes too.
        let second = snapshot
            .items
            .items
            .iter()
            .find(|item| matches!(&item.body, ItemBody::RoomMessage { room_seq, .. } if *room_seq == 3))
            .map(|item| item.id.clone())
            .unwrap_or_else(|| panic!("the second post is in the log"));
        assert_eq!(
            command(
                &mut link,
                11,
                AppCommand::MarkRead {
                    conversation_id: room.clone(),
                    last_item_id: Some(second),
                    last_room_seq: Some(3),
                    expected_revision: snapshot.conversation.revision,
                },
            )
            .await,
            Ok(AppReply::Accepted),
            "the client marked the view it had just read"
        );
        let _ = drain(&mut link).await;
        let after = read_conversation(&mut link, &room, None)
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(after.conversation.unread, 1, "one post is still unread");
        core.close().await;
    }

    // --- and the one that resumes it ---------------------------------------
    let core = AppCore::start(SessionSetup::default());
    let channels = core.channels();
    let mut link = attached(&core).await;
    let replayed = crate::app::roomlog::replay(&sidecar);
    channels.restore_rooms(replayed);
    let events = drain(&mut link).await;
    let room = conversation_of(&events, "#build");
    let snapshot = read_conversation(&mut link, &room, None)
        .await
        .unwrap_or_else(|error| panic!("{error}"));

    let texts: Vec<String> = snapshot
        .items
        .items
        .iter()
        .filter_map(|item| match &item.body {
            ItemBody::RoomMessage { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        texts,
        vec!["joined", "one", "two", "@user three"],
        "the room came back with its whole log, roster changes included"
    );
    assert_eq!(
        snapshot.conversation.unread, 1,
        "and with the user where they left off"
    );
    assert_eq!(
        snapshot.conversation.mentions, 1,
        "including what named them"
    );
    assert!(snapshot.conversation.is_member, "the roster came back too");

    // The `@` ledger is re-derived rather than replayed, so a resumed session
    // still knows what the room owes.
    let owed = channels.owed_in("build");
    assert_eq!(owed.len(), 1, "the mention nobody answered is still open");
    assert_eq!(owed[0].to, USER_NAME);

    let _ = std::fs::remove_dir_all(&home);
}

/// A background command's transitions are a typed resource update rather than a
/// label-only string (parity ledger; B1 review ruling ①).
#[tokio::test]
async fn a_background_command_reports_its_transitions_as_a_resource() {
    struct Shell(String);
    impl crate::watch::Watchable for Shell {
        fn label(&self) -> String {
            self.0.clone()
        }
        fn poll(&self) -> crate::watch::WatchPoll {
            crate::watch::WatchPoll {
                state: crate::watch::WatchState::Running,
                detail: None,
                payload: None,
                signal: None,
            }
        }
        fn check_interval(&self) -> Option<std::time::Duration> {
            None
        }
    }

    let core = AppCore::start(SessionSetup::default());
    let watch = core.watch();
    let mut link = attached(&core).await;
    let id = watch.register_with_conditions(
        Box::new(Shell("$ cargo test".to_string())),
        Vec::new(),
        None,
    );
    watch.set_state(
        id,
        crate::watch::WatchState::Done,
        Some("ok".to_string()),
        None,
    );

    let events = drain(&mut link).await;
    let reported: Vec<&crate::app::snapshot::BackgroundCommandResource> = events
        .iter()
        .filter_map(|event| match &event.payload {
            AppEventPayload::CommandChanged(changed) => Some(&changed.command),
            _ => None,
        })
        .collect();
    match reported.as_slice() {
        [started, .., done] => {
            assert_eq!(started.label, "$ cargo test");
            assert_eq!(
                started.command, "cargo test",
                "the line it runs, without the prompt marker"
            );
            assert_eq!(
                started.state,
                crate::app::snapshot::BackgroundCommandState::Running
            );
            assert_eq!(
                done.state,
                crate::app::snapshot::BackgroundCommandState::Done
            );
            assert_eq!(done.id, started.id, "one command, one identifier");
        }
        other => panic!("expected a start and an end, got {other:?}"),
    }
}

/// The `@` ledger reaches the conversation summary: a debt the user owes is
/// stated rather than left for a frontend to read out of prose, and speaking is
/// what settles it.
#[tokio::test]
async fn a_mention_the_user_owes_is_an_obligation_until_they_speak() {
    let core = AppCore::start(SessionSetup::default());
    let channels = core.channels();
    let mut link = attached(&core).await;
    channels
        .create(
            "build",
            vec![
                MAIN_NAME.to_string(),
                "scout".to_string(),
                USER_NAME.to_string(),
            ],
            ChannelMode::Free,
        )
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    channels
        .post("scout", "build", "@user which branch?")
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    let events = drain(&mut link).await;
    let room = conversation_of(&events, "#build");

    let owing = read_conversation(&mut link, &room, None)
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    match owing.conversation.obligations.as_slice() {
        [owed] => {
            assert_eq!(owed.kind, crate::app::snapshot::ObligationKind::MentionDebt);
            assert_eq!(owed.from.as_deref(), Some("scout"), "who is waiting");
        }
        other => panic!("expected one open debt, got {other:?}"),
    }

    channels
        .post(USER_NAME, "build", "the release branch")
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    let _ = drain(&mut link).await;
    let settled = read_conversation(&mut link, &room, None)
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(
        settled.conversation.obligations.is_empty(),
        "speaking is the answer"
    );
    assert_eq!(
        settled.conversation.unread, 0,
        "and the user's own words are read by definition"
    );
}
