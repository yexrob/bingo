//! A project's resident agents. `.bingo/team.json` names them, a root session
//! opened in that project seats them as children of itself, and `/team` says
//! which of them are running. A team is no new machinery: a role is a child
//! session like any other, and the roster is the tree.

mod command;
mod file;
mod seat;

pub use command::TeamCommand;
pub use seat::SeatHook;
