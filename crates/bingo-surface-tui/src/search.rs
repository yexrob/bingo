//! Looking through the transcript.
//!
//! `ctrl+f` takes the status line's row for a query; committing it finds every
//! place the words occur in the rendered transcript and `n`/`N` walk them,
//! scrolling the transcript to each. The hits are cells — a line and a run of
//! columns — because that is what a highlight is drawn over, and because a
//! block's rendering is what a person is actually reading.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::theme;

/// One occurrence: a transcript line and the cells it covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    pub line: usize,
    pub column: usize,
    pub width: usize,
}

/// What is being looked for, and what was found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Search {
    pub query: String,
    /// The query is still being typed; `enter` commits it.
    pub typing: bool,
    hits: Vec<Hit>,
    at: usize,
}

/// What the row says when a committed query found nothing.
pub const NONE: &str = "no matches";
/// What the row opens with.
pub const PROMPT: &str = "/";

impl Search {
    /// A fresh, empty query with the caret in it.
    pub fn open() -> Self {
        Self {
            typing: true,
            ..Self::default()
        }
    }

    pub fn typed(&mut self, c: char) {
        self.query.push(c);
    }

    pub fn backspace(&mut self) {
        self.query.pop();
    }

    /// Look through the rendered transcript and stop typing. Matching ignores
    /// case: a person searching a transcript is looking for words, not for
    /// the shape they happened to be written in.
    pub fn find(&mut self, lines: &[String]) {
        self.typing = false;
        self.at = 0;
        self.hits = match self.query.is_empty() {
            true => Vec::new(),
            false => hits(lines, &self.query),
        };
    }

    /// Step `by` hits, wrapping at either end.
    pub fn step(&mut self, by: isize) {
        if self.hits.is_empty() {
            return;
        }
        let count = self.hits.len() as isize;
        self.at = (self.at as isize + by).rem_euclid(count) as usize;
    }

    pub fn current(&self) -> Option<Hit> {
        self.hits.get(self.at).copied()
    }

    /// The hits on one transcript line, and whether each is the current one.
    pub fn on(&self, line: usize) -> impl Iterator<Item = (Hit, bool)> {
        self.hits
            .iter()
            .enumerate()
            .filter(move |(_, hit)| hit.line == line)
            .map(|(index, hit)| (*hit, index == self.at))
    }

    /// What the row says to the right of the query.
    pub fn tally(&self) -> String {
        if self.typing {
            return String::new();
        }
        match self.hits.len() {
            0 => NONE.to_string(),
            found => format!("{}/{found} · n/N · esc", self.at + 1),
        }
    }
}

/// Every occurrence of `query` in these lines, in reading order.
fn hits(lines: &[String], query: &str) -> Vec<Hit> {
    let needle = query.to_lowercase();
    lines
        .iter()
        .enumerate()
        .flat_map(|(line, text)| in_line(line, text, &needle))
        .collect()
}

fn in_line(line: usize, text: &str, needle: &str) -> Vec<Hit> {
    let haystack = text.to_lowercase();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(found) = haystack[from..].find(needle) {
        let start = from + found;
        out.push(Hit {
            line,
            // Cells, not bytes: a highlight is drawn over columns.
            column: haystack[..start].width(),
            width: needle.width(),
        });
        from = start + needle.len().max(1);
    }
    out
}

/// The one row a search takes, in the status line's place.
pub fn row(search: &Search) -> Line<'static> {
    let caret = if search.typing { "▌" } else { "" };
    Line::from(vec![
        Span::styled(format!("{PROMPT}{}{caret}", search.query), theme::text()),
        Span::styled(format!("  {}", search.tally()), theme::dim()),
    ])
}

/// Paint the hits that are on the screen. It is a pass over the cells the
/// transcript already drew, so no block has to know it was searched.
///
/// `area`'s first row is line `top`: it is the rows carrying lines, which is
/// not the whole region when a short transcript hangs from the composer.
pub fn mark(frame: &mut Frame, area: Rect, top: usize, search: &Search) {
    for row in 0..area.height {
        let line = top + row as usize;
        for (hit, current) in search.on(line) {
            let style = match current {
                true => theme::presence().patch(theme::bold()),
                false => theme::raised().patch(theme::bold()),
            };
            paint(frame, area, area.y + row, hit, style);
        }
    }
}

fn paint(frame: &mut Frame, area: Rect, y: u16, hit: Hit, style: ratatui::style::Style) {
    let buffer = frame.buffer_mut();
    let from = area
        .x
        .saturating_add(u16::try_from(hit.column).unwrap_or(u16::MAX));
    let to = from.saturating_add(u16::try_from(hit.width).unwrap_or(0));
    for x in from..to.min(area.right()) {
        buffer[(x, y)].set_style(style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines() -> Vec<String> {
        vec![
            "the cache is warm".to_string(),
            "Cache the cache".to_string(),
            "nothing here".to_string(),
        ]
    }

    fn found(query: &str) -> Search {
        let mut search = Search::open();
        for c in query.chars() {
            search.typed(c);
        }
        search.find(&lines());
        search
    }

    #[test]
    fn a_committed_query_finds_every_occurrence_in_reading_order() {
        let search = found("cache");
        assert_eq!(
            search.hits,
            vec![
                Hit {
                    line: 0,
                    column: 4,
                    width: 5
                },
                Hit {
                    line: 1,
                    column: 0,
                    width: 5
                },
                Hit {
                    line: 1,
                    column: 10,
                    width: 5
                },
            ]
        );
        assert!(!search.typing, "committing stops the typing");
        assert_eq!(search.tally(), "1/3 · n/N · esc");
    }

    #[test]
    fn stepping_walks_the_hits_and_wraps_both_ways() {
        let mut search = found("cache");
        search.step(1);
        assert_eq!(search.current().map(|h| h.line), Some(1));
        assert_eq!(search.tally(), "2/3 · n/N · esc");
        search.step(1);
        search.step(1);
        assert_eq!(search.current().map(|h| h.line), Some(0), "it wrapped");
        search.step(-1);
        assert_eq!(search.current().map(|h| h.column), Some(10));
    }

    #[test]
    fn a_query_nothing_matches_says_so_and_steps_nowhere() {
        let mut search = found("banana");
        assert_eq!(search.tally(), NONE);
        search.step(1);
        assert!(search.current().is_none());
    }

    #[test]
    fn an_empty_query_finds_nothing_rather_than_everything() {
        let mut search = Search::open();
        search.find(&lines());
        assert!(search.current().is_none());
    }

    #[test]
    fn the_hits_on_a_line_know_which_one_the_eye_is_on() {
        let mut search = found("cache");
        search.step(1);
        let on = search.on(1).collect::<Vec<_>>();
        assert_eq!(on.len(), 2);
        assert!(on[0].1, "the second hit is the current one");
        assert!(!on[1].1);
        assert_eq!(search.on(2).count(), 0);
    }

    #[test]
    fn a_hit_is_measured_in_cells_so_a_wide_glyph_does_not_shift_it() {
        let mut search = Search::open();
        for c in "warm".chars() {
            search.typed(c);
        }
        // `✻ ` is two cells and each of `你好` is two more.
        search.find(&["✻ 你好 warm".to_string()]);
        assert_eq!(search.current().map(|h| h.column), Some(7));
    }
}
