//! `session/request_permission`, both ways, as one pure translation.
//!
//! The agent offers its own options, by its own ids — `allow-once` and
//! `allow-with-updates` from `claude-agent-acp`, `allow_once`,
//! `allow_for_session`, `decline` and `cancel` from `codex-acp` — and the only
//! answer either will accept is one of those ids back. So this is a
//! `Question`, not a `Permission`: bingo is not deciding whether the tool may
//! run, it is relaying a choice the agent is waiting on. Matching on `kind`,
//! or on position, would have picked the wrong option on the second adapter.
//!
//! ADR-0035 §5 lists `elicitation/create` at this door too. It is not here:
//! the schema crate keeps elicitation behind `unstable_elicitation`, this
//! client declares no elicitation capability, and an agent that was told the
//! door does not exist is answered `method not found` rather than guessed at.

use agent_client_protocol_schema::v1::{
    PermissionOptionId, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome,
};
use bingo_sdk::{Answer, AnswerSpec, InteractionKind, QuestionOption};

/// What a person is shown, and what they may answer with. `Cancel` is always
/// offered because ACP has an outcome for it: an agent that is told the
/// question went away can stop asking.
pub fn question(
    request: &RequestPermissionRequest,
    adapter: &str,
) -> (InteractionKind, Vec<AnswerSpec>) {
    let kind = InteractionKind::Question {
        question: asked(request),
        header: Some(format!("{adapter} asks")),
        options: request.options.iter().map(option).collect(),
        free_text: false,
        multi: false,
    };
    (kind, vec![AnswerSpec::Choice, AnswerSpec::Cancel])
}

/// The agent's own title for the call, which is the whole of what it is
/// asking about. A call with no title is named by its id rather than by
/// nothing.
fn asked(request: &RequestPermissionRequest) -> String {
    request
        .tool_call
        .fields
        .title
        .clone()
        .unwrap_or_else(|| request.tool_call.tool_call_id.0.to_string())
}

fn option(offered: &agent_client_protocol_schema::v1::PermissionOption) -> QuestionOption {
    QuestionOption {
        id: offered.option_id.0.to_string(),
        label: offered.name.clone(),
        description: None,
    }
}

/// The answer, in the agent's own vocabulary. Anything that is not a choice of
/// one of its ids is a cancellation: refusing to guess is what keeps a person
/// from allowing something they did not read.
pub fn outcome(answer: &Answer, request: &RequestPermissionRequest) -> RequestPermissionResponse {
    let picked = match answer {
        Answer::Choice { ids } => ids.first().and_then(|id| offered(request, id)),
        _ => None,
    };
    let outcome = match picked {
        Some(id) => RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(id)),
        None => RequestPermissionOutcome::Cancelled,
    };
    RequestPermissionResponse::new(outcome)
}

/// An id the agent did not offer is not an answer to its question.
fn offered(request: &RequestPermissionRequest, id: &str) -> Option<PermissionOptionId> {
    request
        .options
        .iter()
        .find(|option| option.option_id.0.as_ref() == id)
        .map(|option| option.option_id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;
    use serde_json::Value;

    fn request(recorded: Value) -> RequestPermissionRequest {
        serde_json::from_value(recorded).expect("a recorded request parses")
    }

    fn ids(kind: &InteractionKind) -> Vec<String> {
        let InteractionKind::Question { options, .. } = kind else {
            panic!("a permission request is asked as a question");
        };
        options.iter().map(|o| o.id.clone()).collect()
    }

    fn answered(recorded: Value, answer: Answer) -> Value {
        let request = request(recorded);
        serde_json::to_value(outcome(&answer, &request)).expect("an outcome serialises")
    }

    /// Both adapters, both option sets, and neither id is derivable from the
    /// other's — which is the whole reason the person sees the agent's own
    /// options rather than bingo's allow and deny.
    #[test]
    fn the_person_is_shown_the_agents_own_options() {
        let claude = request(fixtures::request_permission());
        let (kind, answers) = question(&claude, "claude");
        assert_eq!(ids(&kind), ["allow-once", "allow-with-updates", "reject"]);
        assert_eq!(answers, [AnswerSpec::Choice, AnswerSpec::Cancel]);
        let InteractionKind::Question {
            question: asked,
            header,
            ..
        } = &kind
        else {
            panic!("a question");
        };
        assert_eq!(asked, "Edit src/lib.rs");
        assert_eq!(header.as_deref(), Some("claude asks"));

        let codex = request(fixtures::request_permission_codex());
        let (kind, _) = question(&codex, "codex-acp");
        assert_eq!(
            ids(&kind),
            ["allow_once", "allow_for_session", "decline", "cancel"],
            "four options, two of them rejects, and none of them claude's"
        );
    }

    #[test]
    fn the_chosen_id_goes_back_exactly_as_the_agent_wrote_it() {
        assert_eq!(
            answered(
                fixtures::request_permission(),
                Answer::Choice {
                    ids: vec!["allow-once".into()]
                }
            ),
            fixtures::request_permission_selected()
        );
        assert_eq!(
            answered(
                fixtures::request_permission_codex(),
                Answer::Choice {
                    ids: vec!["allow_for_session".into()]
                }
            )["outcome"]["optionId"],
            "allow_for_session"
        );
    }

    /// A cancelled interaction, a denial, a plain confirm — none of these name
    /// an option the agent offered, and inventing one would allow something
    /// nobody read.
    #[test]
    fn anything_that_is_not_one_of_the_agents_ids_is_a_cancellation() {
        for answer in [
            Answer::Cancel,
            Answer::AllowOnce,
            Answer::Confirm,
            Answer::Deny { feedback: None },
            Answer::Text {
                text: "allow-once".into(),
            },
            Answer::Choice {
                ids: vec!["allow_once".into()],
            },
        ] {
            assert_eq!(
                answered(fixtures::request_permission(), answer.clone()),
                fixtures::request_permission_cancelled(),
                "{answer:?} names no option claude offered"
            );
        }
    }

    /// A call with no title is still a question; naming it by its id beats
    /// asking a person to approve a blank.
    #[test]
    fn a_call_with_no_title_is_named_by_its_id() {
        let bare = request(serde_json::json!({
            "sessionId": "s",
            "toolCall": { "toolCallId": "call_9" },
            "options": [{ "optionId": "yes", "name": "Yes", "kind": "allow_once" }]
        }));
        let (kind, _) = question(&bare, "acp");
        let InteractionKind::Question {
            question: asked, ..
        } = &kind
        else {
            panic!("a question");
        };
        assert_eq!(asked, "call_9");
    }
}
