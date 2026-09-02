//! What this client answers when an adapter asks it something (ADR-0035 §5).
//!
//! An ACP adapter is a whole agent, permission machinery included, and the row
//! that spawns it already says what it may do — in the adapter's own words,
//! as arguments or environment on `acp.adapters.<name>`: Claude Code's
//! permission modes, Codex's approval policy. bingo does not become a second
//! gate in front of the agent's own, so a question that arrives anyway is
//! refused rather than put to a person who configured the answer elsewhere.
//!
//! Refusing is pure and fails closed: the answer is one of the ids the agent
//! itself offered, never one invented here, and where it offered nothing to
//! refuse with, the question is cancelled instead.

use agent_client_protocol_schema::v1::{
    CreateElicitationResponse, ElicitationAction, PermissionOptionId, PermissionOptionKind,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome,
};
use bingo_sdk::Level;

/// The notice code a person sees when an adapter asked and was refused.
pub const CODE: &str = "ACP_ASKED";

/// The agent's own way of saying no. `reject_once` comes first: refusing this
/// call is the answer, and a standing `reject_always` would be a decision
/// nobody made.
pub fn refused(request: &RequestPermissionRequest) -> RequestPermissionResponse {
    let outcome = match rejection(request) {
        Some(id) => RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(id)),
        // An agent that offers no way to say no is told the question went
        // away, which ACP has an outcome for. Inventing an id would be
        // answering something else.
        None => RequestPermissionOutcome::Cancelled,
    };
    RequestPermissionResponse::new(outcome)
}

fn rejection(request: &RequestPermissionRequest) -> Option<PermissionOptionId> {
    offered(request, PermissionOptionKind::RejectOnce)
        .or_else(|| offered(request, PermissionOptionKind::RejectAlways))
}

fn offered(
    request: &RequestPermissionRequest,
    kind: PermissionOptionKind,
) -> Option<PermissionOptionId> {
    request
        .options
        .iter()
        .find(|option| option.kind == kind)
        .map(|option| option.option_id.clone())
}

/// `elicitation/create` is the same door under another name — an agent asking
/// this client to collect something from a person — and it is closed for the
/// same reason.
pub fn declined() -> CreateElicitationResponse {
    CreateElicitationResponse::new(ElicitationAction::Decline)
}

/// What a person is told, in the words of the thing they would change. Said
/// once per adapter session: the agent may ask on every call it makes, and a
/// line repeated twenty times is not a clearer line.
pub fn told(adapter: &str) -> (Level, String, String) {
    (
        Level::Warn,
        CODE.to_string(),
        format!(
            "{adapter} asked bingo for permission and was refused: an ACP agent \
             brings its own. Say what it may do on its own row, \
             `acp.adapters.{adapter}` — its permission mode or approval policy \
             goes in `args` or `env`, in the adapter's own words."
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;
    use serde_json::{Value, json};

    fn request(recorded: Value) -> RequestPermissionRequest {
        serde_json::from_value(recorded).expect("a recorded request parses")
    }

    fn answered(recorded: Value) -> Value {
        serde_json::to_value(refused(&request(recorded))).expect("an outcome serialises")
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

    /// `reject_always` would teach the agent a standing no; it is the fallback,
    /// not the answer.
    #[test]
    fn a_standing_refusal_is_only_taken_when_it_is_the_only_one() {
        let both = json!({
            "sessionId": "s",
            "toolCall": { "toolCallId": "c1" },
            "options": [
                { "optionId": "never", "name": "Never", "kind": "reject_always" },
                { "optionId": "no", "name": "No", "kind": "reject_once" }
            ]
        });
        assert_eq!(answered(both)["outcome"]["optionId"], "no");
        let standing = json!({
            "sessionId": "s",
            "toolCall": { "toolCallId": "c1" },
            "options": [{ "optionId": "never", "name": "Never", "kind": "reject_always" }]
        });
        assert_eq!(answered(standing)["outcome"]["optionId"], "never");
    }

    #[test]
    fn an_elicitation_is_declined_in_the_protocols_own_word() {
        assert_eq!(
            serde_json::to_value(declined()).expect("it serialises"),
            fixtures::elicitation_declined()
        );
    }

    /// A notice a person cannot act on is noise. This one names the row.
    #[test]
    fn the_notice_names_the_row_where_the_answer_is_configured() {
        let (level, code, said) = told("codex-acp");
        assert_eq!(level, Level::Warn);
        assert_eq!(code, CODE);
        assert!(said.contains("acp.adapters.codex-acp"), "{said}");
        assert!(said.contains("args"), "{said}");
        assert!(said.contains("env"), "{said}");
    }
}
