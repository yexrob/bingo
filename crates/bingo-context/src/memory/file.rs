//! A memory: one fact, in one file, under three lines of frontmatter.
//!
//! The grammar is three keys and one line each, so it is read by hand rather
//! than by a YAML parser: a memory the model wrote with `Write` must parse
//! here, and a parser is a smaller promise than a dependency.

/// What a memory is about, and so which directory it belongs in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The person: how they work and what they prefer, in every project.
    User,
    /// A correction they made, and what to do differently next time.
    Feedback,
    /// This project: how it is built, where things are, what was decided.
    Project,
    /// Something worth reading again, and where it is.
    Reference,
}

impl Kind {
    /// The four words a `type:` line may hold, as the error says them.
    pub const NAMES: &'static str = "user | feedback | project | reference";

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::User => "user",
            Kind::Feedback => "feedback",
            Kind::Project => "project",
            Kind::Reference => "reference",
        }
    }

    pub fn of(word: &str) -> Option<Self> {
        [Kind::User, Kind::Feedback, Kind::Project, Kind::Reference]
            .into_iter()
            .find(|kind| kind.as_str() == word)
    }
}

/// One remembered fact. `name` is the file's own name, so a memory that is
/// moved is a memory that is renamed, and nothing points at a file that is
/// not there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Memory {
    pub name: String,
    pub description: String,
    pub kind: Kind,
    pub body: String,
}

/// Why a file is not a memory. A memory that cannot be read is skipped, never
/// guessed at: a half-understood fact is worse than one the model re-learns.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Malformed {
    #[error("a memory opens with `---` and closes its frontmatter with `---`")]
    Frontmatter,
    #[error("a memory's frontmatter needs a `{0}` line")]
    Missing(&'static str),
    #[error("`{word}` is not a memory type ({})", Kind::NAMES)]
    Type { word: String },
    #[error("the memory is named `{name}`, but its file is `{file}.md`")]
    Renamed { name: String, file: String },
}

const FENCE: &str = "---";

/// The memory in `text`, which the directory keeps as `<file>.md`. A `name`
/// that is not the file's name is refused rather than corrected: the index
/// links the file, and a memory that answers to two names has two.
pub fn parse(file: &str, text: &str) -> Result<Memory, Malformed> {
    let (front, body) = split(text).ok_or(Malformed::Frontmatter)?;
    let name = field(front, "name").ok_or(Malformed::Missing("name"))?;
    if name != file {
        return Err(Malformed::Renamed {
            name: name.to_string(),
            file: file.to_string(),
        });
    }
    let word = field(front, "type").ok_or(Malformed::Missing("type"))?;
    Ok(Memory {
        name: name.to_string(),
        description: field(front, "description")
            .ok_or(Malformed::Missing("description"))?
            .to_string(),
        kind: Kind::of(word).ok_or_else(|| Malformed::Type {
            word: word.to_string(),
        })?,
        body: normal(body),
    })
}

/// The bytes a memory is kept as. Printing what was parsed gives the same
/// bytes back, so a file this writes is a file this reads.
pub fn print(memory: &Memory) -> String {
    let Memory {
        name,
        description,
        kind,
        body,
    } = memory;
    let head = format!(
        "name: {name}\ndescription: {description}\ntype: {}",
        kind.as_str()
    );
    format!("{FENCE}\n{head}\n{FENCE}\n\n{}", normal(body))
}

/// One trailing newline, or none at all for a memory whose fact is entirely
/// in its description.
fn normal(body: &str) -> String {
    let body = body.trim_end_matches('\n').trim_end_matches('\r');
    if body.is_empty() {
        String::new()
    } else {
        format!("{body}\n")
    }
}

/// The frontmatter and the body: what sits between the opening `---` line and
/// the next one, and what follows it once its blank line is dropped.
fn split(text: &str) -> Option<(&str, &str)> {
    let opened = past_fence(text)?;
    let closes = fence_at(opened)?;
    let body = past_fence(&opened[closes..])?;
    Some((&opened[..closes], past_blank(body)))
}

/// The body past the blank line a printed memory leaves under its
/// frontmatter, whichever way the writer ends a line.
fn past_blank(body: &str) -> &str {
    body.strip_prefix("\r\n")
        .or_else(|| body.strip_prefix('\n'))
        .unwrap_or(body)
}

/// What follows a `---` line at the start of `text`, or nothing when there is
/// no such line.
fn past_fence(text: &str) -> Option<&str> {
    let (line, rest) = line_at(text);
    (line.trim_end() == FENCE).then_some(rest)
}

/// Where the next `---` line starts.
fn fence_at(text: &str) -> Option<usize> {
    let mut at = 0;
    while at < text.len() {
        let (line, _) = line_at(&text[at..]);
        if line.trim_end() == FENCE {
            return Some(at);
        }
        at += line.len() + 1;
    }
    None
}

/// One line and what follows it, the newline dropped.
fn line_at(text: &str) -> (&str, &str) {
    match text.find('\n') {
        Some(at) => (&text[..at], &text[at + 1..]),
        None => (text, ""),
    }
}

/// The value of one frontmatter key, unquoted and trimmed. An empty value is
/// no value: a `description:` with nothing after it says nothing.
fn field<'a>(front: &'a str, key: &str) -> Option<&'a str> {
    front.lines().find_map(|line| value(line, key))
}

fn value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let rest = line.trim().strip_prefix(key)?.strip_prefix(':')?.trim();
    let rest = unquote(rest);
    (!rest.is_empty()).then_some(rest)
}

fn unquote(text: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = text.strip_prefix(quote).and_then(|t| t.strip_suffix(quote)) {
            return inner;
        }
    }
    text
}

/// Words a slug may hold, and bytes it may spend. A file name is read by a
/// person scanning a directory, so it is short enough to scan.
const SLUG_WORDS: usize = 8;
const SLUG_BYTES: usize = 48;

/// A file name for a fact: its first words, lowercased and hyphenated.
/// `None` when the fact holds nothing a name may keep.
pub fn slug(fact: &str) -> Option<String> {
    let joined = words(fact);
    let name = joined[..cut_at(&joined, SLUG_BYTES)].trim_end_matches('-');
    (!name.is_empty()).then(|| unreserved(name))
}

fn words(fact: &str) -> String {
    fact.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .take(SLUG_WORDS)
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join("-")
}

/// The longest prefix of at most `max` bytes that ends on a character.
fn cut_at(text: &str, max: usize) -> usize {
    let mut cut = max.min(text.len());
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    cut
}

/// A name something else has already claimed, given a suffix: `MEMORY.md` is
/// the index, and Windows keeps `con`, `nul` and the ports whatever extension
/// they wear.
fn unreserved(name: &str) -> String {
    if ["memory", "con", "prn", "aux", "nul"].contains(&name) || is_port(name) {
        return format!("{name}-note");
    }
    name.to_string()
}

fn is_port(name: &str) -> bool {
    let Some((head, digit)) = name.split_at_checked(3) else {
        return false;
    };
    matches!(head, "com" | "lpt") && matches!(digit.as_bytes(), [b'1'..=b'9'])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One memory as it sits on disk. The contract: what this file says, the
    /// parser reads and the printer writes back byte for byte.
    const FIXTURE: &str = "\
---
name: the-build-runs-cargo-test
description: how this project is tested
type: project
---

`cargo test --workspace --locked` is the whole suite.

**Why:** the workspace has one lock file and the crates share fixtures.
**How to apply:** run it before saying a change is done; see [[the-gates]].
";

    fn fixture() -> Memory {
        Memory {
            name: "the-build-runs-cargo-test".into(),
            description: "how this project is tested".into(),
            kind: Kind::Project,
            body: "`cargo test --workspace --locked` is the whole suite.\n\n\
                   **Why:** the workspace has one lock file and the crates share fixtures.\n\
                   **How to apply:** run it before saying a change is done; see [[the-gates]].\n"
                .into(),
        }
    }

    fn parsed(text: &str) -> Result<Memory, Malformed> {
        parse("the-build-runs-cargo-test", text)
    }

    #[test]
    fn the_fixture_is_the_memory_it_describes() {
        assert_eq!(parsed(FIXTURE).expect("a memory"), fixture());
    }

    #[test]
    fn a_memory_printed_is_the_memory_parsed() {
        assert_eq!(print(&fixture()), FIXTURE);
        assert_eq!(parsed(&print(&fixture())).expect("a memory"), fixture());
    }

    #[test]
    fn every_kind_round_trips() {
        for kind in [Kind::User, Kind::Feedback, Kind::Project, Kind::Reference] {
            let memory = Memory {
                name: "a-fact".into(),
                description: "one line".into(),
                kind,
                body: "the fact\n".into(),
            };
            assert_eq!(parse("a-fact", &print(&memory)).expect("a memory"), memory);
            assert!(Kind::NAMES.contains(kind.as_str()), "{kind:?}");
        }
    }

    #[test]
    fn a_memory_whose_fact_is_its_description_has_no_body() {
        let memory = Memory {
            name: "a-fact".into(),
            description: "the whole fact".into(),
            kind: Kind::User,
            body: String::new(),
        };
        assert!(print(&memory).ends_with("---\n\n"));
        assert_eq!(parse("a-fact", &print(&memory)).expect("a memory"), memory);
    }

    #[test]
    fn a_name_that_is_not_the_file_name_is_refused() {
        let error = parse("something-else", FIXTURE).expect_err("a rename");
        assert_eq!(
            error,
            Malformed::Renamed {
                name: "the-build-runs-cargo-test".into(),
                file: "something-else".into(),
            }
        );
        assert!(error.to_string().contains("something-else.md"), "{error}");
    }

    #[test]
    fn a_file_without_frontmatter_is_not_a_memory() {
        assert_eq!(parsed("just some notes\n"), Err(Malformed::Frontmatter));
        assert_eq!(parsed("---\nname: x\n"), Err(Malformed::Frontmatter));
        assert_eq!(parsed(""), Err(Malformed::Frontmatter));
    }

    #[test]
    fn every_frontmatter_line_is_needed() {
        assert_eq!(
            parsed("---\ndescription: d\ntype: user\n---\n\nx\n"),
            Err(Malformed::Missing("name"))
        );
        assert_eq!(
            parsed("---\nname: the-build-runs-cargo-test\ntype: user\n---\n\nx\n"),
            Err(Malformed::Missing("description"))
        );
        assert_eq!(
            parsed("---\nname: the-build-runs-cargo-test\ndescription: d\n---\n\nx\n"),
            Err(Malformed::Missing("type"))
        );
        assert_eq!(
            parsed("---\nname: the-build-runs-cargo-test\ndescription:\ntype: user\n---\n\nx\n"),
            Err(Malformed::Missing("description")),
            "an empty value is no value"
        );
    }

    #[test]
    fn a_type_nobody_defined_is_refused_with_the_four_that_exist() {
        let text = "---\nname: the-build-runs-cargo-test\ndescription: d\ntype: notes\n---\n\nx\n";
        let error = parsed(text).expect_err("not a type");
        assert_eq!(
            error,
            Malformed::Type {
                word: "notes".into()
            }
        );
        for word in ["user", "feedback", "project", "reference"] {
            assert!(error.to_string().contains(word), "{error}");
        }
    }

    #[test]
    fn a_quoted_value_is_the_value_inside_the_quotes() {
        let text = "---\nname: \"a-fact\"\ndescription: 'one line'\ntype: user\n---\n\nx\n";
        let memory = parse("a-fact", text).expect("a memory");
        assert_eq!(memory.description, "one line");
    }

    #[test]
    fn a_file_written_with_windows_line_endings_still_parses() {
        let text = "---\r\nname: a-fact\r\ndescription: one line\r\ntype: user\r\n---\r\n\r\nx\r\n";
        let memory = parse("a-fact", text).expect("a memory");
        assert_eq!(memory.description, "one line");
        assert_eq!(memory.body, "x\n");
    }

    #[test]
    fn a_slug_is_the_first_words_of_the_fact() {
        assert_eq!(
            slug("The build runs `cargo test`."),
            Some("the-build-runs-cargo-test".into())
        );
        assert_eq!(slug("  "), None);
        assert_eq!(slug("!!! ???"), None);
    }

    #[test]
    fn a_slug_is_short_enough_to_scan() {
        let long = slug("one two three four five six seven eight nine ten").expect("a slug");
        assert_eq!(long, "one-two-three-four-five-six-seven-eight");
        let wide = slug(&"x".repeat(200)).expect("a slug");
        assert_eq!(wide.len(), SLUG_BYTES);
        assert!(!wide.ends_with('-'));
    }

    #[test]
    fn a_name_the_index_or_windows_has_claimed_is_moved_aside() {
        assert_eq!(slug("Memory"), Some("memory-note".into()));
        assert_eq!(slug("NUL"), Some("nul-note".into()));
        assert_eq!(slug("com1"), Some("com1-note".into()));
        assert_eq!(slug("com10"), Some("com10".into()), "only the nine ports");
        assert_eq!(slug("memory of a turn"), Some("memory-of-a-turn".into()));
    }
}
