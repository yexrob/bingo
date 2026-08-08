use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;

use futures_util::StreamExt;
use thiserror::Error;
use tokio::sync::watch;

use crate::api::client::{AssistantAccumulator, Client, ClientError};
use crate::api::contract::{NeutralRequest, StreamEvent, SystemBlock, ThinkingLevel};
use crate::api::types::{ContentBlock, DEFAULT_MAX_TOKENS, Message, Role};
use crate::budget::MAX_RESULT_CHARS;
use crate::compact::{TokenGate, check_and_compact};
use crate::error::ErrorCode;
use crate::hooks::{run_post_tool_use, run_pre_tool_use, run_stop_hooks, run_user_prompt_submit};
use crate::permission::{PermissionBehavior, PermissionMode, can_use_tool};
use crate::settings::{HooksConfig, Settings};
use crate::tool::executor::{PendingCall, cancel_requested, execute_calls};
use crate::tool::{Tool, ToolContext, ToolError, ToolResult, find_tool, tool_params};
use crate::transcript::Transcript;

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

/// Result of a query.
#[derive(Debug)]
pub struct QueryOutcome {
    pub messages: Vec<Message>,
    /// Turn aborted by the user (stream stopped; tools that already ran finish normally).
    pub aborted: bool,
}

/// Recovery injection after max_tokens truncation.
const MAX_OUTPUT_TOKENS_RECOVERY_LIMIT: u32 = 3;
const MAX_TOKENS_RESUME_PROMPT: &str =
    "Output token limit hit. Resume directly from where you left off. Do not apologize or explain.";

/// Task reminder thresholds (TURNS_SINCE_WRITE / TURNS_BETWEEN_REMINDERS).
const TASK_REMINDER_TURNS: u64 = 10;
const TASK_REMINDER_MARKER: &str = "[SYSTEM NOTIFICATION - TASK REMINDER]";

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
async fn maybe_inject_task_reminder(session: &Session, messages: &mut Vec<Message>) {
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
    messages.push(Message::user_text(text));
}

/// Session runtime mutable via slash commands (/model /clear /resume /permissions):
/// watch channels are read by the query loop each turn.
#[derive(Clone)]
pub struct Runtime {
    pub model_tx: watch::Sender<String>,
    pub model: watch::Receiver<String>,
    pub transcript_tx: watch::Sender<Option<Transcript>>,
    pub transcript: watch::Receiver<Option<Transcript>>,
    /// Runtime permission rules table (modified via /permissions; initially from settings).
    pub permissions: Arc<std::sync::Mutex<crate::settings::PermissionRules>>,
    /// Current provider (/provider switch; "default" = top-level apiKey/apiBaseUrl/env).
    pub provider_tx: watch::Sender<String>,
    pub provider: watch::Receiver<String>,
    /// Current thinking level (/think switch; None = no thinking parameter sent).
    pub thinking_tx: watch::Sender<Option<String>>,
    pub thinking: watch::Receiver<Option<String>>,
    /// MCP connection manager (lazy connection cache; initialized from settings at main
    /// construction; tests default to an empty manager — no MCP tools, behavior unchanged).
    pub mcp: Arc<tokio::sync::Mutex<crate::mcp::McpManager>>,
}

impl Runtime {
    pub fn new(
        model: String,
        transcript: Option<Transcript>,
        permissions: crate::settings::PermissionRules,
    ) -> Self {
        let (model_tx, model) = watch::channel(model);
        let (transcript_tx, transcript) = watch::channel(transcript);
        let (provider_tx, provider) = watch::channel("default".to_string());
        let (thinking_tx, thinking) = watch::channel(None);
        Self {
            model_tx,
            model,
            transcript_tx,
            transcript,
            permissions: Arc::new(std::sync::Mutex::new(permissions)),
            provider_tx,
            provider,
            thinking_tx,
            thinking,
            mcp: Arc::new(tokio::sync::Mutex::new(crate::mcp::McpManager::new(
                HashMap::new(),
                Default::default(),
            ))),
        }
    }
}

/// Full context of a query (shared by TUI and headless).
#[derive(Clone)]
pub struct Session {
    pub client: Client,
    /// Runtime state mutable via slash commands (model/transcript/permission rules).
    pub runtime: Runtime,
    pub permission_mode: PermissionMode,
    pub settings: Settings,
    pub system: Vec<SystemBlock>,
    /// Sub-agent nesting depth (Agent tool recursion).
    pub depth: usize,
    /// User home (memdir memory location).
    pub home: PathBuf,
    /// User config dir (`$XDG_CONFIG_HOME` or `~/.config`), resolved once at
    /// startup: scoped settings writes and /config source display read it
    /// (re-reading the env in library code would break test hermeticity).
    pub user_config_dir: PathBuf,
    /// Interactive TUI session: suppress stderr progress prints (to avoid polluting the screen).
    pub quiet: bool,
    /// Consecutive auto-compact failure count (circuit breaker: skip after MAX_COMPACT_FAILURES).
    pub compact_failures: Arc<std::sync::atomic::AtomicU64>,
    /// Watchable registry (command/agent status observation and notifications).
    pub watch: Arc<crate::watch::WatchRegistry>,
    /// Task store (shared by the Task tool family + TUI task panel + reminder injection).
    pub tasks: Arc<crate::tasks::TaskStore>,
    /// Task panel expand signal (subscribed by the TUI loop).
    pub expand_tasks: watch::Sender<bool>,
    /// Sub-agent instance registry (continuation/lifecycle; sub-sessions share the same table).
    pub agents: Arc<crate::agents::AgentRegistry>,
    /// Agent channel registry (experimental; sub-sessions share the same table).
    pub channels: Arc<crate::channels::ChannelRegistry>,
    /// This session's instance name (sub-agents = Some(registry name); main session None,
    /// channel member name main).
    pub instance: Option<String>,
    /// Images the user mounted on the input box, addressed by the `#[image N]` markers left in
    /// the message text. Sub-sessions share the table, so the hub forwards an image to a
    /// subagent by repeating its marker.
    pub attachments: Arc<crate::api::image::Attachments>,
}

/// Single tool completion event.
#[derive(Debug, Clone)]
pub struct ToolCallDone {
    pub name: String,
    pub summary: String,
    pub output: String,
    pub is_error: bool,
    /// Unified diff preview for edit tools (None = no diff).
    pub diff: Option<String>,
    /// Tool execution duration in milliseconds.
    pub duration_ms: u64,
}

/// Async permission prompt callback: tool name + reason → whether allowed.
pub type AskFn = dyn Fn(&str, &str) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
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

/// UI hooks: stream events, tool completion, permission prompts, non-fatal warnings.
pub struct UiHooks {
    pub on_event: Box<dyn FnMut(&StreamEvent) + Send>,
    /// Callback when a tool block is complete (including input): the fold decision needs
    /// the input (Bash command classification). standalone=true: non-model tools like the
    /// `!` command — summary only, not part of a fold group.
    pub on_tool_ready: Box<dyn Fn(String, serde_json::Value, bool) + Send>,
    pub on_tool_done: Box<dyn Fn(&ToolCallDone) + Send>,
    /// One model response and all its tools finished: fold groups close per batch;
    /// the next turn's tools open a new group.
    pub on_round_end: Box<dyn Fn() + Send>,
    pub on_warning: Box<dyn Fn(String) + Send>,
    /// Permission prompt: tool name + reason → whether allowed (async: the TUI modal may wait for the user).
    pub ask: Arc<AskFn>,
    /// AskUserQuestion tool: title + question + options → selected index (async modal).
    pub ask_question: Arc<AskQuestionFn>,
}

/// Headless permission prompt (stderr question, stdin answer). Shared by `headless_hooks` and
/// the subagent prompt surface attached to the registry, so both ask the same way.
pub fn stdin_ask() -> Arc<AskFn> {
    Arc::new(|tool_name, reason| {
        let prompt = format!("允许 {tool_name} 执行吗？({reason}) [y/N] ");
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
            answer == "y" || answer == "yes"
        })
    })
}

/// Default headless hooks: text deltas to stdout; permissions via stdin interaction.
pub fn headless_hooks() -> UiHooks {
    UiHooks {
        on_event: Box::new(|event| {
            if let StreamEvent::TextDelta { text, .. } = event {
                let _ = std::io::stdout().write_all(text.as_bytes());
                let _ = std::io::stdout().flush();
            }
        }),
        on_tool_ready: Box::new(|_name, _input, _standalone| {}),
        on_tool_done: Box::new(|_| {}),
        on_round_end: Box::new(|| {}),
        on_warning: Box::new(|message| eprintln!("[bingo] warning: {message}")),
        ask: stdin_ask(),
        ask_question: Arc::new(|title, question, options| {
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
                    "  {}. Other（自定义输入）\n请选择 [1-{}] 或直接输入文本（回车 = 跳过）: ",
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
        }),
    }
}

/// Single-turn result: assistant message + the turn's tool_use blocks + stop_reason.
struct Turn {
    assistant: Message,
    tool_uses: Vec<ContentBlock>,
    stop_reason: Option<String>,
    /// Cancelled while reading the stream (assistant incomplete, whole turn discarded).
    aborted: bool,
}

/// One turn: request the model once and accumulate the assistant reply.
async fn one_turn(
    session: &Arc<Session>,
    messages: &[Message],
    tools: &[Box<dyn Tool>],
    ui: &mut UiHooks,
    mut cancel: Option<&mut watch::Receiver<bool>>,
) -> Result<Turn, QueryError> {
    let model = session.runtime.model.borrow().clone();
    let thinking = session.runtime.thinking.borrow().clone();
    // Thinking gate: models that reject the parameter (DeepSeek family) get
    // none regardless of the configured level — the UI shows the same fact
    // when the level is set, so display and wire agree.
    let thinking = if crate::api::models::supports_thinking(&model) {
        ThinkingLevel::parse(thinking.as_deref())
    } else {
        None
    };
    let request = NeutralRequest {
        model,
        max_tokens: DEFAULT_MAX_TOKENS,
        system: session.system.clone(),
        messages: messages.to_vec(),
        tools: tool_params(tools),
        stream: true,
        thinking,
    };
    // The connect phase is also interruptible (Esc gives up immediately on a hanging/
    // retrying connection, without waiting for output to start).
    let mut acc = AssistantAccumulator::new();
    let aborted_turn = |acc: &AssistantAccumulator| Turn {
        assistant: acc.message(),
        tool_uses: Vec::new(),
        stop_reason: None,
        aborted: true,
    };
    let mut stream = match &mut cancel {
        Some(cancel) => {
            // Clear the version and acknowledge an already-set signal before entering select:
            // otherwise a new receiver's changed() is ready immediately, select picks a branch
            // at random, and roughly half the turns would drop the already-issued HTTP stream
            // future and resend (double billing + latency).
            if *cancel.borrow_and_update() {
                return Ok(aborted_turn(&acc));
            }
            tokio::select! {
                stream = session.client.stream(&request) => stream?,
                _ = cancel_requested(cancel) => return Ok(aborted_turn(&acc)),
            }
        }
        None => session.client.stream(&request).await?,
    };
    let mut tool_uses = Vec::new();
    let mut aborted = false;
    loop {
        let event = match &mut cancel {
            Some(cancel) => tokio::select! {
                maybe = stream.next() => maybe,
                _ = cancel_requested(cancel) => {
                    aborted = true;
                    None
                }
            },
            None => stream.next().await,
        };
        let Some(event) = event else { break };
        let event = event?;
        (ui.on_event)(&event);
        if let Err(e) = acc.push(&event) {
            return Err(QueryError::Protocol(e));
        }
        match &event {
            StreamEvent::ApiError { message } => {
                return Err(QueryError::Protocol(message.clone()));
            }
            StreamEvent::BlockStop { index } => {
                if let Some(ContentBlock::ToolUse { id, name, input }) = acc.content.get(*index) {
                    tool_uses.push(ContentBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                    });
                    (ui.on_tool_ready)(name.clone(), input.clone(), false);
                }
            }
            _ => {}
        }
    }
    Ok(Turn {
        assistant: acc.message(),
        tool_uses,
        stop_reason: acc.stop_reason,
        aborted,
    })
}

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

/// Permission gate + PreToolUse hook + UI prompt: returns the final decision and
/// (possibly rewritten) input.
async fn gate_tool(
    tool: &dyn Tool,
    input: &serde_json::Value,
    mode: PermissionMode,
    hooks: &HooksConfig,
    permissions: &crate::settings::PermissionRules,
    ask: &AskFn,
) -> (PermissionBehavior, String, serde_json::Value) {
    let (hook_behavior, hook_reason, hook_input) =
        run_pre_tool_use(hooks, &tool.name(), input, permission_mode_str(mode)).await;
    if hook_behavior != PermissionBehavior::Allow {
        return (hook_behavior, hook_reason, hook_input);
    }

    let decision = can_use_tool(
        tool,
        &hook_input,
        mode,
        &permissions.deny,
        &permissions.ask,
        &permissions.allow,
    );
    match decision.behavior {
        PermissionBehavior::Ask => {
            let reason = decision.reason;
            if ask(&tool.name(), &reason).await {
                (PermissionBehavior::Allow, String::new(), hook_input)
            } else {
                (
                    PermissionBehavior::Deny,
                    format!("user denied {}", tool.name()),
                    hook_input,
                )
            }
        }
        other => (other, decision.reason, hook_input),
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

impl Session {
    pub fn permission_mode_str(&self) -> &'static str {
        permission_mode_str(self.permission_mode)
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
    if text.chars().count() > MAX_RESULT_CHARS {
        let cut: String = text.chars().take(MAX_RESULT_CHARS).collect();
        format!("{cut}\n…[truncated at {MAX_RESULT_CHARS} chars]")
    } else {
        text
    }
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
fn tool_context(session: &Session, ui: &UiHooks) -> Result<ToolContext, QueryError> {
    Ok(ToolContext {
        cwd: std::env::current_dir()
            .map_err(|e| QueryError::Tool(ToolError::failed(e.to_string())))?,
        home: session.home.clone(),
        watch: session.watch.clone(),
        http: tool_http()?,
        tasks: session.tasks.clone(),
        hooks: session.settings.hooks.clone(),
        permission_mode: permission_mode_str(session.permission_mode).to_string(),
        expand_tasks: session.expand_tasks.clone(),
        ask_question: ui.ask_question.clone(),
        instance: session.instance.clone(),
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
fn record(session: &Session, messages: &mut Vec<Message>, message: Message, ui: &mut UiHooks) {
    if let Some(t) = session.runtime.transcript.borrow().clone()
        && let Err(e) = t.append(&message)
    {
        (ui.on_warning)(format!("transcript append failed: {e}"));
    }
    messages.push(message);
}

/// queryLoop: multi-turn tool loop until end_turn (the loop body shared by `run_query`
/// and `run_bash_command`). messages already contain this user input and the transcript
/// write. cancel: when Some, stream reads can be interrupted by a watch signal
/// (TUI Ctrl+C/Esc).
async fn query_loop(
    session: &Arc<Session>,
    mut messages: Vec<Message>,
    ui: &mut UiHooks,
    tools: &[Box<dyn Tool>],
    ctx: &ToolContext,
    mut cancel_rx: Option<watch::Receiver<bool>>,
) -> Result<QueryOutcome, QueryError> {
    let mut recovery_count = 0u32;
    let mut stop_hook_fired = false;
    let mut gate = TokenGate::new();
    normalize_synthetic_bash_calls(&mut messages);
    loop {
        check_and_compact(session, &mut messages, &mut gate).await;
        // task_reminder: no Task tool for 10 turns + 10 turns since the last reminder.
        maybe_inject_task_reminder(session, &mut messages).await;
        // Messages queued by the previous step are delivered here, one batch per recipient:
        // the SendMessage tool only enqueues, so several messages sent in the same step reach
        // the receiver together instead of one per turn.
        crate::tool::agent::flush_agent_inbox(session, &ctx.watch);
        // Background task notification injection (dynamic awareness while running): before
        // each reasoning step, pending state-transition notifications (rounds/completion/
        // failure) are injected into the context; anything unconsumed by the end of the
        // turn carries over to the next turn.
        let notes = session
            .watch
            .consume_notifications(session.instance.as_deref());
        if !notes.is_empty() {
            messages.push(Message::user_text(format!(
                "<task-notifications>\n{}\n</task-notifications>",
                notes.join("\n")
            )));
        }
        // Channel message injection (channels the hub is a member of): batched at turn
        // boundaries, in order.
        let mail = session.channels.drain_hub_mail();
        if !mail.is_empty() {
            messages.push(Message::user_text(format!(
                "<channel-messages>\n{}\n</channel-messages>",
                mail.join("\n")
            )));
        }
        let turn = one_turn(session, &messages, tools, &mut *ui, cancel_rx.as_mut()).await?;
        if turn.aborted {
            // Interrupted: the whole turn is discarded (assistant incomplete); neither
            // executed nor pending tools are filled back.
            println!();
            return Ok(QueryOutcome {
                messages,
                aborted: true,
            });
        }
        // The assistant message must enter history before branching: max_tokens recovery
        // and the Stop hook both need the model to see the truncated content, and a normal
        // end must hand the turn's conclusion to downstream.
        record(session, &mut messages, turn.assistant, ui);
        if turn.tool_uses.is_empty() {
            // Output budget truncation recovery: inject a "continue" message and retry (max 3 times).
            if turn.stop_reason.as_deref() == Some("max_tokens")
                && recovery_count < MAX_OUTPUT_TOKENS_RECOVERY_LIMIT
            {
                recovery_count += 1;
                messages.push(Message::user_text(MAX_TOKENS_RESUME_PROMPT));
                continue;
            }
            // Stop hooks: exit 2 → inject the blocking stderr into the model and retry once (loop guard).
            if !stop_hook_fired
                && let Some(blocking) = run_stop_hooks(
                    &session.settings.hooks,
                    permission_mode_str(session.permission_mode),
                )
                .await
            {
                stop_hook_fired = true;
                messages.push(Message::user_text(format!(
                    "（Stop hook 阻止继续）\n{blocking}"
                )));
                continue;
            }
            println!();
            return Ok(QueryOutcome {
                messages,
                aborted: false,
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
            let (behavior, reason, gated_input) = if name == "AskUserQuestion" {
                (PermissionBehavior::Allow, String::new(), input.clone())
            } else {
                // Clone into a local before awaiting: MutexGuard is not Send, and holding it
                // across an await would break tokio::spawn's Send bound (sub-agent/turn tasks).
                let permissions = session
                    .runtime
                    .permissions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                gate_tool(
                    tool,
                    &input,
                    session.permission_mode,
                    &session.settings.hooks,
                    &permissions,
                    &*ui.ask,
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
                    blocks.push(tool_result_error(
                        &id,
                        format!(
                            "<permission_error>permission denied: {name} ({reason})</permission_error>"
                        ),
                    ));
                    // Denied tools also need UI closure: the tool row shows "denied"
                    // instead of spinning forever.
                    let summary = summarize_input(&name, &input);
                    (ui.on_tool_done)(&ToolCallDone {
                        name,
                        summary,
                        output: format!("permission denied: {reason}"),
                        is_error: true,
                        diff: None,
                        duration_ms: 0,
                    });
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
                    (ui.on_tool_done)(&ToolCallDone {
                        name: name.clone(),
                        summary: summarize_input(name, input),
                        output: clipped_result(render_result(&result)),
                        is_error: result.is_error,
                        diff: result.diff.clone(),
                        duration_ms: outcome.duration_ms,
                    });
                    blocks.push(result_block(&outcome.tool_use_id, &result));
                    if !interrupted {
                        // PostToolUse exit 2 → stop continuing (the hook's blocking error semantics).
                        stop_after_tools |= run_post_tool_use(
                            &session.settings.hooks,
                            name,
                            input,
                            &result.content,
                            permission_mode_str(session.permission_mode),
                        )
                        .await;
                    }
                }
                Err(e) => {
                    // Failures also need UI closure: otherwise the tool row spins forever
                    // and the user never sees the failure.
                    (ui.on_tool_done)(&ToolCallDone {
                        name: name.clone(),
                        summary: summarize_input(name, input),
                        output: e.to_string(),
                        is_error: true,
                        diff: None,
                        duration_ms: outcome.duration_ms,
                    });
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
                (ui.on_tool_done)(&ToolCallDone {
                    name: name.clone(),
                    summary: summarize_input(name, input),
                    output: "interrupted".to_string(),
                    is_error: false,
                    diff: None,
                    duration_ms: 0,
                });
            }
        }
        // Fill every unanswered tool_use: returning early on the interrupt path would leave
        // orphan tool_use blocks in the transcript, and every future restore from history
        // would 400, permanently corrupting the session.
        fill_missing_tool_results(&turn.tool_uses, &mut blocks);
        record(
            session,
            &mut messages,
            Message {
                role: Role::User,
                content: blocks,
            },
            ui,
        );
        if interrupted {
            println!();
            return Ok(QueryOutcome {
                messages,
                aborted: true,
            });
        }
        // All tools in this batch are closed: RoundEnd only marks a batch boundary (image
        // warm-up etc.); fold groups are bounded by text — tools across turns stay in the
        // same fold group.
        (ui.on_round_end)();
        if stop_after_tools || is_cancelled(&cancel_rx) {
            return Ok(QueryOutcome {
                messages,
                aborted: is_cancelled(&cancel_rx),
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
    ui: &mut UiHooks,
    cancel: Option<watch::Receiver<bool>>,
) -> Result<QueryOutcome, QueryError> {
    let tools = crate::tools::assemble_tools(session, &mut ui.on_warning).await;
    let ctx = tool_context(session, &*ui)?;

    // UserPromptSubmit: the hook may block this submission.
    if run_user_prompt_submit(
        &session.settings.hooks,
        user_input,
        permission_mode_str(session.permission_mode),
    )
    .await
    {
        return Ok(QueryOutcome {
            messages: initial_messages,
            aborted: false,
        });
    }

    let mut messages = initial_messages;
    record(
        session,
        &mut messages,
        user_message_with_images(user_input, images, session.client.supports_images()),
        ui,
    );
    query_loop(session, messages, ui, &tools, &ctx, cancel).await
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
) -> Message {
    use crate::api::types::{ContentBlock, ImageSource, Role};
    let attaching = send_images && !images.is_empty();
    let mut body = text.to_string();
    if !attaching && crate::api::image::has_marker(text) {
        body.push_str(if send_images {
            "\n\n<system-reminder>The `#[image N]` placeholders above have no image attached: \
             the referenced attachment is not in this session (attachments live in memory, so \
             markers from a resumed or restored session no longer resolve). Do not go looking \
             for the file — tell the user the image needs to be attached again.</system-reminder>"
        } else {
            "\n\n<system-reminder>The `#[image N]` placeholders above have no image attached: \
             this endpoint is not configured to receive images. Do not go looking for the file — \
             tell the user to enable `sendImages` (or set `supportsImages` on the provider) and \
             resend.</system-reminder>"
        });
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
    ui: &mut UiHooks,
    mut cancel: Option<watch::Receiver<bool>>,
) -> Result<QueryOutcome, QueryError> {
    let tools = crate::tools::assemble_tools(session, &mut ui.on_warning).await;
    let ctx = tool_context(session, &*ui)?;
    let mut messages = history;

    let tool_use_id = format!(
        "bash-{}",
        BASH_CALL_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let input = serde_json::json!({ "command": command });
    // UI tool activity (reuses the Tool fold/expand rows): emitted before the permission
    // gate (the tool row is visible during the permission modal, consistent with run_query's
    // "stream fully, then gate" order).
    (ui.on_event)(&StreamEvent::ToolUseStart {
        index: 0,
        id: tool_use_id.clone(),
        name: "Bash".to_string(),
    });
    (ui.on_tool_ready)("Bash".to_string(), input.clone(), true);

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
            (ui.on_warning)(err.clone());
            (err, true, 0)
        }
        None => {
            let permissions = session
                .runtime
                .permissions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let (behavior, reason, gated_input) = gate_tool(
                tool,
                &input,
                session.permission_mode,
                &session.settings.hooks,
                &permissions,
                &*ui.ask,
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
                        return Ok(QueryOutcome {
                            messages,
                            aborted: true,
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
                    let err = format!("permission denied: Bash ({reason})");
                    (err, true, 0)
                }
                PermissionBehavior::Ask => unreachable!("ask resolved by gate_tool"),
            }
        }
    };
    (ui.on_tool_done)(&ToolCallDone {
        name: "Bash".to_string(),
        summary: format!("$ {command}"),
        output: text.clone(),
        is_error,
        diff: None,
        duration_ms,
    });
    (ui.on_round_end)();

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
                (ui.on_warning)(format!("transcript append failed: {e}"));
            }
        }
    }
    if !respond {
        return Ok(QueryOutcome {
            messages,
            aborted: is_cancelled(&cancel),
        });
    }
    query_loop(session, messages, ui, &tools, &ctx, cancel).await
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
        let is_bash_input = matches!(
            &messages[i].content[0],
            ContentBlock::Text { text } if text.contains("<bash-input>")
        ) && messages[i].role == Role::User;
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
                        &m.content[0],
                        ContentBlock::ToolResult { tool_use_id, .. }
                            if tool_use_id.starts_with("bash-")
                    )
            });
        if synthetic {
            let input_text = match &messages[i].content[0] {
                ContentBlock::Text { text } => text.clone(),
                _ => String::new(),
            };
            let result_text = match &messages[i + 2].content[0] {
                ContentBlock::ToolResult { content, .. } => {
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
mod tests {
    use super::*;

    /// A tool result carrying images must reach the API as protocol blocks. Re-stringifying it
    /// here is what would turn a screenshot into a wall of base64 text the model can't see.
    #[test]
    fn image_tool_results_stay_blocks_while_text_is_still_clipped() {
        let long = "x".repeat(200_000);
        let result = ToolResult {
            content: crate::api::types::tool_result_blocks(
                &long,
                &[crate::api::types::ImageAttachment {
                    media_type: "image/png".into(),
                    data: "aGVsbG8=".into(),
                }],
            ),
            is_error: false,
            diff: None,
        };
        let ContentBlock::ToolResult { content, .. } = result_block("t1", &result) else {
            unreachable!("tool result block")
        };
        let blocks = content.as_array().unwrap_or_else(|| unreachable!());
        assert_eq!(blocks[1]["type"], "image", "图片块原样保留");
        assert_eq!(blocks[1]["source"]["data"], "aGVsbG8=");
        let text = blocks[0]["text"].as_str().unwrap_or_default();
        assert!(text.len() < long.len(), "文本仍受截断上限约束");

        // A plain string result keeps the old shape.
        let plain = ToolResult {
            content: serde_json::Value::String("ok".into()),
            is_error: false,
            diff: None,
        };
        let ContentBlock::ToolResult { content, .. } = result_block("t2", &plain) else {
            unreachable!("tool result block")
        };
        assert_eq!(content, serde_json::Value::String("ok".into()));
    }

    /// A placeholder that arrives without its image must say why. Left bare, the model reads it
    /// as an image it failed to locate and starts hunting through the transcript and temp dirs
    /// instead of telling the user what is actually wrong.
    #[test]
    fn dangling_image_marker_explains_itself() {
        use crate::api::types::{ContentBlock, ImageAttachment};
        let imgs = vec![ImageAttachment {
            media_type: "image/png".into(),
            data: "aGVsbG8=".into(),
        }];
        let text_of = |msg: &Message| match &msg.content[0] {
            ContentBlock::Text { text } => text.clone(),
            _ => unreachable!("text block first"),
        };

        // Endpoint cannot take images: point at the setting, not at the filesystem.
        let msg = user_message_with_images("看图 #[image 1]", &imgs, false);
        assert_eq!(msg.content.len(), 1, "端点不支持时不发图片块");
        let text = text_of(&msg);
        assert!(text.contains("sendImages"), "{text}");
        assert!(text.contains("Do not go looking"), "{text}");

        // Marker that no longer resolves (resumed session): say the attachment is gone.
        let msg = user_message_with_images("看图 #[image 9]", &[], true);
        let text = text_of(&msg);
        assert!(text.contains("not in this session"), "{text}");

        // No marker, no note — the reminder is only for a placeholder without its image.
        let msg = user_message_with_images("随便问问", &[], true);
        assert_eq!(text_of(&msg), "随便问问");
        // Images actually attached: text stays verbatim.
        let msg = user_message_with_images("看图 #[image 1]", &imgs, true);
        assert_eq!(text_of(&msg), "看图 #[image 1]");
    }

    /// Image attachments: text block + image blocks when the provider supports them;
    /// text only otherwise.
    #[test]
    fn user_message_with_images_respects_support_flag() {
        use crate::api::types::{ContentBlock, ImageAttachment};
        let imgs = vec![ImageAttachment {
            media_type: "image/png".into(),
            data: "aGVsbG8=".into(),
        }];
        let msg = user_message_with_images("看图 #[image 1]", &imgs, true);
        assert_eq!(msg.content.len(), 2);
        assert!(
            matches!(msg.content[0], ContentBlock::Text { ref text } if text == "看图 #[image 1]")
        );
        assert!(
            matches!(&msg.content[1], ContentBlock::Image { source } if source.data == "aGVsbG8=")
        );

        let msg = user_message_with_images("看图 #[image 1]", &imgs, false);
        assert_eq!(msg.content.len(), 1, "不支持时图片块不发送");
        assert!(matches!(msg.content[0], ContentBlock::Text { .. }));
    }

    /// Minimal Anthropic endpoint: count_tokens returns a fixed value; /v1/messages
    /// replies with preset SSE in order.
    async fn spawn_api(responses: Vec<String>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut remaining = responses;
        remaining.reverse();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = vec![0u8; 64 * 1024];
                let read = socket.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..read]).to_string();
                let (content_type, body) = if head.contains("/v1/messages/count_tokens") {
                    ("application/json", "{\"input_tokens\":10}".to_string())
                } else {
                    ("text/event-stream", remaining.pop().unwrap_or_default())
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        format!("http://{addr}")
    }

    fn sse(events: &[(&str, String)]) -> String {
        events
            .iter()
            .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
            .collect()
    }

    fn text_turn(text: &str, stop_reason: &str) -> String {
        sse(&[
            (
                "message_start",
                r#"{"message":{"id":"m_1","model":"m"}}"#.into(),
            ),
            (
                "content_block_start",
                r#"{"index":0,"content_block":{"type":"text","text":""}}"#.into(),
            ),
            (
                "content_block_delta",
                format!(r#"{{"index":0,"delta":{{"type":"text_delta","text":"{text}"}}}}"#),
            ),
            ("content_block_stop", r#"{"index":0}"#.into()),
            (
                "message_delta",
                format!(
                    r#"{{"delta":{{"stop_reason":"{stop_reason}"}},"usage":{{"output_tokens":5}}}}"#
                ),
            ),
            ("message_stop", "{}".into()),
        ])
    }

    fn bash_tool_turn(id: &str, command: &str) -> String {
        let input = serde_json::to_string(&serde_json::json!({ "command": command }).to_string())
            .unwrap_or_default();
        sse(&[
            (
                "message_start",
                r#"{"message":{"id":"m_1","model":"m"}}"#.into(),
            ),
            (
                "content_block_start",
                format!(
                    r#"{{"index":0,"content_block":{{"type":"tool_use","id":"{id}","name":"Bash","input":{{}}}}}}"#
                ),
            ),
            (
                "content_block_delta",
                format!(
                    r#"{{"index":0,"delta":{{"type":"input_json_delta","partial_json":{input}}}}}"#
                ),
            ),
            ("content_block_stop", r#"{"index":0}"#.into()),
            (
                "message_delta",
                r#"{"delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":5}}"#.into(),
            ),
            ("message_stop", "{}".into()),
        ])
    }

    fn test_session(base_url: String, transcript: Option<Transcript>) -> Arc<Session> {
        Arc::new(Session {
            client: crate::api::client::Client::new("k".into(), base_url),
            runtime: Runtime::new("m".into(), transcript, Default::default()),
            permission_mode: PermissionMode::BypassPermissions,
            settings: crate::settings::Settings::default(),
            system: Vec::new(),
            depth: 0,
            home: std::env::temp_dir(),
            user_config_dir: std::env::temp_dir().join(".config"),
            quiet: true,
            compact_failures: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            watch: crate::watch::WatchRegistry::new(),
            tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "test")),
            expand_tasks: tokio::sync::watch::channel(false).0,
            agents: crate::agents::AgentRegistry::new(),
            channels: crate::channels::ChannelRegistry::new(Default::default()),
            instance: None,
            attachments: crate::api::image::Attachments::new(),
        })
    }

    fn tool_use_ids(messages: &[Message]) -> Vec<String> {
        messages
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|b| match b {
                ContentBlock::ToolUse { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect()
    }

    fn tool_result_ids(messages: &[Message]) -> Vec<String> {
        messages
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|b| match b {
                ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn clips_oversized_results() {
        let long = "x".repeat(MAX_RESULT_CHARS + 100);
        let clipped = clipped_result(long);
        assert!(clipped.contains("[truncated at"));
        assert!(clipped.chars().count() <= MAX_RESULT_CHARS + 64);
    }

    #[test]
    fn keeps_small_results() {
        assert_eq!(clipped_result("hi".to_string()), "hi");
    }

    #[test]
    fn agent_summary_uses_description_to_distinguish_parallel_agents() {
        let a = serde_json::json!({"background": true, "description": "深挖 TUI", "prompt": "..."});
        let b = serde_json::json!({"background": true, "description": "核查机制", "prompt": "..."});
        let sa = summarize_input("Agent", &a);
        let sb = summarize_input("Agent", &b);
        assert_eq!(sa, "description=\"深挖 TUI\"");
        assert_eq!(sb, "description=\"核查机制\"");
        assert_ne!(sa, sb, "parallel agents distinguishable");
        // Without a description, fall back to the prompt summary
        let c = serde_json::json!({"background": true, "prompt": "长任务的提示词内容..."});
        let sc = summarize_input("Agent", &c);
        assert!(sc.starts_with("prompt=\""), "{sc}");
        assert!(sc.len() < 60, "prompt truncated: {sc}");
    }

    #[test]
    fn skill_summary_shows_name_and_args() {
        let both = serde_json::json!({"skill": "review", "args": "doc.md"});
        assert_eq!(summarize_input("Skill", &both), "review doc.md");
        let bare = serde_json::json!({"skill": "review"});
        assert_eq!(summarize_input("Skill", &bare), "review");
        // Missing skill name (malformed call): empty summary → the header row shows only the tool name.
        let missing = serde_json::json!({"args": "doc.md"});
        assert_eq!(summarize_input("Skill", &missing), "");
    }

    #[test]
    fn clamps_400_recomputation() {
        // max(3000, C − A − 1000): only 500 left in the window → floor of 3000
        let rem = 200_000u64.checked_sub(198_500).unwrap();
        let recomputed = rem.saturating_sub(1000).max(3000);
        assert_eq!(recomputed, 3000);
    }

    #[test]
    fn escapes_xml_for_bash_output() {
        assert_eq!(escape_xml("a<b&c>"), "a&lt;b&amp;c&gt;");
        assert_eq!(escape_xml("plain"), "plain");
        assert_eq!(
            escape_xml("<bash-stdout>x</bash-stdout>"),
            "&lt;bash-stdout&gt;x&lt;/bash-stdout&gt;"
        );
    }

    /// respondToBashCommands=false: `!` commands run purely (no model query);
    /// history is [caveat, bash-input+output], output wrapped in `<bash-stdout>`
    /// with & < > escaped. A synthetic assistant ToolUse is avoided on purpose
    /// (thinking-mode endpoints reject assistant messages without a thinking block).
    #[tokio::test]
    async fn bash_command_executes_without_model_query() {
        let session = Arc::new(Session {
            client: crate::api::client::Client::new("k".into(), "http://127.0.0.1:9".into()),
            runtime: Runtime::new("m".into(), None, Default::default()),
            permission_mode: PermissionMode::BypassPermissions,
            settings: crate::settings::Settings {
                respond_to_bash_commands: Some(false),
                ..Default::default()
            },
            system: Vec::new(),
            depth: 0,
            home: std::env::temp_dir(),
            user_config_dir: std::env::temp_dir().join(".config"),
            quiet: true,
            compact_failures: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            watch: crate::watch::WatchRegistry::new(),
            tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "test")),
            expand_tasks: tokio::sync::watch::channel(false).0,
            agents: crate::agents::AgentRegistry::new(),
            channels: crate::channels::ChannelRegistry::new(Default::default()),
            instance: None,
            attachments: crate::api::image::Attachments::new(),
        });
        let mut ui = headless_hooks();
        let outcome = run_bash_command(&session, "printf '%s' 'a<b&c>'", Vec::new(), &mut ui, None)
            .await
            .unwrap();
        assert!(!outcome.aborted);
        assert_eq!(outcome.messages.len(), 2, "caveat + input/output");

        let text_of = |m: &Message| match &m.content[0] {
            ContentBlock::Text { text } => text.clone(),
            _ => String::new(),
        };
        assert!(
            text_of(&outcome.messages[0]).contains("local-command-caveat"),
            "caveat 前置: {}",
            text_of(&outcome.messages[0])
        );
        let merged = text_of(&outcome.messages[1]);
        assert!(
            merged.contains("<bash-input>printf '%s' 'a<b&c>'</bash-input>"),
            "{merged}"
        );
        assert!(merged.contains("<bash-stdout>"), "{merged}");
        assert!(merged.contains("a&lt;b&amp;c&gt;"), "输出已转义: {merged}");
        let stdout = merged.split("<bash-stdout>").nth(1).unwrap_or("");
        assert!(
            !stdout.contains("a<b&c>"),
            "stdout 段原始 < > 不得泄漏: {merged}"
        );
        assert!(
            !outcome.messages.iter().any(|m| m.role == Role::Assistant),
            "不构造合成 assistant 消息（thinking 校验）"
        );
    }

    /// S2: interruption happens during tool execution — every tool_use in the history
    /// and transcript must have a paired tool_result, otherwise every session restore
    /// afterwards 400s.
    #[tokio::test]
    async fn interrupt_backfills_placeholder_tool_results() {
        let base_url = spawn_api(vec![bash_tool_turn("tu_1", "sleep 5")]).await;
        let home = std::env::temp_dir().join(format!("bingo-interrupt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let transcript = crate::transcript::create(&home, &home).unwrap();
        let session = test_session(base_url, Some(transcript.clone()));

        let (tx, rx) = tokio::sync::watch::channel(false);
        let handle = tokio::spawn({
            let session = session.clone();
            async move {
                let mut ui = headless_hooks();
                run_query(&session, Vec::new(), "go", &[], &mut ui, Some(rx)).await
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        tx.send(true).unwrap();
        let outcome = handle.await.unwrap().unwrap();

        assert!(outcome.aborted, "回合按中断收口");
        let uses = tool_use_ids(&outcome.messages);
        assert_eq!(uses, vec!["tu_1"], "本轮发出了一个 tool_use");
        assert_eq!(
            tool_result_ids(&outcome.messages),
            uses,
            "每个 tool_use 都配对了 tool_result"
        );

        // The transcript must not leave orphan tool_use blocks either (session restore would carry them).
        let saved = transcript.load_messages().unwrap();
        assert_eq!(tool_use_ids(&saved), uses, "transcript 记录了 tool_use");
        assert_eq!(
            tool_result_ids(&saved),
            uses,
            "transcript 里 tool_use 也已配对，恢复不会 400"
        );
        let ContentBlock::ToolResult { is_error, .. } = &saved
            .last()
            .unwrap()
            .content
            .iter()
            .find(|b| matches!(b, ContentBlock::ToolResult { .. }))
            .unwrap()
        else {
            panic!("tool result");
        };
        assert!(is_error, "占位结果标为 is_error");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// M2: on max_tokens truncation recovery, the truncated assistant content must already
    /// be in the request history — otherwise the model has nothing to continue from.
    #[tokio::test]
    async fn max_tokens_recovery_keeps_truncated_assistant_in_history() {
        let base_url = spawn_api(vec![
            text_turn("partial answer", "max_tokens"),
            text_turn("done", "end_turn"),
        ])
        .await;
        let session = test_session(base_url, None);
        let mut ui = headless_hooks();
        let outcome = run_query(&session, Vec::new(), "go", &[], &mut ui, None)
            .await
            .unwrap();

        let texts: Vec<(Role, String)> = outcome
            .messages
            .iter()
            .map(|m| {
                let text = m
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                (m.role, text)
            })
            // task_reminder is irrelevant to this test (a first session always injects one).
            .filter(|(_, text)| !text.starts_with(TASK_REMINDER_MARKER))
            .collect();

        assert_eq!(
            texts.len(),
            4,
            "user / assistant / resume / assistant: {texts:?}"
        );
        assert_eq!(texts[1], (Role::Assistant, "partial answer".to_string()));
        assert_eq!(texts[2], (Role::User, MAX_TOKENS_RESUME_PROMPT.to_string()));
        assert_eq!(
            texts[3],
            (Role::Assistant, "done".to_string()),
            "正常结束的 assistant 也在返回的 messages 里"
        );
    }

    /// M1: a first session (no reminder, no Task tool turns) must not be reminded just
    /// because the scan reached the end; sessions that just used a Task tool must have
    /// a small distance.
    #[test]
    fn task_reminder_distances_stop_at_first_hit() {
        let assistant = |uses_task: bool| Message {
            role: Role::Assistant,
            content: if uses_task {
                vec![ContentBlock::ToolUse {
                    id: "t".into(),
                    name: "TaskUpdate".into(),
                    input: serde_json::json!({}),
                }]
            } else {
                vec![ContentBlock::Text { text: "hi".into() }]
            },
        };

        // The most recent turn used a Task tool: distance 1, no reminder.
        let mut messages: Vec<Message> = (0..20).map(|_| assistant(false)).collect();
        messages.push(assistant(true));
        let (since_management, since_reminder) = task_reminder_turn_distances(&messages);
        assert_eq!(since_management, 1, "距最近一次 Task 工具 1 轮");
        assert_eq!(
            since_reminder,
            TASK_REMINDER_TURNS + 1,
            "从未提醒过 → 视为超阈值"
        );
        assert!(
            since_management < TASK_REMINDER_TURNS,
            "刚用过 Task 工具不该再提醒"
        );

        // Used ten turns ago: distance 11, should remind.
        let mut messages = vec![assistant(true)];
        messages.extend((0..11).map(|_| assistant(false)));
        let (since_management, _) = task_reminder_turn_distances(&messages);
        assert_eq!(since_management, 12);

        // Never used, never reminded: both sides are treated as over the threshold.
        let messages: Vec<Message> = (0..30).map(|_| assistant(false)).collect();
        assert_eq!(
            task_reminder_turn_distances(&messages),
            (TASK_REMINDER_TURNS + 1, TASK_REMINDER_TURNS + 1)
        );
    }

    /// Orphan tool_use blocks are always filled; already-answered ones are not filled
    /// again (duplicate tool_result also 400s).
    #[test]
    fn missing_tool_results_are_filled_exactly_once() {
        let tool_uses = vec![
            ContentBlock::ToolUse {
                id: "a".into(),
                name: "Bash".into(),
                input: serde_json::json!({}),
            },
            ContentBlock::ToolUse {
                id: "b".into(),
                name: "Read".into(),
                input: serde_json::json!({}),
            },
        ];
        let mut blocks = vec![tool_result_text("a", "done")];
        fill_missing_tool_results(&tool_uses, &mut blocks);
        assert_eq!(
            tool_result_ids(&[Message {
                role: Role::User,
                content: blocks.clone()
            }]),
            vec!["a", "b"]
        );

        // Running again must not fill duplicates.
        fill_missing_tool_results(&tool_uses, &mut blocks);
        assert_eq!(blocks.len(), 2, "已配对的不重复补");
    }

    /// Interactive/TTY commands like `!top`: rejected directly without the permission
    /// gate (respond=false, no model query).
    #[tokio::test]
    async fn bash_command_refuses_interactive_tty_commands() {
        let session = Arc::new(Session {
            client: crate::api::client::Client::new("k".into(), "http://127.0.0.1:9".into()),
            runtime: Runtime::new("m".into(), None, Default::default()),
            permission_mode: PermissionMode::Default,
            settings: crate::settings::Settings {
                respond_to_bash_commands: Some(false),
                ..Default::default()
            },
            system: Vec::new(),
            depth: 0,
            home: std::env::temp_dir(),
            user_config_dir: std::env::temp_dir().join(".config"),
            quiet: true,
            compact_failures: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            watch: crate::watch::WatchRegistry::new(),
            tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "test")),
            expand_tasks: tokio::sync::watch::channel(false).0,
            agents: crate::agents::AgentRegistry::new(),
            channels: crate::channels::ChannelRegistry::new(Default::default()),
            instance: None,
            attachments: crate::api::image::Attachments::new(),
        });
        let mut ui = headless_hooks();
        let outcome = run_bash_command(&session, "htop", Vec::new(), &mut ui, None)
            .await
            .unwrap();
        let ContentBlock::Text { text } = &outcome.messages[1].content[0] else {
            panic!("拒绝原因以文本消息呈现");
        };
        assert!(text.contains("interactive command not allowed"), "{text}");
        assert!(text.contains("TTY"), "{text}");
    }

    /// Old transcript shape: `user(<bash-input>) → assistant(ToolUse "bash-N") →
    /// user(ToolResult)` folds back into a single user message; model-generated
    /// tool calls are untouched.
    #[test]
    fn normalizes_synthetic_bash_calls() {
        let old = vec![
            Message::user_text("<bash-input>ls</bash-input>"),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "bash-1".into(),
                    name: "Bash".into(),
                    input: serde_json::json!({ "command": "ls" }),
                }],
            },
            Message {
                role: Role::User,
                content: vec![tool_result_text(
                    "bash-1",
                    "<bash-stdout>a&lt;b</bash-stdout>",
                )],
            },
            Message::user_text("普通问题"),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "toolu_real".into(),
                    name: "Bash".into(),
                    input: serde_json::json!({ "command": "make" }),
                }],
            },
            Message {
                role: Role::User,
                content: vec![tool_result_text("toolu_real", "ok")],
            },
        ];
        let mut messages = old.clone();
        normalize_synthetic_bash_calls(&mut messages);
        assert_eq!(messages.len(), 4, "合成三段折叠为一条");
        assert_eq!(
            match &messages[0].content[0] {
                ContentBlock::Text { text } => text.as_str(),
                _ => "",
            },
            "<bash-input>ls</bash-input>\n<bash-stdout>a&lt;b</bash-stdout>"
        );
        assert_eq!(messages[1].role, Role::User);
        // 模型生成的 tool_use 配对保持原样。
        assert!(matches!(
            &messages[2].content[0],
            ContentBlock::ToolUse { id, .. } if id == "toolu_real"
        ));
        assert!(matches!(
            &messages[3].content[0],
            ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "toolu_real"
        ));
    }
}
