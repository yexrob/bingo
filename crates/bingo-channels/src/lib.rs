//! The IM channel surface (ADR-0016): a session in a chat thread.
//!
//! One surface plugin, `SurfaceKind::Concurrent`, holding adapters that each
//! hand over their own mechanisms. The pieces, in the order they depend on
//! each other:
//!
//! - [`limits`] — what a platform will carry, with the unit its length is in.
//! - [`access`] — who may speak to this bot, and where.
//! - [`adapter`] — the [`ChannelAdapter`] contract: capabilities as accessors.
//! - [`question`] — one [`Question`], two rungs: buttons, or a numbered list.
//! - [`deliver`] — frames to [`Op`]s, coalesced by the dual gate.
//! - [`runner`] — one conversation on one session, both directions.
//! - [`host`] — the surface: arrivals in, runners out.
//! - [`directory`] — which chat a session sits in, for whoever asks later.
//! - [`tool`] — `SendFile`, the one way a file goes out (ADR-0051 §3).
//! - [`loopback`] — the adapter that is the contract fixture.
//! - [`feishu`] — the first real platform, wire bricks and all.
//!
//! Nothing here reaches the sdk: a channel is a client of the one event
//! stream like every other surface, folding frames with `SessionState::apply`
//! and deriving what to say from the fold.

pub mod access;
pub mod adapter;
pub mod conversation;
pub mod deliver;
pub mod directory;
pub mod error;
pub mod feishu;
pub mod gate;
pub mod host;
pub mod limits;
pub mod lock;
pub mod loopback;
pub mod question;
pub mod runner;
pub mod secret;
pub mod settings;
pub mod tool;

#[cfg(test)]
mod fixtures;

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    ConfigClaim, Contribution, Merge, Plugin, PluginError, PluginManifest, Registrar, Surface,
    ToolSource,
};

pub use access::{Access, Policy, Refused, Rule};
pub use adapter::{
    Arrival, Buttons, ChannelAdapter, Edit, Files, Inbox, Incoming, Mode, Outgoing, Threads, Typing,
};
pub use conversation::{Conversation, Posted};
pub use deliver::{Deliverer, Op};
pub use directory::{Directory, Seat};
pub use error::ChannelError;
pub use feishu::Feishu;
pub use gate::Gate;
pub use host::ChannelsSurface;
pub use limits::{Dialect, Encoding, Limits};
pub use lock::Claim;
pub use loopback::Loopback;
pub use question::{Choice, Question};
pub use runner::SURFACE_ID;
pub use settings::{SETTING, Settings, from_flags, wanted};
pub use tool::{SEND_FILE, SendFile, SendFileSource};

static MANIFEST: PluginManifest = PluginManifest {
    id: "bingo.surface.channels",
    version: env!("CARGO_PKG_VERSION"),
    sdk: "^0.1",
    provides: &["surface:channels", "tool:SendFile"],
    requires: &[],
    config: Some(ConfigClaim {
        // Two layers each naming an adapter both apply: a project may add a
        // chat without unsaying the user's.
        keys: &[(SETTING, Merge::ByName)],
        schema: settings::schema,
    }),
};

/// Registers the channel surface with whatever adapters the settings name,
/// and the tool that posts a file into one of its chats. With none named the
/// surface still registers and refuses when run, so `bingo channels` says
/// what is missing rather than doing nothing.
#[derive(Debug, Default, Clone, Copy)]
pub struct ChannelsPlugin;

#[async_trait]
impl Plugin for ChannelsPlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }

    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        let settings: Settings = registrar.config()?;
        let adapters = settings.channels.adapters(registrar.env());
        let surface = ChannelsSurface::new(
            adapters,
            settings.channels.gate(),
            settings.channels.access(),
        );
        // The same directory on both sides: the surface fills it as its
        // conversations open, and the tool reads it to find the chat a
        // session is in (ADR-0051 §3).
        let directory = surface.directory();
        registrar.surface(Arc::new(surface) as Arc<dyn Surface>);
        registrar.add(Contribution::Tools(
            Arc::new(SendFileSource::new(directory)) as Arc<dyn ToolSource>,
        ));
        Ok(())
    }
}

#[cfg(test)]
mod plugin_tests {
    use super::*;
    use bingo_sdk::{Env, SurfaceKind};

    fn contributions(config: serde_json::Value) -> Vec<bingo_sdk::Contribution> {
        let mut registrar = Registrar::new(MANIFEST.id, config, Env::rooted("/tmp"));
        ChannelsPlugin.register(&mut registrar).expect("register");
        registrar.into_contributions()
    }

    fn registered(config: serde_json::Value) -> Arc<dyn Surface> {
        contributions(config)
            .into_iter()
            .find_map(|contribution| match contribution {
                bingo_sdk::Contribution::Surface(surface) => Some(surface),
                _ => None,
            })
            .expect("a surface")
    }

    /// The tool is contributed by a source, not statically: it exists only
    /// while the surface runs (ADR-0009 §1, ADR-0051 §3).
    #[tokio::test]
    async fn the_plugin_contributes_the_file_tool_through_a_source() {
        let source = contributions(serde_json::json!({ "channels": { "loopback": {} } }))
            .into_iter()
            .find_map(|contribution| match contribution {
                bingo_sdk::Contribution::Tools(source) => Some(source),
                _ => None,
            })
            .expect("a tool source");
        assert_eq!(source.id(), SURFACE_ID);
        assert!(
            source.tools().await.is_empty(),
            "nothing is running, so there is no chat to send into"
        );
        assert!(MANIFEST.provides.contains(&"tool:SendFile"));
    }

    #[test]
    fn the_plugin_registers_one_concurrent_surface() {
        let surface = registered(serde_json::json!({ "channels": { "loopback": {} } }));
        assert_eq!(surface.id(), SURFACE_ID);
        assert_eq!(
            surface.kind(),
            SurfaceKind::Concurrent,
            "a chat owns no terminal; it runs beside whatever does"
        );
        assert_eq!(MANIFEST.provides, &["surface:channels", "tool:SendFile"]);
    }

    #[tokio::test]
    async fn a_surface_with_no_adapter_refuses_rather_than_going_quiet() {
        let surface = registered(serde_json::json!({}));
        let error = surface
            .run(host::tests::nowhere(), host::tests::options("/tmp"))
            .await
            .expect_err("a refusal");
        assert_eq!(error.code, bingo_sdk::ErrorCode::InvalidInput);
        assert!(
            error.message.contains("no channel is configured"),
            "{error}"
        );
    }
}
