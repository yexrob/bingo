//! What a filesystem result may cost the context. Every tool here bounds its
//! own output twice — at a number of entries and at a character cap — so the
//! kernel's global clip never has to.

/// The rendered text a single call may cost.
pub(crate) const MAX_CHARS: usize = 20_000;

/// Join rendered lines under both bounds: at most `max` entries, at most
/// `MAX_CHARS` characters. The note counts what the model did not get, in the
/// caller's noun ("lines", "files").
pub(crate) fn join(lines: &[String], max: usize, noun: &str) -> String {
    let mut out = String::new();
    let mut chars = 0;
    let mut taken = 0;
    for line in lines.iter().take(max) {
        let cost = line.chars().count() + usize::from(!out.is_empty());
        if chars + cost > MAX_CHARS {
            break;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
        chars += cost;
        taken += 1;
    }
    let dropped = lines.len() - taken;
    if dropped > 0 {
        out.push_str(&format!("\n[truncated: {dropped} more {noun}]"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("line {i}")).collect()
    }

    #[test]
    fn everything_fits_when_it_fits() {
        assert_eq!(join(&lines(2), usize::MAX, "lines"), "line 0\nline 1");
        assert_eq!(join(&[], usize::MAX, "files"), "");
    }

    #[test]
    fn the_entry_cap_counts_what_it_dropped() {
        assert_eq!(
            join(&lines(5), 2, "files"),
            "line 0\nline 1\n[truncated: 3 more files]"
        );
    }

    #[test]
    fn the_character_cap_counts_what_it_dropped() {
        let long: Vec<String> = (0..4_000).map(|i| format!("{i:>10}")).collect();
        let out = join(&long, usize::MAX, "lines");
        let note = out.lines().last().expect("a last line");
        assert!(note.starts_with("[truncated: "), "got {note}");
        assert!(note.ends_with(" more lines]"), "got {note}");
        let body = out.rsplit_once('\n').map(|(head, _)| head).unwrap_or("");
        assert!(body.chars().count() <= MAX_CHARS);
    }
}
