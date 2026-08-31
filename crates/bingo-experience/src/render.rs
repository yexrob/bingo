//! How an entry reads: one line where the id and the summary are all there is
//! room for, the whole playbook where the model asked for it, and a row for
//! the table `/experience` draws. One vocabulary, so the index, the recall,
//! the search and the table never describe the same entry differently.

use jiff::Timestamp;

use crate::entry::{Entry, Status};

pub const HEADERS: [&str; 5] = ["id", "status", "summary", "outcomes", "age"];

/// The one order the prompt's index and a person's table both show: what has
/// helped most first, what has done harm last, and a retired entry after the
/// active ones whatever it has to show for itself. Ties go by id, so the list
/// does not shuffle between reads.
pub fn by_worth<'a>(entries: impl Iterator<Item = &'a Entry>) -> Vec<&'a Entry> {
    let mut ordered: Vec<&Entry> = entries.collect();
    ordered.sort_by(|a, b| {
        (a.status != Status::Active)
            .cmp(&(b.status != Status::Active))
            .then(b.helpful().cmp(&a.helpful()))
            .then(a.harmful().cmp(&b.harmful()))
            .then(a.id.cmp(&b.id))
    });
    ordered
}

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

/// The whole playbook, for the model that asked for it: nothing is cut here,
/// a summary written over two lines included.
pub fn full(entry: &Entry) -> String {
    let mut out = format!(
        "{} [{}] {} (helpful {}, harmful {})",
        entry.id,
        entry.status.as_str(),
        indented(&entry.summary),
        entry.helpful(),
        entry.harmful()
    );
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

/// One row of the table a person reads.
pub fn row(entry: &Entry, now: Timestamp) -> Vec<String> {
    vec![
        entry.id.clone(),
        entry.status.as_str().to_string(),
        first_line(&entry.summary),
        format!("+{} / -{}", entry.helpful(), entry.harmful()),
        entry.age(now),
    ]
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
    fn a_row_has_a_column_per_header() {
        let now = Timestamp::UNIX_EPOCH + jiff::SignedDuration::from_hours(48);
        let row = row(
            &Entry {
                status: Status::Retired,
                ..scored()
            },
            now,
        );
        assert_eq!(row.len(), HEADERS.len());
        assert_eq!(
            row,
            [
                "abcd1234",
                "retired",
                "clear the target directory",
                "+1 / -1",
                "2d"
            ]
        );
    }

    #[test]
    fn the_worth_of_an_entry_is_what_it_has_helped_with_and_whether_it_is_live() {
        let entries: Vec<Entry> = [("aaaa", 0, 0), ("bbbb", 2, 0), ("cccc", 2, 1)]
            .iter()
            .map(|(id, helpful, harmful)| Entry {
                id: (*id).to_string(),
                outcomes: (0..*helpful)
                    .map(|_| Record {
                        outcome: Outcome::Helpful,
                        at: Timestamp::UNIX_EPOCH,
                        evidence: "e".into(),
                    })
                    .chain((0..*harmful).map(|_| Record {
                        outcome: Outcome::Harmful,
                        at: Timestamp::UNIX_EPOCH,
                        evidence: "e".into(),
                    }))
                    .collect(),
                ..entry()
            })
            .chain(std::iter::once(Entry {
                id: "dddd".into(),
                status: Status::Retired,
                ..scored()
            }))
            .collect();
        let ordered = by_worth(entries.iter());
        assert_eq!(
            ordered.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["bbbb", "cccc", "aaaa", "dddd"]
        );
    }

    #[test]
    fn a_summary_with_more_than_one_line_is_cut_in_a_line_and_whole_in_the_block() {
        let entry = Entry {
            summary: "first line\nsecond line".into(),
            ..entry()
        };
        assert!(line(&entry).contains("first line …"), "{}", line(&entry));
        assert!(!line(&entry).contains("second"), "{}", line(&entry));
        assert!(
            full(&entry).contains("first line\n     second line"),
            "{}",
            full(&entry)
        );
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
