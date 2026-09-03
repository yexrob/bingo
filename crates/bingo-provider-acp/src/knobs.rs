//! Where one conversation's two knobs stand, and what to send before its next
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
}
