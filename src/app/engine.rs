//! Who runs the work a submission opened.
//!
//! The core decides *that* a turn runs, owns its lifecycle, and publishes what it
//! produces. It does not call a provider, execute a tool, or start an instance's
//! loop: that is the engine's, and the engine is *attached* rather than built in.
//! The order matters — a session that can be read, configured and closed exists
//! before one that can run, and that is exactly the session `bingo app-server`
//! served in D147.
//!
//! The seam is one-way on purpose. The actor never waits on anything, so all it
//! can do is hand work over; everything the run then produces comes back the way
//! every other engine report already does — an [`crate::engine::events::EngineEvent`]
//! into the turn the run was bound to, which the actor sequences (D142/D144).
//!
//! That leaves the two halves of a delivery split exactly where B4's ruling ②
//! put them: the registries are the actor's, so it deposits the message itself
//! and knows the item that became; waking whoever the deposit woke needs a
//! runtime and a `Session`, so it travels here.

use std::sync::Arc;

use crate::app::command::Action;
use crate::app::conversation::ConvKey;
use crate::app::ids::{ItemId, OperationId, TurnId};

/// One piece of work the core accepted and cannot do itself.
#[derive(Debug, Clone, PartialEq)]
pub enum Run {
    /// Main's model turn. The turn is already open and the item its prose became
    /// is already in the log; the engine claims the turn, reports into it, and
    /// closes it exactly once.
    Turn { turn: TurnId, text: String },
    /// Send the foreground shell command to the background.
    ///
    /// The handle on a running command belongs to the run that started it, which
    /// is why this is work rather than state: the core knows *which* item is in
    /// the foreground and says so, and the engine is the only thing that can let
    /// go of it.
    Promote { item: ItemId },
    /// The console's shell line. It runs in main's context wherever it was
    /// submitted from, and the model may answer it afterwards.
    Shell { turn: TurnId, command: String },
    /// A direct message reached an instance's inbox. Whoever it woke has to be
    /// started, which is the half of a delivery no registry can do for itself.
    Wake,
    /// A post reached a room's log and every member's inbox. What is left is the
    /// room's display row, the chases an `@` owes, and the members it woke.
    Posted(Box<Posted>),
    /// Somebody asked a turn to stop. The core has already recorded the request
    /// — a runner that checks late still sees it — and this is what reaches the
    /// stream that is open right now.
    Interrupt { turn: TurnId },
    /// One action whose work needs a model, a transcript rewrite or a network
    /// round trip.
    ///
    /// The core has already decided it may run and named the operation it runs
    /// under; what is left is the part that leaves the process, and its report
    /// comes back the way an operation's always does.
    Act(Box<Act>),
}

/// An action the core accepted and handed over.
#[derive(Debug, Clone, PartialEq)]
pub struct Act {
    /// The work's identity: progress and the one terminal state are reported
    /// against it. `None` when the work opens its own further down, which is
    /// what bringing a team up does.
    pub operation: Option<OperationId>,
    /// The page it was asked on. A `/compact` queued on an instance's page
    /// summarises that instance's history, not whatever is on screen when it
    /// runs (D135a), and the report lands there too.
    pub on: ConvKey,
    pub action: Action,
}

/// What a room post left for the engine to finish.
///
/// The deposits are not here because they already happened: an inbox is the
/// actor's table, so it filled them before this was sent. What travels is what
/// the deposits *found* — which mentioned member took its copy and which was
/// stopped — because that is what decides who gets chased.
#[derive(Debug, Clone, PartialEq)]
pub struct Posted {
    pub room: String,
    pub from: String,
    pub text: String,
    pub seq: u64,
    /// Members an `@` named whose copy was accepted: each owes an answer, and
    /// each gets a watchdog.
    pub chase: Vec<String>,
    /// Members an `@` named whose copy could not be delivered. The sender hears
    /// about these rather than waiting on a promise nobody took.
    pub undelivered: Vec<String>,
}

/// Who runs it.
///
/// One method, because the core has exactly one thing to say to an engine: this
/// happened, run it. Whatever the run answers comes back through the turn, not
/// through here.
pub trait Engine: Send + Sync + 'static {
    fn run(&self, run: Run);
}

/// The engine of a session that has none: it drops the work and says nothing.
///
/// Not a stub for production — a core with no engine refuses a submission that
/// would need one, by name, before it opens a turn. This is what a test uses
/// when what it asserts is the core's half.
#[derive(Debug, Default, Clone, Copy)]
pub struct Idle;

impl Engine for Idle {
    fn run(&self, _: Run) {}
}

/// An engine handle as the actor holds it.
pub type Attached = Arc<dyn Engine>;

/// An engine that runs nothing and remembers everything.
///
/// What a test of the core's half needs is not a provider — it is proof that the
/// core handed the right work over, in the right order, having already made
/// everything the work refers to.
#[cfg(test)]
#[derive(Default)]
pub struct Recorder(std::sync::Mutex<Vec<Run>>);

#[cfg(test)]
impl Recorder {
    /// What it was handed, in the order it was handed it.
    pub fn taken(&self) -> Vec<Run> {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

#[cfg(test)]
impl Engine for Recorder {
    fn run(&self, run: Run) {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(run);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_idle_engine_takes_the_work_and_does_nothing_with_it() {
        Idle.run(Run::Wake);
        let recorder = Recorder::default();
        recorder.run(Run::Turn {
            turn: TurnId::new("turn_1"),
            text: "run the tests".to_string(),
        });
        assert_eq!(recorder.taken().len(), 1);
    }
}
