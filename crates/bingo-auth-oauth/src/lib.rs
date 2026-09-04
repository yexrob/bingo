//! OAuth flows and the credential store, as a library (ADR-0012 §1).
//!
//! A library tier exists because these flows are pure over an issuer and the
//! second provider that wants a subscription login must not import the first:
//! ADR-0001 forbids plugin → plugin, and the service registry passes runtime
//! objects, not code. So this crate registers nothing, depends on the sdk and
//! external crates only, and any plugin may depend on it.
//!
//! Read it bottom up: `pkce`, `jwt`, `callback`, `tokens`, `percent` are pure
//! and tested alone; `issuer` is the data one provider fills in; `store`,
//! `redirect`, `device`, `exchange` each own one piece of I/O — the socket
//! under `redirect` and the browser opener are `bingo_loopback`'s since M54
//! (ADR-0042 §1); and `source` is the only place they meet.
//!
//! Nothing here logs a credential.

pub mod callback;
pub mod device;
pub mod error;
pub mod exchange;
pub mod issuer;
pub mod jwt;
mod percent;
pub mod pkce;
pub mod redirect;
pub mod source;
pub mod store;
pub mod tokens;

pub use error::{AuthError, permanent};
pub use issuer::Issuer;
pub use source::{Status, TokenSource};
pub use store::{CredentialStore, Entry};
pub use tokens::Tokens;
