//! How an entry reads: one line where the id and the summary are all there is
//! room for, the whole playbook where the model asked for it, and a row for
//! the table `/experience` draws. One vocabulary, so the index, the recall,
//! the search and the table never describe the same entry differently.

use crate::entry::Entry;

/// The index line: what an entry is, and what it has been worth.
pub fn line(entry: &Entry) -> String {
    format!(
        "{} {} (helpful {}, harmful {})",
        entry.id,
        first_line(&entry.summary),
        entry.helpful(),
        entry.harmful()
    )
}

/// The same, with the status spelled out: what a search shows, where a retired
/// entry may well be the answer.
pub fn line_with_status(entry: &Entry) -> String {
    format!(
        "{} [{}] {} (helpful {}, harmful {})",
        entry.id,
        entry.status.as_str(),
        first_line(&entry.summary),
        entry.helpful(),
        entry.harmful()
    )
}

/// The whole playbook, for the model that asked for it.
pub fn full(entry: &Entry) -> String {
    let mut out = line_with_status(entry);
    for trigger in &entry.trigger {
        out.push_str(&format!("\n  when: {trigger}"));
    }
    for (n, step) in entry.steps.iter().enumerate() {
        out.push_str(&format!("\n  {}. {}", n + 1, indented(step)));
    }
    if let Some(verify) = &entry.verify {
        out.push_str(&format!("\n  verify: {}", indented(verify)));
    }
    if !entry.notes.is_empty() {
        out.push_str(&format!("\n  notes: {}", indented(&entry.notes)));
    }
    out
}

/// A summary may have been written with newlines in it; a line is a line.
fn first_line(text: &str) -> String {
    match text.lines().next() {
        Some(first) if first.len() < text.len() => format!("{first} …"),
        Some(first) => first.to_string(),
        None => String::new(),
    }
}

/// Text that runs over a line stays under its bullet.
fn indented(text: &str) -> String {
    text.replace('\n', "\n     ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::tests::entry;
    use crate::entry::{Outcome, Record};
    use jiff::Timestamp;

    fn scored() -> Entry {
        Entry {
            outcomes: vec![
                Record {
                    outcome: Outcome::Helpful,
                    at: Timestamp::UNIX_EPOCH,
                    evidence: "green".into(),
                },
                Record {
                    outcome: Outcome::Harmful,
                    at: Timestamp::UNIX_EPOCH,
                    evidence: "red".into(),
                },
            ],
            ..entry()
        }
    }

    #[test]
    fn one_line_is_the_id_the_summary_and_the_counts() {
        assert_eq!(
            line(&scored()),
            "abcd1234 clear the target directory (helpful 1, harmful 1)"
        );
        assert_eq!(
            line_with_status(&scored()),
            "abcd1234 [active] clear the target directory (helpful 1, harmful 1)"
        );
    }

    #[test]
    fn a_summary_with_more_than_one_line_is_cut_to_the_first() {
        let entry = Entry {
            summary: "first line\nsecond line".into(),
            ..entry()
        };
        assert!(line(&entry).contains("first line …"), "{}", line(&entry));
        assert!(!line(&entry).contains("second"), "{}", line(&entry));
    }

    #[test]
    fn the_whole_playbook_is_the_when_the_steps_and_the_check() {
        let entry = Entry {
            notes: "two\nlines".into(),
            ..scored()
        };
        assert_eq!(
            full(&entry),
            "abcd1234 [active] clear the target directory (helpful 1, harmful 1)\n  \
             when: the build breaks\n  1. cargo clean\n  2. cargo build\n  \
             verify: the build is green\n  notes: two\n     lines"
        );
    }
}
