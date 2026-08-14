use crate::api::models::ModelResolver;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextUsageBand {
    Normal,
    Warning,
    Danger,
}

/// Context occupancy as the footer states it: `used` against the model's raw
/// `window`, plus `trigger` — the token count at which auto-compact actually
/// fires (`budget::autocompact_threshold_for`, 90% of the window minus the
/// reserved output budget).
///
/// The colour bands measure the distance to `trigger`, not to `window`. A fixed
/// 70/90% of the raw window described no model correctly: the two denominators
/// differ by that model's own output budget, so the same percentage sat 12
/// points before compaction on one endpoint and 35 points after it on another —
/// the danger band could open only once compaction had already run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextUsage {
    pub used: u64,
    pub window: u64,
    pub trigger: u64,
}

/// How far ahead of the auto-compact trigger each band opens, in percentage
/// points of the window the label shows.
const WARNING_POINTS: u128 = 20;
const DANGER_POINTS: u128 = 5;

impl ContextUsage {
    pub fn new(used: u64, window: u64, trigger: u64) -> Self {
        Self {
            used,
            window: window.max(1),
            trigger: trigger.max(1),
        }
    }

    /// The measurement every production call site wants: one resolver decides
    /// both the denominator on screen and the trigger the compactor obeys.
    pub fn for_model(used: u64, models: &ModelResolver, model: &str) -> Self {
        Self::new(
            used,
            crate::budget::context_window_for(models, model),
            crate::budget::autocompact_threshold_for(models, model),
        )
    }

    pub fn percent(self) -> u64 {
        ((self.used as u128 * 100 / self.window as u128).min(u64::MAX as u128)) as u64
    }

    pub fn band(self) -> ContextUsageBand {
        let headroom = self.trigger.saturating_sub(self.used) as u128 * 100;
        let window = self.window as u128;
        if headroom <= window * DANGER_POINTS {
            ContextUsageBand::Danger
        } else if headroom <= window * WARNING_POINTS {
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
            ContextUsage::new(1_234, 128_000, 100_000).label(),
            "░░░░ 0% 1234/128k"
        );
        assert_eq!(
            ContextUsage::new(74_240, 128_000, 100_000).label(),
            "▓▓░░ 58% 74240/128k"
        );
    }

    /// Bands open a fixed distance ahead of the trigger, measured in points of
    /// the window the label shows: warning 20 points out, danger 5.
    #[test]
    fn bands_open_ahead_of_the_auto_compact_trigger() {
        let at = |used| ContextUsage::new(used, 100, 90).band();
        assert_eq!(at(69), ContextUsageBand::Normal);
        assert_eq!(at(70), ContextUsageBand::Warning, "trigger - 20 points");
        assert_eq!(at(84), ContextUsageBand::Warning);
        assert_eq!(at(85), ContextUsageBand::Danger, "trigger - 5 points");
        assert_eq!(at(90), ContextUsageBand::Danger);
        assert_eq!(at(200), ContextUsageBand::Danger, "past the trigger");
    }

    /// The trigger, not the window, sets where the colours change: a model that
    /// reserves a large output budget compacts far below 90% of its window, and
    /// the footer has to warn before that happens, not after.
    #[test]
    fn a_low_trigger_moves_the_bands_down_with_it() {
        let at = |used| ContextUsage::new(used, 1_000_000, 554_400).band();
        assert_eq!(at(354_399), ContextUsageBand::Normal);
        assert_eq!(at(354_400), ContextUsageBand::Warning);
        assert_eq!(at(504_399), ContextUsageBand::Warning);
        assert_eq!(at(504_400), ContextUsageBand::Danger);
        assert_eq!(
            ContextUsage::new(504_400, 1_000_000, 900_000).band(),
            ContextUsageBand::Normal,
            "the same tokens are unremarkable when compaction is far off"
        );
    }

    /// One resolver measures the label and the bands, so the footer cannot
    /// disagree with the compactor about when it fires.
    #[test]
    fn for_model_takes_window_and_trigger_from_the_same_resolver() {
        let models = ModelResolver::default();
        let usage = ContextUsage::for_model(0, &models, "claude-sonnet-5");
        assert_eq!(
            usage.window,
            crate::budget::context_window_for(&models, "claude-sonnet-5")
        );
        assert_eq!(
            usage.trigger,
            crate::budget::autocompact_threshold_for(&models, "claude-sonnet-5")
        );
        assert!(usage.trigger < usage.window);
        assert_eq!(
            ContextUsage::for_model(usage.trigger, &models, "claude-sonnet-5").band(),
            ContextUsageBand::Danger
        );
    }
}
