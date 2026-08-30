//! Black-box: a room over JSON-RPC (ADR-0011). `/room` opens a session nobody
//! answers under the person's own; a post into it is recorded there at once
//! and reaches the member it names, which opens a turn on it as a peer's.
//! Nothing is fanned back into the room.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use bingo_sdk::{
    Attachment, Driver, Event, Frame, HostApi, Input, IntentId, IntentOutcome, ItemBody, ItemId,
    OpenOptions, Origin, SessionFilter, SessionId, SessionSelector, SessionState, TurnId,
    TurnOrigin,
};
use futures::StreamExt;

mod support;

use support::{LIMIT, Server, ack_for, create, ready, who};

/// The name the room seats, and the agent that holds it.
const MEMBER: &str = "reviewer";

/// One script serves every session in the process: the fake provider hands its
/// responses out in the order they are asked for.
///
/// 1. the root spawns `reviewer` and is told its name at once;
/// 2. and 3. the root's own turn ends, and so does the reviewer's first —
///    which of the two asks first is a race no script can settle, so both say
///    the same thing;
/// 4. the root takes in the report the reviewer's end sent it;
/// 5. the reviewer answers the post the room woke it with.
const ROOM: &str = r#"{"responses":[
    {"steps":[{"toolCall":{"name":"SpawnAgent","input":{"name":"reviewer","prompt":"wait for the room","background":true}}}]},
    {"steps":[{"text":"ready"}]},
    {"steps":[{"text":"ready"}]},
    {"steps":[{"text":"noted"}]},
    {"steps":[{"text":"hi from the room"}]}
]}"#;

#[tokio::test(flavor = "multi_thread")]
async fn a_post_in_a_room_wakes_its_member_and_is_not_fanned_back() {
    let mut server = Server::spawn(ROOM);
    let kernel = ready(&mut server).await;
    let mut tree = kernel
        .open(create(server.cwd()), who(), OpenOptions::with_children())
        .await
        .unwrap();
    let root = tree.session.clone();

    // `#design` seats a name nobody holds yet: a room is names, not sessions.
    let opened = IntentId::mint();
    tree.handle.submit(
        opened.clone(),
        Input::text("/room design reviewer", Origin::surface("test")),
    );
    let IntentOutcome::Applied { result } = ack_on_root(&mut tree, &root, &opened).await else {
        panic!("opening a room is applied");
    };
    assert_eq!(result["message"], "#design: reviewer");

    // The agent that will hold the name, and the root settling once its
    // report has come back.
    tree.handle.submit(
        IntentId::mint(),
        Input::text("spawn one", Origin::surface("test")),
    );
    until_reported(&mut tree, &root).await;

    let children = kernel
        .sessions(SessionFilter {
            parent: Some(root.clone()),
            ..SessionFilter::default()
        })
        .await
        .unwrap();
    let room = children
        .iter()
        .find(|child| child.driver == Driver::Log)
        .expect("the room is a session nobody answers")
        .id
        .clone();
    let member = children
        .iter()
        .find(|child| child.title.as_deref() == Some(MEMBER))
        .expect("the agent that holds the name")
        .id
        .clone();

    // A post: a `Log` session records it at once and opens no turn of its own.
    let mut posted = kernel
        .open(
            SessionSelector::ById { id: room.clone() },
            who(),
            OpenOptions::default(),
        )
        .await
        .unwrap();
    let post = IntentId::mint();
    posted.handle.submit(
        post.clone(),
        Input::text("hello team", Origin::surface("test")),
    );
    let IntentOutcome::Applied { result } = ack_for(&mut posted, &post).await else {
        panic!("a room records a post at once");
    };
    assert!(result["item"].is_string(), "{result}");

    let woken = until_woken(&mut tree, &member).await;
    let (item, origin) = post_in(&woken);
    assert_eq!(origin.surface, "room");
    assert_eq!(
        origin.principal.as_deref(),
        Some("parent"),
        "a post nobody signed came from the session the room hangs under"
    );
    assert_eq!(origin.conversation.as_deref(), Some("#design"));
    let (turn, turn_origin, inputs) = opened_turn(&woken);
    assert_eq!(turn_origin, TurnOrigin::Peer);
    assert_eq!(
        inputs,
        std::slice::from_ref(&item),
        "the turn runs the post"
    );
    assert_eq!(
        item_turn(&woken, &item),
        Some(turn),
        "the post is the turn's own input"
    );

    // The room's journal, read back: the person's post and nothing else. A
    // member that answers answers in its own session, never into the room.
    let after = kernel
        .open(
            SessionSelector::ById { id: room },
            who(),
            OpenOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(after.snapshot.summary.driver, Driver::Log);
    assert_eq!(
        after.snapshot.extensions["bingo.rooms"]["members"],
        serde_json::json!({ "members": ["reviewer"] }),
        "the membership lives in the room's own journal"
    );
    let posts = after
        .snapshot
        .items
        .iter()
        .filter(|item| matches!(item.body, ItemBody::User { .. }))
        .count();
    assert_eq!(posts, 1, "nothing was fanned back into the room");

    kernel.shutdown().await.unwrap();
}

/// The root's own ack for `intent`. A tree attachment carries every live
/// descendant's frames too (ADR-0010 §3), and they fold into no state here.
async fn ack_on_root(tree: &mut Attachment, root: &SessionId, intent: &IntentId) -> IntentOutcome {
    let deadline = tokio::time::sleep(LIMIT);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            frame = tree.events.next() => {
                let frame = frame.expect("the stream stays open");
                if &frame.session != root {
                    continue;
                }
                tree.snapshot.apply(&frame);
                if let Event::IntentAck { intent: i, outcome } = frame.event
                    && &i == intent
                {
                    return outcome;
                }
            }
            _ = &mut deadline => panic!("no ack for {intent} on the root"),
        }
    }
}

/// The root, settled once the agent's report has landed in it. Which door the
/// report came through is a race — a `Peer` turn on an idle root, or a steer
/// the turn already running absorbed at its barrier — and either way four
/// scripted responses have been handed out when this returns.
async fn until_reported(tree: &mut Attachment, root: &SessionId) {
    let deadline = tokio::time::sleep(LIMIT);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            frame = tree.events.next() => {
                let frame = frame.expect("the stream stays open");
                if &frame.session != root {
                    continue;
                }
                let idle = matches!(frame.event, Event::TurnCompleted { .. });
                tree.snapshot.apply(&frame);
                if idle && reported(&tree.snapshot) {
                    return;
                }
            }
            _ = &mut deadline => panic!("the agent never reported to the root"),
        }
    }
}

/// Whether the agent has written into this session.
fn reported(state: &SessionState) -> bool {
    state.items.iter().any(|item| {
        matches!(&item.body, ItemBody::User { origin, .. }
            if origin.principal.as_deref() == Some(MEMBER))
    })
}

/// Every frame the tree carries until `member` opens a turn.
async fn until_woken(tree: &mut Attachment, member: &SessionId) -> Vec<Frame> {
    let mut seen = Vec::new();
    let deadline = tokio::time::sleep(LIMIT);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            frame = tree.events.next() => {
                let frame = frame.expect("the stream stays open");
                let woken = &frame.session == member
                    && matches!(frame.event, Event::TurnStarted { .. });
                if &frame.session == member {
                    seen.push(frame);
                }
                if woken {
                    return seen;
                }
            }
            _ = &mut deadline => panic!("the room never reached {member}: {} of its frames", seen.len()),
        }
    }
}

/// The post as the member received it: the item, and who it says wrote it.
fn post_in(frames: &[Frame]) -> (ItemId, Origin) {
    frames
        .iter()
        .find_map(|frame| match &frame.event {
            Event::ItemCompleted { item } => match &item.body {
                ItemBody::User { origin, .. } if origin.conversation.is_some() => {
                    Some((item.id.clone(), origin.clone()))
                }
                _ => None,
            },
            _ => None,
        })
        .expect("a room's post is a user item in the member's journal")
}

/// The turn the post opened.
fn opened_turn(frames: &[Frame]) -> (TurnId, TurnOrigin, Vec<ItemId>) {
    frames
        .iter()
        .find_map(|frame| match &frame.event {
            Event::TurnStarted {
                turn,
                inputs,
                origin,
            } => Some((turn.clone(), *origin, inputs.clone())),
            _ => None,
        })
        .expect("the member opened a turn")
}

/// Which turn an item belongs to, as the last frame about it says.
fn item_turn(frames: &[Frame], id: &ItemId) -> Option<TurnId> {
    frames.iter().rev().find_map(|frame| match &frame.event {
        Event::ItemStarted { item }
        | Event::ItemUpdated { item }
        | Event::ItemCompleted { item }
            if &item.id == id =>
        {
            item.turn.clone()
        }
        _ => None,
    })
}
