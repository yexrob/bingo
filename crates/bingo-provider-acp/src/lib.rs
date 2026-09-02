//! ACP adapters as model providers (ADR-0035).
//!
//! Every configured adapter — `{command, args, env}` — is one `Provider`
//! instance. The message types come from `agent-client-protocol-schema`; the
//! newline-framed JSON-RPC client loop is written here, in tokio, `Send`.

#[cfg(test)]
pub(crate) mod fixtures;
pub mod method;
pub mod wire;
