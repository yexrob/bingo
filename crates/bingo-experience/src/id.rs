//! An entry's id: a short slug minted at creation, which is also its file
//! name, and the prefix every tool that takes an id accepts. What the index
//! shows is what a tool accepts (ADR-0014 §4); the old project's display-only
//! short id was accepted by no tool, so what a person could read they could
//! not name.

use crate::entry::Entry;

/// Long enough that a project's entries do not collide, short enough to type.
const LEN: usize = 8;

/// A fresh slug. The tail of a ULID is its random component — the head is the
/// clock, and a project's entries should not sort by the hour they were
/// written.
pub fn mint() -> String {
    let raw = ulid::Ulid::generate().to_string().to_lowercase();
    raw.chars()
        .skip(raw.chars().count().saturating_sub(LEN))
        .collect()
}

/// What a prefix named.
#[derive(Debug, PartialEq)]
pub enum Named<'a> {
    One(&'a Entry),
    Unknown,
    /// The ids it could have meant.
    Ambiguous(Vec<&'a str>),
}

/// The entry `prefix` names. An exact id always wins over the entries it is a
/// prefix of, so a hand-shortened file name can still be named in full.
pub fn resolve<'a>(entries: &'a [Entry], prefix: &str) -> Named<'a> {
    let prefix = prefix.trim();
    if prefix.is_empty() {
        return Named::Unknown;
    }
    if let Some(exact) = entries.iter().find(|entry| entry.id == prefix) {
        return Named::One(exact);
    }
    let matched: Vec<&Entry> = entries
        .iter()
        .filter(|entry| entry.id.starts_with(prefix))
        .collect();
    match matched.as_slice() {
        [] => Named::Unknown,
        [one] => Named::One(one),
        many => Named::Ambiguous(many.iter().map(|entry| entry.id.as_str()).collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_ids(ids: &[&str]) -> Vec<Entry> {
        ids.iter()
            .map(|id| Entry {
                id: (*id).to_string(),
                ..crate::entry::tests::entry()
            })
            .collect()
    }

    #[test]
    fn a_minted_id_is_short_lowercase_and_alphanumeric() {
        let ids: Vec<String> = (0..100).map(|_| mint()).collect();
        for id in &ids {
            assert_eq!(id.chars().count(), LEN, "{id}");
            assert!(
                id.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
                "{id}"
            );
        }
        let unique: std::collections::HashSet<&String> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "minted ids collided");
    }

    #[test]
    fn a_unique_prefix_names_one_entry() {
        let entries = with_ids(&["ab12cd34", "ab99zz00", "ffffffff"]);
        assert!(matches!(resolve(&entries, "ff"), Named::One(e) if e.id == "ffffffff"));
        assert!(matches!(resolve(&entries, "ab12cd34"), Named::One(e) if e.id == "ab12cd34"));
        assert_eq!(resolve(&entries, "zz"), Named::Unknown);
        assert_eq!(resolve(&entries, ""), Named::Unknown);
        assert_eq!(
            resolve(&entries, "ab"),
            Named::Ambiguous(vec!["ab12cd34", "ab99zz00"])
        );
    }

    #[test]
    fn an_exact_id_wins_over_the_ids_it_prefixes() {
        let entries = with_ids(&["ab12", "ab12cd34"]);
        assert!(matches!(resolve(&entries, "ab12"), Named::One(e) if e.id == "ab12"));
    }
}
