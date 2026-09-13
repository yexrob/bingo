//! Sub-agents: an agent *is* a child session (ADR-0010). This plugin owns
//! every noun the kernel refuses to — agent, name, definition, team — and adds
//! no machinery of its own: a spawn is `open(Create)`, a message is `deliver`,
//! a roster is `sessions{parent}`, and `@name` is a submit hook that
//! redirects.
//!
//! Five tools, two hooks, two commands:
//!
//! - `SpawnAgent` mints a child under the calling tool item and delivers the
//!   prompt. In the foreground it waits for the child's final text; in the
//!   background it returns the name and leaves a watcher to wake the parent.
//! - `SendMessage` wakes an agent — a child, a teammate beside the caller, or
//!   `parent` — or posts into a room's journal, `ListAgents` reads the
//!   tree, `ListModels` reads the model catalogue,
//!   `SetThinking` moves how hard this session or a child thinks.
//! - `@name rest` in the composer reaches the child of that name.
//! - A root session opening in a project with a `.bingo/team.json` seats the
//!   roles it declares, as children of itself.
//! - `/agents` shows the roster a person needs; `/team` what was declared.
//! - `agents.team` is the one door onto `.bingo/team.json` for the plugins
//!   that own its other keys: this plugin parses the file, and nobody else
//!   knows where it is (ADR-0031).
//!
//! Every tool is declared read-only and trusted: none of them reads or writes
//! anything outside the process, and what a child then does is gated in the
//! child, against the child's own directory and rules.

mod command;
mod definition;
pub mod guide;
mod hook;
mod layers;
mod library;
mod list;
mod message;
mod models;
mod names;
mod note;
mod rooms;
mod serial;
mod spawn;
mod team;
mod thinking;
mod watch;

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    Command, Contribution, Hook, Plugin, PluginError, PluginManifest, Registrar, ServiceHandle,
    Tool, ToolTraits, WireService,
};

pub use command::AgentsCommand;
pub use definition::Definition;
pub use hook::AtNameHook;
pub use list::ListAgentsTool;
pub use message::MessageTool;
pub use models::ListModelsTool;
pub use note::NOTE;
pub use spawn::SpawnAgentTool;
pub use team::{SeatHook, TEAM, TeamCommand, TeamFile};
pub use thinking::SetThinkingTool;

static MANIFEST: PluginManifest = PluginManifest {
    id: "bingo.agents",
    version: env!("CARGO_PKG_VERSION"),
    sdk: "^0.1",
    provides: &[
        "tool:SpawnAgent",
        "tool:SendMessage",
        "tool:ListAgents",
        "tool:ListModels",
        "tool:SetThinking",
        "hook:agents",
        "hook:team",
        "command:agents",
        "command:team",
        "service:agents.team",
        "service:bingo.agents.pages",
    ],
    requires: &[],
    // Definitions are files, not settings, and the limits on a session tree
    // are the kernel's.
    config: None,
};

/// `.bingo/team.json` under the key another plugin looks it up by. The typed
/// lookup is a `ServiceHandle` over the wire face, which is how a service met
/// by method rather than by type is reached from in process (ADR-0031 §4) —
/// and it must be met that way, because `team` is a noun the kernel has no
/// word for and a plugin may not import this one to get a trait.
///
/// No wire face is opened: every key of the file answers through one method,
/// and a key nobody has claimed yet is not this plugin's to hand a stranger.
fn team_file() -> Contribution {
    let wire = Arc::new(TeamFile) as Arc<dyn WireService>;
    Contribution::Service {
        key: TEAM.to_string(),
        value: Arc::new(ServiceHandle::new(wire)),
        wire: None,
    }
}

/// What every tool here is. They read the session tree and post into a
/// queue: nothing outside the process changes, and a child's own calls are
/// gated in the child, so trusting these traits costs a person nothing.
/// None is concurrency-safe — a spawn and a message that raced would agree on
/// neither a name nor an order.
pub(crate) fn traits() -> ToolTraits {
    ToolTraits {
        read_only: true,
        trusted: true,
        concurrency_safe: false,
        ..ToolTraits::default()
    }
}

/// Registers the six tools, the `@name` hook and `/agents`. Nothing here
/// holds the host: a tool reads it from its call, a hook from its context.
#[derive(Debug, Default, Clone, Copy)]
pub struct AgentsPlugin;

#[async_trait]
impl Plugin for AgentsPlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }

    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        registrar.tool(Arc::new(SpawnAgentTool) as Arc<dyn Tool>);
        registrar.tool(Arc::new(MessageTool) as Arc<dyn Tool>);
        registrar.tool(Arc::new(ListAgentsTool) as Arc<dyn Tool>);
        registrar.tool(Arc::new(ListModelsTool) as Arc<dyn Tool>);
        registrar.tool(Arc::new(SetThinkingTool) as Arc<dyn Tool>);
        registrar.add(Contribution::Hook(Arc::new(AtNameHook) as Arc<dyn Hook>));
        registrar.add(Contribution::Hook(
            Arc::new(SeatHook::new(registrar.env().clone())) as Arc<dyn Hook>,
        ));
        registrar.add(Contribution::Command(
            Arc::new(AgentsCommand) as Arc<dyn Command>
        ));
        registrar.add(Contribution::Command(
            Arc::new(TeamCommand) as Arc<dyn Command>
        ));
        registrar.add(team_file());
        registrar.add(guide::contribution(registrar));
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
mod plugin_tests {
    use super::*;
    use bingo_sdk::Env;

    fn registrar() -> Registrar {
        Registrar::new("bingo.agents", serde_json::Value::Null, Env::rooted("/tmp"))
    }

    #[test]
    fn the_manifest_says_what_it_provides_and_claims_no_settings() {
        assert_eq!(MANIFEST.id, "bingo.agents");
        assert_eq!(
            MANIFEST.provides,
            [
                "tool:SpawnAgent",
                "tool:SendMessage",
                "tool:ListAgents",
                "tool:ListModels",
                "tool:SetThinking",
                "hook:agents",
                "hook:team",
                "command:agents",
                "command:team",
                "service:agents.team",
                &format!("service:{}", bingo_sdk::Pages::key(MANIFEST.id)),
            ]
        );
        assert!(MANIFEST.requires.is_empty());
        assert!(MANIFEST.config.is_none());
    }

    #[test]
    fn registering_reads_nothing_and_contributes_what_the_manifest_promises() {
        let mut registrar = registrar();
        AgentsPlugin.register(&mut registrar).expect("register");
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
            [
                "SpawnAgent",
                "SendMessage",
                "ListAgents",
                "ListModels",
                "SetThinking"
            ]
        );
        let hooks: Vec<String> = contributions
            .iter()
            .filter_map(|c| match c {
                Contribution::Hook(hook) => Some(hook.id().to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(hooks, ["agents.at-name", "agents.team"]);
        let commands: Vec<String> = contributions
            .iter()
            .filter_map(|c| match c {
                Contribution::Command(command) => Some(command.spec().name),
                _ => None,
            })
            .collect();
        assert_eq!(commands, ["agents", "team"]);
        let services: Vec<(&str, bool)> = contributions
            .iter()
            .filter_map(|c| match c {
                Contribution::Service { key, wire, .. } => Some((key.as_str(), wire.is_some())),
                _ => None,
            })
            .collect();
        let page = bingo_sdk::Pages::key(MANIFEST.id);
        assert_eq!(
            services,
            [("agents.team", false), (page.as_str(), false)],
            "the team file and this plugin's page, in process only (ADR-0031 §3)"
        );
    }

    #[test]
    fn every_tool_is_read_only_trusted_and_alone() {
        let traits = traits();
        assert!(traits.read_only && traits.trusted);
        assert!(!traits.concurrency_safe && !traits.destructive && !traits.edit);
    }
}
