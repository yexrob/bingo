//! A scripted provider for deterministic tests and demos.
//!
//! One `Response` per `stream()` call, one `Step` per block the model would
//! emit. The script is the only source of truth: ids, chunking and the finish
//! reason are derived from the step's position, so a loop test that reads the
//! script knows every event it will see. Requests are recorded and validated
//! the way a real API validates them, so a malformed conversation fails here
//! instead of at the first real provider.
//!
//! Responses are handed out in order, one per `stream()` call. One script
//! serves every session of a run, so as soon as two sessions are awake that
//! order is decided by whichever of them asks first. A response that must not
//! be spent on the wrong one carries a `when` matcher and waits for the
//! request it was written for; a response without one goes to whoever asks
//! next, which is what a single-session script wants.

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bingo_sdk::{
    CancellationToken, ContentPart, EndpointCapabilities, FinishReason, ModelEvent, ModelInfo,
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
    /// What a side question — a plugin's, asked beside the conversation and
    /// marked `provider_options.bingo.purpose` — is answered with, in order;
    /// past the end of this list, with nothing. The conversation's responses
    /// are never spent on one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub side: Vec<Response>,
}

/// One provider response: what a single `stream()` call yields.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Response {
    pub steps: Vec<Step>,
    /// Overrides the finish reason derived from the steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish: Option<UnifiedFinish>,
    /// Which requests may take this response. Without one it goes to whichever
    /// session asks next; with one it is passed over until its own asker comes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<Match>,
}

impl Response {
    /// Whether a request carrying `text` may be answered with this response.
    fn answers(&self, text: &str) -> bool {
        self.when.as_ref().is_none_or(|when| when.matches(text))
    }
}

/// What a response waits for: a way of recognising the request it was written
/// for, so a script shared by several sessions can address a turn instead of
/// racing for it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum Match {
    /// The request carries this text somewhere — its system prompt, a message,
    /// a tool call's input, or a tool result. Pick something only the intended
    /// session can have said or been told.
    Contains(String),
}

impl Match {
    fn matches(&self, text: &str) -> bool {
        match self {
            Self::Contains(needle) => text.contains(needle),
        }
    }
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
                when: None,
            }],
            side: Vec::new(),
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
                    when: None,
                },
                Response {
                    steps: vec![Step::Text("Read it.".into())],
                    finish: Some(UnifiedFinish::Stop),
                    when: None,
                },
            ],
            side: Vec::new(),
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

/// A list of responses and which of them have been handed out. A request takes
/// the first one still free that it matches, so an unaddressed script is dealt
/// straight down the list while an addressed response is passed over until the
/// request it names arrives.
#[derive(Debug)]
struct Deck {
    responses: Vec<Response>,
    taken: Mutex<Vec<bool>>,
}

impl Deck {
    fn new(responses: Vec<Response>) -> Self {
        let taken = Mutex::new(vec![false; responses.len()]);
        Self { responses, taken }
    }

    /// The first free response a request carrying `text` matches, marked taken.
    fn claim(&self, text: &str) -> Option<(usize, &Response)> {
        let mut taken = self.lock_taken();
        let index = self
            .responses
            .iter()
            .enumerate()
            .find(|(index, response)| !taken[*index] && response.answers(text))
            .map(|(index, _)| index)?;
        taken[index] = true;
        Some((index, &self.responses[index]))
    }

    /// How many responses nobody has taken yet.
    fn free(&self) -> usize {
        self.lock_taken().iter().filter(|taken| !**taken).count()
    }

    fn lock_taken(&self) -> std::sync::MutexGuard<'_, Vec<bool>> {
        // A panic in another thread must not strand every request after it.
        self.taken.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// What a request that could take nothing is told: a spent script and one
/// whose remainder belongs to other sessions are different mistakes.
fn nothing_left(free: usize) -> ProviderError {
    if free == 0 {
        return request_error("script exhausted");
    }
    request_error(format!(
        "script exhausted for this request: {free} response(s) left, none addressed to it"
    ))
}

/// A provider that replays a script and records what it was asked.
#[derive(Debug)]
pub struct FakeProvider {
    conversation: Deck,
    /// Side questions are dealt from their own deck, so one never spends a
    /// response the conversation was going to need.
    side: Deck,
    requests: Mutex<Vec<ModelRequest>>,
}

impl FakeProvider {
    pub fn new(script: Script) -> Self {
        Self {
            conversation: Deck::new(script.responses),
            side: Deck::new(script.side),
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

/// Everything a request says, for a response's `when` to read: the system
/// prompt and every message part, a tool call's input and a tool result
/// included. The tool specs are left out — they are the same on every request,
/// so nothing in them can tell one session from another.
fn request_text(request: &ModelRequest) -> String {
    let mut text = String::new();
    for block in &request.system {
        text.push_str(&block.text);
        text.push('\n');
    }
    for part in request.messages.iter().flat_map(|m| m.parts.iter()) {
        push_part_text(&mut text, part);
    }
    text
}

/// One part's text, appended; a tool result is whatever its own parts say.
fn push_part_text(text: &mut String, part: &ContentPart) {
    match part {
        ContentPart::Text { text: said } | ContentPart::Reasoning { text: said, .. } => {
            text.push_str(said);
            text.push('\n');
        }
        ContentPart::ToolUse { name, input, .. } => {
            text.push_str(name);
            text.push('\n');
            text.push_str(&json_text(input));
            text.push('\n');
        }
        ContentPart::ToolResult { parts, .. } => {
            for part in parts {
                push_part_text(text, part);
            }
        }
        ContentPart::Image(_) => {}
    }
}

fn part_chars(part: &ContentPart) -> usize {
    match part {
        ContentPart::Text { text } => text.chars().count(),
        ContentPart::Reasoning { text, .. } => text.chars().count(),
        ContentPart::Image(image) => image.data.chars().count(),
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
/// A request a plugin asks beside the conversation — a memory extractor's,
/// say — carries `provider_options.bingo.purpose`. The script's responses
/// are the conversation's, so a scenario's responses land on the turns that
/// asked for them; a side question is answered from `side`, or with nothing.
fn side_question(request: &ModelRequest) -> bool {
    request
        .provider_options
        .get("bingo")
        .and_then(|about| about.get("purpose"))
        .is_some()
}

/// The empty answer a side question gets: a stream that opens and finishes.
fn nothing_to_say(input_chars: usize) -> Vec<Result<ModelEvent, ProviderError>> {
    vec![
        Ok(ModelEvent::StreamStart {
            warnings: Vec::new(),
        }),
        Ok(ModelEvent::Finish {
            usage: Usage {
                input_tokens: estimate_tokens(input_chars),
                ..Default::default()
            },
            finish_reason: FinishReason::unified(UnifiedFinish::Stop),
        }),
    ]
}

impl FakeProvider {
    /// The next side answer, or nothing to say.
    fn side_answer(&self, text: &str, input_chars: usize) -> ModelStream {
        let events: Vec<Result<ModelEvent, ProviderError>> = match self.side.claim(text) {
            Some((index, response)) => beats(response, index, input_chars)
                .into_iter()
                .filter_map(|beat| match beat {
                    Beat::Event(event) => Some(Ok(event)),
                    Beat::Fail(error) => Some(Err(error)),
                    Beat::Sleep(_) => None,
                })
                .collect(),
            None => nothing_to_say(input_chars),
        };
        Box::pin(futures::stream::iter(events))
    }
}

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
                out.extend(text_beats(block, text));
            }
            Step::Reasoning(text) => {
                emitted += text.chars().count();
                out.extend(reasoning_beats(block, text));
            }
            Step::ToolCall { name, input } => {
                called = true;
                let json = json_text(input);
                emitted += json.chars().count();
                let call = format!("call_{index}_{step_index}");
                out.extend(tool_call_beats(call, name, json));
            }
            Step::Error(error) => {
                out.push(Beat::Fail(error.clone()));
                return out;
            }
            Step::Delay { ms } => out.push(Beat::Sleep(*ms)),
        }
    }
    out.push(finish_beat(response.finish, called, input_chars, emitted));
    out
}

/// One text block: start, the text in `CHUNK_CHARS`-wide deltas, end.
fn text_beats(block: String, text: &str) -> Vec<Beat> {
    let mut out = vec![Beat::Event(ModelEvent::TextStart { id: block.clone() })];
    out.extend(chunks(text).into_iter().map(|delta| {
        Beat::Event(ModelEvent::TextDelta {
            id: block.clone(),
            delta,
        })
    }));
    out.push(Beat::Event(ModelEvent::TextEnd { id: block }));
    out
}

/// One reasoning block, chunked the same way as prose.
fn reasoning_beats(block: String, text: &str) -> Vec<Beat> {
    let mut out = vec![Beat::Event(ModelEvent::ReasoningStart {
        id: block.clone(),
    })];
    out.extend(chunks(text).into_iter().map(|delta| {
        Beat::Event(ModelEvent::ReasoningDelta {
            id: block.clone(),
            delta,
        })
    }));
    out.push(Beat::Event(ModelEvent::ReasoningEnd {
        id: block,
        provider_metadata: Default::default(),
    }));
    out
}

/// One tool call: the input arrives whole, in one delta, then the call itself.
fn tool_call_beats(call: String, name: &str, input: String) -> Vec<Beat> {
    vec![
        Beat::Event(ModelEvent::ToolInputStart {
            id: call.clone(),
            name: name.to_string(),
        }),
        Beat::Event(ModelEvent::ToolInputDelta {
            id: call.clone(),
            delta: input.clone(),
        }),
        Beat::Event(ModelEvent::ToolInputEnd { id: call.clone() }),
        Beat::Event(ModelEvent::ToolCall {
            id: call,
            name: name.to_string(),
            input,
        }),
    ]
}

/// The response's own finish reason, or the one its steps imply.
fn finish_beat(
    finish: Option<UnifiedFinish>,
    called: bool,
    input_chars: usize,
    output_chars: usize,
) -> Beat {
    let unified = finish.unwrap_or(if called {
        UnifiedFinish::ToolCalls
    } else {
        UnifiedFinish::Stop
    });
    Beat::Event(ModelEvent::Finish {
        usage: Usage {
            input_tokens: estimate_tokens(input_chars),
            output_tokens: estimate_tokens(output_chars),
            ..Default::default()
        },
        finish_reason: FinishReason::unified(unified),
    })
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

    fn endpoint(&self, _model: &str) -> EndpointCapabilities {
        EndpointCapabilities {
            images: true,
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
        let text = request_text(&request);
        // Recorded before it is judged: a rejected request is the one a test
        // most wants to read back.
        let verdict = validate(&request);
        let side = side_question(&request);
        self.lock_requests().push(request);
        verdict?;
        if side {
            return Ok(self.side_answer(&text, input_chars));
        }
        let Some((index, response)) = self.conversation.claim(&text) else {
            return Err(nothing_left(self.conversation.free()));
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
mod side_tests {
    use super::*;
    use bingo_sdk::{Message, ProviderMetadata, Role};
    use futures::StreamExt;

    /// A memory extractor's question is not the conversation's next turn:
    /// it is answered with nothing and the script's cursor does not move.
    #[tokio::test]
    async fn a_side_question_is_answered_with_nothing_and_takes_no_response() {
        let provider = FakeProvider::new(Script {
            responses: vec![Response {
                steps: vec![Step::Text("the turn's answer".into())],
                finish: None,
                when: None,
            }],

            side: Vec::new(),
        });
        let mut side = ModelRequest {
            model: FAKE_MODEL.into(),
            max_tokens: 16,
            system: Vec::new(),
            messages: vec![Message::text(Role::User, "what did we learn?")],
            tools: Vec::new(),
            reasoning: None,
            session: None,
            provider_options: ProviderMetadata::new(),
        };
        let mut about = serde_json::Map::new();
        about.insert("purpose".into(), serde_json::Value::String("memory".into()));
        side.provider_options.insert("bingo".into(), about);
        let mut conversation = side.clone();
        conversation.provider_options.clear();

        let mut answered = provider
            .stream(side, CancellationToken::new())
            .await
            .expect("a side question streams");
        let mut text = String::new();
        while let Some(event) = answered.next().await {
            if let Ok(ModelEvent::TextDelta { delta, .. }) = event {
                text.push_str(&delta);
            }
        }
        assert_eq!(text, "", "nothing to say");

        let mut turn = provider
            .stream(conversation, CancellationToken::new())
            .await
            .expect("the conversation streams");
        let mut text = String::new();
        while let Some(event) = turn.next().await {
            if let Ok(ModelEvent::TextDelta { delta, .. }) = event {
                text.push_str(&delta);
            }
        }
        assert_eq!(
            text, "the turn's answer",
            "the script's first response is still the turn's"
        );
    }
}

#[cfg(test)]
mod addressed_tests {
    use super::*;
    use bingo_sdk::{Message, ProviderMetadata};
    use futures::StreamExt;

    fn request(text: &str) -> ModelRequest {
        ModelRequest {
            model: FAKE_MODEL.into(),
            max_tokens: 16,
            system: Vec::new(),
            messages: vec![Message::text(Role::User, text)],
            tools: Vec::new(),
            reasoning: None,
            session: None,
            provider_options: ProviderMetadata::new(),
        }
    }

    fn addressed(needle: &str, says: &str) -> Response {
        Response {
            steps: vec![Step::Text(says.into())],
            finish: None,
            when: Some(Match::Contains(needle.into())),
        }
    }

    fn open(says: &str) -> Response {
        Response {
            steps: vec![Step::Text(says.into())],
            finish: None,
            when: None,
        }
    }

    fn script(responses: Vec<Response>) -> Script {
        Script {
            responses,
            side: Vec::new(),
        }
    }

    /// What one `stream()` call said, and the id of the response it came from —
    /// `fake-<index>`, so a test can see where in the script it was taken from.
    async fn answer(provider: &FakeProvider, asks: ModelRequest) -> (String, Option<String>) {
        let mut stream = provider
            .stream(asks, CancellationToken::new())
            .await
            .expect("a response");
        let (mut said, mut from) = (String::new(), None);
        while let Some(event) = stream.next().await {
            match event {
                Ok(ModelEvent::TextDelta { delta, .. }) => said.push_str(&delta),
                Ok(ModelEvent::ResponseMetadata { id, .. }) => from = id,
                _ => {}
            }
        }
        (said, from)
    }

    /// The defect `when` exists for: one script serves every session of a run,
    /// so a response written for one of them is spent on whichever asks next.
    /// Addressed, it survives being asked for out of order — and the ids still
    /// come from where the response sits in the script, not from when it went.
    #[tokio::test]
    async fn an_addressed_response_waits_for_its_own_asker() {
        let provider = FakeProvider::new(script(vec![
            addressed("seat the relay", "the parent's tail"),
            addressed("in #relay]", "the member's count"),
        ]));

        assert_eq!(
            answer(&provider, request("[from parent in #relay]\ncount to 3")).await,
            ("the member's count".into(), Some("fake-1".into())),
            "the member took the response written for it, not the one in front of it"
        );
        assert_eq!(
            answer(&provider, request("seat the relay and start it")).await,
            ("the parent's tail".into(), Some("fake-0".into())),
            "and the parent's was still waiting when the parent came for it"
        );
    }

    /// A script that addresses nothing is dealt straight down the list — which
    /// is every script written before `when` existed.
    #[tokio::test]
    async fn an_unaddressed_script_is_dealt_in_order() {
        let provider = FakeProvider::new(script(vec![open("first"), open("second")]));
        assert_eq!(answer(&provider, request("alice")).await.0, "first");
        assert_eq!(answer(&provider, request("bob")).await.0, "second");
    }

    /// The sharp edge of the rule: addressing protects only what carries a
    /// matcher. An open response in front of one still goes to whoever asks.
    #[tokio::test]
    async fn an_open_response_is_taken_by_whoever_asks_first() {
        let provider = FakeProvider::new(script(vec![
            open("anyone's"),
            addressed("parent", "the parent's"),
        ]));
        assert_eq!(
            answer(&provider, request("parent")).await.0,
            "anyone's",
            "the open response was in front, so the parent took that"
        );
    }

    /// `when` reads the whole request, not the last thing said into it: a
    /// session is known by anything in its transcript, a tool result included.
    #[tokio::test]
    async fn a_matcher_reads_tool_calls_and_their_results() {
        let provider = FakeProvider::new(script(vec![addressed("is seated and idle", "go on")]));
        let mut asks = request("seat one");
        asks.messages
            .push(Message::assistant(vec![ContentPart::ToolUse {
                id: "call_0".into(),
                name: "SpawnAgent".into(),
                input: serde_json::json!({ "name": "alpha" }),
            }]));
        asks.messages
            .push(Message::user(vec![ContentPart::ToolResult {
                tool_use_id: "call_0".into(),
                parts: vec![ContentPart::text("alpha is seated and idle")],
                is_error: false,
            }]));

        assert_eq!(answer(&provider, asks).await.0, "go on");
    }

    /// A request nothing left is addressed to is told exactly that, instead of
    /// being handed a response written for somebody else.
    #[tokio::test]
    async fn a_request_matching_nothing_left_is_refused() {
        let provider = FakeProvider::new(script(vec![addressed("parent", "the parent's")]));
        let refused = provider
            .stream(request("a member"), CancellationToken::new())
            .await
            .err();
        let Some(ProviderError::Request { message }) = refused else {
            panic!("nothing here is addressed to a member: {refused:?}");
        };
        assert!(message.contains("1 response(s) left"), "{message}");
        assert!(message.contains("none addressed to it"), "{message}");
    }

    /// A spent script still says the one thing every script test knows.
    #[tokio::test]
    async fn a_spent_script_is_exhausted() {
        let provider = FakeProvider::new(script(vec![open("only")]));
        answer(&provider, request("first")).await;
        assert_eq!(
            provider
                .stream(request("second"), CancellationToken::new())
                .await
                .err(),
            Some(ProviderError::Request {
                message: "script exhausted".into()
            })
        );
    }

    /// The side list is dealt by the same rule, so two plugins asking beside
    /// one conversation are not the same race the conversation just lost.
    #[tokio::test]
    async fn a_side_question_takes_the_side_response_addressed_to_it() {
        let provider = FakeProvider::new(Script {
            responses: Vec::new(),
            side: vec![
                addressed("what did we learn", "a fact"),
                addressed("what is it called", "a name"),
            ],
        });
        let mut asks = request("what is it called?");
        let mut about = serde_json::Map::new();
        about.insert("purpose".into(), Value::String("naming".into()));
        asks.provider_options.insert("bingo".into(), about);

        assert_eq!(answer(&provider, asks).await.0, "a name");
    }

    /// The field is optional: a script written before `when` existed parses
    /// unchanged, and one that sets none serialises without mentioning it.
    #[test]
    fn when_is_optional_in_the_script_schema() {
        let plain = Script::from_json(r#"{"responses":[{"steps":[{"text":"hi"}]}]}"#)
            .expect("a script without `when` parses");
        assert_eq!(plain.responses[0].when, None);
        let json = serde_json::to_string(&plain).expect("serialize");
        assert!(!json.contains("when"), "{json}");

        let addressed = Script::from_json(
            r#"{"responses":[{"steps":[{"text":"hi"}],"when":{"contains":"a"}}]}"#,
        )
        .expect("a script with `when` parses");
        assert_eq!(
            addressed.responses[0].when,
            Some(Match::Contains("a".into()))
        );
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
            session: None,
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
                when: None,
            }],

            side: Vec::new(),
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
                when: None,
            }],

            side: Vec::new(),
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
                when: None,
            }],

            side: Vec::new(),
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
                when: None,
            }],

            side: Vec::new(),
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
                when: None,
            }],

            side: Vec::new(),
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
                when: None,
            }],

            side: Vec::new(),
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
        assert!(provider.endpoint(FAKE_MODEL).images);
        assert!(!provider.endpoint(FAKE_MODEL).caching);
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
                    when: None,
                },
                Response::default(),
            ],

            side: Vec::new(),
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
        let mut registrar = Registrar::new(
            "bingo.provider.fake",
            Value::Null,
            bingo_sdk::Env::rooted("/tmp"),
        );
        plugin.register(&mut registrar).expect("register");
        let contributions = registrar.into_contributions();
        assert_eq!(contributions.len(), 1);
        assert!(matches!(contributions[0], Contribution::Provider(_)));
        assert_eq!(plugin.manifest().provides, &["provider:fake"]);
        assert_eq!(plugin.provider().id(), "fake");
    }
}
