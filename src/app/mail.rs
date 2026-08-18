//! The main agent's inbox, digested on a quiet window rather than per message
//! (D98) — and decided here rather than in a frontend (乙案).
//!
//! Before the debounce, every room post with main as a member woke a turn of its
//! own: three agents talking in a room for a minute bought the user three digests
//! of a conversation that had not finished happening. Coalescing is the whole
//! fix — the wake is deferred until the mail stops growing, or until the
//! deadline, so a room that never goes quiet still gets read. Urgent direct mail
//! is the exception at both ends: it rings the moment it lands and does not wait
//! out the window.
//!
//! **Why it is here.** This was `tui::chat_tail::digest_mail`, which meant a GUI
//! either reimplemented it or got a different product. The rule the campaign
//! settled on is that both frontends wake main the same way, so the rule lives
//! in one place and each frontend asks. What is still the caller's is
//! *performing* the wake: opening and running main's turn needs the engine the
//! console still owns (B7).

use std::time::{Duration, Instant};

/// How long the inbox has to stay quiet before a digest turn is due.
///
/// Long enough that a room's back-and-forth arrives as one batch — agents
/// answering each other land within a round trip of one another — and short
/// enough that a single message still feels delivered rather than queued.
pub(crate) const QUIET: Duration = Duration::from_millis(2_000);

/// The ceiling on that wait. A room that never stops talking would otherwise
/// starve the wake forever, because the window restarts on every post.
pub(crate) const DEADLINE: Duration = Duration::from_millis(15_000);

/// The debounce's state for one batch of waiting mail.
#[derive(Debug, Clone, Copy)]
struct Batch {
    /// When the batch started; the deadline is measured from here.
    first_seen: Instant,
    /// When the inbox last changed size; the quiet window restarts here.
    last_change: Instant,
    /// How much was waiting when it was last looked at.
    waiting: usize,
    /// Something in this batch asked for the attention channel. Sticky for the
    /// batch: the flag itself is one-shot and the console takes it to ring its
    /// bell, so a debounce that re-read it would find it already spent.
    urgent: bool,
    /// A digest turn has been asked for. The drain clears the batch.
    woken: bool,
}

/// What the core decided about the mail waiting for main.
#[derive(Debug, Default)]
pub(crate) struct MailWake {
    batch: Option<Batch>,
}

/// What changed about the waiting mail, for the frontends that show it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Waiting {
    /// A batch started waiting, or grew.
    Started { count: usize, urgent: bool },
    /// The inbox is empty again.
    Cleared,
    /// Nothing worth saying.
    Unchanged,
}

impl MailWake {
    /// Read where the inbox stands. Returns what to say about it, if anything.
    pub(crate) fn observe(&mut self, now: Instant, waiting: usize, urgent: bool) -> Waiting {
        if waiting == 0 {
            return match self.batch.take() {
                Some(_) => Waiting::Cleared,
                None => Waiting::Unchanged,
            };
        }
        match &mut self.batch {
            None => {
                self.batch = Some(Batch {
                    first_seen: now,
                    last_change: now,
                    waiting,
                    urgent,
                    woken: false,
                });
                Waiting::Started {
                    count: waiting,
                    urgent,
                }
            }
            Some(batch) if batch.waiting != waiting => {
                batch.waiting = waiting;
                batch.last_change = now;
                batch.urgent |= urgent;
                Waiting::Started {
                    count: waiting,
                    urgent: batch.urgent,
                }
            }
            Some(batch) if urgent && !batch.urgent => {
                batch.urgent = true;
                Waiting::Started {
                    count: waiting,
                    urgent: true,
                }
            }
            Some(_) => Waiting::Unchanged,
        }
    }

    /// Whether a digest turn is due.
    ///
    /// Urgent mail skips the window at both ends; otherwise the batch has to have
    /// gone quiet or run out of time. One wake per batch: the latch is what stops
    /// a frontend polling every frame from starting a turn per frame.
    pub(crate) fn due(&self, now: Instant) -> bool {
        let Some(batch) = &self.batch else {
            return false;
        };
        if batch.woken {
            return false;
        }
        let quiet = now.saturating_duration_since(batch.last_change) >= QUIET;
        let overdue = now.saturating_duration_since(batch.first_seen) >= DEADLINE;
        batch.urgent || quiet || overdue
    }

    /// A digest turn was asked for. Nothing more is due until this batch drains.
    pub(crate) fn woke(&mut self) {
        if let Some(batch) = &mut self.batch {
            batch.woken = true;
        }
    }

    pub(crate) fn is_waiting(&self) -> bool {
        self.batch.is_some()
    }

    /// Move the batch's clocks back, so a test can reach the far side of a
    /// window without waiting out two real seconds.
    #[cfg(test)]
    pub(crate) fn rewind(&mut self, by: Duration) {
        if let Some(batch) = &mut self.batch {
            batch.first_seen -= by;
            batch.last_change -= by;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A burst coalesces into one wake: every message restarts the quiet window,
    /// and the wake happens once the inbox stops growing.
    #[test]
    fn a_burst_wakes_once_after_the_quiet_window() {
        let start = Instant::now();
        let mut wake = MailWake::default();
        assert_eq!(
            wake.observe(start, 1, false),
            Waiting::Started {
                count: 1,
                urgent: false
            }
        );
        assert!(!wake.due(start), "the window has not run yet");

        let nearly = start + QUIET - Duration::from_millis(1);
        assert!(!wake.due(nearly));
        assert_eq!(
            wake.observe(nearly, 2, false),
            Waiting::Started {
                count: 2,
                urgent: false
            },
            "a second message restarts the window"
        );
        assert!(!wake.due(nearly + QUIET - Duration::from_millis(1)));
        assert!(wake.due(nearly + QUIET), "and then it is due");

        wake.woke();
        assert!(
            !wake.due(nearly + QUIET),
            "one wake per batch, however often it is asked"
        );
    }

    /// A room that never goes quiet cannot starve the wake: the deadline is
    /// measured from the batch, not from the last message.
    #[test]
    fn a_chatty_room_cannot_starve_the_wake_past_the_deadline() {
        let start = Instant::now();
        let mut wake = MailWake::default();
        let mut now = start;
        let mut waiting = 0;
        loop {
            waiting += 1;
            wake.observe(now, waiting, false);
            if wake.due(now) {
                break;
            }
            now += QUIET - Duration::from_millis(1);
            assert!(
                now.saturating_duration_since(start) <= DEADLINE + QUIET,
                "the deadline is a ceiling, not a suggestion"
            );
        }
        assert!(
            now.saturating_duration_since(start) >= DEADLINE,
            "and not before it"
        );
    }

    /// Urgent mail skips the window.
    #[test]
    fn urgent_mail_is_due_the_moment_it_lands() {
        let now = Instant::now();
        let mut wake = MailWake::default();
        wake.observe(now, 1, true);
        assert!(wake.due(now), "urgency is the batch's, and it is sticky");

        let mut ordinary = MailWake::default();
        ordinary.observe(now, 1, false);
        assert!(!ordinary.due(now), "an ordinary batch still waits");
    }

    /// An emptied inbox disarms the debounce, so the next batch starts its own
    /// window rather than inheriting a spent one.
    #[test]
    fn a_drained_inbox_disarms_the_window() {
        let start = Instant::now();
        let mut wake = MailWake::default();
        wake.observe(start, 1, false);
        wake.woke();
        assert_eq!(wake.observe(start, 0, false), Waiting::Cleared);
        assert_eq!(
            wake.observe(start, 0, false),
            Waiting::Unchanged,
            "an empty inbox that was already empty is not news"
        );
        assert!(!wake.is_waiting());

        wake.observe(start, 1, false);
        assert!(!wake.due(start), "the new batch waits out its own window");
        assert!(wake.due(start + QUIET));
    }
}

// ---------------------------------------------------------------------------
// Reaching it from outside the actor
// ---------------------------------------------------------------------------

use tokio::sync::{mpsc, oneshot};

use crate::app::answer::Answer;
use crate::app::controller::Control;

/// What the actor is asked about the waiting mail.
pub(crate) enum MailMsg {
    /// Is a digest turn due right now?
    Due {
        /// The console's own interrupted flag, which is a frontend's state until
        /// B7 gives the run state to the core. A turn must not be started on top
        /// of an interrupt the user has not finished.
        interrupted: bool,
        reply: oneshot::Sender<bool>,
    },
    /// A digest turn was asked for.
    Woke,
    /// Is a batch waiting at all?
    Waiting { reply: oneshot::Sender<bool> },
    /// Move the batch's clocks back (tests only).
    #[cfg(test)]
    Rewind { by: Duration },
}

/// How the rest of the process asks the core whether main should wake.
#[derive(Clone)]
pub struct MailHandle {
    control: mpsc::UnboundedSender<Control>,
}

impl std::fmt::Debug for MailHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MailHandle")
    }
}

impl MailHandle {
    pub(crate) fn new(control: mpsc::UnboundedSender<Control>) -> Self {
        Self { control }
    }

    /// Whether a digest turn is due: the window, the urgency, main's own turn and
    /// main's queue, all decided in one place.
    pub fn due(&self, interrupted: bool) -> Answer<bool> {
        let (reply, answer) = oneshot::channel();
        let _ = self
            .control
            .send(Control::Mail(MailMsg::Due { interrupted, reply }));
        Answer::new(answer, false)
    }

    /// A digest turn was started. Nothing more is due until the batch drains.
    pub fn woke(&self) {
        let _ = self.control.send(Control::Mail(MailMsg::Woke));
    }

    /// Whether a batch of mail is waiting to be read.
    pub fn is_waiting(&self) -> Answer<bool> {
        let (reply, answer) = oneshot::channel();
        let _ = self.control.send(Control::Mail(MailMsg::Waiting { reply }));
        Answer::new(answer, false)
    }

    /// Move the batch's clocks back, so a test can reach the far side of a
    /// window without waiting out two real seconds.
    #[cfg(test)]
    pub fn rewind(&self, by: Duration) {
        let _ = self.control.send(Control::Mail(MailMsg::Rewind { by }));
    }
}
