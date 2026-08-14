//! `notify_user` (D94): the one deliberate road from a subagent to the user.
//!
//! The hub is the user's conversation with the main agent, not the system's
//! message bus. Everything an agent *does* belongs in its own DM; the handful of
//! things the user actually needs to look up for — the overall task is finished,
//! something is blocking, a finding that changes the plan — come through here and
//! land as one line in the hub.
//!
//! Because it is the one road, it is also the one place that can flood. A relay
//! is rate limited **per agent**: one line per [`WINDOW`], and everything else in
//! that window is counted rather than printed. Nothing is lost by counting —
//! the text was written by a tool call, so it is already in that agent's
//! transcript and reaches the user through its DM. What the window protects is
//! the hub's scarcity: a line there is supposed to mean something.
//!
//! The module owns the decision and not the drawing. It hands out [`Notice`]s
//! through a sink the host installs — the TUI turns them into a `UiEvent`, a
//! headless run prints one line to stderr, and a host with no user surface takes
//! [`Relay::detached`] and sees nothing. Same shape as [`crate::live::LiveBash`].
//!
//! Time is a parameter, never a call to the clock: every entry point takes
//! `now`, so the window is testable without sleeping (the [`crate::token_rate`]
//! pattern).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// One relay line per agent per window; the rest are counted.
///
/// Also the ceiling on the D79 attention channel: an `urgent` notice always
/// prints its line, but it may only interrupt the user once per window.
pub const WINDOW: Duration = Duration::from_secs(60);

/// How loud a notice is.
///
/// `info` is a line in the hub and an unread badge — it waits until the user
/// looks. `urgent` additionally fires the D79 notifier, which is the only thing
/// in bingo that reaches a user who is in another window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum NotifyLevel {
    /// A line in the hub. The user reads it when they next look.
    #[default]
    Info,
    /// Also rings the D79 attention channel.
    Urgent,
}

/// What the host is asked to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// A relay that won its window: shown verbatim.
    Relay {
        agent: String,
        text: String,
        level: NotifyLevel,
        /// Whether the D79 notifier should fire for this one. Decided here so
        /// the per-agent ceiling holds across every host that renders a notice.
        notifier: bool,
    },
    /// What the window swallowed, reported once when it rolls.
    Coalesced { agent: String, count: usize },
}

impl Notice {
    /// The agent this notice is about.
    pub fn agent(&self) -> &str {
        match self {
            Notice::Relay { agent, .. } | Notice::Coalesced { agent, .. } => agent,
        }
    }

    /// The hub line, without the stamp the renderer hangs on it.
    ///
    /// One string for every host: the TUI, a headless run and any future
    /// surface say the same thing, so a user who moves between them is not
    /// learning two vocabularies.
    pub fn line(&self) -> String {
        match self {
            Notice::Relay { agent, text, .. } => format!("🔔 @{agent} → you: {text}"),
            Notice::Coalesced { agent, count } => {
                format!("🔔 @{agent}: {count} more — see the DM")
            }
        }
    }
}

/// What the calling agent is told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The line is on the user's screen.
    Delivered,
    /// The window is still open; this notice is counted into the next line.
    /// `pending` is how many are waiting, this one included.
    Queued { pending: usize },
}

impl Verdict {
    /// The tool result the model reads.
    ///
    /// A queued notice is not a failure and must not read like one: the text
    /// still reaches the user through the DM, and saying so stops an agent from
    /// "helpfully" retrying into the same closed window.
    pub fn tool_result(self) -> String {
        match self {
            Verdict::Delivered => "delivered".to_string(),
            Verdict::Queued { pending } => format!(
                "queued: the user was notified less than {}s ago, so this notice is counted \
                 into the next relay ({pending} waiting). Nothing is lost — the text is in \
                 this transcript and reaches the user in your DM. Do not resend it.",
                WINDOW.as_secs()
            ),
        }
    }
}

/// Where notices go. The host installs one; the domain never draws.
pub type Sink = Arc<dyn Fn(Notice) + Send + Sync>;

/// One line per notice on stderr, for a run with no viewport.
pub fn stderr_sink() -> Sink {
    Arc::new(|notice: Notice| eprintln!("[bingo] {}", notice.line()))
}

/// One agent's window.
#[derive(Debug, Default)]
struct Window {
    /// When this agent last put a line in the hub.
    last_line: Option<Instant>,
    /// When the D79 notifier last fired for this agent.
    last_notifier: Option<Instant>,
    /// Notices counted since `last_line`.
    pending: usize,
}

impl Window {
    /// Whether `at` is far enough past `mark` to open a new window. `None` is an
    /// agent that has never spoken, which is always due.
    fn due(mark: Option<Instant>, at: Instant) -> bool {
        mark.is_none_or(|t| at.saturating_duration_since(t) >= WINDOW)
    }
}

/// The rate-limited road from an agent to the user.
///
/// Shared: the tool side calls [`Relay::notify`], the host side calls
/// [`Relay::flush_due`] on its tick to collect what the rolling window owes.
pub struct Relay {
    /// Interior-mutable because the session outlives the surface: the relay is
    /// built with the session (so a subagent inherits it in `build_sub_session`),
    /// while the channel a notice travels on is created later, when the host
    /// builds its screen. The host [`attach`](Relay::attach)es then.
    sink: Mutex<Sink>,
    windows: Mutex<HashMap<String, Window>>,
}

impl std::fmt::Debug for Relay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Relay").finish_non_exhaustive()
    }
}

/// Silent. A context built without a host — every tool test, the JSON protocol
/// host — relays nothing, exactly as [`crate::live::LiveBash`] publishes nothing.
impl Default for Relay {
    fn default() -> Self {
        Self {
            sink: Mutex::new(Arc::new(|_| {})),
            windows: Mutex::new(HashMap::new()),
        }
    }
}

impl Relay {
    /// A relay that hands every notice to `sink`.
    pub fn new(sink: Sink) -> Arc<Self> {
        Arc::new(Self {
            sink: Mutex::new(sink),
            windows: Mutex::new(HashMap::new()),
        })
    }

    /// Point an existing relay at a surface. The host calls this once, when it
    /// has a channel to send on; everything already counted keeps its window.
    pub fn attach(&self, sink: Sink) {
        *self.sink.lock().unwrap_or_else(|e| e.into_inner()) = sink;
    }

    /// Hand a notice to whatever is currently attached, holding no other lock.
    fn emit(&self, notice: Notice) {
        let sink = self.sink.lock().unwrap_or_else(|e| e.into_inner()).clone();
        sink(notice);
    }

    /// A relay with nowhere to show a notice: the JSON protocol host, and every
    /// test that builds a session without a screen.
    ///
    /// The rate limiting still runs, so the tool answers the model the same way
    /// it would anywhere else — a contract that changed with the host would be a
    /// contract the model cannot learn.
    pub fn detached() -> Arc<Self> {
        Self::new(Arc::new(|_| {}))
    }

    /// Offer a notice. Returns what to tell the calling agent.
    ///
    /// `urgent` bypasses the coalescing — a blocker the user has to act on is
    /// worth a line whenever it arrives — but it does not bypass the attention
    /// ceiling: the notifier still fires at most once per agent per [`WINDOW`],
    /// so an agent cannot turn a bad loop into a stream of interruptions.
    pub fn notify(&self, agent: &str, text: &str, level: NotifyLevel, now: Instant) -> Verdict {
        let (verdict, notice) = {
            let mut windows = self.windows.lock().unwrap_or_else(|e| e.into_inner());
            let window = windows.entry(agent.to_string()).or_default();
            match level {
                NotifyLevel::Urgent => {
                    let notifier = Window::due(window.last_notifier, now);
                    if notifier {
                        window.last_notifier = Some(now);
                    }
                    window.last_line = Some(now);
                    (
                        Verdict::Delivered,
                        Some(Notice::Relay {
                            agent: agent.to_string(),
                            text: text.to_string(),
                            level,
                            notifier,
                        }),
                    )
                }
                NotifyLevel::Info if Window::due(window.last_line, now) => {
                    window.last_line = Some(now);
                    (
                        Verdict::Delivered,
                        Some(Notice::Relay {
                            agent: agent.to_string(),
                            text: text.to_string(),
                            level,
                            notifier: false,
                        }),
                    )
                }
                NotifyLevel::Info => {
                    window.pending += 1;
                    (
                        Verdict::Queued {
                            pending: window.pending,
                        },
                        None,
                    )
                }
            }
        };
        // Outside the lock: a sink is host code and may do anything, including
        // re-entering this relay.
        if let Some(notice) = notice {
            self.emit(notice);
        }
        verdict
    }

    /// Emit what the rolled windows owe. Called on the host's tick.
    ///
    /// A coalesced line opens a fresh window, so a persistently chatty agent
    /// costs one line per minute and never more.
    pub fn flush_due(&self, now: Instant) {
        let due = {
            let mut windows = self.windows.lock().unwrap_or_else(|e| e.into_inner());
            let mut due = Vec::new();
            for (agent, window) in windows.iter_mut() {
                if window.pending == 0 || !Window::due(window.last_line, now) {
                    continue;
                }
                due.push(Notice::Coalesced {
                    agent: agent.clone(),
                    count: window.pending,
                });
                window.pending = 0;
                window.last_line = Some(now);
            }
            due
        };
        // Deterministic order: a HashMap walk is not one, and two agents rolling
        // on the same tick must not reorder between runs (or between test runs).
        let mut due = due;
        due.sort_by(|a, b| a.agent().cmp(b.agent()));
        for notice in due {
            self.emit(notice);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A relay that records what it was asked to show.
    fn recording() -> (Arc<Relay>, Arc<Mutex<Vec<Notice>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = seen.clone();
        let relay = Relay::new(Arc::new(move |notice| {
            sink_seen
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(notice);
        }));
        (relay, seen)
    }

    fn lines(seen: &Arc<Mutex<Vec<Notice>>>) -> Vec<String> {
        seen.lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(Notice::line)
            .collect()
    }

    #[test]
    fn the_first_notice_of_a_window_is_delivered_and_the_rest_are_counted() {
        let (relay, seen) = recording();
        let t0 = Instant::now();

        assert_eq!(
            relay.notify("scout", "the parser is fixed", NotifyLevel::Info, t0),
            Verdict::Delivered
        );
        assert_eq!(
            relay.notify(
                "scout",
                "and the tests pass",
                NotifyLevel::Info,
                t0 + Duration::from_secs(10)
            ),
            Verdict::Queued { pending: 1 },
            "a second notice inside the window is counted, not printed"
        );
        assert_eq!(
            lines(&seen),
            vec!["🔔 @scout → you: the parser is fixed".to_string()],
            "exactly one line so far"
        );

        // The window has not rolled yet, so there is nothing to flush.
        relay.flush_due(t0 + Duration::from_secs(10));
        assert_eq!(lines(&seen).len(), 1, "an open window owes nothing");

        relay.flush_due(t0 + WINDOW + Duration::from_secs(1));
        assert_eq!(
            lines(&seen),
            vec![
                "🔔 @scout → you: the parser is fixed".to_string(),
                "🔔 @scout: 1 more — see the DM".to_string(),
            ],
            "the rolled window reports what it swallowed"
        );
    }

    #[test]
    fn a_rolled_window_delivers_again() {
        let (relay, seen) = recording();
        let t0 = Instant::now();
        relay.notify("scout", "one", NotifyLevel::Info, t0);
        assert_eq!(
            relay.notify("scout", "two", NotifyLevel::Info, t0 + WINDOW),
            Verdict::Delivered,
            "a full window later, the agent may speak again"
        );
        assert_eq!(lines(&seen).len(), 2);
    }

    #[test]
    fn the_window_is_per_agent() {
        let (relay, seen) = recording();
        let t0 = Instant::now();
        relay.notify("scout", "one", NotifyLevel::Info, t0);
        assert_eq!(
            relay.notify("zoe", "one", NotifyLevel::Info, t0),
            Verdict::Delivered,
            "one agent's window does not silence another's"
        );
        assert_eq!(lines(&seen).len(), 2);
    }

    #[test]
    fn urgent_always_prints_but_rings_once_per_window() {
        let (relay, seen) = recording();
        let t0 = Instant::now();

        relay.notify("scout", "quiet note", NotifyLevel::Info, t0);
        assert_eq!(
            relay.notify(
                "scout",
                "blocked on a key",
                NotifyLevel::Urgent,
                t0 + Duration::from_secs(5)
            ),
            Verdict::Delivered,
            "urgent is not coalesced away by an open info window"
        );
        assert_eq!(
            relay.notify(
                "scout",
                "still blocked",
                NotifyLevel::Urgent,
                t0 + Duration::from_secs(10)
            ),
            Verdict::Delivered
        );

        let fired: Vec<bool> = seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter_map(|n| match n {
                Notice::Relay { notifier, .. } => Some(*notifier),
                Notice::Coalesced { .. } => None,
            })
            .collect();
        assert_eq!(
            fired,
            vec![false, true, false],
            "three lines, and the attention channel fires exactly once"
        );

        assert_eq!(
            relay.notify(
                "scout",
                "day two",
                NotifyLevel::Urgent,
                t0 + WINDOW + Duration::from_secs(11)
            ),
            Verdict::Delivered
        );
        let fired_later = seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|n| matches!(n, Notice::Relay { notifier: true, .. }))
            .count();
        assert_eq!(fired_later, 2, "a new window may ring again");
    }

    #[test]
    fn a_queued_notice_tells_the_agent_not_to_resend() {
        let queued = Verdict::Queued { pending: 3 }.tool_result();
        assert!(queued.contains("3 waiting"), "{queued}");
        assert!(queued.contains("Do not resend"), "{queued}");
        assert!(
            queued.contains("reaches the user in your DM"),
            "a queued notice must not read as a loss: {queued}"
        );
        assert_eq!(Verdict::Delivered.tool_result(), "delivered");
    }

    #[test]
    fn a_detached_relay_still_rate_limits() {
        let relay = Relay::detached();
        let t0 = Instant::now();
        assert_eq!(
            relay.notify("scout", "one", NotifyLevel::Info, t0),
            Verdict::Delivered
        );
        assert_eq!(
            relay.notify("scout", "two", NotifyLevel::Info, t0),
            Verdict::Queued { pending: 1 },
            "the answer the model gets may not depend on whether a screen exists"
        );
        // Nowhere to draw, but nothing panics.
        relay.flush_due(t0 + WINDOW);
    }

    #[test]
    fn coalesced_flushes_are_ordered_by_agent() {
        let (relay, seen) = recording();
        let t0 = Instant::now();
        for agent in ["zoe", "scout", "ada"] {
            relay.notify(agent, "first", NotifyLevel::Info, t0);
            relay.notify(agent, "second", NotifyLevel::Info, t0);
        }
        seen.lock().unwrap_or_else(|e| e.into_inner()).clear();
        relay.flush_due(t0 + WINDOW);
        assert_eq!(
            lines(&seen),
            vec![
                "🔔 @ada: 1 more — see the DM".to_string(),
                "🔔 @scout: 1 more — see the DM".to_string(),
                "🔔 @zoe: 1 more — see the DM".to_string(),
            ],
            "a HashMap walk is not an order; the flush imposes one"
        );
    }
}
