//! Whether a newer release is out, and how this binary becomes it.
//!
//! The release line already publishes everything this needs
//! (`.github/workflows/release.yml`): four archives named after their target
//! triple, a `checksums.txt` beside them, and a GitHub Release per `v*` tag —
//! a *pre*-release when the tag is not on `main`, which is why
//! `releases/latest` is the right question to ask.
//!
//! # The bricks
//!
//! - [`version::newer`] — whether one tag stands above another.
//! - [`release::latest`] — the release out of the API's answer.
//! - [`asset`] — the archive this build's target is published as.
//! - [`checksums`] and [`sha256`] — what the list says, and what the bytes are.
//! - [`stamp`] — when this machine last asked, and what it heard.
//! - [`check`] — the four of them, once a day, silent whenever it cannot
//!   answer.
//! - [`install`] — unpack with the system `tar`, and the two renames that let
//!   a running binary be replaced.
//!
//! # What it does not do
//!
//! It reaches no network of its own: [`check`] is handed the fetch and the
//! command that installs does its own downloading (ADR-0043 §2). That is what
//! keeps this crate — and the platform code in [`install`] — buildable for
//! Windows on any machine, rather than behind a C toolchain the way every
//! crate above `reqwest` is.
//!
//! Nothing here is ever run with elevated rights.

pub mod api;
pub mod asset;
mod check;
pub mod checksums;
pub mod install;
pub mod release;
mod settings;
pub mod sha256;
pub mod stamp;
pub mod version;

pub use check::check;
pub use install::InstallError;
pub use release::{Asset, Release, ReleaseError};
pub use settings::{SETTING, Settings, schema, wanted};
pub use stamp::Stamp;
