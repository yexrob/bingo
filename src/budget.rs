use crate::api::models::ModelResolver;

/// Per-model context window, measured with the caller's resolver (declared
/// metadata → prefix table → conservative default, see `api::models`). Display
/// and auto-compact take the same resolver so one ruler measures both — a
/// fixed 200k constant misread every non-Claude endpoint, and a second ruler
/// would put the status bar and the compactor at odds.
pub fn context_window_for(models: &ModelResolver, model: &str) -> u64 {
    models.context_window(model)
}

/// Output budget actually sent as `max_tokens`, clamped to half the window.
/// The clamp is what keeps the threshold hierarchy standing for small windows:
/// a flat 64k against a declared 32k window left an effective window of 0, so
/// the compact threshold was 0 and every turn past KEEP_RECENT compacted — and
/// the request 400d anyway for asking more output than the model can give.
/// Half the window guarantees `effective >= window / 2` for any declaration.
pub fn max_tokens_for(models: &ModelResolver, model: &str) -> u32 {
    let meta = models.meta(model);
    let half = (meta.context_window / 2).min(u32::MAX as u64) as u32;
    meta.max_tokens.min(half)
}

/// Effective input window: requests are sent with the model's max_tokens; once input
/// crosses this line the server 400s with "input length and max_tokens exceed context
/// limit", so the reserved headroom matches the real max_tokens (not a fixed 20k).
pub fn effective_window_for(models: &ModelResolver, model: &str) -> u64 {
    context_window_for(models, model).saturating_sub(max_tokens_for(models, model) as u64)
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
    use crate::api::models::ModelCatalog;
    use std::sync::Arc;

    /// A provider declaring a 32k window — the case a flat 64k output budget
    /// collapsed to an effective window of 0.
    fn small_window() -> ModelResolver {
        let settings =
            serde_json::from_str(r#"{"models": [{"id": "tiny-model", "contextWindow": 32768}]}"#)
                .unwrap();
        ModelResolver::new(
            Arc::new(ModelCatalog::from_settings(&settings)),
            "default".to_string(),
        )
    }

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
            let output = max_tokens_for(&models, model) as u64;
            assert!(
                autocompact_threshold_for(&models, model) + output
                    <= context_window_for(&models, model)
            );
            assert!(
                effective_window_for(&models, model) + output <= context_window_for(&models, model)
            );
        }
    }

    /// Known families keep their numbers, except DeepSeek: its real 8k output
    /// ceiling hands 56k of wrongly reserved headroom back to the input window.
    #[test]
    fn per_family_output_budgets() {
        let models = ModelResolver::default();
        assert_eq!(max_tokens_for(&models, "claude-sonnet-5"), 64_000);
        assert_eq!(
            autocompact_threshold_for(&models, "claude-sonnet-5"),
            122_400
        );
        assert_eq!(max_tokens_for(&models, "gpt-5.6-sol"), 64_000);
        assert_eq!(autocompact_threshold_for(&models, "gpt-5.6-sol"), 302_400);
        assert_eq!(max_tokens_for(&models, "deepseek-chat"), 8_000);
        assert_eq!(autocompact_threshold_for(&models, "deepseek-chat"), 108_000);
    }

    /// A declared small window must keep the whole hierarchy standing: the clamp
    /// leaves at least half the window for input, so the threshold is a real
    /// number instead of 0 (which compacted every turn past KEEP_RECENT).
    #[test]
    fn declared_small_window_keeps_the_hierarchy() {
        let models = small_window();
        assert_eq!(max_tokens_for(&models, "tiny-model"), 16_384);
        assert_eq!(effective_window_for(&models, "tiny-model"), 16_384);
        assert!(autocompact_threshold_for(&models, "tiny-model") > 0);
        assert!(
            autocompact_threshold_for(&models, "tiny-model")
                < effective_window_for(&models, "tiny-model")
        );
        assert!(
            autocompact_threshold_for(&models, "tiny-model")
                + max_tokens_for(&models, "tiny-model") as u64
                <= context_window_for(&models, "tiny-model")
        );
    }
}
