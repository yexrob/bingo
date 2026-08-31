//! An entry's id: a short slug minted at creation, which is also its file
//! name. It is short because a person reads it in an index and types it back
//! (ADR-0014 §4); the old project's display-only short id was accepted by no
//! tool, so what a person could read they could not name.

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
}
