//! What a person reads: one row per entry, and the head of a text.
//!
//! `/schedule` and `ScheduleList` show the same rows, so what a person sees
//! and what the model sees cannot drift apart.

use bingo_sdk::View;
use jiff::Zoned;
use jiff::tz::TimeZone;

use crate::entry::Entry;
use crate::store::Shelf;

pub const HEADERS: [&str; 5] = ["id", "spec", "next fire", "enabled", "text"];

/// What a store with nothing in it says.
const NONE: &str = "no schedules yet";

/// Enough of a prompt to tell two schedules apart, in a table cell.
const HEAD: usize = 48;

/// What a next fire that is not coming shows as.
const NEVER: &str = "—";

/// The whole screen: the entries, who runs them, the last fire that never
/// became a turn, and any file that was meant to be an entry. `/schedule`
/// and `ScheduleList` show this, so a person and the model read the same
/// store.
pub fn view(shelf: &Shelf, holder: &str, trouble: Option<&str>, tz: &TimeZone) -> View {
    let mut parts = vec![match shelf.is_empty() {
        true => View::Text { text: NONE.into() },
        false => View::Table {
            headers: HEADERS.map(str::to_string).to_vec(),
            rows: shelf.entries.iter().map(|e| row(e, tz)).collect(),
        },
    }];
    parts.push(View::Text {
        text: format!("schedules: {holder}"),
    });
    parts.extend(trouble.map(|said| View::Badge {
        text: said.to_string(),
        tone: bingo_sdk::Tone::Bad,
    }));
    parts.extend(unreadable(shelf));
    View::Stack { children: parts }
}

/// A file that was meant to be an entry says so here, and nowhere else: a
/// store is hand-editable, and a silent skip is how a person loses one.
fn unreadable(shelf: &Shelf) -> Option<View> {
    if shelf.unreadable.is_empty() {
        return None;
    }
    let lines: Vec<String> = shelf
        .unreadable
        .iter()
        .map(|bad| format!("{}: {}", bad.file, bad.why))
        .collect();
    Some(View::Text {
        text: format!(
            "{} file(s) could not be read:\n{}",
            lines.len(),
            lines.join("\n")
        ),
    })
}

pub fn row(entry: &Entry, tz: &TimeZone) -> Vec<String> {
    vec![
        entry.id.clone(),
        entry.spec.to_string(),
        when(entry.next_fire(tz).as_ref()),
        if entry.enabled { "yes" } else { "no" }.into(),
        head(&entry.text, HEAD),
    ]
}

/// A fire as a person reads a clock: local, to the minute.
pub fn when(next: Option<&Zoned>) -> String {
    match next {
        Some(fire) => fire.strftime("%Y-%m-%d %H:%M").to_string(),
        None => NEVER.into(),
    }
}

/// The first line of a text, short enough for a cell and honest about
/// what it left out.
pub fn head(text: &str, width: usize) -> String {
    let line = text.lines().next().unwrap_or_default().trim();
    if line.chars().count() <= width {
        return line.to_string();
    }
    let kept: String = line.chars().take(width.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::tests::entry;

    #[test]
    fn a_row_says_when_it_fires_next_and_what_it_will_say() {
        let row = row(&entry(), &TimeZone::UTC);
        assert_eq!(row.len(), HEADERS.len());
        assert_eq!(row[0], "abcd1234");
        assert_eq!(row[1], "every 30m");
        assert_eq!(row[2], "1970-01-01 00:30");
        assert_eq!(row[3], "yes");
        assert_eq!(row[4], "check whether the nightly build is green");
    }

    #[test]
    fn a_disabled_entry_has_no_next_fire_and_says_so() {
        let row = row(
            &Entry {
                enabled: false,
                ..entry()
            },
            &TimeZone::UTC,
        );
        assert_eq!(row[2], NEVER);
        assert_eq!(row[3], "no");
    }

    fn shelf(entries: Vec<Entry>) -> Shelf {
        Shelf {
            entries,
            unreadable: Vec::new(),
        }
    }

    fn children(view: View) -> Vec<View> {
        match view {
            View::Stack { children } => children,
            other => panic!("a schedule table is a stack, not {other:?}"),
        }
    }

    #[test]
    fn an_empty_store_is_one_line_and_still_says_who_runs_it() {
        let shown = children(view(
            &shelf(Vec::new()),
            "held by this process",
            None,
            &TimeZone::UTC,
        ));
        assert_eq!(shown[0], View::Text { text: NONE.into() });
        assert_eq!(
            shown[1],
            View::Text {
                text: "schedules: held by this process".into()
            }
        );
        assert_eq!(shown.len(), 2);
    }

    #[test]
    fn every_entry_is_a_row_and_the_holder_is_the_line_under_them() {
        let shown = children(view(
            &shelf(vec![entry()]),
            "dormant — held by pid 42",
            None,
            &TimeZone::UTC,
        ));
        let View::Table { headers, rows } = &shown[0] else {
            panic!("a store with entries is a table: {shown:?}");
        };
        assert_eq!(headers, &HEADERS);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][0], "abcd1234");
        assert_eq!(
            shown[1],
            View::Text {
                text: "schedules: dormant — held by pid 42".into()
            }
        );
    }

    #[test]
    fn a_file_that_could_not_be_read_is_said_out_loud() {
        let shelf = Shelf {
            entries: vec![entry()],
            unreadable: vec![crate::store::Unreadable {
                file: "broken.json".into(),
                why: "expected value at line 1".into(),
            }],
        };
        let shown = children(view(&shelf, "held by this process", None, &TimeZone::UTC));
        let View::Text { text } = &shown[2] else {
            panic!("the notice is text: {shown:?}");
        };
        assert!(text.contains("broken.json"), "{text}");
        assert!(text.contains("1 file(s) could not be read"), "{text}");
    }

    #[test]
    fn a_fire_that_opened_no_turn_is_shown_where_a_person_looks() {
        let said = "abcd1234 fired at 1970-01-01 00:30 and opened no turn";
        let shown = children(view(
            &shelf(vec![entry()]),
            "held by this process",
            Some(said),
            &TimeZone::UTC,
        ));
        assert_eq!(
            shown[2],
            View::Badge {
                text: said.into(),
                tone: bingo_sdk::Tone::Bad
            },
            "this tree installs no tracing subscriber, so a log line would be nowhere"
        );
    }

    #[test]
    fn a_head_is_the_first_line_and_says_when_it_left_something_out() {
        assert_eq!(head("one line", 48), "one line");
        assert_eq!(head("  padded  \nand more\n", 48), "padded");
        assert_eq!(head(&"x".repeat(60), 10), "xxxxxxxxx…");
        assert_eq!(head("", 10), "");
        assert_eq!(head("日本語のテキストがここにある", 5), "日本語の…");
    }
}
