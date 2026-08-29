//! Fakes shared by the kernel's own tests: a scripted provider, an echo tool,
//! and a tool host that answers nothing.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, Weak};

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
            meta: Default::default(),
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
            meta: Default::default(),
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
        model: crate::turn::ModelChoice {
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
        tools: crate::turn::ToolSet::fixed(tools),
        policy: Arc::new(crate::gate::DefaultPolicy),
        hooks: vec![],
        contributors: vec![],
        compaction: Arc::new(crate::turn::Breaker::default()),
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

/// A compaction strategy that answers from a script, so a test decides
/// whether a summary shrinks anything.
pub struct ScriptedCompactor {
    answers: Mutex<VecDeque<Result<Compaction, KernelError>>>,
    pub calls: Mutex<Vec<(CompactReason, u32, u64)>>,
}

impl ScriptedCompactor {
    pub fn new(answers: Vec<Result<Compaction, KernelError>>) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers.into()),
            calls: Mutex::new(Vec::new()),
        })
    }

    /// A cut at `boundary` that claims to go from `before` to `after` tokens.
    pub fn cut(boundary: &str, before: u64, after: u64) -> Result<Compaction, KernelError> {
        Ok(Compaction {
            summary: "what happened so far".into(),
            boundary: ItemId::from_raw(boundary),
            kept: Vec::new(),
            before,
            after,
            usage: Usage {
                input_tokens: 100,
                output_tokens: 20,
                ..Usage::default()
            },
        })
    }
}

#[async_trait]
impl Compactor for ScriptedCompactor {
    async fn compact(
        &self,
        cx: CompactContext<'_>,
        reason: CompactReason,
    ) -> Result<Compaction, KernelError> {
        self.calls
            .lock()
            .unwrap()
            .push((reason, cx.failures, cx.keep_budget));
        self.answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(KernelError::new(ErrorCode::Internal, "compactor exhausted")))
    }
}

/// A command that answers from a script, so a test decides its outcome; a
/// gated one waits for the test before it answers.
pub struct ScriptedCommand {
    name: &'static str,
    instant: bool,
    outcome: Mutex<Option<Result<CommandOutcome, KernelError>>>,
    calls: Mutex<Vec<String>>,
    gate: Mutex<Option<Arc<tokio::sync::Notify>>>,
}

impl ScriptedCommand {
    pub fn new(
        name: &'static str,
        instant: bool,
        outcome: Result<CommandOutcome, KernelError>,
    ) -> Arc<Self> {
        Arc::new(Self {
            name,
            instant,
            outcome: Mutex::new(Some(outcome)),
            calls: Mutex::new(Vec::new()),
            gate: Mutex::new(None),
        })
    }

    /// Make the run wait for a `notify_one` on the returned gate.
    pub fn gated(&self) -> Arc<tokio::sync::Notify> {
        let gate = Arc::new(tokio::sync::Notify::new());
        *self.gate.lock().unwrap() = Some(gate.clone());
        gate
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl Command for ScriptedCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: self.name.into(),
            aliases: Vec::new(),
            hint: String::new(),
            args: ArgSpec::None,
            instant: self.instant,
            family: "test".into(),
        }
    }

    async fn run(&self, args: &str, _cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
        self.calls.lock().unwrap().push(args.to_string());
        let gate = self.gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            gate.notified().await;
        }
        self.outcome
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| Err(KernelError::new(ErrorCode::Internal, "already answered")))
    }
}

/// A host that serves nothing, so a command context has one to hold.
pub struct NoApi;

#[async_trait]
impl HostApi for NoApi {
    async fn sessions(&self, _: SessionFilter) -> Result<Vec<SessionSummary>, KernelError> {
        Ok(Vec::new())
    }
    async fn open(&self, _: SessionSelector, _: ClientIdentity) -> Result<Attachment, KernelError> {
        Err(KernelError::new(ErrorCode::Internal, "no"))
    }
    async fn close(&self, _: &SessionId, _: CloseReason) -> Result<(), KernelError> {
        Ok(())
    }
    async fn delete(&self, _: &SessionId) -> Result<(), KernelError> {
        Ok(())
    }
    async fn catalog(&self, kind: CatalogKind) -> Result<Catalog, KernelError> {
        Ok(Catalog {
            kind,
            entries: Vec::new(),
        })
    }
    fn gateway_events(&self) -> GatewayStream {
        Box::pin(futures::stream::empty())
    }
    fn service_any(&self, _: &str) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        None
    }
}

static NO_API: std::sync::LazyLock<Arc<NoApi>> = std::sync::LazyLock::new(|| Arc::new(NoApi));

/// Services over `NoApi`, kept alive for the whole test binary.
pub fn services(commands: Vec<Arc<dyn Command>>) -> crate::session::Services {
    let weak = Arc::downgrade(&*NO_API);
    let host: Weak<dyn HostApi> = weak;
    crate::session::Services {
        commands,
        command_sources: Vec::new(),
        host,
    }
}

/// A turn-end hook that waits for the test before it records that it ran.
pub struct GatedHook {
    pub gate: Arc<tokio::sync::Notify>,
    pub fired: Arc<std::sync::atomic::AtomicBool>,
}

impl GatedHook {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            gate: Arc::new(tokio::sync::Notify::new()),
            fired: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }
}

#[async_trait]
impl Hook for GatedHook {
    fn id(&self) -> &str {
        "gated"
    }
    fn matcher(&self) -> HookMatcher {
        HookMatcher {
            points: vec![HookPoint::Turn],
            tool: None,
        }
    }
    async fn on_turn(&self, phase: Phase, _: &TurnId, _: &[Item], _: &HookContext) {
        if phase == Phase::End {
            self.gate.notified().await;
            self.fired.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

/// A tool source a test fills after the fact, so a turn can see tools
/// arrive (ADR-0009).
#[derive(Default)]
pub struct ScriptedToolSource {
    tools: Mutex<Vec<Arc<dyn Tool>>>,
}

impl ScriptedToolSource {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn set(&self, tools: Vec<Arc<dyn Tool>>) {
        *self.tools.lock().unwrap() = tools;
    }
}

#[async_trait]
impl ToolSource for ScriptedToolSource {
    fn id(&self) -> &str {
        "scripted"
    }
    async fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.lock().unwrap().clone()
    }
}

/// A command source with a fixed table.
pub struct ScriptedCommandSource {
    commands: Vec<Arc<dyn Command>>,
}

impl ScriptedCommandSource {
    pub fn new(commands: Vec<Arc<dyn Command>>) -> Arc<Self> {
        Arc::new(Self { commands })
    }
}

#[async_trait]
impl CommandSource for ScriptedCommandSource {
    fn id(&self) -> &str {
        "scripted"
    }
    async fn commands(&self, _: &std::path::Path) -> Vec<Arc<dyn Command>> {
        self.commands.clone()
    }
}

/// A hook that asks a person before every tool, for a reason of its own.
pub struct AskingHook {
    pub reason: String,
}

#[async_trait]
impl Hook for AskingHook {
    fn id(&self) -> &str {
        "asking"
    }
    fn matcher(&self) -> HookMatcher {
        HookMatcher {
            points: vec![HookPoint::BeforeTool],
            tool: None,
        }
    }
    async fn before_tool(&self, _: &mut ToolCall, _: &HookContext) -> HookOutcome {
        HookOutcome::Ask {
            reason: self.reason.clone(),
        }
    }
}

/// A hook that writes down every compaction, session and journal point it
/// is called at. Its journal observer waits for `open()` when gated.
pub struct RecordingHook {
    calls: Mutex<Vec<String>>,
    open: tokio::sync::watch::Sender<bool>,
}

impl RecordingHook {
    pub fn new(gated: bool) -> Arc<Self> {
        let (open, _) = tokio::sync::watch::channel(!gated);
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            open,
        })
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    /// Let the gated journal observer through.
    pub fn open(&self) {
        let _ = self.open.send(true);
    }

    fn note(&self, what: String) {
        self.calls.lock().unwrap().push(what);
    }
}

#[async_trait]
impl Hook for RecordingHook {
    fn id(&self) -> &str {
        "recording"
    }
    fn matcher(&self) -> HookMatcher {
        HookMatcher {
            points: vec![HookPoint::Compact, HookPoint::Session, HookPoint::Event],
            tool: None,
        }
    }
    async fn on_compact(&self, phase: Phase, _: &HookContext) {
        self.note(format!("compact:{phase:?}"));
    }
    async fn on_session(&self, phase: Phase, _: &HookContext) {
        self.note(format!("session:{phase:?}"));
    }
    async fn on_event(&self, frame: &Frame, _: &HookContext) {
        let mut open = self.open.subscribe();
        let _ = open.wait_for(|o| *o).await;
        self.note(format!("event:{}", frame.seq.0));
    }
}
