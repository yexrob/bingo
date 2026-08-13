use crate::api::models::ModelResolver;
use crate::api::types::DEFAULT_MAX_TOKENS;

/// Per-model context window, measured with the caller's resolver (declared
/// metadata → prefix table → conservative default, see `api::models`). Display
/// and auto-compact take the same resolver so one ruler measures both — a
/// fixed 200k constant misread every non-Claude endpoint, and a second ruler
/// would put the status bar and the compactor at odds.
pub fn context_window_for(models: &ModelResolver, model: &str) -> u64 {
    models.context_window(model)
}

/// Effective input window: requests are sent with DEFAULT_MAX_TOKENS; once input
/// crosses this line the server 400s with "input length and max_tokens exceed context
/// limit", so the reserved headroom matches the real max_tokens (not a fixed 20k).
pub fn effective_window_for(models: &ModelResolver, model: &str) -> u64 {
    context_window_for(models, model).saturating_sub(DEFAULT_MAX_TOKENS as u64)
}

/// Auto-compact threshold: 90% of the effective window (same semantics as Codex
/// auto_compact_token_limit).
pub fn autocompact_threshold_for(models: &ModelResolver, model: &str) -> u64 {
    effective_window_for(models, model) * 9 / 10
}

/// Warning buffer before the compact threshold (20k).
pub fn warning_threshold_for(models: &ModelResolver, model: &str) -> u64 {
    autocompact_threshold_for(models, model).saturating_sub(20_000)
}

/// Consecutive compact-failure circuit breaker (cap 3).
pub const MAX_COMPACT_FAILURES: u64 = 3;

/// Max chars of tool results backfilled into the model (50k).
pub const MAX_RESULT_CHARS: usize = 50_000;

#[cfg(test)]
mod tests {
    use super::*;

    /// Threshold hierarchy holds for every known window size (the old
    /// constant-based test covered only the Claude default).
    #[test]
    fn threshold_hierarchy_per_model() {
        let models = ModelResolver::default();
        for model in ["claude-sonnet-5", "gpt-5.6-sol", "deepseek-chat", "unknown"] {
            assert!(
                warning_threshold_for(&models, model) < autocompact_threshold_for(&models, model)
            );
            assert!(
                autocompact_threshold_for(&models, model) < effective_window_for(&models, model)
            );
            assert!(effective_window_for(&models, model) < context_window_for(&models, model));
        }
    }

    /// Compact threshold + output budget must stay inside the window, otherwise every
    /// request 400s first and then retries.
    #[test]
    fn compaction_fires_before_the_api_rejects_the_request() {
        let models = ModelResolver::default();
        for model in ["claude-sonnet-5", "gpt-5.6-sol", "deepseek-chat"] {
            assert!(
                autocompact_threshold_for(&models, model) + DEFAULT_MAX_TOKENS as u64
                    <= context_window_for(&models, model)
            );
            assert!(
                effective_window_for(&models, model) + DEFAULT_MAX_TOKENS as u64
                    <= context_window_for(&models, model)
            );
        }
    }
}
