//! The IM channel surface (ADR-0016): a session in a chat thread.
//!
//! One surface plugin, `SurfaceKind::Concurrent`, holding adapters that each
//! hand over their own mechanisms. The pieces, in the order they depend on
//! each other:
//!
//! - [`limits`] — what a platform will carry, with the unit its length is in.
//! - [`adapter`] — the [`ChannelAdapter`] contract: capabilities as accessors.
//! - [`question`] — one [`Question`], two rungs: buttons, or a numbered list.
//! - [`loopback`] — the adapter that is the contract fixture.
//!
//! Nothing here reaches the sdk: a channel is a client of the one event
//! stream like every other surface, folding frames with `SessionState::apply`
//! and deriving what to say from the fold.

pub mod adapter;
pub mod conversation;
pub mod error;
pub mod limits;
pub mod loopback;
pub mod question;

pub use adapter::{Arrival, Buttons, ChannelAdapter, Edit, Inbox, Incoming, Mode, Threads, Typing};
pub use conversation::{Conversation, Posted};
pub use error::ChannelError;
pub use limits::{Dialect, Encoding, Limits};
pub use loopback::Loopback;
pub use question::{Choice, Question};
