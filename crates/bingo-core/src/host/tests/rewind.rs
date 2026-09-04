//! `HostApi::rewind` (ADR-0045 §1): the one verb a checkpoint plugin needs,
//! and the two refusals that are the whole of its guard.

use super::*;

/// Open one session on this host, attached from its first frame.
async fn attach(host: &Host) -> Attachment {
    host.open(
        SessionSelector::Create {
            spec: spec("/work"),
        },
        who(),
        OpenOptions::default(),
    )
    .await
    .unwrap()
}

/// One turn, and every frame it wrote.
async fn turn(attachment: &mut Attachment, prompt: &str) -> Vec<Frame> {
    attachment.handle.submit(
        IntentId::mint(),
        Input::text(prompt, Origin::surface("test")),
    );
    let mut seen = Vec::new();
    while let Some(frame) = attachment.events.next().await {
        let done = matches!(frame.event, Event::TurnCompleted { .. });
        seen.push(frame);
        if done {
            return seen;
        }
    }
    panic!("the turn never completed");
}

fn opened_turn(frames: &[Frame]) -> TurnId {
    frames
        .iter()
        .find_map(|frame| match &frame.event {
            Event::TurnStarted { turn, .. } => Some(turn.clone()),
            _ => None,
        })
        .expect("a turn opened")
}

/// Frames up to and including the one a rewind ends with, folded in.
async fn until_rewound(attachment: &mut Attachment, state: &mut SessionState) -> Vec<Frame> {
    let mut seen = Vec::new();
    while let Some(frame) = attachment.events.next().await {
        state.apply(&frame);
        let done = matches!(frame.event, Event::Rewound { .. });
        seen.push(frame);
        if done {
            return seen;
        }
    }
    panic!("the stream ended before the rewind");
}

#[tokio::test]
async fn a_rewind_appends_the_item_and_takes_the_turn_out_of_every_fold() {
    let (host, provider) = host_with(vec![
        Script::Events(text("first answer")),
        Script::Events(text("second answer")),
        Script::Events(text("third answer")),
    ])
    .await;
    let mut attachment = attach(&host).await;
    let session = attachment.session.clone();
    let mut state = attachment.snapshot.clone();

    for frame in turn(&mut attachment, "first ask").await {
        state.apply(&frame);
    }
    let frames = turn(&mut attachment, "second ask").await;
    let second = opened_turn(&frames);
    for frame in &frames {
        state.apply(frame);
    }
    assert_eq!(state.items.len(), 4, "two asks and two answers");

    let dropped = host.rewind(&session, &second).await.expect("a rewind");
    assert_eq!(dropped, 2, "the ask and the answer of the second turn");

    let frames = until_rewound(&mut attachment, &mut state).await;
    let recorded = frames.iter().find_map(|frame| match &frame.event {
        Event::ItemCompleted { item } => Some(item.body.clone()),
        _ => None,
    });
    assert_eq!(
        recorded,
        Some(ItemBody::Rewind {
            to_turn: second.clone(),
            dropped: 2,
        }),
        "the kernel's own item says what it undid"
    );
    assert!(
        frames.iter().any(
            |frame| matches!(&frame.event, Event::Rewound { to_turn, dropped, .. }
                if to_turn == &second && dropped.len() == 2)
        ),
        "and the event names the items"
    );

    let bodies: Vec<&ItemBody> = state.items.iter().map(|item| &item.body).collect();
    assert_eq!(bodies.len(), 3, "the first turn and the rewind: {bodies:?}");
    assert!(
        !bodies
            .iter()
            .any(|body| matches!(body, ItemBody::Assistant { text } if text == "second answer")),
        "the client's fold dropped what the rewind undid: {bodies:?}"
    );
    assert_eq!(state.history_generation, 1);

    turn(&mut attachment, "third ask").await;
    let requests = provider.requests();
    let sent = &requests.last().expect("a third request").messages;
    let said: String = sent
        .iter()
        .flat_map(|m| m.parts.iter().filter_map(|p| p.as_text()))
        .collect::<Vec<_>>()
        .join("|");
    assert!(said.contains("first answer"), "{said}");
    assert!(
        !said.contains("second answer") && !said.contains("second ask"),
        "the model is sent none of the rewound turn: {said}"
    );
}

#[tokio::test]
async fn a_rewind_under_a_running_turn_is_refused() {
    let (host, _) = host_with(vec![
        Script::Events(text("first answer")),
        Script::Hang(Vec::new()),
    ])
    .await;
    let mut attachment = attach(&host).await;
    let session = attachment.session.clone();
    let first = opened_turn(&turn(&mut attachment, "first ask").await);

    attachment.handle.submit(
        IntentId::mint(),
        Input::text("second ask", Origin::surface("test")),
    );
    while let Some(frame) = attachment.events.next().await {
        if matches!(frame.event, Event::TurnStarted { .. }) {
            break;
        }
    }

    let refused = host
        .rewind(&session, &first)
        .await
        .expect_err("a turn is running");
    assert_eq!(refused.code, ErrorCode::NotReady);
    assert_eq!(refused.message, "a turn is running");
}

#[tokio::test]
async fn a_turn_this_session_never_had_is_nothing_to_go_back_to() {
    let (host, _) = host_with(vec![Script::Events(text("first answer"))]).await;
    let mut attachment = attach(&host).await;
    let session = attachment.session.clone();
    turn(&mut attachment, "first ask").await;

    let refused = host
        .rewind(&session, &TurnId::from_raw("trn_elsewhere"))
        .await
        .expect_err("no such turn");
    assert_eq!(refused.code, ErrorCode::InvalidInput);
    assert!(
        refused.message.contains("trn_elsewhere"),
        "{}",
        refused.message
    );
}
