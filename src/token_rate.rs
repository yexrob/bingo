use std::collections::VecDeque;
use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(3);
const STALE_AFTER: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenRateBand {
    VerySlow,
    Slow,
    Normal,
    Fast,
    VeryFast,
}

impl TokenRateBand {
    pub fn for_rate(rate: f64) -> Self {
        if rate < 5.0 {
            Self::VerySlow
        } else if rate < 15.0 {
            Self::Slow
        } else if rate < 40.0 {
            Self::Normal
        } else if rate < 80.0 {
            Self::Fast
        } else {
            Self::VeryFast
        }
    }

    fn frames(self) -> &'static [&'static str] {
        match self {
            Self::VerySlow => &["⠋", "⠙", "⠹"],
            Self::Slow => &["▓░", "░▓"],
            Self::Normal => &["▁▃▅▇", "▃▅▇▁", "▅▇▁▃", "▇▁▃▅"],
            Self::Fast => &["█▀▄", "▀▄█", "▄█▀"],
            Self::VeryFast => &["≡  ", "≡≡ ", "≡≡≡"],
        }
    }

    fn frame_millis(self) -> u128 {
        match self {
            Self::VerySlow => 480,
            Self::Slow => 320,
            Self::Normal => 200,
            Self::Fast => 120,
            Self::VeryFast => 70,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TokenRate {
    pub tokens_per_second: f64,
    pub band: TokenRateBand,
}

impl TokenRate {
    pub fn label(self, now: Instant, started: Instant, motion_off: bool) -> String {
        let frames = self.band.frames();
        let index = if motion_off {
            0
        } else {
            (now.saturating_duration_since(started).as_millis() / self.band.frame_millis()) as usize
                % frames.len()
        };
        format!("{} {:.1} tok/s", frames[index], self.tokens_per_second)
    }
}

#[derive(Debug, Clone)]
pub struct TokenRateSampler {
    samples: VecDeque<(Instant, u64)>,
    started: Instant,
    active: bool,
    committed_tokens: u64,
    round_tokens: u64,
}

impl Default for TokenRateSampler {
    fn default() -> Self {
        Self::new(Instant::now())
    }
}

impl TokenRateSampler {
    pub fn new(now: Instant) -> Self {
        Self {
            samples: VecDeque::new(),
            started: now,
            active: false,
            committed_tokens: 0,
            round_tokens: 0,
        }
    }

    pub fn start(&mut self, now: Instant) {
        self.samples.clear();
        self.started = now;
        self.active = true;
        self.committed_tokens = 0;
        self.round_tokens = 0;
    }

    pub fn observe_round(&mut self, tokens: u64, now: Instant) {
        if !self.active {
            self.start(now);
        }
        if tokens < self.round_tokens {
            self.samples.clear();
        }
        self.round_tokens = tokens;
        self.observe_total(self.committed_tokens.saturating_add(tokens), now);
    }

    /// End-of-round authoritative usage: replaces the running estimate without
    /// counting the correction as output produced at `now`. Feeding the jump
    /// through `observe_round` divided it by the live window and rendered as a
    /// one-frame spike of thousands of tok/s; the rate window restarts from the
    /// corrected total instead, exactly like a downward correction.
    pub fn correct_round(&mut self, tokens: u64, now: Instant) {
        if !self.active {
            self.start(now);
        }
        if tokens != self.round_tokens {
            self.samples.clear();
        }
        self.round_tokens = tokens;
        self.observe_total(self.committed_tokens.saturating_add(tokens), now);
    }

    pub fn finish_round(&mut self) {
        self.committed_tokens = self.committed_tokens.saturating_add(self.round_tokens);
        self.round_tokens = 0;
    }

    pub fn retry_round(&mut self) {
        self.samples.clear();
        self.round_tokens = 0;
    }

    pub fn stop(&mut self) {
        self.samples.clear();
        self.active = false;
        self.committed_tokens = 0;
        self.round_tokens = 0;
    }

    pub fn rate(&self, now: Instant) -> Option<TokenRate> {
        if !self.active {
            return None;
        }
        let (last_at, last_tokens) = self.samples.back().copied()?;
        if now.saturating_duration_since(last_at) > STALE_AFTER {
            return None;
        }
        let (first_at, first_tokens) = self.samples.front().copied()?;
        let elapsed = last_at.saturating_duration_since(first_at).as_secs_f64();
        if elapsed <= f64::EPSILON || last_tokens <= first_tokens {
            return None;
        }
        let tokens_per_second = (last_tokens - first_tokens) as f64 / elapsed;
        Some(TokenRate {
            tokens_per_second,
            band: TokenRateBand::for_rate(tokens_per_second),
        })
    }

    pub fn label(&self, now: Instant, motion_off: bool) -> Option<String> {
        self.rate(now)
            .map(|rate| rate.label(now, self.started, motion_off))
    }

    fn observe_total(&mut self, tokens: u64, now: Instant) {
        if self
            .samples
            .back()
            .is_some_and(|(at, _)| now.saturating_duration_since(*at) > STALE_AFTER)
        {
            self.samples.clear();
        }
        if self.samples.is_empty() {
            self.samples.push_back((now, 0));
        }
        if let Some((at, previous)) = self.samples.back_mut()
            && *at == now
        {
            *previous = tokens;
        } else {
            self.samples.push_back((now, tokens));
        }
        self.trim(now);
    }

    fn trim(&mut self, now: Instant) {
        while self.samples.len() > 1
            && self
                .samples
                .get(1)
                .is_some_and(|(at, _)| now.saturating_duration_since(*at) >= WINDOW)
        {
            self.samples.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampler_uses_the_recent_window_and_hides_without_live_output() {
        let start = Instant::now();
        let mut sampler = TokenRateSampler::new(start);
        assert_eq!(sampler.rate(start), None, "idle stays quiet");

        sampler.start(start);
        sampler.observe_round(4, start + Duration::from_secs(1));
        sampler.observe_round(12, start + Duration::from_secs(2));
        sampler.observe_round(20, start + Duration::from_secs(3));
        let rate = sampler
            .rate(start + Duration::from_secs(3))
            .expect("the live window has output");
        assert!((rate.tokens_per_second - 8.0).abs() < 0.01);
        assert_eq!(rate.band, TokenRateBand::Slow);

        assert_eq!(
            sampler.rate(start + Duration::from_secs(6)),
            None,
            "a stalled stream does not leave a stale speed visible"
        );
        sampler.stop();
        assert_eq!(sampler.rate(start + Duration::from_secs(7)), None);
    }

    #[test]
    fn first_sample_does_not_include_connection_latency() {
        let start = Instant::now();
        let mut sampler = TokenRateSampler::new(start);
        sampler.start(start);
        sampler.observe_round(8, start + Duration::from_secs(10));
        assert_eq!(sampler.rate(start + Duration::from_secs(10)), None);
        sampler.observe_round(12, start + Duration::from_secs(11));
        let rate = sampler
            .rate(start + Duration::from_secs(11))
            .expect("two live samples establish a rate");
        assert!((rate.tokens_per_second - 4.0).abs() < 0.01);

        sampler.observe_round(16, start + Duration::from_secs(20));
        assert_eq!(
            sampler.rate(start + Duration::from_secs(20)),
            None,
            "the first sample after a long stall starts a fresh window"
        );
    }

    #[test]
    fn round_counters_are_folded_into_one_monotonic_turn_total() {
        let start = Instant::now();
        let mut sampler = TokenRateSampler::new(start);
        sampler.start(start);
        sampler.observe_round(8, start + Duration::from_secs(1));
        sampler.finish_round();
        sampler.observe_round(4, start + Duration::from_secs(2));
        let rate = sampler
            .rate(start + Duration::from_secs(2))
            .expect("two rounds produced output");
        assert!((rate.tokens_per_second - 4.0).abs() < 0.01);
    }

    #[test]
    fn authoritative_round_count_replaces_the_estimate() {
        let start = Instant::now();
        let mut sampler = TokenRateSampler::new(start);
        sampler.start(start);
        sampler.observe_round(20, start + Duration::from_secs(1));
        sampler.observe_round(12, start + Duration::from_secs(2));
        assert_eq!(
            sampler.rate(start + Duration::from_secs(2)),
            None,
            "an authoritative correction resets the estimated window"
        );
        sampler.finish_round();
        sampler.observe_round(4, start + Duration::from_millis(2500));
        sampler.observe_round(8, start + Duration::from_millis(2750));
        sampler.observe_round(12, start + Duration::from_secs(3));
        let rate = sampler
            .rate(start + Duration::from_secs(3))
            .expect("the corrected count and next round remain measurable");
        assert!(
            rate.tokens_per_second.is_finite() && rate.tokens_per_second > 0.0,
            "the correction must keep a finite live rate: {rate:?}"
        );
    }

    /// The usage total on message_delta covers thinking and tool JSON the chars/4
    /// estimate undercounted; treating that jump as instantly streamed output showed
    /// absurd one-frame spikes (thousands of tok/s). A correction restarts the
    /// window at the corrected total instead.
    #[test]
    fn upward_usage_correction_is_not_a_rate_spike() {
        let start = Instant::now();
        let mut sampler = TokenRateSampler::new(start);
        sampler.start(start);
        sampler.observe_round(10, start + Duration::from_millis(100));
        sampler.observe_round(20, start + Duration::from_millis(200));
        sampler.correct_round(5_000, start + Duration::from_millis(210));
        assert_eq!(
            sampler.rate(start + Duration::from_millis(210)),
            None,
            "the correction restarts the rate window instead of spiking"
        );
        sampler.finish_round();
        sampler.observe_round(8, start + Duration::from_millis(700));
        sampler.observe_round(16, start + Duration::from_millis(1200));
        let rate = sampler
            .rate(start + Duration::from_millis(1200))
            .expect("streaming after the correction is measurable again");
        assert!(
            rate.tokens_per_second < 100.0,
            "the corrected baseline does not leak into the next round: {rate:?}"
        );
    }

    #[test]
    fn bands_and_animation_cadence_make_slow_and_fast_distinct() {
        assert_eq!(TokenRateBand::for_rate(4.9), TokenRateBand::VerySlow);
        assert_eq!(TokenRateBand::for_rate(5.0), TokenRateBand::Slow);
        assert_eq!(TokenRateBand::for_rate(15.0), TokenRateBand::Normal);
        assert_eq!(TokenRateBand::for_rate(40.0), TokenRateBand::Fast);
        assert_eq!(TokenRateBand::for_rate(80.0), TokenRateBand::VeryFast);

        let start = Instant::now();
        let slow = TokenRate {
            tokens_per_second: 3.0,
            band: TokenRateBand::VerySlow,
        };
        let fast = TokenRate {
            tokens_per_second: 90.0,
            band: TokenRateBand::VeryFast,
        };
        assert_eq!(slow.label(start, start, false), "⠋ 3.0 tok/s");
        assert_eq!(
            slow.label(start + Duration::from_millis(100), start, false),
            "⠋ 3.0 tok/s",
            "the slow band holds its frame"
        );
        assert_ne!(
            fast.label(start, start, false),
            fast.label(start + Duration::from_millis(100), start, false),
            "the very-fast band advances within the same interval"
        );
        assert_eq!(
            fast.label(start + Duration::from_secs(1), start, true),
            "≡   90.0 tok/s",
            "motion-off keeps the indicator but freezes it"
        );
    }
}
