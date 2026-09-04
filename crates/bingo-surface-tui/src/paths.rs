//! Paths as a person reads them (`docs/design/tui.md` §4): relative inside the
//! working directory, `~` for home, middle-elided past [`MAX`] cells. Pure —
//! the working directory comes from the session's own summary and home from
//! the environment once, so every row that names a file names it the same way.

use std::sync::OnceLock;

use unicode_width::UnicodeWidthStr;

/// Past this many cells a path is elided in the middle, which leaves room for
/// the rest of the row it sits in at 80 columns.
pub const MAX: usize = 48;

/// The mark an elision leaves, in whichever alphabet the look is drawn in.
fn gap() -> &'static str {
    crate::theme::ellipsis()
}

/// The one path in the row a person is reading.
pub fn short(path: &str, cwd: &str, home: Option<&str>) -> String {
    elide(&nearest(path, cwd, home), MAX)
}

/// Every path-shaped word in a line of prose — a tool's summary, a permission's
/// question — shortened where it stands.
pub fn shorten_in(text: &str, cwd: &str, home: Option<&str>) -> String {
    text.split(' ')
        .map(|word| match looks_like_a_path(word) {
            true => short(word, cwd, home),
            false => word.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A path as this machine spells it, from the way it is written down: `~` is
/// home. The inverse of the `~` [`short`] writes, so a path a person or a
/// model wrote comes back through here before anything reads it.
pub fn expand(word: &str, home: Option<&str>) -> String {
    let Some(home) = home else {
        return word.to_string();
    };
    match word.strip_prefix('~') {
        Some("") => home.to_string(),
        Some(rest) if rest.starts_with('/') => format!("{home}{rest}"),
        // `~other` is another person's home, which this does not know how to
        // find; it stays the word it is and is not there.
        _ => word.to_string(),
    }
}

/// The home directory, read once. A surface that cannot see one just prints
/// absolute paths.
pub fn home() -> Option<&'static str> {
    static HOME: OnceLock<Option<String>> = OnceLock::new();
    HOME.get_or_init(|| std::env::var("HOME").ok().filter(|h| !h.is_empty()))
        .as_deref()
}

fn looks_like_a_path(word: &str) -> bool {
    word.contains('/') || word.starts_with('~')
}

/// The shortest true name: inside the working directory it is relative to it,
/// inside home it wears a `~`, and anywhere else it stays whole.
fn nearest(path: &str, cwd: &str, home: Option<&str>) -> String {
    if path == cwd {
        return ".".to_string();
    }
    if let Some(rest) = under(path, cwd) {
        return rest;
    }
    let Some(home) = home else {
        return path.to_string();
    };
    if path == home {
        return "~".to_string();
    }
    match under(path, home) {
        Some(rest) => format!("~/{rest}"),
        None => path.to_string(),
    }
}

fn under(path: &str, root: &str) -> Option<String> {
    let root = root.trim_end_matches('/');
    if root.is_empty() {
        return None;
    }
    Some(path.strip_prefix(root)?.strip_prefix('/')?.to_string())
}

/// Drop whole directories from the middle before cutting a name in half: the
/// first segment says where it started, the last says what it is.
fn elide(path: &str, max: usize) -> String {
    if path.width() <= max {
        return path.to_string();
    }
    let (head, rest) = path.split_once('/').unwrap_or(("", path));
    let mut kept = String::new();
    for segment in rest.rsplit('/') {
        let candidate = match kept.is_empty() {
            true => segment.to_string(),
            false => format!("{segment}/{kept}"),
        };
        if width_of(head, &candidate) > max {
            break;
        }
        kept = candidate;
    }
    match kept.is_empty() {
        true => cut(path, max),
        false => format!("{head}/{}/{kept}", gap()),
    }
}

fn width_of(head: &str, kept: &str) -> usize {
    head.width() + gap().width() + kept.width() + 2
}

/// A single name longer than the whole budget: keep both of its ends.
fn cut(text: &str, max: usize) -> String {
    let keep = max.saturating_sub(gap().width());
    let head: String = clip(text.chars(), keep / 2);
    let tail: String = clip(text.chars().rev(), keep - keep / 2)
        .chars()
        .rev()
        .collect();
    format!("{head}{}{tail}", gap())
}

fn clip(chars: impl Iterator<Item = char>, cells: usize) -> String {
    let mut out = String::new();
    for c in chars {
        if out.width() + c.to_string().width() > cells {
            break;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CWD: &str = "/tmp/project";
    const HOME: &str = "/Users/ada";

    fn short_path(path: &str) -> String {
        short(path, CWD, Some(HOME))
    }

    /// The way back: the `~` [`short`] writes, read again as a real path.
    /// What it does *not* do is the other half of that reading — a relative
    /// path is in the session's own directory, which is the caller's to join.
    #[test]
    fn the_tilde_a_path_is_shown_with_reads_back_as_home() {
        for path in ["/Users/ada/notes.md", "/Users/ada"] {
            assert_eq!(expand(&short_path(path), Some(HOME)), path);
        }
        assert_eq!(expand("~/a.png", Some(HOME)), "/Users/ada/a.png");
        assert_eq!(expand("docs/a.png", Some(HOME)), "docs/a.png");
        assert_eq!(
            expand("~other/a.png", Some(HOME)),
            "~other/a.png",
            "not ours"
        );
        assert_eq!(expand("~/a.png", None), "~/a.png", "no home, no expansion");
    }

    #[test]
    fn a_path_is_named_from_the_nearest_root_a_person_knows() {
        let cases = [
            ("/tmp/project/src/lib.rs", "src/lib.rs"),
            ("/tmp/project", "."),
            ("/Users/ada/notes.md", "~/notes.md"),
            ("/Users/ada", "~"),
            ("/etc/hosts", "/etc/hosts"),
            ("src/lib.rs", "src/lib.rs"),
            ("/tmp/projector/x", "/tmp/projector/x"),
        ];
        for (path, expected) in cases {
            assert_eq!(short_path(path), expected, "{path}");
        }
    }

    #[test]
    fn a_long_path_keeps_its_ends_and_loses_its_middle() {
        let long = "/tmp/project/crates/bingo-surface-tui/src/snapshots/transcript.rs";
        assert_eq!(short_path(long), "crates/…/src/snapshots/transcript.rs");
        assert!(short_path(long).width() <= MAX);
        assert_eq!(
            short_path("/tmp/project/crates/bingo-surface-tui/src/transcript.rs"),
            "crates/bingo-surface-tui/src/transcript.rs",
            "what fits is left whole"
        );
    }

    #[test]
    fn a_path_of_a_hundred_and_twenty_cells_fits_one_row_at_eighty_columns() {
        let deep = format!("/elsewhere/{}/file.rs", "segment/".repeat(13));
        assert!(deep.width() > 120, "the fixture is long enough");
        let short = short_path(&deep);
        assert!(short.width() <= MAX, "{short} is {} cells", short.width());
        assert!(short.ends_with("file.rs"), "{short}");
    }

    #[test]
    fn one_unbreakable_name_is_cut_through_the_middle() {
        let name = "a".repeat(80);
        let cut = short(&name, CWD, None);
        assert_eq!(cut.width(), MAX);
        assert!(cut.contains(gap()), "{cut}");
        assert!(cut.starts_with("aaa") && cut.ends_with("aaa"));
    }

    #[test]
    fn only_the_path_shaped_words_of_a_summary_are_touched() {
        assert_eq!(
            shorten_in("Write /tmp/project/note.txt", CWD, Some(HOME)),
            "Write note.txt"
        );
        assert_eq!(
            shorten_in("Bash cargo test --workspace", CWD, Some(HOME)),
            "Bash cargo test --workspace",
            "a command is not a path"
        );
    }

    #[test]
    fn a_wide_glyph_counts_two_cells_when_a_name_is_cut() {
        let name = "名".repeat(40);
        let cut = short(&name, CWD, None);
        assert!(cut.width() <= MAX, "{cut} is {} cells", cut.width());
    }
}
