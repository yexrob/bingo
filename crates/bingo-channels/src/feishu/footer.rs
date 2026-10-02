//! The final line of a Feishu answer, derived from the session's folded facts.

use bingo_sdk::{Effort, SessionState, Usage};

use crate::limits::Limits;

const SEPARATOR: &str = "\n\n---\n";

pub(super) fn append(text: &str, state: &SessionState, limits: &Limits) -> String {
    if text.is_empty() {
        return String::new();
    }
    let footer = line(state);
    let mut answer_limits = limits.clone();
    answer_limits.max_text.0 = answer_limits
        .max_text
        .0
        .saturating_sub(SEPARATOR.len() + footer.len());
    let answer = clipped_answer(text, &answer_limits);
    format!("{answer}{SEPARATOR}{footer}")
}

/// The footer must sit outside code even when a cut removes its closing fence.
fn clipped_answer(text: &str, limits: &Limits) -> String {
    let mut room = limits.clone();
    loop {
        if room.max_text.0 < '…'.len_utf8() {
            return String::new();
        }
        let answer = room.clip(text);
        let Some(fence) = open_fence(&answer) else {
            return answer.into_owned();
        };
        let closing = format!("\n{fence}");
        if answer.len() + closing.len() <= limits.max_text.0 {
            return format!("{answer}{closing}");
        }
        // The smaller cut can cross a delimiter, so read its fence again.
        room.max_text.0 = limits.max_text.0.saturating_sub(closing.len());
    }
}

fn open_fence(text: &str) -> Option<&str> {
    let mut open: Option<&str> = None;
    for line in text.lines() {
        let start = line.trim_start_matches(' ');
        if line.len() - start.len() > 3 {
            continue;
        }
        let Some(marker @ (b'`' | b'~')) = start.bytes().next() else {
            continue;
        };
        let width = start.bytes().take_while(|byte| *byte == marker).count();
        if width < 3 {
            continue;
        }
        let (fence, rest) = start.split_at(width);
        match open {
            Some(opening)
                if opening.as_bytes()[0] == marker
                    && width >= opening.len()
                    && rest.trim_matches([' ', '\t']).is_empty() =>
            {
                open = None
            }
            None if marker != b'`' || !rest.contains('`') => open = Some(fence),
            _ => {}
        }
    }
    open
}

fn line(state: &SessionState) -> String {
    let mut parts = vec![
        state
            .summary
            .model
            .as_deref()
            .unwrap_or("bingo")
            .to_string(),
    ];
    if let Some(level) = state
        .config
        .kernel
        .get("thinking")
        .and_then(|value| serde_json::from_value::<Effort>(value.clone()).ok())
    {
        parts.push(format!("effort:{}", level.name()));
    }
    let usage = state
        .turn
        .as_ref()
        .map(|turn| turn.usage)
        .or_else(|| state.last_turn.as_ref().map(|turn| turn.usage));
    if let Some(usage) = usage.filter(|usage| *usage != Usage::default()) {
        parts.push(format!("out {}", count(usage.output_tokens)));
        parts.push(format!(
            "in {} cw {} cr {}",
            count(usage.input_total()),
            count(usage.cache_write_tokens),
            count(usage.cache_read_tokens)
        ));
    }
    if let Some(context) = state.context.filter(|context| context.window > 0) {
        parts.push(format!("ctx {}%", context.percent()));
    }
    parts.join(" · ")
}

fn count(value: u64) -> String {
    match value {
        1_000_000.. => short(value, 1_000_000, "m"),
        1_000.. => short(value, 1_000, "k"),
        _ => value.to_string(),
    }
}

fn short(value: u64, unit: u64, suffix: &str) -> String {
    let tenths = value / (unit / 10);
    match tenths % 10 {
        0 => format!("{}{suffix}", tenths / 10),
        decimal => format!("{}.{decimal}{suffix}", tenths / 10),
    }
}

#[cfg(test)]
mod tests {
    use bingo_sdk::{ContextUsage, Event, TurnId, TurnStatus};
    use serde_json::json;

    use super::*;
    use crate::fixtures;
    use crate::limits::{Dialect, Encoding};

    fn limits(max: usize) -> Limits {
        Limits {
            max_text: (max, Encoding::Utf8Bytes),
            dialect: Dialect::Markdown,
            max_actions: 4,
            max_label: 30,
        }
    }

    #[test]
    fn a_completed_turn_shows_its_model_effort_usage_and_context() {
        let mut state = fixtures::state();
        state.summary.model = Some("gpt-6-sol".into());
        state.config.kernel = json!({ "thinking": "xHigh" });
        state.apply(&fixtures::turn_started(1));
        state.apply(&fixtures::frame(
            2,
            Event::TurnUsage {
                turn: TurnId::from_raw(fixtures::TURN),
                usage: Usage::default(),
                context: ContextUsage {
                    used: 150_000,
                    window: 200_000,
                    trigger: 180_000,
                },
            },
        ));
        state.apply(&fixtures::frame(
            3,
            Event::TurnCompleted {
                turn: TurnId::from_raw(fixtures::TURN),
                status: TurnStatus::Completed,
                usage: Usage {
                    input_tokens: 400,
                    output_tokens: 328,
                    cache_write_tokens: 0,
                    cache_read_tokens: 193_000,
                    reasoning_tokens: 0,
                },
            },
        ));
        assert_eq!(
            append("Done.", &state, &limits(20_000)),
            "Done.\n\n---\ngpt-6-sol · effort:xhigh · out 328 · in 193.4k cw 0 cr 193k · ctx 75%"
        );
        assert_eq!(
            append("已经完成。", &state, &limits(20_000)),
            "已经完成。\n\n---\ngpt-6-sol · effort:xhigh · out 328 · in 193.4k cw 0 cr 193k · ctx 75%"
        );
    }

    #[test]
    fn footer_labels_are_english_whatever_the_answer_language_or_code() {
        let mut state = fixtures::state();
        state.config.kernel = json!({ "thinking": "high" });
        for text in [
            "Tests passed. `你好` means hello.",
            "测试完成，`cargo test` 全部通过。",
            "Great!\n```sh\necho 中文\n```",
            "Done. ``你好你好你好你好``",
        ] {
            assert_eq!(
                append(text, &state, &limits(20_000)),
                format!("{text}\n\n---\nfake-1 · effort:high")
            );
        }
    }

    #[test]
    fn a_live_turn_does_not_use_the_previous_turns_usage() {
        let mut state = fixtures::state();
        state.apply(&fixtures::turn_started(1));
        state.apply(&fixtures::frame(
            2,
            Event::TurnCompleted {
                turn: TurnId::from_raw(fixtures::TURN),
                status: TurnStatus::Completed,
                usage: Usage {
                    output_tokens: 20,
                    ..Usage::default()
                },
            },
        ));
        state.apply(&fixtures::turn_started(3));
        state.config.kernel = json!({ "thinking": null });
        state.context = Some(ContextUsage {
            used: 1,
            window: 0,
            trigger: 0,
        });
        assert_eq!(
            append("Question?", &state, &limits(20_000)),
            "Question?\n\n---\nfake-1"
        );
        assert_eq!(append("", &state, &limits(20_000)), "");
    }

    #[test]
    fn the_footer_stays_outside_a_valid_fenced_answer_at_the_limit() {
        let state = fixtures::state();
        let text = format!("```text\n{}\n```", "x".repeat(19_988));
        assert_eq!(text.len(), 20_000);
        let result = append(&text, &state, &limits(20_000));
        assert!(result.len() <= 20_000);
        let (answer, _) = result.rsplit_once(SEPARATOR).unwrap();
        assert!(
            answer.ends_with("\n```"),
            "footer is inside an unclosed code fence"
        );
    }

    #[test]
    fn complete_fences_are_unchanged_and_clipped_fences_are_closed() {
        let state = fixtures::state();
        for fence in ["```", "`````", "~~~", "~~~~~~"] {
            let text = format!("{fence}text\n{}\n{fence}", "你好🙂".repeat(20));
            assert_eq!(
                append(&text, &state, &limits(20_000)),
                format!("{text}{SEPARATOR}fake-1")
            );
            let result = append(&text, &state, &limits(100));
            assert!(result.len() <= 100);
            assert!(result.ends_with(&format!("\n{fence}{SEPARATOR}fake-1")));
        }
    }

    #[test]
    fn clipping_through_fence_delimiters_never_encloses_the_footer() {
        let state = fixtures::state();
        for fence in ["```", "`````", "~~~", "~~~~~~"] {
            let text = format!("prefix\n{fence}text\n你好🙂\n{fence}\nend");
            for max in 15..=text.len() + 12 {
                let result = append(&text, &state, &limits(max));
                let (answer, footer) = result.rsplit_once(SEPARATOR).unwrap();
                assert_eq!(footer, "fake-1");
                assert!(result.len() <= max, "max {max}: {result:?}");
                assert_eq!(open_fence(answer), None, "max {max}: {result:?}");
            }
        }
    }

    #[test]
    fn a_fence_closes_only_with_its_own_marker_and_enough_characters() {
        assert_eq!(
            open_fence("````rust\n```\n~~~\n````` trailing"),
            Some("````")
        );
        assert_eq!(open_fence("````rust\n```\n  `````  \n"), None);
        assert_eq!(open_fence("~~~text\n```\n~~\n~~~"), None);
        assert_eq!(open_fence("   ~~~~text\n~~~"), Some("~~~~"));
        assert_eq!(open_fence("    ```\nindented code"), None);
        assert_eq!(open_fence("```not ` a fence"), None);
        assert_eq!(open_fence("```\ncode\n```\u{a0}"), Some("```"));
    }

    #[test]
    fn a_partial_answer_is_closed_even_without_clipping() {
        let state = fixtures::state();
        assert_eq!(
            append("```rust\nlet x = 1;", &state, &limits(20_000)),
            "```rust\nlet x = 1;\n```\n\n---\nfake-1"
        );
    }

    #[test]
    fn a_long_answer_keeps_the_footer_inside_feishus_text_limit() {
        let state = fixtures::state();
        let result = append(&"a".repeat(20_000), &state, &limits(60));
        assert!(result.len() <= 60);
        assert!(result.ends_with("\n\n---\nfake-1"));
        assert!(result.starts_with('a'));
    }
}
