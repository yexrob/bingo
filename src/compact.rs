use crate::api::types::{ContentBlock, Message, Request};
use crate::hooks::{run_post_compact, run_pre_compact};
use crate::permission::PermissionMode;
use crate::query::Session;

/// 压缩触发阈值：占用上下文窗口比例。
const COMPACT_FRACTION: f64 = 0.9;
/// 压缩后保留的最近消息条数。
const KEEP_RECENT: usize = 8;

const COMPACT_PROMPT: &str = "\
你是对话压缩器。把下面的 agent 对话压缩成一段结构化摘要，要求：
- 保留关键决策、文件路径、执行的命令及其结果、结论
- 保留尚未完成的待办与约束
- 输出纯文本，300 字以内
对话内容：
";

/// 上下文超阈值时自动压缩：旧消息 → 模型摘要，保留最近 KEEP_RECENT 条。
/// 返回是否发生了压缩。
pub async fn maybe_compact(
    session: &Session,
    messages: &mut Vec<Message>,
    tokens: u64,
) -> bool {
    if messages.len() <= KEEP_RECENT {
        return false;
    }
    let fraction = tokens as f64 / crate::budget::CONTEXT_WINDOW as f64;
    if fraction < COMPACT_FRACTION {
        return false;
    }

    let split = messages.len() - KEEP_RECENT;
    let old: Vec<Message> = messages.drain(..split).collect();
    let recent: Vec<Message> = std::mem::take(messages);

    run_pre_compact(&session.settings.hooks, permission_mode_str(session.permission_mode)).await;

    let mut prompt_text = String::from(COMPACT_PROMPT);
    for message in &old {
        let text = message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.clone()),
                ContentBlock::ToolResult { content, .. } => {
                    Some(content.to_string())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        prompt_text.push_str(&format!("\n---\n{text}"));
    }

    let request = Request {
        model: session.model.clone(),
        max_tokens: 1024,
        system: Vec::new(),
        messages: vec![Message::user_text(prompt_text)],
        tools: Vec::new(),
        stream: false,
    };
    let summary = session
        .client
        .complete_text(&request)
        .await
        .unwrap_or_else(|_| "[compact failed: no summary]".to_string());

    let mut compacted = vec![Message::user_text(format!(
        "（此前对话的摘要，来自自动压缩）\n{summary}"
    ))];
    compacted.extend(recent);
    *messages = compacted;

    run_post_compact(&session.settings.hooks, permission_mode_str(session.permission_mode)).await;
    eprintln!("[bingo] compacted {} old messages", old.len());
    true
}

/// 每轮请求前调用：token 超阈值即压缩。
pub async fn check_and_compact(session: &Session, messages: &mut Vec<Message>) {
    let Ok(tokens) = session
        .client
        .count_tokens(&session.model, &session.system, messages)
        .await
    else {
        return;
    };
    if tokens > 0 {
        eprintln!("[bingo] context: {tokens} tokens");
    }
    maybe_compact(session, messages, tokens).await;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fraction_threshold() {
        let tokens = (COMPACT_FRACTION * crate::budget::CONTEXT_WINDOW as f64) as u64;
        assert!(tokens as f64 / crate::budget::CONTEXT_WINDOW as f64 >= COMPACT_FRACTION);
    }

    #[test]
    fn keeps_recent_budget() {
        let tokens = (COMPACT_FRACTION * crate::budget::CONTEXT_WINDOW as f64) as u64;
        assert!(tokens > 0);
    }
}
