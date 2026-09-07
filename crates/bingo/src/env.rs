//! Where a `BINGO_*` variable is spelled: never in this binary.
//!
//! Each of the twenty-one this tree reads is a named const beside the code
//! that acts on it, in the crate that owns the behaviour —
//! `bingo_provider_acp::bridge::{ADDRESS_VAR, TOKEN_VAR}`,
//! `bingo_provider_fake::SCRIPT_ENV`, the channels' in
//! `bingo_channels::settings`, the terminal's in `bingo-surface-tui`, the
//! browser's in `bingo-loopback`. The binary reads through those consts and
//! writes none of the words itself, so renaming one is an edit rather than a
//! hunt, and a typo here cannot quietly disagree with the reader there.
//!
//! So this module is the rule and not a list: a list would be the second copy
//! of every name, which is the thing being forbidden. What it holds is the
//! check that keeps the rule true, and nothing else.

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    /// A variable's name written out as a string. Test code is cut away
    /// before the search, so the needle below — and this module — is not
    /// itself a hit.
    const SPELLED: &str = "\"BINGO_";

    /// Every `.rs` file under this crate's `src`, in a stable order.
    fn sources() -> Vec<PathBuf> {
        let mut found = Vec::new();
        walk(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut found,
        );
        found.sort();
        found
    }

    fn walk(dir: PathBuf, found: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(&dir).expect("this crate's own source") {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                walk(path, found);
            } else if path.extension().is_some_and(|kind| kind == "rs") {
                found.push(path);
            }
        }
    }

    /// A file's code, with its test module cut off. A test may name the
    /// variable it is asserting about, the cut `scripts/check_discipline.sh`
    /// makes for the same reason.
    fn code(path: &Path) -> String {
        let source = std::fs::read_to_string(path).expect("a source file");
        match source.split_once("#[cfg(test)]") {
            Some((before, _)) => before.to_string(),
            None => source,
        }
    }

    #[test]
    fn no_environment_variable_is_named_in_the_binary() {
        let sources = sources();
        assert!(
            sources.iter().any(|path| path.ends_with("main.rs")),
            "the search found no source to read"
        );
        let spelled: Vec<String> = sources
            .iter()
            .flat_map(|path| {
                code(path)
                    .lines()
                    .enumerate()
                    .filter(|(_, line)| line.contains(SPELLED))
                    .map(|(n, line)| format!("{}:{}: {}", path.display(), n + 1, line.trim()))
                    .collect::<Vec<_>>()
            })
            .collect();
        assert!(
            spelled.is_empty(),
            "a BINGO_* name belongs to the crate that reads it, as a const \
             this binary imports:\n{}",
            spelled.join("\n")
        );
    }
}
