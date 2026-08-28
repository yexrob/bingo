//! A scripted provider for deterministic tests and demos.
//!
//! One `Response` per `stream()` call, one `Step` per block the model would
//! emit. The script is the only source of truth: ids, chunking and the finish
//! reason are derived from the step's position, so a loop test that reads the
//! script knows every event it will see. Requests are recorded and validated
//! the way a real API validates them, so a malformed conversation fails here
//! instead of at the first real provider.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bingo_sdk::{
    CancellationToken, ContentPart, FinishReason, ModelCapabilities, ModelEvent, ModelInfo,
    ModelRequest, ModelStream, Plugin, PluginError, PluginManifest, Provider, ProviderError,
    Registrar, Role, UnifiedFinish, Usage,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Text is emitted in chunks this wide, so a surface sees several deltas per block.
const CHUNK_CHARS: usize = 8;

/// The fake's token estimate: four characters each, in and out.
const CHARS_PER_TOKEN: usize = 4;

/// The model id the fake answers as.
pub const FAKE_MODEL: &str = "fake-1";

/// Everything the provider will say, in order.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Script {
    pub responses: Vec<Response>,
}

/// One provider response: what a single `stream()` call yields.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Response {
    pub steps: Vec<Step>,
    /// Overrides the finish reason derived from the steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish: Option<UnifiedFinish>,
}

/// One block of a response.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum Step {
    Text(String),
    Reasoning(String),
    ToolCall {
        name: String,
        input: Value,
    },
    /// The stream yields this error and ends.
    Error(ProviderError),
    /// Sleeps on the tokio timer, so tests can drive it with `tokio::time::pause`.
    Delay {
        ms: u64,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ScriptError {
    #[error("reading {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing script: {0}")]
    Parse(#[from] serde_json::Error),
}

/// The environment variable `Script::from_env` reads a script path from.
pub const SCRIPT_ENV: &str = "BINGO_FAKE_SCRIPT";

impl Script {
    /// The one-response greeting `FakeProvider::demo` answers with.
    pub fn demo() -> Self {
        Self {
            responses: vec![Response {
                steps: vec![Step::Text("Hello from the fake provider.".into())],
                finish: Some(UnifiedFinish::Stop),
            }],
        }
    }

    /// Two responses: a `Read` call, then the answer that follows its result.
    pub fn demo_tool_round() -> Self {
        Self {
            responses: vec![
                Response {
                    steps: vec![
                        Step::Text("Let me look at the manifest.".into()),
                        Step::ToolCall {
                            name: "Read".into(),
                            input: serde_json::json!({ "file_path": "Cargo.toml" }),
                        },
                    ],
                    finish: Some(UnifiedFinish::ToolCalls),
                },
                Response {
                    steps: vec![Step::Text("Read it.".into())],
                    finish: Some(UnifiedFinish::Stop),
                },
            ],
        }
    }

    pub fn from_json(json: &str) -> Result<Self, ScriptError> {
        Ok(serde_json::from_str(json)?)
    }

    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, ScriptError> {
        let path = path.as_ref();
        let json = std::fs::read_to_string(path).map_err(|source| ScriptError::Read {
            path: path.display().to_string(),
            source,
        })?;
        Self::from_json(&json)
    }

    /// The script at `BINGO_FAKE_SCRIPT`, or `None` when the variable is unset.
    pub fn from_env() -> Result<Option<Self>, ScriptError> {
        match std::env::var(SCRIPT_ENV) {
            Ok(path) => Self::from_path(path).map(Some),
            Err(_) => Ok(None),
        }
    }
}

/// A provider that replays a script and records what it was asked.
#[derive(Debug)]
pub struct FakeProvider {
    script: Script,
    /// The next response to hand out; also the number of `stream()` calls served.
    cursor: AtomicUsize,
    requests: Mutex<Vec<ModelRequest>>,
}

impl FakeProvider {
    pub fn new(script: Script) -> Self {
        Self {
            script,
            cursor: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// The greeting script, for `--provider fake` with no script configured.
    pub fn demo() -> Self {
        Self::new(Script::demo())
    }

    /// Every request this provider was asked to stream, in order.
    pub fn requests(&self) -> Vec<ModelRequest> {
        self.lock_requests().clone()
    }

    fn lock_requests(&self) -> std::sync::MutexGuard<'_, Vec<ModelRequest>> {
        // A panic in another thread must not hide the recording from a test.
        self.requests.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A request part the loop can send but the fake cannot answer.
fn request_error(message: impl Into<String>) -> ProviderError {
    ProviderError::Request {
        message: message.into(),
    }
}

/// Reject the conversations a real API rejects: an unanswered tool call, an
/// empty assistant turn, an empty conversation.
fn validate(request: &ModelRequest) -> Result<(), ProviderError> {
    if request.messages.is_empty() {
        return Err(request_error("messages must not be empty"));
    }
    for (i, message) in request.messages.iter().enumerate() {
        if message.role != Role::Assistant {
            continue;
        }
        if message.parts.is_empty() {
            return Err(request_error(format!("assistant message {i} has no parts")));
        }
        for part in &message.parts {
            let ContentPart::ToolUse { id, .. } = part else {
                continue;
            };
            let answered = request.messages.get(i + 1).is_some_and(|next| {
                next.role == Role::User
                    && next.parts.iter().any(|p| {
                        matches!(p, ContentPart::ToolResult { tool_use_id, .. } if tool_use_id == id)
                    })
            });
            if !answered {
                return Err(request_error(format!(
                    "tool_use {id} has no tool_result in the following message"
                )));
            }
        }
    }
    Ok(())
}

/// The characters a request costs, the one input the token estimate reads.
fn request_chars(request: &ModelRequest) -> usize {
    let system: usize = request.system.iter().map(|b| b.text.chars().count()).sum();
    let messages: usize = request
        .messages
        .iter()
        .flat_map(|m| m.parts.iter())
        .map(part_chars)
        .sum();
    let tools: usize = request
        .tools
        .iter()
        .map(|t| {
            t.name.chars().count() + t.description.chars().count() + json_chars(&t.input_schema)
        })
        .sum();
    system + messages + tools
}

fn part_chars(part: &ContentPart) -> usize {
    match part {
        ContentPart::Text { text } => text.chars().count(),
        ContentPart::Reasoning { text, .. } => text.chars().count(),
        ContentPart::Image { data, .. } => data.chars().count(),
        ContentPart::ToolUse { name, input, .. } => name.chars().count() + json_chars(input),
        ContentPart::ToolResult { parts, .. } => parts.iter().map(part_chars).sum(),
    }
}

fn json_chars(value: &Value) -> usize {
    json_text(value).chars().count()
}

fn json_text(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| String::from("null"))
}

fn estimate_tokens(chars: usize) -> u64 {
    (chars / CHARS_PER_TOKEN) as u64
}

/// One thing the stream does next: hand out an event, wait, or fail.
#[derive(Debug)]
enum Beat {
    Event(ModelEvent),
    Sleep(u64),
    Fail(ProviderError),
}

/// Expand one response into the beats its steps describe. `index` is the
/// response's position in the script; block ids are derived from it, so the
/// same script always yields the same ids.
fn beats(response: &Response, index: usize, input_chars: usize) -> Vec<Beat> {
    let mut out = vec![
        Beat::Event(ModelEvent::StreamStart {
            warnings: Vec::new(),
        }),
        Beat::Event(ModelEvent::ResponseMetadata {
            id: Some(format!("fake-{index}")),
            model: Some(FAKE_MODEL.to_string()),
        }),
    ];
    let mut emitted = 0usize;
    let mut called = false;
    for (step_index, step) in response.steps.iter().enumerate() {
        let block = format!("blk_{index}_{step_index}");
        match step {
            Step::Text(text) => {
                emitted += text.chars().count();
                out.push(Beat::Event(ModelEvent::TextStart { id: block.clone() }));
                for delta in chunks(text) {
                    out.push(Beat::Event(ModelEvent::TextDelta {
                        id: block.clone(),
                        delta,
                    }));
                }
                out.push(Beat::Event(ModelEvent::TextEnd { id: block }));
            }
            Step::Reasoning(text) => {
                emitted += text.chars().count();
                out.push(Beat::Event(ModelEvent::ReasoningStart {
                    id: block.clone(),
                }));
                for delta in chunks(text) {
                    out.push(Beat::Event(ModelEvent::ReasoningDelta {
                        id: block.clone(),
                        delta,
                    }));
                }
                out.push(Beat::Event(ModelEvent::ReasoningEnd {
                    id: block,
                    provider_metadata: Default::default(),
                }));
            }
            Step::ToolCall { name, input } => {
                called = true;
                let call = format!("call_{index}_{step_index}");
                let json = json_text(input);
                emitted += json.chars().count();
                out.push(Beat::Event(ModelEvent::ToolInputStart {
                    id: call.clone(),
                    name: name.clone(),
                }));
                out.push(Beat::Event(ModelEvent::ToolInputDelta {
                    id: call.clone(),
                    delta: json.clone(),
                }));
                out.push(Beat::Event(ModelEvent::ToolInputEnd { id: call.clone() }));
                out.push(Beat::Event(ModelEvent::ToolCall {
                    id: call,
                    name: name.clone(),
                    input: json,
                }));
            }
            Step::Error(error) => {
                out.push(Beat::Fail(error.clone()));
                return out;
            }
            Step::Delay { ms } => out.push(Beat::Sleep(*ms)),
        }
    }
    let unified = response.finish.unwrap_or(if called {
        UnifiedFinish::ToolCalls
    } else {
        UnifiedFinish::Stop
    });
    out.push(Beat::Event(ModelEvent::Finish {
        usage: Usage {
            input_tokens: estimate_tokens(input_chars),
            output_tokens: estimate_tokens(emitted),
            ..Default::default()
        },
        finish_reason: FinishReason::unified(unified),
    }));
    out
}

fn chunks(text: &str) -> Vec<String> {
    text.chars()
        .collect::<Vec<_>>()
        .chunks(CHUNK_CHARS)
        .map(|c| c.iter().collect())
        .collect()
}

#[async_trait]
impl Provider for FakeProvider {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self, _model: &str) -> ModelCapabilities {
        ModelCapabilities {
            context_window: 200_000,
            max_output: 8_192,
            images: true,
            reasoning: true,
            count_tokens: true,
            caching: false,
        }
    }

    async fn stream(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<ModelStream, ProviderError> {
        let input_chars = request_chars(&request);
        // Recorded before it is judged: a rejected request is the one a test
        // most wants to read back.
        let verdict = validate(&request);
        self.lock_requests().push(request);
        verdict?;
        let index = self.cursor.fetch_add(1, Ordering::SeqCst);
        let Some(response) = self.script.responses.get(index) else {
            return Err(request_error("script exhausted"));
        };
        let beats = beats(response, index, input_chars).into_iter();
        Ok(Box::pin(futures::stream::unfold(
            (beats, cancel),
            |(mut beats, cancel)| async move {
                loop {
                    if cancel.is_cancelled() {
                        return None;
                    }
                    match beats.next()? {
                        Beat::Event(event) => return Some((Ok(event), (beats, cancel))),
                        Beat::Fail(error) => return Some((Err(error), (beats, cancel))),
                        Beat::Sleep(ms) => {
                            tokio::select! {
                                _ = tokio::time::sleep(std::time::Duration::from_millis(ms)) => {}
                                _ = cancel.cancelled() => return None,
                            }
                        }
                    }
                }
            },
        )))
    }

    async fn count_tokens(&self, request: &ModelRequest) -> Result<u64, ProviderError> {
        Ok(estimate_tokens(request_chars(request)))
    }

    async fn models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        Ok(vec![ModelInfo {
            id: FAKE_MODEL.to_string(),
            display: None,
            capabilities: Some(self.capabilities(FAKE_MODEL)),
        }])
    }
}

static MANIFEST: PluginManifest = PluginManifest {
    id: "bingo.provider.fake",
    version: env!("CARGO_PKG_VERSION"),
    sdk: "^0.1",
    provides: &["provider:fake"],
    requires: &[],
    config: None,
};

/// Registers one `FakeProvider`.
#[derive(Debug)]
pub struct FakePlugin {
    provider: Arc<FakeProvider>,
}

impl FakePlugin {
    pub fn new(provider: Arc<FakeProvider>) -> Self {
        Self { provider }
    }

    pub fn demo() -> Self {
        Self::new(Arc::new(FakeProvider::demo()))
    }

    /// The provider it registered, so a test can read the recorded requests.
    pub fn provider(&self) -> Arc<FakeProvider> {
        Arc::clone(&self.provider)
    }
}

#[async_trait]
impl Plugin for FakePlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }

    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        registrar.provider(Arc::clone(&self.provider) as Arc<dyn Provider>);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_sdk::{Contribution, Message};
    use futures::StreamExt;

    fn request(text: &str) -> ModelRequest {
        ModelRequest {
            model: FAKE_MODEL.into(),
            max_tokens: 1024,
            system: Vec::new(),
            messages: vec![Message::text(Role::User, text)],
            tools: Vec::new(),
            reasoning: None,
            provider_options: Default::default(),
        }
    }

    async fn drain(provider: &FakeProvider, text: &str) -> Vec<Result<ModelEvent, ProviderError>> {
        let stream = provider
            .stream(request(text), CancellationToken::new())
            .await
            .expect("the script has a response");
        stream.collect().await
    }

    fn events(items: Vec<Result<ModelEvent, ProviderError>>) -> Vec<ModelEvent> {
        items.into_iter().map(|e| e.expect("no error")).collect()
    }

    #[tokio::test]
    async fn a_text_step_yields_start_chunked_deltas_end_then_finish() {
        let provider = FakeProvider::new(Script {
            responses: vec![Response {
                steps: vec![Step::Text("0123456789".into())],
                finish: None,
            }],
        });
        assert_eq!(
            events(drain(&provider, "hi").await),
            vec![
                ModelEvent::StreamStart {
                    warnings: Vec::new()
                },
                ModelEvent::ResponseMetadata {
                    id: Some("fake-0".into()),
                    model: Some(FAKE_MODEL.into()),
                },
                ModelEvent::TextStart {
                    id: "blk_0_0".into()
                },
                ModelEvent::TextDelta {
                    id: "blk_0_0".into(),
                    delta: "01234567".into(),
                },
                ModelEvent::TextDelta {
                    id: "blk_0_0".into(),
                    delta: "89".into(),
                },
                ModelEvent::TextEnd {
                    id: "blk_0_0".into()
                },
                ModelEvent::Finish {
                    usage: Usage {
                        input_tokens: 0,
                        output_tokens: 2,
                        ..Default::default()
                    },
                    finish_reason: FinishReason::unified(UnifiedFinish::Stop),
                },
            ]
        );
    }

    #[tokio::test]
    async fn a_tool_call_step_yields_start_delta_end_then_the_call() {
        let provider = FakeProvider::new(Script::demo_tool_round());
        let first = events(drain(&provider, "read the manifest").await);
        assert_eq!(
            &first[first.len() - 5..],
            &[
                ModelEvent::ToolInputStart {
                    id: "call_0_1".into(),
                    name: "Read".into(),
                },
                ModelEvent::ToolInputDelta {
                    id: "call_0_1".into(),
                    delta: r#"{"file_path":"Cargo.toml"}"#.into(),
                },
                ModelEvent::ToolInputEnd {
                    id: "call_0_1".into()
                },
                ModelEvent::ToolCall {
                    id: "call_0_1".into(),
                    name: "Read".into(),
                    input: r#"{"file_path":"Cargo.toml"}"#.into(),
                },
                ModelEvent::Finish {
                    usage: Usage {
                        input_tokens: 4,
                        output_tokens: 13,
                        ..Default::default()
                    },
                    finish_reason: FinishReason::unified(UnifiedFinish::ToolCalls),
                },
            ]
        );
    }

    #[tokio::test]
    async fn a_tool_call_without_an_explicit_finish_finishes_as_tool_calls() {
        let provider = FakeProvider::new(Script {
            responses: vec![Response {
                steps: vec![Step::ToolCall {
                    name: "Read".into(),
                    input: serde_json::json!({}),
                }],
                finish: None,
            }],
        });
        let last = events(drain(&provider, "hi").await).pop().expect("finish");
        assert!(matches!(
            last,
            ModelEvent::Finish {
                finish_reason: FinishReason {
                    unified: UnifiedFinish::ToolCalls,
                    ..
                },
                ..
            }
        ));
    }

    #[tokio::test]
    async fn an_error_step_ends_the_stream_with_that_error() {
        let provider = FakeProvider::new(Script {
            responses: vec![Response {
                steps: vec![
                    Step::Text("almost".into()),
                    Step::Error(ProviderError::Server {
                        status: 503,
                        message: "overloaded".into(),
                    }),
                    Step::Text("never".into()),
                ],
                finish: None,
            }],
        });
        let items = drain(&provider, "hi").await;
        let last = items.last().expect("an item");
        assert_eq!(
            last.as_ref().err(),
            Some(&ProviderError::Server {
                status: 503,
                message: "overloaded".into(),
            })
        );
        assert!(
            !items
                .iter()
                .any(|e| matches!(e, Ok(ModelEvent::Finish { .. }))),
            "an errored response never finishes"
        );
    }

    #[tokio::test]
    async fn a_reasoning_step_yields_reasoning_events() {
        let provider = FakeProvider::new(Script {
            responses: vec![Response {
                steps: vec![Step::Reasoning("think".into())],
                finish: None,
            }],
        });
        let kinds = events(drain(&provider, "hi").await);
        assert!(matches!(kinds[2], ModelEvent::ReasoningStart { .. }));
        assert!(matches!(kinds[3], ModelEvent::ReasoningDelta { .. }));
        assert!(matches!(kinds[4], ModelEvent::ReasoningEnd { .. }));
    }

    #[tokio::test]
    async fn each_call_takes_the_next_response_and_then_the_script_is_exhausted() {
        let provider = FakeProvider::new(Script::demo_tool_round());
        drain(&provider, "one").await;
        drain(&provider, "two").await;
        let third = provider
            .stream(request("three"), CancellationToken::new())
            .await;
        assert_eq!(
            third.err(),
            Some(ProviderError::Request {
                message: "script exhausted".into()
            })
        );
        assert_eq!(
            provider.requests().len(),
            3,
            "even the refused call is recorded"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_delay_step_waits_on_the_timer() {
        let provider = FakeProvider::new(Script {
            responses: vec![Response {
                steps: vec![Step::Delay { ms: 5_000 }, Step::Text("late".into())],
                finish: None,
            }],
        });
        let started = tokio::time::Instant::now();
        drain(&provider, "hi").await;
        assert!(started.elapsed() >= std::time::Duration::from_millis(5_000));
    }

    #[tokio::test]
    async fn a_cancelled_token_stops_the_stream() {
        let provider = FakeProvider::new(Script {
            responses: vec![Response {
                steps: vec![Step::Text("0123456789abcdefgh".into())],
                finish: None,
            }],
        });
        let cancel = CancellationToken::new();
        let mut stream = provider
            .stream(request("hi"), cancel.clone())
            .await
            .expect("a response");
        assert!(stream.next().await.is_some());
        cancel.cancel();
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn an_unanswered_tool_use_is_rejected() {
        let provider = FakeProvider::new(Script::demo());
        let mut request = request("hi");
        request
            .messages
            .push(Message::assistant(vec![ContentPart::ToolUse {
                id: "call_1".into(),
                name: "Read".into(),
                input: serde_json::json!({}),
            }]));
        let error = provider
            .stream(request, CancellationToken::new())
            .await
            .err();
        assert!(matches!(error, Some(ProviderError::Request { .. })));
    }

    #[tokio::test]
    async fn an_answered_tool_use_is_accepted() {
        let provider = FakeProvider::new(Script::demo());
        let mut request = request("hi");
        request
            .messages
            .push(Message::assistant(vec![ContentPart::ToolUse {
                id: "call_1".into(),
                name: "Read".into(),
                input: serde_json::json!({}),
            }]));
        request
            .messages
            .push(Message::user(vec![ContentPart::ToolResult {
                tool_use_id: "call_1".into(),
                parts: vec![ContentPart::text("ok")],
                is_error: false,
            }]));
        assert!(
            provider
                .stream(request, CancellationToken::new())
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn an_empty_assistant_message_is_rejected() {
        let provider = FakeProvider::new(Script::demo());
        let mut request = request("hi");
        request.messages.push(Message::assistant(Vec::new()));
        let error = provider
            .stream(request, CancellationToken::new())
            .await
            .err();
        assert!(matches!(error, Some(ProviderError::Request { .. })));
    }

    #[tokio::test]
    async fn an_empty_conversation_is_rejected() {
        let provider = FakeProvider::new(Script::demo());
        let mut request = request("hi");
        request.messages.clear();
        let error = provider
            .stream(request, CancellationToken::new())
            .await
            .err();
        assert!(matches!(error, Some(ProviderError::Request { .. })));
    }

    #[tokio::test]
    async fn every_request_is_recorded_in_order() {
        let provider = FakeProvider::new(Script::demo_tool_round());
        drain(&provider, "first").await;
        drain(&provider, "second").await;
        let recorded = provider.requests();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[1].messages[0].parts[0].as_text(), Some("second"));
    }

    #[tokio::test]
    async fn counting_tokens_estimates_four_characters_each() {
        let provider = FakeProvider::demo();
        let count = provider
            .count_tokens(&request("12345678"))
            .await
            .expect("the fake counts tokens");
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn the_fake_advertises_one_model() {
        let provider = FakeProvider::demo();
        let models = provider.models().await.expect("models");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, FAKE_MODEL);
        assert_eq!(provider.capabilities(FAKE_MODEL).context_window, 200_000);
        assert!(!provider.capabilities(FAKE_MODEL).caching);
    }

    #[test]
    fn a_script_round_trips_through_json() {
        let script = Script {
            responses: vec![
                Response {
                    steps: vec![
                        Step::Text("hi".into()),
                        Step::Reasoning("hm".into()),
                        Step::ToolCall {
                            name: "Read".into(),
                            input: serde_json::json!({"file_path": "a.txt"}),
                        },
                        Step::Delay { ms: 10 },
                        Step::Error(ProviderError::Timeout),
                    ],
                    finish: Some(UnifiedFinish::Length),
                },
                Response::default(),
            ],
        };
        let json = serde_json::to_string(&script).expect("serialize");
        assert_eq!(Script::from_json(&json).expect("parse"), script);
    }

    /// `set_var` is unsafe in Rust 2024 and `unsafe` is forbidden workspace-wide,
    /// so the file half of `from_env` is tested through the path it delegates to.
    #[test]
    fn from_env_reads_the_scripted_path_and_is_none_without_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("script.json");
        let script = Script::demo_tool_round();
        std::fs::write(&path, serde_json::to_string(&script).expect("serialize")).expect("write");
        assert_eq!(Script::from_path(&path).expect("read"), script);

        if std::env::var(SCRIPT_ENV).is_err() {
            assert_eq!(
                Script::from_env().expect("an unset variable is not an error"),
                None
            );
        }
    }

    #[test]
    fn a_missing_script_file_is_a_read_error() {
        let dir = tempfile::tempdir().expect("temp dir");
        let error = Script::from_path(dir.path().join("absent.json")).expect_err("missing");
        assert!(matches!(error, ScriptError::Read { .. }));
    }

    #[test]
    fn the_plugin_registers_the_provider_it_was_built_with() {
        let plugin = FakePlugin::demo();
        let mut registrar = Registrar::new("bingo.provider.fake", Value::Null);
        plugin.register(&mut registrar).expect("register");
        let contributions = registrar.into_contributions();
        assert_eq!(contributions.len(), 1);
        assert!(matches!(contributions[0], Contribution::Provider(_)));
        assert_eq!(plugin.manifest().provides, &["provider:fake"]);
        assert_eq!(plugin.provider().id(), "fake");
    }
}
