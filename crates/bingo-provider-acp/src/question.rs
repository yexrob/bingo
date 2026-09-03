//! An agent's `session/request_permission` as a question for a person, and the
//! answer on its way back (ADR-0039 §3).
//!
//! The mapping judges nothing. The options are the agent's own — ids kept
//! verbatim, labels its own words — and what comes back is one of those ids,
//! sent back as it was given. Two of the options are marked, because a session
//! that answers for the person can only answer in the asker's own words
//! (ADR-0039 §2): the narrowest yes is the allowing one, the narrowest no is
//! the refusal every unanswered question falls to. The `always` variants stay
//! unmarked wherever the agent also offers the narrow ones — a standing
//! decision is a person's to make, not a session's.
//!
//! The row still speaks first (ADR-0039 §4): an adapter whose own row says
//! what it may do never asks, and none of this happens.

use agent_client_protocol_schema::v1::{
    PermissionOptionId, PermissionOptionKind, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome,
};
use bingo_sdk::{Answer, AnswerRole, AnswerSpec, InteractionKind, QuestionOption};

/// What a person is asked, in the agent's own words.
pub fn asked(adapter: &str, request: &RequestPermissionRequest) -> InteractionKind {
    let (yes, no) = (allowing(request), refusing(request));
    InteractionKind::Question {
        question: about(request),
        // The adapter's name, so a person reading the prompt knows which agent
        // is asking before they read what it wants.
        header: Some(adapter.to_string()),
        options: request
            .options
            .iter()
            .map(|option| QuestionOption {
                id: option.option_id.to_string(),
                label: option.name.clone(),
                description: None,
                role: role_of(&option.option_id, yes.as_ref(), no.as_ref()),
            })
            .collect(),
        free_text: false,
        multi: false,
    }
}

/// The answers the kernel will take. `Cancel` is offered because a surface with
/// nobody at the keyboard declines what it is handed with it, and a question no
/// surface can decline is a question that waits for ever (`HostApi::ask`).
pub fn answers() -> Vec<AnswerSpec> {
    vec![AnswerSpec::Choice, AnswerSpec::Cancel]
}

/// The agent's outcome for what came back, when what came back is one of the
/// options the agent itself offered. `None` is every other answer — a surface
/// that cancelled, an id this agent never named — which is nobody having
/// answered, and the caller's to say so and fall closed.
pub fn picked(
    request: &RequestPermissionRequest,
    answer: &Answer,
) -> Option<RequestPermissionResponse> {
    let Answer::Choice { ids } = answer else {
        return None;
    };
    let chosen = ids.first()?;
    let option = request
        .options
        .iter()
        .find(|option| option.option_id.to_string() == *chosen)?;
    Some(selected(option.option_id.clone()))
}

/// The agent's own way of saying no. `reject_once` comes first: refusing this
/// call is the answer, and a standing `reject_always` would be a decision
/// nobody made — it is taken only when it is the only refusal on offer.
pub fn refusing(request: &RequestPermissionRequest) -> Option<PermissionOptionId> {
    offered(request, PermissionOptionKind::RejectOnce)
        .or_else(|| offered(request, PermissionOptionKind::RejectAlways))
}

/// One of the agent's options, chosen.
pub fn selected(option: PermissionOptionId) -> RequestPermissionResponse {
    RequestPermissionResponse::new(RequestPermissionOutcome::Selected(
        SelectedPermissionOutcome::new(option),
    ))
}

/// The agent's own way of saying yes, narrowest first, for the same reason.
fn allowing(request: &RequestPermissionRequest) -> Option<PermissionOptionId> {
    offered(request, PermissionOptionKind::AllowOnce)
        .or_else(|| offered(request, PermissionOptionKind::AllowAlways))
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

fn role_of(
    option: &PermissionOptionId,
    yes: Option<&PermissionOptionId>,
    no: Option<&PermissionOptionId>,
) -> Option<AnswerRole> {
    match option {
        _ if yes == Some(option) => Some(AnswerRole::Allowing),
        _ if no == Some(option) => Some(AnswerRole::Refusing),
        _ => None,
    }
}

/// What the agent wants to do, in its own title for the call — nothing of this
/// client's is added to it. A call that named itself nothing is asked about by
/// its id, which is at least something a person can match against the
/// transcript.
fn about(request: &RequestPermissionRequest) -> String {
    let call = &request.tool_call;
    call.fields
        .title
        .clone()
        .unwrap_or_else(|| call.tool_call_id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;
    use serde_json::{Value, json};

    fn request(recorded: Value) -> RequestPermissionRequest {
        serde_json::from_value(recorded).expect("a recorded request parses")
    }

    fn question(recorded: Value) -> (String, Option<String>, Vec<QuestionOption>) {
        let InteractionKind::Question {
            question,
            header,
            options,
            free_text,
            multi,
        } = asked("scripted", &request(recorded))
        else {
            panic!("a permission request is a question");
        };
        assert!(!free_text, "the agent named the answers it will take");
        assert!(!multi, "one of them");
        (question, header, options)
    }

    fn roles(options: &[QuestionOption]) -> Vec<(&str, Option<AnswerRole>)> {
        options
            .iter()
            .map(|option| (option.id.as_str(), option.role))
            .collect()
    }

    /// The agent's words cross untranslated: its ids, its labels, and the two
    /// roles a session may answer for a person.
    #[test]
    fn the_agents_own_options_become_the_questions_own() {
        let (question, header, options) = question(fixtures::request_permission());
        assert_eq!(question, "Edit src/lib.rs");
        assert_eq!(header.as_deref(), Some("scripted"));
        assert_eq!(
            options.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(),
            [
                "Yes",
                "Yes, and don't ask again for edits to this file",
                "No"
            ]
        );
        assert_eq!(
            roles(&options),
            [
                ("allow-once", Some(AnswerRole::Allowing)),
                ("allow-with-updates", None),
                ("reject", Some(AnswerRole::Refusing))
            ],
            "the narrow answers are the session's; the standing one is a person's"
        );
    }

    /// Codex's four options, whose ids resemble neither the kinds nor the other
    /// adapter's: the first of each kind is the marked one, and the second
    /// reject stays a person's.
    #[test]
    fn a_second_option_of_the_same_kind_is_left_to_the_person() {
        let (_, _, options) = question(fixtures::request_permission_codex());
        assert_eq!(
            roles(&options),
            [
                ("allow_once", Some(AnswerRole::Allowing)),
                ("allow_for_session", None),
                ("decline", Some(AnswerRole::Refusing)),
                ("cancel", None)
            ]
        );
    }

    /// An agent that offers only standing answers still gets a question a
    /// session can answer: the narrowest on offer is the marked one.
    #[test]
    fn a_standing_option_is_marked_when_it_is_the_only_one_of_its_kind() {
        let (_, _, options) = question(json!({
            "sessionId": "s",
            "toolCall": { "toolCallId": "c1" },
            "options": [
                { "optionId": "always", "name": "Always", "kind": "allow_always" },
                { "optionId": "never", "name": "Never", "kind": "reject_always" }
            ]
        }));
        assert_eq!(
            roles(&options),
            [
                ("always", Some(AnswerRole::Allowing)),
                ("never", Some(AnswerRole::Refusing))
            ]
        );
    }

    /// A call with no title of its own is still asked about by name.
    #[test]
    fn a_call_that_named_nothing_is_named_by_its_id() {
        let (question, _, _) = question(json!({
            "sessionId": "s",
            "toolCall": { "toolCallId": "c1" },
            "options": []
        }));
        assert_eq!(question, "c1");
    }

    /// The chosen id goes back exactly as the agent gave it.
    #[test]
    fn the_persons_choice_is_the_agents_own_option_id() {
        let asked = request(fixtures::request_permission());
        let chosen = Answer::Choice {
            ids: vec!["allow-once".into()],
        };
        let answered = serde_json::to_value(picked(&asked, &chosen).expect("an option was picked"))
            .expect("an outcome serialises");
        assert_eq!(answered, fixtures::request_permission_selected());
    }

    /// Nobody answered: a surface that cancelled, and an id this agent never
    /// offered, are both no answer at all. What to do about it is the caller's.
    #[test]
    fn an_answer_that_is_not_one_of_the_agents_options_is_no_answer() {
        let asked = request(fixtures::request_permission());
        assert!(picked(&asked, &Answer::Cancel).is_none());
        assert!(picked(&asked, &Answer::AllowOnce).is_none());
        assert!(
            picked(
                &asked,
                &Answer::Choice {
                    ids: vec!["allow_once".into()]
                }
            )
            .is_none(),
            "the other adapter's spelling is not this one's option"
        );
    }

    /// `reject_always` would teach the agent a standing no; it is the fallback,
    /// not the answer.
    #[test]
    fn a_standing_refusal_is_only_taken_when_it_is_the_only_one() {
        let both = request(json!({
            "sessionId": "s",
            "toolCall": { "toolCallId": "c1" },
            "options": [
                { "optionId": "never", "name": "Never", "kind": "reject_always" },
                { "optionId": "no", "name": "No", "kind": "reject_once" }
            ]
        }));
        assert_eq!(refusing(&both).map(|id| id.to_string()), Some("no".into()));
        let standing = request(json!({
            "sessionId": "s",
            "toolCall": { "toolCallId": "c1" },
            "options": [{ "optionId": "never", "name": "Never", "kind": "reject_always" }]
        }));
        assert_eq!(
            refusing(&standing).map(|id| id.to_string()),
            Some("never".into())
        );
        assert_eq!(
            refusing(&request(json!({
                "sessionId": "s",
                "toolCall": { "toolCallId": "c1" },
                "options": [{ "optionId": "yes", "name": "Yes", "kind": "allow_once" }]
            }))),
            None,
            "an agent that offers no way to refuse has none"
        );
    }
}
