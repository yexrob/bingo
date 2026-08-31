//! The file an entry is: a YAML block between `---` fences and a free body.
//! Contract first — the round trip is total, so this module's tests were
//! written before its serializer.
//!
//! Every scalar is written as a double-quoted YAML string with its escapes,
//! which is what the old project's interpolated writer left out: a summary
//! with a newline in it, or a step that starts with `---`, corrupted the file
//! it was written into. Reading is `serde-saphyr`, as a skill's frontmatter is
//! read.

use jiff::Timestamp;
use serde::Deserialize;

use crate::entry::{Entry, Record, Status};

/// The line that opens and closes the block.
const FENCE: &str = "---";

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("no frontmatter block: a file starts with a --- fence")]
    NoFrontmatter,
    #[error("yaml: {0}")]
    Yaml(String),
    #[error("summary is empty: an entry says what it is for in one line")]
    NoSummary,
}

/// What a file may declare. A key this plugin does not know is ignored, so a
/// person may keep their own notes in the header.
#[derive(Debug, Deserialize)]
struct Front {
    #[serde(default)]
    status: Status,
    #[serde(default)]
    trigger: Vec<String>,
    summary: String,
    #[serde(default)]
    steps: Vec<String>,
    #[serde(default)]
    verify: Option<String>,
    /// A hand-written file that omits it is as old as this read; every file
    /// this module writes carries one.
    #[serde(default = "Timestamp::now")]
    created: Timestamp,
    #[serde(default)]
    outcomes: Vec<Record>,
}

/// The file for `entry`, id and derived counts left out of it.
pub fn to_markdown(entry: &Entry) -> String {
    let mut out = String::from("---\n");
    out.push_str(&format!("status: {}\n", quoted(entry.status.as_str())));
    out.push_str(&format!("summary: {}\n", quoted(&entry.summary)));
    out.push_str(&list("trigger", &entry.trigger));
    out.push_str(&list("steps", &entry.steps));
    if let Some(verify) = &entry.verify {
        out.push_str(&format!("verify: {}\n", quoted(verify)));
    }
    out.push_str(&format!(
        "created: {}\n",
        quoted(&entry.created.to_string())
    ));
    out.push_str(&outcomes(entry));
    out.push_str("---\n");
    if !entry.notes.is_empty() {
        out.push_str(&entry.notes);
        out.push('\n');
    }
    out
}

/// The entry `text` holds, under the id its file name gave it.
pub fn parse(id: &str, text: &str) -> Result<Entry, ParseError> {
    let (block, body) = split(text).ok_or(ParseError::NoFrontmatter)?;
    let front: Front =
        serde_saphyr::from_str(block).map_err(|e| ParseError::Yaml(e.to_string()))?;
    if front.summary.trim().is_empty() {
        return Err(ParseError::NoSummary);
    }
    Ok(Entry {
        id: id.to_string(),
        status: front.status,
        trigger: front.trigger,
        summary: front.summary,
        steps: front.steps,
        verify: front.verify,
        created: front.created,
        outcomes: front.outcomes,
        notes: body.trim().to_string(),
    })
}

fn list(key: &str, values: &[String]) -> String {
    if values.is_empty() {
        return format!("{key}: []\n");
    }
    let mut out = format!("{key}:\n");
    for value in values {
        out.push_str(&format!("  - {}\n", quoted(value)));
    }
    out
}

fn outcomes(entry: &Entry) -> String {
    if entry.outcomes.is_empty() {
        return "outcomes: []\n".into();
    }
    let mut out = String::from("outcomes:\n");
    for record in &entry.outcomes {
        out.push_str(&format!(
            "  - outcome: {}\n    at: {}\n    evidence: {}\n",
            quoted(record.outcome.as_str()),
            quoted(&record.at.to_string()),
            quoted(&record.evidence),
        ));
    }
    out
}

/// A double-quoted YAML scalar: the one shape that carries any text at all,
/// fences and newlines and quotes included.
fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The frontmatter block and the body that follows it, or nothing when the
/// file does not open with a fence and close it.
fn split(source: &str) -> Option<(&str, &str)> {
    let mut lines = source.split_inclusive('\n');
    let open = lines.next().filter(|line| line.trim_end() == FENCE)?;
    let start = open.len();
    let mut end = start;
    for line in lines {
        if line.trim_end() == FENCE {
            return Some((&source[start..end], &source[end + line.len()..]));
        }
        end += line.len();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{Entry, Outcome, Record, Status};
    use jiff::Timestamp;

    /// Everything that broke the old project's interpolated serializer, in one
    /// entry: a newline in the summary, a step that starts with the fence, a
    /// comma inside a trigger, quotes, a backslash, a tab, a control character
    /// and CJK throughout.
    fn adversarial() -> Entry {
        Entry {
            id: "7f3kq2ab".into(),
            status: Status::Retired,
            trigger: vec![
                "cargo test, then clippy".into(),
                "构建失败：cache miss".into(),
                "# not a comment".into(),
            ],
            summary: "when the build breaks\nrun the fixer".into(),
            steps: vec![
                "--- reset the tree".into(),
                "he said \"run \\ it\"".into(),
                "\tan indented step".into(),
                "清理 target/ 目录".into(),
                "a bell \u{7} and a lone \r return".into(),
            ],
            verify: Some("the suite is green: 0 failed".into()),
            created: Timestamp::UNIX_EPOCH + jiff::SignedDuration::from_hours(9),
            outcomes: vec![Record {
                outcome: Outcome::Harmful,
                at: Timestamp::UNIX_EPOCH + jiff::SignedDuration::from_hours(11),
                evidence: "log line 42: \"still red\" — 缓存未命中".into(),
            }],
            notes: "Body text.\n\n---\n\nA fence line in the body, and 中文.\n  indented".into(),
        }
    }

    #[test]
    fn the_adversarial_entry_survives_the_round_trip() {
        let entry = adversarial();
        let text = to_markdown(&entry);
        let read = parse(&entry.id, &text).expect("the file this module wrote parses");
        assert_eq!(read, entry, "the file was:\n{text}");
        assert_eq!(
            to_markdown(&read),
            text,
            "a second write is byte for byte the first"
        );
    }

    #[test]
    fn the_counts_are_nowhere_in_the_file() {
        let mut entry = adversarial();
        entry.outcomes.push(Record {
            outcome: Outcome::Helpful,
            at: Timestamp::UNIX_EPOCH,
            evidence: "it worked".into(),
        });
        let text = to_markdown(&entry);
        assert!(!text.contains("helpful:"), "{text}");
        assert!(!text.contains("harmful:"), "{text}");
        assert!(!text.contains(&entry.id), "the id is the file name: {text}");
    }

    #[test]
    fn a_hand_written_file_is_read_as_it_was_meant() {
        let text = "---\n\
                    status: active\n\
                    summary: Fix the flaky test\n\
                    trigger:\n  - flaky\n  - retry\n\
                    steps:\n  - run it twice\n\
                    ---\n\
                    Wrote this by hand.\n";
        let entry = parse("hand0001", text).expect("a hand-written entry");
        assert_eq!(entry.id, "hand0001");
        assert_eq!(entry.summary, "Fix the flaky test");
        assert_eq!(entry.trigger, ["flaky", "retry"]);
        assert_eq!(entry.steps, ["run it twice"]);
        assert_eq!(entry.verify, None);
        assert!(entry.outcomes.is_empty());
        assert_eq!(entry.notes, "Wrote this by hand.");
    }

    #[test]
    fn a_file_that_does_not_parse_says_why() {
        for (text, why) in [
            ("no frontmatter at all\n", "frontmatter"),
            ("---\nsummary: [unterminated\n---\nbody\n", "yaml"),
            ("---\nsteps:\n  - one\n---\nno summary\n", "summary"),
            ("---\nsummary: x\nstatus: retried\n---\nbody\n", "yaml"),
        ] {
            let error = parse("id", text).expect_err("{text}").to_string();
            assert!(error.to_lowercase().contains(why), "{error} ({text})");
        }
    }

    #[test]
    fn an_unknown_key_is_ignored_and_the_body_keeps_its_fences() {
        let text = "---\nsummary: x\nhits: 12\n---\n---\nstill the body\n";
        let entry = parse("id", text).expect("an entry");
        assert_eq!(entry.notes, "---\nstill the body");
    }
}
