//! Black-box: a room over JSON-RPC (ADR-0011). `/room` opens a session nobody
//! answers under the person's own; a post into it is recorded there at once
//! and wakes the live seat it names, which opens a turn as a peer's and reads
//! the room at the head of it (ADR-0034). The post itself is copied nowhere —
//! not into the member, and nothing is fanned back into the room.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use bingo_sdk::{
    Activation, Answer, Attachment, ContentPart, Driver, Event, Frame, HostApi, Input, IntentId,
    IntentOutcome, Interaction, ItemBody, ItemId, OpenOptions, Origin, SessionFilter, SessionId,
    SessionSelector, SessionState, TurnId, TurnOrigin,
};
use futures::StreamExt;

mod support;

use support::{LIMIT, Server, ack_for, create, ready, who};

/// The name the room seats, and the agent that holds it.
const MEMBER: &str = "reviewer";

/// The word that seats it: a live ear, because the default is patient now and
/// a patient seat reads the room at its next turn rather than at the post
/// (ADR-0034 §6).
const SEAT: &str = "reviewer:0";

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
        Input::text(format!("/room design {SEAT}"), Origin::surface("test")),
    );
    let IntentOutcome::Applied { result } = ack_on_root(&mut tree, &root, &opened).await else {
        panic!("opening a room is applied");
    };
    assert_eq!(result["message"], format!("#design: {SEAT}"));

    // The agent that will hold the name, and the root settling once its
    // report has come back.
    tree.handle.submit(
        IntentId::mint(),
        Input::text("spawn one", Origin::surface("test")),
    );
    until_reported(&mut tree, &root).await;

    let (room, member) = seats(&kernel, &root).await;

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

    // The wake: a nudge from the room and nobody in it, carrying no post
    // (ADR-0034 §3). It points at what is unread; the posts themselves are
    // folded into the turn it opens.
    let woken = until_woken(&mut tree, &member).await;
    let (item, text, origin) = nudge_in(&woken);
    assert_eq!(origin.surface, "room");
    assert_eq!(
        origin.principal, None,
        "a wake is signed by nobody, so it opens no debt and counts as no post"
    );
    assert_eq!(origin.conversation.as_deref(), Some("#design"));
    assert!(
        !text.contains("hello team"),
        "the wake carries no post: {text}"
    );
    let (turn, turn_origin, inputs) = opened_turn(&woken);
    assert_eq!(turn_origin, TurnOrigin::Peer);
    assert_eq!(
        inputs,
        std::slice::from_ref(&item),
        "the turn runs the nudge"
    );
    assert_eq!(
        item_turn(&woken, &item),
        Some(turn),
        "the nudge is the turn's own input"
    );

    // And the turn it opened reads the room (ADR-0034 §4): one piece under the
    // room's label, holding the post the member was never handed a copy of.
    let ran = until_member_completed(&mut tree, &member).await;
    let said = user_text(&woken, &ran);
    assert!(
        said.iter()
            .any(|line| line == "[#design, since you last read]\nparent: hello team"),
        "the member read the room at the head of its turn: {said:?}"
    );
    assert!(
        said.iter().all(|line| line != "hello team"),
        "and the post itself was copied into it nowhere: {said:?}"
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
        serde_json::json!({
            "members": ["reviewer"],
            "listeners": [{"name": "reviewer", "patience_s": 0}],
            "kind": "tree",
            "nodes": [{"label": "reviewer", "badge": "live", "tone": "neutral"}],
        }),
        "the membership lives in the room's own journal, wearing the tree a \
         surface draws it as (ADR-0013 §2) and the ear each seat asked for"
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

/// The same opening, and then the member needs a person: the sixth response
/// is a shell command the default policy asks about.
const ASKING: &str = r#"{"responses":[
    {"steps":[{"toolCall":{"name":"SpawnAgent","input":{"name":"reviewer","prompt":"wait for the room","background":true}}}]},
    {"steps":[{"text":"ready"}]},
    {"steps":[{"text":"ready"}]},
    {"steps":[{"text":"noted"}]},
    {"steps":[{"toolCall":{"name":"Bash","input":{"command":"echo hi"}}}]},
    {"steps":[{"text":"it was not allowed"}]}
]}"#;

/// A room member that needs a person reaches the person (ADR-0010 §3, and
/// M8's rule for every surface): the prompt a woken member raises arrives on
/// the root's tree attachment and is answered through the root's handle. A
/// room adds only the way the member was woken.
#[tokio::test(flavor = "multi_thread")]
async fn a_room_member_that_asks_a_person_is_answered_through_the_root() {
    let mut server = Server::spawn(ASKING);
    let kernel = ready(&mut server).await;
    let mut tree = kernel
        .open(create(server.cwd()), who(), OpenOptions::with_children())
        .await
        .unwrap();
    let root = tree.session.clone();
    let opened = IntentId::mint();
    tree.handle.submit(
        opened.clone(),
        Input::text(format!("/room design {SEAT}"), Origin::surface("test")),
    );
    ack_on_root(&mut tree, &root, &opened).await;
    tree.handle.submit(
        IntentId::mint(),
        Input::text("spawn one", Origin::surface("test")),
    );
    until_reported(&mut tree, &root).await;
    let (room, member) = seats(&kernel, &root).await;

    let mut posted = kernel
        .open(
            SessionSelector::ById { id: room },
            who(),
            OpenOptions::default(),
        )
        .await
        .unwrap();
    let post = IntentId::mint();
    posted.handle.submit(
        post.clone(),
        Input::text("run the tests", Origin::surface("test")),
    );
    ack_for(&mut posted, &post).await;

    let asked = until_asked(&mut tree, &member).await;
    assert_eq!(asked.session, member, "the prompt is the member's own");
    tree.handle.answer(
        IntentId::mint(),
        asked.id.clone(),
        Answer::Deny {
            feedback: Some("not now".into()),
        },
        Activation::Programmatic,
    );
    let frames = until_member_completed(&mut tree, &member).await;
    assert!(
        frames.iter().any(|frame| matches!(
            &frame.event,
            Event::InteractionResolved { id, .. } if id == &asked.id
        )),
        "the answer through the root resolved the member's prompt"
    );
    let refused = frames.iter().any(|frame| match &frame.event {
        Event::ItemCompleted { item } => match &item.body {
            ItemBody::ToolCall { name, output, .. } if name == "Bash" => {
                output.as_ref().is_some_and(|o| o.is_error)
            }
            _ => false,
        },
        _ => false,
    });
    assert!(
        refused,
        "the member's shell command was refused, and its turn went on"
    );

    kernel.shutdown().await.unwrap();
}

/// The room and the agent under `root`, once both are seated.
async fn seats(
    kernel: &bingo_surface_rpc::RemoteKernel,
    root: &SessionId,
) -> (SessionId, SessionId) {
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
    (room, member)
}

/// The interaction `member` opens, as the root's tree carries it.
async fn until_asked(tree: &mut Attachment, member: &SessionId) -> Interaction {
    let deadline = tokio::time::sleep(LIMIT);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            frame = tree.events.next() => {
                let frame = frame.expect("the stream stays open");
                if &frame.session == member
                    && let Event::InteractionOpened { interaction } = frame.event
                {
                    return interaction;
                }
            }
            _ = &mut deadline => panic!("{member} never asked"),
        }
    }
}

/// Every frame of `member` until its turn completes.
async fn until_member_completed(tree: &mut Attachment, member: &SessionId) -> Vec<Frame> {
    let mut seen = Vec::new();
    let deadline = tokio::time::sleep(LIMIT);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            frame = tree.events.next() => {
                let frame = frame.expect("the stream stays open");
                if &frame.session != member {
                    continue;
                }
                let done = matches!(frame.event, Event::TurnCompleted { .. });
                seen.push(frame);
                if done {
                    return seen;
                }
            }
            _ = &mut deadline => panic!("{member}'s turn never completed"),
        }
    }
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

/// The wake as the member received it: the item, what it says, and where it
/// came from.
fn nudge_in(frames: &[Frame]) -> (ItemId, String, Origin) {
    frames
        .iter()
        .find_map(|frame| match &frame.event {
            Event::ItemCompleted { item } => match &item.body {
                ItemBody::User { parts, origin } if origin.conversation.is_some() => Some((
                    item.id.clone(),
                    parts.iter().filter_map(ContentPart::as_text).collect(),
                    origin.clone(),
                )),
                _ => None,
            },
            _ => None,
        })
        .expect("a room's wake is a user item in the member's journal")
}

/// Everything said into the member's own session, in order: the wake, and what
/// its turn read of the room.
fn user_text(woken: &[Frame], ran: &[Frame]) -> Vec<String> {
    woken
        .iter()
        .chain(ran)
        .filter_map(|frame| match &frame.event {
            Event::ItemCompleted { item } => match &item.body {
                ItemBody::User { parts, .. } => {
                    Some(parts.iter().filter_map(ContentPart::as_text).collect())
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
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
