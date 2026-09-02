//! One request, drained: this crate asks the model questions about a
//! conversation rather than continuing one, so it wants the whole answer.

use bingo_sdk::{CancellationToken, ModelEvent, ModelRequest, Provider, ProviderError, Usage};
use futures::StreamExt;

/// Everything the model said, and what saying it cost.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Answer {
    pub text: String,
    pub usage: Usage,
}

pub async fn drain(
    provider: &dyn Provider,
    request: ModelRequest,
    cancel: CancellationToken,
) -> Result<Answer, ProviderError> {
    let mut stream = provider.stream(request, cancel).await?;
    let mut answer = Answer::default();
    while let Some(event) = stream.next().await {
        match event? {
            ModelEvent::TextDelta { delta, .. } => answer.text.push_str(&delta),
            ModelEvent::Finish { usage, .. } => answer.usage = usage,
            _ => {}
        }
    }
    Ok(answer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scripted::Scripted;

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

    #[tokio::test]
    async fn the_answer_is_every_delta_and_the_usage_of_the_finish() {
        let provider = Scripted::saying("one two");
        let answer = drain(&provider, request(), CancellationToken::new())
            .await
            .expect("an answer");
        assert_eq!(answer.text, "one two");
        assert_eq!(answer.usage.output_tokens, Scripted::USAGE.output_tokens);
    }

    #[tokio::test]
    async fn a_provider_that_refuses_is_the_error_it_gave() {
        let provider = Scripted::failing(ProviderError::RateLimited {
            retry_after_ms: None,
        });
        let error = drain(&provider, request(), CancellationToken::new())
            .await
            .expect_err("refused");
        assert!(matches!(error, ProviderError::RateLimited { .. }));
    }
}
