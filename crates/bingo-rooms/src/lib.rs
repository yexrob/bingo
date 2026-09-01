//! Rooms: a room *is* a session nobody answers (ADR-0011 §1), and a post into
//! it reaches every member. This plugin owns the noun the kernel refuses to
//! and adds no machinery of its own: a room is a `Log` session under the
//! person's, its membership is an `Event::Extension` in its own journal, a
//! post is a `User` item in it, and the fan-out is `deliver`.
//!
//! One command, one tool and one hook:
//!
//! - `/room` lists the rooms under this session; `/room design reviewer scout`
//!   opens `#design` under it, or resets who is in the one that stands.
//! - `OpenRoom` is that same door with an agent on the other side of it
//!   (ADR-0021): the room hangs under the caller, or — with `shared` — under
//!   the caller's parent, which is the whole of who will hear it.
//! - The hook seats the rooms `.bingo/team.json` declares when a person's own
//!   session opens, and watches every journal: a room announces itself, an
//!   extension says who is in it, and a user item in one is a post to fan out.
//!
//! A post also owes (ADR-0022): `@name` opens a debt the member's next post
//! closes, one bounded chaser nudges whoever stays silent, and `/room` and a
//! card on the room's parent show what stands.
//!
//! Every seat has an ear (ADR-0029): patience 0 is a live one — every post
//! wakes it, the default — and thirty seconds or more a patient one, whose
//! posts wait for the seat's next turn and wake it once when they have waited
//! long enough. The three doors declare it, `Listen` retunes your own, and
//! `@name` pierces either.
//!
//! Nothing here keeps a roster or a debt beside the journal: what the hook
//! holds in memory is a fold of the frames it saw, `/room` reads a membership
//! back out of the room it belongs to, and the mentions are derived from the
//! room's own posts every time they are asked for.

mod chase;
mod command;
mod deadline;
mod ear;
mod hook;
mod listen;
mod mentions;
mod name;
mod owed;
mod placement;
mod post;
mod room;
mod roster;
mod seat;
mod team;
mod tool;

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    ClientIdentity, Command, Contribution, Hook, Plugin, PluginError, PluginManifest, Registrar,
    Tool,
};

pub use command::RoomCommand;
pub use ear::{Ear, Listener, Seat};
pub use hook::RoomsHook;
pub use listen::ListenTool;
pub use room::Room;
pub use seat::seat;
pub use team::{Entry, TeamError};
pub use tool::OpenRoomTool;

/// This plugin's id: the owner of a room's key, and the plugin a room's
/// membership is published under.
pub const PLUGIN: &str = "bingo.rooms";

/// The surface a room's messages come from; a person's say `tui` or `print`.
pub const SURFACE: &str = "room";

static MANIFEST: PluginManifest = PluginManifest {
    id: PLUGIN,
    version: env!("CARGO_PKG_VERSION"),
    sdk: "^0.1",
    provides: &["command:room", "hook:rooms", "tool:OpenRoom", "tool:Listen"],
    requires: &[],
    // Who sits in which room is a project's file, not a person's settings.
    config: None,
};

/// How this plugin identifies itself when it opens a session.
pub(crate) fn identity() -> ClientIdentity {
    ClientIdentity {
        name: "rooms".into(),
        surface: SURFACE.into(),
    }
}

/// Registers `/room`, the tool an agent opens a room with, and the hook that
/// seats and fans out. Each is handed the host by the kernel at the call, so
/// this plugin keeps none.
#[derive(Debug, Default)]
pub struct RoomsPlugin;

#[async_trait]
impl Plugin for RoomsPlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }

    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        registrar.add(Contribution::Command(
            Arc::new(RoomCommand) as Arc<dyn Command>
        ));
        registrar.add(Contribution::Hook(
            Arc::new(RoomsHook::default()) as Arc<dyn Hook>
        ));
        registrar.add(Contribution::Tool(Arc::new(OpenRoomTool) as Arc<dyn Tool>));
        registrar.add(Contribution::Tool(Arc::new(ListenTool) as Arc<dyn Tool>));
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
mod plugin_tests {
    use super::*;
    use bingo_sdk::Env;

    #[test]
    fn the_manifest_says_what_it_provides_and_claims_no_settings() {
        assert_eq!(MANIFEST.id, "bingo.rooms");
        assert_eq!(
            MANIFEST.provides,
            ["command:room", "hook:rooms", "tool:OpenRoom", "tool:Listen"]
        );
        assert!(MANIFEST.requires.is_empty());
        assert!(MANIFEST.config.is_none());
    }

    #[test]
    fn registering_reads_nothing_and_contributes_what_the_manifest_promises() {
        let mut registrar =
            Registrar::new(PLUGIN, serde_json::Value::Null, Env::rooted("/nowhere"));
        RoomsPlugin
            .register(&mut registrar)
            .expect("registration does no I/O");
        let contributions = registrar.into_contributions();
        assert_eq!(contributions.len(), MANIFEST.provides.len());
        assert!(matches!(&contributions[0], Contribution::Command(c) if c.spec().name == "room"));
        assert!(matches!(&contributions[1], Contribution::Hook(h) if h.id() == "rooms"));
        assert!(matches!(&contributions[2], Contribution::Tool(t) if t.spec().name == "OpenRoom"));
        assert!(matches!(&contributions[3], Contribution::Tool(t) if t.spec().name == "Listen"));
    }
}
