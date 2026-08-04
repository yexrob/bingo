/// 上下文窗口大小（deepseek-v4 系列）。
pub const CONTEXT_WINDOW: u64 = 200_000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_is_positive() {
        assert!(CONTEXT_WINDOW > 0);
    }
}
