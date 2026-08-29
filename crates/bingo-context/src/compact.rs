//! The compaction strategy: what the summary says, and what to answer when
//! there is no summary to be had.

use async_trait::async_trait;
use bingo_sdk::compactor::BREAKER_TRIP;
use bingo_sdk::{
    CompactContext, CompactReason, Compaction, Compactor, ErrorCode, Item, ItemId, KernelError,
    Usage,
};

use crate::{estimate, prompt, split, stream};

/// What the transcript says where a summary could not be bought. The rung is
/// still a cut: an honest gap shrinks the window, and a model that reads it
/// knows not to answer about what came before.
const DROPPED: &str = "[earlier conversation dropped]";

/// Summarises the old turns through the session's own model.
#[derive(Debug, Default, Clone, Copy)]
pub struct SummaryCompactor;

#[async_trait]
impl Compactor for SummaryCompactor {
    async fn compact(
        &self,
        cx: CompactContext<'_>,
        reason: CompactReason,
    ) -> Result<Compaction, KernelError> {
        let cut = Cut::of(cx.items, cx.keep_budget)?;
        if spent(&reason, cx.failures) {
            return Ok(cut.dropped(Usage::default()));
        }
        let request = prompt::request(cx.model, cx.usage.window, instructions(&reason), cut.old);
        let answer = stream::drain(cx.provider.as_ref(), request, cx.cancel.clone())
            .await
            .map_err(|e| KernelError::new(e.code(), e.to_string()))?;
        let summary = answer.text.trim();
        if summary.is_empty() {
            // A model that answered nothing was still paid for the attempt.
            return Ok(cut.dropped(answer.usage));
        }
        Ok(cut.summarised(summary.to_string(), answer.usage))
    }
}

/// Asking again is throwing money after the last three: an overflow still has
/// to shrink, so it takes the rung that needs no model.
fn spent(reason: &CompactReason, failures: u32) -> bool {
    matches!(reason, CompactReason::Overflow { .. }) && failures >= BREAKER_TRIP
}

fn instructions(reason: &CompactReason) -> Option<&str> {
    match reason {
        CompactReason::Manual { instructions } => instructions.as_deref(),
        _ => None,
    }
}

/// A cut of this journal before anything is written: where the boundary falls,
/// the items a summary would replace, and what they cost now.
struct Cut<'a> {
    boundary: ItemId,
    old: &'a [Item],
    before: u64,
}

impl<'a> Cut<'a> {
    fn of(items: &'a [Item], keep_budget: u64) -> Result<Self, KernelError> {
        let at = split::split(items, keep_budget);
        // One item summarised into one summary is not a cut, and the boundary
        // has to name an item the kernel can still find.
        if at < 2 || at >= items.len() {
            return Err(KernelError::new(
                ErrorCode::InvalidInput,
                "nothing to compact",
            ));
        }
        let old = &items[..at];
        Ok(Self {
            boundary: items[at].id.clone(),
            old,
            before: estimate::items(old),
        })
    }

    fn dropped(&self, usage: Usage) -> Compaction {
        self.summarised(DROPPED.to_string(), usage)
    }

    /// `kept` stays empty: everything from the boundary on is the kernel's to
    /// keep, and nothing older is worth carrying past its own summary.
    fn summarised(&self, summary: String, usage: Usage) -> Compaction {
        Compaction {
            after: estimate::text(&summary),
            summary,
            boundary: self.boundary.clone(),
            kept: Vec::new(),
            before: self.before,
            usage,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{assistant, user};
    use crate::scripted::Scripted;
    use bingo_sdk::{CancellationToken, ContextUsage, ModelCapabilities, Provider, ProviderError};
    use std::sync::Arc;

    const WINDOW: u64 = 100_000;

    fn journal(n: usize) -> Vec<Item> {
        (0..n)
            .map(|i| {
                if i % 2 == 0 {
                    user(&format!("u{i}"), "what does the ruler do")
                } else {
                    assistant(&format!("a{i}"), "it measures the window")
                }
            })
            .collect()
    }

    fn capabilities() -> ModelCapabilities {
        ModelCapabilities {
            context_window: WINDOW,
            max_output: 8_000,
            images: false,
            reasoning: false,
            count_tokens: false,
            caching: false,
        }
    }

    async fn run(
        items: &[Item],
        provider: Arc<Scripted>,
        reason: CompactReason,
        failures: u32,
    ) -> Result<Compaction, KernelError> {
        let capabilities = capabilities();
        let cx = CompactContext {
            items,
            usage: ContextUsage {
                used: 90_000,
                window: WINDOW,
                trigger: 90_000,
            },
            capabilities: &capabilities,
            provider: provider as Arc<dyn Provider>,
            model: "model-x",
            cancel: CancellationToken::new(),
            failures,
            keep_budget: 25_000,
        };
        SummaryCompactor.compact(cx, reason).await
    }

    #[tokio::test]
    async fn a_summary_replaces_the_old_turns_and_bills_what_it_cost() {
        let items = journal(30);
        let provider = Arc::new(Scripted::saying("## Task and current state\nthe ruler"));
        let compaction = run(&items, provider.clone(), CompactReason::Threshold, 0)
            .await
            .expect("a summary");
        assert_eq!(compaction.boundary, items[18].id);
        assert!(compaction.kept.is_empty());
        assert!(
            compaction.after < compaction.before,
            "{} → {}",
            compaction.before,
            compaction.after
        );
        assert_eq!(compaction.usage, Scripted::USAGE);
        assert_eq!(provider.requests().len(), 1);
    }

    #[tokio::test]
    async fn an_overflow_under_a_tripped_breaker_asks_no_model() {
        let items = journal(30);
        let provider = Arc::new(Scripted::saying("never asked"));
        let reason = CompactReason::Overflow {
            message: "too long".into(),
        };
        let compaction = run(&items, provider.clone(), reason, 3)
            .await
            .expect("a cut");
        assert_eq!(compaction.summary, DROPPED);
        assert_eq!(compaction.boundary, items[18].id);
        assert_eq!(compaction.usage, Usage::default());
        assert!(provider.requests().is_empty(), "no request was paid for");
    }

    #[tokio::test]
    async fn an_overflow_under_two_failures_still_asks() {
        let provider = Arc::new(Scripted::saying("a summary"));
        let reason = CompactReason::Overflow {
            message: "too long".into(),
        };
        let compaction = run(&journal(30), provider.clone(), reason, 2)
            .await
            .expect("a summary");
        assert_eq!(compaction.summary, "a summary");
        assert_eq!(provider.requests().len(), 1);
    }

    #[tokio::test]
    async fn an_empty_summary_falls_to_the_rung_that_needs_no_model() {
        let provider = Arc::new(Scripted::saying("   \n  "));
        let compaction = run(&journal(30), provider, CompactReason::Threshold, 0)
            .await
            .expect("a cut");
        assert_eq!(compaction.summary, DROPPED);
        assert_eq!(
            compaction.usage,
            Scripted::USAGE,
            "the attempt was still billed"
        );
    }

    #[tokio::test]
    async fn manual_instructions_reach_the_system_prompt() {
        let provider = Arc::new(Scripted::saying("a summary"));
        let reason = CompactReason::Manual {
            instructions: Some("keep every file path".into()),
        };
        run(&journal(30), provider.clone(), reason, 0)
            .await
            .expect("a summary");
        let request = provider.requests().remove(0);
        assert!(request.system[0].text.ends_with("keep every file path"));
    }

    #[tokio::test]
    async fn a_provider_error_comes_back_with_its_own_code() {
        let provider = Arc::new(Scripted::failing(ProviderError::Auth {
            message: "no key".into(),
        }));
        let error = run(&journal(30), provider, CompactReason::Threshold, 0)
            .await
            .expect_err("refused");
        assert_eq!(error.code, ErrorCode::AuthRequired);
        assert!(error.message.contains("no key"), "{error}");
    }

    #[tokio::test]
    async fn a_journal_with_nothing_old_enough_is_an_invalid_request() {
        let provider = Arc::new(Scripted::saying("a summary"));
        let error = run(&journal(5), provider, CompactReason::Threshold, 0)
            .await
            .expect_err("nothing to compact");
        assert_eq!(error.code, ErrorCode::InvalidInput);
        assert_eq!(error.message, "nothing to compact");
    }
}
