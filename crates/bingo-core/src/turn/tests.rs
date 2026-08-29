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
}

impl RecordingHost {
    fn new() -> Self {
        Self {
            events: Mutex::new(vec![]),
            answers: Mutex::new(VecDeque::new()),
            queue: Mutex::new(vec![]),
        }
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
    fn emit(&self, event: Event) {
        self.events.lock().unwrap().push(event);
    }
    async fn ask(
        &self,
        _item: Option<ItemId>,
        _kind: InteractionKind,
        _answers: Vec<AnswerSpec>,
    ) -> Result<Answer, KernelError> {
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
    }
}

fn config(provider: Arc<ScriptedProvider>, tools: Vec<Arc<dyn Tool>>) -> TurnConfig {
    TurnConfig {
        session: summary(),
        cwd: "/tmp".into(),
        model: ModelChoice {
            provider,
            id: "m".into(),
            capabilities: capabilities(),
            max_tokens: 1000,
            reasoning: None,
            learned: Arc::new(crate::models::Learned::default()),
        },
        system: vec![SystemBlock {
            text: "You are bingo.".into(),
            cache: false,
        }],
        tools,
        policy: Arc::new(DefaultPolicy),
        hooks: vec![],
        contributors: vec![],
        compaction: Arc::new(crate::turn::Breaker::default()),
        compactor: None,
        budget: TurnBudget::default(),
        env: Arc::new(Env {
            home: "/tmp".into(),
            config_dir: "/tmp".into(),
            data_dir: "/tmp".into(),
        }),
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
    assert_eq!(cfg.model.learned.window("scripted", "m"), Some(150_000));
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
        !cfg.model.capabilities.images,
        "the scripted model is blind"
    );
    let host = RecordingHost::new();
    let mut frames = history("look at this");
    if let Event::ItemCompleted { item } = &mut frames[0].event
        && let ItemBody::User { parts, .. } = &mut item.body
    {
        parts.push(ContentPart::Image {
            media_type: "image/png".into(),
            data: "iVBORw0KGgo=".into(),
        });
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
