//! The query/tool engine's interface to whoever is hosting a run.
//!
//! The engine streams from a provider, executes tools, and keeps a transcript.
//! It does not know what a page, a modal, or a JSON frame is. [`events`] is the
//! whole boundary: what a run reports, and who answers the questions it cannot
//! answer itself. [`runner`] is the other direction — the session actor asking
//! for a run rather than a run reporting into it.

pub mod events;
pub mod runner;
