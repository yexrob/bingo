use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;

use futures_util::StreamExt;
use thiserror::Error;
use tokio::sync::watch;

use crate::api::client::{AssistantAccumulator, Client, ClientError};
use crate::api::types::{
    ContentBlock, Message, Request, StreamEvent, SystemBlock, Role, DEFAULT_MAX_TOKENS,
};
use crate::budget::MAX_RESULT_CHARS;
use crate::compact::check_and_compact;
use crate::hooks::{run_post_tool_use, run_pre_tool_use, run_stop_hooks, run_user_prompt_submit};
use crate::permission::{can_use_tool, PermissionBehavior, PermissionMode};
use crate::settings::{HooksConfig, Settings};
use crate::tool::executor::{execute_calls, PendingCall};
use crate::tool::{find_tool, tool_params, Tool, ToolContext, ToolError, ToolResult};
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

/// 一次查询的结果。
#[derive(Debug)]
pub struct QueryOutcome {
    pub messages: Vec<Message>,
    /// 回合被用户中断（流中止；已执行工具照常跑完）。
    pub aborted: bool,
}

/// max_tokens 截断后的恢复注入（对标 Claude Code MAX_OUTPUT_TOKENS_RECOVERY_LIMIT）。
const MAX_OUTPUT_TOKENS_RECOVERY_LIMIT: u32 = 3;
const MAX_TOKENS_RESUME_PROMPT: &str =
    "Output token limit hit. Resume directly from where you left off. Do not apologize or explain.";

/// 一次查询的全部上下文（TUI 与 headless 共用）。
#[derive(Clone)]
pub struct Session {
    pub client: Client,
    pub model: String,
    pub permission_mode: PermissionMode,
    pub settings: Settings,
    pub system: Vec<SystemBlock>,
    pub transcript: Option<Transcript>,
    /// 子代理嵌套深度（Agent 工具递归）。
    pub depth: usize,
    /// 用户 home（memdir 记忆定位）。
    pub home: PathBuf,
    /// 交互式 TUI 会话：抑制 stderr 进度打印（避免污染屏幕）。
    pub quiet: bool,
    /// 自动压缩连续失败计数（熔断：MAX_COMPACT_FAILURES 后跳过）。
    pub compact_failures: Arc<std::sync::atomic::AtomicU64>,
}

/// 单个工具完成事件。
#[derive(Debug, Clone)]
pub struct ToolCallDone {
    pub name: String,
    pub summary: String,
    pub output: String,
    pub is_error: bool,
    /// 编辑类工具的 unified diff 预览（None = 无 diff）。
    pub diff: Option<String>,
    /// 工具执行耗时（毫秒）。
    pub duration_ms: u64,
}

/// 异步权限询问回调：工具名 + 理由 → 是否允许。
pub type AskFn = dyn Fn(&str, &str) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
    + Send
    + Sync;

/// UI 挂钩：流事件、工具完成、权限询问、非致命警告。
pub struct UiHooks {
    pub on_event: Box<dyn FnMut(&StreamEvent) + Send>,
    /// 工具 block 完整（含 input）时回调：折叠判定需要输入（Bash 命令分类）。
    pub on_tool_ready: Box<dyn Fn(String, serde_json::Value) + Send>,
    pub on_tool_done: Box<dyn Fn(&ToolCallDone) + Send>,
    /// 一轮模型响应及其工具全部执行完：折叠组按批收口，下一轮工具开新组。
    pub on_round_end: Box<dyn Fn() + Send>,
    pub on_warning: Box<dyn Fn(String) + Send>,
    /// 权限询问：工具名 + 理由 → 是否允许（异步：TUI 模态可能等待用户）。
    pub ask: Box<AskFn>,
}

/// headless 默认挂钩：文本增量打 stdout；权限走 stdin 交互。
pub fn headless_hooks() -> UiHooks {
    UiHooks {
        on_event: Box::new(|event| {
            if let StreamEvent::TextDelta { text, .. } = event {
                let _ = std::io::stdout().write_all(text.as_bytes());
                let _ = std::io::stdout().flush();
            }
        }),
        on_tool_ready: Box::new(|_name, _input| {}),
        on_tool_done: Box::new(|_| {}),
        on_round_end: Box::new(|| {}),
        on_warning: Box::new(|message| eprintln!("[bingo] warning: {message}")),
        ask: Box::new(|tool_name, reason| {
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
        }),
    }
}

/// 单轮结果：assistant 消息 + 该轮产生的 tool_use 块 + stop_reason。
struct Turn {
    assistant: Message,
    tool_uses: Vec<ContentBlock>,
    stop_reason: Option<String>,
    /// 流读取中被取消（assistant 不完整，整轮丢弃）。
    aborted: bool,
}

/// 单轮：请求一次模型，累积 assistant 回复。
async fn one_turn(
    client: &Client,
    model: &str,
    messages: &[Message],
    tools: &[Box<dyn Tool>],
    system: &[SystemBlock],
    ui: &mut UiHooks,
    mut cancel: Option<&mut watch::Receiver<bool>>,
) -> Result<Turn, QueryError> {
    let request = Request {
        model: model.to_string(),
        max_tokens: DEFAULT_MAX_TOKENS,
        system: system.to_vec(),
        messages: messages.to_vec(),
        tools: tool_params(tools),
        stream: true,
    };
    let mut stream = Box::pin(client.stream(&request).await?);
    let mut acc = AssistantAccumulator::new();
    let mut tool_uses = Vec::new();
    let mut aborted = false;
    loop {
        let event = match &mut cancel {
            Some(cancel) => tokio::select! {
                maybe = stream.next() => maybe,
                _ = cancel.changed() => {
                    if *cancel.borrow() {
                        aborted = true;
                        None
                    } else {
                        continue;
                    }
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
                if let Some(ContentBlock::ToolUse { id, name, input }) = acc.content.get(*index)
                {
                    tool_uses.push(ContentBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                    });
                    (ui.on_tool_ready)(name.clone(), input.clone());
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

/// 权限门 + PreToolUse hook + UI 询问：返回最终决策与（可能被改写的）输入。
async fn gate_tool(
    tool: &dyn Tool,
    input: &serde_json::Value,
    mode: PermissionMode,
    hooks: &HooksConfig,
    permissions: &crate::settings::PermissionRules,
    ask: &AskFn,
) -> (PermissionBehavior, String, serde_json::Value) {
    let (hook_behavior, hook_reason, hook_input) = run_pre_tool_use(
        hooks,
        &tool.name(),
        input,
        permission_mode_str(mode),
    )
    .await;
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
    match &result.content {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn result_block(tool_use_id: &str, result: &ToolResult) -> ContentBlock {
    if result.is_error {
        tool_result_error(tool_use_id, clipped_result(render_result(result)))
    } else {
        tool_result_text(tool_use_id, clipped_result(render_result(result)))
    }
}

pub(crate) fn summarize_input(tool_name: &str, input: &serde_json::Value) -> String {
    match (tool_name, input) {
        // Bash 摘要直接显示命令（Claude Code 风格）
        ("Bash", serde_json::Value::Object(map)) => map
            .get("command")
            .and_then(|c| c.as_str())
            .map(|c| format!("$ {c}"))
            .unwrap_or_else(|| "Bash".to_string()),
        // 搜索摘要显示查询（Claude Code 风格：Web Search("query")）
        ("WebSearch", serde_json::Value::Object(map)) => map
            .get("query")
            .and_then(|q| q.as_str())
            .map(|q| format!("Web Search({q:?})"))
            .unwrap_or_else(|| "Web Search".to_string()),
        (_, serde_json::Value::Object(map)) => map
            .iter()
            .take(1)
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(" "),
        (_, other) => other.to_string(),
    }
}

/// 工具结果回填模型前裁剪：超长输出截断并标注（对标 DEFAULT_MAX_RESULT_SIZE_CHARS 50k，
/// 简化为截断而非落盘 + 预览）。
fn clipped_result(text: String) -> String {
    if text.chars().count() > MAX_RESULT_CHARS {
        let cut: String = text.chars().take(MAX_RESULT_CHARS).collect();
        format!("{cut}\n…[truncated at {MAX_RESULT_CHARS} chars]")
    } else {
        text
    }
}

/// queryLoop：多轮 tool loop，直到 end_turn。
/// cancel：Some 时流读取可被 watch 信号中断（TUI Ctrl+C/Esc）。
pub async fn run_query(
    session: &Arc<Session>,
    initial_messages: Vec<Message>,
    user_input: &str,
    ui: &mut UiHooks,
    cancel: Option<watch::Receiver<bool>>,
) -> Result<QueryOutcome, QueryError> {
    let tools = crate::tools::assemble_tools(session, &mut ui.on_warning).await;
    let ctx = ToolContext {
        cwd: std::env::current_dir()
            .map_err(|e| QueryError::Tool(ToolError::failed(e.to_string())))?,
    };

    // UserPromptSubmit：hook 可阻止本次提交（对标 Claude Code）。
    if run_user_prompt_submit(&session.settings.hooks, user_input, permission_mode_str(session.permission_mode)).await
    {
        return Ok(QueryOutcome {
            messages: initial_messages,
            aborted: false,
        });
    }

    let mut messages = initial_messages;
    messages.push(Message::user_text(user_input));
    if let Some(t) = &session.transcript
        && let Err(e) = t.append(messages.last().unwrap())
    {
        (ui.on_warning)(format!("transcript append failed: {e}"));
    }
    let mut recovery_count = 0u32;
    let mut stop_hook_fired = false;
    let mut cancel_rx = cancel;
    loop {
        check_and_compact(session, &mut messages).await;
        let turn = one_turn(
            &session.client,
            &session.model,
            &messages,
            &tools,
            &session.system,
            &mut *ui,
            cancel_rx.as_mut(),
        )
        .await?;
        if turn.aborted {
            // 中断：整轮丢弃（assistant 不完整），已执行/未执行工具都不回填。
            println!();
            return Ok(QueryOutcome { messages, aborted: true });
        }
        if let Some(t) = &session.transcript
            && let Err(e) = t.append(&turn.assistant)
        {
            (ui.on_warning)(format!("transcript append failed: {e}"));
        }
        if turn.tool_uses.is_empty() {
            // 输出预算截断恢复：注入"继续"消息重试（上限 3 次），对齐 Claude Code。
            if turn.stop_reason.as_deref() == Some("max_tokens")
                && recovery_count < MAX_OUTPUT_TOKENS_RECOVERY_LIMIT
            {
                recovery_count += 1;
                messages.push(Message::user_text(MAX_TOKENS_RESUME_PROMPT));
                continue;
            }
            // Stop hooks：exit 2 → blocking stderr 注入模型并重试一次（防循环）。
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
            return Ok(QueryOutcome { messages, aborted: false });
        }
        messages.push(turn.assistant);

        // 阶段 1：逐工具走权限门（串行，可能交互；hook 可改写输入）
        let mut pending: Vec<PendingCall> = Vec::new();
        let mut blocks: Vec<ContentBlock> = Vec::new();
        for tool_use in &turn.tool_uses {
            let (id, name, input) = match tool_use {
                ContentBlock::ToolUse { id, name, input } => (id.clone(), name.clone(), input.clone()),
                _ => unreachable!(),
            };
            let Some(tool) = find_tool(&tools, &name) else {
                blocks.push(tool_result_error(
                    &id,
                    format!("<tool_use_error>No such tool: {name}</tool_use_error>"),
                ));
                continue;
            };
            let (behavior, reason, gated_input) = gate_tool(
                tool,
                &input,
                session.permission_mode,
                &session.settings.hooks,
                &session.settings.permissions,
                &*ui.ask,
            )
            .await;
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
                    // 拒绝也要收尾 UI 活动：工具行显示被拒绝而不是永远旋转。
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

        // 阶段 2：队列执行（safe 并行 / 非 safe 串行）。
        // 中断语义对标 interruptBehavior: 'block'：已入队工具继续跑完，结果照常回填。
        let mut stop_after_tools = false;
        let outcomes = execute_calls(pending, &ctx).await;
        for outcome in outcomes {
            match outcome.result {
                Ok(result) => {
                    blocks.push(result_block(&outcome.tool_use_id, &result));
                    if let Some(ContentBlock::ToolUse { name, input, .. }) = turn
                        .tool_uses
                        .iter()
                        .find(|t| matches!(t, ContentBlock::ToolUse { id, .. } if id == &outcome.tool_use_id))
                    {
                        let text = clipped_result(render_result(&result));
                        (ui.on_tool_done)(&ToolCallDone {
                            name: name.clone(),
                            summary: summarize_input(name, input),
                            output: text,
                            is_error: result.is_error,
                            diff: result.diff.clone(),
                            duration_ms: outcome.duration_ms,
                        });
                        // PostToolUse exit 2 → 阻断继续（hook 的 blocking error 语义）。
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
                    blocks.push(tool_result_error(
                        &outcome.tool_use_id,
                        format!("<tool_use_error>{e}</tool_use_error>"),
                    ));
                }
            }
        }

        messages.push(Message {
            role: Role::User,
            content: blocks,
        });
        if let Some(t) = &session.transcript
            && let Err(e) = t.append(messages.last().unwrap())
        {
            (ui.on_warning)(format!("transcript append failed: {e}"));
        }
        // 本批工具全部收口：折叠组按批聚合，下一轮模型响应的工具开新组。
        (ui.on_round_end)();
        if stop_after_tools || is_cancelled(&cancel_rx) {
            return Ok(QueryOutcome {
                messages,
                aborted: is_cancelled(&cancel_rx),
            });
        }
    }
}

fn is_cancelled(cancel: &Option<watch::Receiver<bool>>) -> bool {
    cancel.as_ref().is_some_and(|rx| *rx.borrow())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn clamps_400_recomputation() {
        // max(3000, C − A − 1000)：窗口只剩 500 → 保底 3000
        let rem = 200_000u64.checked_sub(198_500).unwrap();
        let recomputed = rem.saturating_sub(1000).max(3000);
        assert_eq!(recomputed, 3000);
    }
}
