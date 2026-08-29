//! A provider that says one thing and remembers what it was asked. The fake
//! provider crate is another plugin, and a plugin never imports a plugin.

use std::sync::Mutex;

use async_trait::async_trait;
use bingo_sdk::{
    CancellationToken, EndpointCapabilities, FinishReason, ModelEvent, ModelRequest, ModelStream,
    Provider, ProviderError, UnifiedFinish, Usage,
};

#[derive(Debug, Default)]
pub struct Scripted {
    answer: String,
    error: Option<ProviderError>,
    seen: Mutex<Vec<ModelRequest>>,
}

impl Scripted {
    pub const USAGE: Usage = Usage {
        input_tokens: 900,
        output_tokens: 40,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: 0,
    };

    pub fn saying(answer: &str) -> Self {
        Self {
            answer: answer.to_string(),
            ..Self::default()
        }
    }

    pub fn failing(error: ProviderError) -> Self {
        Self {
            error: Some(error),
            ..Self::default()
        }
    }

    /// The requests it was asked, in order.
    pub fn requests(&self) -> Vec<ModelRequest> {
        self.seen
            .lock()
            .map(|seen| seen.clone())
            .unwrap_or_default()
    }
}

#[async_trait]
impl Provider for Scripted {
    fn id(&self) -> &str {
        "scripted"
    }

    fn endpoint(&self, _model: &str) -> EndpointCapabilities {
        EndpointCapabilities::default()
    }

    async fn stream(
        &self,
        request: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<ModelStream, ProviderError> {
        if let Ok(mut seen) = self.seen.lock() {
            seen.push(request);
        }
        if let Some(error) = self.error.clone() {
            return Err(error);
        }
        let events = vec![
            Ok(ModelEvent::TextStart { id: "t".into() }),
            Ok(ModelEvent::TextDelta {
                id: "t".into(),
                delta: self.answer.clone(),
            }),
            Ok(ModelEvent::TextEnd { id: "t".into() }),
            Ok(ModelEvent::Finish {
                usage: Self::USAGE,
                finish_reason: FinishReason::unified(UnifiedFinish::Stop),
            }),
        ];
        Ok(Box::pin(futures::stream::iter(events)))
    }
}
