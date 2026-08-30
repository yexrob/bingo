//! Every point, run against real `sh` one-liners: what a hook says is only worth
//! testing through the process that says it.

use super::*;

use bingo_sdk::{
    Action, AnswerSpec, Input, Interaction, InteractionId, ItemId, Origin, Seq, SessionId, TurnId,
};
use jiff::Timestamp;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A hook set, the directory it works in, and the session it runs for.
struct Fixture {
    hooks: ShellHooks,
    cx: HookContext,
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new(config: serde_json::Value) -> Self {
        Self::build(|_| config)
    }

    /// A fixture whose hooks may name paths inside its own directory.
    fn build(config: impl FnOnce(&Path) -> serde_json::Value) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let hooks: Hooks = serde_json::from_value(config(dir.path())).expect("the hooks parse");
        Self {
            hooks: ShellHooks::new(&hooks, dir.path()),
            cx: HookContext {
                host: bingo_sdk::testing::NoHost::handle(),
                session: SessionId::from_raw("ses_test"),
                turn: None,
                cwd: dir.path().to_path_buf(),
                provider: None,
                model: Some("fake/echo".into()),
            },
            dir,
        }
    }

    /// A path in the fixture's directory a hook was told to write to.
    fn scratch(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
}

/// One `PreToolUse` rule, matching every tool, running these commands in order.
fn pre_tool_use(commands: &[&str]) -> serde_json::Value {
    let hooks: Vec<_> = commands
        .iter()
        .map(|c| json!({"type": "command", "command": c}))
        .collect();
    json!({"PreToolUse": [{"hooks": hooks}]})
}

/// A command that prints exactly this JSON.
fn says(body: &str) -> String {
    format!("printf '%s' '{body}'")
}

/// A command that saves whatever it was given on stdin.
fn saves(path: &Path) -> String {
    format!("cat > '{}'", path.display())
}

fn read_json(path: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(path).expect("the hook wrote its input");
    serde_json::from_str(&text).expect("the hook was given JSON")
}

fn call(name: &str, input: serde_json::Value) -> ToolCall {
    ToolCall {
        call_id: "call_01".into(),
        name: name.into(),
        input,
    }
}

fn bash() -> ToolCall {
    call("Bash", json!({"command": "ls"}))
}

fn frame(event: Event) -> Frame {
    Frame {
        seq: Seq(1),
        ts: Timestamp::from_second(1_700_000_000).expect("a timestamp"),
        session: SessionId::from_raw("ses_test"),
        cause: None,
        event,
    }
}

fn notice(code: &str, text: &str) -> Frame {
    frame(Event::Notice {
        level: Level::Warn,
        code: code.into(),
        text: text.into(),
    })
}

fn permission(tool: &str, summary: &str) -> Frame {
    frame(Event::InteractionOpened {
        interaction: Interaction {
            id: InteractionId::from_raw("int_01"),
            session: SessionId::from_raw("ses_test"),
            turn: Some(TurnId::from_raw("trn_01")),
            item: Some(ItemId::from_raw("itm_01")),
            opened_at: Timestamp::from_second(1_700_000_000).expect("a timestamp"),
            guard_until: None,
            expires_at: None,
            kind: InteractionKind::Permission {
                tool: tool.into(),
                summary: summary.into(),
                preview: None,
                session_scope: None,
            },
            answers: vec![AnswerSpec::Deny],
        },
    })
}

#[tokio::test]
async fn a_deny_refuses_the_call_with_its_reason() {
    let f = Fixture::new(pre_tool_use(&[&says(
        r#"{"hookSpecificOutput":{"permissionDecision":"deny","permissionDecisionReason":"not that"}}"#,
    )]));
    assert_eq!(
        f.hooks.before_tool(&mut bash(), &f.cx).await,
        HookOutcome::Deny {
            reason: "not that".into()
        }
    );
}

#[tokio::test]
async fn an_ask_puts_the_call_to_a_person() {
    let f = Fixture::new(pre_tool_use(&[&says(
        r#"{"hookSpecificOutput":{"permissionDecision":"ask","permissionDecisionReason":"sure?"}}"#,
    )]));
    assert_eq!(
        f.hooks.before_tool(&mut bash(), &f.cx).await,
        HookOutcome::Ask {
            reason: "sure?".into()
        }
    );
}

#[tokio::test]
async fn an_allow_still_goes_to_the_gate() {
    let f = Fixture::new(pre_tool_use(&[&says(
        r#"{"hookSpecificOutput":{"permissionDecision":"allow"}}"#,
    )]));
    assert_eq!(
        f.hooks.before_tool(&mut bash(), &f.cx).await,
        HookOutcome::Continue
    );
}

#[tokio::test]
async fn the_first_hook_to_object_settles_the_call() {
    let f = Fixture::new(pre_tool_use(&[
        &says(r#"{"decision":"deny","reason":"first"}"#),
        &says(r#"{"decision":"deny","reason":"second"}"#),
    ]));
    assert_eq!(
        f.hooks.before_tool(&mut bash(), &f.cx).await,
        HookOutcome::Deny {
            reason: "first".into()
        }
    );
}

#[tokio::test]
async fn updated_input_accumulates_over_the_hooks() {
    let f = Fixture::new(pre_tool_use(&[
        &says(r#"{"hookSpecificOutput":{"updatedInput":{"command":"ls -a"}}}"#),
        &says(r#"{"hookSpecificOutput":{"updatedInput":{"timeout":5}}}"#),
    ]));
    let mut call = bash();
    assert_eq!(
        f.hooks.before_tool(&mut call, &f.cx).await,
        HookOutcome::Continue
    );
    assert_eq!(call.input, json!({"command": "ls -a", "timeout": 5}));
}

#[tokio::test]
async fn a_later_hook_is_told_what_an_earlier_one_rewrote() {
    let f = Fixture::new(pre_tool_use(&[
        &says(r#"{"hookSpecificOutput":{"updatedInput":{"command":"echo safe"}}}"#),
        // Deny with whatever `command` now says, to prove it was passed on.
        r#"printf '{"decision":"deny","reason":"%s"}' "$(sed -n 's/.*"command":"\([^"]*\)".*/\1/p')""#,
    ]));
    assert_eq!(
        f.hooks.before_tool(&mut bash(), &f.cx).await,
        HookOutcome::Deny {
            reason: "echo safe".into()
        }
    );
}

#[tokio::test]
async fn exit_two_denies_with_what_the_hook_wrote_on_stderr() {
    let f = Fixture::new(pre_tool_use(&["echo 'no writes today' >&2; exit 2"]));
    assert_eq!(
        f.hooks.before_tool(&mut bash(), &f.cx).await,
        HookOutcome::Deny {
            reason: "no writes today".into()
        }
    );
}

#[tokio::test]
async fn exit_two_without_a_word_still_names_the_event_that_blocked() {
    let f = Fixture::new(pre_tool_use(&["exit 2"]));
    assert_eq!(
        f.hooks.before_tool(&mut bash(), &f.cx).await,
        HookOutcome::Deny {
            reason: "a PreToolUse hook said so".into()
        }
    );
}

#[tokio::test]
async fn a_hook_that_fails_any_other_way_decides_nothing() {
    let f = Fixture::new(pre_tool_use(&[&format!(
        "{}; exit 7",
        says(r#"{"decision":"deny","reason":"ignored"}"#)
    )]));
    assert_eq!(
        f.hooks.before_tool(&mut bash(), &f.cx).await,
        HookOutcome::Continue
    );
}

#[tokio::test]
async fn a_hook_past_its_deadline_is_killed_and_decides_nothing() {
    let f = Fixture::new(json!({"PreToolUse": [{"hooks": [
        {"type": "command", "command": "sleep 30", "timeout": 0}
    ]}]}));
    let started = Instant::now();
    let outcome = f.hooks.before_tool(&mut bash(), &f.cx).await;
    let elapsed = started.elapsed();
    assert_eq!(outcome, HookOutcome::Continue);
    assert!(elapsed < Duration::from_secs(1), "took {elapsed:?}");
}

#[tokio::test]
async fn output_that_is_not_json_decides_nothing() {
    let f = Fixture::new(pre_tool_use(&["printf '{not json}'"]));
    assert_eq!(
        f.hooks.before_tool(&mut bash(), &f.cx).await,
        HookOutcome::Continue
    );
}

#[tokio::test]
async fn a_large_input_and_a_hook_that_never_reads_it_still_finishes() {
    let f = Fixture::new(pre_tool_use(&["exit 0"]));
    let mut big = call("Write", json!({"content": "x".repeat(64 * 1024)}));
    let started = Instant::now();
    assert_eq!(
        f.hooks.before_tool(&mut big, &f.cx).await,
        HookOutcome::Continue
    );
    assert!(started.elapsed() < Duration::from_secs(5), "it hung");
}

#[tokio::test]
async fn a_matcher_selects_the_tools_it_names() {
    let f = Fixture::new(json!({"PreToolUse": [{
        "matcher": "Edit|Write",
        "hooks": [{"type": "command", "command": "echo no >&2; exit 2"}]
    }]}));
    let mut edit = call("Edit", json!({}));
    assert!(matches!(
        f.hooks.before_tool(&mut edit, &f.cx).await,
        HookOutcome::Deny { .. }
    ));
    assert_eq!(
        f.hooks.before_tool(&mut bash(), &f.cx).await,
        HookOutcome::Continue
    );
}

#[tokio::test]
async fn an_event_nobody_configured_runs_nothing() {
    let f = Fixture::new(json!({"Stop": [{"hooks": [{"type": "command", "command": "exit 2"}]}]}));
    assert_eq!(
        f.hooks.before_tool(&mut bash(), &f.cx).await,
        HookOutcome::Continue
    );
}

#[tokio::test]
async fn the_call_reaches_the_hook_as_the_documented_shape() {
    let f = Fixture::build(|dir| {
        json!({"PreToolUse": [{"hooks": [
            {"type": "command", "command": saves(&dir.join("seen"))}
        ]}]})
    });
    f.hooks.before_tool(&mut bash(), &f.cx).await;
    let written = read_json(&f.scratch("seen"));
    assert_eq!(written["hook_event_name"], "PreToolUse");
    assert_eq!(written["session_id"], "ses_test");
    assert_eq!(written["tool_name"], "Bash");
    assert_eq!(written["tool_use_id"], "call_01");
    assert_eq!(written["tool_input"]["command"], "ls");
}

#[tokio::test]
async fn what_session_start_exports_reaches_the_next_hook() {
    let f = Fixture::new(json!({
        "SessionStart": [{"hooks": [
            {"type": "command", "command": "echo 'export FOO=bar' >> \"$BINGO_ENV_FILE\""}
        ]}],
        "PreToolUse": [{"hooks": [
            {"type": "command",
             "command": "printf '{\"decision\":\"deny\",\"reason\":\"%s\"}' \"$FOO\""}
        ]}]
    }));
    f.hooks.on_session(Phase::Start, &f.cx).await;
    assert_eq!(
        f.hooks.before_tool(&mut bash(), &f.cx).await,
        HookOutcome::Deny {
            reason: "bar".into()
        }
    );
}

#[tokio::test]
async fn a_second_session_start_hook_sees_the_first_one_s_exports() {
    let f = Fixture::new(json!({
        "SessionStart": [{"hooks": [
            {"type": "command", "command": "echo 'FOO=bar' >> \"$BINGO_ENV_FILE\""},
            {"type": "command", "command": "echo \"SEEN=$FOO\" >> \"$BINGO_ENV_FILE\""}
        ]}]
    }));
    f.hooks.on_session(Phase::Start, &f.cx).await;
    let env = f.hooks.dispatch.sessions().env(&f.cx.session);
    assert_eq!(env["SEEN"], "bar");
}

#[tokio::test]
async fn a_session_that_ended_forgets_what_it_exported() {
    let f = Fixture::new(json!({
        "SessionStart": [{"hooks": [
            {"type": "command", "command": "echo 'FOO=bar' >> \"$BINGO_ENV_FILE\""}
        ]}]
    }));
    f.hooks.on_session(Phase::Start, &f.cx).await;
    assert_eq!(f.hooks.dispatch.sessions().env(&f.cx.session)["FOO"], "bar");
    f.hooks.on_session(Phase::End, &f.cx).await;
    assert!(f.hooks.dispatch.sessions().env(&f.cx.session).is_empty());
}

#[tokio::test]
async fn a_stop_hook_can_ask_for_one_more_turn() {
    let f = Fixture::new(json!({"Stop": [{"hooks": [
        {"type": "command", "command": "echo 'the tests are red' >&2; exit 2"}
    ]}]}));
    assert_eq!(
        f.hooks.on_stop(&f.cx).await,
        HookOutcome::Block {
            reason: "the tests are red".into()
        }
    );
}

#[tokio::test]
async fn a_stop_hook_may_also_block_in_json() {
    let f = Fixture::new(json!({"Stop": [{"hooks": [
        {"type": "command", "command": says(r#"{"decision":"block","reason":"one more"}"#)}
    ]}]}));
    assert_eq!(
        f.hooks.on_stop(&f.cx).await,
        HookOutcome::Block {
            reason: "one more".into()
        }
    );
}

#[tokio::test]
async fn a_stop_hook_that_is_content_lets_the_turn_end() {
    let f = Fixture::new(json!({"Stop": [{"hooks": [{"type": "command", "command": "exit 0"}]}]}));
    assert_eq!(f.hooks.on_stop(&f.cx).await, HookOutcome::Continue);
}

#[tokio::test]
async fn a_prompt_hook_appends_its_context_to_the_input() {
    let f = Fixture::new(json!({"UserPromptSubmit": [{"hooks": [
        {"type": "command",
         "command": says(r#"{"hookSpecificOutput":{"additionalContext":"branch: main"}}"#)}
    ]}]}));
    let mut input = Input::text("fix the build", Origin::surface("test"));
    assert_eq!(
        f.hooks.on_submit(&mut input, &f.cx).await,
        HookOutcome::Continue
    );
    let Input::Text { text, .. } = &input else {
        panic!("the input is still text")
    };
    assert_eq!(text, "fix the build\nbranch: main");
}

#[tokio::test]
async fn a_prompt_hook_can_reject_the_input() {
    let f = Fixture::new(json!({"UserPromptSubmit": [{"hooks": [
        {"type": "command", "command": says(r#"{"decision":"block","reason":"no secrets"}"#)}
    ]}]}));
    let mut input = Input::text("here is my key", Origin::surface("test"));
    assert_eq!(
        f.hooks.on_submit(&mut input, &f.cx).await,
        HookOutcome::Deny {
            reason: "no secrets".into()
        }
    );
}

#[tokio::test]
async fn the_prompt_reaches_the_hook_under_its_documented_name() {
    let f = Fixture::build(|dir| {
        json!({"UserPromptSubmit": [{"hooks": [
            {"type": "command", "command": saves(&dir.join("seen"))}
        ]}]})
    });
    let mut input = Input::text("write the tests", Origin::surface("test"));
    f.hooks.on_submit(&mut input, &f.cx).await;
    assert_eq!(read_json(&f.scratch("seen"))["prompt"], "write the tests");
}

#[tokio::test]
async fn an_action_carries_no_prompt_and_runs_no_hook() {
    let f = Fixture::new(json!({"UserPromptSubmit": [{"hooks": [
        {"type": "command", "command": "exit 2"}
    ]}]}));
    let mut input = Input::Action {
        action: Action {
            name: "permission".into(),
            args: json!({}),
        },
    };
    assert_eq!(
        f.hooks.on_submit(&mut input, &f.cx).await,
        HookOutcome::Continue
    );
}

#[tokio::test]
async fn a_result_that_failed_is_a_different_event_from_one_that_did_not() {
    let f = Fixture::new(json!({
        "PostToolUse": [{"hooks": [{"type": "command", "command": "exit 0"}]}],
        "PostToolUseFailure": [{"hooks": [
            {"type": "command", "command": says(r#"{"decision":"block","reason":"it failed"}"#)}
        ]}]
    }));
    let call = bash();
    assert_eq!(
        f.hooks
            .after_tool(&call, &ToolOutput::text("ok"), &f.cx)
            .await,
        HookOutcome::Continue
    );
    assert_eq!(
        f.hooks
            .after_tool(&call, &ToolOutput::error("boom"), &f.cx)
            .await,
        HookOutcome::Block {
            reason: "it failed".into()
        }
    );
}

#[tokio::test]
async fn a_failed_result_reaches_the_hook_as_tool_error() {
    let f = Fixture::build(|dir| {
        json!({"PostToolUseFailure": [{"hooks": [
            {"type": "command", "command": saves(&dir.join("seen"))}
        ]}]})
    });
    f.hooks
        .after_tool(&bash(), &ToolOutput::error("boom"), &f.cx)
        .await;
    let written = read_json(&f.scratch("seen"));
    assert_eq!(written["hook_event_name"], "PostToolUseFailure");
    assert_eq!(written["tool_error"]["isError"], true);
    assert!(written.get("tool_response").is_none());
}

#[tokio::test]
async fn every_post_tool_hook_runs_even_after_one_objects() {
    let f = Fixture::build(|dir| {
        json!({"PostToolUse": [{"hooks": [
            {"type": "command", "command": says(r#"{"decision":"block","reason":"first"}"#)},
            {"type": "command", "command": format!("touch '{}'", dir.join("ran").display())}
        ]}]})
    });
    let outcome = f
        .hooks
        .after_tool(&bash(), &ToolOutput::text("ok"), &f.cx)
        .await;
    assert_eq!(
        outcome,
        HookOutcome::Block {
            reason: "first".into()
        }
    );
    assert!(f.scratch("ran").exists(), "the second hook was skipped");
}

#[tokio::test]
async fn a_notice_reaches_a_notification_hook_matched_on_its_code() {
    let f = Fixture::build(|dir| {
        json!({"Notification": [{
            "matcher": "TOOL_SHADOWED",
            "hooks": [{"type": "command", "command": saves(&dir.join("seen"))}]
        }]})
    });
    let seen = f.scratch("seen");

    f.hooks
        .on_event(&notice("TOOL_SHADOWED", "Echo2 twice"), &f.cx)
        .await;
    let written = read_json(&seen);
    assert_eq!(written["hook_event_name"], "Notification");
    assert_eq!(written["notification_type"], "TOOL_SHADOWED");
    assert_eq!(written["level"], "warn");
    assert_eq!(written["message"], "Echo2 twice");

    std::fs::remove_file(&seen).expect("clear");
    f.hooks
        .on_event(&notice("SOMETHING_ELSE", "x"), &f.cx)
        .await;
    assert!(
        !seen.exists(),
        "a code the matcher rejects still ran a hook"
    );
}

#[tokio::test]
async fn an_opened_permission_reaches_a_permission_request_hook() {
    let f = Fixture::build(|dir| {
        json!({"PermissionRequest": [{
            "matcher": "Bash",
            "hooks": [{"type": "command", "command": saves(&dir.join("seen"))}]
        }]})
    });
    f.hooks
        .on_event(&permission("Bash", "Bash(rm -rf b)"), &f.cx)
        .await;
    let written = read_json(&f.scratch("seen"));
    assert_eq!(written["hook_event_name"], "PermissionRequest");
    assert_eq!(written["tool_name"], "Bash");
    assert_eq!(written["summary"], "Bash(rm -rf b)");
}

#[tokio::test]
async fn a_frame_no_hook_asked_for_runs_nothing() {
    let f = Fixture::new(json!({"Notification": [{"hooks": [
        {"type": "command", "command": "exit 2"}
    ]}]}));
    // A hook that ran would exit 2; the point is that nothing is spawned at all.
    f.hooks
        .on_event(
            &frame(Event::CatalogChanged {
                kind: "Tools".into(),
            }),
            &f.cx,
        )
        .await;
}

#[tokio::test]
async fn only_the_start_of_a_compaction_is_a_pre_compact() {
    let f = Fixture::build(|dir| {
        json!({"PreCompact": [{"hooks": [
            {"type": "command", "command": saves(&dir.join("seen"))}
        ]}]})
    });
    let seen = f.scratch("seen");
    f.hooks.on_compact(Phase::End, &f.cx).await;
    assert!(
        !seen.exists(),
        "the end of a compaction ran a PreCompact hook"
    );
    f.hooks.on_compact(Phase::Start, &f.cx).await;
    let written = read_json(&seen);
    assert_eq!(written["hook_event_name"], "PreCompact");
    assert_eq!(written["trigger"], "auto");
}

#[tokio::test]
async fn a_session_end_hook_does_not_hold_the_teardown() {
    let f = Fixture::new(json!({"SessionEnd": [{"hooks": [
        {"type": "command", "command": "sleep 30"}
    ]}]}));
    let started = Instant::now();
    f.hooks.on_session(Phase::End, &f.cx).await;
    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_secs(3), "took {elapsed:?}");
}

#[test]
fn the_matcher_claims_every_point_this_plugin_serves() {
    let f = Fixture::new(json!({}));
    let matcher = f.hooks.matcher();
    assert!(
        matcher.tool.is_none(),
        "tool names are matched here, by regex"
    );
    for point in [
        HookPoint::Submit,
        HookPoint::BeforeTool,
        HookPoint::AfterTool,
        HookPoint::Stop,
        HookPoint::Compact,
        HookPoint::Session,
        HookPoint::Event,
    ] {
        assert!(matcher.points.contains(&point), "{point:?} is not claimed");
    }
    // No Claude Code event marks a turn's edges; `Stop` is `on_stop`.
    assert!(!matcher.points.contains(&HookPoint::Turn));
}
