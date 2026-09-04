//! `MEMORY.md`: one line per memory, and nothing a memory file does not
//! already hold.
//!
//! The index is the fact *what memories exist*; each file is the fact itself.
//! An entry is therefore a projection of a memory — its title read from the
//! name, its hook the description the file already carries — so a writer
//! invents nothing here that could later disagree with the file.

use crate::memory::file::Memory;

/// One line of the index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub title: String,
    pub slug: String,
    pub hook: String,
}

const OPEN: &str = "- [";
const LINK: &str = "](";
const CLOSE: &str = ".md)";
const DASH: &str = " — ";

/// The line this memory answers to.
pub fn of(memory: &Memory) -> Entry {
    Entry {
        title: title(&memory.name),
        slug: memory.name.clone(),
        hook: memory.description.clone(),
    }
}

/// A slug as a person reads it: the hyphens are spaces and the first letter
/// is a capital.
fn title(name: &str) -> String {
    let words = name.replace('-', " ");
    let mut chars = words.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => words,
    }
}

pub fn line(entry: &Entry) -> String {
    let Entry { title, slug, hook } = entry;
    format!("{OPEN}{title}{LINK}{slug}{CLOSE}{DASH}{hook}")
}

/// The entry one line holds, or nothing: a line that is not an entry is prose
/// somebody put in the index, and prose is not a memory.
pub fn parse(line: &str) -> Option<Entry> {
    let rest = line.trim().strip_prefix(OPEN)?;
    let (title, rest) = rest.split_once(LINK)?;
    let (slug, hook) = rest.split_once(CLOSE)?;
    Some(Entry {
        title: title.trim().to_string(),
        slug: slug.trim().to_string(),
        hook: hook_of(hook).to_string(),
    })
}

/// What follows the link, past the one dash that separates it.
fn hook_of(rest: &str) -> &str {
    let rest = rest.trim_start();
    let rest = rest
        .strip_prefix('—')
        .or_else(|| rest.strip_prefix('-'))
        .unwrap_or(rest);
    rest.trim()
}

/// The index with this entry in it: in place of the line that already names
/// the slug, else appended at the end. Every other line is left exactly as it
/// was, so an entry a writer does not understand outlives the write, and the
/// newest entry is the last — which is the one the prompt's cap keeps.
pub fn with(text: &str, entry: &Entry) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut replaced = false;
    for existing in text.lines() {
        match parse(existing).filter(|e| e.slug == entry.slug) {
            Some(_) => {
                lines.push(line(entry));
                replaced = true;
            }
            None => lines.push(existing.to_string()),
        }
    }
    if !replaced {
        lines.push(line(entry));
    }
    joined(lines)
}

fn joined(lines: Vec<String>) -> String {
    if lines.is_empty() {
        return String::new();
    }
    lines.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::file::Kind;

    /// One index as it sits on disk, beside the memories it names.
    const FIXTURE: &str = "\
- [The build runs cargo test](the-build-runs-cargo-test.md) — how this project is tested
- [Prefers short replies](prefers-short-replies.md) — how the person likes to be answered
";

    /// The index as its entries, and its entries as an index: the two halves
    /// of the round trip the fixture pins.
    fn read(text: &str) -> Vec<Entry> {
        text.lines().filter_map(parse).collect()
    }

    fn write(entries: &[Entry]) -> String {
        joined(entries.iter().map(line).collect())
    }

    fn entry(slug: &str, title: &str, hook: &str) -> Entry {
        Entry {
            title: title.into(),
            slug: slug.into(),
            hook: hook.into(),
        }
    }

    fn fixture() -> Vec<Entry> {
        vec![
            entry(
                "the-build-runs-cargo-test",
                "The build runs cargo test",
                "how this project is tested",
            ),
            entry(
                "prefers-short-replies",
                "Prefers short replies",
                "how the person likes to be answered",
            ),
        ]
    }

    #[test]
    fn the_fixture_is_the_entries_it_names() {
        assert_eq!(read(FIXTURE), fixture());
    }

    #[test]
    fn an_index_printed_is_the_index_parsed() {
        assert_eq!(write(&fixture()), FIXTURE);
        assert_eq!(read(&write(&fixture())), fixture());
    }

    #[test]
    fn an_entry_says_only_what_its_memory_says() {
        let memory = Memory {
            name: "the-build-runs-cargo-test".into(),
            description: "how this project is tested".into(),
            kind: Kind::Project,
            body: "a body the index never carries".into(),
        };
        assert_eq!(of(&memory), fixture()[0]);
        assert!(!line(&of(&memory)).contains("a body"));
    }

    #[test]
    fn an_empty_index_is_an_empty_file() {
        assert_eq!(write(&[]), "");
        assert!(read("").is_empty());
    }

    #[test]
    fn a_line_that_is_not_an_entry_is_not_one() {
        assert_eq!(parse("just some prose"), None);
        assert_eq!(parse("- [no link] here"), None);
        assert_eq!(parse("- [a](b.txt) — x"), None);
    }

    #[test]
    fn a_hook_written_with_a_plain_dash_reads_the_same() {
        let entry = parse("- [A fact](a-fact.md) - one line").expect("an entry");
        assert_eq!(entry.hook, "one line");
        assert_eq!(parse(&line(&entry)).expect("an entry"), entry);
    }

    #[test]
    fn a_new_memory_is_appended_and_a_known_one_is_replaced() {
        let added = with(FIXTURE, &entry("a-fact", "A fact", "one line"));
        assert_eq!(read(&added).len(), 3);
        assert!(added.ends_with("- [A fact](a-fact.md) — one line\n"));

        let corrected = with(&added, &entry("a-fact", "A fact", "two lines"));
        assert_eq!(read(&corrected).len(), 3);
        assert!(corrected.contains("two lines") && !corrected.contains("one line"));
    }

    #[test]
    fn a_line_nobody_here_wrote_survives_a_write() {
        let text = format!("# notes\n\n{FIXTURE}");
        let after = with(&text, &entry("a-fact", "A fact", "one line"));
        assert!(after.starts_with("# notes\n\n"), "{after}");
        assert_eq!(read(&after).len(), 3);
    }
}
