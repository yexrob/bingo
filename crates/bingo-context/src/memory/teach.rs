//! What the model is told about its memory: where it is, what a file looks
//! like, and the rules a good memory keeps.
//!
//! Every word here is paid for on every turn, so there are few of them and a
//! snapshot pins them. The directories are not named here: the index headings
//! that follow carry the paths, and a path written twice is a path that can
//! disagree with itself.

use bingo_sdk::SystemBlock;

const TEACHING: &str = "\
# Memory

You keep memories as markdown files, one fact each, in the two directories \
named by the index headings below. A file is frontmatter — name (its file \
name, without .md), description (one line), type — then the fact. Types: \
user (who the person is, how they work), feedback (a correction or a \
confirmed approach, with **Why:** and **How to apply:**), project (goals and \
constraints the repository does not record; dates absolute), reference (a \
URL, a ticket, a dashboard). [[slug]] links another memory.

An index says what exists; Read a file for the whole fact. To remember: check \
the index for a file that already covers it, then Write or Edit that file and \
its line in MEMORY.md. Fix or delete a memory that turned out wrong. Never \
store what the repository already records, nor what matters only to this \
conversation.";

/// The same words for every session, so they sit in the cached prefix rather
/// than beside the indexes, which change under the model's hands.
pub fn block() -> SystemBlock {
    SystemBlock {
        text: TEACHING.to_string(),
        cache: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The words every turn pays for, pinned. A change to them is read as a
    /// diff rather than slipped in.
    #[test]
    fn the_teaching_is_the_same_words_for_every_turn() {
        insta::assert_snapshot!(block().text);
    }

    /// Words this block may spend. Past ~150 it is a document, and a document
    /// in every prompt is a tax on every turn. The four types are named here
    /// because a memory of the wrong type is a memory nobody finds.
    const MAX_WORDS: usize = 150;

    #[test]
    fn it_is_short_enough_to_carry_every_turn() {
        let words = TEACHING.split_whitespace().count();
        assert!(words <= MAX_WORDS, "{words} words");
    }

    #[test]
    fn it_says_the_format_and_the_rules_and_no_path() {
        for word in ["name", "description", "type", "MEMORY.md", "delete"] {
            assert!(TEACHING.contains(word), "the teaching drops {word}");
        }
        assert!(block().cache, "the words never change within a session");
    }
}
