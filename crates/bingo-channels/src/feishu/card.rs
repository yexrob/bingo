//! The card bodies, as JSON (ADR-0016 §6).
//!
//! Two kinds, and they are not interchangeable. A **streaming** card is a
//! CardKit entity: created, sent by `card_id`, then updated element by element
//! with the whole text every time. A **question** card is sent inline as an
//! interactive message, because a card sent by `card_id` cannot be edited
//! through `im/v1` at all — and because callbacks are blocked while a stream
//! is open, so a question is never buttons on the answer.
//!
//! Cards have no code-block component: code is a triple-backtick fence inside
//! the `markdown` element.

use serde_json::{Value, json};

use super::event::button_value;
use crate::conversation::Conversation;
use crate::limits::Limits;
use crate::question::Question;

/// The element a streamed answer is written into. Fixed, so an update never
/// has to look one up.
pub const ANSWER: &str = "answer";

/// A card entity that will be streamed into. `streaming_mode` is what makes
/// the platform draw the text as it grows — and what closes the card to
/// callbacks until it is turned off again.
pub fn streaming() -> Value {
    json!({
        "schema": "2.0",
        "config": { "streaming_mode": true, "update_multi": true },
        "body": {
            "elements": [
                { "tag": "markdown", "element_id": ANSWER, "content": "" },
            ],
        },
    })
}

/// What `POST /open-apis/cardkit/v1/cards` takes: the card, serialised.
pub fn entity(card: &Value) -> Value {
    json!({ "type": "card_json", "data": card.to_string() })
}

/// What `im/v1` takes to send a card entity by id.
pub fn by_id(card_id: &str) -> Value {
    json!({ "type": "card", "data": { "card_id": card_id } })
}

/// A question, with its buttons. The value each button carries is enough to
/// route the click on its own (`event::button_value`).
pub fn question(to: &Conversation, question: &Question, limits: &Limits) -> Value {
    let buttons: Vec<Value> = question
        .buttons(limits)
        .unwrap_or_default()
        .iter()
        .map(|choice| {
            json!({
                "tag": "button",
                "text": { "tag": "plain_text", "content": choice.label },
                "type": if choice.key == "1" { "primary" } else { "default" },
                "behaviors": [{
                    "type": "callback",
                    "value": button_value(to, question, &choice.key),
                }],
            })
        })
        .collect();
    let mut elements = vec![markdown(&question.prompt)];
    if !buttons.is_empty() {
        elements.push(json!({ "tag": "action", "actions": buttons }));
    }
    card(elements)
}

/// The same question with the buttons taken off and the outcome under it —
/// what a resolution anywhere leaves behind (ADR-0016 §3).
pub fn settled(prompt: &str, outcome: &str) -> Value {
    card(vec![markdown(prompt), markdown(&format!("_{outcome}_"))])
}

fn card(elements: Vec<Value>) -> Value {
    json!({ "schema": "2.0", "body": { "elements": elements } })
}

fn markdown(content: &str) -> Value {
    json!({ "tag": "markdown", "content": content })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::{Dialect, Encoding};
    use crate::question::Choice;
    use bingo_sdk::{Answer, InteractionId};

    fn limits() -> Limits {
        Limits {
            max_text: (20_000, Encoding::Utf8Bytes),
            dialect: Dialect::Markdown,
            max_actions: 4,
            max_label: 30,
        }
    }

    fn permission() -> Question {
        Question {
            id: InteractionId::from_raw("int_1"),
            prompt: "Bash: run `cargo test`".into(),
            choices: vec![
                Choice {
                    key: "1".into(),
                    label: "Allow once".into(),
                    answer: Answer::AllowOnce,
                },
                Choice {
                    key: "2".into(),
                    label: "Deny".into(),
                    answer: Answer::Deny { feedback: None },
                },
            ],
            free_text: false,
        }
    }

    /// The bodies are the contract with the platform: a change here is a
    /// change to what a person sees, and to what a click carries back.
    #[test]
    fn the_card_bodies_are_pinned() {
        insta::assert_json_snapshot!(
            "feishu-cards",
            json!({
                "streaming": streaming(),
                "entity": entity(&streaming()),
                "by_id": by_id("ctp_1"),
                "question": question(
                    &Conversation::group("oc_1").in_thread("omt_9"),
                    &permission(),
                    &limits(),
                ),
                "settled": settled("Bash: run `cargo test`", "approved in the TUI"),
            })
        );
    }

    #[test]
    fn a_question_that_will_not_fit_in_buttons_is_a_card_with_none() {
        let narrow = Limits {
            max_actions: 1,
            ..limits()
        };
        let card = question(&Conversation::direct("oc_1"), &permission(), &narrow);
        let elements = card["body"]["elements"].as_array().expect("elements");
        assert_eq!(elements.len(), 1, "no action element at all: {card}");
    }

    #[test]
    fn a_streaming_card_is_created_with_its_element_already_named() {
        let card = streaming();
        assert_eq!(card["config"]["streaming_mode"], json!(true));
        assert_eq!(card["body"]["elements"][0]["element_id"], json!(ANSWER));
    }
}
