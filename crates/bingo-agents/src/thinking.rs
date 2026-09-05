//! `SetThinking`: how hard a session thinks from its next turn — this one, or
//! a child by the name `SpawnAgent` gave back. The knob is the kernel's
//! (`SessionChange::Thinking`); this is the door a model reaches it through,
//! and the words are the sdk's one list (ADR-0047 §4).

use std::path::Path;

use async_trait::async_trait;
use bingo_sdk::{
    Effort, ErrorCode, KernelError, SessionChange, Subject, Tool, ToolContext, ToolError,
    ToolOutput, ToolSpec, ToolTraits, input_schema,
};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::Deserialize;
use serde_json::Value;

use crate::names;

const DESCRIPTION: &str = "\
Set how hard a session thinks: this one, or a sub-agent you started, named \
with `agent`. A higher level spends longer reasoning before answering; `off` \
asks for none. It lands on the next turn and never inside the one running \
now, so a session already working finishes what it is doing at the level it \
started on — set your own level before the work you want it for, not in the \
middle of it.";

/// The level a word names, or a refusal a model can act on: what it said and
/// the whole list it could have said instead. `whose` names where the word
/// came from, so a mistake in a definition file is not read as one in a call.
pub(crate) fn spoken(word: &str, whose: &str) -> Result<Option<Effort>, KernelError> {
    Effort::spoken(word).ok_or_else(|| {
        KernelError::new(
            ErrorCode::InvalidInput,
            format!(
                "{whose}: {word:?} is not a thinking level; say one of {}",
                words()
            ),
        )
    })
}

/// The words a level may be said in, in a row.
fn words() -> String {
    Effort::words().collect::<Vec<_>>().join(", ")
}

/// The seven words as the schema a model is shown, built from the sdk's one
/// list: a level added there is offered here without a second edit.
pub(crate) fn word_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "type": "string", "enum": Effort::words().collect::<Vec<_>>() })
}

/// The same where the field may be left out, in the shape schemars gives
/// every other optional field.
pub(crate) fn maybe_word_schema(_: &mut SchemaGenerator) -> Schema {
    let words: Vec<Value> = Effort::words()
        .map(Value::from)
        .chain([Value::Null])
        .collect();
    json_schema!({ "type": ["string", "null"], "enum": words })
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetThinkingArgs {
    /// How hard it thinks: `off`, or a level.
    #[schemars(schema_with = "word_schema")]
    pub level: String,
    /// The sub-agent to set it for, by the name `SpawnAgent` gave back.
    /// Without one this session's own level moves.
    pub agent: Option<String>,
}

/// Moving a knob the kernel owns: nothing outside the process changes, and
/// what the session then does with the level is gated in that session.
#[derive(Debug, Default, Clone, Copy)]
pub struct SetThinkingTool;

impl SetThinkingTool {
    /// The change, and the receipt saying whose next turn carries it.
    async fn set(&self, args: &SetThinkingArgs, cx: &ToolContext) -> Result<String, KernelError> {
        let level = spoken(&args.level, "level")?;
        let target = match &args.agent {
            None => None,
            Some(name) => Some(names::child(&cx.host, &cx.session, name).await?),
        };
        let session = target.as_ref().map_or(&cx.session, |child| &child.id);
        cx.host
            .reconfigure(session, SessionChange::Thinking(level))
            .await?;
        Ok(said(level, target.as_ref().map(names::name_of)))
    }
}

/// What was set, for whom, and when it lands (ADR-0047 §5). A session setting
/// its own level reads "your next turn", because the turn it is speaking in
/// is not the one that will carry it.
fn said(level: Option<Effort>, whose: Option<&str>) -> String {
    let level = Effort::word(level);
    match whose {
        Some(name) => format!("thinking: {level} for {name}, from its next turn"),
        None => format!("thinking: {level} for this session, from your next turn"),
    }
}

#[async_trait]
impl Tool for SetThinkingTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "SetThinking".into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<SetThinkingArgs>(),
            meta: Default::default(),
        }
    }

    fn traits(&self, _input: &Value) -> ToolTraits {
        crate::traits()
    }

    fn subjects(&self, input: &Value, _cwd: &Path) -> Vec<Subject> {
        serde_json::from_value::<SetThinkingArgs>(input.clone())
            .ok()
            .and_then(|args| args.agent)
            .map(|name| vec![Subject::Name { name }])
            .unwrap_or_default()
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: SetThinkingArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        // A word that is not one, a name nobody has, a session with no model:
        // each is something the model reads and corrects.
        match self.set(&args, cx).await {
            Ok(receipt) => Ok(ToolOutput::text(receipt)),
            Err(refused) => Ok(ToolOutput::error(refused.message)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{Fleet, Recorder, tool_context};
    use serde_json::json;
    use std::sync::Arc;

    async fn set(input: Value) -> (ToolOutput, Arc<Recorder>) {
        let fleet = Fleet::default();
        let root = fleet.root();
        fleet.child(&root, "reviewer");
        let host = Recorder::new(&fleet);
        let out = SetThinkingTool
            .call(input, &tool_context(&root, host.clone()))
            .await
            .expect("a call this crate can serve");
        (out, host)
    }

    #[tokio::test]
    async fn a_session_sets_its_own_level_and_is_told_when_it_lands() {
        let (out, host) = set(json!({ "level": "high" })).await;
        assert!(!out.is_error);
        assert_eq!(
            out.parts[0].as_text(),
            Some("thinking: high for this session, from your next turn")
        );
        let moved = host.reconfigured();
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0].1, SessionChange::Thinking(Some(Effort::High)));
    }

    #[tokio::test]
    async fn off_is_a_word_like_the_others() {
        let (out, host) = set(json!({ "level": "OFF" })).await;
        assert!(!out.is_error);
        assert_eq!(
            out.parts[0].as_text(),
            Some("thinking: off for this session, from your next turn")
        );
        assert_eq!(host.reconfigured()[0].1, SessionChange::Thinking(None));
    }

    #[tokio::test]
    async fn a_child_is_moved_by_the_name_the_spawn_gave_back() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let reviewer = fleet.child(&root, "reviewer");
        let host = Recorder::new(&fleet);
        let out = SetThinkingTool
            .call(
                json!({ "level": "low", "agent": "reviewer" }),
                &tool_context(&root, host.clone()),
            )
            .await
            .expect("a call");
        assert_eq!(
            out.parts[0].as_text(),
            Some("thinking: low for reviewer, from its next turn")
        );
        let moved = host.reconfigured();
        assert_eq!(moved[0].0, reviewer, "the child, not the caller");
        assert_eq!(moved[0].1, SessionChange::Thinking(Some(Effort::Low)));
    }

    #[tokio::test]
    async fn a_word_that_is_not_one_is_handed_back_with_the_list() {
        let (out, host) = set(json!({ "level": "loud" })).await;
        assert!(out.is_error);
        let text = out.parts[0].as_text().unwrap_or_default();
        assert!(text.contains("\"loud\" is not a thinking level"), "{text}");
        for word in Effort::words() {
            assert!(text.contains(word), "{word} is missing from {text}");
        }
        assert!(host.reconfigured().is_empty(), "nothing moved");
    }

    #[tokio::test]
    async fn a_name_nobody_has_is_handed_back_with_the_children() {
        let (out, host) = set(json!({ "level": "low", "agent": "nobody" })).await;
        assert!(out.is_error);
        let text = out.parts[0].as_text().unwrap_or_default();
        assert!(text.contains("reviewer"), "{text}");
        assert!(host.reconfigured().is_empty());
    }

    #[test]
    fn it_reads_only_names_the_agent_a_rule_may_match_and_offers_seven_words() {
        let tool = SetThinkingTool;
        let spec = tool.spec();
        assert_eq!(spec.name, "SetThinking");
        assert_eq!(
            spec.input_schema["properties"]["level"]["enum"],
            json!(["minimal", "low", "medium", "high", "xhigh", "max", "off"])
        );
        let traits = tool.traits(&Value::Null);
        assert!(traits.read_only && traits.trusted && !traits.concurrency_safe);
        assert_eq!(
            tool.subjects(
                &json!({ "level": "low", "agent": "reviewer" }),
                Path::new("/")
            ),
            [Subject::Name {
                name: "reviewer".into()
            }]
        );
        assert!(
            tool.subjects(&json!({ "level": "low" }), Path::new("/"))
                .is_empty(),
            "a session moving its own level names nobody"
        );
    }

    /// ADR-0047 §5, where the model reads it: the level is for the next turn,
    /// so a brief that sets it mid-work has to know that.
    #[test]
    fn the_description_says_when_the_level_lands() {
        assert!(
            DESCRIPTION.contains("a sub-agent you started"),
            "{DESCRIPTION}"
        );
        assert!(
            DESCRIPTION.contains("lands on the next turn and never inside the one running now"),
            "{DESCRIPTION}"
        );
    }
}
