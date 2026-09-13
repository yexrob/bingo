//! Source attribution in the requests assembled by real turns.

use super::*;

#[tokio::test]
async fn a_peer_at_the_barrier_preserves_the_previous_request_prefix() {
    let provider = ScriptedProvider::new(vec![
        Script::Events(tool_call("Echo", json!({"v": 1}))),
        Script::Events(text("ok")),
    ]);
    let cfg = config(
        provider.clone(),
        vec![Arc::new(EchoTool { read_only: true })],
    );
    let host = RecordingHost::new();
    let origin = Origin {
        surface: "peer".into(),
        principal: Some("reviewer".into()),
        conversation: Some("#review".into()),
    };
    host.queue
        .lock()
        .unwrap()
        .push((IntentId::mint(), Input::text("ship it", origin.clone())));
    let out = run(&cfg, &host, CancellationToken::new()).await;
    assert_eq!(out.status, TurnStatus::Completed);
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].system, requests[0].system);
    assert_eq!(
        &requests[1].messages[..requests[0].messages.len()],
        requests[0].messages
    );
    assert!(
        requests[1]
            .messages
            .iter()
            .flat_map(|m| &m.parts)
            .any(|p| { p.as_text() == Some("[from reviewer in #review]") })
    );
    assert!(out.items.iter().any(|i| {
        matches!(&i.body, ItemBody::User { origin: recorded, .. } if recorded == &origin)
    }));
}

/// Two inputs the session coalesced into one turn (ADR-0010 §1): what woke it,
/// and the line a person typed straight at it.
fn a_nudge_then_a_line() -> Vec<Frame> {
    let ts = Timestamp::from_second(0).unwrap();
    let said = |seq: u64, id: &str, origin: Origin, text: &str| Frame {
        seq: Seq(seq),
        ts,
        session: SessionId::from_raw("ses_1"),
        cause: None,
        event: Event::ItemCompleted {
            item: Item {
                id: ItemId::from_raw(id),
                turn: Some(TurnId::from_raw("trn_1")),
                round: 0,
                status: ItemStatus::Completed,
                started_at: ts,
                completed_at: None,
                intent: None,
                body: ItemBody::User {
                    parts: vec![ContentPart::text(text)],
                    origin,
                },
                meta: Default::default(),
            },
        },
    };
    vec![
        said(
            1,
            "itm_nudge",
            Origin {
                surface: "peer".into(),
                principal: None,
                conversation: Some("#collab".into()),
            },
            "there is something unread",
        ),
        said(2, "itm_line", Origin::surface("tui"), "Hi"),
    ]
}

/// The field failure the mark is for: one turn carried both a nudge and a
/// direct line, and a model briefed to stand by read the unlabeled line as
/// more of the chatter. Everything in the request now says what it is — and
/// what the kernel itself adds still says nothing, being nobody.
#[tokio::test]
async fn a_turn_that_mixes_tells_the_model_which_line_is_the_persons() {
    let provider = ScriptedProvider::new(vec![Script::Events(text("ok"))]);
    let mut cfg = config(provider.clone(), vec![]);
    cfg.contributors = ContributorSet {
        fixed: vec![fixed_contributor("notes")],
        sources: vec![],
    };
    let host = RecordingHost::new();
    run_turn(
        &cfg,
        TurnRun {
            turn: TurnId::from_raw("trn_1"),
            history: a_nudge_then_a_line(),
            generation: 0,
            cancel: CancellationToken::new(),
            kind: TurnKind::Respond,
        },
        &host,
    )
    .await;
    let requests = provider.requests();
    let read: Vec<&str> = requests[0]
        .messages
        .iter()
        .flat_map(|m| m.parts.iter())
        .filter_map(|part| part.as_text())
        .collect();
    assert_eq!(
        read,
        [
            "[in #collab]",
            "there is something unread",
            "[from the person you work for]",
            "Hi",
            "notes said so",
        ]
    );
}
