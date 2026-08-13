use std::sync::atomic::{AtomicBool, Ordering};

use crate::api::contract::{NeutralRequest, SystemBlock};
use crate::api::types::{ContentBlock, Message};
use crate::budget::{MAX_COMPACT_FAILURES, autocompact_threshold_for, warning_threshold_for};
use crate::hooks::{run_post_compact, run_pre_compact};
use crate::permission::PermissionMode;
use crate::query::Session;

/// Number of most recent messages kept after compaction.
const KEEP_RECENT: usize = 8;

/// count_tokens measurement interval (turns): measuring every turn = one extra round
/// trip each; 20 tool turns = 20 round trips.
const COUNT_TOKENS_INTERVAL: u32 = 5;

/// Measure early when the local estimate has grown this much over the last exact count.
const COUNT_TOKENS_GROWTH: u64 = 20_000;

/// Warn only once when count_tokens is unavailable (no point spamming every turn on
/// non-Anthropic endpoints).
static COUNT_TOKENS_WARNED: AtomicBool = AtomicBool::new(false);

const COMPACT_PROMPT: &str = "\
You are a conversation compactor. Compress the agent conversation below into one structured summary:
- Keep key decisions, file paths, executed commands and their results, and conclusions
- Keep unfinished todos and constraints
- Output plain text within 300 characters
Conversation content:
";

/// Compaction split point: advance from split to the first message boundary that
/// contains no tool_result.
/// A hard cut would split the assistant(tool_use)/user(tool_result) pair, leaving an
/// orphan tool_result as the kept side's first message — every later request then 400s.
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

/// Old messages → summary prompt.
fn summary_prompt(old: &[Message]) -> String {
    let mut prompt = String::from(COMPACT_PROMPT);
    for message in old {
        let text = message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.clone()),
                ContentBlock::ToolResult { content, .. } => {
                    Some(crate::api::types::tool_result_text(content))
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        prompt.push_str(&format!("\n---\n{text}"));
    }
    prompt
}

/// Auto-compact when context exceeds the threshold: old messages → model summary,
/// keep the most recent KEEP_RECENT.
/// Success resets the circuit breaker; failure increments it (after
/// MAX_COMPACT_FAILURES consecutive failures the caller skips).
/// messages aren't touched until the summary arrives — on failure history is kept
/// verbatim, never replaced by a placeholder string.
/// Returns whether compaction happened.
pub async fn maybe_compact(session: &Session, messages: &mut Vec<Message>, tokens: u64) -> bool {
    if messages.len() <= KEEP_RECENT {
        return false;
    }
    if tokens
        < autocompact_threshold_for(
            &session.client.models(),
            &session.runtime.model.borrow().clone(),
        )
    {
        return false;
    }
    compact(session, messages).await
}

/// Recovery after the server rejected the request as too long. Takes the gate
/// because compaction invalidates it: the anchor describes the pre-compaction
/// history and projection floors at that anchor (`saturating_sub` eats the
/// drop), so a kept anchor reads the shrunken history at its old size and
/// compacts what was just compacted.
pub async fn compact_after_overflow(
    session: &Session,
    messages: &mut Vec<Message>,
    gate: &mut TokenGate,
) -> bool {
    if session.compact_failures.load(Ordering::SeqCst) >= MAX_COMPACT_FAILURES {
        if !session.quiet {
            eprintln!(
                "[bingo] warning: overflow compaction disabled after {MAX_COMPACT_FAILURES} consecutive failures"
            );
        }
        return false;
    }
    if messages.len() <= KEEP_RECENT {
        session.compact_failures.fetch_add(1, Ordering::SeqCst);
        return false;
    }
    let compacted = compact(session, messages).await;
    if compacted {
        gate.reset();
    }
    compacted
}

async fn compact(session: &Session, messages: &mut Vec<Message>) -> bool {
    let split = safe_split(messages, messages.len() - KEEP_RECENT);

    run_pre_compact(
        &session.settings.hooks,
        permission_mode_str(session.permission_mode),
        &session.cwd(),
    )
    .await;

    let request = NeutralRequest {
        model: session.runtime.model.borrow().clone(),
        max_tokens: 1024,
        system: Vec::new(),
        messages: vec![Message::user_text(summary_prompt(&messages[..split]))],
        tools: Vec::new(),
        stream: false,
        thinking: None,
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
            "(summary of the earlier conversation, from automatic compaction)\n{summary}"
        ))],
    );

    run_post_compact(
        &session.settings.hooks,
        permission_mode_str(session.permission_mode),
        &session.cwd(),
    )
    .await;
    eprintln!("[bingo] compacted {split} old messages");
    true
}

/// Quarter-token units for one text fragment: ASCII ≈ 4 chars/token, other
/// scripts (CJK etc.) ≈ 1 token/char. A flat chars/4 undercounted Chinese
/// conversations ~4x, so growth between exact counts went mostly unseen and
/// auto-compact fired far too late.
pub(crate) fn text_units(text: &str) -> u64 {
    text.chars().map(|c| if c.is_ascii() { 1 } else { 4 }).sum()
}

/// Local estimate when count_tokens is unavailable. Non-Anthropic endpoints
/// (DeepSeek/ollama) lack this API; silently returning would mean auto-compact
/// never triggers and context grows until it explodes.
/// tools must be the schemas actually sent with each request: they are input
/// tokens too, and skipping them (10k+ for the base pool) ate most of the
/// headroom between the compact threshold and the hard context limit.
pub(crate) fn estimate_tokens(
    system: &[SystemBlock],
    messages: &[Message],
    tools: &[serde_json::Value],
) -> u64 {
    let mut units: u64 = system.iter().map(|b| text_units(&b.text)).sum();
    units += tools
        .iter()
        .map(|schema| text_units(&schema.to_string()))
        .sum::<u64>();
    for message in messages {
        for block in &message.content {
            units += match block {
                ContentBlock::Text { text } => text_units(text),
                ContentBlock::Thinking { thinking, .. } => text_units(thinking),
                ContentBlock::ToolUse { name, input, .. } => {
                    text_units(name) + text_units(&input.to_string())
                }
                ContentBlock::ToolResult { content, .. } => text_units(&content.to_string()),
                // Image blocks estimated by base64 length (the real token hog).
                ContentBlock::Image { source } => source.data.chars().count() as u64,
            };
        }
    }
    units / 4
}

/// count_tokens call throttling: always measured at turn start, then every
/// COUNT_TOKENS_INTERVAL turns or when the local estimate has grown past
/// COUNT_TOKENS_GROWTH over the last exact count; other turns extrapolate from
/// "last exact + estimate delta" to avoid one extra round trip per turn.
#[derive(Debug, Default)]
pub struct TokenGate {
    /// (last exact count, local estimate at that time).
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

    pub(crate) fn current(&self, estimate: u64) -> u64 {
        match self.last {
            Some((exact, estimate_then)) => {
                exact.saturating_add(estimate.saturating_sub(estimate_then))
            }
            None => estimate,
        }
    }

    fn reset(&mut self) {
        self.last = None;
        self.turns_since_exact = 0;
    }

    /// Turns without an exact count: extrapolate from the last exact value by the
    /// estimate delta.
    fn project(&mut self, estimate: u64) -> u64 {
        self.turns_since_exact = self.turns_since_exact.saturating_add(1);
        self.current(estimate)
    }
}

/// Called before every turn's request: compact when tokens exceed the threshold; skip
/// and remind once the breaker has tripped.
/// tools are the schemas the next request will carry — measured with the request,
/// exactly, or the count reads under the real input size (see `estimate_tokens`).
pub async fn check_and_compact(
    session: &Session,
    messages: &mut Vec<Message>,
    gate: &mut TokenGate,
    tools: &[serde_json::Value],
) -> u64 {
    let estimate = estimate_tokens(&session.system, messages, tools);
    let tokens = if gate.wants_exact(estimate) {
        let model = session.runtime.model.borrow().clone();
        match session
            .client
            .count_tokens(&model, &session.system, messages, tools)
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
    let model = session.runtime.model.borrow().clone();
    let models = session.client.models();
    let threshold = autocompact_threshold_for(&models, &model);
    if tokens >= threshold {
        if session.compact_failures.load(Ordering::SeqCst) >= MAX_COMPACT_FAILURES {
            if !session.quiet {
                eprintln!(
                    "[bingo] warning: auto-compact disabled after {MAX_COMPACT_FAILURES} consecutive failures"
                );
            }
        } else if maybe_compact(session, messages, tokens).await {
            gate.reset();
            return estimate_tokens(&session.system, messages, tools);
        }
    } else if tokens >= warning_threshold_for(&models, &model) && !session.quiet {
        eprintln!("[bingo] warning: context at {tokens} tokens, auto-compact at {threshold}");
    }
    tokens
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
        Message {
            role,
            content: vec![ContentBlock::Text { text: body.into() }],
        }
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
        let models = crate::api::models::ModelResolver::default();
        assert!(
            crate::budget::autocompact_threshold_for(&models, "claude-sonnet-5")
                < crate::budget::context_window_for(&models, "claude-sonnet-5")
        );
    }

    /// When the split point lands mid tool_use/tool_result pair, advance:
    /// the kept side's first message must not be an orphan tool_result (otherwise every
    /// later request 400s).
    #[test]
    fn split_advances_past_tool_result_boundary() {
        let messages = vec![
            text(Role::User, "hi"),
            tool_use("tu_1"),
            tool_result("tu_1"),
            text(Role::Assistant, "done"),
        ];
        // Hard split point 2 would cut the tool_use/tool_result pair.
        let split = safe_split(&messages, 2);
        assert_eq!(split, 3, "advances to just after tool_result");
        assert!(
            !messages[split]
                .content
                .iter()
                .any(|b| matches!(b, ContentBlock::ToolResult { .. })),
            "kept side must not start with a tool_result"
        );
    }

    /// Consecutive tool_results must all be advanced past too.
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

    /// All tool_results: advance to the end; maybe_compact then fully compacts because
    /// split == len, but never out of bounds.
    #[test]
    fn split_never_exceeds_len() {
        let messages = vec![tool_result("a"), tool_result("b")];
        assert_eq!(safe_split(&messages, 0), messages.len());
    }

    /// Local estimate when count_tokens is unavailable: grows monotonically with
    /// content, never stuck at 0.
    #[test]
    fn local_estimate_grows_with_content() {
        let system = vec![SystemBlock {
            text: "s".repeat(400),
            cache: false,
        }];
        let empty = estimate_tokens(&system, &[], &[]);
        assert_eq!(empty, 100, "400 chars ≈ 100 tokens");

        let messages = vec![text(Role::User, &"x".repeat(4_000))];
        let with_message = estimate_tokens(&system, &messages, &[]);
        assert!(with_message > empty);
        assert_eq!(with_message, 1_100);

        // tool_use / tool_result count too, otherwise tool-turn growth is invisible.
        let with_tools = estimate_tokens(&system, &[tool_use("a"), tool_result("a")], &[]);
        assert!(with_tools > empty);
    }

    /// CJK text is ~1 token/char, not 4 chars/token: a flat chars/4 read a Chinese
    /// conversation at a quarter of its real size and compaction fired far too late.
    #[test]
    fn local_estimate_weighs_cjk_as_one_token_per_char() {
        let messages = vec![text(Role::User, &"上下文压缩".repeat(80))];
        assert_eq!(
            estimate_tokens(&[], &messages, &[]),
            400,
            "400 CJK chars ≈ 400 tokens"
        );
    }

    /// Tool schemas ride along with every request; a measurement that skips them
    /// (10k+ tokens for the base pool) eats the headroom between the compact
    /// threshold and the hard context limit.
    #[test]
    fn local_estimate_includes_tool_schemas() {
        let tools = vec![serde_json::json!({
            "name": "Bash",
            "description": "d".repeat(398),
        })];
        let without = estimate_tokens(&[], &[], &[]);
        let with = estimate_tokens(&[], &[], &tools);
        assert_eq!(without, 0);
        assert!(
            with >= 100,
            "schema JSON counts as input tokens, got {with}"
        );
    }

    /// Throttling: first turn always measures; then by interval or estimate growth,
    /// other turns extrapolate.
    #[test]
    fn token_gate_throttles_exact_counts() {
        let mut gate = TokenGate::new();
        assert!(
            gate.wants_exact(1_000),
            "the first turn of a round always measures"
        );
        gate.record_exact(5_000, 1_000);

        assert!(
            !gate.wants_exact(1_100),
            "small growth does not trigger a measure"
        );
        assert_eq!(
            gate.project(1_100),
            5_100,
            "extrapolated from the estimated delta"
        );
        assert_eq!(gate.current(1_200), 5_200);

        assert!(
            gate.wants_exact(1_000 + COUNT_TOKENS_GROWTH),
            "estimate crossing the threshold triggers an early measure"
        );

        let mut gate = TokenGate::new();
        gate.record_exact(5_000, 1_000);
        for _ in 0..COUNT_TOKENS_INTERVAL {
            gate.project(1_000);
        }
        assert!(gate.wants_exact(1_000), "always measures after N rounds");
    }
}
