use std::io::{BufRead, Write};

use futures_util::StreamExt;
use thiserror::Error;

use crate::api::client::{AssistantAccumulator, Client, ClientError};
use crate::api::types::{
    ContentBlock, Message, Request, StreamEvent, SystemBlock, Role, DEFAULT_MAX_TOKENS,
};
use crate::budget::check_input_budget;
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

/// headless 模式：把文本增量实时打到 stdout。
fn print_text_delta(event: &StreamEvent) {
    if let StreamEvent::TextDelta { text, .. } = event {
        let _ = std::io::stdout().write_all(text.as_bytes());
        let _ = std::io::stdout().flush();
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
    on_event: impl FnMut(&StreamEvent),
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
    let mut on_event = on_event;
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

/// 交互确认（headless）：stderr 提示，stdin 读 y/n。stdin 不可用时视为拒绝。
fn interactive_confirm(prompt: &str) -> bool {
    eprintln!("{prompt} [y/N]");
    let mut line = String::new();
    let Ok(_) = std::io::stdin().lock().read_line(&mut line) else {
        return false;
    };
    let answer = line.trim().to_ascii_lowercase();
    answer == "y" || answer == "yes"
}

/// 权限门 + PreToolUse hook + 交互：返回最终决策与（可能被改写的）输入。
async fn gate_tool(
    tool: &dyn Tool,
    input: &serde_json::Value,
    mode: PermissionMode,
    hooks: &HooksConfig,
) -> (PermissionBehavior, String, serde_json::Value) {
    let (hook_behavior, hook_reason, hook_input) = run_pre_tool_use(
        hooks,
        tool.name(),
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
            if interactive_confirm(&format!(
                "允许 {} 执行 {:?} 吗？",
                tool.name(),
                hook_input
            )) {
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

/// 一轮查询的上下文。
pub struct QueryConfig<'a> {
    pub client: &'a Client,
    pub model: &'a str,
    pub permission_mode: PermissionMode,
    pub settings: &'a Settings,
    pub system: &'a [SystemBlock],
    pub transcript: &'a Option<Transcript>,
    pub initial_messages: Vec<Message>,
}

/// queryLoop：多轮 tool loop，直到 end_turn。
pub async fn run_query(cfg: QueryConfig<'_>, user_input: &str) -> Result<(), QueryError> {
    let QueryConfig {
        client,
        model,
        permission_mode,
        settings,
        system,
        transcript,
        initial_messages,
    } = cfg;
    let tools = crate::tools::base_tools();
    let ctx = ToolContext {
        cwd: std::env::current_dir()
            .map_err(|e| QueryError::Tool(ToolError::failed(e.to_string())))?,
    };

    let mut warned = false;
    let mut messages = initial_messages;
    messages.push(Message::user_text(user_input));
    loop {
        check_input_budget(client, model, system, &messages, &mut warned).await;
        let (assistant, tool_uses) =
            one_turn(client, model, &messages, &tools, system, print_text_delta).await?;
        if let Some(t) = transcript {
            let _ = t.append(&assistant);
        }
        if tool_uses.is_empty() {
            println!();
            return Ok(());
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
            let (behavior, reason, gated_input) = gate_tool(tool, &input, permission_mode, &settings.hooks).await;
            match behavior {
                PermissionBehavior::Allow => pending.push(PendingCall {
                    tool_use_id: id,
                    tool,
                    input: gated_input,
                }),
                PermissionBehavior::Deny => blocks.push(tool_result_error(
                    &id,
                    format!(
                        "<permission_error>permission denied: {name} ({reason})</permission_error>"
                    ),
                )),
                PermissionBehavior::Ask => unreachable!("ask resolved by gate_tool"),
            }
        }

        // 阶段 2：队列执行（safe 并行 / 非 safe 串行）
        let outcomes = execute_calls(pending, &ctx).await;
        for outcome in outcomes {
            match outcome.result {
                Ok(result) => {
                    blocks.push(tool_result_text(&outcome.tool_use_id, render_result(&result)));
                    if let Some(ContentBlock::ToolUse { name, input, .. }) = tool_uses
                        .iter()
                        .find(|t| matches!(t, ContentBlock::ToolUse { id, .. } if id == &outcome.tool_use_id))
                    {
                        run_post_tool_use(
                            &settings.hooks,
                            name,
                            input,
                            &result.content,
                            permission_mode_str(permission_mode),
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
        if let Some(t) = transcript {
            let _ = t.append(messages.last().unwrap());
        }
    }
}
