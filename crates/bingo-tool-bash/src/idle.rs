//! The command that only waits.
//!
//! A foreground `sleep` holds the turn — and the person with it — for as long
//! as it says, and answers with nothing: it is the model's way of waiting for
//! something the harness already waits for on its behalf. A background job or
//! agent opens a turn when it ends (ADR-0018 §4), so the wait it stands in for
//! is the turn's end, and the call is refused with that said. A `sleep` the
//! model wants as a timer is the same call in the background: a job like any
//! other, whose completion wakes the session.
//!
//! The table is pure and reads the command as words, the way [`crate::reject`]
//! does. It refuses only the bare shape — `sleep` followed by durations and
//! nothing else — and leaves `sleep 2 && curl …` alone, because a check that
//! must follow a pause belongs in one call.

use crate::reject::tokenise;

/// Why this command only waits, if that is all it does.
pub fn reason(command: &str) -> Option<String> {
    let words = tokenise(command);
    let (name, durations) = words.split_first()?;
    if name != "sleep" || durations.is_empty() || !durations.iter().all(|w| is_duration(w)) {
        return None;
    }
    Some(format!(
        "`{}` in the foreground only holds the turn, and the person with it, until it ends; \
         rejected. A check that must follow a pause goes in one call: `{} && <command>`. A \
         background job or agent wakes you when it finishes, so the way to wait for one is to \
         end your turn. To be woken after a delay instead, run this same `sleep` with \
         `background: true`: it becomes a job, and its completion opens a turn.",
        command.trim(),
        command.trim()
    ))
}

/// A number, with or without a fraction, and with or without the suffix GNU
/// `sleep` takes: `30`, `0.5`, `2m`.
fn is_duration(word: &str) -> bool {
    let number = word.strip_suffix(['s', 'm', 'h', 'd']).unwrap_or(word);
    !number.is_empty()
        && number.chars().all(|c| c.is_ascii_digit() || c == '.')
        && number.chars().filter(|c| *c == '.').count() <= 1
        && number.chars().any(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every row: a command, and whether it only waits.
    const IDLE: &[(&str, bool)] = &[
        ("sleep 30", true),
        ("sleep 0.5", true),
        ("sleep 2m", true),
        ("sleep 1 2", true),
        ("  sleep 30  ", true),
        ("sleep", false),
        ("sleep $n", false),
        ("sleep 30 && curl localhost:8080", false),
        ("sleep 30; echo done", false),
        ("echo waiting; sleep 30", false),
        ("sleep 30 | cat", false),
        ("sleep 30 &", false),
        ("sleepy 30", false),
        ("cargo build", false),
        ("", false),
    ];

    #[test]
    fn the_table_answers_every_row() {
        for (command, idle) in IDLE {
            assert_eq!(
                reason(command).is_some(),
                *idle,
                "{command:?} -> {:?}",
                reason(command)
            );
        }
    }

    #[test]
    fn the_reason_names_the_command_and_every_way_round_it() {
        let reason = reason("sleep 30").expect("refused");
        assert!(reason.starts_with("`sleep 30`"), "{reason}");
        assert!(reason.contains("end your turn"), "{reason}");
        assert!(reason.contains("`sleep 30 && <command>`"), "{reason}");
        assert!(reason.contains("`background: true`"), "{reason}");
    }
}
