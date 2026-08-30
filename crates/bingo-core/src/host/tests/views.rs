//! A plugin's live state through a real host (ADR-0013 §2): a signal is on
//! the attachment's stream and folded into the state, and nowhere in the
//! journal a client replays.

use super::*;

#[tokio::test]
async fn a_signal_is_live_and_never_journaled() {
    let store = Arc::new(crate::journal::MemoryStore::new());
    let host = host_on(
        store,
        ScriptedProvider::new(vec![
            Script::Events(text("one")),
            Script::Events(text("two")),
        ]),
    )
    .await;
    let mut a = host
        .open(
            SessionSelector::Create {
                spec: spec("/work"),
            },
            who(),
            OpenOptions::default(),
        )
        .await
        .unwrap();
    one_turn(&mut a, "first").await;
    let id = a.session.clone();

    host.signal(&id, "bingo.demo.ui", "progress", json!({"value": 3}))
        .await
        .expect("a live session takes a signal");
    let seen = a.events.next().await.expect("the stream is open");
    assert!(
        matches!(&seen.event, Event::Signal { plugin, kind, payload }
            if plugin == "bingo.demo.ui" && kind == "progress" && payload == &json!({"value": 3})),
        "the signal is the next live frame: {seen:?}"
    );
    let mut folded = a.snapshot.clone();
    assert_eq!(folded.apply(&seen), Applied::Signal);
    assert_eq!(
        folded.signals["bingo.demo.ui"]["progress"],
        json!({"value": 3})
    );

    // A later durable frame bounds the replay; the signal is not in it.
    let end = one_turn(&mut a, "second").await;
    let mut replay = a.handle.events_since(Seq::ZERO).await.unwrap();
    while let Some(frame) = replay.next().await {
        assert!(
            !matches!(frame.event, Event::Signal { .. }),
            "a signal was journaled at {:?}",
            frame.seq
        );
        if frame.seq >= end {
            break;
        }
    }
}
