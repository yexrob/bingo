//! Which door a conversation goes back in through (ADR-0035 §3).
//!
//! The decision is pure: what `initialize` advertised, whether the journal
//! remembers an agent-side session id, and whether there is any history to
//! carry. The climbing is [`crate::session`]'s, because only the rungs
//! themselves touch a wire.
//!
//! The order is `resume` → `load` → a fresh session that is handed a file,
//! and it is an order for a reason: `session/resume` reattaches without
//! replaying, `session/load` replays a history the journal already holds, and
//! the file is what is left when the agent kept nothing.

use agent_client_protocol_schema::v1::AgentCapabilities;
use bingo_sdk::Level;

/// How this session's conversation with the adapter begins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Opening {
    /// Nothing happened before: `session/new`, and no history to carry.
    New,
    /// The agent kept the session and can pick it up without replaying.
    Resume(String),
    /// The agent kept the session but only by replaying it. The replay is
    /// swallowed: the journal is already the one record of those turns.
    Load(String),
    /// A new agent-side session. `transcript` means it is being given a
    /// conversation it was not part of, and the first prompt names the file.
    Fresh { transcript: bool },
}

/// The rung, from what the agent said it can do and what the journal knows.
///
/// A `known` id with neither door is the case that costs the most: the agent
/// has a session somewhere it will not let us back into, so the conversation
/// is carried across as a file instead.
pub fn opening(
    capabilities: &AgentCapabilities,
    known: Option<&str>,
    has_history: bool,
) -> Opening {
    let Some(id) = known else {
        return match has_history {
            true => Opening::Fresh { transcript: true },
            false => Opening::New,
        };
    };
    if capabilities.session_capabilities.resume.is_some() {
        return Opening::Resume(id.to_string());
    }
    if capabilities.load_session {
        return Opening::Load(id.to_string());
    }
    Opening::Fresh {
        transcript: has_history,
    }
}

/// The next rung down, for when a door the agent advertised refuses at the
/// moment it is used — an agent that restarted has forgotten sessions it still
/// says it can resume.
pub fn below(opening: &Opening, has_history: bool) -> Option<Opening> {
    match opening {
        Opening::Resume(id) => Some(Opening::Load(id.clone())),
        Opening::Load(_) => Some(Opening::Fresh {
            transcript: has_history,
        }),
        Opening::New | Opening::Fresh { .. } => None,
    }
}

/// What a person is told about it. Only a degradation is worth saying: a
/// resume that worked is the expected case and says nothing (ADR-0035
/// Consequences — "the degradation is said in a notice").
pub fn notice(opening: &Opening, adapter: &str) -> Option<(Level, String, String)> {
    let said = match opening {
        Opening::New | Opening::Resume(_) => return None,
        Opening::Load(_) => format!(
            "{adapter} cannot resume, so this conversation was reloaded into it. \
             Its replay is not written to the journal a second time."
        ),
        Opening::Fresh { transcript: true } => format!(
            "{adapter} kept nothing of this conversation, so it was started fresh \
             and handed the transcript so far as a file to read. It has no memory \
             of the earlier turns beyond what it reads there."
        ),
        Opening::Fresh { transcript: false } => {
            format!("{adapter} kept nothing of this conversation, so it was started fresh.")
        }
    };
    Some((Level::Warn, "ACP_RESTORE".to_string(), said))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;

    fn capabilities(recorded: serde_json::Value) -> AgentCapabilities {
        let response: agent_client_protocol_schema::v1::InitializeResponse =
            serde_json::from_value(recorded).expect("a recorded handshake parses");
        response.agent_capabilities
    }

    fn both() -> AgentCapabilities {
        capabilities(fixtures::initialize_response())
    }

    fn neither() -> AgentCapabilities {
        capabilities(fixtures::initialize_response_without_restore())
    }

    /// `loadSession` without `sessionCapabilities.resume`: the older flag on
    /// its own, which is what a second-tier adapter still ships.
    fn load_only() -> AgentCapabilities {
        capabilities(serde_json::json!({
            "protocolVersion": 1,
            "agentCapabilities": {
                "loadSession": true,
                "promptCapabilities": { "image": false }
            }
        }))
    }

    #[test]
    fn a_conversation_with_no_past_simply_begins() {
        assert_eq!(opening(&both(), None, false), Opening::New);
        assert_eq!(notice(&Opening::New, "claude"), None);
    }

    #[test]
    fn the_ladder_takes_the_highest_rung_the_agent_offers() {
        assert_eq!(
            opening(&both(), Some("s1"), true),
            Opening::Resume("s1".into()),
            "resume first: it reattaches without replaying"
        );
        assert_eq!(
            opening(&load_only(), Some("s1"), true),
            Opening::Load("s1".into())
        );
        assert_eq!(
            opening(&neither(), Some("s1"), true),
            Opening::Fresh { transcript: true },
            "an id we cannot use is no better than no id"
        );
    }

    /// The journal remembers turns the agent never heard of — a fresh adapter,
    /// a session the id was lost from. It is still the ladder's last rung.
    #[test]
    fn history_with_no_remembered_id_is_carried_as_a_file() {
        assert_eq!(
            opening(&both(), None, true),
            Opening::Fresh { transcript: true }
        );
        assert_eq!(
            opening(&neither(), None, false),
            Opening::New,
            "and nothing to carry is not a degradation"
        );
    }

    /// An agent that says it can resume and then refuses drops one rung, not
    /// all the way: `session/load` is still worth trying.
    #[test]
    fn a_door_that_refuses_at_the_moment_of_use_drops_one_rung() {
        let resumed = Opening::Resume("s1".into());
        assert_eq!(below(&resumed, true), Some(Opening::Load("s1".into())));
        assert_eq!(
            below(&Opening::Load("s1".into()), true),
            Some(Opening::Fresh { transcript: true })
        );
        assert_eq!(
            below(&Opening::Fresh { transcript: true }, true),
            None,
            "the last rung has nothing under it"
        );
        assert_eq!(below(&Opening::New, false), None);
    }

    #[test]
    fn only_a_degradation_is_worth_saying_and_it_says_what_was_lost() {
        assert_eq!(notice(&Opening::Resume("s1".into()), "claude"), None);
        let (level, code, said) =
            notice(&Opening::Load("s1".into()), "claude").expect("a load is worth saying");
        assert_eq!(level, Level::Warn);
        assert_eq!(code, "ACP_RESTORE");
        assert!(said.contains("claude"), "{said}");
        assert!(said.contains("second time"), "{said}");

        let (_, _, said) =
            notice(&Opening::Fresh { transcript: true }, "codex-acp").expect("worth saying");
        assert!(said.contains("no memory"), "{said}");
        assert!(said.contains("file"), "{said}");
    }
}
