//! An entry's id: a short slug minted at creation, which is also its file
//! name, and the prefix every tool that takes an id accepts. What
//! `/schedule` shows is what a tool accepts.

/// Long enough that a store's entries do not collide, short enough to type.
const LEN: usize = 8;

/// A fresh slug. The tail of a ULID is its random component — the head is
/// the clock, and a store's entries should not sort by the hour they were
/// written.
pub fn mint() -> String {
    let raw = ulid::Ulid::generate().to_string().to_lowercase();
    raw.chars()
        .skip(raw.chars().count().saturating_sub(LEN))
        .collect()
}

/// What a prefix named.
#[derive(Debug, PartialEq, Eq)]
pub enum Named<'a> {
    One(&'a str),
    Unknown,
    /// The ids it could have meant.
    Ambiguous(Vec<&'a str>),
}

/// The id `prefix` names. An exact id always wins over the ids it is a
/// prefix of, so a hand-shortened file name can still be named in full.
pub fn resolve<'a>(ids: &[&'a str], prefix: &str) -> Named<'a> {
    let prefix = prefix.trim();
    if prefix.is_empty() {
        return Named::Unknown;
    }
    if let Some(exact) = ids.iter().find(|id| **id == prefix) {
        return Named::One(exact);
    }
    match ids
        .iter()
        .copied()
        .filter(|id| id.starts_with(prefix))
        .collect::<Vec<&str>>()
        .as_slice()
    {
        [] => Named::Unknown,
        [one] => Named::One(one),
        many => Named::Ambiguous(many.to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let ids = ["ab12cd34", "ab99zz00", "ffffffff"];
        assert_eq!(resolve(&ids, "ff"), Named::One("ffffffff"));
        assert_eq!(resolve(&ids, "ab12cd34"), Named::One("ab12cd34"));
        assert_eq!(resolve(&ids, "zz"), Named::Unknown);
        assert_eq!(resolve(&ids, ""), Named::Unknown);
        assert_eq!(
            resolve(&ids, "ab"),
            Named::Ambiguous(vec!["ab12cd34", "ab99zz00"])
        );
    }

    #[test]
    fn an_exact_id_wins_over_the_ids_it_prefixes() {
        assert_eq!(resolve(&["ab12", "ab12cd34"], "ab12"), Named::One("ab12"));
    }
}
