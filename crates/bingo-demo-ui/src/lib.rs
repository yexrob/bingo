//! A demo plugin: the three lanes of ADR-0013, end to end, in one small
//! crate. It is off unless `--demo-ui` (or the `demoUi` setting) turns it on,
//! and it exists to be read — this is the shape a plugin has when it wants to
//! put something rich, live or interactive on a screen it knows nothing about.
//!
//! - **block** — [`DemoProgressTool`] returns a `Code` view as
//!   `ToolOutput.display`: what a person reads beside what the model reads.
//! - **panel** — `/board` publishes [`Board::view`] with `HostApi::extend`.
//!   It is journaled, so `--continue` reads back the same board.
//! - **live** — the same tool publishes a `Progress` with `HostApi::signal`
//!   every 200 ms. A signal is never journaled and is gone after a resume, so
//!   a bar that moves ten times a second costs the journal nothing.
//!
//! Interaction is the buttons under the board: an `Actions` item names a
//! command, a surface fires it as `Input::Action`, and the command publishes
//! the board again. The plugin never learns what a terminal is — it says what
//! to show and which command a button runs, and every surface decides the
//! rest (ADR-0013 §4).
//!
//! The three lanes side by side are `docs/design/tui.md` §8, which this crate
//! compiles as a doc test so the example a plugin author reads is one that
//! builds ([`DesignDoc`]).

mod board;
mod command;
mod journal;
mod progress;

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    Command, ConfigClaim, Contribution, Merge, Plugin, PluginError, PluginManifest, Registrar, Tool,
};
use schemars::JsonSchema;
use serde::Deserialize;

pub use board::{Board, Row, State};
pub use command::BoardCommand;
pub use journal::{BOARD, PLUGIN, PROGRESS};
pub use progress::DemoProgressTool;

/// The setting that turns this plugin on, for a person who would rather not
/// pass `--demo-ui` every time. The bin reads it before it composes the
/// plugins; claiming it here is what keeps it from being an unknown key.
pub const SETTING: &str = "demoUi";

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Settings {
    /// Whether the demo plugin is registered at all.
    #[serde(default)]
    demo_ui: bool,
}

fn schema() -> schemars::Schema {
    schemars::schema_for!(Settings)
}

/// Whether a merged settings object asks for this plugin. The bin composes
/// the plugin list before a host exists, so it reads the answer here rather
/// than keeping a second spelling of the key.
pub fn wanted(settings: &serde_json::Value) -> bool {
    serde_json::from_value::<Settings>(settings.clone())
        .map(|settings| settings.demo_ui)
        .unwrap_or(false)
}

static MANIFEST: PluginManifest = PluginManifest {
    id: PLUGIN,
    version: env!("CARGO_PKG_VERSION"),
    sdk: "^0.1",
    provides: &[
        "tool:DemoProgress",
        "command:board",
        "command:board.tick",
        "command:board.reset",
    ],
    requires: &[],
    config: Some(ConfigClaim {
        keys: &[(SETTING, Merge::Replace)],
        schema,
    }),
};

/// Registers the tool and the three commands. It keeps nothing of its own:
/// the board lives in the session's journal, and the bar lives for as long as
/// the stream carries it.
#[derive(Debug, Default, Clone, Copy)]
pub struct DemoUiPlugin;

#[async_trait]
impl Plugin for DemoUiPlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }

    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        registrar.tool(Arc::new(DemoProgressTool) as Arc<dyn Tool>);
        for command in [command::SHOW, command::TICK_COMMAND, command::RESET_COMMAND] {
            registrar.add(Contribution::Command(Arc::new(command) as Arc<dyn Command>));
        }
        Ok(())
    }
}

/// The plugin author's worked example is `docs/design/tui.md` §8, and it
/// compiles here: `cargo test --doc -p bingo-demo-ui` builds every fenced
/// block of that file against this crate.
#[cfg(doctest)]
#[doc = include_str!("../../../docs/design/tui.md")]
struct DesignDoc;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod plugin_tests {
    use super::*;
    use bingo_sdk::Env;

    #[test]
    fn the_setting_is_off_unless_it_says_otherwise() {
        assert!(!wanted(&serde_json::json!({})));
        assert!(!wanted(&serde_json::Value::Null));
        assert!(!wanted(&serde_json::json!({"demoUi": false})));
        assert!(wanted(&serde_json::json!({"demoUi": true})));
        assert!(
            !wanted(&serde_json::json!({"demoUi": "yes"})),
            "a setting of the wrong shape leaves it off"
        );
    }

    #[test]
    fn the_manifest_says_what_it_provides_and_claims_one_setting() {
        assert_eq!(MANIFEST.id, "bingo.demo.ui");
        assert_eq!(
            MANIFEST.provides,
            [
                "tool:DemoProgress",
                "command:board",
                "command:board.tick",
                "command:board.reset",
            ]
        );
        assert!(MANIFEST.requires.is_empty());
        let claim = MANIFEST.config.expect("the setting that turns it on");
        assert_eq!(claim.keys, [(SETTING, Merge::Replace)]);
    }

    #[test]
    fn registering_reads_nothing_and_contributes_what_the_manifest_promises() {
        let mut registrar = Registrar::new(PLUGIN, serde_json::Value::Null, Env::rooted("/tmp"));
        DemoUiPlugin.register(&mut registrar).expect("register");
        let contributions = registrar.into_contributions();
        assert_eq!(contributions.len(), MANIFEST.provides.len());
        let names: Vec<String> = contributions
            .iter()
            .map(|c| match c {
                Contribution::Tool(tool) => tool.spec().name,
                Contribution::Command(command) => command.spec().name,
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(
            names,
            ["DemoProgress", "board", "board.tick", "board.reset"]
        );
    }
}
