//! A plugin process the bridge's own tests drive.
//!
//! It speaks the wire with this crate's envelope types, so the double cannot
//! drift from the contract it stands for. `cargo test` builds a crate's
//! examples, so the binary is always beside the test binary.
//!
//! One tool, `echo`, whose input says what it should do: answer, send progress
//! first, read an environment variable, wait for a `tool/cancel`, or end the
//! process without answering at all — which is what a killed plugin looks like
//! from the host's side. One command, `stub`. `--protocol N` answers the
//! handshake with a major this host does not speak.

use std::io::{BufRead, Write};
use std::process::ExitCode;

use bingo_plugin_rpc::codec::{
    INVALID_PARAMS, METHOD_NOT_FOUND, Message, Request, Response, RpcError,
};
use bingo_plugin_rpc::wire::{
    CommandCompleteParams, CommandCompleteResult, CommandRunParams, CommandRunResult,
    InitializeResult, PROTOCOL, ToolCallParams, ToolCallResult, ToolCancelParams,
    ToolProgressParams, name,
};
use bingo_sdk::{ArgSpec, CommandOutcome, CommandSpec, Completion, ToolOutput, ToolSpec};
use serde_json::{Value, json};

/// Calls waiting for a `tool/cancel` before they answer.
type Held = Vec<(String, i64)>;

fn main() -> ExitCode {
    let protocol = protocol();
    let mut held: Held = Vec::new();
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            break;
        };
        if !serve(&line, protocol, &mut held) {
            break;
        }
    }
    ExitCode::SUCCESS
}

/// `--protocol N`, for the handshake this host must refuse.
fn protocol() -> u32 {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--protocol" {
            return args.next().and_then(|n| n.parse().ok()).unwrap_or(PROTOCOL);
        }
    }
    PROTOCOL
}

/// Whether to keep reading.
fn serve(line: &str, protocol: u32, held: &mut Held) -> bool {
    match serde_json::from_str::<Message>(line) {
        Ok(Message::Request(request)) => request_line(request, protocol, held),
        Ok(Message::Notification(notification)) => {
            cancel(&notification.params, held);
            true
        }
        _ => true,
    }
}

fn request_line(request: Request, protocol: u32, held: &mut Held) -> bool {
    match request.method.as_str() {
        name::INITIALIZE => answer(request.id, handshake(protocol)),
        name::TOOL_CALL => return call(request.id, request.params, held),
        name::COMMAND_RUN => answer(request.id, run(request.params)),
        name::COMMAND_COMPLETE => answer(request.id, complete(request.params)),
        other => fail(
            request.id,
            RpcError::new(METHOD_NOT_FOUND, format!("no such method: {other}")),
        ),
    }
    true
}

fn handshake(protocol: u32) -> Value {
    let result = InitializeResult {
        protocol,
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
        contributors: Vec::new(),
        compactors: Vec::new(),
    };
    serde_json::to_value(result).unwrap_or(Value::Null)
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
