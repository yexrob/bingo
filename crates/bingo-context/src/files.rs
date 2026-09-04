//! A file on its way into the prompt: capped from the top, never in silence.

use bingo_sdk::SystemBlock;

use crate::tail;

/// Lines a contributed file may spend. Past this it is a document, and the
/// newest lines of a document are the ones still true. What a caller spends
/// is the caller's: an index is not a file, and says so with a smaller cap.
pub const MAX_LINES: usize = 300;

/// Bytes a contributed file may spend, for a file whose lines are long.
pub const MAX_BYTES: usize = 32_768;

/// The newest lines that fit both caps, and how many earlier lines they cost.
/// A file under both caps comes back byte for byte.
pub fn capped(text: &str, max_lines: usize, max_bytes: usize) -> (String, usize) {
    let lines: Vec<&str> = text.lines().collect();
    let by_lines = lines.len().saturating_sub(max_lines);
    let by_bytes = tail::first_within(&lines, max_bytes as u64, |l| l.len() as u64 + 1);
    let dropped = by_lines.max(by_bytes);
    if dropped == 0 {
        return (text.to_string(), 0);
    }
    (lines[dropped..].join("\n"), dropped)
}

/// One file as a system block: what it is, what was left out, and the rest.
pub fn block(heading: &str, text: &str, cache: bool, max_lines: usize) -> SystemBlock {
    let (kept, dropped) = capped(text, max_lines, MAX_BYTES);
    let mut body = String::new();
    if dropped > 0 {
        body.push_str(&omitted(dropped));
        body.push('\n');
    }
    body.push_str(&kept);
    SystemBlock {
        text: format!("{heading}\n\n{body}"),
        cache,
    }
}

fn omitted(lines: usize) -> String {
    format!("[… {lines} earlier lines not shown]")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered(n: usize) -> String {
        (1..=n)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    }

    #[test]
    fn a_file_under_both_caps_is_unchanged() {
        let text = numbered(10);
        assert_eq!(capped(&text, MAX_LINES, MAX_BYTES), (text.clone(), 0));
    }

    #[test]
    fn four_hundred_lines_contribute_the_last_three_hundred() {
        let (kept, dropped) = capped(&numbered(400), MAX_LINES, MAX_BYTES);
        assert_eq!(dropped, 100);
        assert_eq!(kept.lines().count(), 300);
        assert_eq!(kept.lines().next(), Some("line 101"));
        assert_eq!(kept.lines().last(), Some("line 400"));
    }

    #[test]
    fn a_byte_cap_cutting_mid_file_keeps_whole_newest_lines() {
        // Ten lines of seven bytes each with their newline, and room for three.
        let text = numbered(10);
        let (kept, dropped) = capped(&text, MAX_LINES, 28);
        assert_eq!(dropped, 7);
        assert_eq!(kept, "line 8\nline 9\nline 10");
        assert!(kept.len() <= 28);
    }

    #[test]
    fn a_block_says_how_many_lines_it_left_out() {
        let block = block("# Project memory", &numbered(400), false, MAX_LINES);
        assert!(
            block
                .text
                .starts_with("# Project memory\n\n[… 100 earlier lines not shown]\n")
        );
        assert!(!block.cache);
    }

    #[test]
    fn a_block_that_left_nothing_out_says_nothing() {
        let block = block(
            "# Instructions from /a/AGENTS.md",
            "be brief\n",
            true,
            MAX_LINES,
        );
        assert_eq!(block.text, "# Instructions from /a/AGENTS.md\n\nbe brief\n");
        assert!(block.cache);
    }
}
