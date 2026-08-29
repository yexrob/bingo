//! What a hook reads on stdin: one JSON object per event.
//!
//! Every shape is a snapshot fixture, so a wording change is a deliberate diff
//! and never a silent break of somebody's script. The three fields every event
//! carries are `hook_event_name`, `session_id` and `cwd`; the rest is the event's
//! own. Two fields Claude Code sends are absent here and cannot be faked:
//! `transcript_path` (bingo's journal is not a Claude Code transcript, and the
//! hook context carries no path to it) and `permission_mode` (the mode lives in
//! the permissions plugin, and one plugin may not read another's state).

use std::path::Path;

use bingo_sdk::{HookContext, Level, SessionId, ToolCall, ToolOutput};
use serde_json::{Map, Value, json};

use crate::config::HookEvent;

/// The session a hook is being run for.
#[derive(Clone, Copy, Debug)]
pub struct Common<'a> {
    pub session: &'a SessionId,
    pub cwd: &'a Path,
    /// The session's model, when the kernel knows one.
    pub model: Option<&'a str>,
}

pub fn common(cx: &HookContext) -> Common<'_> {
    Common {
        session: &cx.session,
        cwd: &cx.cwd,
        model: cx.model.as_deref(),
    }
}

fn base(common: Common<'_>, event: HookEvent) -> Map<String, Value> {
    let mut object = Map::new();
    object.insert("hook_event_name".into(), json!(event.name()));
    object.insert("session_id".into(), json!(common.session.as_str()));
    object.insert("cwd".into(), json!(common.cwd.to_string_lossy()));
    object
}

fn with(common: Common<'_>, event: HookEvent, fields: Value) -> Value {
    let mut object = base(common, event);
    if let Value::Object(fields) = fields {
        object.extend(fields);
    }
    Value::Object(object)
}

/// The call as the hook may still rewrite it.
pub fn pre_tool_use(common: Common<'_>, call: &ToolCall) -> Value {
    with(
        common,
        HookEvent::PreToolUse,
        json!({
            "tool_name": call.name,
            "tool_input": call.input,
            "tool_use_id": call.call_id,
        }),
    )
}

/// The permission the gate opened, as an observer sees it. `tool_input` is not
/// available here — the kernel hands `on_event` a frame, not the call — so the
/// interaction's own one-line summary stands in its place.
pub fn permission_request(common: Common<'_>, tool: &str, summary: &str) -> Value {
    with(
        common,
        HookEvent::PermissionRequest,
        json!({"tool_name": tool, "summary": summary}),
    )
}

/// A call that succeeded. `tool_response` is bingo's `ToolOutput`, the only
/// representation of a result this process has.
pub fn post_tool_use(common: Common<'_>, call: &ToolCall, output: &ToolOutput) -> Value {
    with(
        common,
        HookEvent::PostToolUse,
        json!({
            "tool_name": call.name,
            "tool_input": call.input,
            "tool_use_id": call.call_id,
            "tool_response": response(output),
        }),
    )
}

/// A call that failed. Claude Code names the same object `tool_error` here.
pub fn post_tool_use_failure(common: Common<'_>, call: &ToolCall, output: &ToolOutput) -> Value {
    with(
        common,
        HookEvent::PostToolUseFailure,
        json!({
            "tool_name": call.name,
            "tool_input": call.input,
            "tool_use_id": call.call_id,
            "tool_error": response(output),
        }),
    )
}

fn response(output: &ToolOutput) -> Value {
    serde_json::to_value(output).unwrap_or(Value::Null)
}

pub fn user_prompt_submit(common: Common<'_>, prompt: &str) -> Value {
    with(
        common,
        HookEvent::UserPromptSubmit,
        json!({"prompt": prompt}),
    )
}

pub fn stop(common: Common<'_>) -> Value {
    with(common, HookEvent::Stop, Value::Null)
}

/// `trigger` is always `auto`: the kernel's `on_compact` says the phase and not
/// what asked for the cut, so claiming `manual` would be a guess.
pub fn pre_compact(common: Common<'_>) -> Value {
    with(common, HookEvent::PreCompact, json!({"trigger": "auto"}))
}

/// `source` is always `startup` for the same reason: `on_session(Start)` does not
/// say whether the session was opened fresh, resumed or forked.
pub fn session_start(common: Common<'_>) -> Value {
    let mut fields = json!({"source": "startup"});
    if let (Some(model), Value::Object(object)) = (common.model, &mut fields) {
        object.insert("model".into(), json!(model));
    }
    with(common, HookEvent::SessionStart, fields)
}

/// `end_reason` is always `other`: `on_session(End)` carries no close reason.
pub fn session_end(common: Common<'_>) -> Value {
    with(
        common,
        HookEvent::SessionEnd,
        json!({"end_reason": "other"}),
    )
}

/// A notice. `notification_type` carries bingo's notice code, which is what a
/// `Notification` matcher is written against.
pub fn notification(common: Common<'_>, level: Level, code: &str, text: &str) -> Value {
    with(
        common,
        HookEvent::Notification,
        json!({
            "notification_type": code,
            "level": level,
            "message": text,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_sdk::ContentPart;
    use std::path::PathBuf;

    fn common() -> (SessionId, PathBuf) {
        (
            SessionId::from_raw("ses_01"),
            PathBuf::from("/work/project"),
        )
    }

    fn fixture<'a>(session: &'a SessionId, cwd: &'a Path) -> Common<'a> {
        Common {
            session,
            cwd,
            model: Some("anthropic/claude-sonnet-4"),
        }
    }

    fn call() -> ToolCall {
        ToolCall {
            call_id: "call_01".into(),
            name: "Bash".into(),
            input: serde_json::json!({"command": "ls -a"}),
        }
    }

    #[test]
    fn pre_tool_use_shape() {
        let (session, cwd) = common();
        insta::assert_json_snapshot!(pre_tool_use(fixture(&session, &cwd), &call()));
    }

    #[test]
    fn post_tool_use_shape() {
        let (session, cwd) = common();
        let output = ToolOutput::text("a\nb\n");
        insta::assert_json_snapshot!(post_tool_use(fixture(&session, &cwd), &call(), &output));
    }

    #[test]
    fn post_tool_use_failure_shape() {
        let (session, cwd) = common();
        let output = ToolOutput::error("no such file");
        insta::assert_json_snapshot!(post_tool_use_failure(
            fixture(&session, &cwd),
            &call(),
            &output
        ));
    }

    #[test]
    fn permission_request_shape() {
        let (session, cwd) = common();
        insta::assert_json_snapshot!(permission_request(
            fixture(&session, &cwd),
            "Bash",
            "Bash(rm -rf build)"
        ));
    }

    #[test]
    fn user_prompt_submit_shape() {
        let (session, cwd) = common();
        insta::assert_json_snapshot!(user_prompt_submit(
            fixture(&session, &cwd),
            "write the tests"
        ));
    }

    #[test]
    fn stop_shape() {
        let (session, cwd) = common();
        insta::assert_json_snapshot!(stop(fixture(&session, &cwd)));
    }

    #[test]
    fn pre_compact_shape() {
        let (session, cwd) = common();
        insta::assert_json_snapshot!(pre_compact(fixture(&session, &cwd)));
    }

    #[test]
    fn session_start_shape() {
        let (session, cwd) = common();
        insta::assert_json_snapshot!(session_start(fixture(&session, &cwd)));
    }

    #[test]
    fn session_start_without_a_model_omits_it() {
        let (session, cwd) = common();
        let plain = Common {
            session: &session,
            cwd: &cwd,
            model: None,
        };
        insta::assert_json_snapshot!(session_start(plain));
    }

    #[test]
    fn session_end_shape() {
        let (session, cwd) = common();
        insta::assert_json_snapshot!(session_end(fixture(&session, &cwd)));
    }

    #[test]
    fn notification_shape() {
        let (session, cwd) = common();
        insta::assert_json_snapshot!(notification(
            fixture(&session, &cwd),
            Level::Warn,
            "TOOL_SHADOWED",
            "Echo2 is already a tool"
        ));
    }

    #[test]
    fn a_multipart_result_reaches_the_hook_whole() {
        let output = ToolOutput {
            parts: vec![ContentPart::text("one"), ContentPart::text("two")],
            is_error: false,
            display: None,
        };
        let value = response(&output);
        assert_eq!(
            value.pointer("/parts/1/text").and_then(Value::as_str),
            Some("two")
        );
    }
}
