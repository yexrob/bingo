use futures_util::StreamExt;
use thiserror::Error;

use crate::api::client::{AssistantAccumulator, Client, ClientError};
use crate::api::types::{ContentBlock, Message, Request, StreamEvent, Role, DEFAULT_MAX_TOKENS};

#[derive(Debug, Error)]
pub enum QueryError {
    #[error(transparent)]
    Client(#[from] ClientError),
    #[error("stream protocol error: {0}")]
    Protocol(String),
}

/// headless 模式：把文本增量实时打到 stdout。
fn print_text_delta(event: &StreamEvent) {
    if let StreamEvent::TextDelta { text, .. } = event {
        use std::io::Write;
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
    on_event: impl FnMut(&StreamEvent),
) -> Result<(Message, Vec<ContentBlock>), QueryError> {
    let request = Request {
        model: model.to_string(),
        max_tokens: DEFAULT_MAX_TOKENS,
        system: String::new(),
        messages: messages.to_vec(),
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

/// queryLoop：多轮 tool loop，直到 end_turn。
pub async fn run_query(
    client: &Client,
    model: &str,
    user_input: &str,
) -> Result<(), QueryError> {
    let mut messages = vec![Message::user_text(user_input)];
    loop {
        let (assistant, tool_uses) =
            one_turn(client, model, &messages, print_text_delta).await?;
        if tool_uses.is_empty() {
            println!();
            return Ok(());
        }
        messages.push(assistant);
        let results: Vec<ContentBlock> = tool_uses
            .into_iter()
            .map(|tool_use| {
                let (id, name) = match &tool_use {
                    ContentBlock::ToolUse { id, name, .. } => (id.clone(), name.clone()),
                    _ => unreachable!(),
                };
                ContentBlock::ToolResult {
                    tool_use_id: id,
                    content: serde_json::json!(
                        format!("<tool_use_error>No such tool: {name} (tool registry empty in round 1)</tool_use_error>")
                    ),
                    is_error: true,
                }
            })
            .collect();
        messages.push(Message {
            role: Role::User,
            content: results,
        });
    }
}
