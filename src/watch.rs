//! Watchable notification mechanism: any entity that can be watched — commands, agents, etc.
//!
//! Every watchable has a state machine (Running → Idle → Done/Failed/Cancelled);
//! state transitions are broadcast to the TUI, and terminal notifications are queued
//! for injection into the main agent's context.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::broadcast;

/// Watchable lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchState {
    Running,
    /// One round completed (round boundary of periodic commands); may repeat before terminal.
    Idle,
    Done,
    Failed,
    /// Cancel terminal state (used once the kill entry lands).
    #[allow(dead_code)]
    Cancelled,
}

impl WatchState {
    /// Terminal: polling stops, the snapshot freezes.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }
}

/// Watchable category: the display layer picks the icon (⏺ command / ◉ subagent / ◇ channel);
/// doesn't affect the state machine or notification semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WatchKind {
    #[default]
    Command,
    Agent,
    Channel,
}

/// Unique watchable identifier within a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WatchId(pub u64);

impl std::fmt::Display for WatchId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// State snapshot returned by poll.
#[derive(Debug, Clone)]
pub struct WatchPoll {
    pub state: WatchState,
    /// Human-readable current activity (e.g. "round 3 · 12 lines of output").
    pub detail: Option<String>,
    /// Structured data (e.g. the completed final message).
    pub payload: Option<serde_json::Value>,
    /// Condition signal: carries text when a condition is met (e.g. an error in the log);
    /// notifies unconditionally.
    pub signal: Option<String>,
}

/// A watchable entity: declares its own check interval and round semantics.
pub trait Watchable: Send + Sync {
    fn label(&self) -> String;
    fn poll(&self) -> WatchPoll;
    /// Periodic polling interval; None = no polling (the implementer calls set_state itself).
    fn check_interval(&self) -> Option<Duration>;
    /// Category (display icon); defaults to command.
    fn kind(&self) -> WatchKind {
        WatchKind::Command
    }
}

/// Notification condition: matches the incremental content fed to the watchable
/// (feed_content); a hit triggers a signal on the polling tick (unconditional notification).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotifyCondition {
    /// Content contains any of the words (substring match).
    Contains(Vec<String>),
    /// Content matches a regex.
    Regex(String),
    /// Cumulative line count exceeds a threshold (notifies once on breakthrough).
    #[allow(dead_code)] // used once the line-count condition entry (e.g. notify_lines) lands
    LinesOver(usize),
    /// Default error-line pattern (common error keywords).
    Errors,
}

/// Cap on a single signal text: `tail -f` can produce MBs of error logs in one tick;
/// without a limit this would flood the model context.
const MAX_SIGNAL_CHARS: usize = 500;
/// Max hit lines participating in condition matching per round (beyond that, only a count is reported).
const MAX_MATCH_HITS: usize = 20;
/// Feed buffer cap: when nobody drains (e.g. polling not started), the oldest lines are dropped — no unbounded growth.
const MAX_FEED_BUFFER_LINES: usize = 1000;
/// Cap on retained terminal entries: beyond this, the oldest terminal entries are pruned.
const MAX_TERMINAL_ENTRIES: usize = 64;

/// Truncate by chars (multibyte-safe) and annotate.
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max).collect();
    format!("{head}…[truncated]")
}

/// Hit-line aggregation: line cap + length cap, so a single signal can't blow up the context.
/// Reserves headroom for the condition prefix and the hit count so the aggregate stays
/// within MAX_SIGNAL_CHARS overall.
fn join_hits(hits: &[&str]) -> String {
    const HITS_BUDGET: usize = MAX_SIGNAL_CHARS - 120;
    let shown: Vec<&str> = hits.iter().take(MAX_MATCH_HITS).copied().collect();
    let mut text = truncate_chars(&shown.join(" | "), HITS_BUDGET);
    if hits.len() > shown.len() {
        text.push_str(&format!(" …（共 {} 行命中）", hits.len()));
    }
    text
}

/// Polling state of a condition.
struct ConditionState {
    cond: NotifyCondition,
    /// After a LinesOver breakthrough, no repeat notifications.
    fired: bool,
}

impl ConditionState {
    fn new(cond: NotifyCondition) -> Self {
        Self { cond, fired: false }
    }
    /// Match against one round of content buffer; returns the signal text (incremental
    /// conditions matched one by one, aggregated output).
    fn match_buffer(&mut self, buffer: &[String], total_lines: usize) -> Option<String> {
        match &self.cond {
            NotifyCondition::Contains(patterns) => {
                let hits: Vec<&str> = buffer
                    .iter()
                    .filter(|l| patterns.iter().any(|p| l.contains(p.as_str())))
                    .map(|l| l.as_str())
                    .collect();
                if hits.is_empty() {
                    None
                } else {
                    Some(format!(
                        "命中条件 {}：{}",
                        patterns.join("/"),
                        join_hits(&hits)
                    ))
                }
            }
            NotifyCondition::Regex(pattern) => {
                let re = match regex::Regex::new(pattern) {
                    Ok(re) => re,
                    Err(_) => return None,
                };
                let hits: Vec<&str> = buffer
                    .iter()
                    .filter(|l| re.is_match(l))
                    .map(|l| l.as_str())
                    .collect();
                if hits.is_empty() {
                    None
                } else {
                    Some(format!("命中条件 /{pattern}/：{}", join_hits(&hits)))
                }
            }
            NotifyCondition::Errors => {
                let hits: Vec<&str> = buffer
                    .iter()
                    .filter(|l| is_error_line(l))
                    .map(|l| l.as_str())
                    .collect();
                if hits.is_empty() {
                    None
                } else {
                    Some(format!("检测到错误行：{}", join_hits(&hits)))
                }
            }
            NotifyCondition::LinesOver(n) => {
                if !self.fired && total_lines > *n {
                    self.fired = true;
                    Some(format!("输出已超过 {n} 行（当前 {total_lines} 行）"))
                } else {
                    None
                }
            }
        }
    }
}

/// Common error-line patterns (signal source for the default Errors condition).
fn is_error_line(line: &str) -> bool {
    const PATTERNS: &[&str] = &[
        "error",
        "ERROR",
        "Error",
        "Traceback",
        "panic",
        "PANIC",
        "FATAL",
        "fatal",
        "FAILED",
        "Exception",
        "❌",
    ];
    PATTERNS.iter().any(|p| line.contains(p))
}

/// Event broadcast to subscribers (TUI).
#[derive(Debug, Clone)]
pub struct WatchEvent {
    /// Event source identifier (TUI locates by label; id is for programmatic subscribers).
    #[allow(dead_code)]
    pub id: WatchId,
    pub label: String,
    pub kind: WatchKind,
    pub state: WatchState,
    pub detail: Option<String>,
    pub payload: Option<serde_json::Value>,
    /// Condition signal text (present when a condition is met).
    pub signal: Option<String>,
    /// Milliseconds the watchable has been running (since registration).
    pub elapsed_ms: u64,
}

/// Snapshot: for TUI initial render / state queries (the current display layer is driven
/// by label; the API is reserved).
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct WatchSnapshot {
    pub id: WatchId,
    pub label: String,
    pub state: WatchState,
    pub detail: Option<String>,
    pub payload: Option<serde_json::Value>,
}

/// Terminal notification pending injection into the model (adjacent Idle entries with the
/// same id are merged on consume).
#[derive(Debug, Clone)]
struct Notification {
    id: WatchId,
    label: String,
    state: WatchState,
    detail: Option<String>,
    payload: Option<serde_json::Value>,
    signal: Option<String>,
}

struct Entry {
    label: String,
    kind: WatchKind,
    state: WatchState,
    detail: Option<String>,
    payload: Option<serde_json::Value>,
    born: Instant,
    conditions: Vec<ConditionState>,
    /// Content fed in this round (between two ticks).
    feed_buffer: Vec<String>,
    /// Cumulative fed line count (for the LinesOver condition).
    total_lines: usize,
}

struct Inner {
    next_id: u64,
    entries: HashMap<WatchId, Entry>,
    notifications: VecDeque<Notification>,
}

/// Session-level watch registry (Session holds the Arc).
pub struct WatchRegistry {
    inner: Mutex<Inner>,
    tx: broadcast::Sender<WatchEvent>,
}

impl WatchRegistry {
    pub fn new() -> Arc<Self> {
        let (tx, _) = broadcast::channel(256);
        Arc::new(Self {
            inner: Mutex::new(Inner {
                next_id: 1,
                entries: HashMap::new(),
                notifications: VecDeque::new(),
            }),
            tx,
        })
    }

    /// Register and configure notification conditions (content increments fed via
    /// feed_content are matched against the conditions → signals).
    pub fn register_with_conditions(
        self: &Arc<Self>,
        watchable: Box<dyn Watchable>,
        conditions: Vec<NotifyCondition>,
    ) -> WatchId {
        let poll = watchable.poll();
        let label = watchable.label();
        let kind = watchable.kind();
        let id = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let id = WatchId(inner.next_id);
            inner.next_id += 1;
            inner.entries.insert(
                id,
                Entry {
                    label: label.clone(),
                    kind,
                    state: poll.state,
                    detail: poll.detail.clone(),
                    payload: poll.payload.clone(),
                    born: Instant::now(),
                    conditions: conditions.into_iter().map(ConditionState::new).collect(),
                    feed_buffer: Vec::new(),
                    total_lines: 0,
                },
            );
            id
        };
        // Initial state is force-broadcast (set_state's idempotency would swallow a state equal to the entry's).
        if poll.state.is_terminal() || poll.state == WatchState::Idle {
            self.inner
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .notifications
                .push_back(Notification {
                    id,
                    label: label.clone(),
                    state: poll.state,
                    detail: poll.detail.clone(),
                    payload: poll.payload.clone(),
                    signal: None,
                });
        }
        let _ = self.tx.send(WatchEvent {
            id,
            label,
            kind,
            state: poll.state,
            detail: poll.detail.clone(),
            payload: poll.payload.clone(),
            signal: None,
            elapsed_ms: 0,
        });
        let interval = watchable.check_interval();
        if let Some(interval) = interval {
            let registry = self.clone();
            let watchable = Arc::new(watchable);
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval(interval);
                loop {
                    ticker.tick().await;
                    // Externally set to terminal (e.g. subagent done): stop polling, don't overwrite.
                    if registry.is_terminal(id) {
                        break;
                    }
                    let p = watchable.poll();
                    if let Some(sig) = p.signal.clone() {
                        registry.emit_signal(id, sig, p.detail.clone());
                    }
                    // Condition matching: this round's fed content hits a notification condition → signal.
                    for sig in registry.match_conditions(id) {
                        registry.emit_signal(id, sig, p.detail.clone());
                    }
                    let terminal = p.state.is_terminal();
                    registry.set_state(id, p.state, p.detail, p.payload);
                    if terminal {
                        break;
                    }
                }
            });
        }
        id
    }

    /// The implementer feeds content increments (a line or a text block): the condition
    /// engine matches against the notification conditions; hits are aggregated into a
    /// signal triggered by the polling tick.
    pub fn feed_content(&self, id: WatchId, text: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(entry) = inner.entries.get_mut(&id) else {
            return;
        };
        entry.total_lines += text.lines().count();
        entry.feed_buffer.push(text.to_string());
        // Drop the oldest lines when nobody drains: the buffer doesn't grow unboundedly.
        if entry.feed_buffer.len() > MAX_FEED_BUFFER_LINES {
            let overflow = entry.feed_buffer.len() - MAX_FEED_BUFFER_LINES;
            entry.feed_buffer.drain(..overflow);
        }
    }

    /// Match this round's fed content against the notification conditions; returns the
    /// hit signals (incremental conditions matched one by one, aggregated).
    pub fn match_conditions(&self, id: WatchId) -> Vec<String> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(entry) = inner.entries.get_mut(&id) else {
            return Vec::new();
        };
        let buffer = std::mem::take(&mut entry.feed_buffer);
        let total = entry.total_lines;
        let mut signals = Vec::new();
        for cs in &mut entry.conditions {
            if let Some(sig) = cs.match_buffer(&buffer, total) {
                signals.push(sig);
            }
        }
        signals
    }

    /// Condition signal: the implementer calls this when a condition is met (e.g. an
    /// error in the log); goes into the notification queue + broadcast unconditionally
    /// (doesn't change state).
    pub fn emit_signal(&self, id: WatchId, signal: String, detail: Option<String>) {
        // Single-signal cap: any long text fed by callers is cut off here.
        let signal = truncate_chars(&signal, MAX_SIGNAL_CHARS);
        let (label, kind, state, entry_detail) = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let Some(entry) = inner.entries.get_mut(&id) else {
                return;
            };
            let label = entry.label.clone();
            let kind = entry.kind;
            let state = entry.state;
            if let Some(d) = &detail {
                entry.detail = Some(d.clone());
            }
            let entry_detail = entry.detail.clone();
            inner.notifications.push_back(Notification {
                id,
                label: label.clone(),
                state,
                detail,
                payload: None,
                signal: Some(signal.clone()),
            });
            (label, kind, state, entry_detail)
        };
        let elapsed_ms = {
            let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner
                .entries
                .get(&id)
                .map(|e| e.born.elapsed().as_millis() as u64)
                .unwrap_or(0)
        };
        // Broadcast the real state and detail (used to be hardcoded Running/None, which
        // made the TUI display inconsistent with the state).
        let _ = self.tx.send(WatchEvent {
            id,
            label,
            kind,
            state,
            detail: entry_detail,
            payload: None,
            signal: Some(signal),
            elapsed_ms,
        });
    }

    /// Whether the polling loop should stop: already terminal, or the entry was reaped
    /// (polling after reaping would only spin idly).
    pub fn is_terminal(&self, id: WatchId) -> bool {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.entries.get(&id).is_none_or(|e| e.state.is_terminal())
    }

    /// The implementer updates state proactively (no interval, or immediate changes outside polling).
    /// Only broadcasts on state transitions: same state + same detail is idempotently skipped.
    pub fn set_state(
        &self,
        id: WatchId,
        state: WatchState,
        detail: Option<String>,
        payload: Option<serde_json::Value>,
    ) {
        let (label, kind) = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let Some(entry) = inner.entries.get_mut(&id) else {
                return;
            };
            // Terminal freeze: non-terminal updates after Done/Failed (e.g. a polling
            // Running) no longer overwrite.
            if entry.state.is_terminal() && !state.is_terminal() {
                return;
            }
            if entry.state == state && entry.detail == detail {
                return;
            }
            entry.state = state;
            entry.detail = detail.clone();
            entry.payload = payload.clone();
            let label = entry.label.clone();
            let kind = entry.kind;
            let notify = state.is_terminal() || state == WatchState::Idle;
            if state.is_terminal() {
                // After terminal, no more content feeding or condition matching: compact
                // the entry and free the buffers.
                entry.feed_buffer = Vec::new();
                entry.conditions = Vec::new();
            }
            if notify {
                inner.notifications.push_back(Notification {
                    id,
                    label: label.clone(),
                    state,
                    detail: detail.clone(),
                    payload: payload.clone(),
                    signal: None,
                });
            }
            if state.is_terminal() {
                prune_terminal_entries(&mut inner);
            }
            (label, kind)
        };
        let elapsed_ms = {
            let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner
                .entries
                .get(&id)
                .map(|e| e.born.elapsed().as_millis() as u64)
                .unwrap_or(0)
        };
        let _ = self.tx.send(WatchEvent {
            id,
            label,
            kind,
            state,
            detail,
            payload,
            signal: None,
            elapsed_ms,
        });
    }

    pub fn subscribe(&self) -> broadcast::Receiver<WatchEvent> {
        self.tx.subscribe()
    }

    /// Snapshot of all current watchables (TUI initial render / state queries).
    #[allow(dead_code)]
    pub fn snapshot(&self) -> Vec<WatchSnapshot> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .entries
            .iter()
            .map(|(id, e)| WatchSnapshot {
                id: *id,
                label: e.label.clone(),
                state: e.state,
                detail: e.detail.clone(),
                payload: e.payload.clone(),
            })
            .collect()
    }

    /// Whether there are unconsumed wake notifications (terminal or signal) — used to
    /// trigger an extra automatic turn after a turn ends.
    pub fn has_wake_notifications(&self) -> bool {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .notifications
            .iter()
            .any(|n| n.state.is_terminal() || n.signal.is_some())
    }

    /// Take out the notifications pending injection into the model (merges adjacent Idle
    /// entries with the same id into one round summary).
    pub fn consume_notifications(&self) -> Vec<String> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<String> = Vec::new();
        // (id, label, round count, latest detail)
        let mut pending: Option<(WatchId, String, u32, String)> = None;
        while let Some(n) = inner.notifications.pop_front() {
            if let Some(sig) = n.signal {
                if let Some((id, label, count, last)) = pending.take() {
                    out.push(format_notification(id, label, count, last));
                }
                out.push(format!("任务 {}（{}）：⚠ {sig}", n.id, n.label));
                continue;
            }
            let is_same_idle =
                n.state == WatchState::Idle && pending.as_ref().is_some_and(|(id, ..)| *id == n.id);
            if is_same_idle {
                if let Some((_, _, count, last)) = &mut pending {
                    *count += 1;
                    if let Some(d) = &n.detail {
                        *last = d.clone();
                    }
                }
            } else {
                if let Some((id, label, count, last)) = pending.take() {
                    out.push(format_notification(id, label, count, last));
                }
                pending = Some((n.id, n.label, 1, notification_body(&n.detail, &n.payload)));
            }
        }
        if let Some((id, label, count, last)) = pending.take() {
            out.push(format_notification(id, label, count, last));
        }
        out
    }
}

/// Terminal entry reaping: keep only the most recent MAX_TERMINAL_ENTRIES terminal entries
/// (entries don't grow monotonically in long sessions; notifications hold their own copies,
/// so reaping loses no content).
fn prune_terminal_entries(inner: &mut Inner) {
    let mut terminal: Vec<(WatchId, Instant)> = inner
        .entries
        .iter()
        .filter(|(_, e)| e.state.is_terminal())
        .map(|(id, e)| (*id, e.born))
        .collect();
    if terminal.len() <= MAX_TERMINAL_ENTRIES {
        return;
    }
    terminal.sort_by_key(|(_, born)| *born);
    let drop_count = terminal.len() - MAX_TERMINAL_ENTRIES;
    for (id, _) in terminal.into_iter().take(drop_count) {
        inner.entries.remove(&id);
    }
}

/// Notification body: detail + payload (truncated to prevent context bloat).
fn notification_body(detail: &Option<String>, payload: &Option<serde_json::Value>) -> String {
    const MAX_PAYLOAD_CHARS: usize = 4000;
    let mut body = detail.clone().unwrap_or_default();
    if let Some(text) = payload
        .as_ref()
        .and_then(|p| p.as_str().map(str::to_string))
    {
        if !body.is_empty() {
            body.push('\n');
        }
        if text.chars().count() > MAX_PAYLOAD_CHARS {
            body.push_str(&text.chars().take(MAX_PAYLOAD_CHARS).collect::<String>());
            body.push_str("…[truncated]");
        } else {
            body.push_str(&text);
        }
    }
    body
}

fn format_notification(id: WatchId, label: String, count: u32, last: String) -> String {
    if count > 1 {
        format!("任务 {id}（{label}）：已完成 {count} 轮（最近：{last}）")
    } else {
        format!("任务 {id}（{label}）：{last}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeWatch {
        label: &'static str,
        sequence: Vec<WatchPoll>,
        index: AtomicUsize,
        interval: Option<Duration>,
    }

    impl Watchable for FakeWatch {
        fn label(&self) -> String {
            self.label.to_string()
        }
        fn poll(&self) -> WatchPoll {
            let i = self
                .index
                .fetch_add(1, Ordering::SeqCst)
                .min(self.sequence.len() - 1);
            self.sequence[i].clone()
        }
        fn check_interval(&self) -> Option<Duration> {
            self.interval
        }
    }

    fn watch() -> Arc<WatchRegistry> {
        WatchRegistry::new()
    }

    #[tokio::test]
    async fn register_publishes_initial_state() {
        let reg = watch();
        let mut rx = reg.subscribe();
        let id = reg.register_with_conditions(
            Box::new(FakeWatch {
                label: "watch -n 2 ls",
                sequence: vec![WatchPoll {
                    state: WatchState::Running,
                    detail: None,
                    payload: None,
                    signal: None,
                }],
                index: AtomicUsize::new(0),
                interval: None,
            }),
            Vec::new(),
        );
        let ev = rx.recv().await.unwrap_or_else(|_| unreachable!());
        assert_eq!(ev.id, id);
        assert_eq!(ev.state, WatchState::Running);
        assert_eq!(ev.label, "watch -n 2 ls");
    }

    #[test]
    fn set_state_idempotent_and_notifies() {
        let reg = watch();
        let id = reg.register_with_conditions(
            Box::new(FakeWatch {
                label: "l",
                sequence: vec![WatchPoll {
                    state: WatchState::Running,
                    detail: None,
                    payload: None,
                    signal: None,
                }],
                index: AtomicUsize::new(0),
                interval: None,
            }),
            Vec::new(),
        );
        reg.set_state(id, WatchState::Running, None, None);
        assert_eq!(reg.snapshot()[0].state, WatchState::Running);
        reg.set_state(id, WatchState::Idle, Some("第 1 轮".into()), None);
        assert_eq!(reg.snapshot()[0].state, WatchState::Idle);
        reg.set_state(id, WatchState::Done, Some("ok".into()), None);
        let snaps = reg.snapshot();
        assert_eq!(snaps[0].state, WatchState::Done);
        assert_eq!(snaps[0].label, "l");
    }

    #[test]
    fn notifications_merge_consecutive_idle_rounds() {
        let reg = watch();
        let id = reg.register_with_conditions(
            Box::new(FakeWatch {
                label: "poll",
                sequence: vec![WatchPoll {
                    state: WatchState::Running,
                    detail: None,
                    payload: None,
                    signal: None,
                }],
                index: AtomicUsize::new(0),
                interval: None,
            }),
            Vec::new(),
        );
        reg.set_state(id, WatchState::Idle, Some("第 1 轮".into()), None);
        reg.set_state(id, WatchState::Idle, Some("第 2 轮".into()), None);
        reg.set_state(id, WatchState::Done, Some("fin".into()), None);
        let notes = reg.consume_notifications();
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes[0].contains("已完成 2 轮"), "merged: {}", notes[0]);
        assert!(notes[1].contains("fin"), "terminal: {}", notes[1]);
        assert!(reg.consume_notifications().is_empty(), "consumed");
    }

    #[test]
    fn terminal_state_is_frozen_against_poll_override() {
        // Regression: after the subagent finishes (Done), a polling Running must not overwrite.
        let reg = watch();
        let id = reg.register_with_conditions(
            Box::new(FakeWatch {
                label: "agent",
                sequence: vec![WatchPoll {
                    state: WatchState::Running,
                    detail: Some("已产出 33 字符".into()),
                    payload: None,
                    signal: None,
                }],
                index: AtomicUsize::new(0),
                interval: None,
            }),
            Vec::new(),
        );
        reg.set_state(id, WatchState::Done, Some("完成".into()), None);
        reg.set_state(id, WatchState::Running, Some("已产出 33 字符".into()), None);
        assert_eq!(reg.snapshot()[0].state, WatchState::Done, "frozen");
    }

    #[test]
    fn condition_engine_matches_all_condition_kinds() {
        // Contains: any word hit is aggregated into the output
        let mut cs = ConditionState::new(NotifyCondition::Contains(vec![
            "DEPLOYED".into(),
            "FAILED".into(),
        ]));
        assert!(
            cs.match_buffer(&["INFO x".into(), "build DEPLOYED ok".into()], 2)
                .is_some_and(|s| s.contains("DEPLOYED")),
            "contains fires on pattern"
        );
        assert!(cs.match_buffer(&["INFO x".into()], 3).is_none(), "no hit");
        // Errors: default error lines
        let mut cs = ConditionState::new(NotifyCondition::Errors);
        assert!(
            cs.match_buffer(&["ERROR: boom".into()], 1)
                .is_some_and(|s| s.contains("ERROR: boom")),
            "errors fires"
        );
        assert!(
            cs.match_buffer(&["INFO ok".into()], 2).is_none(),
            "clean line"
        );
        // Regex
        let mut cs = ConditionState::new(NotifyCondition::Regex(r"\d{4}-\d{2}".into()));
        assert!(
            cs.match_buffer(&["time 2026-08-05".into()], 1).is_some(),
            "regex fires"
        );
        // LinesOver: fires once on breakthrough
        let mut cs = ConditionState::new(NotifyCondition::LinesOver(3));
        assert!(cs.match_buffer(&[], 4).is_some(), "threshold crossed");
        assert!(cs.match_buffer(&[], 9).is_none(), "fires once");
    }

    #[test]
    fn feed_content_drives_condition_signals() {
        // End to end: feed_content → match_conditions (the polling-tick path).
        let reg = watch();
        let id = reg.register_with_conditions(
            Box::new(FakeWatch {
                label: "monitor",
                sequence: vec![WatchPoll {
                    state: WatchState::Running,
                    detail: None,
                    payload: None,
                    signal: None,
                }],
                index: AtomicUsize::new(0),
                interval: None,
            }),
            vec![NotifyCondition::Contains(vec!["boom".into()])],
        );
        reg.feed_content(id, "INFO line");
        assert!(reg.match_conditions(id).is_empty(), "no hit yet");
        reg.feed_content(id, "boom happened");
        let signals = reg.match_conditions(id);
        assert_eq!(signals.len(), 1, "{signals:?}");
        assert!(signals[0].contains("boom"), "{}", signals[0]);
        // Buffer cleared: another match yields no signal
        assert!(reg.match_conditions(id).is_empty(), "buffer drained");
    }

    fn running_watch(label: &'static str) -> Box<FakeWatch> {
        Box::new(FakeWatch {
            label,
            sequence: vec![WatchPoll {
                state: WatchState::Running,
                detail: None,
                payload: None,
                signal: None,
            }],
            index: AtomicUsize::new(0),
            interval: None,
        })
    }

    /// M3 regression: MBs of error logs in one tick must not be stuffed into a signal whole.
    #[test]
    fn signal_text_is_bounded() {
        let mut cs = ConditionState::new(NotifyCondition::Errors);
        let flood: Vec<String> = (0..5000)
            .map(|i| format!("ERROR line {i} {}", "x".repeat(200)))
            .collect();
        let signal = cs.match_buffer(&flood, flood.len()).unwrap_or_default();
        assert!(
            signal.chars().count() <= MAX_SIGNAL_CHARS + 32,
            "信号长度 {} 应受限",
            signal.chars().count()
        );
        assert!(
            signal.contains("共 5000 行命中"),
            "标注被截断的总数: {signal}"
        );
    }

    #[test]
    fn emit_signal_truncates_long_text() {
        let reg = watch();
        let id = reg.register_with_conditions(running_watch("noisy"), Vec::new());
        reg.emit_signal(id, "巨".repeat(10_000), None);
        let notes = reg.consume_notifications();
        assert!(notes.iter().any(|n| n.contains("[truncated]")), "{notes:?}");
        assert!(
            notes
                .iter()
                .all(|n| n.chars().count() < MAX_SIGNAL_CHARS + 200),
            "通知长度受限"
        );
    }

    /// S6 regression: an undrained feed buffer must not grow unboundedly.
    #[test]
    fn feed_buffer_is_bounded_without_drain() {
        let reg = watch();
        let id =
            reg.register_with_conditions(running_watch("flood"), vec![NotifyCondition::Errors]);
        for i in 0..(MAX_FEED_BUFFER_LINES * 3) {
            reg.feed_content(id, &format!("line {i}\n"));
        }
        let buffered = {
            let inner = reg.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner
                .entries
                .get(&id)
                .map(|e| e.feed_buffer.len())
                .unwrap_or(0)
        };
        assert!(
            buffered <= MAX_FEED_BUFFER_LINES,
            "缓冲 {buffered} 行应受限"
        );
        // The cumulative line count is still tracked faithfully (the LinesOver semantics).
        let total = {
            let inner = reg.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.entries.get(&id).map(|e| e.total_lines).unwrap_or(0)
        };
        assert_eq!(total, MAX_FEED_BUFFER_LINES * 3);
    }

    /// S6 regression: buffers are freed after terminal, and terminal entries don't accumulate unboundedly.
    #[test]
    fn terminal_entries_are_compacted_and_pruned() {
        let reg = watch();
        let id = reg.register_with_conditions(running_watch("done"), vec![NotifyCondition::Errors]);
        reg.feed_content(id, "some output\n");
        reg.set_state(id, WatchState::Done, Some("ok".into()), None);
        {
            let inner = reg.inner.lock().unwrap_or_else(|e| e.into_inner());
            let entry = inner.entries.get(&id).unwrap_or_else(|| unreachable!());
            assert!(entry.feed_buffer.is_empty(), "终态释放缓冲");
            assert!(entry.conditions.is_empty(), "终态释放条件");
        }
        for _ in 0..(MAX_TERMINAL_ENTRIES * 2) {
            let extra = reg.register_with_conditions(running_watch("x"), Vec::new());
            reg.set_state(extra, WatchState::Done, Some("ok".into()), None);
        }
        {
            let inner = reg.inner.lock().unwrap_or_else(|e| e.into_inner());
            assert!(
                inner.entries.len() <= MAX_TERMINAL_ENTRIES + 1,
                "终态条目应被回收，当前 {}",
                inner.entries.len()
            );
        }
        // A reaped entry must stop the polling loop — no idle spinning.
        assert!(reg.is_terminal(id), "已回收的条目视为终态");
        assert!(reg.is_terminal(WatchId(999_999)), "不存在的条目视为终态");
    }

    #[tokio::test]
    async fn signal_event_queues_and_broadcasts() {
        let reg = watch();
        let mut rx = reg.subscribe();
        let id = reg.register_with_conditions(
            Box::new(FakeWatch {
                label: "log-tail",
                sequence: vec![WatchPoll {
                    state: WatchState::Running,
                    detail: None,
                    payload: None,
                    signal: None,
                }],
                index: AtomicUsize::new(0),
                interval: None,
            }),
            Vec::new(),
        );
        let _ = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .unwrap_or_else(|_| unreachable!())
            .unwrap_or_else(|_| unreachable!());
        reg.emit_signal(
            id,
            "发现错误：boom".to_string(),
            Some("发现 1 个错误".into()),
        );
        let ev = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .unwrap_or_else(|_| unreachable!())
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(ev.signal.as_deref(), Some("发现错误：boom"));
        let notes = reg.consume_notifications();
        assert!(
            notes
                .iter()
                .any(|n| n.contains("发现错误") && n.contains("boom")),
            "{notes:?}"
        );
    }

    #[tokio::test]
    async fn interval_polling_publishes_transitions() {
        let reg = watch();
        let mut rx = reg.subscribe();
        let id = reg.register_with_conditions(
            Box::new(FakeWatch {
                label: "slow",
                sequence: vec![
                    WatchPoll {
                        state: WatchState::Running,
                        detail: None,
                        payload: None,
                        signal: None,
                    },
                    WatchPoll {
                        state: WatchState::Idle,
                        detail: Some("第 1 轮".into()),
                        payload: None,
                        signal: None,
                    },
                    WatchPoll {
                        state: WatchState::Idle,
                        detail: Some("第 2 轮".into()),
                        payload: None,
                        signal: None,
                    },
                    WatchPoll {
                        state: WatchState::Done,
                        detail: Some("fin".into()),
                        payload: None,
                        signal: None,
                    },
                    WatchPoll {
                        state: WatchState::Done,
                        detail: Some("fin".into()),
                        payload: None,
                        signal: None,
                    },
                ],
                index: AtomicUsize::new(0),
                interval: Some(Duration::from_millis(5)),
            }),
            Vec::new(),
        );
        // Initial event + polling events: at least Running → Idle → Done appears.
        let mut seen: Vec<WatchState> = Vec::new();
        for _ in 0..6 {
            let ev = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .unwrap_or_else(|_| unreachable!())
                .unwrap_or_else(|_| unreachable!());
            if !seen.contains(&ev.state) {
                seen.push(ev.state);
            }
            if ev.state == WatchState::Done {
                break;
            }
        }
        assert!(seen.contains(&WatchState::Done), "{seen:?}");
        assert_eq!(reg.snapshot()[0].state, WatchState::Done);
        let _ = id;
    }
}
