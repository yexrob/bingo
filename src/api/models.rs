//! Per-model metadata: context window and thinking support.
//!
//! One prefix table instead of hard-coded globals: `/status` percentages, the
//! auto-compact threshold and the thinking gate all read the model actually in
//! use — the old fixed 200k window measured every non-Claude model with a
//! Claude ruler. Unknown models fall back to the conservative Claude defaults
//! (200k window, thinking supported), which preserves the old behavior
//! exactly where nothing better is known.

/// Metadata for one model family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelMeta {
    pub context_window: u64,
    pub supports_thinking: bool,
}

/// Conservative default (Claude family): what the whole app assumed for
/// every model before this table existed.
pub const DEFAULT_META: ModelMeta = ModelMeta {
    context_window: 200_000,
    supports_thinking: true,
};

/// Longest-prefix match over known families. Kept deliberately small: entries
/// earn their place by a real behavioral difference (window size or a wire
/// parameter that would 400).
const PREFIXES: &[(&str, ModelMeta)] = &[
    (
        "claude-",
        ModelMeta {
            context_window: 200_000,
            supports_thinking: true,
        },
    ),
    // Codex subscription family (gpt-5.x): larger window, reasoning effort.
    (
        "gpt-5",
        ModelMeta {
            context_window: 400_000,
            supports_thinking: true,
        },
    ),
    // DeepSeek chat endpoints reject anthropic thinking parameters — the
    // documented reason `/think off` exists. The gate skips the parameter for
    // them regardless of the configured level.
    (
        "deepseek",
        ModelMeta {
            context_window: 128_000,
            supports_thinking: false,
        },
    ),
];

pub fn meta(model: &str) -> ModelMeta {
    PREFIXES
        .iter()
        .filter(|(prefix, _)| model.starts_with(prefix))
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(_, meta)| *meta)
        .unwrap_or(DEFAULT_META)
}

pub fn context_window(model: &str) -> u64 {
    meta(model).context_window
}

pub fn supports_thinking(model: &str) -> bool {
    meta(model).supports_thinking
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_table_and_default() {
        assert_eq!(context_window("claude-sonnet-5"), 200_000);
        assert_eq!(context_window("gpt-5.6-sol"), 400_000);
        assert_eq!(context_window("deepseek-chat"), 128_000);
        assert_eq!(context_window("some-unknown-model"), 200_000, "保守默认");
        assert!(supports_thinking("claude-sonnet-5"));
        assert!(
            !supports_thinking("deepseek-chat"),
            "DeepSeek 不发 thinking"
        );
        assert!(
            supports_thinking("totally-new-model"),
            "未知默认支持（保持旧行为）"
        );
    }
}
