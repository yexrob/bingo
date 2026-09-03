//! What this client answers when a question came back with no answer
//! (ADR-0039 §3).
//!
//! A `session/request_permission` is put to whoever is at the session
//! ([`crate::question`]). This is the other end of that: what is answered when
//! nothing chose an option — no session behind this conversation, a door that
//! refused it, a surface that declined what it was handed. It fails closed, in
//! the agent's own words: the answer is one of the ids the agent itself
//! offered, never one invented here, and where it offered nothing to refuse
//! with, the question is cancelled instead.
//!
//! The person is told, once, because a refusal nobody chose is a decision made
//! by a rule they cannot see. The row is where they change it (ADR-0039 §4):
//! an adapter configured in its own words never asks at all.

use agent_client_protocol_schema::v1::{
    CreateElicitationResponse, ElicitationAction, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse,
};
use bingo_sdk::Level;

use crate::question;

/// The notice code a person sees when an adapter asked and nobody answered.
pub const CODE: &str = "ACP_ASKED";

/// The agent's own way of saying no, or that the question went away.
pub fn refused(request: &RequestPermissionRequest) -> RequestPermissionResponse {
    match question::refusing(request) {
        Some(option) => question::selected(option),
        // An agent that offers no way to say no is told the question went
        // away, which ACP has an outcome for. Inventing an id would be
        // answering something else.
        None => RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled),
    }
}

/// `elicitation/create` is a door of another shape — free-form input, not a
/// choice among the asker's own answers — and stays closed until something
/// opens it (ADR-0039 §3).
pub fn declined() -> CreateElicitationResponse {
    CreateElicitationResponse::new(ElicitationAction::Decline)
}

/// What a person is told, in the words of the thing they would change. Said
/// once per adapter session: the agent may ask on every call it makes, and a
/// line repeated twenty times is not a clearer line.
///
/// It says "got no answer" rather than "nobody was there", because both ways of
/// reaching here look the same from the agent's side: a run with nobody at it
/// declines every question it is handed, and so does a person who leaves the
/// prompt.
pub fn told(adapter: &str) -> (Level, String, String) {
    (
        Level::Warn,
        CODE.to_string(),
        format!(
            "{adapter} asked for permission and got no answer, so it was refused. \
             A headless run has nobody to ask; to decide in advance, say what it \
             may do on its own row, `acp.adapters.{adapter}` — its permission mode \
             or approval policy goes in `args` or `env`, in the adapter's own words."
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;
    use serde_json::{Value, json};

    fn answered(recorded: Value) -> Value {
        let request: RequestPermissionRequest =
            serde_json::from_value(recorded).expect("a recorded request parses");
        serde_json::to_value(refused(&request)).expect("an outcome serialises")
    }

    /// Both adapters, both option sets. Neither's ids resemble the other's,
    /// which is why the answer is found by kind and sent back by id.
    #[test]
    fn the_refusal_is_the_agents_own_reject_option() {
        assert_eq!(
            answered(fixtures::request_permission()),
            fixtures::request_permission_refused()
        );
        assert_eq!(
            answered(fixtures::request_permission_codex())["outcome"]["optionId"],
            "decline",
            "the first reject, not the last: `cancel` rejects too"
        );
    }

    /// An agent that offers only ways to say yes is told the question went
    /// away. Picking one of them would be allowing what nobody read.
    #[test]
    fn an_agent_that_offers_no_way_to_refuse_is_told_the_question_is_gone() {
        assert_eq!(
            answered(json!({
                "sessionId": "s",
                "toolCall": { "toolCallId": "c1" },
                "options": [{ "optionId": "yes", "name": "Yes", "kind": "allow_once" }]
            })),
            fixtures::request_permission_cancelled()
        );
    }

    #[test]
    fn an_elicitation_is_declined_in_the_protocols_own_word() {
        assert_eq!(
            serde_json::to_value(declined()).expect("it serialises"),
            fixtures::elicitation_declined()
        );
    }

    /// A notice a person cannot act on is noise. This one says what happened
    /// and names both ways out of it.
    #[test]
    fn the_notice_says_nobody_answered_and_names_the_row() {
        let (level, code, said) = told("codex-acp");
        assert_eq!(level, Level::Warn);
        assert_eq!(code, CODE);
        assert!(said.contains("got no answer"), "{said}");
        assert!(said.contains("acp.adapters.codex-acp"), "{said}");
        assert!(said.contains("args"), "{said}");
        assert!(said.contains("env"), "{said}");
    }
}
