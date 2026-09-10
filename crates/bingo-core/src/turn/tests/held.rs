//! A context the endpoint holds and measures itself (ADR-0055): what a round
//! reports, what a cut it made becomes, and everything the kernel's own ruler
//! does not do to it.

use super::*;

/// The same config, on an endpoint that holds the conversation itself: the
/// window is the one it named, and the kernel draws no line in it
/// (ADR-0055 §1).
fn held(provider: Arc<ScriptedProvider>, window: u64) -> TurnConfig {
    let mut cfg = config(provider, vec![]);
    let model = cfg.model.as_mut().expect("a model");
    model.capabilities.holds_context = true;
    model.capabilities.context_window = window;
    cfg
}

fn context_of(host: &RecordingHost) -> Vec<ContextUsage> {
    host.events()
        .into_iter()
        .filter_map(|e| match e {
            Event::TurnUsage { context, .. } => Some(context),
            _ => None,
        })
        .collect()
}

/// ADR-0055 §2: for a stateful agent the turn's bill is every API call of the
/// turn summed, which is not a measure of anything a window holds. The
/// reading is, and it wins.
#[tokio::test]
async fn a_reading_is_the_rounds_context_whatever_the_bill_said() {
    let mut events = vec![Ok(ModelEvent::Context {
        used: 412_000,
        window: 1_000_000,
    })];
    events.extend(text("Answered."));
    events.push(Ok(ModelEvent::Finish {
        usage: Usage {
            input_tokens: 2_367_503,
            output_tokens: 900,
            ..Default::default()
        },
        finish_reason: FinishReason::unified(UnifiedFinish::Stop),
    }));
    let provider = ScriptedProvider::new(vec![Script::Events(events)]);
    let cfg = held(provider, 200_000);
    let host = RecordingHost::new();
    let out = run(&cfg, &host, CancellationToken::new()).await;
    assert_eq!(out.status, TurnStatus::Completed);
    assert_eq!(
        context_of(&host),
        [ContextUsage {
            used: 412_000,
            window: 1_000_000,
            trigger: 1_000_000,
        }],
        "the agent's own count, against the agent's own window"
    );
    assert!(
        out.items
            .iter()
            .all(|i| !matches!(i.body, ItemBody::Compaction { .. })),
        "and nothing was cut"
    );
}

/// A cut the endpoint made is a row of the round, journaled like any other
/// item and replacing nothing (ADR-0055 §3).
#[tokio::test]
async fn a_cut_the_endpoint_made_is_journalled_as_a_compaction_row() {
    let mut events = vec![
        Ok(ModelEvent::Compacted {
            before: 967_000,
            after: 120_000,
        }),
        Ok(ModelEvent::Context {
            used: 120_000,
            window: 1_000_000,
        }),
    ];
    events.extend(text("Compacted, and here is the answer."));
    let provider = ScriptedProvider::new(vec![Script::Events(events)]);
    let cfg = held(provider, 1_000_000);
    let host = RecordingHost::new();
    let out = run(&cfg, &host, CancellationToken::new()).await;
    assert_eq!(out.status, TurnStatus::Completed);
    assert_eq!(
        host.kinds(),
        [
            "completed:compaction/completed",
            "started:assistant/running",
            "delta",
            "completed:assistant/completed",
            "usage"
        ],
        "a row, and no Compacted event: the journal lost nothing"
    );
    assert_eq!(
        out.items
            .iter()
            .filter_map(|i| match &i.body {
                ItemBody::Compaction {
                    summary,
                    replaced,
                    before,
                    after,
                    ..
                } => Some((summary.clone(), *replaced, *before, *after)),
                _ => None,
            })
            .collect::<Vec<_>>(),
        [(String::new(), 0, 967_000, 120_000)]
    );
}

/// ADR-0055 §1: the kernel asks for no summary of a conversation it does not
/// hold — where the same numbers on its own ruler would have asked for one.
#[tokio::test]
async fn a_held_turn_asks_for_no_summary_where_the_kernels_own_would() {
    let compacted = |cfg: TurnConfig| async move {
        let compactor =
            ScriptedCompactor::new(vec![ScriptedCompactor::cut("itm_none", 9_000, 100)]);
        let mut cfg = cfg;
        cfg.compactor = CompactorSet::fixed(Some(compactor.clone()));
        let host = RecordingHost::new();
        run(&cfg, &host, CancellationToken::new()).await;
        let calls = compactor.calls.lock().unwrap().len();
        (calls, host.kinds())
    };
    let mut kernels = config(
        ScriptedProvider::new(vec![Script::Events(text("hi"))]),
        vec![],
    );
    kernels
        .model
        .as_mut()
        .expect("a model")
        .capabilities
        .context_window = 100;
    let (kernel_calls, _) = compacted(kernels).await;
    assert!(kernel_calls > 0, "the kernel's own ruler cuts at its line");

    let (held_calls, kinds) = compacted(held(
        ScriptedProvider::new(vec![Script::Events(text("hi"))]),
        100,
    ))
    .await;
    assert_eq!(held_calls, 0, "and a held one has nothing to cut");
    assert!(
        !kinds.iter().any(|k| k.contains("CONTEXT_WARNING")),
        "nor anything to warn about: {kinds:?}"
    );
}

/// The overflow ladder learns a window, cuts the fold and sends it again —
/// none of which is the kernel's to do here, so the error is the turn's
/// answer (ADR-0055 §1).
#[tokio::test]
async fn an_overflow_on_a_held_session_fails_the_turn_without_a_ladder() {
    let provider = ScriptedProvider::new(vec![Script::Fail(ProviderError::ContextOverflow {
        message: "prompt is too long: 160000 tokens > 150000 maximum".into(),
    })]);
    let cfg = held(provider.clone(), 150_000);
    let host = RecordingHost::new();
    let out = run(&cfg, &host, CancellationToken::new()).await;
    assert!(
        matches!(&out.status, TurnStatus::Failed { error } if error.code == ErrorCode::ContextOverflow),
        "{:?}",
        out.status
    );
    assert_eq!(provider.requests().len(), 1, "it was not sent again");
    assert!(
        !host.kinds().iter().any(|k| k.starts_with("retrying")),
        "{:?}",
        host.kinds()
    );
    assert_eq!(
        cfg.model
            .as_ref()
            .expect("a model")
            .learned
            .window("scripted", "m"),
        None,
        "and no lesson was drawn about a window that is not ours"
    );
}
