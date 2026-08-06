/// 上下文窗口大小（deepseek-v4 系列）。
pub const CONTEXT_WINDOW: u64 = 200_000;

/// 有效窗口：预留输出预算（min(max_tokens, 20k)）。
pub const EFFECTIVE_WINDOW: u64 = CONTEXT_WINDOW - 20_000;

/// 自动压缩阈值：90% 窗口（与 Codex auto_compact_token_limit 同语义）。
pub const AUTOCOMPACT_THRESHOLD: u64 = CONTEXT_WINDOW * 9 / 10;

/// 接近压缩阈值的提醒缓冲（20k）。
pub const WARNING_THRESHOLD: u64 = EFFECTIVE_WINDOW - 20_000;

/// 连续压缩失败熔断（上限 3 次）。
pub const MAX_COMPACT_FAILURES: u64 = 3;

/// 工具结果回填模型前的最大字符数（50k）。
pub const MAX_RESULT_CHARS: usize = 50_000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn threshold_hierarchy() {
        assert!(WARNING_THRESHOLD < AUTOCOMPACT_THRESHOLD);
        assert!(AUTOCOMPACT_THRESHOLD < CONTEXT_WINDOW);
    }
}
