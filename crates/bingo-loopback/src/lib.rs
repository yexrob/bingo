//! A page served on a loopback port, and the browser that is sent to it.
//!
//! A library (ADR-0012 §1, widened by ADR-0042 §2): registers nothing, reaches
//! for no crate of this workspace at all, and any plugin — or any other library
//! — may depend on it. It exists because two unrelated callers need the same
//! socket and ADR-0001 forbids one plugin importing another: an OAuth login
//! waits for a redirect on it, and a tool holds a page open on it until the
//! person answers.
//!
//! Read it bottom up: [`Head::parse`], [`Response`], [`Token`], [`script`],
//! [`page`] and [`answer`] are pure and tested without a port; [`Loopback`] and
//! [`Connection`] own the socket; [`serve::until_answered`] is the only place
//! the two meet; and [`browser`] owns the one process this crate ever starts.
//!
//! What is *not* here: a timeout and a turn's cancellation. A caller races
//! [`serve::until_answered`] against its own clock and its own interrupt, which
//! is what lets one `esc` drop a page like any other call in flight.

pub mod answer;
pub mod browser;
mod error;
pub mod page;
mod request;
mod response;
pub mod script;
pub mod serve;
mod server;
mod token;

pub use answer::Answer;
pub use error::LoopbackError;
pub use request::{Head, MAX_BODY, MAX_HEAD, Request};
pub use response::Response;
pub use server::{Connection, Loopback};
pub use token::Token;
