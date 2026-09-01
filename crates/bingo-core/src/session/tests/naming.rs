//! The mint (M32): a session nobody named takes its first ask as its name,
//! once, and loses to every explicit name there is.

use super::*;

fn named(title: Option<&str>) -> Mailbox {
    let head = SessionSummary {
        title: title.map(Into::into),
        ..summary("ses_1")
    };
    let provider = ScriptedProvider::new(vec![
        Script::Events(text("one")),
        Script::Events(text("two")),
    ]);
    spawn(head, None, Services::none(), |_| {
        Arc::new(config(provider, vec![], Arc::new(NoHost)))
    })
}

fn renames(frames: &[Frame]) -> Vec<Option<String>> {
    frames
        .iter()
        .filter_map(|frame| match &frame.event {
            Event::SessionUpdated { summary } => Some(summary.title.clone()),
            _ => None,
        })
        .collect()
}

async fn ask(
    mailbox: &Mailbox,
    events: &mut FrameStream,
    state: &mut SessionState,
    said: &str,
) -> Vec<Frame> {
    mailbox.submit(IntentId::mint(), Input::text(said, Origin::surface("test")));
    frames_until(events, state, turn_completed).await
}

#[tokio::test]
async fn an_unnamed_session_takes_its_first_ask_and_is_not_renamed_again() {
    let mailbox = named(None);
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    assert_eq!(state.summary.title, None, "nothing was asked yet");

    let first = ask(
        &mailbox,
        &mut events,
        &mut state,
        "Fix the parser. It crashes on unicode.",
    )
    .await;
    assert_eq!(
        renames(&first),
        vec![Some("Fix the parser".to_string())],
        "one frame, one name: the first sentence of the first ask"
    );
    assert_eq!(state.summary.title.as_deref(), Some("Fix the parser"));

    let second = ask(&mailbox, &mut events, &mut state, "now the lexer").await;
    assert!(renames(&second).is_empty(), "the mint fires once");
    assert_eq!(state.summary.title.as_deref(), Some("Fix the parser"));
}

#[tokio::test]
async fn a_session_somebody_named_is_never_renamed() {
    let mailbox = named(Some("reviewer"));
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    let frames = ask(&mailbox, &mut events, &mut state, "review the diff").await;
    assert!(renames(&frames).is_empty(), "a seat keeps its name");
    assert_eq!(state.summary.title.as_deref(), Some("reviewer"));
}

/// The journal's own order decides: a name that landed after the first ask is
/// the session's name, and resuming it mints nothing over the top.
#[tokio::test]
async fn a_name_that_landed_after_the_first_ask_wins_on_resume() {
    let ts = jiff::Timestamp::from_second(0).unwrap();
    let frame = |seq: u64, event: Event| Frame {
        seq: Seq(seq),
        ts,
        session: SessionId::from_raw("ses_1"),
        cause: None,
        event,
    };
    let asked = Item {
        id: ItemId::from_raw("itm_1"),
        turn: None,
        round: 0,
        status: ItemStatus::Completed,
        started_at: ts,
        completed_at: Some(ts),
        intent: None,
        body: ItemBody::User {
            parts: vec![ContentPart::text("fix the parser")],
            origin: Origin::surface("test"),
        },
        meta: Default::default(),
    };
    let frames = vec![
        frame(
            1,
            Event::SessionUpdated {
                summary: summary("ses_1"),
            },
        ),
        frame(2, Event::ItemCompleted { item: asked }),
        frame(
            3,
            Event::SessionUpdated {
                summary: SessionSummary {
                    title: Some("reviewer".into()),
                    messages: Some(1),
                    ..summary("ses_1")
                },
            },
        ),
    ];
    let provider = ScriptedProvider::new(vec![Script::Events(text("ok"))]);
    let mailbox = resume(frames, None, Services::none(), |_| {
        Arc::new(config(provider, vec![], Arc::new(NoHost)))
    })
    .unwrap();
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    assert_eq!(
        state.summary.title.as_deref(),
        Some("reviewer"),
        "the head of the new segment carries the explicit name"
    );

    let frames = ask(&mailbox, &mut events, &mut state, "and the lexer").await;
    assert!(renames(&frames).is_empty());
    assert_eq!(state.summary.title.as_deref(), Some("reviewer"));
}

/// An old journal — one written before the mint — earns its name at the head
/// of the segment that reopens it, with no frame of its own.
#[tokio::test]
async fn an_unnamed_journal_is_named_by_the_head_that_reopens_it() {
    let ts = jiff::Timestamp::from_second(0).unwrap();
    let frame = |seq: u64, event: Event| Frame {
        seq: Seq(seq),
        ts,
        session: SessionId::from_raw("ses_1"),
        cause: None,
        event,
    };
    let asked = Item {
        id: ItemId::from_raw("itm_1"),
        turn: None,
        round: 0,
        status: ItemStatus::Completed,
        started_at: ts,
        completed_at: Some(ts),
        intent: None,
        body: ItemBody::User {
            parts: vec![ContentPart::text("请帮我把这个解析器修好，它遇到中文就崩")],
            origin: Origin::surface("test"),
        },
        meta: Default::default(),
    };
    let frames = vec![
        frame(
            1,
            Event::SessionUpdated {
                summary: summary("ses_1"),
            },
        ),
        frame(2, Event::ItemCompleted { item: asked }),
    ];
    let provider = ScriptedProvider::new(vec![Script::Events(text("ok"))]);
    let mailbox = resume(frames, None, Services::none(), |_| {
        Arc::new(config(provider, vec![], Arc::new(NoHost)))
    })
    .unwrap();
    let (state, _events) = mailbox.attach().await.unwrap();
    assert_eq!(
        state.summary.title.as_deref(),
        Some("请帮我把这个解析器修好，它遇到中文就崩")
    );
    assert_eq!(
        state.summary.messages,
        Some(1),
        "the fold counted the ask the journal already held"
    );
}
