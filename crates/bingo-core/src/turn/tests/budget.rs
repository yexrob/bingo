//! The ruler, the microcompact and the breaker (ADR-0006).

use super::*;

/// A journal of one prompt and `n` completed tool calls of `chars` each, all
/// in the first round.
fn frames_with_results(n: usize, chars: usize) -> Vec<Frame> {
    let mut frames = history("hello");
    for k in 0..n {
        let item = Item {
            id: ItemId::from_raw(format!("itm_t{k}")),
            turn: Some(TurnId::from_raw("trn_1")),
            round: 0,
            status: ItemStatus::Completed,
            started_at: Timestamp::from_second(0).unwrap(),
            completed_at: None,
            intent: None,
            body: ItemBody::ToolCall {
                call_id: format!("c{k}"),
                name: "Echo".into(),
                input: json!({}),
                output: Some(ToolOutput::text("x".repeat(chars))),
                progress: None,
                child_session: None,
                duration_ms: None,
            },
            meta: Default::default(),
        };
        frames.push(Frame {
            seq: Seq(2 + k as u64),
            ts: Timestamp::from_second(0).unwrap(),
            session: SessionId::from_raw("ses_1"),
            cause: None,
            event: Event::ItemCompleted { item },
        });
    }
    frames
}

fn run_on(
    cfg: &TurnConfig,
    host: &RecordingHost,
    history: Vec<Frame>,
) -> impl Future<Output = TurnOutcome> {
    run_turn(
        cfg,
        TurnRun {
            turn: TurnId::from_raw("trn_2"),
            history,
            generation: 0,
            cancel: CancellationToken::new(),
        },
        host,
    )
}

fn elided_results(request: &ModelRequest) -> (usize, usize) {
    let results: Vec<&ContentPart> = request
        .messages
        .iter()
        .flat_map(|m| m.parts.iter())
        .filter(|p| matches!(p, ContentPart::ToolResult { .. }))
        .collect();
    let elided = results
        .iter()
        .filter(|p| {
            matches!(p, ContentPart::ToolResult { parts, .. }
                if parts.iter().any(|q| q.as_text().is_some_and(|t| t.starts_with("[tool result elided"))))
        })
        .count();
    (results.len(), elided)
}

#[tokio::test]
async fn past_the_micro_line_stale_results_leave_the_wire_only() {
    let provider = ScriptedProvider::new(vec![Script::Events(text("ok"))]);
    let mut cfg = config(provider.clone(), vec![]);
    cfg.model.capabilities.context_window = 10_000;
    cfg.model.max_tokens = 1_000; // effective 9 000, micro 4 500
    let host = RecordingHost::new();
    let out = run_on(&cfg, &host, frames_with_results(12, 2_000)).await;
    assert_eq!(out.status, TurnStatus::Completed);
    assert_eq!(
        elided_results(&provider.requests()[0]),
        (12, 2),
        "the last ten stay"
    );

    let roomy = ScriptedProvider::new(vec![Script::Events(text("ok"))]);
    let cfg = config(roomy.clone(), vec![]);
    let out = run_on(&cfg, &RecordingHost::new(), frames_with_results(12, 2_000)).await;
    assert_eq!(out.status, TurnStatus::Completed);
    assert_eq!(
        elided_results(&roomy.requests()[0]),
        (12, 0),
        "below the line nothing is touched"
    );
}

#[tokio::test]
async fn the_ruler_never_reads_below_what_the_server_counted() {
    let counted = |events: Vec<Result<ModelEvent, ProviderError>>| {
        events
            .into_iter()
            .map(|e| match e {
                Ok(ModelEvent::Finish { finish_reason, .. }) => Ok(ModelEvent::Finish {
                    usage: Usage {
                        input_tokens: 5_000,
                        output_tokens: 3,
                        ..Usage::default()
                    },
                    finish_reason,
                }),
                other => other,
            })
            .collect::<Vec<_>>()
    };
    let provider = ScriptedProvider::new(vec![
        Script::Events(counted(tool_call("Echo", json!({"say": "hi"})))),
        Script::Events(counted(text("done"))),
    ]);
    let cfg = config(provider, vec![Arc::new(EchoTool { read_only: true })]);
    let host = RecordingHost::new();
    let out = run(&cfg, &host, CancellationToken::new()).await;
    assert_eq!(out.status, TurnStatus::Completed);
    let contexts: Vec<u64> = host
        .events()
        .iter()
        .filter_map(|e| match e {
            Event::TurnUsage { context, .. } => Some(context.used),
            _ => None,
        })
        .collect();
    assert_eq!(contexts.len(), 2);
    assert!(contexts[0] >= 5_000, "{contexts:?}");
    assert!(
        contexts[1] > 5_000,
        "the second round is the server's count plus what the tool round added: {contexts:?}"
    );
}

#[tokio::test]
async fn the_person_is_warned_once_near_the_line() {
    let provider = ScriptedProvider::new(vec![
        Script::Events(tool_call("Echo", json!({"say": "hi"}))),
        Script::Events(text("done")),
    ]);
    let mut cfg = config(provider, vec![Arc::new(EchoTool { read_only: true })]);
    cfg.model.capabilities.context_window = 30_000;
    cfg.model.max_tokens = 1_000; // effective 29 000, warn 6 100, trigger 26 100
    let host = RecordingHost::new();
    let out = run_on(&cfg, &host, frames_with_results(16, 2_000)).await;
    assert_eq!(out.status, TurnStatus::Completed);
    let warnings = host
        .kinds()
        .iter()
        .filter(|k| k.as_str() == "notice:CONTEXT_WARNING")
        .count();
    assert_eq!(warnings, 1, "{:?}", host.kinds());
}

#[tokio::test]
async fn a_summary_that_shrinks_nothing_is_discarded_billed_and_counted() {
    let provider = ScriptedProvider::new(vec![Script::Events(text("ok"))]);
    let compactor = ScriptedCompactor::new(vec![ScriptedCompactor::cut("itm_t2", 8_000, 9_000)]);
    let mut cfg = config(provider.clone(), vec![]);
    cfg.compactor = Some(compactor.clone());
    cfg.model.capabilities.context_window = 10_000;
    cfg.model.max_tokens = 1_000; // trigger 8 100
    let host = RecordingHost::new();
    let out = run_on(&cfg, &host, frames_with_results(10, 4_000)).await;
    assert_eq!(out.status, TurnStatus::Completed);
    assert_eq!(cfg.compaction.failures(), 1);
    assert!(
        host.kinds()
            .contains(&"notice:COMPACTION_USELESS".to_string()),
        "{:?}",
        host.kinds()
    );
    assert!(!host.kinds().contains(&"compacted".to_string()));
    assert_eq!(
        out.usage.output_tokens,
        3 + 20,
        "the summary request is billed"
    );
    let (reason, failures, keep) = compactor.calls.lock().unwrap()[0].clone();
    assert_eq!(reason, CompactReason::Threshold);
    assert_eq!(failures, 0);
    assert_eq!(keep, 9_000 / 4);
    assert_eq!(
        elided_results(&provider.requests()[0]).0,
        10,
        "the items are untouched by a discarded cut"
    );
}

#[tokio::test]
async fn three_useless_summaries_trip_the_breaker_and_one_good_one_resets_it() {
    let useless = || ScriptedCompactor::cut("itm_t2", 8_000, 9_000);
    let provider = ScriptedProvider::new(vec![
        Script::Events(text("1")),
        Script::Events(text("2")),
        Script::Events(text("3")),
        Script::Events(text("4")),
        Script::Events(text("5")),
    ]);
    let compactor = ScriptedCompactor::new(vec![
        useless(),
        useless(),
        useless(),
        ScriptedCompactor::cut("itm_t8", 8_000, 2_000),
    ]);
    let mut cfg = config(provider, vec![]);
    cfg.compactor = Some(compactor.clone());
    cfg.model.capabilities.context_window = 10_000;
    cfg.model.max_tokens = 1_000;
    let breaker = cfg.compaction.clone();
    for _ in 0..3 {
        let out = run_on(&cfg, &RecordingHost::new(), frames_with_results(10, 4_000)).await;
        assert_eq!(out.status, TurnStatus::Completed);
    }
    assert!(breaker.tripped());

    let host = RecordingHost::new();
    let out = run_on(&cfg, &host, frames_with_results(10, 4_000)).await;
    assert_eq!(out.status, TurnStatus::Completed);
    assert!(
        host.kinds()
            .contains(&"notice:COMPACTION_SKIPPED".to_string()),
        "{:?}",
        host.kinds()
    );
    assert_eq!(
        compactor.calls.lock().unwrap().len(),
        3,
        "a tripped breaker asks for nothing"
    );

    breaker.succeeded();
    let host = RecordingHost::new();
    let out = run_on(&cfg, &host, frames_with_results(10, 4_000)).await;
    assert_eq!(out.status, TurnStatus::Completed);
    let kinds = host.kinds();
    assert!(kinds.contains(&"compacted".to_string()), "{kinds:?}");
    assert!(
        kinds.contains(&"completed:compaction/completed".to_string()),
        "{kinds:?}"
    );
    assert_eq!(breaker.failures(), 0);
}

#[tokio::test]
async fn an_overflow_passes_the_failures_on_and_retries_once() {
    let provider = ScriptedProvider::new(vec![
        Script::Fail(ProviderError::ContextOverflow {
            message: "too long: 9000 tokens > 8000 maximum".into(),
        }),
        Script::Events(text("recovered")),
    ]);
    let compactor = ScriptedCompactor::new(vec![ScriptedCompactor::cut("itm_t10", 9_000, 3_000)]);
    let mut cfg = config(provider.clone(), vec![]);
    cfg.compactor = Some(compactor.clone());
    cfg.compaction.failed();
    cfg.compaction.failed();
    cfg.compaction.failed();
    let host = RecordingHost::new();
    let out = run_on(&cfg, &host, frames_with_results(12, 2_000)).await;
    assert_eq!(out.status, TurnStatus::Completed, "{:?}", host.kinds());
    let (reason, failures, _) = compactor.calls.lock().unwrap()[0].clone();
    assert!(matches!(reason, CompactReason::Overflow { .. }));
    assert_eq!(
        failures, 3,
        "the strategy is told to take its no-model rung"
    );
    assert_eq!(provider.requests().len(), 2);
    let (results, elided) = elided_results(&provider.requests()[1]);
    assert_eq!(results, 2, "the cut left the last two items");
    assert_eq!(elided, 0);
    assert_eq!(
        cfg.compaction.failures(),
        0,
        "a cut that shrank resets the breaker"
    );
}
