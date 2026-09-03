use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;

use super::*;
use crate::gate::DefaultPolicy;
use crate::test_support::*;

struct RecordingHost {
    events: Mutex<Vec<Event>>,
    answers: Mutex<VecDeque<Answer>>,
    queue: Mutex<Vec<(IntentId, Input)>>,
    asked: Mutex<Vec<InteractionKind>>,
}

impl RecordingHost {
    fn new() -> Self {
        Self {
            events: Mutex::new(vec![]),
            answers: Mutex::new(VecDeque::new()),
            queue: Mutex::new(vec![]),
            asked: Mutex::new(vec![]),
        }
    }
    fn asked(&self) -> Vec<InteractionKind> {
        self.asked.lock().unwrap().clone()
    }
    fn events(&self) -> Vec<Event> {
        self.events.lock().unwrap().clone()
    }
    fn kinds(&self) -> Vec<String> {
        self.events()
            .iter()
            .map(|e| match e {
                Event::ItemStarted { item } => format!("started:{}", kind(item)),
                Event::ItemUpdated { item } => format!("updated:{}", kind(item)),
                Event::ItemCompleted { item } => format!("completed:{}", kind(item)),
                Event::ItemDelta { .. } => "delta".into(),
                Event::TurnUsage { .. } => "usage".into(),
                Event::TurnRetrying { .. } => "retrying".into(),
                Event::Compacted { .. } => "compacted".into(),
                Event::Notice { code, .. } => format!("notice:{code}"),
                other => format!("{other:?}")
                    .split_whitespace()
                    .next()
                    .unwrap_or("?")
                    .to_string(),
            })
            .collect()
    }
}

#[async_trait]
impl TurnHost for RecordingHost {
    fn offered(&self, _tools: Vec<Arc<dyn Tool>>) {}

    fn emit(&self, event: Event) {
        self.events.lock().unwrap().push(event);
    }
    async fn ask(
        &self,
        _item: Option<ItemId>,
        kind: InteractionKind,
        _answers: Vec<AnswerSpec>,
    ) -> Result<Answer, KernelError> {
        self.asked.lock().unwrap().push(kind);
        Ok(self
            .answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Answer::Cancel))
    }
    async fn absorb(&self) -> Vec<(IntentId, Input)> {
        std::mem::take(&mut *self.queue.lock().unwrap())
    }
}

fn summary() -> SessionSummary {
    let ts = Timestamp::from_second(0).unwrap();
    SessionSummary {
        tools: None,
        system_extra: None,
        driver: Default::default(),
        id: SessionId::from_raw("ses_1"),
        key: None,
        title: None,
        cwd: "/tmp".into(),
        parent: None,
        model: Some("m".into()),
        provider: Some("scripted".into()),
        created_at: ts,
        updated_at: ts,
        usage: Usage::default(),
        busy: false,
        messages: None,
    }
}

fn config(provider: Arc<ScriptedProvider>, tools: Vec<Arc<dyn Tool>>) -> TurnConfig {
    TurnConfig {
        session: summary(),
        cwd: "/tmp".into(),
        model: Some(ModelChoice {
            provider,
            id: "m".into(),
            capabilities: capabilities(),
            max_tokens: 1000,
            reasoning: None,
            learned: Arc::new(crate::models::Learned::default()),
        }),
        system: vec![SystemBlock {
            text: "You are bingo.".into(),
            cache: false,
        }],
        tools: ToolSet::fixed(tools),
        policy: Arc::new(DefaultPolicy),
        hooks: HookSet::default(),
        contributors: ContributorSet::default(),
        compaction: Arc::new(crate::turn::Breaker::default()),
        compactor: CompactorSet::default(),
        budget: TurnBudget::default(),
        env: Arc::new(Env {
            home: "/tmp".into(),
            config_dir: "/tmp".into(),
            data_dir: "/tmp".into(),
        }),
        host: bingo_sdk::testing::NoHost::handle(),
        tool_host: Arc::new(NoHost),
    }
}

fn history(prompt: &str) -> Vec<Frame> {
    let item = Item {
        id: ItemId::from_raw("itm_user"),
        turn: Some(TurnId::from_raw("trn_1")),
        round: 0,
        status: ItemStatus::Completed,
        started_at: Timestamp::from_second(0).unwrap(),
        completed_at: None,
        intent: None,
        body: ItemBody::User {
            parts: vec![ContentPart::text(prompt)],
            origin: Origin::surface("test"),
        },
        meta: Default::default(),
    };
    vec![Frame {
        seq: Seq(1),
        ts: Timestamp::from_second(0).unwrap(),
        session: SessionId::from_raw("ses_1"),
        cause: None,
        event: Event::ItemCompleted { item },
    }]
}

fn run(
    cfg: &TurnConfig,
    host: &RecordingHost,
    cancel: CancellationToken,
) -> impl Future<Output = TurnOutcome> {
    run_turn(
        cfg,
        TurnRun {
            turn: TurnId::from_raw("trn_1"),
            history: history("hello"),
            generation: 0,
            cancel,
            kind: TurnKind::Respond,
        },
        host,
    )
}

#[tokio::test]
async fn a_text_only_turn_streams_one_assistant_item_and_completes() {
    let provider = ScriptedProvider::new(vec![Script::Events(text("Hi there"))]);
    let cfg = config(provider.clone(), vec![]);
    let host = RecordingHost::new();
    let out = run(&cfg, &host, CancellationToken::new()).await;
    assert_eq!(out.status, TurnStatus::Completed);
    assert_eq!(out.usage.input_tokens, 10);
    assert_eq!(
        host.kinds(),
        [
            "started:assistant/running",
            "delta",
            "completed:assistant/completed",
            "usage"
        ]
    );
    let req = provider.requests();
    assert_eq!(req.len(), 1);
    assert_eq!(req[0].messages[0].parts[0].as_text(), Some("hello"));
    assert_eq!(req[0].system[0].text, "You are bingo.");
}

/// ADR-0035 §3: a provider that keeps a conversation of its own per session —
/// an ACP adapter holds one agent session per bingo session — has no other way
/// to learn whose turn it is answering. The kernel already knows, so it says.
#[tokio::test]
async fn the_request_names_the_session_the_turn_runs_for() {
    let provider = ScriptedProvider::new(vec![Script::Events(text("Hi there"))]);
    let cfg = config(provider.clone(), vec![]);
    let host = RecordingHost::new();
    run(&cfg, &host, CancellationToken::new()).await;
    assert_eq!(
        provider.requests()[0].session.as_ref(),
        Some(&cfg.session.id)
    );
}

#[tokio::test]
async fn a_tool_round_gates_executes_and_feeds_the_result_back() {
    let provider = ScriptedProvider::new(vec![
        Script::Events(tool_call("Echo", json!({"v": 1}))),
        Script::Events(text("done")),
    ]);
    let cfg = config(
        provider.clone(),
        vec![Arc::new(EchoTool { read_only: true })],
    );
    let host = RecordingHost::new();
    let out = run(&cfg, &host, CancellationToken::new()).await;
    assert_eq!(out.status, TurnStatus::Completed);
    assert_eq!(
        host.kinds(),
        [
            "started:tool/pending",
            "usage",
            "updated:tool/running",
            "completed:tool/completed",
            "started:assistant/running",
            "delta",
            "completed:assistant/completed",
            "usage"
        ]
    );
    let req = provider.requests();
    assert_eq!(req.len(), 2);
    let second = &req[1].messages;
    assert!(matches!(&second[1].parts[0], ContentPart::ToolUse { name, .. } if name == "Echo"));
    assert!(
        matches!(&second[2].parts[0], ContentPart::ToolResult { tool_use_id, parts, .. } if tool_use_id == "c1" && parts[0].as_text() == Some("echo:1"))
    );
}

#[tokio::test]
async fn an_interrupt_mid_stream_keeps_the_text_and_records_the_marker() {
    let provider = ScriptedProvider::new(vec![Script::Hang(vec![
        ModelEvent::TextStart { id: "b".into() },
        ModelEvent::TextDelta {
            id: "b".into(),
            delta: "partial".into(),
        },
    ])]);
    let cfg = config(provider, vec![]);
    let host = RecordingHost::new();
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        c2.cancel();
    });
    let out = run(&cfg, &host, cancel).await;
    assert_eq!(
        out.status,
        TurnStatus::Interrupted {
            reason: InterruptReason::UserCancel
        }
    );
    assert_eq!(
        host.kinds(),
        [
            "started:assistant/running",
            "delta",
            "completed:assistant/interrupted",
            "completed:interruption/completed"
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn a_retryable_error_withdraws_the_attempt_and_tries_again() {
    let provider = ScriptedProvider::new(vec![
        Script::Events(vec![
            Ok(ModelEvent::TextStart { id: "b".into() }),
            Ok(ModelEvent::TextDelta {
                id: "b".into(),
                delta: "lost".into(),
            }),
            Err(ProviderError::Server {
                status: 503,
                message: "overloaded".into(),
            }),
        ]),
        Script::Events(text("second try")),
    ]);
    let cfg = config(provider.clone(), vec![]);
    let host = RecordingHost::new();
    let out = run(&cfg, &host, CancellationToken::new()).await;
    assert_eq!(out.status, TurnStatus::Completed);
    let kinds = host.kinds();
    assert!(kinds.contains(&"retrying".to_string()));
    let retry = host.events().into_iter().find_map(|e| match e {
        Event::TurnRetrying {
            dropped, attempt, ..
        } => Some((dropped, attempt)),
        _ => None,
    });
    let (dropped, attempt) = retry.unwrap();
    assert_eq!(dropped.len(), 1, "the lost item is withdrawn by id");
    assert_eq!(attempt, 1);
    assert_eq!(provider.requests().len(), 2);
    assert_eq!(
        provider.requests()[1].messages.len(),
        1,
        "the withdrawn text is not in the retried context"
    );
}

#[tokio::test]
async fn a_non_retryable_error_fails_the_turn_with_its_code() {
    let provider = ScriptedProvider::new(vec![Script::Fail(ProviderError::Auth {
        message: "bad key".into(),
    })]);
    let cfg = config(provider, vec![]);
    let host = RecordingHost::new();
    let out = run(&cfg, &host, CancellationToken::new()).await;
    assert!(
        matches!(out.status, TurnStatus::Failed { error } if error.code == ErrorCode::AuthRequired)
    );
}

#[tokio::test]
async fn an_empty_response_is_retried_once_then_accepted() {
    let empty = || {
        Script::Events(vec![Ok(ModelEvent::Finish {
            usage: Usage::default(),
            finish_reason: FinishReason::unified(UnifiedFinish::Stop),
        })])
    };
    let provider = ScriptedProvider::new(vec![empty(), empty()]);
    let cfg = config(provider.clone(), vec![]);
    let host = RecordingHost::new();
    let out = run(&cfg, &host, CancellationToken::new()).await;
    assert_eq!(out.status, TurnStatus::Completed);
    assert_eq!(provider.requests().len(), 2);
}

#[tokio::test]
async fn the_round_budget_stops_a_runaway_loop() {
    let provider = ScriptedProvider::new(
        (0..5)
            .map(|_| Script::Events(tool_call("Echo", json!({"v": 1}))))
            .collect(),
    );
    let mut cfg = config(provider, vec![Arc::new(EchoTool { read_only: true })]);
    cfg.budget = TurnBudget {
        max_rounds: 2,
        max_retries: 0,
    };
    let host = RecordingHost::new();
    let out = run(&cfg, &host, CancellationToken::new()).await;
    assert!(
        matches!(out.status, TurnStatus::Failed { error } if error.code == ErrorCode::TurnBudgetExhausted)
    );
}

#[tokio::test]
async fn a_denied_permission_fails_the_call_and_records_a_receipt() {
    let provider = ScriptedProvider::new(vec![
        Script::Events(tool_call("Echo", json!({"v": 1}))),
        Script::Events(text("ok")),
    ]);
    let cfg = config(
        provider.clone(),
        vec![Arc::new(EchoTool { read_only: false })],
    );
    let host = RecordingHost::new();
    host.answers.lock().unwrap().push_back(Answer::Deny {
        feedback: Some("not now".into()),
    });
    let out = run(&cfg, &host, CancellationToken::new()).await;
    assert_eq!(out.status, TurnStatus::Completed);
    let kinds = host.kinds();
    assert!(kinds.contains(&"completed:receipt/completed".to_string()));
    assert!(kinds.contains(&"completed:tool/failed".to_string()));
    let second = &provider.requests()[1].messages;
    let ContentPart::ToolResult {
        is_error, parts, ..
    } = &second[2].parts[0]
    else {
        panic!()
    };
    assert!(is_error);
    assert!(parts[0].as_text().unwrap().contains("not now"));
}

#[tokio::test]
async fn queued_input_is_absorbed_at_the_barrier() {
    let provider = ScriptedProvider::new(vec![
        Script::Events(tool_call("Echo", json!({"v": 1}))),
        Script::Events(text("ok")),
    ]);
    let cfg = config(
        provider.clone(),
        vec![Arc::new(EchoTool { read_only: true })],
    );
    let host = RecordingHost::new();
    host.queue.lock().unwrap().push((
        IntentId::mint(),
        Input::text("also do this", Origin::surface("tui")),
    ));
    run(&cfg, &host, CancellationToken::new()).await;
    let second = &provider.requests()[1].messages;
    let user = &second[2];
    assert!(
        matches!(&user.parts[0], ContentPart::ToolResult { .. }),
        "results first"
    );
    assert_eq!(user.parts[1].as_text(), Some("also do this"));
}

/// Where an absorbed input lands in the journal: after the tool item of the
/// round it was queued during, and before the next round's. A tool that reads
/// its own session therefore sees everything absorbed at earlier barriers and
/// nothing absorbed at this one — which is what makes the calling item a
/// usable cut (ADR-0025 §2).
#[tokio::test]
async fn an_absorbed_input_lands_between_one_round_s_tool_item_and_the_next() {
    let provider = ScriptedProvider::new(vec![
        Script::Events(tool_call("Echo", json!({"v": 1}))),
        Script::Events(tool_call("Echo", json!({"v": 2}))),
        Script::Events(text("ok")),
    ]);
    let cfg = config(
        provider.clone(),
        vec![Arc::new(EchoTool { read_only: true })],
    );
    let host = RecordingHost::new();
    host.queue.lock().unwrap().push((
        IntentId::mint(),
        Input::text("meanwhile, from elsewhere", Origin::surface("peer")),
    ));
    run(&cfg, &host, CancellationToken::new()).await;

    let kinds = host.kinds();
    let at = |what: &str| -> Vec<usize> {
        kinds
            .iter()
            .enumerate()
            .filter(|(_, k)| k.as_str() == what)
            .map(|(i, _)| i)
            .collect()
    };
    let calls = at("started:tool/pending");
    let absorbed = at("completed:user/completed");
    assert_eq!(calls.len(), 2, "{kinds:?}");
    assert_eq!(absorbed.len(), 1, "{kinds:?}");
    assert!(calls[0] < absorbed[0], "{kinds:?}");
    assert!(absorbed[0] < calls[1], "{kinds:?}");
}

#[tokio::test]
async fn a_length_stop_injects_a_continue_prompt_up_to_three_times() {
    let cut = || {
        Script::Events(vec![
            Ok(ModelEvent::TextStart { id: "b".into() }),
            Ok(ModelEvent::TextDelta {
                id: "b".into(),
                delta: "long".into(),
            }),
            Ok(ModelEvent::TextEnd { id: "b".into() }),
            Ok(ModelEvent::Finish {
                usage: Usage::default(),
                finish_reason: FinishReason::unified(UnifiedFinish::Length),
            }),
        ])
    };
    let provider = ScriptedProvider::new(vec![cut(), cut(), cut(), cut(), cut()]);
    let cfg = config(provider.clone(), vec![]);
    let host = RecordingHost::new();
    let out = run(&cfg, &host, CancellationToken::new()).await;
    assert_eq!(out.status, TurnStatus::Completed);
    assert_eq!(
        provider.requests().len(),
        4,
        "three recoveries, then accept"
    );
    assert_eq!(
        provider.requests()[1].messages.last().unwrap().parts[0].as_text(),
        Some(CONTINUE_PROMPT)
    );
}

#[test]
fn backoff_doubles_from_half_a_second_and_honours_the_server() {
    assert_eq!(backoff(1, None), Duration::from_millis(500));
    assert_eq!(backoff(4, None), Duration::from_millis(4000));
    assert_eq!(backoff(20, None), MAX_RETRY_DELAY);
    assert_eq!(backoff(1, Some(90_000)), MAX_SERVER_RETRY_DELAY);
}

#[tokio::test]
async fn an_overflow_teaches_the_window_the_server_named() {
    let overflow = || {
        Script::Fail(ProviderError::ContextOverflow {
            message: "prompt is too long: 160000 tokens > 150000 maximum".into(),
        })
    };
    let provider = ScriptedProvider::new(vec![overflow(), overflow()]);
    let cfg = config(provider.clone(), vec![]);
    let host = RecordingHost::new();
    let out = run(&cfg, &host, CancellationToken::new()).await;
    assert!(
        matches!(&out.status, TurnStatus::Failed { error } if error.code == ErrorCode::ContextOverflow),
        "{:?}",
        out.status
    );
    assert_eq!(
        provider.requests().len(),
        2,
        "one retry with the forced microcompact, then the turn fails"
    );
    assert!(
        host.kinds().iter().any(|k| k.starts_with("retrying")),
        "{:?}",
        host.kinds()
    );
    assert_eq!(
        cfg.model
            .as_ref()
            .expect("a model")
            .learned
            .window("scripted", "m"),
        Some(150_000)
    );
    assert!(
        host.events().iter().any(|e| matches!(
            e,
            Event::Notice { code, text, .. } if code == "WINDOW_LEARNED" && text.contains("150000")
        )),
        "{:?}",
        host.kinds()
    );
}

#[tokio::test]
async fn a_model_without_vision_gets_a_note_where_the_image_was() {
    let provider = ScriptedProvider::new(vec![Script::Events(text("seen"))]);
    let cfg = config(provider.clone(), vec![]);
    assert!(
        !cfg.model.as_ref().expect("a model").capabilities.images,
        "the scripted model is blind"
    );
    let host = RecordingHost::new();
    let mut frames = history("look at this");
    if let Event::ItemCompleted { item } = &mut frames[0].event
        && let ItemBody::User { parts, .. } = &mut item.body
    {
        parts.push(ContentPart::Image(Image {
            media_type: "image/png".into(),
            data: "iVBORw0KGgo=".into(),
        }));
    }
    let out = run_turn(
        &cfg,
        TurnRun {
            turn: TurnId::from_raw("trn_1"),
            history: frames,
            generation: 0,
            cancel: CancellationToken::new(),
            kind: TurnKind::Respond,
        },
        &host,
    )
    .await;
    assert_eq!(out.status, TurnStatus::Completed);
    let sent = &provider.requests()[0].messages[0].parts;
    assert_eq!(
        sent,
        &vec![
            ContentPart::text("look at this"),
            ContentPart::text("[image omitted: m has no vision]"),
        ]
    );
}

mod budget;

#[tokio::test]
async fn a_stream_that_ends_without_a_finish_is_retried_like_a_dropped_connection() {
    let cut_off = vec![
        Ok(ModelEvent::TextStart { id: "b".into() }),
        Ok(ModelEvent::TextDelta {
            id: "b".into(),
            delta: "half".into(),
        }),
    ];
    let provider =
        ScriptedProvider::new(vec![Script::Events(cut_off), Script::Events(text("whole"))]);
    let cfg = config(provider.clone(), vec![]);
    let host = RecordingHost::new();
    let out = run(&cfg, &host, CancellationToken::new()).await;
    assert_eq!(out.status, TurnStatus::Completed, "{:?}", host.kinds());
    assert_eq!(provider.requests().len(), 2);
    assert!(
        host.kinds().iter().any(|k| k == "retrying"),
        "{:?}",
        host.kinds()
    );
    let events = host.events();
    let answers: Vec<(ItemId, String)> = events
        .iter()
        .filter_map(|e| match e {
            Event::ItemCompleted { item } => match &item.body {
                ItemBody::Assistant { text } => Some((item.id.clone(), text.clone())),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(answers.len(), 2, "{answers:?}");
    assert_eq!(answers[1].1, "whole");
    let withdrawn = events.iter().any(
        |e| matches!(e, Event::TurnRetrying { dropped, .. } if dropped == &[answers[0].0.clone()]),
    );
    assert!(
        withdrawn,
        "the half answer is withdrawn by the retry: {:?}",
        host.kinds()
    );
}

// ----- sources and hook points (ADR-0009) -----

#[tokio::test]
async fn a_source_tool_is_gathered_when_the_turn_starts_and_a_duplicate_is_dropped() {
    let provider =
        ScriptedProvider::new(vec![Script::Events(text("a")), Script::Events(text("b"))]);
    let source = ScriptedToolSource::new();
    let mut cfg = config(provider.clone(), vec![]);
    cfg.tools = ToolSet {
        fixed: vec![Arc::new(EchoTool { read_only: true })],
        sources: vec![source.clone()],
        only: None,
    };
    let host = RecordingHost::new();
    run(&cfg, &host, CancellationToken::new()).await;
    let names = |i: usize| -> Vec<String> {
        provider.requests()[i]
            .tools
            .iter()
            .map(|t| t.name.clone())
            .collect()
    };
    assert_eq!(names(0), vec!["Echo"], "the source had nothing yet");

    source.set(vec![
        Arc::new(EchoTool { read_only: true }),
        Arc::new(PanicTool),
    ]);
    run(&cfg, &host, CancellationToken::new()).await;
    assert_eq!(
        names(1),
        vec!["Echo", "Panic"],
        "the source's tools joined; the duplicate did not"
    );
    assert!(
        host.kinds().contains(&"notice:TOOL_SHADOWED".to_string()),
        "{:?}",
        host.kinds()
    );
}

/// Where every user item this turn recorded came from.
fn origins(host: &RecordingHost) -> Vec<String> {
    host.events()
        .into_iter()
        .filter_map(|e| match e {
            Event::ItemCompleted { item } => match item.body {
                ItemBody::User { origin, .. } => Some(origin.surface),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// The same point as the tool sources', for the same reason: a contributor a
/// process only names after its handshake still speaks in the first turn after
/// it, and its piece carries the origin its id earns it.
#[tokio::test]
async fn a_source_contributor_speaks_when_the_turn_starts_with_its_own_origin() {
    let provider =
        ScriptedProvider::new(vec![Script::Events(text("a")), Script::Events(text("b"))]);
    let source = ScriptedContextSource::new("bridge");
    let mut cfg = config(provider, vec![]);
    cfg.contributors = ContributorSet {
        fixed: vec![],
        sources: vec![source.clone()],
    };
    let host = RecordingHost::new();
    run(&cfg, &host, CancellationToken::new()).await;
    assert!(origins(&host).is_empty(), "the source had nothing yet");

    source.set(vec![fixed_contributor("notes")]);
    let host = RecordingHost::new();
    run(&cfg, &host, CancellationToken::new()).await;
    assert_eq!(origins(&host), ["contributor:notes"]);
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

#[tokio::test]
async fn a_source_strategy_compacts_when_nothing_in_process_holds_the_slot() {
    let provider = ScriptedProvider::new(vec![]);
    let compactor = ScriptedCompactor::new(vec![ScriptedCompactor::cut("itm_none", 9_000, 100)]);
    let mut cfg = config(provider, vec![]);
    cfg.compactor = CompactorSet {
        fixed: None,
        sources: vec![ScriptedCompactorSource::new(vec![compactor.clone()])],
    };
    let host = RecordingHost::new();
    run_turn(
        &cfg,
        TurnRun {
            turn: TurnId::from_raw("trn_1"),
            history: history("hello"),
            generation: 0,
            cancel: CancellationToken::new(),
            kind: TurnKind::Compact { instructions: None },
        },
        &host,
    )
    .await;
    assert_eq!(compactor.calls.lock().unwrap().len(), 1);
    assert!(host.kinds().contains(&"compacted".to_string()));
}

#[tokio::test]
async fn a_hook_that_asks_opens_a_permission_with_its_reason_and_allow_runs_the_tool() {
    let provider = ScriptedProvider::new(vec![
        Script::Events(tool_call("Echo", json!({ "v": 1 }))),
        Script::Events(text("done")),
    ]);
    let mut cfg = config(provider, vec![Arc::new(EchoTool { read_only: true })]);
    cfg.hooks = HookSet::fixed(vec![Arc::new(AskingHook {
        reason: "why".into(),
    })]);
    let host = RecordingHost::new();
    host.answers.lock().unwrap().push_back(Answer::AllowOnce);
    let outcome = run(&cfg, &host, CancellationToken::new()).await;
    assert_eq!(outcome.status, TurnStatus::Completed);
    assert!(
        matches!(&host.asked()[0], InteractionKind::Permission { summary, .. } if summary == "why"),
        "{:?}",
        host.asked()
    );
    assert!(
        host.kinds()
            .contains(&"completed:tool/completed".to_string()),
        "{:?}",
        host.kinds()
    );
}

/// A hook that decides nothing and only writes down that it was asked.
struct OrderingHook {
    id: String,
    seen: Arc<Mutex<Vec<String>>>,
}

impl OrderingHook {
    fn new(id: &str, seen: &Arc<Mutex<Vec<String>>>) -> Arc<Self> {
        Arc::new(Self {
            id: id.to_string(),
            seen: Arc::clone(seen),
        })
    }
}

#[async_trait]
impl Hook for OrderingHook {
    fn id(&self) -> &str {
        &self.id
    }
    fn matcher(&self) -> HookMatcher {
        HookMatcher {
            points: vec![HookPoint::BeforeTool],
            tool: None,
        }
    }
    async fn before_tool(&self, _: &mut ToolCall, _: &HookContext) -> HookOutcome {
        self.seen.lock().unwrap().push(self.id.clone());
        HookOutcome::Continue
    }
}

/// Run one turn whose round calls a tool, so this set's `before_tool` hooks
/// are asked; what they wrote down is the test's answer.
async fn gate_one_call(hooks: HookSet) {
    let provider = ScriptedProvider::new(vec![
        Script::Events(tool_call("Echo", json!({ "v": 1 }))),
        Script::Events(text("done")),
    ]);
    let mut cfg = config(provider, vec![Arc::new(EchoTool { read_only: true })]);
    cfg.hooks = hooks;
    run(&cfg, &RecordingHost::new(), CancellationToken::new()).await;
}

/// R-order, pinned rather than assumed: a hook that arrived from a source is
/// asked exactly where a second registered hook would have been asked. The two
/// compositions are run and the two orders compared, so nothing here rests on
/// reading `gather`.
#[tokio::test]
async fn a_source_s_hook_composes_where_a_second_registered_hook_would() {
    let in_process = Arc::new(Mutex::new(Vec::new()));
    gate_one_call(HookSet::fixed(vec![
        OrderingHook::new("first", &in_process),
        OrderingHook::new("second", &in_process),
    ]))
    .await;

    let mixed = Arc::new(Mutex::new(Vec::new()));
    gate_one_call(HookSet {
        fixed: vec![OrderingHook::new("first", &mixed)],
        sources: vec![ScriptedHookSource::new(vec![OrderingHook::new(
            "second", &mixed,
        )])],
    })
    .await;

    let (in_process, mixed) = (
        in_process.lock().unwrap().clone(),
        mixed.lock().unwrap().clone(),
    );
    assert_eq!(in_process, ["first", "second"]);
    assert_eq!(
        mixed, in_process,
        "a late hook takes a registered one's place"
    );
}

#[tokio::test]
async fn compaction_hooks_bracket_the_cut() {
    let provider = ScriptedProvider::new(vec![]);
    let mut cfg = config(provider, vec![]);
    cfg.compactor =
        CompactorSet::fixed(Some(ScriptedCompactor::new(vec![ScriptedCompactor::cut(
            "itm_none", 9_000, 100,
        )])));
    let hook = RecordingHook::new(false);
    cfg.hooks = HookSet::fixed(vec![hook.clone()]);
    let host = RecordingHost::new();
    run_turn(
        &cfg,
        TurnRun {
            turn: TurnId::from_raw("trn_1"),
            history: history("hello"),
            generation: 0,
            cancel: CancellationToken::new(),
            kind: TurnKind::Compact { instructions: None },
        },
        &host,
    )
    .await;
    assert_eq!(hook.calls(), vec!["compact:Start", "compact:End"]);
    assert!(host.kinds().contains(&"compacted".to_string()));
}
