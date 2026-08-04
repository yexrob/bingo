use crate::api::client::Client;
use crate::api::types::{Message, SystemBlock};

/// 上下文窗口大小（deepseek-v4 系列）。
pub const CONTEXT_WINDOW: u64 = 200_000;
/// 警告阈值：占用窗口比例。
const WARN_FRACTION: f64 = 0.9;

/// 输入预算检查（D12 最小面）：计数超阈值时警告一次。
/// autoCompact 在第 6 轮接入；失败静默，不阻断主流程。
pub async fn check_input_budget(
    client: &Client,
    model: &str,
    system: &[SystemBlock],
    messages: &[Message],
    warned: &mut bool,
) {
    let Ok(tokens) = client.count_tokens(model, system, messages).await else {
        return;
    };
    let fraction = tokens as f64 / CONTEXT_WINDOW as f64;
    if fraction >= WARN_FRACTION {
        eprintln!(
            "[bingo] warning: context at {tokens} tokens ({:.0}% of {CONTEXT_WINDOW}); auto-compact not yet implemented",
            fraction * 100.0
        );
        *warned = true;
    } else if !*warned && tokens > 0 {
        eprintln!("[bingo] context: {tokens} tokens");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fraction_thresholds() {
        let high = 195_000.0 / CONTEXT_WINDOW as f64;
        assert!(high >= WARN_FRACTION);
        let low = 100_000.0 / CONTEXT_WINDOW as f64;
        assert!(low < WARN_FRACTION);
    }
}
