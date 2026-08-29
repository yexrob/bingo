//! Fakes shared by the kernel's own tests: a scripted provider, an echo tool,
//! and a tool host that answers nothing.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bingo_sdk::*;
use futures::StreamExt;
use serde_json::{Value, json};

/// One provider response: events in order; `Hang` never ends after them.
pub enum Script {
    Events(Vec<Result<ModelEvent, ProviderError>>),
    Hang(Vec<ModelEvent>),
    Fail(ProviderError),
}

pub struct ScriptedProvider {
    responses: Mutex<VecDeque<Script>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl ScriptedProvider {
    pub fn new(responses: Vec<Script>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(vec![]),
        })
    }
    pub fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn id(&self) -> &str {
        "scripted"
    }
    fn endpoint(&self, _: &str) -> EndpointCapabilities {
        EndpointCapabilities::default()
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

pub fn text(t: &str) -> Vec<Result<ModelEvent, ProviderError>> {
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

pub fn tool_call(name: &str, input: Value) -> Vec<Result<ModelEvent, ProviderError>> {
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

pub struct EchoTool {
    pub read_only: bool,
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

pub struct NoHost;

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

pub fn kind(item: &Item) -> String {
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

pub fn label(event: &Event) -> String {
    match event {
        Event::ItemStarted { item } => format!("started:{}", kind(item)),
        Event::ItemUpdated { item } => format!("updated:{}", kind(item)),
        Event::ItemCompleted { item } => format!("completed:{}", kind(item)),
        Event::ItemDelta { .. } => "delta".into(),
        Event::TurnUsage { .. } => "usage".into(),
        Event::TurnRetrying { .. } => "retrying".into(),
        Event::TurnStarted { .. } => "turnStarted".into(),
        Event::TurnCompleted { status, .. } => format!("turnCompleted:{status:?}")
            .split_whitespace()
            .next()
            .unwrap_or("?")
            .to_string(),
        Event::Compacted { .. } => "compacted".into(),
        Event::Notice { code, .. } => format!("notice:{code}"),
        Event::IntentAck { outcome, .. } => format!("ack:{outcome:?}")
            .split_whitespace()
            .next()
            .unwrap_or("?")
            .to_string(),
        Event::InteractionOpened { .. } => "interactionOpened".into(),
        Event::InteractionResolved { .. } => "interactionResolved".into(),
        Event::InteractionCancelled { .. } => "interactionCancelled".into(),
        Event::QueueChanged { entries, .. } => format!("queue:{}", entries.len()),
        Event::Lagged { .. } => "lagged".into(),
        other => format!("{other:?}")
            .split_whitespace()
            .next()
            .unwrap_or("?")
            .to_string(),
    }
}

/// A tool whose call panics; the actor must report the turn as lost.
pub struct PanicTool;

#[async_trait]
impl Tool for PanicTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "Panic".into(),
            description: "panics".into(),
            input_schema: json!({"type": "object"}),
        }
    }
    fn traits(&self, _: &Value) -> ToolTraits {
        ToolTraits::read_only()
    }
    async fn call(&self, _input: Value, _cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        panic!("tool exploded")
    }
}

/// The scripted model as a turn sees it: a small window, no vision, no
/// reasoning, so a test that needs a fact turns it on explicitly.
pub fn capabilities() -> ModelCapabilities {
    ModelCapabilities {
        context_window: 100_000,
        max_output: 4_000,
        images: false,
        reasoning: false,
        count_tokens: false,
        caching: false,
    }
}

pub fn summary(id: &str) -> SessionSummary {
    let ts = jiff::Timestamp::from_second(0).unwrap();
    SessionSummary {
        id: SessionId::from_raw(id),
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

/// A turn config over the scripted provider with the fail-closed default policy.
pub fn config(
    provider: Arc<ScriptedProvider>,
    tools: Vec<Arc<dyn Tool>>,
    tool_host: Arc<dyn ToolHost>,
) -> crate::turn::TurnConfig {
    crate::turn::TurnConfig {
        session: summary("ses_1"),
        cwd: "/tmp".into(),
        capabilities: capabilities(),
        provider,
        model: "m".into(),
        max_tokens: 1000,
        reasoning: None,
        system: vec![SystemBlock {
            text: "You are bingo.".into(),
            cache: false,
        }],
        tools,
        policy: Arc::new(crate::gate::DefaultPolicy),
        hooks: vec![],
        contributors: vec![],
        compactor: None,
        budget: crate::turn::TurnBudget::default(),
        env: Arc::new(Env {
            home: "/tmp".into(),
            config_dir: "/tmp".into(),
            data_dir: "/tmp".into(),
        }),
        tool_host,
    }
}
