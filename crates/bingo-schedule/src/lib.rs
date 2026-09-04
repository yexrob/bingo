//! Schedules (ADR-0019): deferred and recurring turns, on a session of
//! their own.
//!
//! A schedule is one JSON file under `<data_dir>/schedules/`, and a fire is
//! two calls this tree already had — `open` the session keyed
//! `schedule/<id>`, `deliver` the text with `Delivery::Wake`. Nothing here
//! runs a turn or holds a transcript: the schedule's own session is the
//! record, and `--resume` reads it like any other.
//!
//! The pieces, in the order they depend on each other:
//!
//! - [`spec`] — the grammar and the clock, pure: `every 30m`, `daily at
//!   09:00`, `once at <RFC3339>`, and the next fire after a given moment.
//! - [`entry`] — one schedule, and the session key it fires on.
//! - [`store`] — the directory, rebuilt on every read.
//! - [`lock`] — one runner per store, the channels plugin's claim.
//! - [`runner`] — the timer loop, and the pure pass that decides it.
//! - [`schedules`] — what this process has: the store, the claim, the bell.
//! - [`wake`] and [`wakes`] — the model's own `once` on its own session:
//!   what one is, and the process-local shelf and loop that deliver it.
//! - [`tools`] and [`command`] — four tools, `/schedule` and `/wake`.
//!
//! Schedules fire only while a bingo process runs. There is no daemon and no
//! pretence of one: every surface shows the same line saying who holds them.

mod command;
mod diff;
pub mod entry;
mod id;
pub mod lock;
mod render;
pub mod runner;
pub mod schedules;
pub mod spec;
pub mod store;
pub mod tools;
pub mod wake;
pub mod wakes;

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use bingo_sdk::{
    Command, ConfigClaim, Contribution, HostHandle, Merge, Plugin, PluginError, PluginManifest,
    Registrar, Tool,
};
use schemars::JsonSchema;
use serde::Deserialize;

pub use command::{ScheduleCommand, WakeCommand};
pub use entry::Entry;
pub use lock::Claim;
pub use runner::Runner;
pub use schedules::Schedules;
pub use spec::{Spec, SpecError};
pub use store::{Shelf, Store};
pub use tools::{ScheduleCreateTool, ScheduleForgetTool, ScheduleListTool, WakeTool};

static MANIFEST: PluginManifest = PluginManifest {
    id: "bingo.schedule",
    version: env!("CARGO_PKG_VERSION"),
    sdk: "^0.1",
    provides: &[
        "tool:ScheduleCreate",
        "tool:ScheduleList",
        "tool:ScheduleForget",
        "tool:Wake",
        "command:schedule",
        "command:wake",
    ],
    requires: &[],
    // The store is a directory, not a setting: where it lives follows the
    // data directory, and what is in it is written by the tools. The one
    // setting is whether the model may wake itself at all (ADR-0019 §8).
    config: Some(ConfigClaim {
        keys: &[(SETTING, Merge::Replace)],
        schema,
    }),
};

/// The top-level settings key this plugin claims.
const SETTING: &str = "schedule";

fn schema() -> schemars::Schema {
    schemars::schema_for!(Settings)
}

/// The claimed slice, as the kernel hands it over.
#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub schedule: Schedule,
}

/// A typo here would silently leave wakes on when a person meant them off, so
/// an unknown key is a startup failure rather than a silence.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Schedule {
    /// Whether the model may wake itself on the session it is running in
    /// (ADR-0019 §8). On by default: the wake is bounded, visible on the
    /// status line, and `/wake off` ends it.
    #[serde(default = "on")]
    pub wakes: bool,
}

impl Default for Schedule {
    fn default() -> Self {
        Self { wakes: on() }
    }
}

fn on() -> bool {
    true
}

/// Registers the three tools and `/schedule`, and runs the timer loop if
/// this process is the one that took the store's claim.
#[derive(Debug, Default)]
pub struct SchedulePlugin {
    /// Built in `register`, where the environment is; used by `start` and
    /// `stop`, which are handed nothing but the host.
    schedules: OnceLock<Arc<Schedules>>,
}

#[async_trait]
impl Plugin for SchedulePlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }

    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        let settings: Settings = registrar.config()?;
        let schedules = Arc::new(Schedules::new(&registrar.env().data_dir));
        registrar.tool(Arc::new(ScheduleCreateTool::new(schedules.clone())) as Arc<dyn Tool>);
        registrar.tool(Arc::new(ScheduleListTool::new(schedules.clone())) as Arc<dyn Tool>);
        registrar.tool(Arc::new(ScheduleForgetTool::new(schedules.clone())) as Arc<dyn Tool>);
        // Offered whether or not wakes are on: a model told why it may not
        // wake itself does something else, where one that never saw the tool
        // reaches for a schedule of its own instead.
        registrar.tool(
            Arc::new(WakeTool::new(schedules.clone(), settings.schedule.wakes)) as Arc<dyn Tool>,
        );
        registrar.add(Contribution::Command(
            Arc::new(ScheduleCommand::new(schedules.clone())) as Arc<dyn Command>,
        ));
        registrar.add(Contribution::Command(
            Arc::new(WakeCommand::new(schedules.clone())) as Arc<dyn Command>,
        ));
        self.schedules
            .set(schedules)
            .map_err(|_| PluginError::Failed("the schedules plugin registered twice".into()))
    }

    /// Take the store's claim and run the loop behind it; a process that
    /// came second leaves the schedules dormant and says who has them
    /// (ADR-0019 §5). Neither is a reason to refuse to start.
    async fn start(&self, host: HostHandle) -> Result<(), PluginError> {
        if let Some(schedules) = self.schedules.get() {
            schedules.start(host);
        }
        Ok(())
    }

    async fn stop(&self) -> Result<(), PluginError> {
        if let Some(schedules) = self.schedules.get() {
            schedules.stop();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod plugin_tests {
    use super::*;
    use bingo_sdk::Env;
    use serde_json::json;

    #[test]
    fn the_manifest_says_what_it_provides_and_claims_no_settings() {
        assert_eq!(MANIFEST.id, "bingo.schedule");
        assert!(MANIFEST.requires.is_empty());
        assert_eq!(
            MANIFEST.config.map(|claim| claim.keys),
            Some(&[("schedule", Merge::Replace)][..])
        );
        assert!(Schedule::default().wakes, "wakes are on until turned off");
    }

    #[test]
    fn registering_reads_nothing_and_contributes_what_the_manifest_promises() {
        let home = tempfile::tempdir().expect("a temp home");
        // The slice a plugin that claims a key and finds none is handed.
        let mut registrar = Registrar::new(MANIFEST.id, json!({}), Env::rooted(home.path()));
        SchedulePlugin::default()
            .register(&mut registrar)
            .expect("registering does no i/o");
        let contributions = registrar.into_contributions();
        assert_eq!(contributions.len(), MANIFEST.provides.len());
        let tools: Vec<String> = contributions
            .iter()
            .filter_map(|c| match c {
                Contribution::Tool(tool) => Some(tool.spec().name),
                _ => None,
            })
            .collect();
        assert_eq!(
            tools,
            ["ScheduleCreate", "ScheduleList", "ScheduleForget", "Wake"]
        );
        assert!(matches!(contributions[4], Contribution::Command(_)));
        assert!(matches!(contributions[5], Contribution::Command(_)));
        assert!(
            !home.path().join(".bingo/data/schedules").exists(),
            "registering creates no directory"
        );
    }

    #[tokio::test]
    async fn a_plugin_that_started_holds_the_store_and_gives_it_back_on_stop() {
        let home = tempfile::tempdir().expect("a temp home");
        let env = Env::rooted(home.path());
        let plugin = SchedulePlugin::default();
        let mut registrar = Registrar::new(MANIFEST.id, json!({}), env.clone());
        plugin.register(&mut registrar).expect("register");
        plugin
            .start(bingo_sdk::testing::NoHost::handle())
            .await
            .expect("start");
        let lock = env.data_dir.join("schedules").join("runner.lock");
        assert!(lock.is_file(), "the claim is taken");
        plugin.stop().await.expect("stop");
        assert!(!lock.exists(), "the claim is given back");
    }

    /// The one setting: what a person turns off, and what a typo does.
    #[test]
    fn the_settings_slice_says_whether_the_model_may_wake_itself() {
        let read = |slice| serde_json::from_value::<Settings>(slice);
        assert!(read(json!({})).expect("an empty slice").schedule.wakes);
        assert!(
            !read(json!({"schedule": {"wakes": false}}))
                .expect("a slice")
                .schedule
                .wakes
        );
        assert!(
            read(json!({"schedule": {"wake": false}})).is_err(),
            "a typo leaves wakes on silently unless it is refused"
        );
    }

    #[tokio::test]
    async fn a_plugin_that_never_registered_starts_and_stops_without_a_store() {
        let plugin = SchedulePlugin::default();
        plugin
            .start(bingo_sdk::testing::NoHost::handle())
            .await
            .expect("start");
        plugin.stop().await.expect("stop");
    }
}
