use crate::api::types::DEFAULT_MAX_TOKENS;

/// Context window size.
pub const CONTEXT_WINDOW: u64 = 200_000;

/// Effective input window: requests are sent with DEFAULT_MAX_TOKENS; once input
/// crosses this line the server 400s with "input length and max_tokens exceed context
/// limit", so the reserved headroom matches the real max_tokens (not a fixed 20k).
pub const EFFECTIVE_WINDOW: u64 = CONTEXT_WINDOW - DEFAULT_MAX_TOKENS as u64;

/// Auto-compact threshold: 90% of the effective window (same semantics as Codex
/// auto_compact_token_limit).
pub const AUTOCOMPACT_THRESHOLD: u64 = EFFECTIVE_WINDOW * 9 / 10;

/// Warning buffer before the compact threshold (20k).
pub const WARNING_THRESHOLD: u64 = AUTOCOMPACT_THRESHOLD - 20_000;

/// Consecutive compact-failure circuit breaker (cap 3).
pub const MAX_COMPACT_FAILURES: u64 = 3;

/// Max chars of tool results backfilled into the model (50k).
pub const MAX_RESULT_CHARS: usize = 50_000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn threshold_hierarchy() {
        assert!(WARNING_THRESHOLD < AUTOCOMPACT_THRESHOLD);
        assert!(AUTOCOMPACT_THRESHOLD < EFFECTIVE_WINDOW);
        assert!(EFFECTIVE_WINDOW < CONTEXT_WINDOW);
    }

    /// Compact threshold + output budget must stay inside the window, otherwise every
    /// request 400s first and then retries.
    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn compaction_fires_before_the_api_rejects_the_request() {
        assert!(AUTOCOMPACT_THRESHOLD + DEFAULT_MAX_TOKENS as u64 <= CONTEXT_WINDOW);
        assert!(EFFECTIVE_WINDOW + DEFAULT_MAX_TOKENS as u64 <= CONTEXT_WINDOW);
    }
}
