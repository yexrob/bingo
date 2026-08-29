//! The streaming leg of a round: the provider's events folded into items as
//! they arrive, and what a stream that ended badly means — compact, wait and
//! retry, or give up.

use std::time::Duration;

use bingo_sdk::*;
use futures::StreamExt;

use super::{Step, Turn};
use crate::accumulator::{Accumulator, Emit, Finished};
use crate::models::window_from_overflow;

pub const MAX_RETRY_DELAY: Duration = Duration::from_secs(32);
pub const MAX_SERVER_RETRY_DELAY: Duration = Duration::from_secs(60);

/// How one stream ended. `Failed` carries the items to withdraw before a retry.
pub(super) enum Streamed {
    Done(Finished),
    Failed(ProviderError, Vec<ItemId>),
    Cancelled,
}

impl Turn<'_> {
    pub(super) async fn stream(&mut self, request: ModelRequest) -> Streamed {
        let mut stream = match self
            .cfg
            .model
            .provider
            .stream(request, self.cancel.child_token())
            .await
        {
            Ok(s) => s,
            Err(e) => return Streamed::Failed(e, Vec::new()),
        };
        let mut acc = Accumulator::new(self.id.clone(), self.round);
        let mut cancelled = false;
        let mut error: Option<ProviderError> = None;
        loop {
            tokio::select! {
                next = stream.next() => match next {
                    Some(Ok(event)) => {
                        for emit in acc.push(event) {
                            self.publish(emit);
                        }
                    }
                    Some(Err(e)) => { error = Some(e); break; }
                    None => break,
                },
                _ = self.cancel.cancelled() => { cancelled = true; break; }
            }
        }
        let dropped = acc.item_ids();
        let (emits, mut finished) = acc.finish(cancelled);
        for emit in emits {
            self.publish(emit);
        }
        if cancelled {
            return Streamed::Cancelled;
        }
        if let Some(e) = error.or_else(|| finished.error.take()) {
            return Streamed::Failed(e, dropped);
        }
        Streamed::Done(finished)
    }

    /// One fold result: into the turn's own transcript and out to the clients.
    fn publish(&mut self, emit: Emit) {
        match emit {
            Emit::Started(item) => {
                self.upsert(&item);
                self.host.emit(Event::ItemStarted { item });
            }
            Emit::Delta {
                item,
                n,
                kind,
                data,
            } => self.host.emit(Event::ItemDelta {
                item,
                n,
                kind,
                data,
            }),
            Emit::Completed(item) => {
                self.upsert(&item);
                self.host.emit(Event::ItemCompleted { item });
            }
        }
    }

    /// The server named its real window; later sessions on this model
    /// measure against it (ADR-0004).
    fn learn_window(&self, message: &str) {
        let Some(window) = window_from_overflow(message) else {
            return;
        };
        let provider = self.cfg.model.provider.id();
        if self
            .cfg
            .model
            .learned
            .record(provider, &self.cfg.model.id, window)
        {
            self.host.emit(Event::Notice {
                level: Level::Info,
                code: "WINDOW_LEARNED".into(),
                text: format!(
                    "{provider}/{} takes {window} tokens of context; later sessions measure against it",
                    self.cfg.model.id
                ),
            });
        }
    }

    /// A stream that failed: half-written items are withdrawn, then overflow
    /// compacts once, a retryable error waits, and anything else ends the turn.
    pub(super) async fn failed_stream(
        &mut self,
        error: ProviderError,
        dropped: Vec<ItemId>,
        usage: ContextUsage,
    ) -> Step {
        self.items.retain(|i| !dropped.contains(&i.id));
        if let ProviderError::ContextOverflow { message } = &error {
            self.learn_window(message);
        }
        if let ProviderError::ContextOverflow { .. } = &error
            && !self.overflow_compacted
        {
            // One retry: the strategy's overflow cut if there is one, and
            // the forced microcompact either way (ADR-0006).
            self.overflow_compacted = true;
            self.announce_retry(&error, dropped, 0);
            if let Some(compactor) = self.cfg.compactor.clone() {
                self.compact(
                    compactor.as_ref(),
                    CompactReason::Overflow {
                        message: error.to_string(),
                    },
                    usage,
                )
                .await;
            }
            return Step::Assembling;
        }
        if error.retryable() && self.retries < self.cfg.budget.max_retries {
            self.retries += 1;
            let delay = backoff(self.retries, error.retry_after_ms());
            self.announce_retry(&error, dropped, delay.as_millis() as u64);
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = self.cancel.cancelled() => return self.interrupted(),
            }
            return Step::Assembling;
        }
        Step::Closing(TurnStatus::Failed {
            error: KernelError::new(error.code(), error.to_string()),
        })
    }

    fn announce_retry(&self, error: &ProviderError, dropped: Vec<ItemId>, delay_ms: u64) {
        self.host.emit(Event::TurnRetrying {
            turn: self.id.clone(),
            attempt: self.retries,
            max: self.cfg.budget.max_retries,
            delay_ms,
            dropped,
            reason: error.to_string(),
        });
    }
}

/// 500 ms doubling, capped at 32 s; a server-stated delay wins, capped at 60 s.
pub fn backoff(attempt: u32, retry_after_ms: Option<u64>) -> Duration {
    if let Some(ms) = retry_after_ms {
        return Duration::from_millis(ms).min(MAX_SERVER_RETRY_DELAY);
    }
    let exp = attempt.saturating_sub(1).min(6);
    Duration::from_millis(500 * (1u64 << exp)).min(MAX_RETRY_DELAY)
}
