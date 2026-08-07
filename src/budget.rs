use crate::api::types::DEFAULT_MAX_TOKENS;

/// 上下文窗口大小。
pub const CONTEXT_WINDOW: u64 = 200_000;

/// 有效输入窗口：请求按 DEFAULT_MAX_TOKENS 发出，输入越过这条线服务端
/// 必以 "input length and max_tokens exceed context limit" 回 400，
/// 故预留额度与真实 max_tokens 一致（不是固定 20k）。
pub const EFFECTIVE_WINDOW: u64 = CONTEXT_WINDOW - DEFAULT_MAX_TOKENS as u64;

/// 自动压缩阈值：有效窗口的 90%（与 Codex auto_compact_token_limit 同语义）。
pub const AUTOCOMPACT_THRESHOLD: u64 = EFFECTIVE_WINDOW * 9 / 10;

/// 接近压缩阈值的提醒缓冲（20k）。
pub const WARNING_THRESHOLD: u64 = AUTOCOMPACT_THRESHOLD - 20_000;

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
        assert!(AUTOCOMPACT_THRESHOLD < EFFECTIVE_WINDOW);
        assert!(EFFECTIVE_WINDOW < CONTEXT_WINDOW);
    }

    /// 压缩阈值 + 输出预算必须留在窗口内，否则每个请求都先 400 再重试。
    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn compaction_fires_before_the_api_rejects_the_request() {
        assert!(AUTOCOMPACT_THRESHOLD + DEFAULT_MAX_TOKENS as u64 <= CONTEXT_WINDOW);
        assert!(EFFECTIVE_WINDOW + DEFAULT_MAX_TOKENS as u64 <= CONTEXT_WINDOW);
    }
}
