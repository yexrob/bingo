//! The second reducer: journal → provider messages. Also the one ruler for
//! context size, used by the compaction trigger and by every display.

pub mod budget;
pub mod elide;

use std::collections::HashSet;

use bingo_sdk::*;
use serde_json::Value;

/// Folds items into the messages a provider receives. Pure; the golden tests
/// per journal version live next to it.
#[derive(Debug)]
pub struct ContextView;

impl ContextView {
    /// Apply `Compacted` and `Rewound` to the items in a journal, then fold.
    pub fn fold(frames: &[Frame]) -> Vec<Message> {
        Self::fold_items(&Self::items(frames))
    }

    /// The transcript after compaction and rewind, in order.
    pub fn items(frames: &[Frame]) -> Vec<Item> {
        let mut items: Vec<Item> = Vec::new();
        for frame in frames {
            match &frame.event {
                Event::ItemStarted { item }
                | Event::ItemUpdated { item }
                | Event::ItemCompleted { item } => {
                    match items.iter_mut().find(|i| i.id == item.id) {
                        Some(slot) => *slot = item.clone(),
                        None => items.push(item.clone()),
                    }
                }
                Event::Compacted {
                    boundary,
                    kept,
                    summary,
                    ..
                } => {
                    // A boundary this journal never saw is not a cut it can make.
                    if let Some(cut) = items.iter().position(|i| &i.id == boundary) {
                        splice_compaction(&mut items, cut, kept, summary);
                    }
                }
                Event::Rewound { dropped, .. } => items.retain(|i| !dropped.contains(&i.id)),
                _ => {}
            }
        }
        items
    }

    pub fn fold_items(items: &[Item]) -> Vec<Message> {
        let mut out = Folder {
            mixed: mixed_turns(items),
            ..Default::default()
        };
        for item in items {
            out.item(item);
        }
        out.finish()
    }
}

/// The one splice a compaction performs: before `cut` only `kept` survives, the
/// summary item takes the seam, the tail is untouched.
pub(crate) fn splice_compaction(
    items: &mut Vec<Item>,
    cut: usize,
    kept: &[ItemId],
    summary: &ItemId,
) {
    let summary_item = items
        .iter()
        .position(|i| &i.id == summary)
        .map(|p| items.remove(p));
    let cut = cut.min(items.len());
    let (head, tail) = items.split_at(cut);
    let mut next: Vec<Item> = head
        .iter()
        .filter(|i| kept.contains(&i.id))
        .cloned()
        .collect();
    next.extend(summary_item);
    next.extend(tail.iter().cloned());
    *items = next;
}

/// What the model reads when a journal opens on its own words: the API wants
/// a person to speak first, and nothing is invented about what they said.
const OPENING_NOTE: &str = "[The conversation begins here.]";

#[derive(Default)]
struct Folder {
    messages: Vec<Message>,
    /// Tool results owed to the next user message; they always come first in it.
    pending: Vec<ContentPart>,
    /// The turn and round of the open assistant message: every tool call of
    /// one response joins it, and their results join the one user message
    /// after it, as the model produced them.
    round: Option<(Option<TurnId>, u32)>,
    /// The turns in which the person's own lines are marked, because
    /// something that is not theirs speaks unlabelled in the same turn.
    mixed: HashSet<Option<TurnId>>,
}

impl Folder {
    /// One item as the provider sees it; a body with no wire form is skipped.
    fn item(&mut self, item: &Item) {
        let round = (item.turn.clone(), item.round);
        match &item.body {
            ItemBody::User { parts, origin } => {
                self.user(spoken(parts, origin, self.mixed.contains(&item.turn)))
            }
            ItemBody::Assistant { text } => self.text(text),
            ItemBody::Reasoning {
                text,
                provider_metadata,
            } => self.reasoning(text, provider_metadata),
            // A call the model never asked for is not its to answer: it was
            // handed in from outside the turn and its outcome went back to
            // whoever handed it in (ADR-0036 §2). Replaying it here would put
            // words in the model's mouth that it never said.
            ItemBody::ToolCall { .. } if item.external() => {}
            ItemBody::ToolCall {
                call_id,
                name,
                input,
                output,
                ..
            } => self.tool_call(round, call_id, name, input, output.as_ref()),
            ItemBody::Compaction { summary, .. } => {
                self.note(format!("[Summary of the conversation so far]\n{summary}"))
            }
            ItemBody::Interruption { marker } => self.note(marker.clone()),
            ItemBody::QuestionAnswer {
                question, answer, ..
            } => self.note(format!("Q: {question}\nA: {answer}")),
            ItemBody::Shell {
                command,
                output,
                exit,
                ..
            } => self.note(shell_note(command, output, *exit)),
            ItemBody::Action {
                name,
                args,
                result: Some(result),
            } => self.note(format!("[{name}] {}\n{}", plain(args), plain(result))),
            ItemBody::Action { result: None, .. }
            | ItemBody::Rewind { .. }
            | ItemBody::Notice { .. }
            | ItemBody::PermissionReceipt { .. }
            | ItemBody::Asset { .. } => {}
        }
    }

    /// The kernel speaking to the model in the user's turn.
    fn note(&mut self, text: String) {
        self.user(vec![ContentPart::text(text)]);
    }

    fn text(&mut self, text: &str) {
        if !text.is_empty() {
            self.assistant(vec![ContentPart::text(text.to_string())]);
        }
    }

    /// Reasoning with no text still goes back when it carries the provider's
    /// replay data: an encrypted chain of thought without a summary is what
    /// a stateless OpenAI turn gets, and dropping it makes the model think
    /// again from nothing.
    fn reasoning(&mut self, text: &str, provider_metadata: &ProviderMetadata) {
        if !text.is_empty() || !provider_metadata.is_empty() {
            self.assistant(vec![ContentPart::Reasoning {
                text: text.to_string(),
                provider_metadata: provider_metadata.clone(),
            }]);
        }
    }

    /// The call, then the result it owes the next user message. A call with no
    /// output never completed, and the model is told so.
    fn tool_call(
        &mut self,
        round: (Option<TurnId>, u32),
        call_id: &str,
        name: &str,
        input: &Value,
        output: Option<&ToolOutput>,
    ) {
        let part = ContentPart::ToolUse {
            id: call_id.to_string(),
            name: name.to_string(),
            input: input.clone(),
        };
        match self.messages.last_mut() {
            Some(m) if m.role == Role::Assistant && self.round.as_ref() == Some(&round) => {
                m.parts.push(part);
            }
            _ => {
                self.assistant(vec![part]);
                self.round = Some(round);
            }
        }
        let (parts, is_error) = match output {
            Some(o) => (o.parts.clone(), o.is_error),
            None => (
                vec![ContentPart::text("[no result: the call did not complete]")],
                true,
            ),
        };
        self.pending.push(ContentPart::ToolResult {
            tool_use_id: call_id.to_string(),
            parts,
            is_error,
        });
    }

    fn user(&mut self, parts: Vec<ContentPart>) {
        let mut all = std::mem::take(&mut self.pending);
        all.extend(parts);
        match self.messages.last_mut() {
            Some(m) if m.role == Role::User => m.parts.extend(all),
            _ => self.messages.push(Message::user(all)),
        }
    }

    fn assistant(&mut self, parts: Vec<ContentPart>) {
        if !self.pending.is_empty() {
            let owed = std::mem::take(&mut self.pending);
            self.user(owed);
        }
        match self.messages.last_mut() {
            Some(m) if m.role == Role::Assistant => m.parts.extend(parts),
            _ => self.messages.push(Message::assistant(parts)),
        }
    }

    fn finish(mut self) -> Vec<Message> {
        if !self.pending.is_empty() {
            let owed = std::mem::take(&mut self.pending);
            self.user(owed);
        }
        self.messages.retain(|m| !m.parts.is_empty());
        if self
            .messages
            .first()
            .is_some_and(|m| m.role == Role::Assistant)
        {
            self.messages
                .insert(0, Message::text(Role::User, OPENING_NOTE));
        }
        self.messages
    }
}

pub use bingo_sdk::tokens::estimate as estimate_tokens;

/// A JSON value as a person wrote it: a string verbatim, anything else compact.
fn plain(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// A user item that names who spoke, or where, carries that to the model
/// (ADR-0010 §5, ADR-0011): an agent's message, or a post in a group, must
/// not read as the one the session works for, and a reply goes back where
/// the post came from.
fn spoken(parts: &[ContentPart], origin: &Origin, mixed: bool) -> Vec<ContentPart> {
    let Some(label) = marker(origin, mixed) else {
        return parts.to_vec();
    };
    let mut out = Vec::with_capacity(parts.len() + 1);
    out.push(ContentPart::text(format!("[{label}]")));
    out.extend(parts.iter().cloned());
    out
}

/// What stands above a user item: who spoke, where they spoke — or, in a turn
/// that mixes, that this line is the person's own.
fn marker(origin: &Origin, mixed: bool) -> Option<String> {
    speaker(origin).or_else(|| (mixed && the_persons_own(origin)).then(|| THE_PERSON.to_string()))
}

/// `from <principal>`, `from <principal> in <conversation>`, or
/// `in <conversation>`; nothing for the person the session works for.
fn speaker(origin: &Origin) -> Option<String> {
    let principal = origin.principal.as_deref().filter(|s| !s.is_empty());
    let conversation = origin.conversation.as_deref().filter(|s| !s.is_empty());
    match (principal, conversation) {
        (Some(who), Some(place)) => Some(format!("from {who} in {place}")),
        (Some(who), None) => Some(format!("from {who}")),
        (None, Some(place)) => Some(format!("in {place}")),
        (None, None) => None,
    }
}

/// What the person's own line is called when it has to be called something.
const THE_PERSON: &str = "from the person you work for";

/// The surfaces the kernel itself speaks through in a user's turn: a
/// contributor's piece, a hook's, its own. Each signs nothing, exactly as the
/// person's own line signs nothing, so the surface is the only thing that
/// tells them apart — which is why they are named here, beside the rule that
/// reads them, and used from wherever a piece is minted.
pub(crate) const KERNEL_SURFACE: &str = "kernel";
pub(crate) const CONTRIBUTOR_PREFIX: &str = "contributor:";
pub(crate) const HOOK_PREFIX: &str = "hook:";

fn kernel_speaks_through(surface: &str) -> bool {
    surface == KERNEL_SURFACE
        || surface.starts_with(CONTRIBUTOR_PREFIX)
        || surface.starts_with(HOOK_PREFIX)
}

/// The person's own line: signed by nobody, from nowhere, through a door the
/// kernel does not speak through. A door that is not the person has to sign
/// what it sends — a principal, a conversation, or both — because the fold has
/// nothing else to tell it apart by.
pub(crate) fn the_persons_own(origin: &Origin) -> bool {
    speaker(origin).is_none() && !kernel_speaks_through(&origin.surface)
}

/// The turns whose user entries mix the person's own lines with speech that is
/// not theirs. Bareness marks the person by absence, and an absence is legible
/// only while nothing else in the same turn is unlabelled too: a nudge and a
/// direct line coalesce into one turn, and there the person takes a mark of
/// their own. A turn that is all theirs keeps every line bare.
fn mixed_turns(items: &[Item]) -> HashSet<Option<TurnId>> {
    let mut theirs: HashSet<Option<TurnId>> = HashSet::new();
    let mut others: HashSet<Option<TurnId>> = HashSet::new();
    for item in items {
        let ItemBody::User { origin, .. } = &item.body else {
            continue;
        };
        let side = if the_persons_own(origin) {
            &mut theirs
        } else {
            &mut others
        };
        side.insert(item.turn.clone());
    }
    theirs.intersection(&others).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;

    fn item(id: &str, body: ItemBody) -> Item {
        Item {
            id: ItemId::from_raw(id),
            turn: None,
            round: 0,
            status: ItemStatus::Completed,
            started_at: Timestamp::from_second(0).unwrap(),
            completed_at: None,
            intent: None,
            body,
            meta: Default::default(),
        }
    }

    fn user(id: &str, text: &str) -> Item {
        item(
            id,
            ItemBody::User {
                parts: vec![ContentPart::text(text)],
                origin: Origin::surface("t"),
            },
        )
    }

    /// A user item in a named turn, said through `origin`.
    fn said(id: &str, turn: &str, origin: Origin, text: &str) -> Item {
        let mut said = item(
            id,
            ItemBody::User {
                parts: vec![ContentPart::text(text)],
                origin,
            },
        );
        said.turn = Some(TurnId::from_raw(turn));
        said
    }

    /// The person, through a client: nobody signs it and it comes from nowhere.
    fn person() -> Origin {
        Origin::surface("tui")
    }

    fn peer(who: &str) -> Origin {
        Origin {
            surface: "peer".into(),
            principal: Some(who.into()),
            conversation: None,
        }
    }

    /// A delivery from a conversation and nobody in it — a nudge.
    fn posted(place: &str) -> Origin {
        Origin {
            surface: "peer".into(),
            principal: None,
            conversation: Some(place.into()),
        }
    }

    fn texts(messages: &[Message]) -> Vec<Option<&str>> {
        messages
            .iter()
            .flat_map(|m| &m.parts)
            .map(|part| part.as_text())
            .collect()
    }

    fn tool(id: &str, call: &str, output: Option<&str>) -> Item {
        item(
            id,
            ItemBody::ToolCall {
                call_id: call.into(),
                name: "Read".into(),
                input: serde_json::json!({"file_path": "x"}),
                output: output.map(ToolOutput::text),
                progress: None,
                duration_ms: None,
            },
        )
    }

    #[test]
    fn a_tool_round_folds_into_assistant_then_user_with_results_first() {
        let items = vec![
            user("i1", "read x"),
            item(
                "i2",
                ItemBody::Assistant {
                    text: "Looking.".into(),
                },
            ),
            tool("i3", "c1", Some("contents")),
            user("i4", "also this"),
            item(
                "i5",
                ItemBody::Assistant {
                    text: "Done.".into(),
                },
            ),
        ];
        let msgs = ContextView::fold_items(&items);
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0].role, Role::User);
        assert_eq!(msgs[1].role, Role::Assistant);
        assert!(matches!(msgs[1].parts[1], ContentPart::ToolUse { .. }));
        assert_eq!(msgs[2].role, Role::User);
        assert!(
            matches!(&msgs[2].parts[0], ContentPart::ToolResult { tool_use_id, .. } if tool_use_id == "c1")
        );
        assert_eq!(msgs[2].parts[1].as_text(), Some("also this"));
        assert_eq!(msgs[3].role, Role::Assistant);
    }

    /// Two turns, folded into one message: the mark belongs to the turn that
    /// mixes, and a turn of the person's own keeps its line bare.
    #[test]
    fn a_user_item_from_a_named_principal_says_who_spoke() {
        let msgs = ContextView::fold_items(&[
            said("i1", "trn_1", peer("reviewer"), "ship it"),
            said("i2", "trn_2", person(), "ok"),
        ]);
        assert_eq!(msgs.len(), 1);
        assert_eq!(
            texts(&msgs),
            [Some("[from reviewer]"), Some("ship it"), Some("ok")],
            "a person's own line carries no prefix"
        );
    }

    #[test]
    fn a_turn_that_is_all_the_persons_own_lines_stays_bare() {
        let msgs = ContextView::fold_items(&[
            said("i1", "trn_1", person(), "read x"),
            said("i2", "trn_1", person(), "and this"),
        ]);
        assert_eq!(texts(&msgs), [Some("read x"), Some("and this")]);
    }

    #[test]
    fn a_peer_sharing_the_turn_makes_the_persons_line_say_it_is_theirs() {
        let msgs = ContextView::fold_items(&[
            said("i1", "trn_1", peer("reviewer"), "ship it"),
            said("i2", "trn_1", person(), "ok"),
        ]);
        assert_eq!(
            texts(&msgs),
            [
                Some("[from reviewer]"),
                Some("ship it"),
                Some("[from the person you work for]"),
                Some("ok"),
            ]
        );
    }

    /// The failure this rule is for: a nudge and a line typed straight at the
    /// session coalesce into one turn, and the bare line stops reading as the
    /// person's — a model told to stand by takes it for more of the chatter.
    #[test]
    fn a_nudge_sharing_the_turn_makes_the_persons_line_say_it_is_theirs() {
        let msgs = ContextView::fold_items(&[
            said(
                "i1",
                "trn_1",
                posted("#collab"),
                "there is something unread",
            ),
            said("i2", "trn_1", person(), "Hi"),
        ]);
        assert_eq!(
            texts(&msgs),
            [
                Some("[in #collab]"),
                Some("there is something unread"),
                Some("[from the person you work for]"),
                Some("Hi"),
            ]
        );
    }

    /// A contributor's piece signs nothing and comes from nowhere, exactly as
    /// the person's line does; only the surface tells them apart.
    #[test]
    fn a_contributors_piece_sharing_the_turn_does_the_same() {
        let msgs = ContextView::fold_items(&[
            said(
                "i1",
                "trn_1",
                Origin::surface(format!("{CONTRIBUTOR_PREFIX}notes")),
                "[notes, since you last read]",
            ),
            said("i2", "trn_1", person(), "Hi"),
        ]);
        assert_eq!(
            texts(&msgs),
            [
                Some("[notes, since you last read]"),
                Some("[from the person you work for]"),
                Some("Hi"),
            ]
        );
    }

    #[test]
    fn the_kernel_and_its_hooks_speak_unsigned_and_are_not_the_person() {
        let msgs = ContextView::fold_items(&[
            said("i1", "trn_1", Origin::surface(KERNEL_SURFACE), "carry on"),
            said(
                "i2",
                "trn_1",
                Origin::surface(format!("{HOOK_PREFIX}guard")),
                "not yet",
            ),
            said("i3", "trn_1", person(), "ok"),
        ]);
        assert_eq!(
            texts(&msgs),
            [
                Some("carry on"),
                Some("not yet"),
                Some("[from the person you work for]"),
                Some("ok"),
            ]
        );
    }

    #[test]
    fn a_turn_the_person_never_spoke_in_is_marked_nowhere() {
        let msgs = ContextView::fold_items(&[
            said(
                "i1",
                "trn_1",
                posted("#collab"),
                "there is something unread",
            ),
            said(
                "i2",
                "trn_1",
                Origin::surface(format!("{CONTRIBUTOR_PREFIX}notes")),
                "[notes, since you last read]",
            ),
            said("i3", "trn_1", Origin::surface(KERNEL_SURFACE), "carry on"),
        ]);
        assert_eq!(
            texts(&msgs),
            [
                Some("[in #collab]"),
                Some("there is something unread"),
                Some("[notes, since you last read]"),
                Some("carry on"),
            ]
        );
    }

    #[test]
    fn a_user_item_from_a_room_says_where_it_came_from() {
        let mut posted = user("i1", "ship it");
        if let ItemBody::User { origin, .. } = &mut posted.body {
            origin.principal = Some("reviewer".into());
            origin.conversation = Some("#design".into());
        }
        let mut anonymous = user("i2", "anyone?");
        if let ItemBody::User { origin, .. } = &mut anonymous.body {
            origin.conversation = Some("#design".into());
        }
        let msgs = ContextView::fold_items(&[posted, anonymous]);
        let texts: Vec<Option<&str>> = msgs[0].parts.iter().map(|p| p.as_text()).collect();
        assert_eq!(
            texts,
            [
                Some("[from reviewer in #design]"),
                Some("ship it"),
                Some("[in #design]"),
                Some("anyone?"),
            ]
        );
    }

    /// A call handed in through the host's door ran under this turn and is in
    /// the journal, but the model never asked for it and its outcome went
    /// back to whoever handed it in (ADR-0036 §2). Folding it would tell the
    /// model it had made a call it never made.
    #[test]
    fn a_call_the_model_never_made_never_reaches_it() {
        let mut bridged = tool("i2", "c1", Some("posted"));
        bridged.mark_external();
        let items = vec![user("i1", "go"), bridged, tool("i3", "c2", Some("read"))];
        let msgs = ContextView::fold_items(&items);
        let uses: Vec<&str> = msgs
            .iter()
            .flat_map(|m| &m.parts)
            .filter_map(|part| match part {
                ContentPart::ToolUse { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(uses, ["c2"], "only the call the model made");
        let results: Vec<&str> = msgs
            .iter()
            .flat_map(|m| &m.parts)
            .filter_map(|part| match part {
                ContentPart::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(results, ["c2"], "and no result it is not owed");
    }

    #[test]
    fn every_tool_use_gets_a_result_even_without_output() {
        let items = vec![user("i1", "go"), tool("i2", "c1", None)];
        let msgs = ContextView::fold_items(&items);
        assert_eq!(msgs.len(), 3);
        assert!(matches!(
            &msgs[2].parts[0],
            ContentPart::ToolResult { is_error: true, .. }
        ));
    }

    #[test]
    fn empty_assistant_text_never_becomes_a_message() {
        let items = vec![
            user("i1", "go"),
            item(
                "i2",
                ItemBody::Assistant {
                    text: String::new(),
                },
            ),
        ];
        assert_eq!(ContextView::fold_items(&items).len(), 1);
    }

    #[test]
    fn reasoning_with_only_replay_data_still_goes_back_to_the_provider() {
        let mut replay = ProviderMetadata::new();
        replay.insert(
            "openai".into(),
            serde_json::from_value(serde_json::json!({"id": "rs_1", "encrypted_content": "gAAA"}))
                .unwrap(),
        );
        let items = vec![
            user("i1", "go"),
            item(
                "i2",
                ItemBody::Reasoning {
                    text: String::new(),
                    provider_metadata: replay.clone(),
                },
            ),
            item(
                "i3",
                ItemBody::Reasoning {
                    text: String::new(),
                    provider_metadata: ProviderMetadata::new(),
                },
            ),
        ];
        let messages = ContextView::fold_items(&items);
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages[1].parts,
            vec![ContentPart::Reasoning {
                text: String::new(),
                provider_metadata: replay,
            }],
            "the encrypted part is replayed; the empty one is not"
        );
    }

    // ----- what every projection must satisfy, on random journals -----

    /// One item of a random journal. Tool calls are numbered so their ids
    /// are unique, as the kernel mints them.
    #[derive(Clone, Debug)]
    enum Shape {
        User(String),
        Assistant(String),
        Reasoning { text: String, replay: bool },
        Tool { answered: bool },
        Interruption,
        Compaction(String),
    }

    fn any_shape() -> impl proptest::strategy::Strategy<Value = Shape> {
        use proptest::prelude::*;
        prop_oneof![
            "[a-z ]{0,12}".prop_map(Shape::User),
            "[a-z ]{0,12}".prop_map(Shape::Assistant),
            ("[a-z ]{0,12}", any::<bool>())
                .prop_map(|(text, replay)| Shape::Reasoning { text, replay }),
            any::<bool>().prop_map(|answered| Shape::Tool { answered }),
            Just(Shape::Interruption),
            "[a-z ]{1,12}".prop_map(Shape::Compaction),
        ]
    }

    fn items_of(shapes: &[Shape]) -> Vec<Item> {
        let mut replay = ProviderMetadata::new();
        replay.insert("p".into(), serde_json::Map::new());
        shapes
            .iter()
            .enumerate()
            .map(|(n, shape)| {
                let id = format!("i{n}");
                match shape {
                    Shape::User(text) => user(&id, text),
                    Shape::Assistant(text) => item(&id, ItemBody::Assistant { text: text.clone() }),
                    Shape::Reasoning { text, replay: r } => item(
                        &id,
                        ItemBody::Reasoning {
                            text: text.clone(),
                            provider_metadata: if *r {
                                replay.clone()
                            } else {
                                ProviderMetadata::new()
                            },
                        },
                    ),
                    Shape::Tool { answered } => {
                        tool(&id, &format!("c{n}"), answered.then_some("out"))
                    }
                    Shape::Interruption => item(
                        &id,
                        ItemBody::Interruption {
                            marker: "[interrupted]".into(),
                        },
                    ),
                    Shape::Compaction(summary) => item(
                        &id,
                        ItemBody::Compaction {
                            summary: summary.clone(),
                            replaced: 0,
                            before: 0,
                            after: 0,
                            duration_ms: 0,
                        },
                    ),
                }
            })
            .collect()
    }

    fn tool_use_ids(message: &Message) -> Vec<&str> {
        message
            .parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::ToolUse { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect()
    }

    fn tool_result_ids(message: &Message) -> Vec<&str> {
        message
            .parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
                _ => None,
            })
            .collect()
    }

    /// The frames a live session would have written for these items.
    fn frames_of(items: &[Item]) -> Vec<Frame> {
        let ts = Timestamp::from_second(0).unwrap();
        items
            .iter()
            .enumerate()
            .flat_map(|(n, item)| {
                let seq = |k: u64| Seq(1 + 2 * n as u64 + k);
                [
                    (seq(0), Event::ItemStarted { item: item.clone() }),
                    (seq(1), Event::ItemCompleted { item: item.clone() }),
                ]
            })
            .map(|(seq, event)| Frame {
                seq,
                ts,
                session: SessionId::from_raw("ses_1"),
                cause: None,
                event,
            })
            .collect()
    }

    proptest::proptest! {
        #[test]
        fn every_projection_is_legal_for_the_api(
            shapes in proptest::collection::vec(any_shape(), 0..24)
        ) {
            let messages = ContextView::fold_items(&items_of(&shapes));
            if let Some(first) = messages.first() {
                proptest::prop_assert_eq!(first.role, Role::User, "a conversation opens with the user");
            }
            for message in &messages {
                proptest::prop_assert!(!message.parts.is_empty(), "no message is empty");
            }
            for (n, message) in messages.iter().enumerate() {
                let uses = tool_use_ids(message);
                if uses.is_empty() {
                    continue;
                }
                let next = messages.get(n + 1);
                proptest::prop_assert!(next.is_some(), "a tool use is always answered");
                let next = next.unwrap();
                proptest::prop_assert_eq!(next.role, Role::User);
                let mut results = tool_result_ids(next);
                results.sort_unstable();
                let mut wanted = uses.clone();
                wanted.sort_unstable();
                proptest::prop_assert_eq!(results, wanted, "one result per use, none extra");
            }
            for message in &messages {
                proptest::prop_assert!(
                    message.role == Role::User || tool_result_ids(message).is_empty(),
                    "results are in user messages only"
                );
            }
        }

        #[test]
        fn a_replayed_journal_folds_exactly_like_the_live_session(
            shapes in proptest::collection::vec(any_shape(), 0..16)
        ) {
            let items = items_of(&shapes);
            let replayed = ContextView::items(&frames_of(&items));
            proptest::prop_assert_eq!(&replayed, &items);
            proptest::prop_assert_eq!(
                ContextView::fold_items(&replayed),
                ContextView::fold_items(&items)
            );
        }
    }

    /// Version 1 frames, recorded from a real tool round through the fake
    /// provider. What the kernel makes of them is pinned so a format change
    /// is a deliberate migration, never an accident.
    #[test]
    fn version_one_frames_fold_to_the_same_messages_forever() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/frames-v1.jsonl");
        let text = std::fs::read_to_string(&path).unwrap();
        let frames: Vec<Frame> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let messages = ContextView::fold_items(&ContextView::items(&frames));
        insta::assert_json_snapshot!(messages);
    }

    #[test]
    fn compaction_replaces_the_head_and_keeps_what_it_says() {
        let ts = Timestamp::from_second(0).unwrap();
        let ses = SessionId::from_raw("ses_1");
        let f = |seq: u64, event: Event| Frame {
            seq: Seq(seq),
            ts,
            session: ses.clone(),
            cause: None,
            event,
        };
        let frames = vec![
            f(
                1,
                Event::ItemCompleted {
                    item: user("i1", "first"),
                },
            ),
            f(
                2,
                Event::ItemCompleted {
                    item: item("i2", ItemBody::Assistant { text: "a".into() }),
                },
            ),
            f(
                3,
                Event::ItemCompleted {
                    item: user("i3", "second"),
                },
            ),
            f(
                4,
                Event::ItemCompleted {
                    item: item("i4", ItemBody::Assistant { text: "b".into() }),
                },
            ),
            f(
                5,
                Event::ItemCompleted {
                    item: item(
                        "i9",
                        ItemBody::Compaction {
                            summary: "we did a".into(),
                            replaced: 2,
                            before: 100,
                            after: 20,
                            duration_ms: 1,
                        },
                    ),
                },
            ),
            f(
                6,
                Event::Compacted {
                    generation: 1,
                    boundary: ItemId::from_raw("i3"),
                    summary: ItemId::from_raw("i9"),
                    kept: vec![ItemId::from_raw("i1")],
                },
            ),
        ];
        let items = ContextView::items(&frames);
        let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["i1", "i9", "i3", "i4"]);
        let msgs = ContextView::fold(&frames);
        assert_eq!(msgs[0].parts[0].as_text(), Some("first"));
        assert!(msgs[0].parts[1].as_text().unwrap().starts_with("[Summary"));
    }

    #[test]
    fn rewind_drops_items() {
        let ts = Timestamp::from_second(0).unwrap();
        let ses = SessionId::from_raw("ses_1");
        let f = |seq: u64, event: Event| Frame {
            seq: Seq(seq),
            ts,
            session: ses.clone(),
            cause: None,
            event,
        };
        let frames = vec![
            f(
                1,
                Event::ItemCompleted {
                    item: user("i1", "a"),
                },
            ),
            f(
                2,
                Event::ItemCompleted {
                    item: item("i2", ItemBody::Assistant { text: "b".into() }),
                },
            ),
            f(
                3,
                Event::Rewound {
                    generation: 1,
                    to_turn: TurnId::from_raw("t"),
                    dropped: vec![ItemId::from_raw("i2")],
                    files_restored: vec![],
                },
            ),
        ];
        assert_eq!(ContextView::items(&frames).len(), 1);
    }

    #[test]
    fn the_estimate_counts_cjk_per_character_and_images_flat() {
        assert_eq!(bingo_sdk::tokens::text("abcdefgh"), 2);
        assert_eq!(bingo_sdk::tokens::text("你好"), 2);
        let msgs = vec![Message::user(vec![ContentPart::Image(Image {
            media_type: "image/png".into(),
            data: String::new(),
        })])];
        assert_eq!(estimate_tokens(&[], &msgs, &[]), 1_600);
    }

    /// An action with a result is told to the model as the person wrote it
    /// (ADR-0008 §5); one still running has no wire form.
    #[test]
    fn actions_with_results_reach_the_model_as_notes() {
        let done = item(
            "a1",
            ItemBody::Action {
                name: "!".into(),
                args: serde_json::json!("ls"),
                result: Some(serde_json::json!("a\nb\n[exit 1]")),
            },
        );
        let pending = item(
            "a2",
            ItemBody::Action {
                name: "login".into(),
                args: serde_json::json!({"provider": "x"}),
                result: None,
            },
        );
        let messages = ContextView::fold_items(&[done, pending]);
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].parts[0].as_text(),
            Some("[!] ls\na\nb\n[exit 1]")
        );
    }

    /// A shell line the person ran reaches the model as their own note: the
    /// line under a prompt, what it wrote in a fence, and the code only when
    /// it was not a clean exit (M65).
    #[test]
    fn a_shell_line_reaches_the_model_as_the_line_and_its_output() {
        let shell = |command: &str, output: &str, exit| ItemBody::Shell {
            command: command.into(),
            output: output.into(),
            exit,
            cwd: "/tmp/p".into(),
        };
        let messages = ContextView::fold_items(&[
            item("s1", shell("echo hi", "hi\n", Some(0))),
            item("s2", shell("false", "", Some(1))),
            item("s3", shell("tail -f log", "one\n[interrupted]", None)),
        ]);
        assert_eq!(messages.len(), 1, "one user message carries all three");
        let texts: Vec<Option<&str>> = messages[0].parts.iter().map(|p| p.as_text()).collect();
        assert_eq!(
            texts,
            [
                Some("$ echo hi\n```\nhi\n```"),
                Some("$ false\n[exit 1]"),
                Some("$ tail -f log\n```\none\n[interrupted]\n```"),
            ]
        );
    }
}
