use std::sync::atomic::{AtomicBool, Ordering};

use crate::api::contract::{NeutralRequest, SystemBlock};
use crate::api::types::{ContentBlock, Message};
use crate::budget::{MAX_COMPACT_FAILURES, autocompact_threshold_for, warning_threshold_for};
use crate::hooks::{run_post_compact, run_pre_compact};
use crate::permission::PermissionMode;
use crate::query::Session;

/// Number of most recent messages kept after compaction. Tool turns spend
/// messages fast (one tool_use + one tool_result each), so 8 could mean four
/// tool calls and nothing of the exchange that motivated them.
pub(crate) const KEEP_RECENT: usize = 12;

/// Output budget for the summary request, capped by what the model can produce.
/// The old 1024 was the real ceiling on summary quality: no instruction can put
/// a long session's decisions, paths and pending work into that.
const SUMMARY_MAX_TOKENS: u32 = 4_096;

/// Longest tool input echoed into the summary prompt. Enough to identify the
/// command or path that was acted on; short enough that one file write does not
/// become the bulk of the prompt.
const TOOL_INPUT_CHARS: usize = 200;

/// Slack left over the summary prompt's own budget: the estimate is an
/// estimate, and being a little under costs nothing next to a summary request
/// that overflows on the way out of an overflow.
const SUMMARY_PROMPT_RESERVE: u64 = 2_000;

/// count_tokens measurement interval (turns): measuring every turn = one extra round
/// trip each; 20 tool turns = 20 round trips.
const COUNT_TOKENS_INTERVAL: u32 = 5;

/// Measure early when the local estimate has grown this much over the last exact count.
const COUNT_TOKENS_GROWTH: u64 = 20_000;

/// Warn only once when count_tokens is unavailable (no point spamming every turn on
/// non-Anthropic endpoints).
static COUNT_TOKENS_WARNED: AtomicBool = AtomicBool::new(false);

const COMPACT_PROMPT: &str = "\
You are compacting an agent conversation. Your summary replaces the transcript below, and it is \
all the agent will have of that work — anything you leave out is lost. Write it under these \
headings, skipping any heading with nothing to report:

## Task and current state
What the user asked for, and exactly where the work stands now.

## Decisions and rationale
Choices that were made and why, including approaches that were tried and rejected.

## Files, commands and results
Files read or changed, with their paths. Commands that were executed and what they returned.

## Outstanding work
What is not done yet, in the order it should be tackled.

## Constraints and preferences
Rules, conventions and user preferences that still apply.

Reproduce identifiers, paths, commands and error text exactly; never invent anything the \
transcript does not contain. Let the length follow the content — usually several hundred to a \
thousand words.

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

/// Flatten to one line and cap it: a tool input is a label in the prompt, not
/// a payload.
fn one_line(text: &str, limit: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if flat.chars().count() <= limit {
        return flat;
    }
    flat.chars().take(limit).chain(['…']).collect()
}

/// One transcript message as the summary prompt sees it. tool_use blocks carry
/// the commands that ran, which the prompt asks the model to keep — dropping
/// them left it summarizing results whose cause was no longer in front of it.
fn message_section(message: &Message) -> String {
    let text = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.clone()),
            ContentBlock::ToolUse { name, input, .. } => Some(format!(
                "[tool {name}] {}",
                one_line(&input.to_string(), TOOL_INPUT_CHARS)
            )),
            ContentBlock::ToolResult { content, .. } => {
                Some(crate::api::types::tool_result_text(content))
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("\n---\n{text}")
}

fn omitted_note(count: usize) -> String {
    format!(
        "\n({count} earlier messages are left out of this transcript because they did not fit \
         the summary request.)\n"
    )
}

/// Old messages → summary prompt, trimmed to `budget` tokens.
///
/// The overflow path hands this whole histories that the model just refused,
/// and dropping system blocks and tools is not by itself enough headroom to be
/// sure it fits. Trimming is local and deterministic — drop whole messages
/// from the oldest end, then cut the head off what is left — so recovery never
/// costs an extra request to find out it failed. The oldest end goes first
/// because it is what a summary loses least by losing.
fn summary_prompt(old: &[Message], budget: u64) -> String {
    let allowance = budget.saturating_mul(4);
    let sections: Vec<String> = old.iter().map(message_section).collect();
    // The note is charged from the start so dropping cannot undershoot and
    // need a second pass; over-reserving by one absent line is free.
    let overhead = text_units(COMPACT_PROMPT) + text_units(&omitted_note(old.len()));
    let mut used: u64 = sections.iter().map(|section| text_units(section)).sum();
    let mut dropped = 0;
    while used + overhead > allowance && dropped < sections.len() {
        used -= text_units(&sections[dropped]);
        dropped += 1;
    }

    let mut prompt = String::from(COMPACT_PROMPT);
    if let Some(last) = sections.last().filter(|_| dropped == sections.len()) {
        // Even the newest message alone is over budget: it keeps its tail,
        // which is where a turn's conclusion lives.
        if dropped > 1 {
            prompt.push_str(&omitted_note(dropped - 1));
        }
        // Each char costs at most 4 units, so this many always fit.
        let room = allowance.saturating_sub(overhead) as usize / 4;
        prompt.push('…');
        prompt.extend(last.chars().skip(last.chars().count().saturating_sub(room)));
        return prompt;
    }
    if dropped > 0 {
        prompt.push_str(&omitted_note(dropped));
    }
    prompt.push_str(&sections[dropped..].concat());
    prompt
}

/// Where compaction reports itself. The three surfaces already differ on how a
/// notice is shown (TUI warning row, headless stderr, JSON `warning` event) and
/// `UiHooks::on_warning` is the one channel all three implement, so compaction
/// borrows it rather than writing stderr underneath a TUI that owns the screen.
/// The manual `/compact` path reports through its own slash events and passes a
/// no-op.
pub type CompactNotify<'a> = &'a mut (dyn Fn(String) + Send + 'a);

/// Auto-compact when context exceeds the threshold: old messages → model summary,
/// keep the most recent KEEP_RECENT.
/// Success resets the circuit breaker; failure increments it (after
/// MAX_COMPACT_FAILURES consecutive failures the caller skips).
/// messages aren't touched until the summary arrives — on failure history is kept
/// verbatim, never replaced by a placeholder string.
/// Returns whether compaction happened.
pub async fn maybe_compact(
    session: &Session,
    messages: &mut Vec<Message>,
    tokens: u64,
    notify: CompactNotify<'_>,
) -> bool {
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
    compact(session, messages, notify).await
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
    notify: CompactNotify<'_>,
) -> bool {
    if session.compact_failures.load(Ordering::SeqCst) >= MAX_COMPACT_FAILURES {
        notify(format!(
            "overflow compaction disabled after {MAX_COMPACT_FAILURES} consecutive failures"
        ));
        return false;
    }
    if messages.len() <= KEEP_RECENT {
        session.compact_failures.fetch_add(1, Ordering::SeqCst);
        return false;
    }
    let compacted = compact(session, messages, notify).await;
    if compacted {
        gate.reset();
    }
    compacted
}

async fn compact(
    session: &Session,
    messages: &mut Vec<Message>,
    notify: CompactNotify<'_>,
) -> bool {
    let split = safe_split(messages, messages.len() - KEEP_RECENT);

    run_pre_compact(
        &session.settings.hooks,
        permission_mode_str(session.permission_mode),
        &session.cwd(),
    )
    .await;

    let model = session.runtime.model.borrow().clone();
    let models = session.client.models();
    let max_tokens = SUMMARY_MAX_TOKENS.min(crate::budget::max_tokens_for(&models, &model));
    let prompt_budget = crate::budget::effective_window_for(&models, &model)
        .saturating_sub(max_tokens as u64)
        .saturating_sub(SUMMARY_PROMPT_RESERVE);
    let request = NeutralRequest {
        model,
        max_tokens,
        system: Vec::new(),
        messages: vec![Message::user_text(summary_prompt(
            &messages[..split],
            prompt_budget,
        ))],
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
            notify(match outcome {
                Err(e) => format!("context compaction failed, history kept as-is: {e}"),
                _ => "context compaction returned an empty summary, history kept as-is".to_string(),
            });
            return false;
        }
    };

    // Persist the marker before splicing (the `record` discipline): canonical
    // lines stay untouched, the next load projects instead of re-summarizing.
    // A failed write degrades to today's behavior — the next load re-compacts.
    if let Some(transcript) = session.runtime.transcript.borrow().clone()
        && let Err(e) = transcript.append_compact(&summary, messages.len() - split)
    {
        notify(format!(
            "compact marker write failed (the next load will re-compact): {e}"
        ));
    }
    messages.splice(..split, [crate::transcript::summary_message(&summary)]);

    run_post_compact(
        &session.settings.hooks,
        permission_mode_str(session.permission_mode),
        &session.cwd(),
    )
    .await;
    notify(format!(
        "context compacted: {split} earlier messages replaced by a summary"
    ));
    true
}

/// Quarter-token units for one text fragment: ASCII ≈ 4 chars/token, other
/// scripts (CJK etc.) ≈ 1 token/char. A flat chars/4 undercounted Chinese
/// conversations ~4x, so growth between exact counts went mostly unseen and
/// auto-compact fired far too late (#40).
///
/// The 1 token/char CJK weight stays even though BPE tokenizers pack CJK
/// tighter than that: it is what Anthropic's tokenizer actually does, and the
/// two error directions cost differently. Overestimating compacts early —
/// annoying. Underestimating overflows the window — a failed turn plus a
/// recovery round trip. The estimate only decides anything where `count_tokens`
/// is unavailable, so the conservative side is the right one, and after
/// per-model max_tokens (F2) freed the headroom the flat 64k reservation ate,
/// early compaction on BPE endpoints is a smaller price than it was.
pub(crate) fn text_units(text: &str) -> u64 {
    text.chars().map(|c| if c.is_ascii() { 1 } else { 4 }).sum()
}

/// One image, in units (÷4 = 1600 tokens). Anthropic bills an image at
/// roughly `width * height / 750`, capped near 1600 tokens for anything at or
/// above the resize ceiling — so the largest image costs about this much and a
/// small one costs less. Counting base64 length instead read a 1MB attachment
/// as ~350k tokens, which pinned every image conversation over the threshold
/// forever: the image lives inside KEEP_RECENT, so compaction could never
/// bring the number down and each turn burned one useless summary request.
const IMAGE_UNITS: u64 = 6_400;

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
                ContentBlock::Image { .. } => IMAGE_UNITS,
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
    notify: CompactNotify<'_>,
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

    // Per-turn progress, and only for the stream-to-stderr host: the TUI reads
    // the same number off `UiHooks::on_context_usage` every turn, in the footer
    // that also colours it — a warning row per turn would say it twice.
    if tokens > 0 && !session.quiet {
        eprintln!("[bingo] context: {tokens} tokens");
    }
    let model = session.runtime.model.borrow().clone();
    let models = session.client.models();
    let threshold = autocompact_threshold_for(&models, &model);
    if tokens >= threshold {
        if session.compact_failures.load(Ordering::SeqCst) >= MAX_COMPACT_FAILURES {
            notify(format!(
                "auto-compact disabled after {MAX_COMPACT_FAILURES} consecutive failures"
            ));
        } else if maybe_compact(session, messages, tokens, notify).await {
            gate.reset();
            return estimate_tokens(&session.system, messages, tools);
        }
    } else if tokens >= warning_threshold_for(&models, &model) {
        // The one notice the user can still act on — compaction has not happened
        // yet. It went to stderr under `!quiet`, which is exactly the two hosts
        // that never see stderr, so the surface that owns the screen was the one
        // told nothing. Same channel as compaction's own report (D66).
        notify(format!(
            "context at {tokens} tokens; auto-compact at {threshold}"
        ));
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

    /// The prompt asks the model to keep executed commands, so the tool_use
    /// blocks that hold them have to be in it — long inputs one line each.
    #[test]
    fn summary_prompt_carries_executed_commands() {
        let call = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "tu_1".into(),
                name: "Bash".into(),
                input: serde_json::json!({ "command": format!("echo {}", "x".repeat(400)) }),
            }],
        };
        let prompt = summary_prompt(
            &[text(Role::User, "fix the build"), call, tool_result("tu_1")],
            100_000,
        );
        assert!(prompt.contains("fix the build"));
        assert!(prompt.contains("[tool Bash] {\"command\":\"echo xxx"));
        assert!(
            prompt.contains('…'),
            "a long input is truncated, not dropped"
        );
        assert!(
            prompt.lines().all(|line| line.chars().count() < 300),
            "the tool input stays one bounded line"
        );
    }

    /// The overflow path summarizes a history the model just refused, so the
    /// summary request must be trimmed to fit before it is sent — locally, with
    /// no extra round trip to discover that it did not.
    #[test]
    fn summary_prompt_is_trimmed_into_the_models_budget() {
        let models = crate::api::models::ModelResolver::new(
            std::sync::Arc::new(crate::api::models::ModelCatalog::from_settings(
                &serde_json::from_str(
                    r#"{"models": [{"id": "tiny-model", "contextWindow": 32768}]}"#,
                )
                .unwrap(),
            )),
            "default".to_string(),
        );
        let budget = crate::budget::effective_window_for(&models, "tiny-model")
            .saturating_sub(SUMMARY_MAX_TOKENS as u64)
            .saturating_sub(SUMMARY_PROMPT_RESERVE);
        let history: Vec<Message> = (0..40)
            .map(|index| {
                text(
                    Role::User,
                    &format!("message {index} {}", "x".repeat(4_000)),
                )
            })
            .collect();

        let prompt = summary_prompt(&history, budget);
        assert!(
            text_units(&prompt) / 4 <= budget,
            "prompt is {} tokens against a {budget} budget",
            text_units(&prompt) / 4
        );
        assert!(
            prompt.contains("are left out of this transcript"),
            "the summary must say what it could not see"
        );
        assert!(
            prompt.contains("message 39"),
            "trimming drops the oldest end, so the newest work survives"
        );
    }

    /// One message over budget on its own: keep its tail, where the turn's
    /// conclusion is, rather than dropping the transcript to nothing.
    #[test]
    fn summary_prompt_keeps_the_tail_of_an_oversized_message() {
        let history = vec![
            text(Role::User, &"old ".repeat(4_000)),
            text(Role::User, &format!("{}the conclusion", "y".repeat(40_000))),
        ];
        let prompt = summary_prompt(&history, 4_000);
        assert!(text_units(&prompt) / 4 <= 4_000);
        assert!(prompt.ends_with("the conclusion"));
        assert!(prompt.contains("1 earlier messages are left out"));
    }

    /// An image costs a bounded number of tokens, not one per base64 character:
    /// a 1MB attachment read as ~350k tokens pinned the session over the compact
    /// threshold permanently (the image sits inside KEEP_RECENT, so compaction
    /// cannot bring the number down), burning a summary request every turn.
    #[test]
    fn local_estimate_bounds_image_blocks() {
        let image = Message {
            role: Role::User,
            content: vec![ContentBlock::Image {
                source: crate::api::types::ImageSource::base64("image/png", "A".repeat(1_400_000)),
            }],
        };
        assert_eq!(estimate_tokens(&[], &[image], &[]), 1_600);
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

    /// A gate holding `tokens` as its last exact count, primed so the next turn
    /// extrapolates instead of asking the endpoint (no test ever goes online).
    fn gate_reading(tokens: u64, estimate: u64) -> TokenGate {
        let mut gate = TokenGate::new();
        gate.record_exact(tokens, estimate);
        assert!(!gate.wants_exact(estimate), "the next turn extrapolates");
        gate
    }

    /// The pre-compaction warning is the last point at which the user can still
    /// do something about the context, and it used to go to stderr under
    /// `!quiet` — which is precisely the two hosts that never see stderr. It now
    /// travels the same channel as compaction's own report, so it arrives as a
    /// `UiEvent::Warning` row.
    #[tokio::test]
    async fn the_pre_compaction_warning_reaches_the_tui() {
        let session = crate::tui::test_util::test_session();
        let model = session.runtime.model.borrow().clone();
        let models = session.client.models();
        let threshold = autocompact_threshold_for(&models, &model);
        let warning_at = warning_threshold_for(&models, &model);
        assert!(
            warning_at > 0 && warning_at < threshold,
            "the warning band has to be a real range: {warning_at}..{threshold}"
        );

        let mut messages = vec![text(Role::User, "hi"), text(Role::Assistant, "hello")];
        let estimate = estimate_tokens(&session.system, &messages, &[]);

        let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
        let (asks_tx, _asks_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut ui = crate::ui::tui_hooks(events_tx, asks_tx);

        let mut gate = gate_reading(warning_at.saturating_sub(1), estimate);
        let quiet =
            check_and_compact(&session, &mut messages, &mut gate, &[], &mut ui.on_warning).await;
        assert_eq!(quiet, warning_at - 1);
        assert!(
            events_rx.try_recv().is_err(),
            "one token below the band says nothing"
        );

        let mut gate = gate_reading(warning_at, estimate);
        let tokens =
            check_and_compact(&session, &mut messages, &mut gate, &[], &mut ui.on_warning).await;
        assert_eq!(tokens, warning_at);
        match events_rx.try_recv() {
            Ok(crate::ui::UiEvent::Warning(message)) => assert_eq!(
                message,
                format!("context at {warning_at} tokens; auto-compact at {threshold}")
            ),
            other => panic!("expected a warning row, got {other:?}"),
        }
        assert_eq!(
            messages.len(),
            2,
            "warning only — nothing is compacted below the threshold"
        );
    }
}
