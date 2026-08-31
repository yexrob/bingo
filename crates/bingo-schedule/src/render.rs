//! What a person reads: one row per entry, and the head of a text.
//!
//! `/schedule` and `ScheduleList` show the same rows, so what a person sees
//! and what the model sees cannot drift apart.

use jiff::Zoned;
use jiff::tz::TimeZone;

use crate::entry::Entry;

pub const HEADERS: [&str; 5] = ["id", "spec", "next fire", "enabled", "text"];

/// Enough of a prompt to tell two schedules apart, in a table cell.
const HEAD: usize = 48;

/// What a next fire that is not coming shows as.
const NEVER: &str = "—";

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

    #[test]
    fn a_head_is_the_first_line_and_says_when_it_left_something_out() {
        assert_eq!(head("one line", 48), "one line");
        assert_eq!(head("  padded  \nand more\n", 48), "padded");
        assert_eq!(head(&"x".repeat(60), 10), "xxxxxxxxx…");
        assert_eq!(head("", 10), "");
        assert_eq!(head("日本語のテキストがここにある", 5), "日本語の…");
    }
}
