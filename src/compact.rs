use std::sync::atomic::{AtomicBool, Ordering};

use crate::api::types::{ContentBlock, Message, Request, SystemBlock};
use crate::budget::{AUTOCOMPACT_THRESHOLD, MAX_COMPACT_FAILURES, WARNING_THRESHOLD};
use crate::hooks::{run_post_compact, run_pre_compact};
use crate::permission::PermissionMode;
use crate::query::Session;

/// 压缩后保留的最近消息条数。
const KEEP_RECENT: usize = 8;

/// count_tokens 实测间隔（轮）：每轮一次 = 一次额外往返，20 工具轮就是 20 次。
const COUNT_TOKENS_INTERVAL: u32 = 5;

/// 本地估算比上次实测涨过这么多就提前实测一次。
const COUNT_TOKENS_GROWTH: u64 = 20_000;

/// count_tokens 不可用只警告一次（非 Anthropic 端点每轮刷屏没有意义）。
static COUNT_TOKENS_WARNED: AtomicBool = AtomicBool::new(false);

const COMPACT_PROMPT: &str = "\
你是对话压缩器。把下面的 agent 对话压缩成一段结构化摘要，要求：
- 保留关键决策、文件路径、执行的命令及其结果、结论
- 保留尚未完成的待办与约束
- 输出纯文本，300 字以内
对话内容：
";

/// 压缩切点：从 split 向后推进到第一条不含 tool_result 的消息边界。
/// 硬切会把 assistant(tool_use)/user(tool_result) 对切开，保留侧首条成为
/// 孤儿 tool_result，此后每个请求都 400。
fn safe_split(messages: &[Message], split: usize) -> usize {
    let mut split = split;
    while split < messages.len()
        && messages[split]
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolResult { .. }))
    {
        split += 1;
    }
    split
}

/// 旧消息 → 摘要提示词。
fn summary_prompt(old: &[Message]) -> String {
    let mut prompt = String::from(COMPACT_PROMPT);
    for message in old {
        let text = message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.clone()),
                ContentBlock::ToolResult { content, .. } => Some(content.to_string()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        prompt.push_str(&format!("\n---\n{text}"));
    }
    prompt
}

/// 上下文超阈值时自动压缩：旧消息 → 模型摘要，保留最近 KEEP_RECENT 条。
/// 成功清零熔断计数；失败递增（连续 MAX_COMPACT_FAILURES 次后由调用方跳过）。
/// 摘要拿到之前不动 messages——失败时原样保留，绝不用占位串顶替真实历史。
/// 返回是否发生了压缩。
pub async fn maybe_compact(
    session: &Session,
    messages: &mut Vec<Message>,
    tokens: u64,
) -> bool {
    if messages.len() <= KEEP_RECENT {
        return false;
    }
    if tokens < AUTOCOMPACT_THRESHOLD {
        return false;
    }

    let split = safe_split(messages, messages.len() - KEEP_RECENT);

    run_pre_compact(&session.settings.hooks, permission_mode_str(session.permission_mode)).await;

    let request = Request {
        model: session.runtime.model.borrow().clone(),
        max_tokens: 1024,
        system: Vec::new(),
        messages: vec![Message::user_text(summary_prompt(&messages[..split]))],
        tools: Vec::new(),
        stream: false,
        thinking: None,
        output_config: None,
    };
    let summary = match session.client.complete_text(&request).await {
        Ok(summary) if !summary.trim().is_empty() => {
            session.compact_failures.store(0, Ordering::SeqCst);
            summary
        }
        outcome => {
            session.compact_failures.fetch_add(1, Ordering::SeqCst);
            if !session.quiet {
                match outcome {
                    Err(e) => eprintln!(
                        "[bingo] warning: context compaction failed, history kept as-is: {e}"
                    ),
                    _ => eprintln!(
                        "[bingo] warning: context compaction returned an empty summary, history kept as-is"
                    ),
                }
            }
            return false;
        }
    };

    messages.splice(
        ..split,
        [Message::user_text(format!(
            "（此前对话的摘要，来自自动压缩）\n{summary}"
        ))],
    );

    run_post_compact(&session.settings.hooks, permission_mode_str(session.permission_mode)).await;
    eprintln!("[bingo] compacted {split} old messages");
    true
}

/// count_tokens 不可用时的本地估算：约 4 字符 1 token。
/// 非 Anthropic 端点（DeepSeek/ollama）没有这个接口，静默 return
/// 会让自动压缩永不触发、上下文一路涨到爆。
fn estimate_tokens(system: &[SystemBlock], messages: &[Message]) -> u64 {
    let mut chars: usize = system.iter().map(|b| b.text.chars().count()).sum();
    for message in messages {
        for block in &message.content {
            chars += match block {
                ContentBlock::Text { text } => text.chars().count(),
                ContentBlock::Thinking { thinking, .. } => thinking.chars().count(),
                ContentBlock::ToolUse { name, input, .. } => {
                    name.chars().count() + input.to_string().chars().count()
                }
                ContentBlock::ToolResult { content, .. } => content.to_string().chars().count(),
                // 图片块按 base64 长度估算（真实占位大头）。
                ContentBlock::Image { source } => source.data.chars().count(),
            };
        }
    }
    (chars / 4) as u64
}

/// count_tokens 调用节流：回合开始必测，之后每 COUNT_TOKENS_INTERVAL 轮、
/// 或本地估算比上次实测涨过 COUNT_TOKENS_GROWTH 才再测；其余轮按
/// 「上次实测 + 估算增量」外推，避免每轮都多一次往返。
#[derive(Debug, Default)]
pub struct TokenGate {
    /// (上次实测值, 当时的本地估算)。
    last: Option<(u64, u64)>,
    turns_since_exact: u32,
}

impl TokenGate {
    pub fn new() -> Self {
        Self::default()
    }

    fn wants_exact(&self, estimate: u64) -> bool {
        let Some((_, estimate_then)) = self.last else {
            return true;
        };
        self.turns_since_exact >= COUNT_TOKENS_INTERVAL
            || estimate.saturating_sub(estimate_then) >= COUNT_TOKENS_GROWTH
    }

    fn record_exact(&mut self, exact: u64, estimate: u64) {
        self.last = Some((exact, estimate));
        self.turns_since_exact = 0;
    }

    /// 未实测的轮：按估算增量从上次实测值外推。
    fn project(&mut self, estimate: u64) -> u64 {
        self.turns_since_exact = self.turns_since_exact.saturating_add(1);
        match self.last {
            Some((exact, estimate_then)) => exact + estimate.saturating_sub(estimate_then),
            None => estimate,
        }
    }
}

/// 每轮请求前调用：token 超阈值即压缩；熔断后跳过并提醒。
pub async fn check_and_compact(
    session: &Session,
    messages: &mut Vec<Message>,
    gate: &mut TokenGate,
) {
    let estimate = estimate_tokens(&session.system, messages);
    let tokens = if gate.wants_exact(estimate) {
        let model = session.runtime.model.borrow().clone();
        match session
            .client
            .count_tokens(&model, &session.system, messages)
            .await
        {
            Ok(exact) => {
                gate.record_exact(exact, estimate);
                exact
            }
            Err(e) => {
                if !COUNT_TOKENS_WARNED.swap(true, Ordering::SeqCst) && !session.quiet {
                    eprintln!(
                        "[bingo] warning: count_tokens unavailable ({e}); \
                         falling back to a local estimate for auto-compact"
                    );
                }
                gate.project(estimate)
            }
        }
    } else {
        gate.project(estimate)
    };

    if tokens > 0 && !session.quiet {
        eprintln!("[bingo] context: {tokens} tokens");
    }
    if tokens >= AUTOCOMPACT_THRESHOLD {
        if session.compact_failures.load(Ordering::SeqCst) >= MAX_COMPACT_FAILURES {
            if !session.quiet {
                eprintln!(
                    "[bingo] warning: auto-compact disabled after {MAX_COMPACT_FAILURES} consecutive failures"
                );
            }
        } else {
            maybe_compact(session, messages, tokens).await;
        }
    } else if tokens >= WARNING_THRESHOLD && !session.quiet {
        eprintln!(
            "[bingo] warning: context at {tokens} tokens, auto-compact at {AUTOCOMPACT_THRESHOLD}"
        );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::Role;

    fn text(role: Role, body: &str) -> Message {
        Message { role, content: vec![ContentBlock::Text { text: body.into() }] }
    }

    fn tool_use(id: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: id.into(),
                name: "Bash".into(),
                input: serde_json::json!({}),
            }],
        }
    }

    fn tool_result(id: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: id.into(),
                content: serde_json::Value::String("ok".into()),
                is_error: false,
            }],
        }
    }

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn compact_threshold_matches_budget() {
        assert!(AUTOCOMPACT_THRESHOLD < crate::budget::CONTEXT_WINDOW);
    }

    /// 切点落在 tool_use/tool_result 对中间时向后推进：
    /// 保留侧首条不得是孤儿 tool_result（否则此后每个请求都 400）。
    #[test]
    fn split_advances_past_tool_result_boundary() {
        let messages = vec![
            text(Role::User, "hi"),
            tool_use("tu_1"),
            tool_result("tu_1"),
            text(Role::Assistant, "done"),
        ];
        // 硬切点 2 会把 tool_use/tool_result 对切开。
        let split = safe_split(&messages, 2);
        assert_eq!(split, 3, "推进到 tool_result 之后");
        assert!(
            !messages[split]
                .content
                .iter()
                .any(|b| matches!(b, ContentBlock::ToolResult { .. })),
            "保留侧首条不是 tool_result"
        );
    }

    /// 连续多个 tool_result 也要一路推过去。
    #[test]
    fn split_advances_past_consecutive_tool_results() {
        let messages = vec![
            tool_use("a"),
            tool_result("a"),
            tool_result("b"),
            tool_result("c"),
            text(Role::Assistant, "done"),
        ];
        assert_eq!(safe_split(&messages, 1), 4);
    }

    #[test]
    fn split_is_unchanged_when_already_on_a_boundary() {
        let messages = vec![text(Role::User, "a"), text(Role::Assistant, "b")];
        assert_eq!(safe_split(&messages, 1), 1);
    }

    /// 全是 tool_result 时推到末尾，maybe_compact 会因 split == len 而全量压缩，
    /// 但绝不会越界。
    #[test]
    fn split_never_exceeds_len() {
        let messages = vec![tool_result("a"), tool_result("b")];
        assert_eq!(safe_split(&messages, 0), messages.len());
    }

    /// count_tokens 不可用时的本地估算：随内容单调增长，不恒为 0。
    #[test]
    fn local_estimate_grows_with_content() {
        let system = vec![SystemBlock { text: "s".repeat(400), cache: false }];
        let empty = estimate_tokens(&system, &[]);
        assert_eq!(empty, 100, "400 字符 ≈ 100 token");

        let messages = vec![text(Role::User, &"x".repeat(4_000))];
        let with_message = estimate_tokens(&system, &messages);
        assert!(with_message > empty);
        assert_eq!(with_message, 1_100);

        // tool_use / tool_result 也计入，不然工具轮的增长看不见。
        let with_tools = estimate_tokens(&system, &[tool_use("a"), tool_result("a")]);
        assert!(with_tools > empty);
    }

    /// 节流：首轮必测；随后按间隔或估算增量决定，其余轮外推。
    #[test]
    fn token_gate_throttles_exact_counts() {
        let mut gate = TokenGate::new();
        assert!(gate.wants_exact(1_000), "回合开始必测");
        gate.record_exact(5_000, 1_000);

        assert!(!gate.wants_exact(1_100), "小增长不再实测");
        assert_eq!(gate.project(1_100), 5_100, "按估算增量外推");

        assert!(
            gate.wants_exact(1_000 + COUNT_TOKENS_GROWTH),
            "估算涨过阈值就提前实测"
        );

        let mut gate = TokenGate::new();
        gate.record_exact(5_000, 1_000);
        for _ in 0..COUNT_TOKENS_INTERVAL {
            gate.project(1_000);
        }
        assert!(gate.wants_exact(1_000), "满 N 轮必测");
    }
}
