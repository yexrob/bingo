//! The commands that never end on their own.
//!
//! A foreground call for one of these is always a mistake: the turn would wait
//! for something that will not happen, and the best it could hope for is the
//! timeout. So they are started in the background whatever the call said, with
//! a note giving the reason (ADR-0018 §5) — failing closed costs nothing, and
//! the model reads the note and pulls the output when it wants it.
//!
//! The table is pure and reads the command as words, the way [`crate::reject`]
//! does: no filesystem, no environment, no process. It stays narrow on
//! purpose. Backgrounding something a caller meant to wait for is the more
//! surprising mistake of the two, so a shape it is not sure about is left
//! alone.

use crate::reject::tokenise;

/// Why this command can never finish on its own, if it cannot.
pub fn reason(command: &str) -> Option<String> {
    if detached(command) {
        return Some("it ends in `&`, so the shell returns before the work does".into());
    }
    let words = tokenise(command);
    let words: Vec<&str> = words.iter().map(|word| bare(word)).collect();
    match words.first().copied()? {
        "watch" => Some("`watch` repeats until something stops it".into()),
        "tail" if follows(&words[1..]) => {
            Some("`tail -f` follows its file until something stops it".into())
        }
        keyword @ ("while" | "until") if forever(keyword, words.get(1).copied()) => {
            Some("the loop's condition never ends it".into())
        }
        "for" if unbounded(command) => Some("the `for` loop has no condition to end on".into()),
        _ => None,
    }
}

/// A word as the table reads it: the shell's own `;` is not part of it.
fn bare(word: &str) -> &str {
    word.trim_end_matches(';')
}

/// A trailing `&` puts the work in the shell's own background, so the command
/// this tool waits for is over before the work is. `&&` is a conjunction.
fn detached(command: &str) -> bool {
    let line = command.trim_end();
    line.ends_with('&') && !line.ends_with("&&")
}

/// A loop whose condition can never end it. `while false` ends before it
/// starts and `until true` never runs, so neither is one of these.
fn forever(keyword: &str, condition: Option<&str>) -> bool {
    match keyword {
        "while" => matches!(condition, Some("true" | ":")),
        _ => matches!(condition, Some("false")),
    }
}

/// `for (( ; ; ))` with nothing to stop it. The words a C-style `for` is made
/// of are punctuation, so the shape is read off the text.
fn unbounded(command: &str) -> bool {
    let squeezed: String = command.chars().filter(|c| !c.is_whitespace()).collect();
    squeezed.contains("((;;))")
}

/// `-f`, `-F`, `--follow[=…]`, or a short-flag cluster holding `f`/`F`.
fn follows(args: &[&str]) -> bool {
    args.iter().any(|arg| {
        arg.starts_with("--follow")
            || (arg.starts_with('-') && !arg.starts_with("--") && arg.contains(['f', 'F']))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every row: a command, and whether it can never finish.
    const ENDLESS: &[(&str, bool)] = &[
        ("watch ls", true),
        ("watch -n 2 ls", true),
        ("tail -f app.log", true),
        ("tail -F app.log", true),
        ("tail --follow=name app.log", true),
        ("tail -fn 10 app.log", true),
        ("tail -n 20 app.log", false),
        ("while true; do echo hi; done", true),
        ("while :; do sleep 1; done", true),
        ("until false; do sleep 1; done", true),
        ("while false; do echo never; done", false),
        ("while read line; do echo $line; done", false),
        ("for (( ; ; )); do sleep 1; done", true),
        ("for ((;;)); do sleep 1; done", true),
        ("for i in 1 2 3; do echo $i; done", false),
        ("npm start &", true),
        ("echo one && echo two", false),
        ("echo watch", false),
        ("cargo watch", false),
        ("cargo build", false),
        ("", false),
        ("   ", false),
    ];

    #[test]
    fn the_table_answers_every_row() {
        for (command, endless) in ENDLESS {
            assert_eq!(
                reason(command).is_some(),
                *endless,
                "{command:?} -> {:?}",
                reason(command)
            );
        }
    }

    #[test]
    fn every_reason_says_what_will_not_end() {
        assert!(reason("watch ls").is_some_and(|r| r.contains("`watch`")));
        assert!(reason("tail -f x").is_some_and(|r| r.contains("`tail -f`")));
        assert!(reason("while true; do sleep 1; done").is_some_and(|r| r.contains("condition")));
        assert!(reason("npm start &").is_some_and(|r| r.contains("`&`")));
    }

    /// A pipeline is judged by its first word, as the reject tables are: this
    /// one leans towards leaving a command alone.
    #[test]
    fn a_shape_the_table_is_unsure_of_is_left_alone() {
        assert_eq!(reason("ls | tail -f"), None);
        assert_eq!(reason("sleep 30"), None);
    }
}
