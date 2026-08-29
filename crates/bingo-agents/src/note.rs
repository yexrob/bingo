//! What every child is told about being one.
//!
//! The base prompt is written for the session a person is looking at, and two
//! of its promises do not hold in a child: that the prose of a turn is read by
//! whoever asked, and that a question can be put to the person. Say so, rather
//! than letting the model plan against a surface it does not have. Everything
//! here is a fact about this product — there is one way back to the parent
//! between turns, and permission prompts are the one interaction that still
//! reaches a person.

/// Prepended to every child's system prompt, before the definition's body.
pub const NOTE: &str = "\
# You are a sub-agent

- Another agent spawned you for one task, and your final text is the answer it
  gets: the prose of your last turn is returned as the result of the call that
  started you. Put your conclusions in it — nothing else of your turn is read
  for you.
- `SendMessage(to: \"parent\")` is your one deliberate way to reach that agent
  *between* turns: you are blocked on a decision, or you found something that
  changes what it is doing. It is not for progress, receipts, or anything
  already in your reply.
- Do not put questions to the person: `AskUserQuestion` is not a sub-agent's
  tool, and a question asked instead of an answer is a turn spent on nothing.
  Permission prompts are the exception and do reach them, so a call that needs
  approval still gets a real answer; anything else you need belongs in your
  reply.
- Your turn ends when you stop calling tools, and nothing wakes you afterwards.
  Finish the task within it, or say plainly what is still pending: your parent
  can send you a follow-up.";

/// A child's `system_extra`: the note, then whatever the definition says.
pub fn system_extra(system: &str) -> String {
    let system = system.trim();
    if system.is_empty() {
        return NOTE.to_string();
    }
    format!("{NOTE}\n\n{system}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_definition_s_body_follows_the_note() {
        let extra = system_extra("You review diffs.\n");
        assert!(extra.starts_with(NOTE));
        assert!(extra.ends_with("\n\nYou review diffs."), "{extra}");
    }

    #[test]
    fn a_definition_with_no_body_leaves_the_note_alone() {
        assert_eq!(system_extra("  \n"), NOTE);
    }

    #[test]
    fn the_note_names_the_two_ways_back_and_the_one_that_is_gone() {
        assert!(NOTE.contains("SendMessage(to: \"parent\")"));
        assert!(NOTE.contains("returned as the result"));
        assert!(NOTE.contains("AskUserQuestion"));
        assert!(!NOTE.contains("room"), "this product has no rooms");
        assert!(!NOTE.contains("colleague"), "a child has no colleagues");
    }
}
