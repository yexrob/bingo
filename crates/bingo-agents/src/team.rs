//! A project's resident agents. `.bingo/team.json` names them, a root session
//! opened in that project seats them as children of itself, and `/team` says
//! which of them are running. A team is no new machinery: a role is a child
//! session like any other, and the roster is the tree.
//!
//! The file is shared with the plugins that own its other nouns, and this
//! plugin is its one parser: [`service`] is the door they read their own key
//! through (ADR-0031).

mod command;
mod file;
mod seat;
mod service;

pub use command::TeamCommand;
pub use seat::SeatHook;
pub use service::{TEAM, TeamFile};
