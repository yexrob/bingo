//! The compaction strategy: what the summary says, and what to answer when
//! there is no summary to be had.

use async_trait::async_trait;
use bingo_sdk::compactor::BREAKER_TRIP;
use bingo_sdk::{
    CompactContext, CompactError, CompactReason, Compaction, Compactor, ErrorCode, Item, ItemId,
    KernelError, Usage,
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
    ) -> Result<Compaction, CompactError> {
        let cut = Cut::of(cx.items, cx.keep_budget)?;
        if spent(&reason, cx.failures) {
            return Ok(cut.dropped(Usage::default()));
        }
        let Some(mut request) = prompt::request(cx.request, cx.capabilities, instructions(&reason))
        else {
            if matches!(reason, CompactReason::Overflow { .. }) && !cx.cancel.is_cancelled() {
                return Ok(cut.dropped(Usage::default()));
            }
            return Err(KernelError::new(
                ErrorCode::ContextOverflow,
                "no output headroom for a shared-prefix summary",
            )
            .into());
        };
        let mut paid = Usage::default();
        let mut attempts = 0;
        let answer = loop {
            attempts += 1;
            match stream::summary(cx.provider.as_ref(), request.clone(), cx.cancel.clone()).await {
                Ok(mut answer) => {
                    answer.usage.add(paid);
                    break answer;
                }
                Err(mut failed) => {
                    paid.add(failed.usage);
                    if failed.error.code == ErrorCode::ContextOverflow
                        && attempts < 2
                        && !cx.cancel.is_cancelled()
                        && prompt::shrink(&mut request)
                    {
                        continue;
                    }
                    if failed.error.code == ErrorCode::ContextOverflow
                        && matches!(reason, CompactReason::Overflow { .. })
                        && !cx.cancel.is_cancelled()
                    {
                        return Ok(cut.dropped(paid));
                    }
                    failed.usage = paid;
                    return Err(failed);
                }
            }
        };
        let summary = answer.text.trim();
        if summary.is_empty() {
            // A model that answered nothing was still paid for the attempt.
            return Ok(cut.dropped(answer.usage));
        }
        let summary = if request.messages.len() <= cx.request.messages.len() {
            format!("{}\n\n{summary}", prompt::OMITTED)
        } else {
            summary.to_string()
        };
        Ok(cut.summarised(summary, answer.usage))
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
struct Cut {
    boundary: ItemId,
    before: u64,
}

impl Cut {
    fn of(items: &[Item], keep_budget: u64) -> Result<Self, KernelError> {
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

    struct Retry {
        requests: std::sync::Mutex<Vec<bingo_sdk::ModelRequest>>,
        fail_second: bool,
    }

    #[async_trait]
    impl Provider for Retry {
        fn id(&self) -> &str {
            "retry"
        }
        fn endpoint(&self, _: &str) -> bingo_sdk::EndpointCapabilities {
            Default::default()
        }
        async fn stream(
            &self,
            request: bingo_sdk::ModelRequest,
            _: CancellationToken,
        ) -> Result<bingo_sdk::ModelStream, ProviderError> {
            let first = {
                let mut requests = self.requests.lock().unwrap();
                requests.push(request);
                requests.len() == 1
            };
            let mut events = vec![
                Ok(bingo_sdk::ModelEvent::TextDelta {
                    id: "t".into(),
                    delta: "surviving work".into(),
                }),
                Ok(bingo_sdk::ModelEvent::Finish {
                    usage: Scripted::USAGE,
                    finish_reason: bingo_sdk::FinishReason::unified(bingo_sdk::UnifiedFinish::Stop),
                }),
            ];
            if first || self.fail_second {
                events.push(Err(ProviderError::ContextOverflow {
                    message: "too long".into(),
                }));
            }
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

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
            holds_context: false,
        }
    }

    async fn run(
        items: &[Item],
        provider: Arc<Scripted>,
        reason: CompactReason,
        failures: u32,
    ) -> Result<Compaction, CompactError> {
        run_with_window(items, provider, reason, failures, WINDOW).await
    }

    async fn run_with_window(
        items: &[Item],
        provider: Arc<dyn Provider>,
        reason: CompactReason,
        failures: u32,
        window: u64,
    ) -> Result<Compaction, CompactError> {
        let capabilities = ModelCapabilities {
            context_window: window,
            ..capabilities()
        };
        let request = bingo_sdk::ModelRequest {
            model: "model-x".into(),
            max_tokens: 8_000,
            system: vec![],
            messages: vec![
                bingo_sdk::Message::text(bingo_sdk::Role::User, "old"),
                bingo_sdk::Message::text(bingo_sdk::Role::Assistant, "answer"),
                bingo_sdk::Message::text(bingo_sdk::Role::User, "new"),
            ],
            tools: vec![],
            reasoning: None,
            session: None,
            provider_options: Default::default(),
        };
        let cx = CompactContext {
            items,
            usage: ContextUsage {
                used: 90_000,
                window: WINDOW,
                trigger: 90_000,
            },
            capabilities: &capabilities,
            provider: provider as Arc<dyn Provider>,
            request: &request,
            cancel: CancellationToken::new(),
            failures,
            keep_budget: 25_000,
        };
        SummaryCompactor.compact(cx, reason).await
    }

    #[tokio::test]
    async fn fresh_overflow_without_headroom_drops_without_asking_but_other_reasons_fail() {
        let provider = Arc::new(Scripted::saying("never asked"));
        let items = journal(30);
        for reason in [
            CompactReason::Threshold,
            CompactReason::Manual { instructions: None },
        ] {
            let error = run_with_window(&items, provider.clone(), reason, 0, 1)
                .await
                .expect_err("no silent cut");
            assert_eq!(error.error.code, ErrorCode::ContextOverflow);
        }
        let cut = run_with_window(
            &items,
            provider.clone(),
            CompactReason::Overflow {
                message: "too long".into(),
            },
            0,
            1,
        )
        .await
        .expect("no-model fallback");
        assert_eq!(cut.summary, DROPPED);
        assert_eq!(cut.usage, Usage::default());
        assert!(provider.requests().is_empty());
    }

    #[tokio::test]
    async fn fresh_overflow_after_two_summary_overflows_drops_without_a_third_call() {
        let provider = Arc::new(Retry {
            requests: Default::default(),
            fail_second: true,
        });
        let cut = run_with_window(
            &journal(30),
            provider.clone(),
            CompactReason::Overflow {
                message: "too long".into(),
            },
            0,
            WINDOW,
        )
        .await
        .expect("no-model fallback");
        assert_eq!(cut.summary, DROPPED);
        assert_eq!(cut.usage.output_tokens, 2 * Scripted::USAGE.output_tokens);
        assert_eq!(provider.requests.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_shortened_retry_discloses_omission_in_its_instruction_and_summary() {
        let provider = Arc::new(Retry {
            requests: Default::default(),
            fail_second: false,
        });
        let cut = run_with_window(
            &journal(30),
            provider.clone(),
            CompactReason::Threshold,
            0,
            WINDOW,
        )
        .await
        .expect("retry summary");
        assert_eq!(
            cut.summary,
            format!("{}\n\nsurviving work", prompt::OMITTED)
        );
        assert_eq!(cut.usage.output_tokens, 2 * Scripted::USAGE.output_tokens);
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].messages.len() < requests[0].messages.len());
        assert_eq!(
            requests[1]
                .messages
                .last()
                .unwrap()
                .parts
                .last()
                .unwrap()
                .as_text(),
            Some(prompt::OMITTED)
        );
        assert!(
            !requests[0]
                .messages
                .last()
                .unwrap()
                .parts
                .iter()
                .any(|part| part.as_text() == Some(prompt::OMITTED))
        );
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
        assert!(
            request.messages.last().unwrap().parts[0]
                .as_text()
                .unwrap()
                .ends_with("keep every file path")
        );
    }

    #[tokio::test]
    async fn repeated_overflow_is_bounded_and_only_the_retry_loses_history() {
        let provider = Arc::new(Scripted::failing(ProviderError::ContextOverflow {
            message: "too long".into(),
        }));
        let error = run(&journal(30), provider.clone(), CompactReason::Threshold, 0)
            .await
            .expect_err("bounded overflow");
        assert_eq!(error.error.code, ErrorCode::ContextOverflow);
        let requests = provider.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].messages.len(), 4);
        assert_eq!(requests[1].messages.len(), 2);
        assert_eq!(requests[0].system, requests[1].system);
        assert_eq!(requests[0].tools, requests[1].tools);
        assert_eq!(requests[0].provider_options, requests[1].provider_options);
    }

    #[tokio::test]
    async fn a_provider_error_comes_back_with_its_own_code() {
        let provider = Arc::new(Scripted::failing(ProviderError::Auth {
            message: "no key".into(),
        }));
        let error = run(&journal(30), provider, CompactReason::Threshold, 0)
            .await
            .expect_err("refused");
        assert_eq!(error.error.code, ErrorCode::AuthRequired);
        assert!(error.error.message.contains("no key"), "{error}");
    }

    #[tokio::test]
    async fn a_journal_with_nothing_old_enough_is_an_invalid_request() {
        let provider = Arc::new(Scripted::saying("a summary"));
        let error = run(&journal(5), provider, CompactReason::Threshold, 0)
            .await
            .expect_err("nothing to compact");
        assert_eq!(error.error.code, ErrorCode::InvalidInput);
        assert_eq!(error.error.message, "nothing to compact");
    }
}
