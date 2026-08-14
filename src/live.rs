//! Foreground command liveness: the output tail the user watches while a shell
//! command runs, and the promotion that moves that command to the background.
//!
//! Phase 2 of a turn runs every non-concurrency-safe tool serially
//! ([`crate::tool::executor::execute_calls`]), and `Bash::is_concurrency_safe`
//! is always false, so a session has **at most one foreground command in flight
//! at a time**. This module is built on that invariant: one slot, one promote
//! signal, one tail. A subagent runs against its own [`LiveBash`] (a detached
//! one, since its transcript is its own), so nested Bash calls never contend for
//! the main view's slot.
//!
//! Hosts without a foreground surface (headless, `--print`, the JSON protocol)
//! hold [`LiveBash::detached`]: no sink, so nothing is ever emitted, and nobody
//! can promote — the tool behaves exactly as it did before this seam existed.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::watch;

/// How many lines of a running command's output the tail shows. CC keeps the
/// evidence small enough that it never competes with the transcript above it.
pub const TAIL_LINES: usize = 5;
/// Longest line kept in the tail. Rendering clips to the terminal width anyway;
/// this only bounds what a newline-free stream (`cat` on a binary) can cost.
const MAX_TAIL_LINE_CHARS: usize = 512;
/// Floor between two tail events (~10/s), an order under the 30fps redraw: a
/// build that writes a thousand lines a second must not wake the renderer a
/// thousand times.
pub const TAIL_MIN_INTERVAL: Duration = Duration::from_millis(100);

/// One sample of a running command's output: the last [`TAIL_LINES`] logical
/// lines, plus how many lines the command has produced in total.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveTail {
    /// Last lines, oldest first, already stripped of escape sequences.
    pub lines: Vec<String>,
    /// Completed lines so far (the counter on the `⎿` row).
    pub total_lines: usize,
}

/// Where a tail sample goes. The TUI turns it into a `UiEvent`.
pub type TailFn = dyn Fn(LiveTail) + Send + Sync;

/// The seam between the foreground Bash call and the host watching it.
pub struct LiveBash {
    /// `None` for hosts with no foreground surface.
    sink: Option<Arc<TailFn>>,
    /// The one foreground command in flight, if any.
    slot: Mutex<Option<Slot>>,
}

struct Slot {
    promote: watch::Sender<bool>,
    /// Set by [`LiveBash::promote`]: the command is on its way to the
    /// background, so a second ctrl+b must fall through to its other meaning.
    promoted: bool,
}

/// Detached: the shape a context that was handed no host defaults to.
impl Default for LiveBash {
    fn default() -> Self {
        Self {
            sink: None,
            slot: Mutex::new(None),
        }
    }
}

impl LiveBash {
    /// A handle that publishes tails to `sink` and accepts promotion.
    pub fn new(sink: Arc<TailFn>) -> Arc<Self> {
        Arc::new(Self {
            sink: Some(sink),
            slot: Mutex::new(None),
        })
    }

    /// A handle nobody watches: no tail events, no promotion. The default for
    /// headless hosts and subagents.
    pub fn detached() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Whether producing a tail is worth the work at all.
    pub fn tails(&self) -> bool {
        self.sink.is_some()
    }

    /// Publish a sample (no-op when detached).
    pub fn emit(&self, tail: LiveTail) {
        if let Some(sink) = &self.sink {
            sink(tail);
        }
    }

    /// Register the command that is about to run in the foreground. The
    /// returned receiver resolves when the user promotes it; dropping the
    /// [`LiveRun`] guard ends the registration.
    ///
    /// The serial invariant means the slot is free here. If it somehow is not,
    /// the newcomer keeps its own sender alive inside the guard instead of
    /// evicting the incumbent: an evicted sender would drop, and a dropped
    /// sender must never read as "the user pressed ctrl+b".
    pub fn arm(self: &Arc<Self>) -> (LiveRun, watch::Receiver<bool>) {
        let (promote, rx) = watch::channel(false);
        let mut slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        debug_assert!(
            slot.is_none(),
            "two foreground commands at once: Phase 2 runs non-safe tools serially"
        );
        if slot.is_some() {
            return (
                LiveRun {
                    live: self.clone(),
                    owned: false,
                    parked: Some(promote),
                },
                rx,
            );
        }
        *slot = Some(Slot {
            promote,
            promoted: false,
        });
        drop(slot);
        (
            LiveRun {
                live: self.clone(),
                owned: true,
                parked: None,
            },
            rx,
        )
    }

    /// Whether a foreground command is in flight and could still be promoted
    /// (the hint tier asks this every frame).
    pub fn running(&self) -> bool {
        let slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        slot.as_ref().is_some_and(|s| !s.promoted)
    }

    /// Move the running foreground command to the background. Returns false
    /// when there is nothing to move, which is how ctrl+b keeps its other
    /// meaning (the background-agent manager, D80).
    pub fn promote(&self) -> bool {
        let mut slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        let Some(entry) = slot.as_mut().filter(|s| !s.promoted) else {
            return false;
        };
        entry.promoted = true;
        entry.promote.send_replace(true);
        true
    }
}

/// The foreground registration, held for the length of the run.
pub struct LiveRun {
    live: Arc<LiveBash>,
    owned: bool,
    /// A sender kept alive (and never fired) when the slot was already taken,
    /// so the paired receiver parks instead of resolving.
    parked: Option<watch::Sender<bool>>,
}

impl Drop for LiveRun {
    fn drop(&mut self) {
        let _ = self.parked.take();
        if self.owned {
            *self.live.slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
    }
}

/// Resolves when the user promotes the running command.
///
/// A sender that is gone means nobody can promote any more: park forever rather
/// than resolve, because the caller reads resolution as "the user pressed
/// ctrl+b" and would background a command nobody asked to background. Same
/// shape as [`crate::tool::executor::cancel_requested`].
pub async fn promote_requested(rx: &mut watch::Receiver<bool>) {
    loop {
        if rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
        if *rx.borrow_and_update() {
            return;
        }
    }
}

/// Escape-sequence state of [`TailBuffer::push`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Escape {
    #[default]
    Text,
    /// Saw ESC.
    Esc,
    /// Inside `ESC [ … final` (CSI): colours, cursor motion, line erase.
    Csi,
    /// Inside `ESC ] …` (OSC): title setting, hyperlinks.
    Osc,
    /// Inside an OSC, saw ESC (the string terminator is `ESC \`).
    OscEsc,
}

/// The last few logical lines of a stream, kept the way a terminal would show
/// them.
///
/// The result buffer the model gets ([`crate::tool::bash`]'s `BoundedOutput`)
/// stores bytes verbatim — carriage returns, escape sequences and all — and
/// stops growing at its char cap, so it cannot serve as the tail: a
/// progress-bar command would paint hundreds of "lines" and a long build would
/// freeze the tail at the cap. This buffer instead applies terminal semantics
/// as the bytes arrive: `\r` rewrites the current line (a `cargo`/`curl`
/// progress bar collapses to the one line it visually is), `\n` commits it, and
/// escape sequences are dropped rather than passed on to the renderer, which
/// would otherwise write them straight to the terminal.
#[derive(Debug, Default)]
pub struct TailBuffer {
    lines: VecDeque<String>,
    current: String,
    total_lines: usize,
    escape: Escape,
}

impl TailBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a decoded chunk of output (chunks split anywhere, including mid-line).
    pub fn push(&mut self, text: &str) {
        for ch in text.chars() {
            match self.escape {
                Escape::Text => self.push_text(ch),
                Escape::Esc => {
                    self.escape = match ch {
                        '[' => Escape::Csi,
                        ']' => Escape::Osc,
                        // Two-character sequences (ESC c, ESC =, …) end here.
                        _ => Escape::Text,
                    }
                }
                Escape::Csi => {
                    if ('\x40'..='\x7e').contains(&ch) {
                        self.escape = Escape::Text;
                    }
                }
                Escape::Osc => {
                    self.escape = match ch {
                        '\x07' => Escape::Text,
                        '\x1b' => Escape::OscEsc,
                        _ => Escape::Osc,
                    }
                }
                Escape::OscEsc => {
                    self.escape = if ch == '\\' {
                        Escape::Text
                    } else {
                        Escape::Osc
                    }
                }
            }
        }
    }

    fn push_text(&mut self, ch: char) {
        match ch {
            '\n' => self.commit(),
            // A carriage return rewrites the line in place: keep the segment
            // that follows it, which is what the user would be looking at.
            '\r' => self.current.clear(),
            '\x1b' => self.escape = Escape::Esc,
            '\t' => self.push_char(' '),
            c if c.is_control() => {}
            c => self.push_char(c),
        }
    }

    fn push_char(&mut self, ch: char) {
        if self.current.chars().count() < MAX_TAIL_LINE_CHARS {
            self.current.push(ch);
        }
    }

    fn commit(&mut self) {
        self.total_lines += 1;
        self.lines.push_back(std::mem::take(&mut self.current));
        while self.lines.len() > TAIL_LINES {
            self.lines.pop_front();
        }
    }

    /// The rows to paint: completed lines plus the line being written.
    pub fn sample(&self) -> LiveTail {
        let mut lines: Vec<String> = self.lines.iter().cloned().collect();
        if !self.current.is_empty() {
            lines.push(self.current.clone());
        }
        if lines.len() > TAIL_LINES {
            lines.drain(..lines.len() - TAIL_LINES);
        }
        LiveTail {
            lines,
            total_lines: self.total_lines,
        }
    }
}

/// Rate limit for tail events: at most one per [`TAIL_MIN_INTERVAL`], and never
/// one that would repaint rows the host already shows.
///
/// The producer polls faster than the floor so the *last* sample of a burst
/// still lands (a command that writes hard and then goes quiet must not leave a
/// stale tail on screen).
#[derive(Debug, Default)]
pub struct TailCoalescer {
    last: Option<Instant>,
    sent: Vec<String>,
}

impl TailCoalescer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether this sample goes on the wire now.
    pub fn admit(&mut self, now: Instant, tail: &LiveTail) -> bool {
        if self.sent == tail.lines {
            return false;
        }
        if self
            .last
            .is_some_and(|last| now.duration_since(last) < TAIL_MIN_INTERVAL)
        {
            return false;
        }
        self.last = Some(now);
        self.sent.clone_from(&tail.lines);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(tail: &LiveTail) -> Vec<&str> {
        tail.lines.iter().map(String::as_str).collect()
    }

    #[test]
    fn tail_keeps_the_last_five_lines_and_counts_them_all() {
        let mut buf = TailBuffer::new();
        for i in 1..=8 {
            buf.push(&format!("line {i}\n"));
        }
        let tail = buf.sample();
        assert_eq!(
            lines(&tail),
            ["line 4", "line 5", "line 6", "line 7", "line 8"]
        );
        assert_eq!(tail.total_lines, 8);
    }

    #[test]
    fn tail_shows_the_line_still_being_written() {
        let mut buf = TailBuffer::new();
        buf.push("done\nhalf");
        let tail = buf.sample();
        assert_eq!(lines(&tail), ["done", "half"]);
        assert_eq!(tail.total_lines, 1, "an uncommitted line is not counted");
        buf.push(" a line\n");
        assert_eq!(lines(&buf.sample()), ["done", "half a line"]);
    }

    /// A `\r` progress bar (cargo/curl/pip) is one line being rewritten, not a
    /// line per frame: the tail must show the final state, not scroll.
    #[test]
    fn carriage_returns_rewrite_one_logical_line() {
        let mut buf = TailBuffer::new();
        for percent in 0..200 {
            buf.push(&format!("\rdownloading {percent}%"));
        }
        let tail = buf.sample();
        assert_eq!(tail.lines.len(), 1, "{:?}", tail.lines);
        assert_eq!(lines(&tail), ["downloading 199%"]);
        assert_eq!(tail.total_lines, 0, "nothing was ever committed");
        buf.push("\rdone\n");
        assert_eq!(lines(&buf.sample()), ["done"]);
        assert_eq!(buf.sample().total_lines, 1);
    }

    #[test]
    fn escape_sequences_never_reach_the_renderer() {
        let mut buf = TailBuffer::new();
        buf.push("\x1b[31mred\x1b[0m and \x1b]0;title\x07plain\ttab\n");
        assert_eq!(lines(&buf.sample()), ["red and plain tab"]);
    }

    #[test]
    fn escape_sequences_split_across_chunks_are_still_stripped() {
        let mut buf = TailBuffer::new();
        buf.push("a\x1b[");
        buf.push("32");
        buf.push("mb\n");
        assert_eq!(lines(&buf.sample()), ["ab"]);
    }

    #[test]
    fn a_newline_free_stream_cannot_grow_without_bound() {
        let mut buf = TailBuffer::new();
        buf.push(&"x".repeat(MAX_TAIL_LINE_CHARS * 4));
        assert_eq!(buf.sample().lines[0].chars().count(), MAX_TAIL_LINE_CHARS);
    }

    #[test]
    fn coalescer_bounds_a_burst_and_still_sends_the_last_sample() {
        let mut coalescer = TailCoalescer::new();
        let start = Instant::now();
        let mut sent = 0usize;
        // 1000 writes inside one interval: the first lands, the rest coalesce.
        for i in 0..1000 {
            let tail = LiveTail {
                lines: vec![format!("line {i}")],
                total_lines: i,
            };
            if coalescer.admit(start + Duration::from_micros(i as u64), &tail) {
                sent += 1;
            }
        }
        assert_eq!(sent, 1, "a burst inside one interval is one event");
        let last = LiveTail {
            lines: vec!["line 999".to_string()],
            total_lines: 999,
        };
        assert!(
            coalescer.admit(start + TAIL_MIN_INTERVAL, &last),
            "the sample that closed the burst still lands once the floor passes"
        );
    }

    #[test]
    fn coalescer_drops_a_repaint_of_the_same_rows() {
        let mut coalescer = TailCoalescer::new();
        let start = Instant::now();
        let tail = LiveTail {
            lines: vec!["same".to_string()],
            total_lines: 1,
        };
        assert!(coalescer.admit(start, &tail));
        assert!(
            !coalescer.admit(start + Duration::from_secs(10), &tail),
            "unchanged rows are not worth a redraw"
        );
    }

    #[tokio::test]
    async fn promote_fires_the_armed_run_once() {
        let live = LiveBash::detached();
        assert!(!live.running());
        assert!(!live.promote(), "nothing to promote");

        let (run, mut rx) = live.arm();
        assert!(live.running());
        assert!(live.promote());
        assert!(
            !live.running(),
            "a promoted command is no longer foreground"
        );
        assert!(!live.promote(), "ctrl+b falls through the second time");
        promote_requested(&mut rx).await;

        drop(run);
        assert!(!live.running());
    }

    /// The guard clears the slot, and its dropped sender must not read as a
    /// promotion for whoever still holds the receiver.
    #[tokio::test]
    async fn a_dropped_run_parks_the_receiver_instead_of_resolving() {
        let live = LiveBash::detached();
        let (run, mut rx) = live.arm();
        drop(run);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), promote_requested(&mut rx))
                .await
                .is_err(),
            "a closed channel is not a promotion"
        );
    }

    #[test]
    fn a_detached_handle_swallows_tails() {
        let live = LiveBash::detached();
        assert!(!live.tails());
        live.emit(LiveTail::default());
    }

    #[test]
    fn a_wired_handle_publishes_tails() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let live = LiveBash::new(Arc::new(move |tail: LiveTail| {
            sink.lock().unwrap_or_else(|e| e.into_inner()).push(tail);
        }));
        assert!(live.tails());
        live.emit(LiveTail {
            lines: vec!["hi".to_string()],
            total_lines: 1,
        });
        let seen = seen.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].lines, ["hi"]);
    }
}
