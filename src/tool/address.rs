//! The conversation address grammar shared by the speech tools (split out of
//! `agent.rs`, which sits against the file-size cap; the words and their
//! reasons are unchanged).

use crate::query::Session;
use crate::tool::ToolError;

/// A resolved `to`: one of the two kinds of conversation there are.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Address {
    /// An agent instance, `main` included.
    Agent(String),
    /// A room, named without its `#`.
    Room(String),
}

/// Read the `to` field as the conversation namespace the interface already uses:
/// `#name` is a room, anything else is an agent and may wear a leading `@`.
///
/// One address language for the tool layer and the display layer, so an agent
/// naming a target says what the user's own composer says (D90's `@name ` /
/// `#name ` routing).
pub(crate) fn parse_address(to: &str) -> Result<Address, ToolError> {
    let to = to.trim();
    if let Some(room) = to.strip_prefix('#') {
        let room = room.trim();
        if room.is_empty() {
            return Err(ToolError::failed("`to` is `#` with no room name after it"));
        }
        return Ok(Address::Room(room.to_string()));
    }
    let agent = to.strip_prefix('@').unwrap_or(to).trim();
    if agent.is_empty() {
        return Err(ToolError::failed(
            "`to` is empty; name an agent instance (or @name), or a room as #name",
        ));
    }
    Ok(Address::Agent(agent.to_string()))
}

/// This session's name in a conversation: a subagent's instance name, or
/// `main`. Stamped by the runtime; the model cannot state it for itself.
pub(crate) fn sender_of(session: &Session) -> String {
    session
        .instance
        .clone()
        .unwrap_or_else(|| crate::channels::MAIN_NAME.to_string())
}

/// Whether this caller may address rooms at all. Rooms are still behind the
/// `experimental.agentChannels` gate, and the cohort that could hold a room
/// membership is the same one the retired `Post` was assembled for: the main
/// session, and named direct subagents.
pub(crate) fn rooms_allowed(session: &Session) -> bool {
    session.settings.experimental.agent_channels
        && (session.depth == 0 || (session.depth == 1 && session.instance.is_some()))
}

/// Refuse a target this caller has no business addressing, in words that say
/// what it may address instead.
pub(crate) fn check_target(session: &Session, address: &Address) -> Result<(), ToolError> {
    let me = sender_of(session);
    match address {
        Address::Agent(name) if *name == me => Err(ToolError::failed(format!(
            "{name} is you — a message to yourself is a note, not a message"
        ))),
        // Anyone with a name may write to anyone who has one (D137). The rule
        // this replaces — a subagent reaches main and nothing else — routed
        // every two-agent exchange through the manager, which is the shape a
        // human org uses for authority, not for talking. Two members who need
        // to work something out are a conversation, and D95 already accepted
        // that: it let a member convene a room for exactly that subset. A
        // two-person room is a direct message with ceremony.
        //
        // What is still checked is that the *sender* can be answered. A reply
        // is a message back, so a session with no name in the registry would be
        // opening a conversation nobody can close; main is the one such session
        // and it has a name of its own. Whether the *target* exists is the
        // registry's answer, given at delivery in words that list who does.
        Address::Agent(name) => {
            if session.depth == 0
                || session.instance.is_some()
                || name == crate::channels::MAIN_NAME
            {
                Ok(())
            } else {
                Err(ToolError::failed(format!(
                    "you may not message {name}: this session has no name in the agent namespace, \
so nothing it wrote could be answered. You can write to main."
                )))
            }
        }
        Address::Room(room) => {
            if !rooms_allowed(session) {
                return Err(ToolError::failed(format!(
                    "rooms are not available to you; #{room} cannot be addressed from here"
                )));
            }
            if !session.channels.is_member(room, &me) {
                return Err(ToolError::failed(format!(
                    "you are not a member of #{room} — join the room before speaking in it"
                )));
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The address grammar is the conversation namespace: `#name` is a room,
    /// anything else is an agent and may wear a leading `@`.
    #[test]
    fn to_is_read_as_the_conversation_namespace() {
        assert_eq!(
            parse_address("scout").unwrap_or_else(|e| panic!("{e}")),
            Address::Agent("scout".into())
        );
        assert_eq!(
            parse_address(" @scout ").unwrap_or_else(|e| panic!("{e}")),
            Address::Agent("scout".into()),
            "the sigil the composer writes is accepted, not required"
        );
        assert_eq!(
            parse_address("#build").unwrap_or_else(|e| panic!("{e}")),
            Address::Room("build".into())
        );
        for bad in ["", "   ", "@", "#"] {
            assert!(parse_address(bad).is_err(), "{bad:?} is not an address");
        }
    }
}
