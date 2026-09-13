//! One request, drained: a compaction asks the model a question about a
//! conversation rather than continuing one, so it wants the whole answer.

use bingo_sdk::{CancellationToken, ModelEvent, ModelRequest, Provider, Usage};
use futures::StreamExt;

/// Everything the model said, and what saying it cost.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Answer {
    pub text: String,
    pub usage: Usage,
}

/// A compaction never executes tools and accepts only a complete stop.
/// Keep draining rejected output so a later finish can still report its cost.
pub async fn summary(
    provider: &dyn Provider,
    request: ModelRequest,
    cancel: CancellationToken,
) -> Result<Answer, bingo_sdk::CompactError> {
    use bingo_sdk::{CompactError, ErrorCode, KernelError, UnifiedFinish};
    let failed = |message: &str, usage| CompactError {
        error: KernelError::new(ErrorCode::InvalidInput, message),
        usage,
    };
    let mut answer = Answer::default();
    let mut stream = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(failed("compaction cancelled", answer.usage)),
        result = provider.stream(request, cancel.clone()) => result.map_err(|e| CompactError {
            error: KernelError::new(e.code(), e.to_string()), usage: answer.usage,
        })?,
    };
    let mut stop = false;
    let mut tools = false;
    loop {
        let next = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(failed("compaction cancelled", answer.usage)),
            next = stream.next() => next,
        };
        let Some(event) = next else {
            break;
        };
        match event.map_err(|e| CompactError {
            error: KernelError::new(e.code(), e.to_string()),
            usage: answer.usage,
        })? {
            ModelEvent::TextDelta { delta, .. } => answer.text.push_str(&delta),
            ModelEvent::Finish {
                usage,
                finish_reason,
            } => {
                answer.usage = usage;
                stop = finish_reason.unified == UnifiedFinish::Stop;
            }
            ModelEvent::ToolInputStart { .. }
            | ModelEvent::ToolInputDelta { .. }
            | ModelEvent::ToolInputEnd { .. }
            | ModelEvent::ToolCall { .. } => tools = true,
            _ => {}
        }
    }
    if cancel.is_cancelled() || !stop || tools {
        return Err(failed(
            "compaction requires a complete stop without tool calls",
            answer.usage,
        ));
    }
    Ok(answer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scripted::Scripted;
    use bingo_sdk::ProviderError;

    struct Events(Vec<Result<ModelEvent, ProviderError>>);

    #[async_trait::async_trait]
    impl Provider for Events {
        fn id(&self) -> &str {
            "events"
        }
        fn endpoint(&self, _: &str) -> bingo_sdk::EndpointCapabilities {
            Default::default()
        }
        async fn stream(
            &self,
            _: ModelRequest,
            _: CancellationToken,
        ) -> Result<bingo_sdk::ModelStream, ProviderError> {
            Ok(Box::pin(futures::stream::iter(self.0.clone())))
        }
    }

    struct CancelAfterFinish;

    #[async_trait::async_trait]
    impl Provider for CancelAfterFinish {
        fn id(&self) -> &str {
            "cancel-after-finish"
        }
        fn endpoint(&self, _: &str) -> bingo_sdk::EndpointCapabilities {
            Default::default()
        }
        async fn stream(
            &self,
            _: ModelRequest,
            cancel: CancellationToken,
        ) -> Result<bingo_sdk::ModelStream, ProviderError> {
            Ok(Box::pin(futures::stream::unfold(false, move |finished| {
                let cancel = cancel.clone();
                async move {
                    if finished {
                        cancel.cancel();
                        None
                    } else {
                        Some((finish(bingo_sdk::UnifiedFinish::Stop), true))
                    }
                }
            })))
        }
    }

    #[tokio::test]
    async fn cancellation_after_observed_usage_rejects_the_summary_but_keeps_its_cost() {
        let error = summary(&CancelAfterFinish, request(), CancellationToken::new())
            .await
            .expect_err("cancelled");
        assert_eq!(error.usage, Scripted::USAGE);
    }

    fn finish(reason: bingo_sdk::UnifiedFinish) -> Result<ModelEvent, ProviderError> {
        Ok(ModelEvent::Finish {
            usage: Scripted::USAGE,
            finish_reason: bingo_sdk::FinishReason::unified(reason),
        })
    }

    #[tokio::test]
    async fn summary_rejects_tools_and_length_but_retains_observed_usage() {
        use bingo_sdk::UnifiedFinish;
        let cases = [
            vec![finish(UnifiedFinish::Length)],
            vec![
                Ok(ModelEvent::ToolCall {
                    id: "c".into(),
                    name: "Write".into(),
                    input: "{}".into(),
                }),
                finish(UnifiedFinish::Stop),
            ],
            vec![
                finish(UnifiedFinish::Stop),
                Err(ProviderError::Stream {
                    message: "lost".into(),
                }),
            ],
        ];
        for events in cases {
            let error = summary(&Events(events), request(), CancellationToken::new())
                .await
                .expect_err("invalid summary");
            assert_eq!(error.usage, Scripted::USAGE);
        }
    }

    #[tokio::test]
    async fn summary_rejects_a_missing_finish_and_pre_cancelled_request() {
        let error = summary(&Events(vec![]), request(), CancellationToken::new())
            .await
            .expect_err("no finish");
        assert_eq!(error.usage, Usage::default());
        let cancel = CancellationToken::new();
        cancel.cancel();
        let provider = Scripted::saying("must not be asked");
        let error = summary(&provider, request(), cancel)
            .await
            .expect_err("cancelled");
        assert_eq!(error.usage, Usage::default());
        assert!(provider.requests().is_empty());
    }

    fn request() -> ModelRequest {
        ModelRequest {
            model: "model-x".into(),
            max_tokens: 16,
            system: Vec::new(),
            messages: Vec::new(),
            tools: Vec::new(),
            reasoning: None,
            session: None,
            provider_options: Default::default(),
        }
    }
}
