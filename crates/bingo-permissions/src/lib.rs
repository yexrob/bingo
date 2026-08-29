//! The permission policy: five modes, an allow/deny/ask rule table, and a
//! decision that fails closed at every step.

pub mod decide;
pub mod mode;
pub mod path;
pub mod rule;
pub mod scope;
pub mod split;
pub mod url;

pub use decide::Request;
pub use mode::{Mode, UnknownMode};
pub use rule::{Rule, Rules};
