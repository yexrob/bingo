//! `Effort` → the `reasoning.effort` string a model's family accepts.
//!
//! The Responses API documents `effort` as "model-dependent" and 400s on a
//! level the family does not take, so the adapter clamps rather than guesses.
//! The accepted sets below are models.dev's `openai.models[<id>]
//! .reasoning_options[0].values`, verbatim (snapshot of 2026-08-29); the
//! lookup is the longest model id that prefixes the request, so a dated
//! snapshot (`gpt-5.4-2026-01-30`) reads its family's row.

use bingo_sdk::Effort;

/// The levels the API names, shallowest first (verified against the reasoning
/// guide, 2026-08-29: "`none`, `minimal`, `low`, `medium`, `high`, `xhigh`,
/// and `max`").
const LADDER: &[&str] = &["none", "minimal", "low", "medium", "high", "xhigh", "max"];

/// Reasoning off. Never chosen: `ModelRequest::reasoning` is `Option`, so a
/// caller that wants no reasoning sends no `reasoning` object at all, and
/// answering `Some(Effort::Minimal)` with "off" would contradict the request.
const NONE: &str = "none";

/// The clamp for a model no row claims. `low|medium|high` is the set every
/// reasoning family in the catalogue contains, so it cannot 400.
const UNKNOWN: &[&str] = &["low", "medium", "high"];

/// `(model id prefix, accepted values ascending)`. Longest prefix wins, so a
/// row may refine the one above it (`gpt-5.4-pro` inside `gpt-5.4`).
const FAMILIES: &[(&str, &[&str])] = &[
    ("gpt-5", &["minimal", "low", "medium", "high"]),
    ("gpt-5-pro", &["high"]),
    ("gpt-5.1", &["none", "low", "medium", "high"]),
    ("gpt-5.2", &["none", "low", "medium", "high", "xhigh"]),
    ("gpt-5.2-chat", &["medium"]),
    ("gpt-5.2-pro", &["medium", "high", "xhigh"]),
    ("gpt-5.3-codex", &["none", "low", "medium", "high", "xhigh"]),
    ("gpt-5.4", &["none", "low", "medium", "high", "xhigh"]),
    ("gpt-5.4-pro", &["medium", "high", "xhigh"]),
    ("gpt-5.5", &["none", "low", "medium", "high", "xhigh"]),
    ("gpt-5.5-pro", &["medium", "high", "xhigh"]),
    (
        "gpt-5.6",
        &["none", "low", "medium", "high", "xhigh", "max"],
    ),
    ("o1", &["low", "medium", "high"]),
    ("o3", &["low", "medium", "high"]),
    ("o4-mini", &["low", "medium", "high"]),
];

/// The deepest accepted level at or below what was asked for; the shallowest
/// accepted one when the family starts deeper than the request.
pub fn effort_for(model: &str, effort: Effort) -> &'static str {
    let accepted = accepted_by(model);
    let wanted = rank(requested(effort));
    accepted
        .iter()
        .copied()
        .filter(|value| *value != NONE)
        .take_while(|value| rank(value) <= wanted)
        .last()
        .unwrap_or_else(|| shallowest(accepted))
}

fn accepted_by(model: &str) -> &'static [&'static str] {
    FAMILIES
        .iter()
        .filter(|(prefix, _)| model.starts_with(prefix))
        .max_by_key(|(prefix, _)| prefix.len())
        .map_or(UNKNOWN, |(_, values)| *values)
}

fn shallowest(accepted: &'static [&'static str]) -> &'static str {
    accepted
        .iter()
        .copied()
        .find(|value| *value != NONE)
        .unwrap_or("low")
}

/// Depth on the ladder. An unlisted name sorts above every level, so it is
/// never mistaken for one the request reaches.
fn rank(value: &str) -> usize {
    LADDER
        .iter()
        .position(|l| *l == value)
        .unwrap_or(LADDER.len())
}

fn requested(effort: Effort) -> &'static str {
    match effort {
        Effort::Minimal => "minimal",
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High => "high",
        Effort::XHigh => "xhigh",
        Effort::Max => "max",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEVELS: [Effort; 6] = [
        Effort::Minimal,
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::XHigh,
        Effort::Max,
    ];

    /// One row per family, in `LEVELS` order. The expectations are the
    /// clamp of models.dev's accepted set, not a guess.
    #[test]
    fn every_family_clamps_every_level_into_what_it_accepts() {
        let table: &[(&str, [&str; 6])] = &[
            // minimal|low|medium|high
            (
                "gpt-5",
                ["minimal", "low", "medium", "high", "high", "high"],
            ),
            (
                "gpt-5-mini",
                ["minimal", "low", "medium", "high", "high", "high"],
            ),
            (
                "gpt-5-nano",
                ["minimal", "low", "medium", "high", "high", "high"],
            ),
            // high only
            (
                "gpt-5-pro",
                ["high", "high", "high", "high", "high", "high"],
            ),
            // none|low|medium|high — `none` is never chosen, so minimal is low
            ("gpt-5.1", ["low", "low", "medium", "high", "high", "high"]),
            // none|low|medium|high|xhigh
            (
                "gpt-5.2",
                ["low", "low", "medium", "high", "xhigh", "xhigh"],
            ),
            (
                "gpt-5.3-codex",
                ["low", "low", "medium", "high", "xhigh", "xhigh"],
            ),
            (
                "gpt-5.3-codex-spark",
                ["low", "low", "medium", "high", "xhigh", "xhigh"],
            ),
            (
                "gpt-5.4",
                ["low", "low", "medium", "high", "xhigh", "xhigh"],
            ),
            (
                "gpt-5.4-mini",
                ["low", "low", "medium", "high", "xhigh", "xhigh"],
            ),
            (
                "gpt-5.5",
                ["low", "low", "medium", "high", "xhigh", "xhigh"],
            ),
            // medium|high|xhigh — the request never goes under the floor
            (
                "gpt-5.2-pro",
                ["medium", "medium", "medium", "high", "xhigh", "xhigh"],
            ),
            (
                "gpt-5.4-pro",
                ["medium", "medium", "medium", "high", "xhigh", "xhigh"],
            ),
            (
                "gpt-5.5-pro",
                ["medium", "medium", "medium", "high", "xhigh", "xhigh"],
            ),
            // medium only
            (
                "gpt-5.2-chat-latest",
                ["medium", "medium", "medium", "medium", "medium", "medium"],
            ),
            // none…max — the only family that takes the two deep tiers
            ("gpt-5.6", ["low", "low", "medium", "high", "xhigh", "max"]),
            (
                "gpt-5.6-sol",
                ["low", "low", "medium", "high", "xhigh", "max"],
            ),
            // low|medium|high
            ("o3", ["low", "low", "medium", "high", "high", "high"]),
            ("o3-mini", ["low", "low", "medium", "high", "high", "high"]),
            ("o4-mini", ["low", "low", "medium", "high", "high", "high"]),
            ("o1-pro", ["low", "low", "medium", "high", "high", "high"]),
            // unknown to the table
            (
                "some-proxy-model",
                ["low", "low", "medium", "high", "high", "high"],
            ),
        ];
        for (model, expected) in table {
            for (level, want) in LEVELS.iter().zip(expected) {
                assert_eq!(
                    effort_for(model, *level),
                    *want,
                    "{model} at {level:?} must clamp to {want}"
                );
            }
        }
    }

    #[test]
    fn a_dated_snapshot_reads_its_familys_row() {
        assert_eq!(effort_for("gpt-5.6-2026-02-11", Effort::Max), "max");
        assert_eq!(effort_for("gpt-5-2025-08-07", Effort::Minimal), "minimal");
        assert_eq!(effort_for("gpt-5.4-2026-01-30", Effort::Max), "xhigh");
    }

    #[test]
    fn no_family_ever_answers_with_reasoning_turned_off() {
        for (model, _) in FAMILIES {
            for level in LEVELS {
                assert_ne!(effort_for(model, level), NONE, "{model} at {level:?}");
            }
        }
    }

    #[test]
    fn every_row_offers_a_level_and_lists_it_ascending() {
        for (prefix, values) in FAMILIES.iter().chain([&("", UNKNOWN)]) {
            assert!(
                values.iter().any(|value| *value != NONE),
                "{prefix} has no usable level"
            );
            let ranks: Vec<usize> = values.iter().map(|value| rank(value)).collect();
            assert!(
                ranks.windows(2).all(|pair| pair[0] < pair[1]),
                "{prefix} is not ascending: {values:?}"
            );
            assert!(
                ranks.iter().all(|r| *r < LADDER.len()),
                "{prefix} names a level the api does not: {values:?}"
            );
        }
    }
}
