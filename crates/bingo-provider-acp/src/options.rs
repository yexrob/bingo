//! Which of the options an agent declared are the two knobs bingo turns, and
//! what one of them calls the value being asked for (ADR-0037 §§1–2).
//!
//! Pure, and it invents nothing: every value that crosses is one the agent
//! itself listed at `session/new`. The ids are a hint and never a gate —
//! codex-acp calls its effort `reasoning_effort`, claude-agent-acp calls the
//! same knob `effort`, and the next adapter will call it a third thing — so
//! the spec's own `category` is tried after them and the option's words last.

use agent_client_protocol_schema::v1::{
    SessionConfigId, SessionConfigKind, SessionConfigOption, SessionConfigOptionCategory,
    SessionConfigSelectOption, SessionConfigSelectOptions, SessionConfigValueId,
};
use bingo_sdk::{Effort, ModelInfo};

/// The ids the first-tier adapters give the effort knob, read from their own
/// sources (codex-acp's `REASONING_EFFORT_CONFIG_ID`, claude-agent-acp's
/// `EFFORT_CONFIG_ID`; both 2026-09-03).
const EFFORT_IDS: &[&str] = &["reasoning_effort", "effort"];

/// The one id both of them agree on.
const MODEL_IDS: &[&str] = &["model"];

/// The last resort: a word looked for in an option's id and its name.
const EFFORT_WORD: &str = "effort";
const MODEL_WORD: &str = "model";

/// The levels providers converge on, shallowest first. `Effort` has exactly
/// these six in exactly this order, which is what makes its `Ord` the ladder.
const LADDER: [(&str, Effort); 6] = [
    ("minimal", Effort::Minimal),
    ("low", Effort::Low),
    ("medium", Effort::Medium),
    ("high", Effort::High),
    ("xhigh", Effort::XHigh),
    ("max", Effort::Max),
];

/// Values that are not a level: "whatever the model does by itself"
/// (claude-agent-acp puts `default` first in its effort list) and "no
/// reasoning at all" (codex-acp's `none`). `ModelRequest::reasoning` is an
/// `Option`, so a caller wanting neither sends no level at all, and answering
/// a level with one of these would contradict what was asked for.
const SENTINELS: &[&str] = &["default", "none", "off"];

/// One knob: the option to set, where it stands, and every value it offers —
/// flattened out of whatever grouping the agent declared them in, because a
/// group is a heading for a person and not a fact about the values.
pub struct Knob<'a> {
    pub id: &'a SessionConfigId,
    pub name: &'a str,
    pub current: &'a SessionConfigValueId,
    values: Vec<&'a SessionConfigSelectOption>,
}

/// The effort-shaped option among these, if the agent declared one.
pub fn effort(options: &[SessionConfigOption]) -> Option<Knob<'_>> {
    find(
        options,
        EFFORT_IDS,
        &SessionConfigOptionCategory::ThoughtLevel,
        EFFORT_WORD,
    )
}

/// The model-shaped option among these, if the agent declared one.
pub fn model(options: &[SessionConfigOption]) -> Option<Knob<'_>> {
    find(
        options,
        MODEL_IDS,
        &SessionConfigOptionCategory::Model,
        MODEL_WORD,
    )
}

/// By id, then by the category the spec reserves for this knob, then by the
/// word. Each rung is narrower evidence than the one above it, and a boolean
/// option is never one of these two: both are a choice among values.
fn find<'a>(
    options: &'a [SessionConfigOption],
    ids: &[&str],
    category: &SessionConfigOptionCategory,
    word: &str,
) -> Option<Knob<'a>> {
    by(options, |option| ids.contains(&&*option.id.0))
        .or_else(|| by(options, |option| option.category.as_ref() == Some(category)))
        .or_else(|| {
            by(options, |option| {
                says(&option.id.0, word) || says(&option.name, word)
            })
        })
}

fn by<'a>(
    options: &'a [SessionConfigOption],
    wanted: impl Fn(&SessionConfigOption) -> bool,
) -> Option<Knob<'a>> {
    options
        .iter()
        .filter(|option| wanted(option))
        .find_map(Knob::of)
}

fn says(text: &str, word: &str) -> bool {
    text.to_ascii_lowercase().contains(word)
}

impl<'a> Knob<'a> {
    fn of(option: &'a SessionConfigOption) -> Option<Self> {
        match &option.kind {
            SessionConfigKind::Select(select) => Some(Self {
                id: &option.id,
                name: &option.name,
                current: &select.current_value,
                values: flatten(&select.options),
            }),
            _ => None,
        }
    }

    /// The value this agent takes for the level asked for: the deepest one it
    /// offers at or below, else the shallowest it offers at all. An agent
    /// whose levels are none of the words the ecosystem settled on is one
    /// this client cannot place, and nothing is sent for it — a guess would
    /// be bingo choosing how hard somebody else's model thinks.
    pub fn level(&self, wanted: Effort) -> Option<&'a SessionConfigSelectOption> {
        let mut placed = self.placed();
        placed.sort_by_key(|(level, _)| *level);
        placed
            .iter()
            .rev()
            .find(|(level, _)| *level <= wanted)
            .or_else(|| placed.first())
            .map(|(_, value)| *value)
    }

    /// Every value that names a level bingo knows, with the level it names.
    fn placed(&self) -> Vec<(Effort, &'a SessionConfigSelectOption)> {
        self.values
            .iter()
            .filter_map(|value| Some((level_of(value)?, *value)))
            .collect()
    }

    /// The value this agent serves the named model under, however it spells
    /// it. codex-acp's legacy ids pair a model with an effort in one string
    /// (`gpt-5[high]`), so a plain model name finds the value it names.
    pub fn value(&self, wanted: &str) -> Option<&'a SessionConfigSelectOption> {
        self.at(|id| id == wanted)
            .or_else(|| self.at(|id| id.eq_ignore_ascii_case(wanted)))
            .or_else(|| self.at(|id| base(id).eq_ignore_ascii_case(wanted)))
    }

    fn at(&self, wanted: impl Fn(&str) -> bool) -> Option<&'a SessionConfigSelectOption> {
        self.values
            .iter()
            .copied()
            .find(|value| wanted(&value.value.0))
    }

    /// This agent's own list, as a catalogue serves it (ADR-0026): the ids it
    /// declared, with its own label beside any that has one to add.
    pub fn models(&self) -> Vec<ModelInfo> {
        self.values
            .iter()
            .map(|value| ModelInfo {
                id: value.value.0.to_string(),
                display: (value.name != *value.value.0).then(|| value.name.clone()),
            })
            .collect()
    }
}

/// The model half of a `model[effort]` id, and the whole of anything else.
fn base(id: &str) -> &str {
    id.split_once('[').map_or(id, |(model, _)| model)
}

fn flatten(options: &SessionConfigSelectOptions) -> Vec<&SessionConfigSelectOption> {
    match options {
        SessionConfigSelectOptions::Ungrouped(values) => values.iter().collect(),
        SessionConfigSelectOptions::Grouped(groups) => groups
            .iter()
            .flat_map(|group| group.options.iter())
            .collect(),
        _ => Vec::new(),
    }
}

/// The level a value names, by its id or failing that by its label — the two
/// adapters title-case the same word ("Xhigh" for `xhigh`), and one of them
/// takes its ids from a server at runtime.
fn level_of(value: &SessionConfigSelectOption) -> Option<Effort> {
    named_level(&value.value.0).or_else(|| named_level(&value.name))
}

fn named_level(text: &str) -> Option<Effort> {
    let word = normalised(text);
    if SENTINELS.contains(&word.as_str()) {
        return None;
    }
    LADDER
        .iter()
        .find(|(name, _)| *name == word)
        .map(|(_, level)| *level)
}

/// One spelling for `x-high`, `X High` and `xhigh`.
fn normalised(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn options(recorded: Value) -> Vec<SessionConfigOption> {
        serde_json::from_value(recorded).expect("a recorded option list parses")
    }

    fn ids(knob: &Knob<'_>) -> Vec<String> {
        knob.values
            .iter()
            .map(|value| value.value.0.to_string())
            .collect()
    }

    /// What codex-acp declares (`REASONING_EFFORT_CONFIG_ID`, its `model`
    /// selector, its mode and its fast-mode toggle), in its own shapes.
    fn codex() -> Vec<SessionConfigOption> {
        options(json!([
            {
                "id": "mode", "name": "Mode", "category": "mode", "type": "select",
                "currentValue": "agent",
                "options": [
                    { "value": "read-only", "name": "Read only" },
                    { "value": "agent", "name": "Agent" }
                ]
            },
            {
                "id": "collaboration_mode", "name": "Collaboration mode",
                "category": "collaboration_mode", "type": "select",
                "currentValue": "default",
                "options": [{ "value": "default", "name": "Default" }]
            },
            {
                "id": "model", "name": "Model", "category": "model", "type": "select",
                "currentValue": "gpt-5.4-codex",
                "options": [
                    { "value": "gpt-5.4-codex", "name": "GPT-5.4 Codex" },
                    { "value": "gpt-5.4", "name": "GPT-5.4" }
                ]
            },
            {
                "id": "reasoning_effort", "name": "Reasoning effort",
                "category": "thought_level", "type": "select",
                "currentValue": "medium",
                "options": [
                    { "value": "none", "name": "None", "description": "Off" },
                    { "value": "low", "name": "Low", "description": "Fast" },
                    { "value": "medium", "name": "Medium", "description": "Balanced" },
                    { "value": "high", "name": "High", "description": "Thorough" }
                ]
            },
            { "id": "fast-mode", "name": "Fast mode", "category": "model_config",
              "type": "boolean", "currentValue": false }
        ]))
    }

    /// What claude-agent-acp declares: another id for the same knob, a
    /// `default` sentinel first in both lists, and levels that reach `max`.
    fn claude() -> Vec<SessionConfigOption> {
        options(json!([
            {
                "id": "model", "name": "Model", "category": "model", "type": "select",
                "currentValue": "default",
                "options": [
                    { "value": "default", "name": "Default" },
                    { "value": "opus", "name": "Opus" },
                    { "value": "claude-sonnet-4-6", "name": "Sonnet 4.6" }
                ]
            },
            {
                "id": "effort", "name": "Effort", "category": "thought_level",
                "type": "select",
                "currentValue": "default",
                "options": [
                    { "value": "default", "name": "Default" },
                    { "value": "low", "name": "Low" },
                    { "value": "high", "name": "High" },
                    { "value": "xhigh", "name": "Xhigh" }
                ]
            },
            { "id": "agent", "name": "Agent", "type": "select",
              "currentValue": "default",
              "options": [{ "value": "default", "name": "Default" }] }
        ]))
    }

    /// Two adapters, two ids for one knob, and neither's id resembles the
    /// other's — which is why the id is a hint and the category is the rung
    /// below it.
    #[test]
    fn both_adapters_knobs_are_found_under_their_own_ids() {
        let codex = codex();
        assert_eq!(
            &*effort(&codex).expect("an effort knob").id.0,
            "reasoning_effort"
        );
        assert_eq!(&*model(&codex).expect("a model knob").id.0, "model");

        let claude = claude();
        assert_eq!(&*effort(&claude).expect("an effort knob").id.0, "effort");
        assert_eq!(&*model(&claude).expect("a model knob").id.0, "model");
    }

    /// An adapter that spells its ids some third way is still read: the
    /// category is the spec's own hint, and both of these use it.
    #[test]
    fn an_unknown_id_is_found_by_the_category_the_spec_reserves() {
        let third = options(json!([
            { "id": "_x.thinking", "name": "Thinking", "category": "thought_level",
              "type": "select", "currentValue": "low",
              "options": [{ "value": "low", "name": "Low" }, { "value": "high", "name": "High" }] },
            { "id": "_x.brain", "name": "Brain", "category": "model",
              "type": "select", "currentValue": "a",
              "options": [{ "value": "a", "name": "A" }] }
        ]));
        assert_eq!(&*effort(&third).expect("by category").id.0, "_x.thinking");
        assert_eq!(&*model(&third).expect("by category").id.0, "_x.brain");
    }

    /// Neither an id nor a category: the option's own words are the last
    /// rung, and a boolean is never one of these knobs.
    #[test]
    fn the_last_rung_is_the_options_own_words_and_never_a_toggle() {
        let words = options(json!([
            { "id": "thinking-effort", "name": "How hard", "type": "select",
              "currentValue": "low",
              "options": [{ "value": "low", "name": "Low" }] },
            { "id": "the-model-used", "name": "Which one", "type": "select",
              "currentValue": "a", "options": [{ "value": "a", "name": "A" }] }
        ]));
        assert_eq!(&*effort(&words).expect("by word").id.0, "thinking-effort");
        assert_eq!(&*model(&words).expect("by word").id.0, "the-model-used");

        let toggle = options(json!([
            { "id": "effort", "name": "Effort", "type": "boolean", "currentValue": true }
        ]));
        assert!(effort(&toggle).is_none(), "a knob is a choice among values");
    }

    /// The list ADR-0037 §3 is written for: an agent with neither knob.
    #[test]
    fn an_agent_with_neither_knob_has_neither() {
        let neither = options(json!([
            { "id": "mode", "name": "Mode", "category": "mode", "type": "select",
              "currentValue": "plan",
              "options": [{ "value": "plan", "name": "Plan" }] }
        ]));
        assert!(effort(&neither).is_none());
        assert!(model(&neither).is_none());
        assert!(
            effort(&[]).is_none(),
            "and so has an agent that declared none"
        );
        assert!(model(&[]).is_none());
    }

    /// Every level, against a knob that stops at `high`: deeper asks clamp
    /// down to what it has, and a shallower ask than it offers clamps up.
    #[test]
    fn a_level_is_the_deepest_the_agent_offers_at_or_below_what_was_asked() {
        let codex = codex();
        let knob = effort(&codex).expect("an effort knob");
        let picked = |level| knob.level(level).map(|value| value.value.0.to_string());
        assert_eq!(picked(Effort::Minimal), Some("low".into()), "clamped up");
        assert_eq!(picked(Effort::Low), Some("low".into()));
        assert_eq!(picked(Effort::Medium), Some("medium".into()));
        assert_eq!(picked(Effort::High), Some("high".into()));
        assert_eq!(picked(Effort::XHigh), Some("high".into()), "clamped down");
        assert_eq!(picked(Effort::Max), Some("high".into()));
    }

    /// The sentinels are not levels: `none` is reasoning off and `default` is
    /// the model's own, and answering `minimal` with either would be a
    /// different thing than was asked for.
    #[test]
    fn neither_default_nor_none_is_ever_chosen_as_a_level() {
        let claude = claude();
        let knob = effort(&claude).expect("an effort knob");
        assert_eq!(
            knob.level(Effort::Minimal).map(|v| v.value.0.to_string()),
            Some("low".into()),
            "the shallowest real level, not the sentinel above it"
        );
        assert_eq!(
            knob.level(Effort::Max).map(|v| v.value.0.to_string()),
            Some("xhigh".into())
        );
    }

    /// A knob whose values are nobody's vocabulary is not placed at all.
    /// Sending one would be bingo deciding what "thorough" costs.
    #[test]
    fn a_knob_in_words_nobody_shares_is_left_where_it_is() {
        let strange = options(json!([
            { "id": "effort", "name": "Effort", "category": "thought_level",
              "type": "select", "currentValue": "brisk",
              "options": [
                  { "value": "brisk", "name": "Brisk" },
                  { "value": "thorough", "name": "Thorough" }
              ] }
        ]));
        let knob = effort(&strange).expect("the knob is there");
        assert!(knob.level(Effort::High).is_none());
    }

    /// The label is read when the id is a runtime string: codex-acp's efforts
    /// come from its own server and are only ever title-cased on the way out.
    #[test]
    fn a_level_is_read_off_the_label_when_the_id_does_not_say_it() {
        let labelled = options(json!([
            { "id": "reasoning_effort", "name": "Reasoning effort", "type": "select",
              "currentValue": "e2",
              "options": [
                  { "value": "e1", "name": "Low" },
                  { "value": "e2", "name": "X-High" }
              ] }
        ]));
        let knob = effort(&labelled).expect("the knob");
        assert_eq!(
            knob.level(Effort::Max).map(|v| v.value.0.to_string()),
            Some("e2".into())
        );
        assert_eq!(
            knob.level(Effort::Low).map(|v| v.value.0.to_string()),
            Some("e1".into())
        );
    }

    /// The catalogue is the agent's own list, in the agent's own order, with
    /// its label kept only where it adds something to the id.
    #[test]
    fn the_models_served_are_the_ones_the_agent_declared() {
        let claude = claude();
        let knob = model(&claude).expect("a model knob");
        assert_eq!(
            knob.models(),
            vec![
                ModelInfo {
                    id: "default".into(),
                    display: Some("Default".into())
                },
                ModelInfo {
                    id: "opus".into(),
                    display: Some("Opus".into())
                },
                ModelInfo {
                    id: "claude-sonnet-4-6".into(),
                    display: Some("Sonnet 4.6".into())
                },
            ]
        );
    }

    #[test]
    fn a_model_is_found_by_its_own_id_whatever_its_case() {
        let codex = codex();
        let knob = model(&codex).expect("a model knob");
        assert_eq!(
            knob.value("gpt-5.4").map(|v| v.value.0.to_string()),
            Some("gpt-5.4".into())
        );
        assert_eq!(
            knob.value("GPT-5.4-Codex").map(|v| v.value.0.to_string()),
            Some("gpt-5.4-codex".into())
        );
        assert!(knob.value("claude-opus-4-5").is_none(), "not this agent's");
    }

    /// codex-acp's legacy ids pair the model with its effort in one string.
    /// A person who types the model alone means the model.
    #[test]
    fn a_bracketed_id_is_found_by_the_model_it_names() {
        let bracketed = options(json!([
            { "id": "model", "name": "Model", "category": "model", "type": "select",
              "currentValue": "gpt-5[high]",
              "options": [
                  { "value": "gpt-5[high]", "name": "GPT-5 (high)" },
                  { "value": "gpt-5[low]", "name": "GPT-5 (low)" }
              ] }
        ]));
        let knob = model(&bracketed).expect("a model knob");
        assert_eq!(
            knob.value("gpt-5[low]").map(|v| v.value.0.to_string()),
            Some("gpt-5[low]".into()),
            "the whole id when the whole id is what was said"
        );
        assert_eq!(
            knob.value("gpt-5").map(|v| v.value.0.to_string()),
            Some("gpt-5[high]".into()),
            "and the first value of that model when only the model was"
        );
    }

    /// A group is a heading for a person; the values under it are the values.
    #[test]
    fn grouped_values_are_read_as_one_list() {
        let grouped = options(json!([
            { "id": "model", "name": "Model", "category": "model", "type": "select",
              "currentValue": "sonnet",
              "options": [
                  { "group": "fast", "name": "Fast",
                    "options": [{ "value": "haiku", "name": "Haiku" }] },
                  { "group": "deep", "name": "Deep",
                    "options": [{ "value": "sonnet", "name": "Sonnet" },
                                { "value": "opus", "name": "Opus" }] }
              ] }
        ]));
        let knob = model(&grouped).expect("a model knob");
        assert_eq!(ids(&knob), ["haiku", "sonnet", "opus"]);
        assert_eq!(&*knob.current.0, "sonnet");
    }
}
