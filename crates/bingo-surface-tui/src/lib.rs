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
//! - [`views`] is one renderer per node of the `View` vocabulary (ADR-0013);
//!   [`rail`] derives the cards a plugin's panels and signals become, and
//!   [`panel`] is the sheet they are pinned from.
//! - [`transcript`], [`markdown`], [`dialog`], [`composer`], [`commands`],
//!   [`keys`], [`permission`], [`paths`], [`welcome`] and [`wrap`] are the
//!   bricks those stand on, and [`theme`] is the one table of tokens and
//!   glyphs they draw with.
//! - [`status`] is the one line of furniture, and [`cycle`] is the strip of
//!   sessions that takes it while `↓` has it open.
//! - [`window`] is what every list a cursor walks draws when it outgrows its
//!   room: the rows around the cursor, and a `…` at each end it cut.
//!
//! # What a person types
//!
//! `/help`, `/clear`, `/resume` and `/exit` are the surface's own (ADR-0008
//! §6). Every other `/name` and every `!line` is submitted verbatim: the
//! session actor parses commands, not the client.

mod blocks;
mod clock;
mod commands;
mod complete;
mod composer;
mod cycle;
mod dialog;
mod effect;
mod fold;
mod frame;
mod highlight;
mod history;
mod input;
mod keys;
mod layers;
mod markdown;
mod pager;
mod panel;
mod paths;
mod permission;
mod preview;
mod rail;
mod rewind;
mod run;
mod scroll;
mod search;
mod select;
mod status;
mod terminal;
mod theme;
mod transcript;
mod tree;
mod ui;
mod view;
mod views;
mod welcome;
mod window;
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
        let print = print_on_exit(&opts);
        let mut screen = terminal::Tui::enter().map_err(entering)?;
        let ended = run::drive(&host, opts, &mut screen, run::terminal_keys()).await;
        // The terminal goes back whether the loop ended or failed.
        let _ = screen.leave();
        let ended = ended?;
        if print {
            // The alternate screen took the conversation with it; this puts
            // the last screenful of it back where the shell can see it.
            for line in ended.screen {
                println!("{line}");
            }
        }
        Ok(ended.exit)
    }
}

/// `--no-print-on-exit` is the one thing the bin says about this surface.
fn print_on_exit(opts: &SurfaceOptions) -> bool {
    opts.args
        .get("noPrintOnExit")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
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
mod doubles;
#[cfg(test)]
mod motion;
#[cfg(test)]
mod painted;
#[cfg(test)]
mod screens;
#[cfg(test)]
mod test_lanes;
#[cfg(test)]
mod test_support;
