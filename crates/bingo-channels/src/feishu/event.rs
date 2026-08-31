//! What an event payload means (ADR-0016 §6).
//!
//! Two events matter: someone said something, and someone pressed a button.
//! Both are at-least-once, so both carry the `event_id` the reader dedupes on.
//! Nothing here does I/O, and nothing here reads intent out of prose — whether
//! the bot was addressed comes from the platform's `mentions`, never from the
//! text, because text is what a person writes and a mention is what a person
//! meant.

use std::collections::{HashSet, VecDeque};

use serde_json::{Value, json};

use super::posted::Handle;
use crate::adapter::Incoming;
use crate::conversation::Conversation;
use crate::question::Question;
use bingo_sdk::InteractionId;

const MESSAGE: &str = "im.message.receive_v1";
const CARD_ACTION: &str = "card.action.trigger";

/// The key a button's value carries ours under, so a callback from somebody
/// else's card is never mistaken for an answer.
const OURS: &str = "bingo";

/// One event, and what this surface makes of it. A well-formed event with
/// nothing to do still carries its id: it has been seen, and a redelivery of
/// it should be seen no more than once.
#[derive(Clone, Debug, PartialEq)]
pub struct Heard {
    pub id: String,
    pub incoming: Option<Incoming>,
}

/// `me` is this bot's own open id, read once at startup: there is no
/// `is_mentioned` flag, only a list to look ourselves up in.
pub fn heard(payload: &[u8], me: &str) -> Option<Heard> {
    let event: Value = serde_json::from_slice(payload).ok()?;
    let id = event["header"]["event_id"].as_str()?.to_string();
    let incoming = match event["header"]["event_type"].as_str()? {
        MESSAGE => message(&event["event"], me),
        CARD_ACTION => click(&event["event"]),
        _ => None,
    };
    Some(Heard { id, incoming })
}

fn message(event: &Value, me: &str) -> Option<Incoming> {
    let message = &event["message"];
    let chat = message["chat_id"].as_str()?;
    // Only `p2p` and `group` exist; a topic thread is a group with a thread id.
    let group = message["chat_type"].as_str()? != "p2p";
    let conversation = match message["thread_id"].as_str() {
        Some(thread) => Conversation::group(chat).in_thread(thread),
        None if group => Conversation::group(chat),
        None => Conversation::direct(chat),
    };
    let mentions = &message["mentions"];
    Some(Incoming::Message {
        addressed: !group || mentions_me(mentions, me),
        text: spoken(message, mentions, me)?,
        principal: event["sender"]["sender_id"]["open_id"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        parent: message["message_id"]
            .as_str()
            .map(|id| Handle::Message(id.to_string()).posted()),
        conversation,
    })
}

fn mentions_me(mentions: &Value, me: &str) -> bool {
    mentions
        .as_array()
        .is_some_and(|mentions| mentions.iter().any(|m| m["id"]["open_id"] == me))
}

/// The words, with the `@_user_N` placeholders resolved: ours removed, the
/// rest replaced by the name a person would have read on the screen.
fn spoken(message: &Value, mentions: &Value, me: &str) -> Option<String> {
    let content: Value = serde_json::from_str(message["content"].as_str()?).ok()?;
    let text = match message["message_type"].as_str()? {
        "text" => content["text"].as_str()?.to_string(),
        "post" => post(&content),
        // Files, images and audio are not this surface's (M13 non-goals).
        _ => return None,
    };
    Some(resolve(&text, mentions, me).trim().to_string())
}

/// A rich-text message flattened: its text runs, one line per paragraph.
fn post(content: &Value) -> String {
    let paragraphs = content["content"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    paragraphs
        .iter()
        .map(|runs| {
            runs.as_array()
                .map(Vec::as_slice)
                .unwrap_or(&[])
                .iter()
                .filter_map(|run| run["text"].as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn resolve(text: &str, mentions: &Value, me: &str) -> String {
    let mut text = text.to_string();
    for mention in mentions.as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let Some(key) = mention["key"].as_str() else {
            continue;
        };
        let replacement = match mention["id"]["open_id"] == me {
            true => String::new(),
            false => format!("@{}", mention["name"].as_str().unwrap_or("someone")),
        };
        text = text.replace(key, &replacement);
    }
    text
}

fn click(event: &Value) -> Option<Incoming> {
    let ours = &event["action"]["value"][OURS];
    let chat = ours["chat"]
        .as_str()
        .or_else(|| event["context"]["open_chat_id"].as_str())?;
    let conversation = match (ours["thread"].as_str(), ours["group"].as_bool()) {
        (Some(thread), _) => Conversation::group(chat).in_thread(thread),
        (None, Some(true)) => Conversation::group(chat),
        (None, _) => Conversation::direct(chat),
    };
    Some(Incoming::Click {
        question: InteractionId::from_raw(ours["interaction"].as_str()?),
        choice: ours["choice"].as_str()?.to_string(),
        principal: event["operator"]["open_id"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        conversation,
    })
}

/// What a button carries back: enough to name the question, the choice and
/// the conversation, so a click never has to be guessed at from the chat id.
pub fn button_value(to: &Conversation, question: &Question, choice: &str) -> Value {
    json!({
        OURS: {
            "interaction": question.id.as_str(),
            "choice": choice,
            "chat": to.chat,
            "thread": to.thread,
            "group": to.group,
        }
    })
}

/// Events already handled. The peer delivers at least once, so an id that has
/// been seen is dropped; the ring is bounded because a connection can live for
/// weeks and a set that only grows is a leak.
#[derive(Debug)]
pub struct Seen {
    ids: HashSet<String>,
    order: VecDeque<String>,
    keep: usize,
}

impl Default for Seen {
    fn default() -> Self {
        Self::keeping(4096)
    }
}

impl Seen {
    pub fn keeping(keep: usize) -> Self {
        Self {
            ids: HashSet::new(),
            order: VecDeque::new(),
            keep,
        }
    }

    /// Whether this is the first time; a repeat answers `false`.
    pub fn first(&mut self, id: &str) -> bool {
        if !self.ids.insert(id.to_string()) {
            return false;
        }
        self.order.push_back(id.to_string());
        if self.order.len() > self.keep
            && let Some(oldest) = self.order.pop_front()
        {
            self.ids.remove(&oldest);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "ou_bot";

    fn payload(event_type: &str, event: Value) -> Vec<u8> {
        json!({
            "schema": "2.0",
            "header": { "event_id": "evt_1", "event_type": event_type },
            "event": event,
        })
        .to_string()
        .into_bytes()
    }

    fn text_message(chat_type: &str, content: Value, mentions: Value) -> Vec<u8> {
        payload(
            MESSAGE,
            json!({
                "sender": { "sender_id": { "open_id": "ou_person" } },
                "message": {
                    "message_id": "om_1",
                    "chat_id": "oc_1",
                    "chat_type": chat_type,
                    "message_type": "text",
                    "content": content.to_string(),
                    "mentions": mentions,
                },
            }),
        )
    }

    fn mention_of(open_id: &str, name: &str) -> Value {
        json!([{ "key": "@_user_1", "id": { "open_id": open_id }, "name": name }])
    }

    #[test]
    fn a_direct_message_is_always_addressed_to_the_bot() {
        let heard = heard(
            &text_message("p2p", json!({"text": "run the tests"}), json!([])),
            ME,
        )
        .expect("an event");
        assert_eq!(heard.id, "evt_1");
        assert_eq!(
            heard.incoming,
            Some(Incoming::Message {
                conversation: Conversation::direct("oc_1"),
                principal: "ou_person".into(),
                text: "run the tests".into(),
                addressed: true,
                parent: Some(Handle::Message("om_1".into()).posted()),
            })
        );
    }

    #[test]
    fn a_group_is_addressed_only_when_the_mentions_name_this_bot() {
        let mentioned = heard(
            &text_message(
                "group",
                json!({"text": "@_user_1 run the tests"}),
                mention_of(ME, "bingo"),
            ),
            ME,
        )
        .expect("an event");
        let Some(Incoming::Message {
            addressed, text, ..
        }) = mentioned.incoming
        else {
            panic!("a message");
        };
        assert!(addressed);
        assert_eq!(text, "run the tests", "our own placeholder is taken out");

        let overheard = heard(
            &text_message(
                "group",
                json!({"text": "@_user_1 run the tests"}),
                mention_of("ou_someone_else", "Wei"),
            ),
            ME,
        )
        .expect("an event");
        let Some(Incoming::Message {
            addressed, text, ..
        }) = overheard.incoming
        else {
            panic!("a message");
        };
        assert!(!addressed, "there is no is_mentioned flag, only the list");
        assert_eq!(text, "@Wei run the tests", "somebody else keeps their name");
    }

    #[test]
    fn a_topic_thread_is_a_group_with_a_thread_of_its_own() {
        let mut event: Value = serde_json::from_slice(&text_message(
            "group",
            json!({"text": "@_user_1 go"}),
            mention_of(ME, "bingo"),
        ))
        .expect("json");
        event["event"]["message"]["thread_id"] = json!("omt_9");
        let heard = heard(event.to_string().as_bytes(), ME).expect("an event");
        let Some(Incoming::Message { conversation, .. }) = heard.incoming else {
            panic!("a message");
        };
        assert_eq!(conversation.path(), "oc_1/omt_9");
        assert!(conversation.group);
    }

    #[test]
    fn a_rich_text_message_arrives_as_its_lines() {
        let mut event: Value =
            serde_json::from_slice(&text_message("p2p", json!({}), json!([]))).expect("json");
        event["event"]["message"]["message_type"] = json!("post");
        event["event"]["message"]["content"] = json!(
            json!({
                "title": "",
                "content": [
                    [{ "tag": "text", "text": "first" }],
                    [{ "tag": "text", "text": "sec" }, { "tag": "text", "text": "ond" }],
                ],
            })
            .to_string()
        );
        let heard = heard(event.to_string().as_bytes(), ME).expect("an event");
        let Some(Incoming::Message { text, .. }) = heard.incoming else {
            panic!("a message");
        };
        assert_eq!(text, "first\nsecond");
    }

    #[test]
    fn a_file_is_seen_and_left_alone() {
        let mut event: Value =
            serde_json::from_slice(&text_message("p2p", json!({}), json!([]))).expect("json");
        event["event"]["message"]["message_type"] = json!("image");
        let heard = heard(event.to_string().as_bytes(), ME).expect("an event");
        assert_eq!(heard.id, "evt_1", "it is still acked and still deduped");
        assert_eq!(heard.incoming, None);
    }

    #[test]
    fn a_button_carries_back_the_question_the_choice_and_where_it_was_asked() {
        let conversation = Conversation::group("oc_1").in_thread("omt_9");
        let question = Question {
            id: InteractionId::from_raw("int_1"),
            prompt: "p".into(),
            choices: Vec::new(),
            free_text: false,
        };
        let value = button_value(&conversation, &question, "2");
        let heard = heard(
            &payload(
                CARD_ACTION,
                json!({
                    "operator": { "open_id": "ou_person" },
                    "action": { "tag": "button", "value": value },
                    "context": { "open_chat_id": "oc_1", "open_message_id": "om_2" },
                    "token": "c-abc",
                }),
            ),
            ME,
        )
        .expect("an event");
        assert_eq!(
            heard.incoming,
            Some(Incoming::Click {
                conversation,
                principal: "ou_person".into(),
                question: InteractionId::from_raw("int_1"),
                choice: "2".into(),
            })
        );
    }

    #[test]
    fn a_callback_from_someone_elses_card_is_not_an_answer() {
        let heard = heard(
            &payload(
                CARD_ACTION,
                json!({
                    "operator": { "open_id": "ou_person" },
                    "action": { "value": { "somebody": "else" } },
                    "context": { "open_chat_id": "oc_1" },
                }),
            ),
            ME,
        )
        .expect("an event");
        assert_eq!(heard.incoming, None);
    }

    #[test]
    fn an_event_this_build_has_no_use_for_is_still_an_event() {
        assert!(heard(b"not json", ME).is_none(), "and a non-event is not");
        let unused = heard(&payload("im.chat.updated_v1", json!({})), ME).expect("an event");
        assert_eq!(unused.id, "evt_1");
        assert_eq!(unused.incoming, None);
    }

    #[test]
    fn a_redelivered_event_is_handled_once_and_the_ring_stays_bounded() {
        let mut seen = Seen::keeping(2);
        assert!(seen.first("a"));
        assert!(!seen.first("a"));
        assert!(seen.first("b"));
        assert!(seen.first("c"));
        assert!(
            seen.first("a"),
            "the oldest id fell out, which is the price of a bounded ring"
        );
        assert_eq!(seen.order.len(), 2);
    }
}
