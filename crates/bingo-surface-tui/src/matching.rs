//! The one matcher: what a typed query narrows a list to, and in what order.
//!
//! Four lists are typed into — the `/` commands, the values one of them takes
//! from a catalogue, the names and paths an `@` offers, and the one list of
//! sessions — and a person types at all four the same way. So the behaviour is
//! written once here and every one of them reads it: `nucleo`'s ranking, the
//! one a person's editor and their fuzzy finder already use.
//!
//! Smart case (a lower-case query ignores case; an upper-case letter in it is
//! meant) and smart normalization (`a` finds `ä`, `ä` does not find `a`) are
//! the two rules a person does not have to be told. What falls out of them is
//! that `mdl` finds `model`, `sonnet` finds `anthropic/claude-sonnet-5`, and a
//! typo finds nothing rather than everything.
//!
//! Pure, and the whole of the behaviour: an empty query is every item in the
//! order it came, and ties keep that order too — catalogue order, roster
//! order, the walk's order.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// The items whose `key` the query matches, best first.
///
/// The score is what orders them, and where two score the same the list's own
/// order decides — so a query that says nothing about two rows leaves them as
/// they were.
pub fn rank<'a, T>(query: &str, items: &'a [T], key: impl Fn(&T) -> &str) -> Vec<&'a T> {
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buffer = Vec::new();
    let mut scored: Vec<(u32, &T)> = items
        .iter()
        .filter_map(|item| {
            let haystack = Utf32Str::new(key(item), &mut buffer);
            pattern.score(haystack, &mut matcher).map(|at| (at, item))
        })
        .collect();
    // A stable sort is what leaves the list's own order under the score.
    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    scored.into_iter().map(|(_, item)| item).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matched(query: &str, items: &[&str]) -> Vec<String> {
        let items: Vec<String> = items.iter().map(|item| (*item).to_string()).collect();
        rank(query, &items, String::as_str)
            .into_iter()
            .cloned()
            .collect()
    }

    /// The ask that started M55: what a person types is not a prefix.
    #[test]
    fn a_subsequence_of_the_name_finds_it() {
        assert_eq!(matched("mdl", &["compact", "model"]), vec!["model"]);
        assert_eq!(matched("mo", &["compact", "model"]), vec!["model"]);
    }

    /// A model id is `provider/family-size`, and the word a person remembers
    /// is rarely the first one.
    #[test]
    fn a_word_in_the_middle_finds_the_whole_id() {
        assert_eq!(
            matched("sonnet", &["openai/gpt-5.4", "anthropic/claude-sonnet-5"]),
            vec!["anthropic/claude-sonnet-5"]
        );
    }

    /// The one thing a substring test and a subsequence test must agree on: a
    /// query whose letters are out of order is a typo, not a match.
    #[test]
    fn a_typo_matches_nothing() {
        assert!(matched("mdoel", &["compact", "model"]).is_empty());
        assert!(matched("zz", &["compact", "model"]).is_empty());
    }

    /// Both are matches; the one with no gap in it is the one a person meant.
    #[test]
    fn the_tighter_match_ranks_first() {
        assert_eq!(
            matched("son", &["some-other-name", "claude-sonnet-5"]),
            vec!["claude-sonnet-5", "some-other-name"]
        );
    }

    #[test]
    fn an_empty_query_is_the_whole_list_in_its_own_order() {
        assert_eq!(
            matched("", &["compact", "model", "resume"]),
            vec!["compact", "model", "resume"]
        );
    }

    /// Where the query says nothing to tell two rows apart, the list's order
    /// is the answer — catalogue order for a command, roster order for a row.
    #[test]
    fn ties_keep_the_order_the_list_gave_them() {
        assert_eq!(
            matched("fake", &["fake/fake-2", "fake/fake-1"]),
            vec!["fake/fake-2", "fake/fake-1"]
        );
    }

    /// Smart case: the query a person types in lower case asks nothing about
    /// case, and one they shifted for means it.
    #[test]
    fn an_upper_case_letter_in_the_query_is_meant() {
        assert_eq!(matched("car", &["Cargo.toml"]), vec!["Cargo.toml"]);
        assert_eq!(matched("Car", &["Cargo.toml"]), vec!["Cargo.toml"]);
        assert!(matched("CAR", &["Cargo.toml"]).is_empty());
    }

    /// The key is what is matched, not the item: a row is found by the words
    /// it is composed of, and the item itself is what comes back.
    #[test]
    fn the_key_says_what_is_matched() {
        let rows = [("reviewer", "reviewer #design"), ("scout", "scout")];
        assert_eq!(
            rank("design", &rows, |(_, searched)| *searched)
                .into_iter()
                .map(|(name, _)| *name)
                .collect::<Vec<_>>(),
            vec!["reviewer"]
        );
    }
}
