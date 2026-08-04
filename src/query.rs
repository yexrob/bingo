use std::io::{BufRead, Write};

use futures_util::StreamExt;
use thiserror::Error;

use crate::api::client::{AssistantAccumulator, Client, ClientError};
use crate::api::types::{ContentBlock, Message, Request, StreamEvent, Role, DEFAULT_MAX_TOKENS};
use crate::permission::{can_use_tool, PermissionBehavior, PermissionMode};
use crate::tool::{find_tool, tool_params, Tool, ToolContext, ToolError, ToolResult};

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
    on_event: impl FnMut(&StreamEvent),
) -> Result<(Message, Vec<ContentBlock>), QueryError> {
    let request = Request {
        model: model.to_string(),
        max_tokens: DEFAULT_MAX_TOKENS,
        system: String::new(),
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

/// 执行一个 tool_use：查注册表 → 权限门 → call。
async fn execute_tool(
    tool: &dyn Tool,
    input: &serde_json::Value,
    ctx: &ToolContext,
) -> Result<ToolResult, ToolError> {
    tool.call(input.clone(), ctx).await
}

fn tool_result_block(tool_use_id: &str, content: serde_json::Value, is_error: bool) -> ContentBlock {
    ContentBlock::ToolResult {
        tool_use_id: tool_use_id.to_string(),
        content,
        is_error,
    }
}

fn tool_result_text(tool_use_id: &str, text: impl Into<String>) -> ContentBlock {
    tool_result_block(tool_use_id, serde_json::Value::String(text.into()), false)
}

fn tool_result_error(tool_use_id: &str, text: impl Into<String>) -> ContentBlock {
    tool_result_block(tool_use_id, serde_json::Value::String(text.into()), true)
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

/// 权限门 + 交互：返回最终行为。
async fn gate_tool(
    tool: &dyn Tool,
    input: &serde_json::Value,
    mode: PermissionMode,
) -> PermissionBehavior {
    let decision = can_use_tool(tool, input, mode);
    match decision.behavior {
        PermissionBehavior::Ask => {
            if interactive_confirm(&format!("允许 {} 执行 {:?} 吗？", tool.name(), input)) {
                PermissionBehavior::Allow
            } else {
                PermissionBehavior::Deny
            }
        }
        other => other,
    }
}

/// queryLoop：多轮 tool loop，直到 end_turn。
pub async fn run_query(
    client: &Client,
    model: &str,
    permission_mode: PermissionMode,
    user_input: &str,
) -> Result<(), QueryError> {
    let tools = crate::tools::base_tools();
    let ctx = ToolContext {
        cwd: std::env::current_dir().map_err(|e| QueryError::Tool(ToolError::failed(e.to_string())))?,
    };

    let mut messages = vec![Message::user_text(user_input)];
    loop {
        let (assistant, tool_uses) =
            one_turn(client, model, &messages, &tools, print_text_delta).await?;
        if tool_uses.is_empty() {
            println!();
            return Ok(());
        }
        messages.push(assistant);

        let mut results = Vec::new();
        for tool_use in tool_uses {
            let (tool_use_id, name, input) = match &tool_use {
                ContentBlock::ToolUse { id, name, input } => (id.clone(), name.clone(), input.clone()),
                _ => unreachable!(),
            };
            let block = match find_tool(&tools, &name) {
                None => tool_result_error(
                    &tool_use_id,
                    format!("<tool_use_error>No such tool: {name}</tool_use_error>"),
                ),
                Some(tool) => {
                    let behavior = gate_tool(tool, &input, permission_mode).await;
                    match behavior {
                        PermissionBehavior::Allow => {
                            match execute_tool(tool, &input, &ctx).await {
                                Ok(result) => tool_result_text(&tool_use_id, render_result(&result)),
                                Err(e) => tool_result_error(&tool_use_id, e.to_string()),
                            }
                        }
                        PermissionBehavior::Deny => tool_result_error(
                            &tool_use_id,
                            format!("<permission_error>permission denied: {}</permission_error>", tool.name()),
                        ),
                        PermissionBehavior::Ask => unreachable!("ask resolved by gate_tool"),
                    }
                }
            };
            results.push(block);
        }
        messages.push(Message {
            role: Role::User,
            content: results,
        });
    }
}

/// tool_result 的 content 统一渲染为文本。
fn render_result(result: &ToolResult) -> String {
    match &result.content {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}
