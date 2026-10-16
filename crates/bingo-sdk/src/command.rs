//! Slash commands. One registry serves dispatch, the catalog and help; a
//! surface completes a command's argument from the `ArgSpec` its catalogue
//! entry carries, never by asking the command. The session actor parses `/name args`, `!line` and `Input::Action`,
//! runs the command on its own task and answers with an `IntentAck` whose
//! `Applied.result` is `{"message"}`, `{"view"}` or `{"item"}` (ADR-0008).

use std::collections::BTreeMap;
use std::path::PathBuf;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::KernelError;
use crate::event::ItemBody;
use crate::host::HostHandle;
use crate::ids::SessionId;
use crate::view::View;

/// Commands presented through one destination while their dispatch stays intact.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommandFold {
    /// Destination command shown in the palette for the folded names.
    pub into: String,
    /// Command names or aliases found through the destination without changing dispatch.
    pub names: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommandSpec {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<KeyBinding>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
    /// Run bare, it opens the place where its argument is chosen, so a
    /// palette runs it rather than listing `args`; `args` stays for every
    /// surface that completes it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub opens: bool,
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    /// A one-line description for the command menu; argument syntax belongs in `args`.
    pub hint: String,
    pub args: ArgSpec,
    /// May run while a turn is busy (read-only commands).
    #[serde(default)]
    pub instant: bool,
    pub family: String,
}

impl CommandSpec {
    /// The suffix that names a plugin's drawing handler: a press on what a
    /// plugin drew goes back to that plugin under this name.
    pub const UI_SUFFIX: &'static str = ":ui";

    pub fn ui(plugin: &str) -> Self {
        Self {
            keys: vec![],
            name: Self::ui_name(plugin),
            aliases: vec![],
            hint: String::new(),
            args: ArgSpec::None,
            instant: true,
            hidden: true,
            opens: false,
            family: "ui".into(),
        }
    }

    /// The name of `plugin`'s drawing handler.
    pub fn ui_name(plugin: &str) -> String {
        format!("{plugin}{}", Self::UI_SUFFIX)
    }

    /// Whether `name` is some plugin's drawing handler rather than a command
    /// a person could type.
    pub fn is_ui(name: &str) -> bool {
        name.strip_suffix(Self::UI_SUFFIX)
            .is_some_and(|plugin| !plugin.is_empty())
    }

    /// The first of this spec's names that `owner` may not take: any name
    /// spelled as a drawing handler but its own. A press on what a plugin
    /// drew is looked up by that name, so whoever held another's would
    /// answer its controls. A registrar refuses the spec when this is `Some`.
    pub fn foreign_handler(&self, owner: &str) -> Option<&str> {
        let own = Self::ui_name(owner);
        std::iter::once(&self.name)
            .chain(&self.aliases)
            .find(|name| name.ends_with(Self::UI_SUFFIX) && **name != own)
            .map(String::as_str)
    }
}

/// A chord a command declares for itself. A bare chord runs the command
/// alone; one with `args` submits the command with them, so a shortcut for
/// `/model next` is a binding on `model` and not a second catalogue entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum KeyBinding {
    Chord(String),
    WithArgs { chord: String, args: String },
}

impl KeyBinding {
    pub fn chord(&self) -> &str {
        match self {
            Self::Chord(chord) | Self::WithArgs { chord, .. } => chord,
        }
    }

    /// The command line this binding submits for the command called `name`.
    pub fn line(&self, name: &str) -> String {
        match self {
            Self::Chord(_) => format!("/{name}"),
            Self::WithArgs { args, .. } => format!("/{name} {args}"),
        }
    }
}

impl From<&str> for KeyBinding {
    fn from(chord: &str) -> Self {
        Self::Chord(chord.into())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ArgSpec {
    None,
    /// An argument followed by optional flags, completed from the same spec.
    Options {
        args: Box<ArgSpec>,
        flags: Vec<String>,
    },
    Free {
        hint: String,
    },
    /// Completed from a catalog kind the command validates against.
    Catalog {
        source: String,
    },
    /// One of the words the command lists itself, in the order it offers
    /// them. A surface completes from the list; the kernel carries it.
    ///
    /// `then` says what the word after one of `values` is — the name
    /// `/plugins enable` takes, the server `/mcp reconnect` takes — and is
    /// absent for a command whose argument ends at the first word, so every
    /// spec written before it reads the same (ADR-0008 §6a).
    Words {
        /// Words among `values` that end the argument; `then` does not apply.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        alone: Vec<String>,
        values: Vec<String>,
        /// What a surface shows for a value that is not its own best name;
        /// the line still runs the value, and a value without one reads as
        /// itself.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        labels: BTreeMap<String, String>,
        /// Words among `values` only the person may run: they raise trust or
        /// let repository code run, so no link rewrites a line that starts
        /// with one and no view bingo did not draw may send one.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        person: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        then: Option<Box<ArgSpec>>,
    },
}

impl CommandSpec {
    /// Whether `args` start with a word only the person may run.
    pub fn person_only(&self, args: &str) -> bool {
        self.person_verb(args).is_some()
    }

    /// The word only the person may run that `args` start with, and the
    /// arguments after it: the one reading every owner of such a line uses,
    /// so what is guarded and what runs cannot part.
    pub fn person_verb<'a>(&self, args: &'a str) -> Option<(&'a str, &'a str)> {
        let (verb, rest) = split_verb(args);
        let person = self.args.person_words().iter().any(|word| word == verb);
        person.then_some((verb, rest))
    }
}

/// The first word of `text` and what follows it, split at any whitespace and
/// trimmed: `/name args` read as a name and its arguments, `args` as a verb
/// and the rest.
pub fn split_verb(text: &str) -> (&str, &str) {
    let text = text.trim_start();
    let end = text.find(char::is_whitespace).unwrap_or(text.len());
    (&text[..end], text[end..].trim())
}

/// The prefix that is its own command name: `!rest` is the shell line.
pub const SHELL: &str = "!";

/// The command a typed line names, and its argument text: `!rest` is the
/// shell's, `/name rest` is `name`'s, and anything else is prose (`None`).
/// The kernel reads a person's line with this, and so does anything that
/// must know beforehand whether a line it holds will run as a command.
pub fn command_line(text: &str) -> Option<(&str, &str)> {
    let line = text.trim_start();
    if let Some(rest) = line.strip_prefix(SHELL) {
        return Some((SHELL, rest.trim()));
    }
    let rest = line.strip_prefix('/')?;
    let (name, args) = match rest.split_once(char::is_whitespace) {
        Some((name, args)) => (name, args.trim()),
        None => (rest.trim_end(), ""),
    };
    (!name.is_empty()).then_some((name, args))
}

impl ArgSpec {
    /// The words a line only the person may run starts with.
    pub fn person_words(&self) -> &[String] {
        match self {
            ArgSpec::Words { person, .. } => person,
            ArgSpec::Options { args, .. } => args.person_words(),
            _ => &[],
        }
    }

    /// The catalogue kinds this spec completes from, at any depth.
    pub fn sources(&self) -> Vec<&str> {
        match self {
            ArgSpec::Catalog { source } => vec![source],
            ArgSpec::Options { args, .. } => args.sources(),
            ArgSpec::Words {
                then: Some(next), ..
            } => next.sources(),
            _ => vec![],
        }
    }
}

#[derive(Clone)]
pub struct CommandContext {
    pub session: SessionId,
    pub cwd: PathBuf,
    pub host: HostHandle,
    /// Cancelled when the person stops this command or the session closes.
    pub cancel: crate::CancellationToken,
}

impl std::fmt::Debug for CommandContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandContext")
            .field("session", &self.session)
            .field("cwd", &self.cwd)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CommandOutcome {
    /// Forward an existing command result without recording or executing it again.
    Acknowledged {
        result: serde_json::Value,
    },
    /// A client interprets only actions it supports; the host does not dispatch them.
    Surface {
        action: crate::Action,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fallback: Option<String>,
    },
    /// A site key chose an answer; the surface decides whether to send it.
    Answer {
        answer: crate::Answer,
    },
    Handled {
        handled: bool,
    },
    Applied {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    View {
        view: View,
    },
    /// Becomes a turn, submitted with the command's own intent and origin.
    Prompt {
        text: String,
    },
    /// One completed item the kernel records in the transcript (a shell
    /// line's output, a login's receipt); the ack carries its id.
    Record {
        body: ItemBody,
    },
}

#[async_trait]
pub trait Command: Send + Sync {
    fn spec(&self) -> CommandSpec;

    /// A cheap projection of the supplied session, without I/O.
    fn now(&self, _view: &SessionView<'_>) -> Option<String> {
        None
    }

    /// The words a `Words` argument offers in the supplied session when they
    /// are fewer than the spec's, without I/O; `None` offers every one.
    fn offered(&self, _view: &SessionView<'_>) -> Option<Vec<String>> {
        None
    }

    /// Whether this invocation may run beside the current turn. Implementations
    /// inspect the supplied state without calling back into its session actor.
    async fn is_instant(&self, _args: &str, _state: &crate::SessionState) -> bool {
        self.spec().instant
    }

    async fn run(&self, args: &str, cx: &CommandContext) -> Result<CommandOutcome, KernelError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_spec_names_every_catalogue_it_completes_from() {
        let spec: ArgSpec = serde_json::from_value(json!({
            "kind":"words","values":["enable","model"],
            "then":{"kind":"options","args":{"kind":"catalog","source":"plugins"},"flags":["--keep"]}
        }))
        .unwrap();
        assert_eq!(spec.sources(), ["plugins"]);
        assert!(
            ArgSpec::Free {
                hint: "name".into()
            }
            .sources()
            .is_empty()
        );
    }

    #[test]
    fn optional_flags_wrap_an_existing_argument_contract() {
        let wire = serde_json::json!({"kind":"options","args":{"kind":"catalog","source":"models"},"flags":["--keep"]});
        let args: ArgSpec = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(args).unwrap(), wire);
    }

    #[test]
    fn a_frame_without_alone_keeps_its_following_argument() {
        let wire = json!({
            "name": "plugins", "hint": "plugins", "family": "session",
            "args": {
                "kind": "words", "values": ["enable"],
                "then": { "kind": "catalog", "source": "plugins" }
            }
        });
        let spec: CommandSpec = serde_json::from_value(wire).expect("old frame");
        let ArgSpec::Words { alone, then, .. } = spec.args else {
            panic!("word arguments");
        };
        assert!(alone.is_empty());
        assert_eq!(
            then.as_deref(),
            Some(&ArgSpec::Catalog {
                source: "plugins".into()
            })
        );
    }

    #[test]
    fn words_that_end_the_argument_survive_the_wire() {
        let wire = json!({
            "kind": "words", "values": ["enable", "reload"],
            "alone": ["reload"],
            "then": { "kind": "catalog", "source": "plugins" }
        });
        let args: ArgSpec = serde_json::from_value(wire.clone()).expect("word arguments");
        assert_eq!(serde_json::to_value(args).expect("serialises"), wire);
    }

    /// The shape a surface and a plugin both read: the variant is its `kind`,
    /// the words are `values`, in the command's own order. A word list that
    /// says nothing about the word after it writes nothing down, so a spec
    /// from before `then` and a spec without one are the same bytes.
    #[test]
    fn a_word_list_crosses_the_wire_as_its_kind_and_its_values() {
        let args = ArgSpec::Words {
            labels: Default::default(),
            person: vec![],
            alone: vec![],
            values: vec!["off".into(), "low".into()],
            then: None,
        };
        let wire = serde_json::to_value(&args).expect("serialises");
        assert_eq!(wire, json!({ "kind": "words", "values": ["off", "low"] }));
        let back: ArgSpec = serde_json::from_value(wire).expect("deserialises");
        assert_eq!(back, args);
    }

    /// A verb that takes a name carries the name's own spec inside its own,
    /// and a reader that has never heard of one reads the words as before.
    #[test]
    fn a_verb_carries_the_spec_of_the_word_after_it() {
        let args = ArgSpec::Words {
            labels: Default::default(),
            person: vec![],
            alone: vec![],
            values: vec!["enable".into()],
            then: Some(Box::new(ArgSpec::Catalog {
                source: "plugins".into(),
            })),
        };
        let wire = serde_json::to_value(&args).expect("serialises");
        assert_eq!(
            wire,
            json!({
                "kind": "words",
                "values": ["enable"],
                "then": { "kind": "catalog", "source": "plugins" },
            })
        );
        let back: ArgSpec = serde_json::from_value(wire).expect("deserialises");
        assert_eq!(back, args);
    }
}

#[cfg(test)]
mod hidden_contract {
    use super::*;
    #[test]
    fn hidden_defaults_false_and_survives_the_wire_when_true() {
        let old = serde_json::json!({"name":"counter:ui","hint":"","args":{"kind":"none"},"family":"ui","instant":true});
        let spec: CommandSpec = serde_json::from_value(old.clone()).unwrap();
        assert!(serde_json::to_value(spec).unwrap().get("hidden").is_none());
        let mut hidden = old;
        hidden["hidden"] = serde_json::json!(true);
        let spec: CommandSpec = serde_json::from_value(hidden).unwrap();
        assert_eq!(serde_json::to_value(spec).unwrap()["hidden"], true);
    }
}

#[cfg(test)]
mod key_contract {
    use super::*;
    use serde_json::json;
    #[test]
    fn default_keys_are_additive_and_handled_is_an_explicit_result() {
        let old = json!({"name":"hello","hint":"Hello","args":{"kind":"none"},"family":"fixture"});
        let mut spec: CommandSpec = serde_json::from_value(old).unwrap();
        assert!(spec.keys.is_empty());
        assert!(serde_json::to_value(&spec).unwrap().get("keys").is_none());
        spec.keys.push("ctrl+x".into());
        spec.keys.push(KeyBinding::WithArgs {
            chord: "alt+n".into(),
            args: "next".into(),
        });
        let written = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            written["keys"],
            json!(["ctrl+x", {"chord":"alt+n","args":"next"}])
        );
        assert_eq!(
            serde_json::from_value::<CommandSpec>(written).unwrap(),
            spec
        );
        let wire = json!({"kind":"handled","handled":false});
        assert_eq!(
            serde_json::to_value(serde_json::from_value::<CommandOutcome>(wire.clone()).unwrap())
                .unwrap(),
            wire
        );
    }
}

/// Provider arguments can contain credentials even when dispatch will reject them.
/// Use this only for display and persistence; dispatch keeps the original line.
pub fn recorded_line(line: &str) -> &str {
    if line.split_whitespace().next() == Some("/provider") {
        "/provider [redacted]"
    } else {
        line
    }
}

/// Session-scoped metadata over the existing global command catalogue.
pub trait CommandCatalog: Send + Sync {
    fn catalog(&self, catalog: crate::Catalog, view: &SessionView<'_>) -> crate::Catalog;
}
pub const COMMAND_CATALOG: crate::ServiceKey<dyn CommandCatalog> =
    crate::ServiceKey::new("bingo.commandCatalog");

#[cfg(test)]
mod surface_contract {
    use super::*;
    #[test]
    fn a_client_action_round_trips_without_host_dispatch() {
        let wire = serde_json::json!({"kind":"surface","action":{"name":"client","args":{"handler":"one"}},"fallback":"This command is terminal only."});
        let outcome: CommandOutcome = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(outcome).unwrap(), wire);
    }
}

/// Read-only facts needed by command metadata; no host handle or I/O capability.
pub struct SessionView<'a> {
    pub state: &'a crate::SessionState,
    pub providers: &'a [crate::AuthStatus],
    /// The models catalogue as the client last read it; empty before it has.
    pub models: &'a [crate::CatalogEntry],
}

#[cfg(test)]
mod person_contract {
    use super::*;
    /// A tab or a doubled space reads the same verb and arguments as one
    /// space: what is guarded is what runs.
    #[test]
    fn a_person_verb_reads_the_same_at_any_whitespace() {
        let spec: CommandSpec = serde_json::from_value(serde_json::json!({
            "name":"plugins","hint":"","family":"t",
            "args":{"kind":"words","values":["trust","reload"],"person":["trust"]}
        }))
        .unwrap();
        for args in [
            "trust lift full",
            "trust\tlift full",
            "  trust  lift full ",
            "trust\t\tlift full",
        ] {
            assert_eq!(
                spec.person_verb(args),
                Some(("trust", "lift full")),
                "{args:?}"
            );
        }
        assert_eq!(spec.person_verb("trust"), Some(("trust", "")));
        assert_eq!(spec.person_verb("reload"), None);
        assert_eq!(spec.person_verb("trustx lift"), None);
        assert_eq!(
            split_verb("/plugins\tapprove  x"),
            ("/plugins", "approve  x")
        );
    }
}

#[cfg(test)]
mod ui_name_tests {
    use super::CommandSpec;

    #[test]
    fn a_drawing_handler_is_named_after_its_plugin() {
        assert_eq!(CommandSpec::ui("browse").name, "browse:ui");
        assert!(CommandSpec::is_ui(&CommandSpec::ui_name(
            "bingo.mod.status"
        )));
        for typed in ["plugins", "permission", ":ui", "plugins:uix", "ui"] {
            assert!(!CommandSpec::is_ui(typed), "{typed}");
        }
    }

    #[test]
    fn only_its_drawer_may_take_a_handler_name() {
        let spec = |name: &str, aliases: &[&str]| CommandSpec {
            aliases: aliases.iter().map(|alias| alias.to_string()).collect(),
            ..CommandSpec::ui(name.trim_end_matches(":ui"))
        };
        assert_eq!(spec("counter:ui", &[]).foreign_handler("counter"), None);
        assert_eq!(
            spec("counter:ui", &[]).foreign_handler("aaa"),
            Some("counter:ui")
        );
        let mut plain = spec("x", &["counter:ui"]);
        plain.name = "hello".into();
        assert_eq!(plain.foreign_handler("aaa"), Some("counter:ui"));
        plain.aliases = vec!["hi".into()];
        assert_eq!(plain.foreign_handler("aaa"), None);
        assert_eq!(spec("x", &[]).foreign_handler(""), Some("x:ui"));
    }
}

#[cfg(test)]
mod line_tests {
    use super::command_line;

    #[test]
    fn a_line_names_a_command_only_when_it_starts_with_one() {
        assert_eq!(command_line("!ls -la "), Some(("!", "ls -la")));
        assert_eq!(command_line("  /wake off"), Some(("wake", "off")));
        assert_eq!(command_line("/help"), Some(("help", "")));
        assert_eq!(command_line("/model\tnext"), Some(("model", "next")));
        for prose in ["check the build", "/", "/ wake", "", "a /wake"] {
            assert_eq!(command_line(prose), None, "{prose:?}");
        }
    }
}
