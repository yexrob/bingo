//! The unified diff a change to an entry shows twice: once on the
//! permission card, once in the result every surface renders. One
//! rendering, so what a person approved is what they later read.

use std::path::Path;

use similar::{Algorithm, TextDiff};

pub fn unified(path: &Path, old: &str, new: &str) -> String {
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
    fn a_new_file_is_all_additions_under_its_name() {
        let unified = unified(
            Path::new("/store/ab12.json"),
            "",
            "{\n  \"spec\": \"every 30m\"\n}\n",
        );
        assert!(unified.starts_with("--- /store/ab12.json\n+++ /store/ab12.json\n"));
        assert!(unified.contains("+  \"spec\": \"every 30m\""), "{unified}");
    }

    #[test]
    fn nothing_changed_is_no_diff_at_all() {
        assert_eq!(unified(Path::new("a"), "same\n", "same\n"), "");
    }
}
