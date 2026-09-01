//! A plugin process the bridge's own tests drive.
//!
//! It speaks the wire with this crate's envelope types, so the double cannot
//! drift from the contract it stands for. `cargo test` builds a crate's
//! examples, so the binary is always beside the test binary.
//!
//! One tool, `echo`, whose input says what it should do: answer, send progress
//! first, read an environment variable, wait for a `tool/cancel`, or end the
//! process without answering at all — which is what a killed plugin looks like
//! from the host's side. One command, `stub`, one contributor, `notes`, and
//! one compaction strategy, `cut`. `--protocol N` answers the handshake with a
//! major this host does not speak; `--placement <kind>` declares a placement
//! that may not be one.

use std::io::{BufRead, Write};
use std::process::ExitCode;

use bingo_plugin_rpc::codec::{
    INVALID_PARAMS, METHOD_NOT_FOUND, Message, Request, Response, RpcError,
};
use bingo_plugin_rpc::wire::{
    CommandCompleteParams, CommandCompleteResult, CommandRunParams, CommandRunResult,
    CompactorCompactParams, CompactorCompactResult, CompactorSpec, ContextContributeParams,
    ContextContributeResult, ContributorSpec, InitializeResult, PROTOCOL, ToolCallParams,
    ToolCallResult, ToolCancelParams, ToolProgressParams, name,
};
use bingo_sdk::{
    ArgSpec, CommandOutcome, CommandSpec, CompactReason, Compaction, Completion, ContentPart,
    ContextPiece, ItemId, Placement, ToolOutput, ToolSpec, Usage,
};
use serde_json::{Value, json};

/// Calls waiting for a `tool/cancel` before they answer.
type Held = Vec<(String, i64)>;

/// What this run of the stub says about itself.
struct Options {
    protocol: u32,
    /// The placement the contributor is declared with, as it is written on
    /// the wire; a kind that is not one is refused by the host in words.
    placement: Value,
}

fn main() -> ExitCode {
    let options = options();
    let mut held: Held = Vec::new();
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            break;
        };
        if !serve(&line, &options, &mut held) {
            break;
        }
    }
    ExitCode::SUCCESS
}

/// `--protocol N` for the handshake this host must refuse, `--placement kind`
/// for the declaration it must refuse.
fn options() -> Options {
    let mut options = Options {
        protocol: PROTOCOL,
        placement: json!({ "kind": "roundStart" }),
    };
    let mut args = std::env::args().skip(1);
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
fn serve(line: &str, options: &Options, held: &mut Held) -> bool {
    match serde_json::from_str::<Message>(line) {
        Ok(Message::Request(request)) => request_line(request, options, held),
        Ok(Message::Notification(notification)) => {
            cancel(&notification.params, held);
            true
        }
        _ => true,
    }
}

fn request_line(request: Request, options: &Options, held: &mut Held) -> bool {
    match request.method.as_str() {
        name::INITIALIZE => answer(request.id, handshake(options)),
        name::TOOL_CALL => return call(request.id, request.params, held),
        name::COMMAND_RUN => answer(request.id, run(request.params)),
        name::COMMAND_COMPLETE => answer(request.id, complete(request.params)),
        name::CONTEXT_CONTRIBUTE => answer(request.id, contribute(request.params)),
        name::COMPACTOR_COMPACT => answer(request.id, compact(request.params)),
        other => fail(
            request.id,
            RpcError::new(METHOD_NOT_FOUND, format!("no such method: {other}")),
        ),
    }
    true
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
    };
    let mut declared = serde_json::to_value(result).unwrap_or(Value::Null);
    // Written last, over the typed one: the point of `--placement` is to say
    // what this crate's own types cannot.
    declared["contributors"][0]["placement"] = options.placement.clone();
    declared
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
fn call(id: i64, params: Value, held: &mut Held) -> bool {
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
        held.push((params.call_id, id));
        return true;
    }
    answer(id, output(text(&params.input)));
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
fn text(input: &Value) -> String {
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

fn cancel(params: &Value, held: &mut Held) {
    let Ok(params) = serde_json::from_value::<ToolCancelParams>(params.clone()) else {
        return;
    };
    let Some(at) = held.iter().position(|(call, _)| call == &params.call_id) else {
        return;
    };
    let (_, id) = held.remove(at);
    answer(id, output("cancelled".to_string()));
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
