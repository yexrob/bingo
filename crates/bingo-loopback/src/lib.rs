//! A page served on a loopback port, and the browser that is sent to it.
//!
//! A library (ADR-0012 §1, widened by ADR-0042 §2): registers nothing, depends
//! on the sdk and external crates only, and any plugin — or any other library —
//! may depend on it. It exists because two unrelated callers need the same
//! socket and ADR-0001 forbids one plugin importing another: an OAuth login
//! waits for a redirect on it, and a tool holds a page open on it until the
//! person answers.
//!
//! Read it bottom up: [`request`](self)'s parser and [`Response`] are pure and
//! tested without a port; [`Loopback`] and [`Connection`] own the socket; and
//! [`browser`] owns the one process this crate ever starts.
//!
//! What is *not* here: a timeout, a turn's cancellation, a route table. A
//! caller races this against its own clock and its own interrupt.

pub mod browser;
mod error;
mod request;
mod response;
mod server;

pub use error::LoopbackError;
pub use request::{Head, MAX_BODY, MAX_HEAD, Request};
pub use response::Response;
pub use server::{Connection, Loopback};
