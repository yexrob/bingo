//! A plugin process the bridge's own tests drive.
//!
//! It speaks the wire with this crate's envelope types, so the double cannot
//! drift from the contract it stands for. `cargo test` builds a crate's
//! examples, so the binary is always beside the test binary.
//!
//! One tool, `echo`, whose input says what it should do: answer, send progress
//! first, read an environment variable, wait for a `tool/cancel`, say which
//! streams have been cancelled, ask the host for a service call, or end the
//! process without answering at all — which is what a killed plugin looks like
//! from the host's side. One command, `stub`, one contributor, `notes`, one
//! compaction strategy, `cut`, and one provider, `stub`, serving `stub-1`:
//! what it streams is decided by the last user text in the request, so a test
//! scripts a model by writing to it. One service, `kv`, speaking `set` and
//! `get` over a map this process keeps, so two of these pair through the host.
//! `--protocol N` answers the handshake with a major this host does not speak;
//! `--placement <kind>` declares a placement that may not be one;
//! `--no-service` declares no service at all, which is what a caller is.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::process::ExitCode;

use bingo_plugin_rpc::codec::{
    INVALID_PARAMS, METHOD_NOT_FOUND, Message, Outcome, Request, Response, RpcError,
};
use bingo_plugin_rpc::wire::{
    CommandCompleteParams, CommandCompleteResult, CommandRunParams, CommandRunResult,
    CompactorCompactParams, CompactorCompactResult, CompactorSpec, ContextContributeParams,
    ContextContributeResult, ContributorSpec, InitializeResult, PROTOCOL, ProviderCancelParams,
    ProviderDeltaParams, ProviderSpec, ProviderStreamParams, ProviderStreamResult,
    ServiceCallParams, ServiceCallResult, ServiceSpec, ToolCallParams, ToolCallResult,
    ToolCancelParams, ToolProgressParams, name,
};
use bingo_sdk::{
    ArgSpec, CommandOutcome, CommandSpec, CompactReason, Compaction, Completion, ContentPart,
    ContextPiece, EndpointCapabilities, FinishReason, ItemId, ModelEvent, ModelInfo, ModelRequest,
    Placement, ProviderError, Role, ToolOutput, ToolSpec, UnifiedFinish, Usage,
};
use serde_json::{Value, json};

mod hooks;

/// Everything this run of the stub is holding: calls and streams waiting for
/// a cancel before they answer, the streams that were cancelled — which the
/// `echo` tool reads out, so a test can see that a cancel crossed the pipe at
/// all — the little map the `kv` service keeps, and the ids this process
/// mints for the requests it sends the host.
#[derive(Default)]
struct State {
    calls: Vec<(String, i64)>,
    streams: Vec<(String, i64)>,
    cancelled: Vec<String>,
    store: BTreeMap<String, String>,
    /// Service calls this process asked the host for, each with the tool call
    /// that is waiting to say what came back.
    pending: Vec<(i64, i64)>,
    asked: i64,
}

impl State {
    /// This process's own request ids. The host's ids are its own; a response
    /// is told from a request by its shape, never by the number.
    fn next_id(&mut self) -> i64 {
        self.asked += 1;
        self.asked
    }
}

/// What this run of the stub says about itself.
struct Options {
    protocol: u32,
    /// The placement the contributor is declared with, as it is written on
    /// the wire; a kind that is not one is refused by the host in words.
    placement: Value,
    /// Whether it declares the `kv` service. A caller declares none: one key
    /// has one owner, and the second to claim it is refused.
    serves: bool,
}

fn main() -> ExitCode {
    let options = options();
    let mut state = State::default();
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            break;
        };
        if !serve(&line, &options, &mut state) {
            break;
        }
    }
    ExitCode::SUCCESS
}

/// `--protocol N` for the handshake this host must refuse, `--placement kind`
/// for the declaration it must refuse, `--no-service` for a run that serves
/// nothing and only calls.
fn options() -> Options {
    let mut options = Options {
        protocol: PROTOCOL,
        placement: json!({ "kind": "roundStart" }),
        serves: true,
    };
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    options.serves = !args.iter().any(|arg| arg == "--no-service");
    args.retain(|arg| arg != "--no-service");
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match (arg.as_str(), args.next()) {
            ("--protocol", Some(n)) => options.protocol = n.parse().unwrap_or(PROTOCOL),
            ("--placement", Some(kind)) => options.placement = json!({ "kind": kind }),
            _ => {}
        }
    }
    options
}

/// Whether to keep reading.
fn serve(line: &str, options: &Options, state: &mut State) -> bool {
    match serde_json::from_str::<Message>(line) {
        Ok(Message::Request(request)) => request_line(request, options, state),
        Ok(Message::Notification(notification)) => {
            notification_line(&notification.method, &notification.params, state);
            true
        }
        Ok(Message::Response(response)) => {
            came_back(response, state);
            true
        }
        Err(_) => true,
    }
}

fn request_line(request: Request, options: &Options, state: &mut State) -> bool {
    match request.method.as_str() {
        name::INITIALIZE => answer(request.id, handshake(options)),
        name::TOOL_CALL => return call(request.id, request.params, state),
        name::COMMAND_RUN => answer(request.id, run(request.params)),
        name::COMMAND_COMPLETE => answer(request.id, complete(request.params)),
        name::CONTEXT_CONTRIBUTE => answer(request.id, contribute(request.params)),
        name::COMPACTOR_COMPACT => answer(request.id, compact(request.params)),
        name::PROVIDER_STREAM => return stream(request.id, request.params, state),
        name::SERVICE_CALL => served(request.id, request.params, state),
        name::HOOK_DECIDE => hooks::decide(request.id, request.params, &mut state.store),
        other => fail(
            request.id,
            RpcError::new(METHOD_NOT_FOUND, format!("no such method: {other}")),
        ),
    }
    true
}

fn notification_line(method: &str, params: &Value, state: &mut State) {
    match method {
        name::TOOL_CANCEL => cancel(params, state),
        name::PROVIDER_CANCEL => cancel_stream(params, state),
        name::HOOK_OBSERVE => hooks::observed(params, &mut state.store),
        _ => {}
    }
}

fn handshake(options: &Options) -> Value {
    let result = InitializeResult {
        protocol: options.protocol,
        name: "stub".into(),
        version: "0.1.0".into(),
        tools: vec![ToolSpec {
            name: "echo".into(),
            description: "Says back what it was given.".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "text": { "type": "string" } }
            }),
            meta: serde_json::Map::new(),
        }],
        commands: vec![CommandSpec {
            name: "stub".into(),
            aliases: Vec::new(),
            hint: "say something back".into(),
            args: ArgSpec::Free {
                hint: "<text>".into(),
            },
            instant: true,
            family: "plugin".into(),
        }],
        contributors: vec![ContributorSpec {
            id: "notes".into(),
            placement: Placement::RoundStart,
        }],
        compactors: vec![CompactorSpec { id: "cut".into() }],
        providers: vec![ProviderSpec {
            id: "stub".into(),
            family: None,
            models: vec![ModelInfo {
                id: "stub-1".into(),
                display: Some("Stub One".into()),
            }],
            endpoint: EndpointCapabilities {
                images: true,
                count_tokens: false,
                caching: false,
            },
        }],
        hooks: hooks::hooks(),
        services: declared(options),
    };
    let mut declared = serde_json::to_value(result).unwrap_or(Value::Null);
    // Written last, over the typed one: the point of `--placement` is to say
    // what this crate's own types cannot.
    declared["contributors"][0]["placement"] = options.placement.clone();
    declared
}

/// `kv`, and the two methods it speaks. A run that serves nothing declares
/// nothing: one key has one owner, so a caller must not claim it too.
fn declared(options: &Options) -> BTreeMap<String, ServiceSpec> {
    if !options.serves {
        return BTreeMap::new();
    }
    let methods = [
        (
            "set",
            json!({ "type": "object", "required": ["key", "value"] }),
        ),
        ("get", json!({ "type": "object", "required": ["key"] })),
    ];
    BTreeMap::from([(
        "kv".to_string(),
        ServiceSpec {
            methods: methods
                .into_iter()
                .map(|(name, schema)| (name.to_string(), schema))
                .collect(),
        },
    )])
}

/// The host asking this process for its own service: `set {key, value}` writes
/// and answers nothing, `get {key}` answers what was written or nothing.
fn served(id: i64, params: Value, state: &mut State) {
    let Ok(params) = serde_json::from_value::<ServiceCallParams>(params) else {
        fail(id, RpcError::new(INVALID_PARAMS, "not a service call"));
        return;
    };
    let key = params.params["key"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let result = match params.method.as_str() {
        "set" => {
            let value = params.params["value"].as_str().unwrap_or_default();
            state.store.insert(key, value.to_string());
            Value::Null
        }
        "get" => state
            .store
            .get(&key)
            .map(|v| json!(v))
            .unwrap_or(Value::Null),
        other => {
            fail(
                id,
                RpcError::new(METHOD_NOT_FOUND, format!("kv does not speak {other}")),
            );
            return;
        }
    };
    answer(
        id,
        serde_json::to_value(ServiceCallResult { result }).unwrap_or(Value::Null),
    );
}

/// This process asking the host for a service call. The tool call that wanted
/// it stays open until the answer arrives: one line is read at a time here, so
/// nothing may wait inside one.
fn crossed(asked: &Value, call: i64, state: &mut State) {
    let id = state.next_id();
    state.pending.push((id, call));
    send(&Message::Request(Request::new(
        id,
        name::SERVICE_CALL,
        asked.clone(),
    )));
}

/// The host answered a service call this process asked for, so the tool call
/// waiting on it can say what came back: the result, or the host's refusal in
/// its own words — which is how a test reads a refusal from the far side.
fn came_back(response: Response, state: &mut State) {
    let Some(at) = state
        .pending
        .iter()
        .position(|(asked, _)| Some(*asked) == response.id)
    else {
        return;
    };
    let (_, call) = state.pending.remove(at);
    let said = match response.outcome {
        Outcome::Result(value) => serde_json::from_value::<ServiceCallResult>(value)
            .map(|answered| said_of(&answered.result))
            .unwrap_or_else(|error| error.to_string()),
        Outcome::Error(error) => error.message,
    };
    answer(call, output(said));
}

/// A string as itself, anything else as the JSON it is: a tool answers text.
fn said_of(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}

/// `notes`: one user piece that says what the round it was asked about held,
/// so a test can see the query's projection arrive whole.
fn contribute(params: Value) -> Value {
    let Ok(params) = serde_json::from_value::<ContextContributeParams>(params) else {
        return Value::Null;
    };
    let said = format!(
        "{}: round {} of {} with {} items",
        params.id,
        params.query.round,
        params.query.session.id,
        params.query.items.len()
    );
    serde_json::to_value(ContextContributeResult {
        pieces: vec![ContextPiece::User {
            parts: vec![ContentPart::text(said)],
            label: params.id,
        }],
    })
    .unwrap_or(Value::Null)
}

/// `cut`: a compaction that keeps nothing and claims to halve the context.
fn compact(params: Value) -> Value {
    let Ok(params) = serde_json::from_value::<CompactorCompactParams>(params) else {
        return Value::Null;
    };
    let used = params.context.usage.used;
    serde_json::to_value(CompactorCompactResult {
        compaction: Compaction {
            summary: format!("{} cut on {}", params.id, why(&params.reason)),
            boundary: params
                .context
                .items
                .first()
                .map(|item| item.id.clone())
                .unwrap_or_else(|| ItemId::from_raw("itm_none")),
            kept: Vec::new(),
            before: used,
            after: used / 2,
            usage: Usage::default(),
        },
    })
    .unwrap_or(Value::Null)
}

fn why(reason: &CompactReason) -> &'static str {
    match reason {
        CompactReason::Threshold => "threshold",
        CompactReason::Overflow { .. } => "overflow",
        CompactReason::Manual { .. } => "request",
    }
}

/// Whether to keep reading: an input of `{"die": true}` ends the process
/// without answering, which is what a killed plugin looks like from outside.
fn call(id: i64, params: Value, state: &mut State) -> bool {
    let Ok(params) = serde_json::from_value::<ToolCallParams>(params) else {
        fail(id, RpcError::new(INVALID_PARAMS, "not a tool call"));
        return true;
    };
    if params.input.get("die").is_some() {
        return false;
    }
    for tail in progress_lines(&params.input) {
        notify(&params.call_id, &tail);
    }
    if params.input.get("awaitCancel").is_some() {
        state.calls.push((params.call_id, id));
        return true;
    }
    if params.input.get("cancelled").is_some() {
        answer(id, output(state.cancelled.join(",")));
        return true;
    }
    if let Some(asked) = params.input.get("call").cloned() {
        crossed(&asked, id, state);
        return true;
    }
    answer(id, output(said(&params.input)));
    true
}

fn progress_lines(input: &Value) -> Vec<String> {
    input
        .get("progress")
        .and_then(Value::as_array)
        .map(|lines| {
            lines
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// What the call answers with: a named environment variable, or the text.
fn said(input: &Value) -> String {
    if let Some(key) = input.get("env").and_then(Value::as_str) {
        return std::env::var(key).unwrap_or_default();
    }
    input
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn output(text: String) -> Value {
    serde_json::to_value(ToolCallResult {
        output: ToolOutput::text(text),
    })
    .unwrap_or(Value::Null)
}

fn cancel(params: &Value, state: &mut State) {
    let Ok(params) = serde_json::from_value::<ToolCancelParams>(params.clone()) else {
        return;
    };
    let Some(at) = state
        .calls
        .iter()
        .position(|(call, _)| call == &params.call_id)
    else {
        return;
    };
    let (_, id) = state.calls.remove(at);
    answer(id, output("cancelled".to_string()));
}

/// A stream the host let go of: written down, so the `echo` tool can say the
/// cancel arrived, and closed if it was one this process was holding open.
fn cancel_stream(params: &Value, state: &mut State) {
    let Ok(params) = serde_json::from_value::<ProviderCancelParams>(params.clone()) else {
        return;
    };
    state.cancelled.push(params.call.clone());
    if let Some(at) = state
        .streams
        .iter()
        .position(|(call, _)| call == &params.call)
    {
        let (_, id) = state.streams.remove(at);
        close(id, None);
    }
}

/// `provider/stream`: the deltas, then the close. The last user text says what
/// this response is — `hold` answers nothing until it is cancelled, `die` ends
/// the process mid-stream, `fail` closes with the error the trait speaks, and
/// anything else is a response. Whether to keep reading.
fn stream(id: i64, params: Value, state: &mut State) -> bool {
    let Ok(params) = serde_json::from_value::<ProviderStreamParams>(params) else {
        fail(id, RpcError::new(INVALID_PARAMS, "not a stream"));
        return true;
    };
    let said = asked(&params.request);
    match said.as_str() {
        "hold" => state.streams.push((params.call, id)),
        "die" => {
            delta(&params.call, ModelEvent::TextStart { id: "b1".into() });
            return false;
        }
        "fail" => close(
            id,
            Some(ProviderError::RateLimited {
                retry_after_ms: Some(1_500),
            }),
        ),
        _ => {
            for event in response(&params.request, &said) {
                delta(&params.call, event);
            }
            close(id, None);
        }
    }
    true
}

/// What the stub answers with: the tool call the request's tools invite, the
/// word `done` once a result has come back, and the text it was given
/// otherwise.
fn response(request: &ModelRequest, said: &str) -> Vec<ModelEvent> {
    if answered(request) {
        return text("done");
    }
    match request.tools.first() {
        Some(tool) => vec![
            ModelEvent::ToolCall {
                id: "c1".into(),
                name: tool.name.clone(),
                input: json!({ "text": said }).to_string(),
            },
            finish(UnifiedFinish::ToolCalls),
        ],
        None => text(said),
    }
}

fn text(said: &str) -> Vec<ModelEvent> {
    let mut events = vec![ModelEvent::TextStart { id: "b1".into() }];
    events.extend(said.split_whitespace().map(|word| ModelEvent::TextDelta {
        id: "b1".into(),
        delta: word.to_string(),
    }));
    events.push(ModelEvent::TextEnd { id: "b1".into() });
    events.push(finish(UnifiedFinish::Stop));
    events
}

fn finish(unified: UnifiedFinish) -> ModelEvent {
    ModelEvent::Finish {
        usage: Usage {
            input_tokens: 10,
            output_tokens: 3,
            ..Usage::default()
        },
        finish_reason: FinishReason::unified(unified),
    }
}

/// The last thing the person said, which is this stub's whole script.
fn asked(request: &ModelRequest) -> String {
    request
        .messages
        .iter()
        .rev()
        .find(|message| message.role == Role::User)
        .and_then(|message| message.parts.iter().find_map(ContentPart::as_text))
        .unwrap_or_default()
        .to_string()
}

/// Whether a tool result has already come back, which is what makes a round
/// trip a round trip.
fn answered(request: &ModelRequest) -> bool {
    request.messages.iter().any(|message| {
        message
            .parts
            .iter()
            .any(|part| matches!(part, ContentPart::ToolResult { .. }))
    })
}

fn delta(call: &str, event: ModelEvent) {
    let params = ProviderDeltaParams {
        call: call.to_string(),
        event,
    };
    let Ok(params) = serde_json::to_value(params) else {
        return;
    };
    send(&Message::Notification(
        bingo_plugin_rpc::codec::Notification::new(name::PROVIDER_DELTA, params),
    ));
}

fn close(id: i64, error: Option<ProviderError>) {
    let Ok(result) = serde_json::to_value(ProviderStreamResult { error }) else {
        return;
    };
    answer(id, result);
}

fn run(params: Value) -> Value {
    let Ok(params) = serde_json::from_value::<CommandRunParams>(params) else {
        return Value::Null;
    };
    serde_json::to_value(CommandRunResult {
        outcome: CommandOutcome::Applied {
            message: Some(format!("stub in {}: {}", params.cwd.display(), params.args)),
        },
    })
    .unwrap_or(Value::Null)
}

fn complete(params: Value) -> Value {
    let Ok(params) = serde_json::from_value::<CommandCompleteParams>(params) else {
        return Value::Null;
    };
    serde_json::to_value(CommandCompleteResult {
        completions: vec![Completion {
            value: format!("{}-one", params.partial),
            label: None,
        }],
    })
    .unwrap_or(Value::Null)
}

fn notify(call_id: &str, tail: &str) {
    let params = ToolProgressParams {
        call_id: call_id.to_string(),
        tail: tail.to_string(),
    };
    let Ok(params) = serde_json::to_value(params) else {
        return;
    };
    send(&Message::Notification(
        bingo_plugin_rpc::codec::Notification::new(name::TOOL_PROGRESS, params),
    ));
}

fn answer(id: i64, result: Value) {
    send(&Message::Response(Response::ok(id, result)));
}

fn fail(id: i64, error: RpcError) {
    send(&Message::Response(Response::failed(Some(id), error)));
}

fn send(message: &Message) {
    let Ok(line) = message.line() else {
        return;
    };
    let mut out = std::io::stdout().lock();
    if out.write_all(line.as_bytes()).is_ok() && out.write_all(b"\n").is_ok() {
        let _ = out.flush();
    }
}
