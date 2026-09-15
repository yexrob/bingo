//! The stance itself: one block of words — the crate's, or the person's —
//! and the contributor that puts it in front of the model.

use async_trait::async_trait;
use bingo_sdk::{
    ContextContributor, ContextError, ContextPiece, ContextQuery, Placement, SystemBlock,
};

/// After the kernel's identity and before the project's instructions (−10)
/// and its memory (−5): who bingo is precedes how this project works, so a
/// project's own file can still narrow the stance for that project.
const ORDER: i32 = -20;

pub(crate) const ID: &str = "persona:judgement";

/// The words the model reads unless the person wrote their own. The same in
/// every session, so they sit in the cached prefix and cost a request nothing
/// after the first (ADR-0059 §2).
pub const TEXT: &str = "\
# Judgement
You are a colleague, not an order-taker: you have a view of your own, and the \
person has asked to hear it.
- When the approach you were given looks wrong, or you find a better one \
while working, say so before you go on: what you would do instead, why it is \
better, and what it costs. A few sentences, not a lecture.
- Then the person decides. If they hold to their path after hearing you, take \
it, and take it well. If the work can wait for their answer, wait. If it \
cannot, take the path you believe in, say that you did, and say why.
- Disagree with a claim, never with a person. Put the case against your own \
view as plainly as the case for it, and say when you are guessing.
- Never deviate silently. A better path taken without a word is worse than \
the wrong path taken together: the person cannot see what you saw.
- Small choices — a name, an order of steps, a tool — are yours. Raise the \
ones that change the result, the cost, or what is left behind.";

/// Contributes the stance and nothing else: it reads no file and asks the
/// session nothing, because a stance is the same whatever the session is
/// doing. Its words are settled once, when the plugin registers.
#[derive(Debug, Clone)]
pub struct JudgementContributor {
    text: String,
}

impl JudgementContributor {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }

    /// The block as every request carries it, or nothing at all: a person who
    /// wants the plugin on and silent writes `persona.text = ""`.
    fn block(&self) -> Option<SystemBlock> {
        (!self.text.is_empty()).then(|| SystemBlock {
            text: self.text.clone(),
            cache: true,
        })
    }
}

impl Default for JudgementContributor {
    fn default() -> Self {
        Self::new(TEXT)
    }
}

#[async_trait]
impl ContextContributor for JudgementContributor {
    fn id(&self) -> &str {
        ID
    }

    fn placement(&self) -> Placement {
        Placement::System { order: ORDER }
    }

    async fn contribute(&self, _: ContextQuery<'_>) -> Result<Vec<ContextPiece>, ContextError> {
        Ok(self.block().map(ContextPiece::System).into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::Asked;

    async fn contributed(contributor: &JudgementContributor) -> Vec<ContextPiece> {
        let asked = Asked::new();
        contributor
            .contribute(asked.query())
            .await
            .expect("the stance reads nothing that can fail")
    }

    /// −20 sorts before `context:instructions` (−10) and `context:memory`
    /// (−5), which is what "before the project's own words" means here.
    #[test]
    fn it_speaks_after_the_kernel_and_before_the_project() {
        let contributor = JudgementContributor::default();
        assert_eq!(contributor.id(), "persona:judgement");
        assert_eq!(contributor.placement(), Placement::System { order: -20 });
    }

    #[tokio::test]
    async fn the_one_block_is_cacheable() {
        let pieces = contributed(&JudgementContributor::default()).await;
        let [ContextPiece::System(block)] = pieces.as_slice() else {
            panic!("the stance is one system block, got {pieces:?}");
        };
        assert_eq!(block.text, TEXT);
        assert!(block.cache, "the same words in every session are cached");
    }

    /// The person's words replace the block; they never join it, because two
    /// voices in one prompt argue (ADR-0059 §1).
    #[tokio::test]
    async fn a_persons_own_words_are_the_whole_block() {
        let pieces = contributed(&JudgementContributor::new("You are Bingo the pirate.")).await;
        let [ContextPiece::System(block)] = pieces.as_slice() else {
            panic!("the stance is one system block, got {pieces:?}");
        };
        assert_eq!(block.text, "You are Bingo the pirate.");
        assert!(block.cache);
    }

    #[tokio::test]
    async fn an_empty_text_is_no_block_at_all() {
        assert!(contributed(&JudgementContributor::new("")).await.is_empty());
    }

    /// The four moves, one phrase each. "Never deviate silently" is also what
    /// the binary's black-box run matches on, so it is pinned here too.
    #[test]
    fn the_stance_says_when_to_speak_who_decides_and_what_is_too_small_to_raise() {
        for phrase in [
            "say so before you go on",
            "Then the person decides",
            "Never deviate silently",
            "Small choices",
        ] {
            assert!(TEXT.contains(phrase), "the stance dropped {phrase:?}");
        }
    }

    /// A stance every request pays for stays cheap (ADR-0059, plan R-voice).
    #[test]
    fn the_stance_stays_under_a_thousand_characters() {
        let characters = TEXT.chars().count();
        assert!(characters < 1_000, "{characters} characters");
    }
}
