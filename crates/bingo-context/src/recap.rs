//! The recap (M84): once a long turn is over, one side question asks the
//! model what the turn did and what is next, and the answer goes onto the
//! session's stream as live state for the TUI to draw under the worked row —
//! `※ recap: …`. Ephemeral by design: a recap is about the turn that just
//! ended, so it is a `Signal`, never journaled, and gone with the next turn.

use std::time::Duration;

use crate::{stream, tail, transcript};
use async_trait::async_trait;
use bingo_sdk::{
    CancellationToken, Hook, HookContext, HookMatcher, HookPoint, Item, Message, ModelRequest,
    Phase, ProviderMetadata, Role, SystemBlock, TurnId,
};

const ASK: &str = "\
You are writing a recap for the person who asked for the work below. In one or two \
sentences of plain text, say what was done and, if anything, what is next. No preamble, \
no headings, no lists; write to the person, not about them.";

/// The namespace and the kind the recap is published under. The surface
/// reads it by these names (`tui::recap`); the underscore keeps it out of
/// the generic cards (ADR-0013 §2) as the baselines are kept out.
pub const PLUGIN: &str = "_bingo.context";
pub const KIND: &str = "recap";

/// How long a turn must have run to earn a recap: a short turn's answer is
/// its own recap, and a request per turn would be a tax on every one.
pub const RECAP_AFTER: Duration = Duration::from_secs(120);

/// What a recap may say. Two sentences; a paragraph is an answer.
const MAX_TOKENS: u32 = 160;

/// How much of the turn the model reads, newest kept.
const MAX_CHARS: u64 = 60_000;

/// The turn is not held open for a recap: one bounded chance.
const DEADLINE: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy)]
pub struct RecapHook;

#[async_trait]
impl Hook for RecapHook {
    fn id(&self) -> &str {
        "context:recap"
    }

    fn matcher(&self) -> HookMatcher {
        HookMatcher {
            points: vec![HookPoint::Turn],
            tool: None,
        }
    }

    async fn on_turn(&self, phase: Phase, turn: &TurnId, items: &[Item], cx: &HookContext) {
        if phase != Phase::End {
            return;
        }
        let own = of_turn(turn, items);
        if span(&own) < RECAP_AFTER {
            return;
        }
        let Some(text) = ask(&own, cx).await else {
            return;
        };
        let payload = serde_json::json!({ "turn": turn, "text": text });
        if let Err(error) = cx.host.signal(&cx.session, PLUGIN, KIND, payload).await {
            tracing::warn!(%error, "recap: not published");
        }
    }
}

/// The items of this turn alone. The kernel hands the hook the model's whole
/// view — every turn so far — and a recap is about the one that just ended,
/// not the session: measured over all of them, every turn after the second
/// minute of a session would earn one.
fn of_turn(turn: &TurnId, items: &[Item]) -> Vec<Item> {
    items
        .iter()
        .filter(|item| item.turn.as_ref() == Some(turn))
        .cloned()
        .collect()
}

/// How long the turn ran, by its items' own clocks: from the first to start
/// to the last to finish. A turn with nothing in it ran for no time.
pub fn span(items: &[Item]) -> Duration {
    let started = items.iter().map(|item| item.started_at).min();
    let ended = items.iter().filter_map(|item| item.completed_at).max();
    match (started, ended) {
        (Some(started), Some(ended)) => ended.duration_since(started).unsigned_abs(),
        _ => Duration::ZERO,
    }
}

/// The recap the model wrote, or nothing: a session with no provider cannot
/// ask, and an answer that did not come in time is not waited for.
async fn ask(items: &[Item], cx: &HookContext) -> Option<String> {
    let (provider, model) = (cx.provider.as_ref()?, cx.model.as_ref()?);
    let asked = stream::drain(
        provider.as_ref(),
        request(model, items),
        CancellationToken::new(),
    );
    match tokio::time::timeout(DEADLINE, asked).await {
        Ok(Ok(answer)) => Some(answer.text.trim().to_string()).filter(|text| !text.is_empty()),
        Ok(Err(error)) => {
            tracing::warn!(%error, "recap: the model did not answer");
            None
        }
        Err(_) => {
            tracing::warn!("recap: the model ran out of time");
            None
        }
    }
}

fn request(model: &str, items: &[Item]) -> ModelRequest {
    ModelRequest {
        model: model.to_string(),
        max_tokens: MAX_TOKENS,
        system: vec![SystemBlock {
            text: ASK.to_string(),
            cache: false,
        }],
        messages: vec![Message::text(Role::User, body(items))],
        tools: Vec::new(),
        reasoning: None,
        // A side question, not the session's turn: it belongs to no
        // conversation a stateful provider is keeping.
        session: None,
        provider_options: side_question(KIND),
    }
}

/// A request that is a question about the conversation, not the
/// conversation: named as such, so a provider that answers from a script
/// can tell the two apart. A real provider ignores it.
fn side_question(purpose: &str) -> ProviderMetadata {
    let mut about = serde_json::Map::new();
    about.insert("purpose".into(), serde_json::Value::String(purpose.into()));
    ProviderMetadata::from([("bingo".to_string(), about)])
}

/// The turn as the model reads it, newest kept.
fn body(items: &[Item]) -> String {
    let lines = transcript::lines(items);
    let dropped = tail::first_within(&lines, MAX_CHARS, |l| l.chars().count() as u64 + 1);
    lines[dropped..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::baseline::testing::Journal;
    use crate::fixtures::{tool, user};
    use crate::scripted::Scripted;
    use bingo_sdk::{Provider, ProviderError};
    use jiff::{SignedDuration, Timestamp};
    use std::sync::Arc;

    /// A turn that ran this many seconds: its first item starts at the epoch
    /// and its last completes that much later.
    fn turn_of(seconds: i64) -> Vec<Item> {
        let mut first = user("u", "make the thing");
        first.turn = Some(TurnId::from_raw("trn_1"));
        first.started_at = Timestamp::UNIX_EPOCH;
        let mut last = tool("t", "Bash", r#"{"command":"cargo test"}"#, Some("ok"));
        last.turn = Some(TurnId::from_raw("trn_1"));
        last.started_at = Timestamp::UNIX_EPOCH + SignedDuration::from_secs(1);
        last.completed_at = Some(Timestamp::UNIX_EPOCH + SignedDuration::from_secs(seconds));
        vec![first, last]
    }

    /// What the hook is handed is the whole view: an earlier turn's long
    /// work in front of this turn's short one.
    fn after_a_long_earlier_turn(seconds: i64) -> Vec<Item> {
        let mut earlier = turn_of(600);
        for item in &mut earlier {
            item.turn = Some(TurnId::from_raw("trn_0"));
        }
        let mut this = turn_of(seconds);
        for item in &mut this {
            item.started_at += SignedDuration::from_secs(700);
            item.completed_at = item
                .completed_at
                .map(|at| at + SignedDuration::from_secs(700));
        }
        earlier.extend(this);
        earlier
    }

    fn context(journal: &Journal, provider: Option<Arc<dyn Provider>>) -> HookContext {
        HookContext {
            session: journal.state().summary.id.clone(),
            turn: Some(TurnId::from_raw("trn_1")),
            cwd: std::env::temp_dir(),
            provider,
            model: Some("model-x".to_string()),
            host: journal.handle(),
        }
    }

    async fn ended(journal: &Journal, items: &[Item], provider: Option<Arc<dyn Provider>>) {
        RecapHook
            .on_turn(
                Phase::End,
                &TurnId::from_raw("trn_1"),
                items,
                &context(journal, provider),
            )
            .await;
    }

    fn recap(journal: &Journal) -> Option<serde_json::Value> {
        journal.state().signals.get(PLUGIN)?.get(KIND).cloned()
    }

    #[test]
    fn it_listens_at_the_turn_only() {
        assert_eq!(RecapHook.id(), "context:recap");
        assert_eq!(RecapHook.matcher().points, [HookPoint::Turn]);
        assert!(RecapHook.matcher().tool.is_none());
    }

    #[test]
    fn a_turn_spans_its_items_own_clocks() {
        assert_eq!(span(&turn_of(150)), Duration::from_secs(150));
        assert_eq!(span(&[]), Duration::ZERO);
        assert_eq!(
            span(&[user("u", "hi")]),
            Duration::ZERO,
            "nothing completed"
        );
    }

    #[tokio::test]
    async fn a_long_turn_is_recapped_onto_the_stream() {
        let journal = Journal::at(&std::env::temp_dir());
        let provider = Arc::new(Scripted::saying("  Built the thing; tests are green.\n"));
        ended(&journal, &turn_of(150), Some(provider.clone())).await;
        let published = recap(&journal).expect("a recap");
        assert_eq!(published["turn"], "trn_1");
        assert_eq!(published["text"], "Built the thing; tests are green.");

        let request = provider.requests().remove(0);
        assert!(
            request.system[0]
                .text
                .starts_with("You are writing a recap")
        );
        assert_eq!(request.max_tokens, MAX_TOKENS);
        assert!(format!("{:?}", request.messages[0]).contains("cargo test"));
        assert_eq!(request.provider_options["bingo"]["purpose"], "recap");
    }

    /// The turn that ended is measured, not the session: the items in front
    /// of it belong to turns already recapped or too short to be.
    #[tokio::test]
    async fn only_the_turn_that_ended_is_measured_and_read() {
        let journal = Journal::at(std::env::temp_dir().as_path());
        let provider = Arc::new(Scripted::saying("Did the short thing."));
        ended(
            &journal,
            &after_a_long_earlier_turn(30),
            Some(provider.clone()),
        )
        .await;
        assert!(
            recap(&journal).is_none(),
            "a short turn after a long one asks nothing"
        );
        assert!(provider.requests().is_empty());

        let mut long = after_a_long_earlier_turn(150);
        let mut read = tool(
            "t0",
            "Bash",
            r#"{"command":"the earlier turn"}"#,
            Some("ok"),
        );
        read.turn = Some(TurnId::from_raw("trn_0"));
        long.insert(0, read);
        ended(&journal, &long, Some(provider.clone())).await;
        assert_eq!(
            recap(&journal).map(|r| r["text"].clone()),
            Some("Did the short thing.".into())
        );
        let asked = format!("{:?}", provider.requests()[0].messages[0]);
        assert!(!asked.contains("the earlier turn"), "{asked}");
    }

    #[tokio::test]
    async fn a_short_turn_asks_nothing() {
        let journal = Journal::at(&std::env::temp_dir());
        let provider = Arc::new(Scripted::saying("never asked"));
        ended(&journal, &turn_of(30), Some(provider.clone())).await;
        assert!(provider.requests().is_empty());
        assert_eq!(recap(&journal), None);
    }

    #[tokio::test]
    async fn the_start_of_a_turn_a_missing_provider_and_a_refusal_publish_nothing() {
        let journal = Journal::at(&std::env::temp_dir());
        let provider = Arc::new(Scripted::saying("a recap"));
        RecapHook
            .on_turn(
                Phase::Start,
                &TurnId::from_raw("trn_1"),
                &turn_of(150),
                &context(&journal, Some(provider.clone())),
            )
            .await;
        assert!(provider.requests().is_empty());

        ended(&journal, &turn_of(150), None).await;
        assert_eq!(recap(&journal), None);

        let refusing = Arc::new(Scripted::failing(ProviderError::Auth {
            message: "no key".into(),
        }));
        ended(&journal, &turn_of(150), Some(refusing)).await;
        assert_eq!(recap(&journal), None);

        let silent = Arc::new(Scripted::saying("   "));
        ended(&journal, &turn_of(150), Some(silent)).await;
        assert_eq!(recap(&journal), None);
    }
}
