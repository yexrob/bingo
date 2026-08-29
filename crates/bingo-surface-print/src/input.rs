//! Claude Code's host protocol: what a host writes on stdin under
//! `--input-format stream-json`, and the control lines this surface writes
//! back. The counterpart of `stream_json`, which projects frames onto the
//! same dialect (ADR-0007 §8); like it, this is a compatibility layer, never a
//! second event model — the kernel does not know it exists.
//!
//! Parsing is pure: a line in, a `Line` or a `ParseError` out. A line this
//! module cannot read is a diagnostic on stderr, never the end of a run.
//!
//! Verified on 2026-08-29 against the current documentation and the SDK that
//! speaks this protocol:
//!
//! - `code.claude.com/docs/en/cli-reference` — `--input-format <text|stream-json>`
//!   ("Specify input format for print mode"), which the reference pairs with
//!   `-p`, and `--permission-prompt-tool`, which names the tool that answers
//!   permission prompts in non-interactive mode.
//! - `code.claude.com/docs/en/agent-sdk/streaming-vs-single-mode` — the input
//!   line: `{"type":"user","message":{"role":"user","content":<string|blocks>},
//!   "parent_tool_use_id":null}`, one JSON object per line. `session_id` travels
//!   with it and is ignored here: this run already knows its session, and a
//!   surface does not let a line choose one.
//! - `anthropics/claude-agent-sdk-python` (`types.py`, `_internal/query.py`) —
//!   the control protocol, which the docs describe but do not enumerate:
//!   `{"type":"control_request","request_id":…,"request":{"subtype":"interrupt"}}`
//!   from the host, answered with `{"type":"control_response","response":
//!   {"subtype":"success","request_id":…,"response":{}}}`; `can_use_tool` the
//!   other way, carrying `tool_name`, `input` and `permission_suggestions`, and
//!   answered by a `success` response whose payload is
//!   `{"behavior":"allow"|"deny","updatedInput":…?,"message":…?}`. The SDK sets
//!   `--permission-prompt-tool stdio` exactly when it will answer them.
//!
//! Shapes the protocol has that this surface has no way to honour are refused
//! rather than half-kept: an unknown `control_request` subtype (`initialize`,
//! `set_permission_mode`, …) is answered with an `error` response so a host
//! never waits for one that will not come, and `updatedPermissions` in a
//! verdict is ignored — the kernel installs a session rule from `AllowSession`,
//! which this protocol has no way to ask for.

use serde_json::{Value, json};

/// Where a run's prompts come from: `args.inputFormat`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Format {
    /// One prompt: the command-line argument, or the whole of stdin.
    #[default]
    Text,
    /// The host protocol, one JSON object per line, for as long as stdin is open.
    StreamJson,
}

impl Format {
    /// `args.inputFormat`, defaulting to text for anything unknown.
    pub(crate) fn from_args(args: &Value) -> Self {
        match args.get("inputFormat").and_then(Value::as_str) {
            Some("stream-json") => Format::StreamJson,
            _ => Format::Text,
        }
    }
}

/// `--permission-prompt-tool stdio`: the host answers permission prompts on
/// this protocol. Any other value names a tool this surface cannot call, and
/// is no reason to stop refusing prompts.
pub(crate) fn prompts_on_stdio(args: &Value) -> bool {
    args.get("permissionPromptTool").and_then(Value::as_str) == Some("stdio")
}

/// One line of the host protocol.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Line {
    /// A prompt: one turn.
    User { text: String },
    /// Stop the running turn, then acknowledge `request_id`.
    Interrupt { request_id: String },
    /// The host's verdict on a tool call this surface asked about.
    Decision {
        request_id: String,
        decision: Decision,
    },
    /// A control request with no answer here; refusing it is kinder than
    /// silence, which a host would wait out.
    Unsupported { request_id: String, subtype: String },
}

/// What a `control_response` says about a `can_use_tool` request. An `error`
/// subtype, a missing verdict and an unknown one are all denials: a tool runs
/// only when a host allowed it in as many words.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Decision {
    /// `updatedInput` travels with it, and the caller compares it with the call
    /// it asked about: the kernel runs the call the gate stopped, or none.
    Allow {
        updated_input: Option<Value>,
    },
    Deny {
        message: Option<String>,
    },
}

/// Why a line was not read. It reaches a person, never a host, so it says what
/// was wrong with the line rather than naming an error code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParseError(String);

impl ParseError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One line of stdin, as this surface reads it.
pub(crate) fn parse_line(line: &str) -> Result<Line, ParseError> {
    let value: Value =
        serde_json::from_str(line).map_err(|e| ParseError::new(format!("not JSON: {e}")))?;
    match value.get("type").and_then(Value::as_str) {
        Some("user") => user(&value),
        Some("control_request") => control_request(&value),
        Some("control_response") => control_response(&value),
        Some(other) => Err(ParseError::new(format!("unsupported line type `{other}`"))),
        None => Err(ParseError::new("a line with no `type`")),
    }
}

/// The text of a user message. The rest of it — images, the tool results a
/// host echoes back, `session_id`, `parent_tool_use_id` — is not a prompt.
fn user(value: &Value) -> Result<Line, ParseError> {
    let content = value
        .pointer("/message/content")
        .ok_or_else(|| ParseError::new("a user line with no `message.content`"))?;
    let text = match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(text_block)
            .collect::<Vec<_>>()
            .join("\n"),
        _ => {
            return Err(ParseError::new(
                "`message.content` is neither a string nor a list of blocks",
            ));
        }
    };
    if text.trim().is_empty() {
        return Err(ParseError::new("a user line with no text to submit"));
    }
    Ok(Line::User { text })
}

fn text_block(block: &Value) -> Option<&str> {
    (block.get("type").and_then(Value::as_str) == Some("text"))
        .then(|| block.get("text").and_then(Value::as_str))
        .flatten()
}

fn control_request(value: &Value) -> Result<Line, ParseError> {
    let request_id = string(value, "request_id")
        .ok_or_else(|| ParseError::new("a control request with no `request_id`"))?;
    let subtype = value
        .pointer("/request/subtype")
        .and_then(Value::as_str)
        .ok_or_else(|| ParseError::new("a control request with no `request.subtype`"))?;
    Ok(match subtype {
        "interrupt" => Line::Interrupt { request_id },
        other => Line::Unsupported {
            request_id,
            subtype: other.to_owned(),
        },
    })
}

fn control_response(value: &Value) -> Result<Line, ParseError> {
    let response = value
        .get("response")
        .ok_or_else(|| ParseError::new("a control response with no `response`"))?;
    let request_id = string(response, "request_id")
        .ok_or_else(|| ParseError::new("a control response with no `response.request_id`"))?;
    let decision = match response.get("subtype").and_then(Value::as_str) {
        Some("success") => verdict(response.get("response")),
        // A host that could not answer has not allowed anything.
        Some("error") => Decision::Deny {
            message: string(response, "error"),
        },
        _ => {
            return Err(ParseError::new(
                "a control response with no `response.subtype`",
            ));
        }
    };
    Ok(Line::Decision {
        request_id,
        decision,
    })
}

/// `behavior` decides, and only `allow` allows.
fn verdict(response: Option<&Value>) -> Decision {
    match response {
        Some(payload) if string(payload, "behavior").as_deref() == Some("allow") => {
            Decision::Allow {
                updated_input: payload.get("updatedInput").cloned(),
            }
        }
        Some(payload) => Decision::Deny {
            message: string(payload, "message"),
        },
        None => Decision::Deny { message: None },
    }
}

fn string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// The permission request a host answers under `--permission-prompt-tool stdio`.
pub(crate) fn can_use_tool(
    request_id: &str,
    tool: &str,
    input: &Value,
    session_scope: Option<&str>,
) -> Value {
    let mut request = json!({
        "subtype": "can_use_tool",
        "tool_name": tool,
        "input": input,
    });
    if let Some(suggestions) = suggestions(tool, session_scope)
        && let Some(object) = request.as_object_mut()
    {
        object.insert("permission_suggestions".into(), suggestions);
    }
    json!({
        "type": "control_request",
        "request_id": request_id,
        "request": request,
    })
}

/// The rule `AllowSession` would install, in the shape the protocol suggests
/// permissions in. No scope means no rule to suggest, and the field is left out
/// rather than sent empty.
fn suggestions(tool: &str, session_scope: Option<&str>) -> Option<Value> {
    let scope = session_scope?;
    let content = scope
        .strip_prefix(tool)
        .and_then(|rest| rest.strip_prefix('(')?.strip_suffix(')'));
    Some(json!([{
        "type": "addRules",
        "destination": "session",
        "behavior": "allow",
        "rules": [{ "toolName": tool, "ruleContent": content }],
    }]))
}

/// The acknowledgement every control request gets, once it has been carried out.
pub(crate) fn control_ok(request_id: &str) -> Value {
    json!({
        "type": "control_response",
        "response": { "subtype": "success", "request_id": request_id, "response": {} },
    })
}

pub(crate) fn control_error(request_id: &str, message: &str) -> Value {
    json!({
        "type": "control_response",
        "response": { "subtype": "error", "request_id": request_id, "error": message },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> Line {
        parse_line(line).expect("a line this surface reads")
    }

    fn error(line: &str) -> String {
        parse_line(line).expect_err("a line this surface refuses").0
    }

    // ---- the lines a host writes -----------------------------------------

    #[test]
    fn a_user_line_with_string_content_is_a_prompt() {
        let line = r#"{"type":"user","message":{"role":"user","content":"hello"},
            "parent_tool_use_id":null,"session_id":"ses_1"}"#;
        assert_eq!(
            parse(line),
            Line::User {
                text: "hello".into()
            }
        );
    }

    #[test]
    fn the_text_blocks_of_a_user_line_are_the_prompt_and_the_rest_is_dropped() {
        let line = r#"{"type":"user","message":{"role":"user","content":[
            {"type":"text","text":"look at this"},
            {"type":"image","source":{"type":"base64","media_type":"image/png","data":"iVBOR"}},
            {"type":"text","text":"and this"}]},"parent_tool_use_id":null}"#;
        assert_eq!(
            parse(line),
            Line::User {
                text: "look at this\nand this".into()
            }
        );
    }

    #[test]
    fn a_user_line_with_no_text_is_no_prompt() {
        let line = r#"{"type":"user","message":{"role":"user","content":[
            {"type":"tool_result","tool_use_id":"call_1","content":"ok"}]}}"#;
        assert_eq!(error(line), "a user line with no text to submit");
        let blank = r#"{"type":"user","message":{"role":"user","content":"   "}}"#;
        assert_eq!(error(blank), "a user line with no text to submit");
    }

    #[test]
    fn a_user_line_with_no_content_says_so() {
        let line = r#"{"type":"user","message":{"role":"user"}}"#;
        assert_eq!(error(line), "a user line with no `message.content`");
    }

    #[test]
    fn an_interrupt_is_a_control_request_with_its_id() {
        let line = r#"{"type":"control_request","request_id":"req_1",
            "request":{"subtype":"interrupt"}}"#;
        assert_eq!(
            parse(line),
            Line::Interrupt {
                request_id: "req_1".into()
            }
        );
    }

    #[test]
    fn an_unknown_control_request_keeps_its_id_so_it_can_be_refused() {
        let line = r#"{"type":"control_request","request_id":"req_2",
            "request":{"subtype":"initialize","hooks":{}}}"#;
        assert_eq!(
            parse(line),
            Line::Unsupported {
                request_id: "req_2".into(),
                subtype: "initialize".into()
            }
        );
    }

    // ---- the verdicts ----------------------------------------------------

    fn decision(line: &str) -> Decision {
        match parse(line) {
            Line::Decision { decision, .. } => decision,
            other => panic!("expected a decision, got {other:?}"),
        }
    }

    #[test]
    fn an_allow_carries_the_input_the_host_would_run() {
        let line = r#"{"type":"control_response","response":{"subtype":"success",
            "request_id":"req_3","response":{"behavior":"allow","updatedInput":{"a":1}}}}"#;
        assert_eq!(
            parse(line),
            Line::Decision {
                request_id: "req_3".into(),
                decision: Decision::Allow {
                    updated_input: Some(json!({"a": 1}))
                },
            }
        );
    }

    #[test]
    fn an_allow_without_an_updated_input_is_still_an_allow() {
        let line = r#"{"type":"control_response","response":{"subtype":"success",
            "request_id":"req_3","response":{"behavior":"allow"}}}"#;
        assert_eq!(
            decision(line),
            Decision::Allow {
                updated_input: None
            }
        );
    }

    #[test]
    fn a_deny_carries_the_message_the_model_will_read() {
        let line = r#"{"type":"control_response","response":{"subtype":"success",
            "request_id":"req_4","response":{"behavior":"deny","message":"not that file"}}}"#;
        assert_eq!(
            decision(line),
            Decision::Deny {
                message: Some("not that file".into())
            }
        );
    }

    #[test]
    fn everything_that_is_not_an_allow_is_a_denial() {
        let cases = [
            r#"{"type":"control_response","response":{"subtype":"error",
                "request_id":"r","error":"the callback raised"}}"#,
            r#"{"type":"control_response","response":{"subtype":"success",
                "request_id":"r","response":{"behavior":"maybe"}}}"#,
            r#"{"type":"control_response","response":{"subtype":"success",
                "request_id":"r","response":{}}}"#,
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"r"}}"#,
        ];
        for line in cases {
            assert!(
                matches!(decision(line), Decision::Deny { .. }),
                "allowed by: {line}"
            );
        }
    }

    // ---- the junk --------------------------------------------------------

    #[test]
    fn junk_is_an_error_a_person_can_read_and_never_a_panic() {
        assert!(error("").starts_with("not JSON:"));
        assert!(error("{oh no").starts_with("not JSON:"));
        assert!(error("[1,2,3]").starts_with("a line with no `type`"));
        assert_eq!(
            error(r#"{"type":"result"}"#),
            "unsupported line type `result`"
        );
        assert_eq!(
            error(r#"{"type":"control_request","request":{"subtype":"interrupt"}}"#),
            "a control request with no `request_id`"
        );
        assert_eq!(
            error(r#"{"type":"control_request","request_id":"r","request":{}}"#),
            "a control request with no `request.subtype`"
        );
        assert_eq!(
            error(r#"{"type":"control_response","response":{"request_id":"r"}}"#),
            "a control response with no `response.subtype`"
        );
    }

    // ---- the lines this surface writes -----------------------------------

    #[test]
    fn a_permission_request_names_the_tool_and_the_call() {
        let line = can_use_tool("req_5", "Edit", &json!({"file_path": "a.txt"}), None);
        assert_eq!(
            line,
            json!({
                "type": "control_request",
                "request_id": "req_5",
                "request": {
                    "subtype": "can_use_tool",
                    "tool_name": "Edit",
                    "input": { "file_path": "a.txt" },
                },
            })
        );
    }

    #[test]
    fn a_session_scope_becomes_the_rule_it_would_install() {
        let line = can_use_tool("req_6", "Read", &json!({}), Some("Read(//tmp/**)"));
        assert_eq!(
            line["request"]["permission_suggestions"],
            json!([{
                "type": "addRules",
                "destination": "session",
                "behavior": "allow",
                "rules": [{ "toolName": "Read", "ruleContent": "//tmp/**" }],
            }])
        );
    }

    /// A scope the kernel spells some other way still travels, with no content
    /// invented for it.
    #[test]
    fn a_scope_that_is_not_a_tool_call_keeps_its_tool_and_a_null_rule() {
        let line = can_use_tool("req_7", "Bash", &json!({}), Some("everything"));
        assert_eq!(
            line["request"]["permission_suggestions"][0]["rules"][0],
            json!({ "toolName": "Bash", "ruleContent": Value::Null })
        );
    }

    #[test]
    fn every_control_request_is_answered_by_its_id() {
        assert_eq!(
            control_ok("req_8"),
            json!({
                "type": "control_response",
                "response": { "subtype": "success", "request_id": "req_8", "response": {} },
            })
        );
        assert_eq!(
            control_error("req_9", "no"),
            json!({
                "type": "control_response",
                "response": { "subtype": "error", "request_id": "req_9", "error": "no" },
            })
        );
    }

    #[test]
    fn the_input_format_argument_picks_the_source() {
        assert_eq!(
            Format::from_args(&json!({ "inputFormat": "stream-json" })),
            Format::StreamJson
        );
        assert_eq!(
            Format::from_args(&json!({ "inputFormat": "text" })),
            Format::Text
        );
        assert_eq!(Format::from_args(&Value::Null), Format::Text);
    }

    #[test]
    fn only_the_stdio_prompt_tool_is_this_surface_s_to_answer() {
        assert!(prompts_on_stdio(
            &json!({ "permissionPromptTool": "stdio" })
        ));
        assert!(!prompts_on_stdio(
            &json!({ "permissionPromptTool": "mcp__auth" })
        ));
        assert!(!prompts_on_stdio(&Value::Null));
    }
}
