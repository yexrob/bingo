//! What a command's output costs, and the shape it comes back in.
//!
//! A command can write more than a context window holds, so the collector is
//! bounded as it fills rather than trimmed at the end: it keeps the head, which
//! says what the command started doing, and the tail, which says how it ended,
//! and counts what fell out between them. Nothing here touches a process — the
//! whole module is pure, so the cap and the shape are decided by tests, not by
//! running something and looking.

use std::collections::VecDeque;

use bingo_sdk::ToolOutput;

/// Characters of output one call may spend. Past this the model is reading
/// noise, and a redirect to a file plus `Read` is the cheaper move.
pub const MAX_OUTPUT_CHARS: usize = 48_000;

/// Longest line the live tail carries. A stream with no newlines in it (`cat` on
/// a binary) must not cost more than a screen.
const MAX_TAIL_LINE_CHARS: usize = 512;

/// A command's output, bounded while it is collected: the first `head_max`
/// characters, the last `tail_max`, and the count of everything that arrived.
#[derive(Debug)]
pub struct Bounded {
    head: String,
    head_max: usize,
    tail: VecDeque<char>,
    tail_max: usize,
    total: usize,
}

impl Bounded {
    /// Keep at most `max` characters, split evenly between the head and the tail.
    pub fn new(max: usize) -> Self {
        let head_max = max / 2;
        Self {
            head: String::new(),
            head_max,
            tail: VecDeque::new(),
            tail_max: max - head_max,
            total: 0,
        }
    }

    pub fn push(&mut self, text: &str) {
        for ch in text.chars() {
            if self.total < self.head_max {
                self.head.push(ch);
            }
            if self.tail_max > 0 {
                if self.tail.len() == self.tail_max {
                    self.tail.pop_front();
                }
                self.tail.push_back(ch);
            }
            self.total += 1;
        }
    }

    /// Everything that was kept: the whole output while it fit, otherwise the
    /// head and the tail with a count of what did not.
    pub fn finish(&self) -> String {
        let mut out = self.head.clone();
        let dropped = self.total.saturating_sub(self.head_max + self.tail_max);
        if dropped == 0 {
            let kept_head = self.total.min(self.head_max);
            let overlap = (kept_head + self.tail.len()).saturating_sub(self.total);
            out.extend(self.tail.iter().skip(overlap));
        } else {
            out.push_str(&format!("\n[… {dropped} chars truncated …]\n"));
            out.extend(self.tail.iter());
        }
        out
    }

    /// The last `n` lines collected so far, for the progress tail. The first of
    /// them is a fragment once the output has outgrown the buffer.
    pub fn tail_lines(&self, n: usize) -> String {
        let text: String = self.tail.iter().collect();
        let lines: Vec<&str> = text.lines().collect();
        lines
            .iter()
            .skip(lines.len().saturating_sub(n))
            .map(|line| clip(line))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn clip(line: &str) -> String {
    if line.chars().count() <= MAX_TAIL_LINE_CHARS {
        return line.to_string();
    }
    let kept: String = line.chars().take(MAX_TAIL_LINE_CHARS).collect();
    format!("{kept}…")
}

/// How a command ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    Exited(i32),
    /// The timeout fired and the process group was killed.
    Timeout {
        after_ms: u64,
    },
    /// The turn was interrupted: the group was killed, and this is what it had
    /// produced by then.
    Interrupted,
}

/// The command as it was asked for, what it wrote, and one line saying how it
/// ended. Anything but a clean exit is an error result, because the output is
/// not the answer the model asked for.
pub fn shape(command: &str, output: &str, ended: Ended) -> ToolOutput {
    let footer = match ended {
        Ended::Exited(code) => format!("[Exited with code {code}]"),
        Ended::Timeout { after_ms } => format!("[Killed after {}s timeout]", seconds(after_ms)),
        Ended::Interrupted => "[Killed by the interrupt; output so far]".to_string(),
    };
    let body = output.strip_suffix('\n').unwrap_or(output);
    let text = format!("$ {command}\n{body}\n{footer}");
    if ended == Ended::Exited(0) {
        ToolOutput::text(text)
    } else {
        ToolOutput::error(text)
    }
}

/// Milliseconds as the seconds a person would say: `120000` is `120`, `200` is
/// `0.2`.
fn seconds(ms: u64) -> String {
    let rendered = format!("{:.3}", ms as f64 / 1000.0);
    rendered
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(max: usize, text: &str) -> Bounded {
        let mut bounded = Bounded::new(max);
        bounded.push(text);
        bounded
    }

    #[test]
    fn output_that_fits_comes_back_whole() {
        for text in ["", "a", "hello\nworld\n", &"x".repeat(100)] {
            assert_eq!(collect(100, text).finish(), text, "{text:?}");
        }
    }

    #[test]
    fn many_small_writes_are_one_output() {
        let mut bounded = Bounded::new(100);
        bounded.push("one\n");
        bounded.push("two\n");
        bounded.push("three");
        assert_eq!(bounded.finish(), "one\ntwo\nthree");
    }

    #[test]
    fn output_past_the_cap_keeps_the_head_and_the_tail() {
        let text: String = (0..200)
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect();
        let out = collect(100, &text).finish();
        assert!(out.starts_with(&text[..50]), "{out}");
        assert!(out.ends_with(&text[150..]), "{out}");
        assert!(out.contains("[… 100 chars truncated …]"), "{out}");
    }

    #[test]
    fn the_marker_counts_exactly_what_was_dropped() {
        let text = "y".repeat(1_000);
        let out = collect(100, &text).finish();
        let kept = out.chars().filter(|c| *c == 'y').count();
        assert_eq!(kept, 100);
        assert!(out.contains("[… 900 chars truncated …]"), "{out}");
    }

    #[test]
    fn the_cap_counts_characters_and_never_splits_one() {
        let text = "字".repeat(50);
        let out = collect(10, &text).finish();
        assert_eq!(out.chars().filter(|c| *c == '字').count(), 10);
        assert!(out.contains("[… 40 chars truncated …]"), "{out}");
    }

    #[test]
    fn the_tail_is_the_last_lines() {
        let bounded = collect(1_000, "one\ntwo\nthree\nfour\nfive\nsix\n");
        assert_eq!(bounded.tail_lines(3), "four\nfive\nsix");
        assert_eq!(bounded.tail_lines(50), "one\ntwo\nthree\nfour\nfive\nsix");
    }

    #[test]
    fn a_partial_line_is_still_the_tail() {
        let bounded = collect(1_000, "done\nworking");
        assert_eq!(bounded.tail_lines(2), "done\nworking");
    }

    #[test]
    fn a_line_with_no_end_in_sight_is_clipped() {
        let bounded = collect(100_000, &"z".repeat(2_000));
        let tail = bounded.tail_lines(5);
        assert_eq!(tail.chars().count(), MAX_TAIL_LINE_CHARS + 1);
        assert!(tail.ends_with('…'), "{tail}");
    }

    #[test]
    fn a_clean_exit_is_not_an_error() {
        let out = shape("echo hi", "hi\n", Ended::Exited(0));
        assert_eq!(
            out.parts[0].as_text(),
            Some("$ echo hi\nhi\n[Exited with code 0]")
        );
        assert!(!out.is_error);
    }

    #[test]
    fn a_command_that_wrote_nothing_keeps_the_shape() {
        let out = shape("true", "", Ended::Exited(0));
        assert_eq!(
            out.parts[0].as_text(),
            Some("$ true\n\n[Exited with code 0]")
        );
    }

    #[test]
    fn a_non_zero_exit_is_an_error_and_says_the_code() {
        let out = shape("false", "", Ended::Exited(1));
        assert!(out.is_error);
        assert!(
            out.parts[0]
                .as_text()
                .is_some_and(|t| t.ends_with("[Exited with code 1]"))
        );
    }

    #[test]
    fn a_timeout_says_how_long_it_waited() {
        let out = shape("sleep 5", "", Ended::Timeout { after_ms: 200 });
        assert!(out.is_error);
        assert!(
            out.parts[0]
                .as_text()
                .is_some_and(|t| t.ends_with("[Killed after 0.2s timeout]")),
            "{:?}",
            out.parts[0]
        );
    }

    #[test]
    fn an_interrupt_keeps_the_output_and_says_it_was_cut_short() {
        let out = shape("sleep 5", "tick\n", Ended::Interrupted);
        assert!(out.is_error);
        let text = out.parts[0].as_text().expect("text");
        assert!(text.contains("tick"), "{text}");
        assert!(
            text.ends_with("[Killed by the interrupt; output so far]"),
            "{text}"
        );
    }

    #[test]
    fn seconds_read_the_way_a_person_says_them() {
        for (ms, said) in [
            (0, "0"),
            (200, "0.2"),
            (1_500, "1.5"),
            (120_000, "120"),
            (600_000, "600"),
        ] {
            assert_eq!(seconds(ms), said, "{ms}ms");
        }
    }
}
