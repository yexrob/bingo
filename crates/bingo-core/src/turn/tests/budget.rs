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
            kind: TurnKind::Respond,
        },
        host,
    )
}

struct ReissuedContext;

#[async_trait]
impl ContextContributor for ReissuedContext {
    fn id(&self) -> &str {
        "reissued"
    }
    fn placement(&self) -> Placement {
        Placement::RoundStart
    }
    async fn contribute(&self, query: ContextQuery<'_>) -> Result<Vec<ContextPiece>, ContextError> {
        let mut pieces: Vec<_> =
            ContextPiece::snapshot(self.id(), "runtime state ".repeat(100), query.items)
                .into_iter()
                .collect();
        pieces.push(ContextPiece::User {
            parts: vec![ContentPart::text("retained tail")],
            label: "tail".into(),
        });
        Ok(pieces)
    }
}

struct AcceptedCuts(std::sync::atomic::AtomicUsize);

#[async_trait]
impl Compactor for AcceptedCuts {
    async fn compact(
        &self,
        cx: CompactContext<'_>,
        _: CompactReason,
    ) -> Result<Compaction, CompactError> {
        let calls = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        assert!(
            calls <= 2,
            "automatic compaction must progress instead of buying a third summary"
        );
        let at = cx.items.len() - 1;
        Ok(Compaction {
            summary: "s".into(),
            boundary: cx.items[at].id.clone(),
            kept: vec![],
            before: bingo_sdk::tokens::items(&cx.items[..at]),
            after: 1,
            usage: Usage {
                output_tokens: 7,
                ..Usage::default()
            },
        })
    }
}

#[tokio::test]
async fn accepted_threshold_cuts_are_bounded_when_system_and_reissued_context_stay_large() {
    let provider = ScriptedProvider::new(vec![Script::Events(text("normal progress"))]);
    let compactor = Arc::new(AcceptedCuts(std::sync::atomic::AtomicUsize::new(0)));
    let mut cfg = config(provider.clone(), vec![]);
    cfg.system = vec![SystemBlock {
        text: "unchanged system ".repeat(4_000),
        cache: true,
    }];
    cfg.model.as_mut().unwrap().capabilities.context_window = 10_000;
    cfg.compactor = CompactorSet::fixed(Some(compactor.clone()));
    cfg.contributors = ContributorSet {
        fixed: vec![Arc::new(ReissuedContext)],
        sources: vec![],
    };
    let host = RecordingHost::new();
    let outcome = run_on(&cfg, &host, frames_with_results(2, 4_000)).await;
    assert_eq!(outcome.status, TurnStatus::Completed);
    assert_eq!(compactor.0.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(
        cfg.compaction.failures(),
        0,
        "accepted cuts reset only the consecutive-failure breaker"
    );
    assert_eq!(provider.requests().len(), 1);
    assert_eq!(outcome.usage.output_tokens, 2 * 7 + 3);
    let events = host.events();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Event::Compacted { .. }))
            .count(),
        2
    );
    assert_eq!(events.iter().filter(|event| matches!(event, Event::ItemCompleted { item }
        if matches!(&item.body, ItemBody::User { origin, parts } if origin.surface == "contributor:reissued"
            && parts.iter().any(|part| part.as_text().is_some_and(|text| text.starts_with("runtime state")))))).count(), 3);
}

struct FailedSummary {
    cancel: Option<CancellationToken>,
}

#[async_trait]
impl Compactor for FailedSummary {
    async fn compact(
        &self,
        _: CompactContext<'_>,
        _: CompactReason,
    ) -> Result<Compaction, CompactError> {
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
        Err(CompactError {
            error: KernelError::new(ErrorCode::InvalidInput, "summary failed after usage"),
            usage: Usage {
                output_tokens: 17,
                ..Usage::default()
            },
        })
    }
}

#[tokio::test]
async fn failed_and_cancelled_summary_usage_is_billed_once_without_cutting() {
    for interrupted in [false, true] {
        let provider = ScriptedProvider::new(vec![]);
        let mut cfg = config(provider.clone(), vec![]);
        let cancel = CancellationToken::new();
        cfg.compactor = CompactorSet::fixed(Some(Arc::new(FailedSummary {
            cancel: interrupted.then(|| cancel.clone()),
        })));
        let frames = frames_with_results(10, 4_000);
        let original = ContextView::items(&frames);
        let host = RecordingHost::new();
        let out = run_turn(
            &cfg,
            TurnRun {
                turn: TurnId::mint(),
                history: frames,
                generation: 0,
                cancel,
                kind: TurnKind::Compact { instructions: None },
            },
            &host,
        )
        .await;
        assert_eq!(out.usage.output_tokens, 17);
        assert_eq!(cfg.compaction.failures(), 1);
        // Nothing was cut; a turn a person stopped leaves its marker and
        // nothing else, as any other stopped turn does.
        let (kept, marker) = out.items.split_at(original.len());
        assert_eq!(kept, original.as_slice());
        assert_eq!(
            marker.len(),
            usize::from(interrupted),
            "only the stopped run records a marker"
        );
        assert!(
            marker
                .iter()
                .all(|item| matches!(&item.body, ItemBody::Interruption { .. }))
        );
        assert!(!host.kinds().contains(&"compacted".to_string()));
        assert!(provider.requests().is_empty());
        assert_eq!(
            matches!(out.status, TurnStatus::Interrupted { .. }),
            interrupted
        );
    }
}

struct PrefixContributor;

#[async_trait]
impl ContextContributor for PrefixContributor {
    fn id(&self) -> &str {
        "prefix-system"
    }
    fn placement(&self) -> Placement {
        Placement::System { order: 10 }
    }
    async fn contribute(&self, _: ContextQuery<'_>) -> Result<Vec<ContextPiece>, ContextError> {
        Ok(vec![ContextPiece::System(SystemBlock {
            text: "contributed baseline".into(),
            cache: true,
        })])
    }
}

#[tokio::test]
async fn manual_threshold_and_overflow_receive_the_assembled_parent_request() {
    for mode in 0..3 {
        let mut scripts = vec![];
        if mode == 2 {
            scripts.push(Script::Fail(ProviderError::ContextOverflow {
                message: "too long".into(),
            }));
        }
        scripts.push(Script::Events(text("normal continuation")));
        let provider = ScriptedProvider::new(scripts);
        let compactor = ScriptedCompactor::new(vec![ScriptedCompactor::cut("itm_t8", 9_000, 1)]);
        let mut cfg = config(
            provider.clone(),
            vec![Arc::new(EchoTool { read_only: true })],
        );
        cfg.compactor = CompactorSet::fixed(Some(compactor.clone()));
        cfg.contributors = ContributorSet {
            fixed: vec![Arc::new(PrefixContributor)],
            sources: vec![],
        };
        cfg.model.as_mut().unwrap().reasoning = Some(Effort::High);
        if mode == 1 {
            cfg.model.as_mut().unwrap().capabilities.context_window = 10_000;
        }
        let frames = frames_with_results(10, 4_000);
        let expected = ContextView::fold(&frames);
        let host = RecordingHost::new();
        let kind = if mode == 0 {
            TurnKind::Compact {
                instructions: Some("keep paths".into()),
            }
        } else {
            TurnKind::Respond
        };
        let outcome = run_turn(
            &cfg,
            TurnRun {
                turn: TurnId::mint(),
                history: frames,
                generation: 0,
                cancel: CancellationToken::new(),
                kind,
            },
            &host,
        )
        .await;
        assert_eq!(outcome.status, TurnStatus::Completed);
        let requests = compactor.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let parent = &requests[0];
        assert_eq!(parent.messages, expected);
        assert_eq!(parent.system.last().unwrap().text, "contributed baseline");
        assert_eq!(parent.tools.len(), 1);
        assert_eq!(parent.reasoning, Some(Effort::High));
        assert_eq!(parent.session, Some(cfg.session.id.clone()));
        let sent = provider.requests();
        if mode == 2 {
            assert_eq!(parent, &sent[0]);
        }
        if mode != 0 {
            let fresh = sent.last().unwrap();
            assert_ne!(fresh.messages, parent.messages);
            assert_eq!(fresh.system, parent.system);
            assert_eq!(fresh.tools, parent.tools);
            assert!(!fresh.provider_options.contains_key("bingo"));
        }
    }
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
async fn normal_long_turns_preserve_old_results_and_the_request_prefix() {
    let provider = ScriptedProvider::new(vec![
        Script::Events(tool_call("Echo", json!({"say": "new result"}))),
        Script::Events(text("ok")),
    ]);
    let mut cfg = config(
        provider.clone(),
        vec![Arc::new(EchoTool { read_only: true })],
    );
    cfg.model
        .as_mut()
        .expect("a model")
        .capabilities
        .context_window = 10_000;
    cfg.model.as_mut().expect("a model").max_tokens = 1_000; // effective 9 000, trigger 8 100
    let host = RecordingHost::new();
    let frames = frames_with_results(12, 2_000);
    let expected = ContextView::fold(&frames);
    let out = run_on(&cfg, &host, frames).await;
    assert_eq!(out.status, TurnStatus::Completed);
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(elided_results(&requests[0]), (12, 0));
    assert_eq!(elided_results(&requests[1]), (13, 0));
    assert_eq!(requests[0].messages, expected);
    assert_eq!(requests[1].system, requests[0].system);
    assert_eq!(
        &requests[1].messages[..requests[0].messages.len()],
        requests[0].messages
    );
    assert!(!host.kinds().contains(&"compacted".to_string()));
}

#[tokio::test]
async fn overflow_recovery_elides_only_the_wire_and_retries_once() {
    let provider = ScriptedProvider::new(vec![
        Script::Fail(ProviderError::ContextOverflow {
            message: "too long".into(),
        }),
        Script::Fail(ProviderError::ContextOverflow {
            message: "still too long".into(),
        }),
        Script::Events(text("must not be reached")),
    ]);
    let cfg = config(provider.clone(), vec![]);
    let host = RecordingHost::new();
    let frames = frames_with_results(12, 2_000);
    let original = ContextView::items(&frames);
    let out = run_on(&cfg, &host, frames).await;
    assert!(matches!(out.status, TurnStatus::Failed { .. }));
    let requests = provider.requests();
    assert_eq!(requests.len(), 2, "overflow recovery is bounded");
    assert_eq!(elided_results(&requests[0]), (12, 0));
    assert_eq!(elided_results(&requests[1]), (12, 8));
    assert_eq!(requests[1].system, requests[0].system);
    assert_eq!(&out.items[..original.len()], original);
    assert!(!host.kinds().contains(&"compacted".to_string()));
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
    cfg.model
        .as_mut()
        .expect("a model")
        .capabilities
        .context_window = 30_000;
    cfg.model.as_mut().expect("a model").max_tokens = 1_000; // effective 29 000, warn 6 100, trigger 26 100
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
    cfg.compactor = CompactorSet::fixed(Some(compactor.clone()));
    cfg.model
        .as_mut()
        .expect("a model")
        .capabilities
        .context_window = 10_000;
    cfg.model.as_mut().expect("a model").max_tokens = 1_000; // trigger 8 100
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
    cfg.compactor = CompactorSet::fixed(Some(compactor.clone()));
    cfg.model
        .as_mut()
        .expect("a model")
        .capabilities
        .context_window = 10_000;
    cfg.model.as_mut().expect("a model").max_tokens = 1_000;
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
    cfg.compactor = CompactorSet::fixed(Some(compactor.clone()));
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
