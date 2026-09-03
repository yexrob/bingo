//! Where one conversation's knobs stand, and what to send before its next
//! prompt (ADR-0037 §§1, 4).
//!
//! The request already carries both facts — `reasoning` and `model` are on
//! every `ModelRequest` — so nothing new is asked for anywhere: what is kept
//! here is only what was last *applied*, because a knob is set by a message
//! and a message is only worth sending when something moved.
//!
//! Applied between turns, never inside one. The model goes first: an adapter
//! that clamps its levels to the model does it when the model changes, so a
//! level set before one would not survive the same breath.
//!
//! A third hand reaches the same knobs: the options an adapter's own row asks
//! for, applied once when the session opens ([`Knobs::preset`]). They go
//! through this state and not beside it, so an option the row and a `/thinking`
//! both name is still one applied value — the row sets it, the diff that
//! follows does not send it again, and a change after that still wins.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

use agent_client_protocol_schema::v1::{
    SessionConfigId, SessionConfigOption, SessionConfigSelectOption, SessionConfigValueId,
    SessionId as AcpSessionId, SetSessionConfigOptionRequest,
};
use bingo_sdk::{Effort, HostHandle, Level, ModelInfo};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::config::AGENT;
use crate::connection::Connection;
use crate::error::AcpError;
use crate::legacy::{self, SetModelRequest};
use crate::options;

/// The code a person sees when a knob bingo was asked to turn is not one this
/// agent has. The knob is the agent's; bingo only turns it.
pub const KNOB: &str = "ACP_KNOB";

/// And when the agent's nearest value is not the one that was asked for.
pub const LEVEL: &str = "ACP_LEVEL";

/// What an agent said about its knobs when a session opened.
#[derive(Default)]
pub struct Declared {
    pub options: Vec<SessionConfigOption>,
    /// The models an adapter with no model option listed the old way.
    pub legacy: Vec<ModelInfo>,
}

impl Declared {
    /// The options a session-opening answer declared, and whatever legacy list
    /// rode the same body.
    pub fn of(options: Option<Vec<SessionConfigOption>>, body: &Value) -> Self {
        Self {
            options: options.unwrap_or_default(),
            legacy: legacy::models(body),
        }
    }
}

/// What one request wants of these knobs.
pub struct Wanted<'a> {
    pub effort: Option<Effort>,
    pub model: &'a str,
}

/// Where a step is sent, and who is told about it.
pub struct Wire<'a> {
    pub connection: &'a Connection,
    /// The agent's own session id.
    pub session: &'a str,
    pub adapter: &'a str,
    pub host: Option<&'a HostHandle>,
}

impl Wire<'_> {
    async fn say(&self, level: Level, code: &str, text: &str) {
        if let Some(host) = self.host {
            let _ = host.notice(level, code, text).await;
        }
    }
}

/// One conversation's knobs: what the agent declared, and what bingo has
/// turned them to.
pub struct Knobs {
    declared: Mutex<Declared>,
    applied: Mutex<Applied>,
    told_effort: AtomicBool,
    told_model: AtomicBool,
}

/// The effort is kept as the agent's own value id, so two levels that clamp to
/// one value are one message; the model is kept as the name bingo was asked
/// for, because that is what the next request will say again.
#[derive(Default)]
struct Applied {
    effort: Option<String>,
    model: Option<String>,
}

/// Which knob a step turns, for the words said about it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Which {
    Effort,
    Model,
}

impl Which {
    fn word(self) -> &'static str {
        match self {
            Which::Effort => "thinking level",
            Which::Model => "model",
        }
    }
}

/// One knob's next move: the door it goes through, what `applied` becomes when
/// the agent takes it, and the word owed to a person whose level did not
/// survive the agent's own vocabulary.
struct Step {
    door: Door,
    applied: String,
    clamped: Option<(Effort, String)>,
}

enum Door {
    Option {
        id: SessionConfigId,
        value: SessionConfigValueId,
    },
    /// One adapter's own extension, taken only when it has no option
    /// (`crate::legacy`).
    Legacy { model: String },
}

/// One row entry's move: the same door a turn's own change goes through, and
/// the knob it turns out to be, where that is one the per-turn diff reads too.
struct Set {
    door: Door,
    applied: String,
    tracks: Option<Which>,
}

/// What one entry of an adapter's row comes to, read against what this agent
/// declared. Neither miss is an error: the ids are the agent's own words, and
/// a row that names one it does not have is a person to tell, not a session to
/// refuse (ADR-0037).
enum Asked {
    Take(Set),
    /// No option of that id, or none this client can set.
    NoOption,
    /// That option, and not that value.
    NoValue,
}

/// What one knob has to do before the next prompt.
enum Next {
    Take(Step),
    Nothing,
    /// The agent has no such knob, or none this client can place.
    NoKnob,
    /// It has the knob and not this value.
    Unknown(String),
}

impl Knobs {
    pub fn new(declared: Declared) -> Self {
        let applied = Applied {
            // Where the agent itself says the level stands. Bingo has applied
            // nothing yet, and this is what it would be applying over.
            effort: options::effort(&declared.options).map(|knob| knob.current.0.to_string()),
            model: None,
        };
        Self {
            declared: Mutex::new(declared),
            applied: Mutex::new(applied),
            told_effort: AtomicBool::new(false),
            told_model: AtomicBool::new(false),
        }
    }

    /// This agent's own models, as the catalogue serves them (ADR-0026).
    pub async fn models(&self) -> Vec<ModelInfo> {
        let declared = self.declared.lock().await;
        match options::model(&declared.options) {
            Some(knob) => knob.models(),
            None => declared.legacy.clone(),
        }
    }

    /// Everything this request moved, applied before its prompt goes out.
    pub async fn apply(&self, wire: &Wire<'_>, wanted: Wanted<'_>) {
        let model = self.next_model(wanted.model).await;
        self.turn(wire, Which::Model, model).await;
        if let Some(effort) = wanted.effort {
            let level = self.next_effort(effort).await;
            self.turn(wire, Which::Effort, level).await;
        }
    }

    /// What the adapter's own row asked to be set, applied once for this
    /// opening and before its first prompt (`config::Adapter::options`), in the
    /// order a `BTreeMap` keeps — so one row is the same messages in the same
    /// places on every run, and a scenario about them is not about a hash.
    pub async fn preset(&self, wire: &Wire<'_>, wanted: &BTreeMap<String, String>) {
        for (id, value) in wanted {
            self.place(wire, id, value).await;
        }
    }

    async fn place(&self, wire: &Wire<'_>, id: &str, value: &str) {
        match self.next_preset(id, value).await {
            Asked::Take(set) => self.take_row(wire, id, set).await,
            Asked::NoOption => {
                wire.say(Level::Warn, KNOB, &no_option(wire.adapter, id))
                    .await
            }
            Asked::NoValue => {
                wire.say(Level::Warn, KNOB, &no_value(wire.adapter, id, value))
                    .await
            }
        }
    }

    async fn next_preset(&self, id: &str, value: &str) -> Asked {
        let declared = self.declared.lock().await;
        let Some(knob) = options::by_id(&declared.options, id) else {
            return Asked::NoOption;
        };
        let Some(value) = knob.value(value) else {
            return Asked::NoValue;
        };
        Asked::Take(Set {
            door: Door::Option {
                id: knob.id.clone(),
                value: value.value.clone(),
            },
            applied: value.value.0.to_string(),
            tracks: tracked(&declared.options, knob.id),
        })
    }

    /// A row's entry is recorded where the per-turn diff reads it, when it is
    /// one of the two knobs that diff turns — so the row and `/thinking` are
    /// two hands on one knob rather than two knobs.
    async fn take_row(&self, wire: &Wire<'_>, id: &str, set: Set) {
        let Err(why) = self.send(wire, &set.door).await else {
            if let Some(which) = set.tracks {
                self.record(which, set.applied).await;
            }
            return;
        };
        let said = row_refused(wire.adapter, id, &why);
        wire.say(Level::Warn, KNOB, &said).await;
    }

    async fn turn(&self, wire: &Wire<'_>, which: Which, next: Next) {
        match next {
            Next::Nothing => {}
            Next::Take(step) => self.take(wire, which, step).await,
            Next::NoKnob => self.once(wire, which, no_knob(wire.adapter, which)).await,
            Next::Unknown(model) => {
                self.once(wire, which, not_served(wire.adapter, &model))
                    .await
            }
        }
    }

    /// `agent` is bingo's label for "whatever you would have used", so it is
    /// the one model name that never crosses (ADR-0037 §2).
    async fn next_model(&self, wanted: &str) -> Next {
        let applied = self.applied.lock().await.model.clone();
        if wanted == AGENT || applied.as_deref() == Some(wanted) {
            return Next::Nothing;
        }
        let declared = self.declared.lock().await;
        let Some(knob) = options::model(&declared.options) else {
            return match declared.legacy.is_empty() {
                true => Next::NoKnob,
                false => Next::Take(Step {
                    door: Door::Legacy {
                        model: wanted.to_string(),
                    },
                    applied: wanted.to_string(),
                    clamped: None,
                }),
            };
        };
        match knob.value(wanted) {
            Some(value) => Next::Take(Step {
                door: Door::Option {
                    id: knob.id.clone(),
                    value: value.value.clone(),
                },
                applied: wanted.to_string(),
                clamped: None,
            }),
            None => Next::Unknown(wanted.to_string()),
        }
    }

    async fn next_effort(&self, wanted: Effort) -> Next {
        let applied = self.applied.lock().await.effort.clone();
        let declared = self.declared.lock().await;
        let Some(value) = options::effort(&declared.options)
            .as_ref()
            .and_then(|knob| knob.level(wanted).map(|value| (knob.id.clone(), value)))
        else {
            return Next::NoKnob;
        };
        let (id, value) = value;
        if applied.as_deref() == Some(&value.value.0) {
            return Next::Nothing;
        }
        Next::Take(Step {
            door: Door::Option {
                id,
                value: value.value.clone(),
            },
            applied: value.value.0.to_string(),
            clamped: clamped(wanted, value),
        })
    }

    async fn take(&self, wire: &Wire<'_>, which: Which, step: Step) {
        let Err(why) = self.send(wire, &step.door).await else {
            self.record(which, step.applied).await;
            if let Some((asked, got)) = step.clamped {
                let said = in_its_own_words(wire.adapter, asked, &got);
                wire.say(Level::Info, LEVEL, &said).await;
            }
            return;
        };
        self.once(wire, which, refused(wire.adapter, which, &why))
            .await;
    }

    async fn send(&self, wire: &Wire<'_>, door: &Door) -> Result<(), AcpError> {
        match door {
            Door::Option { id, value } => {
                let answered = wire
                    .connection
                    .call(SetSessionConfigOptionRequest::new(
                        AcpSessionId::new(wire.session),
                        id.clone(),
                        value.clone(),
                    ))
                    .await?;
                self.refresh(answered.config_options).await;
                Ok(())
            }
            Door::Legacy { model } => wire
                .connection
                .call(SetModelRequest::new(wire.session, model))
                .await
                .map(|_| ()),
        }
    }

    /// A set answers with the whole option set, current values and all, so
    /// what is held after one is the agent's own word rather than a guess at
    /// what the change did to the rest — claude-agent-acp reshapes its levels
    /// when the model moves. An empty answer said nothing and changes nothing.
    async fn refresh(&self, options: Vec<SessionConfigOption>) {
        if options.is_empty() {
            return;
        }
        self.declared.lock().await.options = options;
    }

    async fn record(&self, which: Which, value: String) {
        let mut applied = self.applied.lock().await;
        match which {
            Which::Effort => applied.effort = Some(value),
            Which::Model => applied.model = Some(value),
        }
    }

    /// Said once per knob per conversation: every turn asks, and a line
    /// repeated every turn is not a clearer line.
    async fn once(&self, wire: &Wire<'_>, which: Which, said: (Level, String, String)) {
        if self.told(which).swap(true, Ordering::AcqRel) {
            return;
        }
        let (level, code, text) = said;
        wire.say(level, &code, &text).await;
    }

    fn told(&self, which: Which) -> &AtomicBool {
        match which {
            Which::Effort => &self.told_effort,
            Which::Model => &self.told_model,
        }
    }
}

/// Whether the option a row names is one of the two the per-turn diff turns.
/// Found the way that diff finds it, because the point is that they are the
/// same option: one knob, one applied value, whichever hand moved it.
fn tracked(options: &[SessionConfigOption], id: &SessionConfigId) -> Option<Which> {
    if options::effort(options).is_some_and(|knob| knob.id == id) {
        return Some(Which::Effort);
    }
    if options::model(options).is_some_and(|knob| knob.id == id) {
        return Some(Which::Model);
    }
    None
}

/// The agent's own word for a level, when it is not the level asked for.
fn clamped(wanted: Effort, value: &SessionConfigSelectOption) -> Option<(Effort, String)> {
    (options::level_of(value) != Some(wanted)).then(|| (wanted, value.name.clone()))
}

fn in_its_own_words(adapter: &str, asked: Effort, got: &str) -> String {
    format!(
        "{adapter} does not think in `{}`; it was set to {got}, the nearest it offers.",
        options::word(asked)
    )
}

fn no_knob(adapter: &str, which: Which) -> (Level, String, String) {
    (
        Level::Warn,
        KNOB.to_string(),
        format!(
            "{adapter} declared no {} bingo can set, so it keeps its own. \
             The knob is the agent's: say it on its own row, \
             `acp.adapters.{adapter}`, in the adapter's own words.",
            which.word()
        ),
    )
}

fn not_served(adapter: &str, wanted: &str) -> (Level, String, String) {
    (
        Level::Warn,
        KNOB.to_string(),
        format!(
            "{adapter} does not serve a model called `{wanted}`, so it stays \
             where it is. `/models refresh` lists the ones it does."
        ),
    )
}

/// The three things a row's own entry can come to, each naming the row it came
/// from — a person who wrote `options` is the only one who can answer any of
/// them.
fn no_option(adapter: &str, id: &str) -> String {
    format!(
        "{adapter} declared no option `{id}` bingo can set, so nothing was sent \
         for it. The ids in `acp.adapters.{adapter}.options` are the agent's \
         own, and it lists them itself when a session opens."
    )
}

fn no_value(adapter: &str, id: &str, value: &str) -> String {
    format!(
        "{adapter} lists no `{value}` among the values of its own `{id}`, so \
         nothing was sent for it. `acp.adapters.{adapter}.options` says the \
         agent's word for the agent's option."
    )
}

fn row_refused(adapter: &str, id: &str, why: &AcpError) -> String {
    format!("{adapter} refused the `{id}` its own row asked for: {why}")
}

fn refused(adapter: &str, which: Which, why: &AcpError) -> (Level, String, String) {
    (
        Level::Warn,
        KNOB.to_string(),
        format!("{adapter} refused the {}: {why}", which.word()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn declared(recorded: Value) -> Declared {
        Declared::of(
            Some(serde_json::from_value(recorded).expect("an option list parses")),
            &Value::Null,
        )
    }

    fn effort_knob() -> Value {
        json!([{
            "id": "reasoning_effort", "name": "Reasoning effort",
            "category": "thought_level", "type": "select",
            "currentValue": "medium",
            "options": [
                { "value": "low", "name": "Low" },
                { "value": "medium", "name": "Medium" },
                { "value": "high", "name": "High" }
            ]
        }])
    }

    /// The knob claude-agent-acp has no flag and no variable for: its mode is
    /// a session config option or it is nothing, which is the whole reason a
    /// row can speak here at all.
    fn mode_knob() -> Value {
        json!([{
            "id": "mode", "name": "Mode", "category": "mode", "type": "select",
            "currentValue": "default",
            "options": [
                { "value": "default", "name": "Default" },
                { "value": "dontAsk", "name": "Don't ask" }
            ]
        }])
    }

    fn door(set: &Set) -> Option<(String, String)> {
        match &set.door {
            Door::Option { id, value } => Some((id.0.to_string(), value.0.to_string())),
            Door::Legacy { .. } => None,
        }
    }

    fn taken(next: &Next) -> Option<(String, String)> {
        match next {
            Next::Take(Step {
                door: Door::Option { id, value },
                ..
            }) => Some((id.0.to_string(), value.0.to_string())),
            _ => None,
        }
    }

    /// The agent said where it stands at `session/new`, so asking for the
    /// level it is already on is not a message.
    #[tokio::test]
    async fn the_level_the_agent_already_reports_is_not_sent_again() {
        let knobs = Knobs::new(declared(effort_knob()));
        assert!(matches!(
            knobs.next_effort(Effort::Medium).await,
            Next::Nothing
        ));
        assert_eq!(
            taken(&knobs.next_effort(Effort::High).await),
            Some(("reasoning_effort".into(), "high".into()))
        );
    }

    /// Two levels that clamp to one value are one message: what is remembered
    /// is where the agent stands, not what was asked for.
    #[tokio::test]
    async fn two_asks_that_clamp_to_one_value_are_one_message() {
        let knobs = Knobs::new(declared(effort_knob()));
        let Next::Take(step) = knobs.next_effort(Effort::XHigh).await else {
            panic!("a first ask crosses");
        };
        assert_eq!(step.applied, "high");
        assert!(
            step.clamped.is_some(),
            "and is said in the agent's own word"
        );
        knobs.record(Which::Effort, step.applied).await;
        assert!(matches!(
            knobs.next_effort(Effort::Max).await,
            Next::Nothing
        ));
    }

    /// `agent` is bingo's word for the agent's own, and the agent is never
    /// told bingo's words.
    #[tokio::test]
    async fn the_agent_label_is_never_sent_as_a_model() {
        let knobs = Knobs::new(declared(json!([{
            "id": "model", "name": "Model", "category": "model", "type": "select",
            "currentValue": "a", "options": [{ "value": "a", "name": "A" }]
        }])));
        assert!(matches!(knobs.next_model(AGENT).await, Next::Nothing));
        assert_eq!(
            taken(&knobs.next_model("a").await),
            Some(("model".into(), "a".into()))
        );
        assert!(matches!(knobs.next_model("b").await, Next::Unknown(_)));
    }

    /// An agent with neither knob is asked for nothing (ADR-0037 §1).
    #[tokio::test]
    async fn an_agent_with_neither_knob_is_sent_nothing() {
        let knobs = Knobs::new(Declared::default());
        assert!(matches!(
            knobs.next_effort(Effort::High).await,
            Next::NoKnob
        ));
        assert!(matches!(knobs.next_model("gpt-5").await, Next::NoKnob));
        assert!(knobs.models().await.is_empty());
    }

    /// No model option, but the older door and the list that rides with it.
    #[tokio::test]
    async fn an_adapter_with_only_the_legacy_door_goes_through_it() {
        let knobs = Knobs::new(Declared::of(
            None,
            &json!({ "models": { "availableModels": [{ "modelId": "gpt-5[high]" }] } }),
        ));
        assert!(matches!(
            knobs.next_model("gpt-5[high]").await,
            Next::Take(Step {
                door: Door::Legacy { .. },
                ..
            })
        ));
        assert_eq!(knobs.models().await.len(), 1);
    }

    /// A row says the agent's own id and the agent's own value, and that is
    /// what crosses — nothing here is a knob bingo has a word for.
    #[tokio::test]
    async fn a_row_option_crosses_in_the_agents_own_words() {
        let knobs = Knobs::new(declared(mode_knob()));
        let Asked::Take(set) = knobs.next_preset("mode", "dontAsk").await else {
            panic!("the agent declared this option and this value");
        };
        assert_eq!(door(&set), Some(("mode".into(), "dontAsk".into())));
        assert!(
            set.tracks.is_none(),
            "and a mode is not one of the two the per-turn diff turns"
        );
    }

    /// An id the agent never declared, or a value it does not list, is a
    /// person to tell and nothing to send (ADR-0037: the knob is the agent's).
    #[tokio::test]
    async fn a_row_option_the_agent_does_not_have_sends_nothing() {
        let knobs = Knobs::new(declared(effort_knob()));
        assert!(matches!(
            knobs.next_preset("mode", "dontAsk").await,
            Asked::NoOption
        ));
        assert!(matches!(
            knobs.next_preset("reasoning_effort", "brisk").await,
            Asked::NoValue
        ));
    }

    /// The row and `/thinking` are two hands on one knob. What the row applied
    /// is what the diff reads, so the diff does not send it again — and a
    /// change after it still crosses, once.
    #[tokio::test]
    async fn a_row_that_names_the_effort_knob_is_what_the_diff_then_reads() {
        let knobs = Knobs::new(declared(effort_knob()));
        let Asked::Take(set) = knobs.next_preset("reasoning_effort", "low").await else {
            panic!("the agent declared this option and this value");
        };
        assert!(matches!(set.tracks, Some(Which::Effort)));
        // What a successful send does, which is where this value is kept.
        knobs.record(Which::Effort, set.applied).await;

        assert!(
            matches!(knobs.next_effort(Effort::Low).await, Next::Nothing),
            "the row already put it there"
        );
        assert_eq!(
            taken(&knobs.next_effort(Effort::High).await),
            Some(("reasoning_effort".into(), "high".into())),
            "and the change after it is one message, not two"
        );
    }

    /// Which knob a row names is found the way the diff finds it, because it
    /// is the same option: a third id for the effort is still the effort.
    #[test]
    fn which_knob_a_row_names_is_found_the_way_the_diff_finds_it() {
        let options = declared(json!([
            { "id": "mode", "name": "Mode", "category": "mode", "type": "select",
              "currentValue": "default",
              "options": [{ "value": "default", "name": "Default" }] },
            { "id": "model", "name": "Model", "category": "model", "type": "select",
              "currentValue": "a", "options": [{ "value": "a", "name": "A" }] },
            { "id": "_x.thinking", "name": "Thinking", "category": "thought_level",
              "type": "select", "currentValue": "low",
              "options": [{ "value": "low", "name": "Low" }] }
        ]))
        .options;
        let which = |id: &str| tracked(&options, &SessionConfigId::new(id));
        assert!(matches!(which("_x.thinking"), Some(Which::Effort)));
        assert!(matches!(which("model"), Some(Which::Model)));
        assert!(which("mode").is_none());
    }

    /// A notice a person cannot act on is noise; these name the row and the
    /// command.
    #[test]
    fn what_a_person_is_told_names_what_they_would_change() {
        let (level, code, said) = no_knob("codex-acp", Which::Effort);
        assert_eq!(level, Level::Warn);
        assert_eq!(code, KNOB);
        assert!(said.contains("thinking level"), "{said}");
        assert!(said.contains("acp.adapters.codex-acp"), "{said}");

        let (_, _, said) = not_served("codex-acp", "claude-opus-4-5");
        assert!(said.contains("claude-opus-4-5"), "{said}");
        assert!(said.contains("/models refresh"), "{said}");

        let said = in_its_own_words("codex-acp", Effort::XHigh, "High");
        assert!(said.contains("xhigh") && said.contains("High"), "{said}");
    }

    /// And what a person is told about their own row names the row's own key,
    /// because that is the only place either half of it could be wrong.
    #[test]
    fn what_a_person_is_told_about_their_row_names_the_row() {
        let said = no_option("claude", "mode");
        assert!(said.contains("`mode`"), "{said}");
        assert!(said.contains("acp.adapters.claude.options"), "{said}");

        let said = no_value("claude", "mode", "dontAsk");
        assert!(
            said.contains("dontAsk") && said.contains("`mode`"),
            "{said}"
        );
        assert!(said.contains("acp.adapters.claude.options"), "{said}");

        let said = row_refused("claude", "mode", &AcpError::transport("gone"));
        assert!(said.contains("`mode`") && said.contains("gone"), "{said}");
    }
}
