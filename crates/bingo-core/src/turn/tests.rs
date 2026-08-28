use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};

use super::*;
use crate::gate::DefaultPolicy;

/// One provider response: events in order; `None` means hang forever after them.
enum Script {
    Events(Vec<Result<ModelEvent, ProviderError>>),
    Hang(Vec<ModelEvent>),
    Fail(ProviderError),
}

struct ScriptedProvider {
    responses: Mutex<VecDeque<Script>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl ScriptedProvider {
    fn new(responses: Vec<Script>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(vec![]),
        })
    }
    fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn id(&self) -> &str {
        "scripted"
    }
    fn capabilities(&self, _: &str) -> ModelCapabilities {
        ModelCapabilities {
            context_window: 100_000,
            max_output: 4_000,
            images: false,
            reasoning: false,
            count_tokens: false,
            caching: false,
        }
    }
    async fn stream(
        &self,
        request: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<ModelStream, ProviderError> {
        self.requests.lock().unwrap().push(request);
        let next = self.responses.lock().unwrap().pop_front();
        match next {
            None => Err(ProviderError::Request {
                message: "script exhausted".into(),
            }),
            Some(Script::Fail(e)) => Err(e),
            Some(Script::Events(evs)) => Ok(Box::pin(futures::stream::iter(evs))),
            Some(Script::Hang(evs)) => Ok(Box::pin(
                futures::stream::iter(evs.into_iter().map(Ok)).chain(futures::stream::pending()),
            )),
        }
    }
}

fn text(t: &str) -> Vec<Result<ModelEvent, ProviderError>> {
    vec![
        Ok(ModelEvent::TextStart { id: "b".into() }),
        Ok(ModelEvent::TextDelta {
            id: "b".into(),
            delta: t.into(),
        }),
        Ok(ModelEvent::TextEnd { id: "b".into() }),
        Ok(ModelEvent::Finish {
            usage: Usage {
                input_tokens: 10,
                output_tokens: 3,
                ..Default::default()
            },
            finish_reason: FinishReason::unified(UnifiedFinish::Stop),
        }),
    ]
}

fn tool_call(name: &str, input: Value) -> Vec<Result<ModelEvent, ProviderError>> {
    vec![
        Ok(ModelEvent::ToolCall {
            id: "c1".into(),
            name: name.into(),
            input: input.to_string(),
        }),
        Ok(ModelEvent::Finish {
            usage: Usage {
                input_tokens: 10,
                output_tokens: 3,
                ..Default::default()
            },
            finish_reason: FinishReason::unified(UnifiedFinish::ToolCalls),
        }),
    ]
}

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

fn kind(item: &Item) -> String {
    let body = match &item.body {
        ItemBody::User { .. } => "user",
        ItemBody::Assistant { .. } => "assistant",
        ItemBody::Reasoning { .. } => "reasoning",
        ItemBody::ToolCall { .. } => "tool",
        ItemBody::Action { .. } => "action",
        ItemBody::Compaction { .. } => "compaction",
        ItemBody::Rewind { .. } => "rewind",
        ItemBody::Interruption { .. } => "interruption",
        ItemBody::Notice { .. } => "notice",
        ItemBody::QuestionAnswer { .. } => "qa",
        ItemBody::PermissionReceipt { .. } => "receipt",
        ItemBody::Asset { .. } => "asset",
    };
    format!("{body}/{:?}", item.status).to_lowercase()
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

struct EchoTool {
    read_only: bool,
}

#[async_trait]
impl Tool for EchoTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "Echo".into(),
            description: "echo".into(),
            input_schema: json!({"type": "object"}),
        }
    }
    fn traits(&self, _: &Value) -> ToolTraits {
        if self.read_only {
            ToolTraits::read_only()
        } else {
            ToolTraits::edit()
        }
    }
    async fn call(&self, input: Value, _cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::text(format!("echo:{}", input["v"])))
    }
}

struct NoHost;
#[async_trait]
impl Prompter for NoHost {
    async fn ask(&self, _: InteractionKind, _: Vec<AnswerSpec>) -> Result<Answer, KernelError> {
        Ok(Answer::Cancel)
    }
}
#[async_trait]
impl ToolHost for NoHost {
    fn progress(&self, _: &ItemId, _: String) {}
    async fn record(&self, _: ItemBody) -> Result<ItemId, KernelError> {
        Ok(ItemId::mint())
    }
    async fn spawn_session(&self, _: SessionSpec) -> Result<SessionId, KernelError> {
        Err(KernelError::new(ErrorCode::Internal, "no"))
    }
    fn submit(&self, _: &SessionId, _: IntentId, _: Input) {}
    fn service_any(&self, _: &str) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        None
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
        capabilities: provider.capabilities("m"),
        provider,
        model: "m".into(),
        max_tokens: 1000,
        reasoning: None,
        system: vec![SystemBlock {
            text: "You are bingo.".into(),
            cache: false,
        }],
        tools,
        policy: Arc::new(DefaultPolicy),
        hooks: vec![],
        contributors: vec![],
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
