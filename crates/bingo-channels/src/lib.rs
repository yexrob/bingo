//! The IM channel surface (ADR-0016): a session in a chat thread.
//!
//! One surface plugin, `SurfaceKind::Concurrent`, holding adapters that each
//! hand over their own mechanisms. The pieces, in the order they depend on
//! each other:
//!
//! - [`limits`] — what a platform will carry, with the unit its length is in.
//! - [`adapter`] — the [`ChannelAdapter`] contract: capabilities as accessors.
//! - [`question`] — one [`Question`], two rungs: buttons, or a numbered list.
//! - [`deliver`] — frames to [`Op`]s, coalesced by the dual gate.
//! - [`runner`] — one conversation on one session, both directions.
//! - [`host`] — the surface: arrivals in, runners out.
//! - [`loopback`] — the adapter that is the contract fixture.
//!
//! Nothing here reaches the sdk: a channel is a client of the one event
//! stream like every other surface, folding frames with `SessionState::apply`
//! and deriving what to say from the fold.

pub mod adapter;
pub mod conversation;
pub mod deliver;
pub mod error;
pub mod gate;
pub mod host;
pub mod limits;
pub mod lock;
pub mod loopback;
pub mod question;
pub mod runner;
pub mod settings;

#[cfg(test)]
mod fixtures;

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{ConfigClaim, Merge, Plugin, PluginError, PluginManifest, Registrar, Surface};

pub use adapter::{Arrival, Buttons, ChannelAdapter, Edit, Inbox, Incoming, Mode, Threads, Typing};
pub use conversation::{Conversation, Posted};
pub use deliver::{Deliverer, Op};
pub use error::ChannelError;
pub use gate::Gate;
pub use host::ChannelsSurface;
pub use limits::{Dialect, Encoding, Limits};
pub use lock::Claim;
pub use loopback::Loopback;
pub use question::{Choice, Question};
pub use runner::SURFACE_ID;
pub use settings::{SETTING, Settings, from_flags, wanted};

static MANIFEST: PluginManifest = PluginManifest {
    id: "bingo.surface.channels",
    version: env!("CARGO_PKG_VERSION"),
    sdk: "^0.1",
    provides: &["surface:channels"],
    requires: &[],
    config: Some(ConfigClaim {
        // Two layers each naming an adapter both apply: a project may add a
        // chat without unsaying the user's.
        keys: &[(SETTING, Merge::ByName)],
        schema: settings::schema,
    }),
};

/// Registers the channel surface with whatever adapters the settings name.
/// With none named the surface still registers and refuses when run, so
/// `bingo channels` says what is missing rather than doing nothing.
#[derive(Debug, Default, Clone, Copy)]
pub struct ChannelsPlugin;

#[async_trait]
impl Plugin for ChannelsPlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }

    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        let settings: Settings = registrar.config()?;
        let surface = ChannelsSurface::new(settings.channels.adapters(), settings.channels.gate());
        registrar.surface(Arc::new(surface) as Arc<dyn Surface>);
        Ok(())
    }
}

#[cfg(test)]
mod plugin_tests {
    use super::*;
    use bingo_sdk::{Env, SurfaceKind};

    fn registered(config: serde_json::Value) -> Arc<dyn Surface> {
        let mut registrar = Registrar::new(MANIFEST.id, config, Env::rooted("/tmp"));
        ChannelsPlugin.register(&mut registrar).expect("register");
        match registrar.into_contributions().pop() {
            Some(bingo_sdk::Contribution::Surface(surface)) => surface,
            other => panic!("expected a surface, got {other:?}"),
        }
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
        assert_eq!(MANIFEST.provides, &["surface:channels"]);
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
