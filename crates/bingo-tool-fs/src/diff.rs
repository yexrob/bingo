//! The unified diff a change to a file shows twice: once in the approval
//! preview, once in the result every surface renders. One rendering, so what
//! a person approved is what they later read.

use std::path::Path;

use similar::{Algorithm, TextDiff};

/// Histogram, as git resolves line diffs: it lines moved blocks up the way a
/// reader expects.
pub(crate) fn unified(path: &Path, old: &str, new: &str) -> String {
    let shown = path.display().to_string();
    let diff = TextDiff::configure()
        .algorithm(Algorithm::Histogram)
        .diff_lines(old, new);
    diff.unified_diff().header(&shown, &shown).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_changed_line_is_a_hunk_under_the_file_name() {
        let unified = unified(Path::new("/work/a.txt"), "one\ntwo\n", "one\ntoo\n");
        assert!(unified.starts_with("--- /work/a.txt\n+++ /work/a.txt\n"));
        assert!(unified.contains("-two\n"), "got {unified}");
        assert!(unified.contains("+too\n"), "got {unified}");
    }

    #[test]
    fn nothing_changed_is_no_diff_at_all() {
        assert_eq!(unified(Path::new("a"), "same\n", "same\n"), "");
    }
}
