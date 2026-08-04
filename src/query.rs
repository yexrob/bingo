use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;

use futures_util::StreamExt;
use thiserror::Error;

use crate::api::client::{AssistantAccumulator, Client, ClientError};
use crate::api::types::{
    ContentBlock, Message, Request, StreamEvent, SystemBlock, Role, DEFAULT_MAX_TOKENS,
};
use crate::compact::check_and_compact;
use crate::hooks::{run_post_tool_use, run_pre_tool_use};
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
}

/// 单个工具完成事件。
#[derive(Debug, Clone)]
pub struct ToolCallDone {
    pub name: String,
    pub summary: String,
    pub output: String,
    pub is_error: bool,
}

/// 异步权限询问回调：工具名 + 理由 → 是否允许。
pub type AskFn = dyn Fn(&str, &str) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
    + Send
    + Sync;

/// UI 挂钩：流事件、工具完成、权限询问、非致命警告。
pub struct UiHooks {
    pub on_event: Box<dyn FnMut(&StreamEvent) + Send>,
    pub on_tool_done: Box<dyn Fn(&ToolCallDone) + Send>,
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
        on_tool_done: Box::new(|_| {}),
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

/// 单轮：请求一次模型，累积 assistant 回复。
/// 返回 (assistant 消息, 该轮产生的 tool_use 块)。
async fn one_turn(
    client: &Client,
    model: &str,
    messages: &[Message],
    tools: &[Box<dyn Tool>],
    system: &[SystemBlock],
    on_event: &mut (dyn FnMut(&StreamEvent) + Send),
) -> Result<(Message, Vec<ContentBlock>), QueryError> {
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
    while let Some(event) = stream.next().await {
        let event = event?;
        on_event(&event);
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
                }
            }
            _ => {}
        }
    }
    Ok((acc.message(), tool_uses))
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

    let decision = can_use_tool(tool, &hook_input, mode);
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

fn render_result(result: &ToolResult) -> String {
    match &result.content {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn result_block(tool_use_id: &str, result: &ToolResult) -> ContentBlock {
    if result.is_error {
        tool_result_error(tool_use_id, render_result(result))
    } else {
        tool_result_text(tool_use_id, render_result(result))
    }
}

fn summarize_input(tool_name: &str, input: &serde_json::Value) -> String {
    match (tool_name, input) {
        // Bash 摘要直接显示命令（Claude Code 风格）
        ("Bash", serde_json::Value::Object(map)) => map
            .get("command")
            .and_then(|c| c.as_str())
            .map(|c| format!("$ {c}"))
            .unwrap_or_else(|| "Bash".to_string()),
        (_, serde_json::Value::Object(map)) => map
            .iter()
            .take(1)
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(" "),
        (_, other) => other.to_string(),
    }
}

/// queryLoop：多轮 tool loop，直到 end_turn。
/// 返回本次查询产生的全部消息（供记忆提取等消费）。
pub async fn run_query(
    session: &Arc<Session>,
    initial_messages: Vec<Message>,
    user_input: &str,
    ui: &mut UiHooks,
) -> Result<Vec<Message>, QueryError> {
    let tools = crate::tools::assemble_tools(session, &mut ui.on_warning).await;
    let ctx = ToolContext {
        cwd: std::env::current_dir()
            .map_err(|e| QueryError::Tool(ToolError::failed(e.to_string())))?,
    };

    let mut messages = initial_messages;
    messages.push(Message::user_text(user_input));
    if let Some(t) = &session.transcript
        && let Err(e) = t.append(messages.last().unwrap())
    {
        (ui.on_warning)(format!("transcript append failed: {e}"));
    }
    loop {
        check_and_compact(session, &mut messages).await;
        let (assistant, tool_uses) = one_turn(
            &session.client,
            &session.model,
            &messages,
            &tools,
            &session.system,
            &mut ui.on_event as &mut (dyn FnMut(&StreamEvent) + Send),
        )
        .await?;
        if let Some(t) = &session.transcript
            && let Err(e) = t.append(&assistant)
        {
            (ui.on_warning)(format!("transcript append failed: {e}"));
        }
        if tool_uses.is_empty() {
            println!();
            return Ok(messages);
        }
        messages.push(assistant);

        // 阶段 1：逐工具走权限门（串行，可能交互；hook 可改写输入）
        let mut pending: Vec<PendingCall> = Vec::new();
        let mut blocks: Vec<ContentBlock> = Vec::new();
        for tool_use in &tool_uses {
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
                    });
                }
                PermissionBehavior::Ask => unreachable!("ask resolved by gate_tool"),
            }
        }

        // 阶段 2：队列执行（safe 并行 / 非 safe 串行）
        let outcomes = execute_calls(pending, &ctx).await;
        for outcome in outcomes {
            match outcome.result {
                Ok(result) => {
                    blocks.push(result_block(&outcome.tool_use_id, &result));
                    if let Some(ContentBlock::ToolUse { name, input, .. }) = tool_uses
                        .iter()
                        .find(|t| matches!(t, ContentBlock::ToolUse { id, .. } if id == &outcome.tool_use_id))
                    {
                        (ui.on_tool_done)(&ToolCallDone {
                            name: name.clone(),
                            summary: summarize_input(name, input),
                            output: render_result(&result),
                            is_error: result.is_error,
                        });
                        run_post_tool_use(
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
    }
}
