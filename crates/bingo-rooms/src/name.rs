//! What a room is called. One name has to serve as a title (`#design`) and as
//! a segment of the key `rooms/<parent>/design`, so whitespace and `/` are
//! refused here rather than mangled into something nobody asked for.

use bingo_sdk::{ErrorCode, KernelError};

/// The word a child uses for the session that spawned it. A post whose author
/// left no principal came from the person's own session, and that is what the
/// members of the room call it. On a roster it names the room's own holder
/// (ADR-0028 §1): no agent may take the name, so nothing else can mean it.
pub const PARENT: &str = "parent";

/// Two names, as a room compares them: the same word in any case. Every reader
/// of a name — the mention fold, the roster, the fan-out — comes through here.
pub fn same(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// Whether a name on a roster means the session the room hangs under.
pub fn is_holder(member: &str) -> bool {
    same(member, PARENT)
}

/// A name a room can be opened under.
pub fn check(name: &str) -> Result<&str, KernelError> {
    let name = name.trim();
    let bad = name.is_empty() || name.contains('/') || name.chars().any(char::is_whitespace);
    match bad {
        true => Err(KernelError::new(
            ErrorCode::InvalidInput,
            format!("{name:?} is not a room name: one word, no slashes"),
        )),
        false => Ok(name),
    }
}

/// The name as a title. A room reads as a channel, and `#design` is how a
/// member is told where a message it received came from.
pub fn title(name: &str) -> String {
    format!("#{name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_that_would_break_the_key_or_the_title_is_refused() {
        assert_eq!(check(" design "), Ok("design"));
        for bad in ["", "   ", "a/b", "two words", "de\tsign"] {
            assert!(check(bad).is_err(), "{bad:?} was accepted");
        }
        let error = check("a/b").expect_err("a slash makes the key ambiguous");
        assert_eq!(error.code, ErrorCode::InvalidInput);
    }

    #[test]
    fn a_room_wears_its_name_as_a_channel() {
        assert_eq!(title("design"), "#design");
    }

    #[test]
    fn a_name_is_the_same_word_in_any_case_and_parent_is_the_holder_s() {
        assert!(same("reviewer", "Reviewer"));
        assert!(!same("reviewer", "reviewers"));
        assert!(is_holder("parent"));
        assert!(is_holder("PARENT"));
        assert!(!is_holder("parents"), "a name of its own");
    }
}
