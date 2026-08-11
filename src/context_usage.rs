#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextUsageBand {
    Normal,
    Warning,
    Danger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextUsage {
    pub used: u64,
    pub window: u64,
}

impl ContextUsage {
    pub fn new(used: u64, window: u64) -> Self {
        Self {
            used,
            window: window.max(1),
        }
    }

    pub fn percent(self) -> u64 {
        ((self.used as u128 * 100 / self.window as u128).min(u64::MAX as u128)) as u64
    }

    pub fn band(self) -> ContextUsageBand {
        let scaled = self.used as u128 * 100;
        let window = self.window as u128;
        if scaled > window * 90 {
            ContextUsageBand::Danger
        } else if scaled >= window * 70 {
            ContextUsageBand::Warning
        } else {
            ContextUsageBand::Normal
        }
    }

    pub fn label(self) -> String {
        let percent = self.percent();
        let filled = ((percent.min(100) * 4) / 100) as usize;
        format!(
            "{}{} {percent}% {}/{}",
            "▓".repeat(filled),
            "░".repeat(4 - filled),
            compact_tokens(self.used),
            compact_tokens(self.window)
        )
    }
}

fn compact_tokens(tokens: u64) -> String {
    if tokens < 100_000 {
        return tokens.to_string();
    }
    if tokens.is_multiple_of(1_000) {
        return format!("{}k", tokens / 1_000);
    }
    let tenths = (tokens + 50) / 100;
    format!("{}.{}k", tenths / 10, tenths % 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_renders_used_window_bar_and_percent() {
        assert_eq!(
            ContextUsage::new(1_234, 128_000).label(),
            "░░░░ 0% 1234/128k"
        );
        assert_eq!(
            ContextUsage::new(74_240, 128_000).label(),
            "▓▓░░ 58% 74240/128k"
        );
    }

    #[test]
    fn bands_switch_at_the_required_boundaries() {
        assert_eq!(ContextUsage::new(69, 100).band(), ContextUsageBand::Normal);
        assert_eq!(ContextUsage::new(70, 100).band(), ContextUsageBand::Warning);
        assert_eq!(ContextUsage::new(90, 100).band(), ContextUsageBand::Warning);
        assert_eq!(
            ContextUsage::new(90_999, 100_000).band(),
            ContextUsageBand::Danger
        );
        assert_eq!(ContextUsage::new(91, 100).band(), ContextUsageBand::Danger);
    }
}
