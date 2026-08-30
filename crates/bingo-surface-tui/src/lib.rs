//! The full-screen terminal surface.
//!
//! It is a client of the one event stream like every other surface (ADR-0002):
//! it opens a session, folds the frames with `SessionState::apply`, and derives
//! every view at render time. It holds no session state of its own — what it
//! owns is the caret, the scroll, what is armed and what is open ([`ui::Ui`]) —
//! and it never awaits the kernel from the loop.
//!
//! # The shape of it
//!
//! - [`terminal`] takes the terminal and gives it back, on every path
//!   including a panic.
//! - [`run`] is the loop: `select!` over the keyboard, the frame stream, a
//!   tick, and the results of the handful of host calls it spawns.
//! - [`input::on_key`] is pure: it mutates [`ui::Ui`] and returns
//!   [`effect::Effect`]s, so every binding is a test with no runtime in it.
//! - [`frame::regions`] cuts the screen into its regions and [`view::draw`]
//!   fills them; the transcript is what is left once the input box and the
//!   status line have taken theirs, so nothing below the transcript moves.
//! - [`tree::Tree`] holds one reducer state per session the attachment
//!   carries — the root and its sub-sessions (ADR-0010 §3) — and which of
//!   them is on screen.
//! - [`transcript`], [`markdown`], [`block`], [`panel`], [`dialog`],
//!   [`composer`], [`commands`], [`keys`], [`permission`] and [`wrap`] are the
//!   bricks those two stand on.
//!
//! # What a person types
//!
//! `/help`, `/clear`, `/resume` and `/exit` are the surface's own (ADR-0008
//! §6). Every other `/name` and every `!line` is submitted verbatim: the
//! session actor parses commands, not the client.

mod block;
mod blocks;
mod clock;
mod commands;
mod composer;
mod dialog;
mod effect;
mod frame;
mod history;
mod input;
mod keys;
mod markdown;
mod panel;
mod permission;
mod preview;
mod run;
mod status;
mod terminal;
mod theme;
mod transcript;
mod tree;
mod ui;
mod view;
mod wrap;

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    Exit, HostHandle, KernelError, Plugin, PluginError, PluginManifest, Registrar, Surface,
    SurfaceKind, SurfaceOptions,
};

/// The surface id, and the origin every input it submits carries.
pub const SURFACE_ID: &str = "tui";

#[derive(Debug, Default, Clone, Copy)]
pub struct TuiSurface;

#[async_trait]
impl Surface for TuiSurface {
    fn id(&self) -> &str {
        SURFACE_ID
    }

    fn kind(&self) -> SurfaceKind {
        SurfaceKind::Exclusive
    }

    async fn run(&self, host: HostHandle, opts: SurfaceOptions) -> Result<Exit, KernelError> {
        let mut screen = terminal::Tui::enter().map_err(entering)?;
        let exit = run::drive(&host, opts, &mut screen, run::terminal_keys()).await;
        // The terminal goes back whether the loop ended or failed.
        let _ = screen.leave();
        exit
    }
}

fn entering(e: std::io::Error) -> KernelError {
    KernelError::new(
        bingo_sdk::ErrorCode::Internal,
        format!("could not take the terminal: {e}"),
    )
}

static MANIFEST: PluginManifest = PluginManifest {
    id: "bingo.surface.tui",
    version: env!("CARGO_PKG_VERSION"),
    sdk: "^0.1",
    provides: &["surface:tui"],
    requires: &[],
    config: None,
};

#[derive(Debug, Default, Clone, Copy)]
pub struct TuiPlugin;

#[async_trait]
impl Plugin for TuiPlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }

    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        registrar.surface(Arc::new(TuiSurface) as Arc<dyn Surface>);
        Ok(())
    }
}

#[cfg(test)]
mod test_support;
