//! Watchable 通知机制：命令、agent 等一切可被 watch 的实体。
//!
//! 每个 watchable 有状态机（Running → Idle → Done/Failed/Cancelled），
//! 状态转换广播给 TUI，终态通知排队注入主 agent 上下文。

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::broadcast;

/// Watchable 生命周期状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchState {
    Running,
    /// 一轮完成（周期命令的轮次边界）；终态前可反复出现。
    Idle,
    Done,
    Failed,
    /// 取消终态（kill 入口落地后使用）。
    #[allow(dead_code)]
    Cancelled,
}

impl WatchState {
    /// 终态：轮询停止、快照定格。
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }
}

/// Watchable 类别：展示层据此选图标（⏺ 命令 / ◉ 子代理 / ◇ 频道），
/// 不影响状态机与通知语义。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WatchKind {
    #[default]
    Command,
    Agent,
    Channel,
}

/// 会话内唯一的 watchable 标识。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WatchId(pub u64);

impl std::fmt::Display for WatchId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// poll 返回的状态快照。
#[derive(Debug, Clone)]
pub struct WatchPoll {
    pub state: WatchState,
    /// 人类可读的当前活动（如"第 3 轮 · 输出 12 行"）。
    pub detail: Option<String>,
    /// 结构化数据（如完成的 final message）。
    pub payload: Option<serde_json::Value>,
    /// 条件信号：检测到满足的条件（如 log 出现错误）时携带文本，无条件通知。
    pub signal: Option<String>,
}

/// 可被 watch 的实体：声明自己的检查间隔与轮次语义。
pub trait Watchable: Send + Sync {
    fn label(&self) -> String;
    fn poll(&self) -> WatchPoll;
    /// 周期轮询间隔；None = 不轮询（由实现者主动 set_state）。
    fn check_interval(&self) -> Option<Duration>;
    /// 类别（展示层图标）；缺省命令。
    fn kind(&self) -> WatchKind {
        WatchKind::Command
    }
}

/// 通知条件：对 watchable 喂入的内容增量（feed_content）做匹配，
/// 命中即在轮询 tick 时触发信号（无条件通知）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotifyCondition {
    /// 内容出现任一字样（子串匹配）。
    Contains(Vec<String>),
    /// 内容出现正则匹配。
    Regex(String),
    /// 累计行数超过阈值（突破时通知一次）。
    #[allow(dead_code)] // 行数条件入口（如 notify_lines）落地后使用
    LinesOver(usize),
    /// 默认错误行模式（常见错误关键字）。
    Errors,
}

/// 单条信号文本上限：`tail -f` 一个 tick 可产出 MB 级错误日志，
/// 不设限会直灌模型上下文。
const MAX_SIGNAL_CHARS: usize = 500;
/// 一轮内参与条件匹配的最大命中行数（超出只报计数）。
const MAX_MATCH_HITS: usize = 20;
/// feed 缓冲上限：无人 drain（如轮询未启动）时丢弃最旧行，不无界增长。
const MAX_FEED_BUFFER_LINES: usize = 1000;
/// 保留的终态条目上限：超出后清理最旧的终态条目。
const MAX_TERMINAL_ENTRIES: usize = 64;

/// 按字符截断（多字节安全）并标注。
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max).collect();
    format!("{head}…[truncated]")
}

/// 命中行聚合：限行数 + 限长度，防单条信号灌爆上下文。
/// 预留额度给条件前缀与命中计数，使聚合结果整体仍在 MAX_SIGNAL_CHARS 内。
fn join_hits(hits: &[&str]) -> String {
    const HITS_BUDGET: usize = MAX_SIGNAL_CHARS - 120;
    let shown: Vec<&str> = hits.iter().take(MAX_MATCH_HITS).copied().collect();
    let mut text = truncate_chars(&shown.join(" | "), HITS_BUDGET);
    if hits.len() > shown.len() {
        text.push_str(&format!(" …（共 {} 行命中）", hits.len()));
    }
    text
}

/// 条件的轮询状态。
struct ConditionState {
    cond: NotifyCondition,
    /// LinesOver 突破后不再重复通知。
    fired: bool,
}

impl ConditionState {
    fn new(cond: NotifyCondition) -> Self {
        Self { cond, fired: false }
    }
    /// 对一轮内容缓冲匹配，返回信号文本（增量条件逐条匹配、聚合输出）。
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

/// 常见错误行模式（默认 Errors 条件的信号源）。
fn is_error_line(line: &str) -> bool {
    const PATTERNS: &[&str] = &[
        "error", "ERROR", "Error", "Traceback", "panic", "PANIC", "FATAL", "fatal",
        "FAILED", "Exception", "❌",
    ];
    PATTERNS.iter().any(|p| line.contains(p))
}

/// 广播给订阅者（TUI）的事件。
#[derive(Debug, Clone)]
pub struct WatchEvent {
    /// 事件来源标识（TUI 按 label 定位，id 供程序化订阅方使用）。
    #[allow(dead_code)]
    pub id: WatchId,
    pub label: String,
    pub kind: WatchKind,
    pub state: WatchState,
    pub detail: Option<String>,
    pub payload: Option<serde_json::Value>,
    /// 条件信号文本（检测到满足条件时携带）。
    pub signal: Option<String>,
    /// watchable 已运行毫秒（注册时刻起算）。
    pub elapsed_ms: u64,
}

/// 快照：供 TUI 初始渲染 / 状态查询（当前展示层按 label 驱动，API 预留）。
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct WatchSnapshot {
    pub id: WatchId,
    pub label: String,
    pub state: WatchState,
    pub detail: Option<String>,
    pub payload: Option<serde_json::Value>,
}

/// 待注入模型的终态通知（consume 时合并同 id 相邻 Idle）。
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
    /// 本轮（两次 tick 之间）喂入的内容。
    feed_buffer: Vec<String>,
    /// 累计喂入行数（LinesOver 条件用）。
    total_lines: usize,
}

struct Inner {
    next_id: u64,
    entries: HashMap<WatchId, Entry>,
    notifications: VecDeque<Notification>,
}

/// 会话级 watch 注册中心（Session 持有 Arc）。
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

    /// 注册并配置通知条件（内容增量 feed_content 按条件匹配 → 信号）。
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
                    conditions: conditions
                        .into_iter()
                        .map(ConditionState::new)
                        .collect(),
                    feed_buffer: Vec::new(),
                    total_lines: 0,
                },
            );
            id
        };
        // 初始状态强制广播（set_state 幂等会吞掉与 entry 相同的状态）。
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
                    // 外部已置终态（如子 agent 完成）：停止轮询，不再覆盖。
                    if registry.is_terminal(id) {
                        break;
                    }
                    let p = watchable.poll();
                    if let Some(sig) = p.signal.clone() {
                        registry.emit_signal(id, sig, p.detail.clone());
                    }
                    // 条件匹配：本轮喂入内容按通知条件命中 → 信号。
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

    /// 实现者喂入内容增量（一行或一个文本块）：条件引擎按通知条件匹配，
    /// 命中由轮询 tick 聚合触发信号。
    pub fn feed_content(&self, id: WatchId, text: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(entry) = inner.entries.get_mut(&id) else {
            return;
        };
        entry.total_lines += text.lines().count();
        entry.feed_buffer.push(text.to_string());
        // 无人 drain 时丢弃最旧行：缓冲不无界增长。
        if entry.feed_buffer.len() > MAX_FEED_BUFFER_LINES {
            let overflow = entry.feed_buffer.len() - MAX_FEED_BUFFER_LINES;
            entry.feed_buffer.drain(..overflow);
        }
    }

    /// 匹配本轮喂入内容与通知条件，返回命中信号（增量条件逐条聚合）。
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

    /// 条件信号：实现者检测到满足的条件（如 log 出现错误）时调用，
    /// 无条件入通知队列 + 广播（不改变状态）。
    pub fn emit_signal(&self, id: WatchId, signal: String, detail: Option<String>) {
        // 单条信号上限：任何调用方喂来的长文本都在此收口。
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
        // 广播真实状态与 detail（曾固定 Running/None，TUI 显示与状态不一致）。
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

    /// 轮询循环是否该停：已入终态，或条目已被回收（回收后再轮询只会空转）。
    pub fn is_terminal(&self, id: WatchId) -> bool {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .entries
            .get(&id)
            .is_none_or(|e| e.state.is_terminal())
    }

    /// 实现者主动更新状态（无 interval 或轮询之外的即时变化）。
    /// 状态转换才广播：同状态同 detail 幂等跳过。
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
            // 终态定格：Done/Failed 后的非终态更新（如轮询的 Running）不再覆盖。
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
                // 终态后不再喂内容也不再匹配条件：压缩条目，释放缓冲。
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

    /// 当前全部 watchable 快照（TUI 初始渲染 / 状态查询）。
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

    /// 是否有未消费的唤醒通知（终态或信号）——回合结束后据此补触发自动回合。
    pub fn has_wake_notifications(&self) -> bool {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.notifications.iter().any(|n| {
            n.state.is_terminal() || n.signal.is_some()
        })
    }

    /// 取出待注入模型的通知（合并同 id 相邻 Idle 为一条轮次汇总）。
    pub fn consume_notifications(&self) -> Vec<String> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<String> = Vec::new();
        // (id, label, 轮次计数, 最近 detail)
        let mut pending: Option<(WatchId, String, u32, String)> = None;
        while let Some(n) = inner.notifications.pop_front() {
            if let Some(sig) = n.signal {
                if let Some((id, label, count, last)) = pending.take() {
                    out.push(format_notification(id, label, count, last));
                }
                out.push(format!(
                    "任务 {}（{}）：⚠ {sig}",
                    n.id, n.label
                ));
                continue;
            }
            let is_same_idle = n.state == WatchState::Idle
                && pending.as_ref().is_some_and(|(id, ..)| *id == n.id);
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

/// 终态条目回收：只保留最近 MAX_TERMINAL_ENTRIES 个终态条目
/// （会话长跑时 entries 不再单调增长；通知已各自持有副本，回收不丢内容）。
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

/// 通知正文：detail + payload（截断防上下文膨胀）。
fn notification_body(detail: &Option<String>, payload: &Option<serde_json::Value>) -> String {
    const MAX_PAYLOAD_CHARS: usize = 4000;
    let mut body = detail.clone().unwrap_or_default();
    if let Some(text) = payload.as_ref().and_then(|p| p.as_str().map(str::to_string)) {
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

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
            let i = self.index.fetch_add(1, Ordering::SeqCst).min(self.sequence.len() - 1);
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
        let id = reg.register_with_conditions(Box::new(FakeWatch {
            label: "watch -n 2 ls",
            sequence: vec![WatchPoll {
                state: WatchState::Running,
                detail: None,
                payload: None,
                signal: None,
            }],
            index: AtomicUsize::new(0),
            interval: None,
        }), Vec::new());
        let ev = rx.recv().await.unwrap_or_else(|_| unreachable!());
        assert_eq!(ev.id, id);
        assert_eq!(ev.state, WatchState::Running);
        assert_eq!(ev.label, "watch -n 2 ls");
    }

    #[test]
    fn set_state_idempotent_and_notifies() {
        let reg = watch();
        let id = reg.register_with_conditions(Box::new(FakeWatch {
            label: "l",
            sequence: vec![WatchPoll {
                state: WatchState::Running,
                detail: None,
                payload: None,
                signal: None,
            }],
            index: AtomicUsize::new(0),
            interval: None,
        }), Vec::new());
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
        let id = reg.register_with_conditions(Box::new(FakeWatch {
            label: "poll",
            sequence: vec![WatchPoll {
                state: WatchState::Running,
                detail: None,
                payload: None,
                signal: None,
            }],
            index: AtomicUsize::new(0),
            interval: None,
        }), Vec::new());
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
        // 回归：子 agent 完成（Done）后，interval 轮询的 Running 不得覆盖。
        let reg = watch();
        let id = reg.register_with_conditions(Box::new(FakeWatch {
            label: "agent",
            sequence: vec![WatchPoll {
                state: WatchState::Running,
                detail: Some("已产出 33 字符".into()),
                payload: None,
                signal: None,
            }],
            index: AtomicUsize::new(0),
            interval: None,
        }), Vec::new());
        reg.set_state(id, WatchState::Done, Some("完成".into()), None);
        reg.set_state(id, WatchState::Running, Some("已产出 33 字符".into()), None);
        assert_eq!(reg.snapshot()[0].state, WatchState::Done, "frozen");
    }

    #[test]
    fn condition_engine_matches_all_condition_kinds() {
        // Contains：任一字样命中聚合输出
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
        // Errors：默认错误行
        let mut cs = ConditionState::new(NotifyCondition::Errors);
        assert!(
            cs.match_buffer(&["ERROR: boom".into()], 1)
                .is_some_and(|s| s.contains("ERROR: boom")),
            "errors fires"
        );
        assert!(cs.match_buffer(&["INFO ok".into()], 2).is_none(), "clean line");
        // Regex
        let mut cs = ConditionState::new(NotifyCondition::Regex(r"\d{4}-\d{2}".into()));
        assert!(cs.match_buffer(&["time 2026-08-05".into()], 1).is_some(), "regex fires");
        // LinesOver：突破时一次
        let mut cs = ConditionState::new(NotifyCondition::LinesOver(3));
        assert!(cs.match_buffer(&[], 4).is_some(), "threshold crossed");
        assert!(cs.match_buffer(&[], 9).is_none(), "fires once");
    }

    #[test]
    fn feed_content_drives_condition_signals() {
        // 端到端：feed_content → match_conditions（轮询 tick 的路径）。
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
        // 缓冲已清：再次 match 无信号
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

    /// M3 回归：一个 tick 里 MB 级错误日志不得整串灌进信号。
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
        assert!(signal.contains("共 5000 行命中"), "标注被截断的总数: {signal}");
    }

    #[test]
    fn emit_signal_truncates_long_text() {
        let reg = watch();
        let id = reg.register_with_conditions(running_watch("noisy"), Vec::new());
        reg.emit_signal(id, "巨".repeat(10_000), None);
        let notes = reg.consume_notifications();
        assert!(notes.iter().any(|n| n.contains("[truncated]")), "{notes:?}");
        assert!(
            notes.iter().all(|n| n.chars().count() < MAX_SIGNAL_CHARS + 200),
            "通知长度受限"
        );
    }

    /// S6 回归：无人 drain 的 feed 缓冲不得无界增长。
    #[test]
    fn feed_buffer_is_bounded_without_drain() {
        let reg = watch();
        let id = reg.register_with_conditions(running_watch("flood"), vec![NotifyCondition::Errors]);
        for i in 0..(MAX_FEED_BUFFER_LINES * 3) {
            reg.feed_content(id, &format!("line {i}\n"));
        }
        let buffered = {
            let inner = reg.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.entries.get(&id).map(|e| e.feed_buffer.len()).unwrap_or(0)
        };
        assert!(buffered <= MAX_FEED_BUFFER_LINES, "缓冲 {buffered} 行应受限");
        // 累计行数仍如实统计（LinesOver 条件的语义）。
        let total = {
            let inner = reg.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.entries.get(&id).map(|e| e.total_lines).unwrap_or(0)
        };
        assert_eq!(total, MAX_FEED_BUFFER_LINES * 3);
    }

    /// S6 回归：终态后释放缓冲，且终态条目不无限累积。
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
        // 被回收的条目必须让轮询循环停下，不能空转。
        assert!(reg.is_terminal(id), "已回收的条目视为终态");
        assert!(reg.is_terminal(WatchId(999_999)), "不存在的条目视为终态");
    }

    #[tokio::test]
    async fn signal_event_queues_and_broadcasts() {
        let reg = watch();
        let mut rx = reg.subscribe();
        let id = reg.register_with_conditions(Box::new(FakeWatch {
            label: "log-tail",
            sequence: vec![WatchPoll {
                state: WatchState::Running,
                detail: None,
                payload: None,
                signal: None,
            }],
            index: AtomicUsize::new(0),
            interval: None,
        }), Vec::new());
        let _ = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .unwrap_or_else(|_| unreachable!())
            .unwrap_or_else(|_| unreachable!());
        reg.emit_signal(id, "发现错误：boom".to_string(), Some("发现 1 个错误".into()));
        let ev = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .unwrap_or_else(|_| unreachable!())
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(ev.signal.as_deref(), Some("发现错误：boom"));
        let notes = reg.consume_notifications();
        assert!(
            notes.iter().any(|n| n.contains("发现错误") && n.contains("boom")),
            "{notes:?}"
        );
    }

    #[tokio::test]
    async fn interval_polling_publishes_transitions() {
        let reg = watch();
        let mut rx = reg.subscribe();
        let id = reg.register_with_conditions(Box::new(FakeWatch {
            label: "slow",
            sequence: vec![
                WatchPoll { state: WatchState::Running, detail: None, payload: None, signal: None },
                WatchPoll { state: WatchState::Idle, detail: Some("第 1 轮".into()), payload: None, signal: None },
                WatchPoll { state: WatchState::Idle, detail: Some("第 2 轮".into()), payload: None, signal: None },
                WatchPoll { state: WatchState::Done, detail: Some("fin".into()), payload: None, signal: None },
                WatchPoll { state: WatchState::Done, detail: Some("fin".into()), payload: None, signal: None },
            ],
            index: AtomicUsize::new(0),
            interval: Some(Duration::from_millis(5)),
        }), Vec::new());
        // 初始事件 + 轮询事件：至少出现 Running → Idle → Done。
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
