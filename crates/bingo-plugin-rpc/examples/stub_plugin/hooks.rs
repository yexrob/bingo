//! The hooks this stub declares, and what it does when one is asked.
//!
//! One at each kind of point: `guard` decides about one tool by name and
//! rewrites the call it lets through, `prefix` rewrites what was typed,
//! `watch` only watches, and `silent` is asked and never answers — which is
//! what a hook past its deadline looks like from the host's side.
//!
//! Every crossing is written into the map the `kv` service serves, so a test
//! reads what arrived out of this process's own memory — and reads nothing
//! where the matcher kept an event off the pipe.

use std::collections::BTreeMap;

use bingo_plugin_rpc::codec::{INVALID_PARAMS, RpcError};
use bingo_plugin_rpc::wire::{
    HookDecideParams, HookDecideResult, HookDecision, HookObservation, HookObserveParams, HookSpec,
    HookValue,
};
use bingo_sdk::{HookMatcher, HookOutcome, HookPoint, Input, ToolCall};
use serde_json::{Value, json};

use crate::{answer, fail};

/// What this process has written down, which is also what `kv` serves.
type Store = BTreeMap<String, String>;

/// The hooks this stub declares, one for each thing a test needs to see: a
/// `beforeTool` hook that claims one tool name, a `submit` hook that rewrites
/// what was typed, a watcher at two observation points, and one that is asked
/// and never answers.
pub fn hooks() -> Vec<HookSpec> {
    vec![
        claims("guard", vec![HookPoint::BeforeTool], Some("Bash")),
        claims("prefix", vec![HookPoint::Submit], None),
        claims("watch", vec![HookPoint::Session, HookPoint::Turn], None),
        claims("silent", vec![HookPoint::Stop], None),
    ]
}

fn claims(id: &str, points: Vec<HookPoint>, tool: Option<&str>) -> HookSpec {
    HookSpec {
        id: id.to_string(),
        matcher: HookMatcher {
            points,
            tool: tool.map(str::to_string),
        },
    }
}

/// One decision point. What crossed is written down first — into the same map
/// the `kv` service serves, so a test reads it out of this process's own
/// memory — and then whichever hook owns the id answers. `silent` never does,
/// which is what a hook past its deadline looks like from the host's side.
pub fn decide(id: i64, params: Value, store: &mut Store) {
    let Ok(params) = serde_json::from_value::<HookDecideParams>(params) else {
        fail(id, RpcError::new(INVALID_PARAMS, "not a hook decision"));
        return;
    };
    note(&params.id, asked_at(&params.decision), store);
    match (params.id.as_str(), params.decision) {
        ("silent", _) => {}
        ("guard", HookDecision::BeforeTool { call }) => answer(id, guarded(call)),
        ("prefix", HookDecision::Submit { input }) => answer(id, prefixed(input)),
        _ => answer(id, decided(HookOutcome::Continue, None)),
    }
}

fn asked_at(decision: &HookDecision) -> &'static str {
    match decision {
        HookDecision::Submit { .. } => "submit",
        HookDecision::BeforeTool { .. } => "beforeTool",
        HookDecision::AfterTool { .. } => "afterTool",
        HookDecision::Stop => "stop",
    }
}

/// `guard`: a call whose input carries a `deny` is refused in the hook's own
/// words; anything else goes through with `--safe` on the end of it.
fn guarded(mut call: ToolCall) -> Value {
    if let Some(reason) = call.input.get("deny").and_then(Value::as_str) {
        return decided(
            HookOutcome::Deny {
                reason: reason.to_string(),
            },
            None,
        );
    }
    let command = call
        .input
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default();
    call.input = json!({ "command": format!("{command} --safe") });
    decided(HookOutcome::Continue, Some(HookValue::Call { call }))
}

/// `prefix`: the line as it was typed, with this stub's mark on the end.
fn prefixed(input: Input) -> Value {
    let Input::Text {
        text,
        images,
        origin,
    } = input
    else {
        return decided(HookOutcome::Continue, None);
    };
    let input = Input::Text {
        text: format!("{text} (stub)"),
        images,
        origin,
    };
    decided(HookOutcome::Continue, Some(HookValue::Input { input }))
}

fn decided(outcome: HookOutcome, value: Option<HookValue>) -> Value {
    serde_json::to_value(HookDecideResult { outcome, value }).unwrap_or(Value::Null)
}

/// One observation point: written down, and nothing sent back — the host is
/// not waiting.
pub fn observed(params: &Value, store: &mut Store) {
    let Ok(params) = serde_json::from_value::<HookObserveParams>(params.clone()) else {
        return;
    };
    note(&params.id, watched(&params.observation), store);
}

fn watched(observation: &HookObservation) -> &'static str {
    match observation {
        HookObservation::Turn { .. } => "turn",
        HookObservation::Compact { .. } => "compact",
        HookObservation::Session { .. } => "session",
        HookObservation::Event { .. } => "event",
    }
}

/// Every hook crossing, in the order it arrived, under the `hooks` key of the
/// map `kv` serves. A key that is still absent is a crossing that never
/// happened.
fn note(hook: &str, point: &str, store: &mut Store) {
    let seen = store.entry("hooks".to_string()).or_default();
    if !seen.is_empty() {
        seen.push(',');
    }
    seen.push_str(hook);
    seen.push('/');
    seen.push_str(point);
}
