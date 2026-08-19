use std::io::{BufRead, Write};
use std::sync::Arc;

use thiserror::Error;
use tokio::sync::watch;

use crate::api::client::ClientError;
use crate::api::types::{ContentBlock, Message, Role};
use crate::budget::MAX_RESULT_CHARS;
use crate::compact::{TokenGate, check_and_compact, compact_after_overflow};
use crate::engine::events::{EngineEvent, EngineEvents, EngineHost, EngineRequests};
use crate::error::ErrorCode;
use crate::hooks::{run_post_tool_use, run_pre_tool_use, run_stop_hooks, run_user_prompt_submit};
use crate::permission::{PermissionBehavior, PermissionMode, can_use_tool};
use crate::settings::HooksConfig;
use crate::tool::executor::{PendingCall, execute_calls};
use crate::tool::{Tool, ToolContext, ToolError, ToolResult, find_tool, tool_params};

#[derive(Debug, Error)]
pub enum QueryError {
    #[error(transparent)]
    Client(#[from] ClientError),
    #[error("stream protocol error: {0}")]
    Protocol(String),
    #[error("tool execution error: {0}")]
    Tool(#[from] ToolError),
}

impl ErrorCode for QueryError {
    fn error_code(&self) -> &'static str {
        match self {
            QueryError::Client(e) => e.error_code(),
            QueryError::Protocol(_) => "SERVER_ERROR",
            QueryError::Tool(e) => e.error_code(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryEndReason {
    Completed,
    EmptyResponseRetried,
}

/// Result of a query.
#[derive(Debug)]
pub struct QueryOutcome {
    pub messages: Vec<Message>,
    pub end_reason: QueryEndReason,
    /// Turn aborted by the user (stream stopped; tools that already ran finish normally).
    pub aborted: bool,
    /// The interrupt marker this turn appended to the transcript, when it recorded one
    /// ([`INTERRUPT_MARKER`] / [`INTERRUPT_MARKER_TOOL_USE`]). The UI echoes exactly this
    /// string, so the screen and the model read the same sentence.
    pub interrupt_marker: Option<&'static str>,
}

pub(crate) struct InboxWake {
    instance: String,
    rx: watch::Receiver<u64>,
    pub(crate) output_chars: usize,
    claimed: Vec<crate::agents::InboxItem>,
}

impl InboxWake {
    fn for_session(session: &Arc<Session>) -> Option<Self> {
        session.instance.clone().map(|instance| Self {
            instance,
            rx: session.agents.subscribe_inbox(),
            output_chars: 0,
            claimed: Vec::new(),
        })
    }

    async fn take(&mut self, session: &Arc<Session>) -> Vec<crate::agents::InboxItem> {
        let _ = self.rx.borrow_and_update();
        let items = session
            .agents
            .take_running(&self.instance, self.output_chars)
            .await;
        self.claimed.extend(items.iter().cloned());
        items
    }

    fn restore(&mut self, session: &Arc<Session>) {
        session
            .agents
            .restore_inbox(&self.instance, std::mem::take(&mut self.claimed));
    }

    pub(crate) async fn changed(&mut self) {
        if self.rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/// Recovery injection after max_tokens truncation.
const MAX_OUTPUT_TOKENS_RECOVERY_LIMIT: u32 = 3;
pub(crate) const MAX_TOKENS_RESUME_PROMPT: &str =
    "Output token limit hit. Resume directly from where you left off. Do not apologize or explain.";

/// Wrapper around the main agent's drained inbox (D98). It carries room relays
/// and direct messages alike, so it is named for what it is rather than for one
/// of the two; the marker on each line inside says which kind it is.
pub(crate) const MAIL_BLOCK_OPEN: &str = "<messages>";
pub(crate) const MAIL_BLOCK_CLOSE: &str = "</messages>";

/// Task reminder thresholds (TURNS_SINCE_WRITE / TURNS_BETWEEN_REMINDERS).
const TASK_REMINDER_TURNS: u64 = 10;
pub(crate) const TASK_REMINDER_MARKER: &str = "[SYSTEM NOTIFICATION - TASK REMINDER]";

/// Turn distance calculation: count assistant turns backward from the end of the messages,
/// stopping at a turn containing a TaskCreate/TaskUpdate tool_use (or a reminder message).
/// management = turns since the most recent Task tool; reminder = turns since the most
/// recent reminder. If neither has ever occurred → treated as over the threshold
/// (returns REMINDER_TURNS+1).
fn task_reminder_turn_distances(messages: &[Message]) -> (u64, u64) {
    let mut since_management = 0u64;
    let mut since_reminder = 0u64;
    let mut management_seen = false;
    let mut reminder_seen = false;
    for message in messages.iter().rev() {
        if message.role == Role::Assistant {
            // Stop each counter once its own "seen" flag is set: in a first session the
            // reminder never exists, and continuing to count would make "turns since last
            // Task tool" equal the total turns, reminding even right after use.
            if !management_seen {
                since_management += 1;
                let uses_task_tool = message.content.iter().any(|b| {
                    matches!(b, ContentBlock::ToolUse { name, .. } if name == "TaskCreate" || name == "TaskUpdate")
                });
                if uses_task_tool {
                    management_seen = true;
                }
            }
            if !reminder_seen {
                since_reminder += 1;
            }
        } else if !reminder_seen {
            let is_reminder = message.content.iter().any(|b| {
                matches!(b, ContentBlock::Text { text } if text.starts_with(TASK_REMINDER_MARKER))
            });
            if is_reminder {
                reminder_seen = true;
            }
        }
        if management_seen && reminder_seen {
            break;
        }
    }
    let since_management = if management_seen {
        since_management
    } else {
        TASK_REMINDER_TURNS + 1
    };
    let since_reminder = if reminder_seen {
        since_reminder
    } else {
        TASK_REMINDER_TURNS + 1
    };
    (since_management, since_reminder)
}

/// Inject the task reminder: no Task tool for 10 turns + 10 turns since the last reminder.
async fn maybe_inject_task_reminder(
    session: &Session,
    messages: &mut Vec<Message>,
    host: &EngineHost,
) {
    let (since_management, since_reminder) = task_reminder_turn_distances(messages);
    if since_management < TASK_REMINDER_TURNS || since_reminder < TASK_REMINDER_TURNS {
        return;
    }
    let items = match session.tasks.list().await {
        Ok(items) => items,
        Err(e) => {
            eprintln!("[bingo] warning: task_reminder list failed: {e}");
            return;
        }
    };
    let mut text = format!(
        "{TASK_REMINDER_MARKER}\nThe task tools haven't been used recently. If you're working on \
tasks that would benefit from tracking progress, consider using TaskCreate to add new tasks and \
TaskUpdate to update task status (set to in_progress when starting, completed when done). Also \
consider cleaning up the task list if it has become stale. Only use these if relevant to the \
current work. This is just a gentle reminder - ignore if not applicable. Make sure that you NEVER \
mention this reminder to the user."
    );
    if !items.is_empty() {
        let list = items
            .iter()
            .map(|t| format!("#{}. [{}] {}", t.id, t.status, t.subject))
            .collect::<Vec<_>>()
            .join("\n");
        text.push_str(&format!("\n\nHere are the existing tasks:\n\n{list}"));
    }
    record(session, messages, Message::user_text(text), host);
}

pub use crate::query_session::{Runtime, Session};

/// Single tool completion event.
#[derive(Debug, Clone)]
pub struct ToolCallDone {
    pub tool_call_id: String,
    pub name: String,
    pub summary: String,
    pub output: String,
    pub status: ToolCallStatus,
    /// Unified diff preview for edit tools (None = no diff).
    pub diff: Option<String>,
    /// Tool execution duration in milliseconds.
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCallStatus {
    Done,
    Error,
    Interrupted,
}

/// What the user decided at a permission prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskOutcome {
    /// Allow this call; the next one of the same shape asks again.
    Allow,
    /// Allow this call and install [`AskContext::scope`] as a session-scoped
    /// allow rule, so the rest of the session runs unasked. Nothing is written
    /// to disk: "this session" is exactly what the user was offered.
    AllowSession,
    /// Refuse. `feedback` is what the user asked for instead; it travels to the
    /// model inside the `<permission_error>` so a denial carries a direction
    /// rather than only a wall.
    Deny { feedback: Option<String> },
}

impl AskOutcome {
    // Production reads the variants directly; the shorthand is what tests assert with.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn allowed(&self) -> bool {
        matches!(self, Self::Allow | Self::AllowSession)
    }
}

/// Everything a prompt surface needs to describe one pending permission request.
///
/// It travels as a struct rather than positional arguments because the prompt
/// has to show *what* it is approving (the input, resolved against `cwd`, or the
/// dry-run `diff`) and whether "don't ask again" can honestly be offered
/// (`scope`) — a bool-returning `(name, reason)` callback could show neither.
#[derive(Clone, Copy)]
pub struct AskContext<'a> {
    /// Tool being gated.
    pub tool: &'a str,
    /// Why the gate is asking (`can_use_tool`'s reason).
    pub reason: &'a str,
    /// The input the tool would run with, after PreToolUse hooks.
    pub input: &'a serde_json::Value,
    /// Session working directory: relative paths in `input` resolve against it.
    pub cwd: &'a std::path::Path,
    /// The allow rule "don't ask again this session" would install. `None`: the
    /// prompt outranks allow rules (ask rule / safety check), so the option must
    /// not be offered — it could not keep its promise.
    pub scope: Option<&'a str>,
    /// Dry-run unified diff of the change approving would make (edit tools).
    pub diff: Option<&'a str>,
}

/// Async permission prompt callback: one pending request → the user's verdict.
pub type AskFn = dyn Fn(&AskContext<'_>) -> std::pin::Pin<Box<dyn std::future::Future<Output = AskOutcome> + Send>>
    + Send
    + Sync;

/// AskUserQuestion answer for one question: a selected option or Other free-form input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskAnswer {
    /// Option index (0-based).
    Option(usize),
    /// Other free-form text (the Other option CC provides automatically).
    Other(String),
}

/// Ask-the-user callback (AskUserQuestion tool): title + question + options
/// (label, description) → answer (None = user skipped/Esc).
pub type AskQuestionFn = dyn Fn(
        String,
        String,
        Vec<(String, Option<String>)>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<AskAnswer>> + Send>>
    + Send
    + Sync;

/// Prefix of the in-stream reconnect progress warning (`Reconnecting... N/M`); the TUI and
/// subagent views key replacement of stale progress notices off this prefix.
pub const RECONNECT_WARNING_PREFIX: &str = "Reconnecting... ";

/// Mid-turn steering source (D83): what the composer has queued for the turn that is
/// running right now.
///
/// Called once per tool barrier. The caller commits to appending whatever it returns to
/// the message it is about to record, so the take must be atomic — which it is, because
/// since B3 it happens inside the session actor
/// ([`crate::app::queue::QueueHandle::absorb`]). Returning an empty vector is the whole
/// contract for a host with no composer behind it.
pub type SteerFn = dyn Fn() -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Vec<crate::app::queue::SteerItem>> + Send>,
    > + Send
    + Sync;

/// The steering source of a host that has no one to steer with: headless runs, the JSON
/// protocol, and subagents, whose messages arrive through their own inbox instead.
pub fn no_steer() -> Arc<SteerFn> {
    Arc::new(|| Box::pin(async { Vec::new() }))
}

/// Headless permission prompt (stderr question, stdin answer). Shared by `headless_hooks` and
/// the subagent prompt surface attached to the registry, so both ask the same way.
pub fn stdin_ask() -> Arc<AskFn> {
    Arc::new(|ask| {
        // `s` is only listed when a session rule could actually be installed:
        // offering it otherwise would promise silence the gate cannot deliver.
        let scoped = ask.scope.is_some();
        let keys = if scoped { "[y/s/N]" } else { "[y/N]" };
        let prompt = format!("Allow {} to run? ({}) {keys} ", ask.tool, ask.reason);
        Box::pin(async move {
            eprintln!("{prompt}");
            let answer = tokio::task::spawn_blocking(move || {
                let mut line = String::new();
                if let Err(e) = std::io::stdin().lock().read_line(&mut line) {
                    eprintln!("[bingo] warning: cannot read answer from stdin: {e}");
                }
                line.trim().to_ascii_lowercase()
            })
            .await
            .unwrap_or_default();
            match answer.as_str() {
                "y" | "yes" => AskOutcome::Allow,
                "s" | "session" if scoped => AskOutcome::AllowSession,
                _ => AskOutcome::Deny { feedback: None },
            }
        })
    })
}

/// The headless host: the model's prose on stdout, everything else on stderr,
/// permissions and questions through stdin.
///
/// A headless run prints an answer rather than showing a conversation, so most
/// of what a run reports has no surface here. A shim: B8 removes this when
/// `--print` becomes a thin `AppCore` client.
pub fn headless_hooks() -> EngineHost {
    EngineHost::new(
        EngineEvents::new(|event| match event {
            EngineEvent::TextDelta { text, .. } => {
                let _ = std::io::stdout().write_all(text.as_bytes());
                let _ = std::io::stdout().flush();
            }
            EngineEvent::Warning(message) => eprintln!("[bingo] warning: {message}"),
            _ => {}
        }),
        EngineRequests {
            ask: stdin_ask(),
            ask_question: headless_ask_question(),
            // No composer behind a headless run: nothing can be typed mid-turn.
            steer: no_steer(),
            // Nor a screen to tail a command on, nor a key to press to background it.
            live: crate::live::LiveBash::detached(),
        },
    )
}

/// The stdin question prompt: the model's options, numbered, plus free text.
fn headless_ask_question() -> Arc<AskQuestionFn> {
    Arc::new(|title, question, options| {
        Box::pin(async move {
            eprintln!("[bingo] {title}: {question}");
            for (i, (label, desc)) in options.iter().enumerate() {
                match desc {
                    Some(d) if !d.is_empty() => {
                        eprintln!("  {}. {label} ({d})", i + 1)
                    }
                    _ => eprintln!("  {}. {label}", i + 1),
                }
            }
            eprintln!(
                "  {}. Other (free text)\nChoose [1-{}] or type text directly (Enter = skip): ",
                options.len() + 1,
                options.len() + 1
            );
            let answer = tokio::task::spawn_blocking(move || {
                let mut line = String::new();
                if let Err(e) = std::io::stdin().lock().read_line(&mut line) {
                    eprintln!("[bingo] warning: cannot read answer from stdin: {e}");
                }
                line.trim().to_string()
            })
            .await
            .unwrap_or_default();
            if let Ok(n) = answer.parse::<usize>()
                && let Some(i) = n.checked_sub(1)
                && i < options.len()
            {
                Some(AskAnswer::Option(i))
            } else if answer.is_empty() {
                None
            } else {
                Some(AskAnswer::Other(answer))
            }
        })
    })
}

use crate::query_turn::{one_turn_with_stream_retries, retry_after_overflow};

fn tool_result_text(tool_use_id: &str, text: impl Into<String>) -> ContentBlock {
    ContentBlock::ToolResult {
        tool_use_id: tool_use_id.to_string(),
        content: serde_json::Value::String(text.into()),
        is_error: false,
    }
}

fn tool_result_error(tool_use_id: &str, text: impl Into<String>) -> ContentBlock {
    ContentBlock::ToolResult {
        tool_use_id: tool_use_id.to_string(),
        content: serde_json::Value::String(text.into()),
        is_error: true,
    }
}

/// What the permission gate settled on for one call.
struct GateDecision {
    behavior: PermissionBehavior,
    reason: String,
    /// Input after PreToolUse hooks (possibly rewritten).
    input: serde_json::Value,
    /// What the user asked for instead when refusing. Carried separately from
    /// `reason` because it belongs after the parenthesised reason in the
    /// sentence the model reads, not inside it.
    guidance: Option<String>,
}

impl GateDecision {
    fn settled(behavior: PermissionBehavior, reason: String, input: serde_json::Value) -> Self {
        Self {
            behavior,
            reason,
            input,
            guidance: None,
        }
    }
}

/// Read the runtime rule table into an owned copy. The guard must not outlive
/// the call: it is not `Send`, and one held across an await breaks the `Send`
/// bound every spawned turn task depends on.
fn permission_snapshot(
    permissions: &Arc<std::sync::Mutex<crate::settings::PermissionRules>>,
) -> crate::settings::PermissionRules {
    permissions
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Install a session-scoped allow rule. The runtime table is shared with
/// subagents and `/permissions`, and is never persisted — the rule dies with
/// the session, which is what the user was offered.
fn install_session_rule(
    permissions: &Arc<std::sync::Mutex<crate::settings::PermissionRules>>,
    rule: String,
) {
    let mut rules = permissions.lock().unwrap_or_else(|e| e.into_inner());
    if !rules.allow.contains(&rule) {
        rules.allow.push(rule);
    }
}

/// Permission gate + PreToolUse hook + UI prompt: returns the final decision and
/// (possibly rewritten) input.
async fn gate_tool(
    tool: &dyn Tool,
    input: &serde_json::Value,
    mode: PermissionMode,
    hooks: &HooksConfig,
    permissions: &Arc<std::sync::Mutex<crate::settings::PermissionRules>>,
    ask: &AskFn,
    cwd: &std::path::Path,
) -> GateDecision {
    let name = tool.name();
    let (hook_behavior, hook_reason, hook_input) =
        run_pre_tool_use(hooks, &name, input, permission_mode_str(mode), cwd).await;
    if hook_behavior != PermissionBehavior::Allow {
        return GateDecision::settled(hook_behavior, hook_reason, hook_input);
    }

    let rules = permission_snapshot(permissions);
    let decision = can_use_tool(
        tool,
        &hook_input,
        mode,
        &rules.deny,
        &rules.ask,
        &rules.allow,
        cwd,
    );
    if decision.behavior != PermissionBehavior::Ask {
        return GateDecision::settled(decision.behavior, decision.reason, hook_input);
    }
    // Scope and preview are computed here, where the tool and the cwd are both
    // in hand: a prompt surface has neither, and would have to guess at both.
    let scope = crate::permission::session_allow_rule(tool, &hook_input, &rules.ask, cwd);
    let diff = tool.preview_diff(&hook_input, cwd);
    let outcome = ask(&AskContext {
        tool: &name,
        reason: &decision.reason,
        input: &hook_input,
        cwd,
        scope: scope.as_deref(),
        diff: diff.as_deref(),
    })
    .await;
    match outcome {
        AskOutcome::Allow => {
            GateDecision::settled(PermissionBehavior::Allow, String::new(), hook_input)
        }
        AskOutcome::AllowSession => {
            // Installed before the call runs, so the tool that asked is itself
            // covered and the same shape never asks twice in one session.
            if let Some(rule) = scope {
                install_session_rule(permissions, rule);
            }
            GateDecision::settled(PermissionBehavior::Allow, String::new(), hook_input)
        }
        AskOutcome::Deny { feedback } => GateDecision {
            behavior: PermissionBehavior::Deny,
            reason: format!("user denied {name}"),
            input: hook_input,
            guidance: feedback.filter(|text| !text.trim().is_empty()),
        },
    }
}

/// The refusal sentence. Feedback the user typed at the dialog is appended so
/// the model reads what to do instead, not only that it was stopped.
fn permission_denial(subject: &str, guidance: Option<&str>) -> String {
    match guidance {
        Some(text) => format!("permission denied: {subject}. User guidance: {text}"),
        None => format!("permission denied: {subject}"),
    }
}

fn permission_mode_str(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Default => "default",
        PermissionMode::AcceptEdits => "acceptEdits",
        PermissionMode::BypassPermissions => "bypassPermissions",
        PermissionMode::DontAsk => "dontAsk",
        PermissionMode::Plan => "plan",
    }
}

fn render_result(result: &ToolResult) -> String {
    crate::api::types::tool_result_text(&result.content)
}

fn result_block(tool_use_id: &str, result: &ToolResult) -> ContentBlock {
    // Array content is already a list of protocol blocks (text plus images). Pass it through:
    // stringifying it here is what would turn an image result into a wall of base64 text.
    if let serde_json::Value::Array(blocks) = &result.content {
        return ContentBlock::ToolResult {
            tool_use_id: tool_use_id.to_string(),
            content: serde_json::Value::Array(
                blocks.iter().map(clip_text_block).collect::<Vec<_>>(),
            ),
            is_error: result.is_error,
        };
    }
    if result.is_error {
        tool_result_error(tool_use_id, clipped_result(render_result(result)))
    } else {
        tool_result_text(tool_use_id, clipped_result(render_result(result)))
    }
}

/// Apply the tool-result length cap to a block's text, leaving image blocks untouched
/// (they are already bounded by `prepare_image`).
fn clip_text_block(block: &serde_json::Value) -> serde_json::Value {
    match (
        block.get("type").and_then(|t| t.as_str()),
        block.get("text").and_then(|t| t.as_str()),
    ) {
        (Some("text"), Some(text)) => {
            serde_json::json!({"type": "text", "text": clipped_result(text.to_string())})
        }
        _ => block.clone(),
    }
}

/// What an action-shaped tool call is aimed at, in the order the tools name it
/// (`AgentControl.agent`, `Channel.channel`, `Team.name`).
const TARGET_KEYS: &[&str] = &["agent", "channel", "name"];

pub(crate) fn summarize_input(tool_name: &str, input: &serde_json::Value) -> String {
    match (tool_name, input) {
        // Bash summary shows the command directly
        ("Bash", serde_json::Value::Object(map)) => map
            .get("command")
            .and_then(|c| c.as_str())
            .map(|c| format!("$ {c}"))
            .unwrap_or_else(|| "Bash".to_string()),
        // Search summary shows the query (Web Search("query"))
        ("WebSearch", serde_json::Value::Object(map)) => map
            .get("query")
            .and_then(|q| q.as_str())
            .map(|q| format!("Web Search({q:?})"))
            .unwrap_or_else(|| "Web Search".to_string()),
        // Agent summary shows the description (tool row = name + summary, summary without
        // the tool name), so parallel agents' tool rows are distinguishable (it once
        // degraded to showing background=true repeatedly as the first field).
        ("Agent", serde_json::Value::Object(map)) => {
            if let Some(desc) = map.get("description").and_then(|d| d.as_str())
                && !desc.is_empty()
            {
                format!("description=\"{desc}\"")
            } else if let Some(p) = map.get("prompt").and_then(|p| p.as_str()) {
                format!("prompt=\"{}\"", p.chars().take(40).collect::<String>())
            } else {
                String::new()
            }
        }
        // Skill summary shows the skill name and args (`Skill(review doc.md)`),
        // avoiding the k=v fallback (`args="…"` is too noisy).
        ("Skill", serde_json::Value::Object(map)) => {
            let skill = map.get("skill").and_then(|s| s.as_str()).unwrap_or("");
            let args = map.get("args").and_then(|a| a.as_str()).unwrap_or("");
            if skill.is_empty() {
                String::new()
            } else if args.is_empty() {
                skill.to_string()
            } else {
                format!("{skill} {args}")
            }
        }
        // AskUserQuestion summary shows the question text (readable tool row).
        ("AskUserQuestion", serde_json::Value::Object(map)) => map
            .get("questions")
            .and_then(|qs| qs.as_array())
            .and_then(|qs| qs.first())
            .and_then(|q| q.get("question"))
            .and_then(|q| q.as_str())
            .map(|q| format!("{q:?}"))
            .unwrap_or_else(|| "AskUserQuestion".to_string()),
        // Action-shaped tools (AgentControl/Channel/Team): the k=v fallback below takes the
        // map's first key, and serde_json orders keys alphabetically — `action` always wins and
        // the target never shows, so three rows aimed at three different instances read
        // identically. Name the action and who it is aimed at instead.
        (_, serde_json::Value::Object(map))
            if map.get("action").and_then(|a| a.as_str()).is_some() =>
        {
            let action = map
                .get("action")
                .and_then(|a| a.as_str())
                .unwrap_or_default();
            match TARGET_KEYS
                .iter()
                .find_map(|k| map.get(*k).and_then(|v| v.as_str()))
                .filter(|t| !t.is_empty())
            {
                Some(target) => format!("{action} {target}"),
                None => action.to_string(),
            }
        }
        (_, serde_json::Value::Object(map)) => map
            .iter()
            .take(1)
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(" "),
        (_, other) => other.to_string(),
    }
}

/// Clip tool results before feeding back to the model: overlong output is truncated with
/// a note (50k cap; simplified to truncation rather than spilling to disk + preview).
fn clipped_result(text: String) -> String {
    let total = text.chars().count();
    if total <= MAX_RESULT_CHARS {
        return text;
    }
    const TAIL_CHARS: usize = 1_000;
    let note = format!("\n…[truncated: {total} chars total]");
    let note_chars = note.chars().count();
    let tail_chars = TAIL_CHARS.min(MAX_RESULT_CHARS.saturating_sub(note_chars));
    let head_chars = MAX_RESULT_CHARS.saturating_sub(note_chars + tail_chars);
    let head: String = text.chars().take(head_chars).collect();
    let tail: String = text
        .chars()
        .rev()
        .take(tail_chars)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{head}{note}{tail}")
}

/// HTTP client for tools: creating one per turn would lose the connection pool (TLS
/// handshake from scratch); a single process-wide client is enough (clone just shares
/// the inner Arc).
static TOOL_HTTP: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();

fn tool_http() -> Result<reqwest::Client, QueryError> {
    if let Some(client) = TOOL_HTTP.get() {
        return Ok(client.clone());
    }
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| QueryError::Tool(ToolError::failed(e.to_string())))?;
    Ok(TOOL_HTTP.get_or_init(|| client).clone())
}

/// Tool execution context (cwd/registry/http shared by tool pool assembly and execution).
pub(crate) fn tool_context(
    session: &Session,
    host: &EngineHost,
) -> Result<ToolContext, QueryError> {
    Ok(ToolContext {
        cwd: session.cwd(),
        home: session.home.clone(),
        watch: session.watch.clone(),
        live: host.requests.live.clone(),
        http: tool_http()?,
        tasks: session.tasks.clone(),
        hooks: session.settings.hooks.clone(),
        permission_mode: permission_mode_str(session.permission_mode).to_string(),
        expand_tasks: session.expand_tasks.clone(),
        ask_question: host.requests.ask_question.clone(),
        instance: session.instance.clone(),
        rewind: session.runtime.rewind.clone(),
        // Session-scoped, like `rewind`: a subagent inherits the parent's handle in
        // `build_sub_session`, so the per-agent rate limit is one table for the whole
        // session rather than one per spawn.
    })
}

/// HTML entity escaping (escape `& < >` before wrapping bash-mode output,
/// preventing fake tags in the output from breaking the `<bash-stdout>` structure).
fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Placeholder result for unanswered tool_use blocks when interrupted.
const INTERRUPTED_TOOL_RESULT: &str =
    "<tool_use_error>interrupted by the user before this tool produced a result</tool_use_error>";

/// Recorded when the user stops a reply mid-stream. Model-facing text, verbatim CC: the
/// turn the user cut off has to say so in the history, or the model keeps answering a
/// question it never learned was withdrawn.
/// What the console's own `!` line calls its shell call.
///
/// The model never mints one: its call identifiers come from the provider. So
/// the prefix is what says a `Bash` call was standalone — the console's own line
/// rather than one the model made — which is the fold decision, and the only
/// place that fact travels once the call is an item.
pub const BASH_CALL_PREFIX: &str = "bash-";

pub const INTERRUPT_MARKER: &str = "[Request interrupted by user]";
/// Recorded when the interrupt landed while tools were running (the assistant message and
/// the filled tool_results are already in history). Model-facing text, verbatim CC.
pub const INTERRUPT_MARKER_TOOL_USE: &str = "[Request interrupted by user for tool use]";

/// Whether a message body is one of the interrupt markers (the render layers strip the
/// bubble off these: they are transcript facts, not something the user typed).
pub fn is_interrupt_marker(text: &str) -> bool {
    text == INTERRUPT_MARKER || text == INTERRUPT_MARKER_TOOL_USE
}

/// What survives from a reply the user cut off mid-stream: the text and the thinking that
/// finished being signed. A tool_use block has no result and an unsigned thinking block
/// fails signature verification on replay — either one would 400 every later request in
/// the session, so the partial reply is trimmed to what can be replayed.
fn interrupted_content(assistant: Message) -> Vec<ContentBlock> {
    assistant
        .content
        .into_iter()
        .filter(|block| match block {
            ContentBlock::Text { text } => !text.trim().is_empty(),
            ContentBlock::Thinking {
                thinking,
                signature,
            } => !thinking.trim().is_empty() && !signature.is_empty(),
            ContentBlock::ToolUse { .. }
            | ContentBlock::ToolResult { .. }
            | ContentBlock::Image { .. } => false,
        })
        .collect()
}

/// Close an interrupted turn honestly: keep whatever the model managed to say, then mark
/// the interruption. Empty partials are skipped — an empty assistant message is another
/// lie, and some endpoints reject it.
fn record_interrupt(
    session: &Session,
    messages: &mut Vec<Message>,
    partial: Option<Message>,
    marker: &'static str,
    host: &EngineHost,
) -> &'static str {
    if let Some(partial) = partial {
        let content = interrupted_content(partial);
        if !content.is_empty() {
            record(
                session,
                messages,
                Message {
                    role: Role::Assistant,
                    content,
                },
                host,
            );
        }
    }
    record(session, messages, Message::user_text(marker), host);
    marker
}

/// Whether blocks already contain a result for this tool_use.
fn answered(blocks: &[ContentBlock], tool_use_id: &str) -> bool {
    blocks
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolResult { tool_use_id: id, .. } if id == tool_use_id))
}

/// Add an is_error placeholder result for every tool_use not yet answered.
/// The API requires tool_use and tool_result to pair one-to-one within a request —
/// missing one makes every subsequent request carrying this history fail with 400.
fn fill_missing_tool_results(tool_uses: &[ContentBlock], blocks: &mut Vec<ContentBlock>) {
    for tool_use in tool_uses {
        let ContentBlock::ToolUse { id, .. } = tool_use else {
            continue;
        };
        if !answered(blocks, id) {
            blocks.push(tool_result_error(id, INTERRUPTED_TOOL_RESULT));
        }
    }
}

/// Append to history + persist to the transcript (fixed order: persist first, then
/// append, avoiding last().expect).
/// Record the message that opens a turn and open the rewind checkpoint it is
/// (D91). The marker goes down first, so the checkpoint is the user message's
/// own line; a transcript that cannot take it costs this turn its checkpoint,
/// never the turn itself.
fn record_turn_open(
    session: &Session,
    messages: &mut Vec<Message>,
    message: Message,
    host: &EngineHost,
) {
    match session.runtime.transcript.borrow().clone() {
        Some(transcript) => match transcript
            .append_turn(crate::channels::now_unix())
            .and_then(|()| transcript.line_count())
        {
            Ok(line) => session.runtime.rewind.open(
                crate::rewind::session_dir(&session.home, &transcript.name()),
                line,
            ),
            Err(_) => session.runtime.rewind.close(),
        },
        None => session.runtime.rewind.close(),
    }
    record(session, messages, message, host);
}

fn record(session: &Session, messages: &mut Vec<Message>, message: Message, host: &EngineHost) {
    if let Some(t) = session.runtime.transcript.borrow().clone()
        && let Err(e) = t.append(&message)
    {
        host.events.warn(format!("transcript append failed: {e}"));
    }
    messages.push(message);
}

/// Publish the turn's context measurement. Every exit of the loop reports it, so
/// the numbers are built in one place against the model in use right now — the
/// window on screen and the trigger the compactor obeys come from one resolver.
fn report_context_usage(session: &Session, host: &EngineHost, tokens: u64) {
    host.events.emit(EngineEvent::ContextUsage(
        crate::context_usage::ContextUsage::for_model(
            tokens,
            &session.client.models(),
            &session.runtime.model.borrow().clone(),
        ),
    ));
}

/// queryLoop: multi-turn tool loop until end_turn (the loop body shared by `run_query`
/// and `run_bash_command`). messages already contain this user input and the transcript
/// write. cancel: when Some, stream reads can be interrupted by a watch signal
/// (TUI Ctrl+C/Esc).
async fn query_loop(
    session: &Arc<Session>,
    mut messages: Vec<Message>,
    host: &EngineHost,
    tools: &[Box<dyn Tool>],
    ctx: &ToolContext,
    mut cancel_rx: Option<watch::Receiver<bool>>,
) -> Result<QueryOutcome, QueryError> {
    let mut recovery_count = 0u32;
    let mut empty_retry_count = 0u32;
    let mut stop_hook_fired = false;
    let mut gate = TokenGate::new();
    let mut inbox_wake = InboxWake::for_session(session);
    // The schemas every request in this loop carries: token measurements must
    // count the same payload, or they read under the real input size.
    let tool_schemas = tool_params(tools);
    normalize_synthetic_bash_calls(&mut messages);
    loop {
        if let Some(inbox) = inbox_wake.as_mut() {
            let items = inbox.take(session).await;
            if !items.is_empty() {
                let (prompt, images) =
                    crate::tool::agent::absorb_inbox(&session.channels, &inbox.instance, &items);
                host.events.emit(EngineEvent::Inbound(prompt.clone()));
                record(
                    session,
                    &mut messages,
                    user_message_with_images(
                        &prompt,
                        &images,
                        session.client.supports_images(),
                        &session.client.image_capable_providers(),
                    ),
                    host,
                );
            }
        }
        check_and_compact(
            session,
            &mut messages,
            &mut gate,
            &tool_schemas,
            &mut host.events.warn_sink(),
        )
        .await;
        // task_reminder: no Task tool for 10 turns + 10 turns since the last reminder.
        maybe_inject_task_reminder(session, &mut messages, host).await;
        // Recovery sweep: event-driven SendMessage claims idle recipients immediately, while
        // this catches mail left behind by a failed run or deposited through another path.
        crate::tool::agent::flush_agent_inbox(session, &ctx.watch);
        // Temporary hires are released once their task is done (D53) — after the flush, so a
        // follow-up sent in the previous round has already refilled the inbox and renewed the
        // lease. Only fires in a project whose crew is up; elsewhere the sweep is a no-op.
        //
        // Main sweeps, and only main: every instance shares this registry, so letting a
        // subagent's own loop run it would have hires releasing each other — and themselves.
        let released = if session.instance.is_none() {
            session.agents.release_hires().await
        } else {
            Vec::new()
        };
        // Background task notification injection (dynamic awareness while running): before
        // each reasoning step, pending state-transition notifications (rounds/completion/
        // failure) are injected into the context; anything unconsumed by the end of the
        // turn carries over to the next turn.
        let mut notes = session
            .watch
            .consume_notifications(session.instance.as_deref())
            .await;
        // Named rather than swept silently: without this main's next SendMessage to a
        // released hire fails with "no subagent named …", which reads as a bug rather than
        // as the lifetime it agreed to.
        if !released.is_empty() {
            notes.push(format!(
                "released temporary hire(s) {} — their task is done and their result is in. \
                 Hire again if more of that work comes up; the crew is unaffected.",
                released.join(", ")
            ));
        }
        if !notes.is_empty() {
            // record, not push: the model sees this message, so the canonical
            // transcript must carry it too — a reload that lacks it diverges
            // from the provider's cached prefix and from compact kept-counts.
            record(
                session,
                &mut messages,
                Message::user_text(format!(
                    "<task-notifications>\n{}\n</task-notifications>",
                    notes.join("\n")
                )),
                host,
            );
        }
        // The main agent's inbox (D98): room relays it is a member of, plus direct
        // messages an agent sent it, batched at turn boundaries, in order. One
        // store, one drain, one block — the marker on each line says which kind
        // it is, and `app::projection::line_source` is what reads those markers back.
        //
        // Guarded on the main session: the registry is shared with every
        // subagent, so an unguarded drain let a subagent's own turn boundary eat
        // mail addressed to main.
        let mail = if session.instance.is_none() {
            // v7: nothing is held back, so this is the same absorption every
            // other member does at its own tool boundary — a running main
            // steers on what landed while it worked.
            session.channels.drain_main_mail().await
        } else {
            Vec::new()
        };
        if !mail.is_empty() {
            record(
                session,
                &mut messages,
                Message::user_text(format!(
                    "{MAIL_BLOCK_OPEN}\n{}\n{MAIL_BLOCK_CLOSE}",
                    mail.join("\n")
                )),
                host,
            );
        }
        let context_tokens = gate.current(crate::compact::estimate_tokens(
            &session.system,
            &messages,
            &tool_schemas,
        ));
        report_context_usage(session, host, context_tokens);
        let turn = match one_turn_with_stream_retries(
            session,
            &messages,
            tools,
            host,
            cancel_rx.as_mut(),
            inbox_wake.as_mut(),
        )
        .await
        {
            Err(error @ QueryError::Client(ClientError::ContextOverflow { .. })) => {
                if !compact_after_overflow(
                    session,
                    &mut messages,
                    &mut gate,
                    &mut host.events.warn_sink(),
                )
                .await
                {
                    if let Some(inbox) = inbox_wake.as_mut() {
                        inbox.restore(session);
                    }
                    return Err(error);
                }
                retry_after_overflow(
                    session,
                    &messages,
                    tools,
                    host,
                    cancel_rx.as_mut(),
                    inbox_wake.as_mut(),
                )
                .await?
            }
            Err(error) => {
                if let Some(inbox) = inbox_wake.as_mut() {
                    inbox.restore(session);
                }
                return Err(error);
            }
            outcome => outcome?,
        };
        if turn.aborted {
            // Interrupted mid-stream: what the model already said stays (the user is
            // looking at it), followed by the marker that says it was cut off. Discarding
            // the turn instead left the screen and the history telling different stories.
            let marker = record_interrupt(
                session,
                &mut messages,
                Some(turn.assistant),
                INTERRUPT_MARKER,
                host,
            );
            if !session.quiet {
                println!();
            }
            return Ok(QueryOutcome {
                messages,
                end_reason: QueryEndReason::Completed,
                aborted: true,
                interrupt_marker: Some(marker),
            });
        }
        // A turn nobody downstream can read: thinking counts as nothing, because its text
        // never leaves the model's own head. A turn the output budget cut off mid-thought
        // reads identically here and is not the same thing — that is truncation, and it
        // belongs to the max_tokens recovery below, which this classifier used to shadow
        // (D73's leftover: the thinking-only truncated turn was discarded whole).
        let empty_assistant = turn.assistant.content.iter().all(|block| match block {
            ContentBlock::Text { text } => text.trim().is_empty(),
            ContentBlock::Thinking { .. } => true,
            ContentBlock::ToolUse { .. } => false,
            ContentBlock::ToolResult { .. } | ContentBlock::Image { .. } => true,
        });
        let truncated = turn.stop_reason.as_deref() == Some("max_tokens");
        if turn.tool_uses.is_empty() && empty_assistant && !truncated {
            if empty_retry_count == 0 {
                empty_retry_count = 1;
                // Not gated on `quiet`, which is the framing contract for this
                // run's *stdout* and says nothing about who may hear that the
                // model answered with nothing. The second warning — raised once
                // the run ends — never was gated, so gating the first one meant a
                // wire client heard the retry's conclusion and not its cause.
                host.events
                    .warn("model returned an empty response; retrying once".to_string());
                continue;
            }
            // Twice over is a decision, not a broken stream: a member draining room lines
            // it owes nothing is *told* to end its turn this way (`CHANNEL_NOTE`), and any
            // session can be woken by an inbox with nothing in it to answer. Failing the
            // turn there restored the inbox and had the same batch redelivered into the
            // same silence, once per chase round, under a message that named the transport
            // (D124). The turn ends instead — silent, reported, and not written to history,
            // for the same reason the first attempt is not.
            if !session.quiet {
                println!();
            }
            let context_tokens = gate.current(crate::compact::estimate_tokens(
                &session.system,
                &messages,
                &tool_schemas,
            ));
            report_context_usage(session, host, context_tokens);
            return Ok(QueryOutcome {
                messages,
                end_reason: QueryEndReason::EmptyResponseRetried,
                aborted: false,
                interrupt_marker: None,
            });
        }
        // The assistant message must enter history before branching: max_tokens recovery
        // and the Stop hook both need the model to see the truncated content, and a normal
        // end must hand the turn's conclusion to downstream.
        record(session, &mut messages, turn.assistant, host);
        if turn.tool_uses.is_empty() {
            // Output budget truncation recovery: inject a "continue" message and retry (max 3 times).
            if turn.stop_reason.as_deref() == Some("max_tokens")
                && recovery_count < MAX_OUTPUT_TOKENS_RECOVERY_LIMIT
            {
                recovery_count += 1;
                host.events.emit(EngineEvent::RoundEnd);
                record(
                    session,
                    &mut messages,
                    Message::user_text(MAX_TOKENS_RESUME_PROMPT),
                    host,
                );
                continue;
            }
            // Stop hooks: exit 2 → inject the blocking stderr into the model and retry once (loop guard).
            if !stop_hook_fired
                && let Some(blocking) = run_stop_hooks(
                    &session.settings.hooks,
                    permission_mode_str(session.permission_mode),
                    &ctx.cwd,
                )
                .await
            {
                stop_hook_fired = true;
                host.events.emit(EngineEvent::RoundEnd);
                record(
                    session,
                    &mut messages,
                    Message::user_text(format!("(Stop hook blocked continuation)\n{blocking}")),
                    host,
                );
                continue;
            }
            if !session.quiet {
                println!();
            }
            let end_reason = if empty_retry_count > 0 {
                QueryEndReason::EmptyResponseRetried
            } else {
                QueryEndReason::Completed
            };
            let context_tokens = gate.current(crate::compact::estimate_tokens(
                &session.system,
                &messages,
                &tool_schemas,
            ));
            report_context_usage(session, host, context_tokens);
            return Ok(QueryOutcome {
                messages,
                end_reason,
                aborted: false,
                interrupt_marker: None,
            });
        }

        // Phase 1: run each tool through the permission gate (serial, possibly interactive;
        // hooks may rewrite input)
        let mut pending: Vec<PendingCall> = Vec::new();
        let mut blocks: Vec<ContentBlock> = Vec::new();
        for tool_use in &turn.tool_uses {
            let (id, name, input) = match tool_use {
                ContentBlock::ToolUse { id, name, input } => {
                    (id.clone(), name.clone(), input.clone())
                }
                _ => unreachable!(),
            };
            let Some(tool) = find_tool(tools, &name) else {
                blocks.push(tool_result_error(
                    &id,
                    format!("<tool_use_error>No such tool: {name}</tool_use_error>"),
                ));
                continue;
            };
            // AskUserQuestion: asking the user is itself the interaction (the dialog is the
            // approval), no permission gate.
            let GateDecision {
                behavior,
                reason,
                input: gated_input,
                guidance,
            } = if name == "AskUserQuestion" {
                GateDecision::settled(PermissionBehavior::Allow, String::new(), input.clone())
            } else {
                gate_tool(
                    tool,
                    &input,
                    session.permission_mode,
                    &session.settings.hooks,
                    &session.runtime.permissions,
                    &*host.requests.ask,
                    &ctx.cwd,
                )
                .await
            };
            match behavior {
                PermissionBehavior::Allow => pending.push(PendingCall {
                    tool_use_id: id,
                    tool,
                    input: gated_input,
                }),
                PermissionBehavior::Deny => {
                    let guidance = guidance.as_deref();
                    let denial = permission_denial(&format!("{name} ({reason})"), guidance);
                    blocks.push(tool_result_error(
                        &id,
                        format!("<permission_error>{denial}</permission_error>"),
                    ));
                    // Denied tools also need UI closure: the tool row shows "denied"
                    // instead of spinning forever.
                    let summary = summarize_input(&name, &input);
                    host.events.emit(EngineEvent::ToolDone(ToolCallDone {
                        tool_call_id: id,
                        name,
                        summary,
                        output: permission_denial(&reason, guidance),
                        status: ToolCallStatus::Error,
                        diff: None,
                        duration_ms: 0,
                    }));
                }
                PermissionBehavior::Ask => unreachable!("ask resolved by gate_tool"),
            }
        }

        // Phase 2: queue execution (safe parallel / non-safe serial).
        // Interrupt semantics: stop immediately on signal — in-flight tools are cancelled
        // (future drop), not-yet-started ones never run; completed ones keep their real
        // results, unanswered ones get an is_error placeholder, guaranteeing every tool_use
        // has a paired tool_result (otherwise the history 400s forever).
        let mut stop_after_tools = false;
        let (outcomes, interrupted) = execute_calls(pending, ctx, cancel_rx.as_mut()).await;
        for outcome in outcomes {
            let tool_use = turn.tool_uses.iter().find(
                |t| matches!(t, ContentBlock::ToolUse { id, .. } if id == &outcome.tool_use_id),
            );
            let Some(ContentBlock::ToolUse { name, input, .. }) = tool_use else {
                continue;
            };
            match outcome.result {
                Ok(result) => {
                    host.events.emit(EngineEvent::ToolDone(ToolCallDone {
                        tool_call_id: outcome.tool_use_id.clone(),
                        name: name.clone(),
                        summary: summarize_input(name, input),
                        output: clipped_result(render_result(&result)),
                        status: if result.is_error {
                            ToolCallStatus::Error
                        } else {
                            ToolCallStatus::Done
                        },
                        diff: result.diff.clone(),
                        duration_ms: outcome.duration_ms,
                    }));
                    blocks.push(result_block(&outcome.tool_use_id, &result));
                    if !interrupted {
                        // PostToolUse exit 2 → stop continuing (the hook's blocking error semantics).
                        stop_after_tools |= run_post_tool_use(
                            &session.settings.hooks,
                            name,
                            input,
                            &result.content,
                            permission_mode_str(session.permission_mode),
                            &ctx.cwd,
                        )
                        .await;
                    }
                }
                Err(e) => {
                    // Failures also need UI closure: otherwise the tool row spins forever
                    // and the user never sees the failure.
                    host.events.emit(EngineEvent::ToolDone(ToolCallDone {
                        tool_call_id: outcome.tool_use_id.clone(),
                        name: name.clone(),
                        summary: summarize_input(name, input),
                        output: e.to_string(),
                        status: ToolCallStatus::Error,
                        diff: None,
                        duration_ms: outcome.duration_ms,
                    }));
                    blocks.push(tool_result_error(
                        &outcome.tool_use_id,
                        format!("<tool_use_error>{e}</tool_use_error>"),
                    ));
                }
            }
        }

        if interrupted {
            // Close the rows of tools that never ran as "interrupted": no dangling spinner,
            // no false completion.
            for tool_use in &turn.tool_uses {
                let ContentBlock::ToolUse { id, name, input } = tool_use else {
                    continue;
                };
                if answered(&blocks, id) {
                    continue;
                }
                host.events.emit(EngineEvent::ToolDone(ToolCallDone {
                    tool_call_id: id.clone(),
                    name: name.clone(),
                    summary: summarize_input(name, input),
                    output: "interrupted".to_string(),
                    status: ToolCallStatus::Interrupted,
                    diff: None,
                    duration_ms: 0,
                }));
            }
        }
        // Fill every unanswered tool_use: returning early on the interrupt path would leave
        // orphan tool_use blocks in the transcript, and every future restore from history
        // would 400, permanently corrupting the session.
        fill_missing_tool_results(&turn.tool_uses, &mut blocks);
        // The tool barrier (D83): results are assembled and the next request has not gone
        // out yet, which is the one moment a correction can still change what the model
        // does with them. Anything the user typed while this turn worked rides along in
        // this same user message — appended *after* the tool_results, which the API
        // requires to come first — so the model reads it before deciding the next step.
        //
        // Only a turn that is going to ask again may take them. An interrupt, a blocking
        // Stop hook and a cancel between rounds all end the turn here, and a message
        // folded into a request that is never sent would be a message swallowed: those
        // stay in the composer's queue, where TurnEnd hands them to the next turn.
        if !interrupted && !stop_after_tools && !is_cancelled(&cancel_rx) {
            blocks.extend(
                (host.requests.steer)()
                    .await
                    .iter()
                    .map(|item| ContentBlock::Text {
                        text: item.block_text(),
                    }),
            );
        }
        record(
            session,
            &mut messages,
            Message {
                role: Role::User,
                content: blocks,
            },
            host,
        );
        if interrupted {
            // The assistant message and every tool_result are already in history; all the
            // model still lacks is that the stop was the user's doing.
            let marker = record_interrupt(
                session,
                &mut messages,
                None,
                INTERRUPT_MARKER_TOOL_USE,
                host,
            );
            if !session.quiet {
                println!();
            }
            let context_tokens = gate.current(crate::compact::estimate_tokens(
                &session.system,
                &messages,
                &tool_schemas,
            ));
            report_context_usage(session, host, context_tokens);
            return Ok(QueryOutcome {
                messages,
                end_reason: QueryEndReason::Completed,
                aborted: true,
                interrupt_marker: Some(marker),
            });
        }
        // All tools in this batch are closed: RoundEnd only marks a batch boundary (image
        // warm-up etc.); fold groups are bounded by text — tools across turns stay in the
        // same fold group.
        host.events.emit(EngineEvent::RoundEnd);
        if stop_after_tools || is_cancelled(&cancel_rx) {
            // The cancel landed between the last tool finishing and the next round: no
            // tool row was cut short, but the turn still stops on the user's word and the
            // model is owed the same marker. A Stop-hook halt is not an interrupt.
            let marker = is_cancelled(&cancel_rx).then(|| {
                record_interrupt(
                    session,
                    &mut messages,
                    None,
                    INTERRUPT_MARKER_TOOL_USE,
                    host,
                )
            });
            let context_tokens = gate.current(crate::compact::estimate_tokens(
                &session.system,
                &messages,
                &tool_schemas,
            ));
            report_context_usage(session, host, context_tokens);
            return Ok(QueryOutcome {
                messages,
                end_reason: if empty_retry_count > 0 {
                    QueryEndReason::EmptyResponseRetried
                } else {
                    QueryEndReason::Completed
                },
                aborted: is_cancelled(&cancel_rx),
                interrupt_marker: marker,
            });
        }
    }
}

/// One query (multi-turn tool loop): UserPromptSubmit hook + user input into history +
/// the loop body. cancel: when Some, stream reads can be interrupted by a watch signal
/// (TUI Ctrl+C/Esc). images: image attachments mounted in the message box; when the
/// current provider supports them (`supportsImages`), appended as image content blocks
/// after the text block, otherwise text only (placeholders stay in the body).
pub async fn run_query(
    session: &Arc<Session>,
    initial_messages: Vec<Message>,
    user_input: &str,
    images: &[crate::api::types::ImageAttachment],
    host: &EngineHost,
    cancel: Option<watch::Receiver<bool>>,
) -> Result<QueryOutcome, QueryError> {
    claim_run(host).await?;
    let tools = crate::tools::assemble_tools(session, &mut host.events.warn_sink()).await;
    let ctx = tool_context(session, host)?;

    // UserPromptSubmit: the hook may block this submission.
    if run_user_prompt_submit(
        &session.settings.hooks,
        user_input,
        permission_mode_str(session.permission_mode),
        &ctx.cwd,
    )
    .await
    {
        return Ok(QueryOutcome {
            messages: initial_messages,
            end_reason: QueryEndReason::Completed,
            aborted: false,
            interrupt_marker: None,
        });
    }

    let mut messages = initial_messages;
    // Recalled context rides the tail of the user turn — the one position
    // that never disturbs the cached request prefix — and is recorded with
    // it, so what the model saw is what every reload replays (D75).
    let user_input = match recall_context(session, user_input) {
        Some(recalled) => format!("{user_input}\n\n{recalled}"),
        None => user_input.to_string(),
    };
    host.events.emit(EngineEvent::Inbound(user_input.clone()));
    record_turn_open(
        session,
        &mut messages,
        user_message_with_images(
            &user_input,
            images,
            session.client.supports_images(),
            &session.client.image_capable_providers(),
        ),
        host,
    );
    query_loop(session, messages, host, &tools, &ctx, cancel).await
}

/// One host, one run (spec "Turn and round"; B3).
///
/// The host carries the turn its reports belong to, and the actor hands that turn
/// to exactly one run. A second run on the same host would interleave two
/// attempts into one item stream, so it is refused here rather than allowed to
/// write a history nobody can read back.
async fn claim_run(host: &EngineHost) -> Result<(), QueryError> {
    if host.begin_run().await {
        return Ok(());
    }
    Err(QueryError::Protocol(
        "this run's turn already has a run".to_string(),
    ))
}

/// BM25 recall over this project's committed experiences and extracted memory
/// facts (D75): the few entries relevant to what the user just said, surfaced
/// without waiting for the model to think of querying. Active experiences only
/// — injecting a known-stale pattern unprompted would be advice against the
/// record.
fn recall_context(session: &Session, user_input: &str) -> Option<String> {
    /// At most this many recalled lines per turn: recall is a hint, not a
    /// second system prompt.
    const RECALL_LIMIT: usize = 3;
    if user_input.trim().is_empty() {
        return None;
    }
    let cwd = session.cwd();
    let mut docs = Vec::new();
    let mut lines = Vec::new();
    let key = crate::experience::project_key(&cwd);
    for entry in crate::experience::load_entries(&session.home, &key) {
        if entry.status != crate::experience::ExperienceStatus::Active {
            continue;
        }
        docs.push(crate::experience::entry_document(&entry));
        let short = entry.id.chars().take(4).collect::<String>();
        lines.push(format!(
            "- experience E{short}: {} (full steps via ExperienceQuery)",
            entry.summary
        ));
    }
    if let Some(memory) = crate::memory::load_project_memory(&session.home, &cwd) {
        for fact in memory.lines().map(str::trim).filter(|l| !l.is_empty()) {
            docs.push(crate::bm25::Document::default().field(fact, 1.0));
            lines.push(format!("- memory: {fact}"));
        }
    }
    let ranked = crate::bm25::Bm25::new(docs).rank(user_input, RECALL_LIMIT);
    if ranked.is_empty() {
        return None;
    }
    let recalled: Vec<&str> = ranked
        .iter()
        .map(|(index, _)| lines[*index].as_str())
        .collect();
    Some(format!(
        "<system-reminder>\nPossibly relevant project context, recalled by keyword match — verify \
         before relying on it; after applying an experience, record the observed outcome with \
         ExperienceOutcome:\n{}\n</system-reminder>",
        recalled.join("\n")
    ))
}

/// User input → message: text block first, image blocks after (when the provider supports
/// them). The text keeps `#[image N]` placeholders so the model senses the images through them.
///
/// When a placeholder arrives without its image, say why. A bare dangling marker reads to the
/// model as an image it merely failed to locate, and it will go looking — through the
/// transcript, through temp directories — instead of telling the user what is actually wrong.
fn user_message_with_images(
    text: &str,
    images: &[crate::api::types::ImageAttachment],
    send_images: bool,
    image_providers: &[String],
) -> Message {
    use crate::api::types::{ContentBlock, ImageSource, Role};
    let attaching = send_images && !images.is_empty();
    let mut body = text.to_string();
    if !attaching && crate::api::image::has_marker(text) {
        body.push_str(&missing_image_note(images.is_empty(), image_providers));
    }
    let mut content = vec![ContentBlock::Text { text: body }];
    if attaching {
        content.extend(images.iter().map(|img| ContentBlock::Image {
            source: ImageSource::base64(&img.media_type, &img.data),
        }));
    }
    Message {
        role: Role::User,
        content,
    }
}

/// Explains a placeholder whose image is not attached, and — when the attachment exists and only
/// this endpoint can't take it — points at the way through instead of the way out: a subagent
/// forked onto an image-capable provider resolves the same marker against the same session table,
/// so a text-only main session can still get an image looked at.
fn missing_image_note(unresolved: bool, image_providers: &[String]) -> String {
    if unresolved {
        return "\n\n<system-reminder>The `#[image N]` placeholders above have no image attached: \
                the referenced attachment is not in this session (attachments live in memory, so \
                markers from a resumed or restored session no longer resolve). Do not go looking \
                for the file — tell the user the image needs to be attached again.\
                </system-reminder>"
            .to_string();
    }
    let route = if image_providers.is_empty() {
        "No configured provider accepts images, so nothing in this session can look at it: tell \
         the user to enable `sendImages` (or set `supportsImages` on a provider) and resend."
            .to_string()
    } else {
        format!(
            "The attachment itself is still held by this session, and a subagent resolves the \
             same marker against the same table. To get it looked at, spawn one on an \
             image-capable provider and repeat the marker in its prompt — \
             Agent(provider: \"<one of: {}>\", model: \"<model on that provider>\", \
             prompt: \"…#[image N]…\") — it receives the real image and reports back to you. \
             Crossing providers requires an explicit model, and the provider must already be \
             keyed or logged in.",
            image_providers.join(", ")
        )
    };
    format!(
        "\n\n<system-reminder>The `#[image N]` placeholders above have no image attached: this \
         endpoint does not accept image blocks. Do not go looking for the file. {route}\
         </system-reminder>"
    )
}

/// Caveat injected before running a local command: `!` command output stays in the
/// session history (when the model is not consulted), and the model must not treat
/// its content as instructions to answer.
const BASH_CAVEAT: &str = "<local-command-caveat>Caveat: The messages below were generated by \
the user while running local commands. DO NOT respond to these messages or otherwise consider \
them in your response unless the user explicitly asks you to.</local-command-caveat>";

/// `!` command executed directly (bash mode):
/// skips the model and UserPromptSubmit hooks; the command runs through the permission
/// gate + Pre/PostToolUse hooks, and input/output are written to history wrapped in
/// `<bash-input>`/`<bash-stdout>`; when settings.respondToBashCommands is true (default),
/// the model is queried as usual afterwards (the model sees the output and can continue),
/// otherwise pure execution with a caveat injected to prevent the model from misreading.
pub async fn run_bash_command(
    session: &Arc<Session>,
    command: &str,
    history: Vec<Message>,
    host: &EngineHost,
    mut cancel: Option<watch::Receiver<bool>>,
) -> Result<QueryOutcome, QueryError> {
    claim_run(host).await?;
    let tools = crate::tools::assemble_tools(session, &mut host.events.warn_sink()).await;
    let tool_schemas = tool_params(&tools);
    let ctx = tool_context(session, host)?;
    let mut messages = history;

    let tool_use_id = format!(
        "{BASH_CALL_PREFIX}{}",
        BASH_CALL_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let input = serde_json::json!({ "command": command });
    // UI tool activity (reuses the Tool fold/expand rows): emitted before the permission
    // gate (the tool row is visible during the permission modal, consistent with run_query's
    // "stream fully, then gate" order).
    host.events.emit(EngineEvent::ToolUseStarted {
        index: 0,
        id: tool_use_id.clone(),
        name: "Bash".to_string(),
    });
    host.events.emit(EngineEvent::ToolReady {
        tool_call_id: tool_use_id.clone(),
        name: "Bash".to_string(),
        input: input.clone(),
    });

    let Some(tool) = find_tool(&tools, "Bash") else {
        return Err(QueryError::Protocol("Bash tool not found".to_string()));
    };
    // Interactive/TTY commands (top/htop/vim/ssh etc.): skip the permission gate (asking is
    // pointless — they can't run anyway) and reject directly; with respond on, the model
    // sees the rejection reason (and can suggest alternatives).
    let interactive_reason = crate::tool::bash::interactive_command_reason(command);
    let (text, is_error, duration_ms) = match interactive_reason {
        Some(reason) => {
            let err = format!("interactive command not allowed: {reason}");
            // Fold rows cannot be expanded after being persisted in inline mode — the
            // rejection reason is shown directly as a warning line.
            host.events.warn(err.clone());
            (err, true, 0)
        }
        None => {
            let GateDecision {
                behavior,
                reason,
                input: gated_input,
                guidance,
            } = gate_tool(
                tool,
                &input,
                session.permission_mode,
                &session.settings.hooks,
                &session.runtime.permissions,
                &*host.requests.ask,
                &ctx.cwd,
            )
            .await;
            match behavior {
                PermissionBehavior::Allow => {
                    let (outcomes, interrupted) = execute_calls(
                        vec![PendingCall {
                            tool_use_id: tool_use_id.clone(),
                            tool,
                            input: gated_input,
                        }],
                        &ctx,
                        cancel.as_mut(),
                    )
                    .await;
                    // Interruption or (shouldn't happen) empty results both close as
                    // interrupted: the `!` command's tool_use is not yet in history, so
                    // returning directly leaves no orphans.
                    let Some(outcome) = outcomes.into_iter().next().filter(|_| !interrupted) else {
                        // The row must close with the turn: a tool left Running keeps its
                        // message from ever settling, and the session's whole flush prefix
                        // with it.
                        host.events.emit(EngineEvent::ToolDone(ToolCallDone {
                            tool_call_id: tool_use_id,
                            name: "Bash".to_string(),
                            summary: format!("$ {command}"),
                            output: "interrupted".to_string(),
                            status: ToolCallStatus::Interrupted,
                            diff: None,
                            duration_ms: 0,
                        }));
                        host.events.emit(EngineEvent::RoundEnd);
                        let marker =
                            record_interrupt(session, &mut messages, None, INTERRUPT_MARKER, host);
                        let context_tokens = crate::compact::estimate_tokens(
                            &session.system,
                            &messages,
                            &tool_schemas,
                        );
                        report_context_usage(session, host, context_tokens);
                        return Ok(QueryOutcome {
                            messages,
                            end_reason: QueryEndReason::Completed,
                            aborted: true,
                            interrupt_marker: Some(marker),
                        });
                    };
                    match outcome.result {
                        Ok(result) => {
                            let text = clipped_result(render_result(&result));
                            (text, result.is_error, outcome.duration_ms)
                        }
                        Err(e) => (format!("Command failed: {e}"), true, outcome.duration_ms),
                    }
                }
                PermissionBehavior::Deny => {
                    // The `!` path has no tool_result to wrap: the same sentence
                    // reaches the model inside the `<bash-stderr>` block below.
                    let err = permission_denial(&format!("Bash ({reason})"), guidance.as_deref());
                    (err, true, 0)
                }
                PermissionBehavior::Ask => unreachable!("ask resolved by gate_tool"),
            }
        }
    };
    host.events.emit(EngineEvent::ToolDone(ToolCallDone {
        tool_call_id: tool_use_id,
        name: "Bash".to_string(),
        summary: format!("$ {command}"),
        output: text.clone(),
        status: if is_error {
            ToolCallStatus::Error
        } else {
            ToolCallStatus::Done
        },
        diff: None,
        duration_ms,
    }));
    host.events.emit(EngineEvent::RoundEnd);

    // Command + output as a single user message. A fabricated assistant ToolUse is
    // deliberately avoided: in thinking mode the API requires every assistant message
    // to carry its thinking block unchanged, and a synthetic tool call has none.
    let output = if is_error {
        format!("<bash-stderr>{}</bash-stderr>", escape_xml(&text))
    } else {
        format!("<bash-stdout>{}</bash-stdout>", escape_xml(&text))
    };
    let mut added: Vec<Message> = Vec::new();
    added.push(Message::user_text(format!(
        "<bash-input>{command}</bash-input>\n{output}"
    )));

    let stop = run_post_tool_use(
        &session.settings.hooks,
        "Bash",
        &input,
        &serde_json::Value::String(text),
        permission_mode_str(session.permission_mode),
        &ctx.cwd,
    )
    .await;
    let respond = session.settings.respond_to_bash_commands.unwrap_or(true)
        && !stop
        && !is_cancelled(&cancel);
    if !respond {
        // Model not consulted: output stays in the history with a caveat injected so the
        // model does not treat it as instructions.
        added.insert(0, Message::user_text(BASH_CAVEAT));
    }
    messages.extend(added.clone());
    if let Some(t) = session.runtime.transcript.borrow().clone() {
        for m in &added {
            if let Err(e) = t.append(m) {
                host.events.warn(format!("transcript append failed: {e}"));
            }
        }
    }
    if !respond {
        // `respond` is off for three reasons; only the interrupt owes the model a marker
        // (the setting and the Stop hook are not the user pressing Esc).
        let marker = is_cancelled(&cancel)
            .then(|| record_interrupt(session, &mut messages, None, INTERRUPT_MARKER, host));
        let context_tokens =
            crate::compact::estimate_tokens(&session.system, &messages, &tool_schemas);
        report_context_usage(session, host, context_tokens);
        return Ok(QueryOutcome {
            messages,
            end_reason: QueryEndReason::Completed,
            aborted: is_cancelled(&cancel),
            interrupt_marker: marker,
        });
    }
    query_loop(session, messages, host, &tools, &ctx, cancel).await
}

/// Monotonic tool_use_id sequence for `!` commands (unique across turns, no clash with
/// old pairs in the transcript).
static BASH_CALL_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Fold pre-normalization bash-turn history back into the modern shape. Old
/// transcripts carry `user(<bash-input>) → assistant(ToolUse "bash-N") → user(ToolResult)`;
/// the synthetic assistant message has no thinking block, which thinking-mode endpoints
/// reject ("content[].thinking must be passed back"). The pair merges into the input
/// message; model-generated tool calls (ids not prefixed `bash-`) are never touched.
fn normalize_synthetic_bash_calls(messages: &mut Vec<Message>) {
    use crate::api::types::{ContentBlock, Role};
    let mut i = 0;
    while i < messages.len() {
        // Every block lookup goes through `first()`: a content-free message is a shape the
        // transcript really carries (a model turn that streamed nothing), and indexing it
        // panicked the whole turn inside the spawned task, latching the TUI as busy forever.
        let is_bash_input = messages[i].role == Role::User
            && matches!(
                messages[i].content.first(),
                Some(ContentBlock::Text { text }) if text.contains("<bash-input>")
            );
        let synthetic = is_bash_input
            && messages.get(i + 1).is_some_and(|m| {
                m.role == Role::Assistant
                    && !m.content.is_empty()
                    && m.content.iter().all(|b| {
                        matches!(b, ContentBlock::ToolUse { id, .. } if id.starts_with("bash-"))
                    })
            })
            && messages.get(i + 2).is_some_and(|m| {
                m.role == Role::User
                    && matches!(
                        m.content.first(),
                        Some(ContentBlock::ToolResult { tool_use_id, .. })
                            if tool_use_id.starts_with("bash-")
                    )
            });
        if synthetic {
            let input_text = match messages[i].content.first() {
                Some(ContentBlock::Text { text }) => text.clone(),
                _ => String::new(),
            };
            let result_text = match messages[i + 2].content.first() {
                Some(ContentBlock::ToolResult { content, .. }) => {
                    crate::api::types::tool_result_text(content)
                }
                _ => String::new(),
            };
            messages[i] = Message::user_text(format!("{input_text}\n{result_text}"));
            messages.drain(i + 1..=i + 2);
        }
        i += 1;
    }
}

fn is_cancelled(cancel: &Option<watch::Receiver<bool>>) -> bool {
    cancel.as_ref().is_some_and(|rx| *rx.borrow())
}

#[cfg(test)]
use crate::transcript::Transcript;

#[cfg(test)]
#[path = "query_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "query_steer_tests.rs"]
mod steer_tests;
