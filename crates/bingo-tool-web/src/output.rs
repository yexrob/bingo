//! What one page may cost the context. A fetch bounds its own result, so the
//! kernel's global clip never has to, and what it dropped is written where the
//! model reads last.

/// The rendered text a single fetch may cost.
pub(crate) const MAX_CHARS: usize = 100_000;

/// Keep the first `MAX_CHARS` characters, counting the rest on the last line.
/// Characters, not bytes: a cut inside a multi-byte character is not text.
pub(crate) fn cap(text: &str) -> String {
    let total = text.chars().count();
    if total <= MAX_CHARS {
        return text.to_string();
    }
    let kept: String = text.chars().take(MAX_CHARS).collect();
    format!("{kept}\n[truncated: {} more characters]", total - MAX_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_fits_is_returned_unchanged() {
        assert_eq!(cap("# Title\n\ntext"), "# Title\n\ntext");
        assert_eq!(cap(""), "");
    }

    #[test]
    fn what_does_not_fit_is_cut_and_counted() {
        let long = "a".repeat(MAX_CHARS + 25);
        let out = cap(&long);
        let note = out.lines().last().expect("a last line");
        assert_eq!(note, "[truncated: 25 more characters]");
        assert_eq!(out.lines().next().map(str::len), Some(MAX_CHARS));
    }

    #[test]
    fn the_cut_falls_on_a_character_boundary_not_a_byte_one() {
        let long = "é".repeat(MAX_CHARS + 1);
        let out = cap(&long);
        let kept = out.lines().next().expect("a first line");
        assert_eq!(kept.chars().count(), MAX_CHARS);
        assert!(kept.chars().all(|c| c == 'é'));
    }
}
