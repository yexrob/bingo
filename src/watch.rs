//! Watchable 通知机制：命令、agent 等一切可被 watch 的实体。
//!
//! 每个 watchable 有状态机（Running → Idle → Done/Failed/Cancelled），
//! 状态转换广播给 TUI，终态通知排队注入主 agent 上下文。
#![allow(dead_code)] // 阶段 2 接入 Bash/Agent 后移除

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
    Cancelled,
}

impl WatchState {
    /// 终态：轮询停止、快照定格。
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }
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
}

/// 可被 watch 的实体：声明自己的检查间隔与轮次语义。
pub trait Watchable: Send + Sync {
    fn label(&self) -> String;
    fn poll(&self) -> WatchPoll;
    /// 周期轮询间隔；None = 不轮询（由实现者主动 set_state）。
    fn check_interval(&self) -> Option<Duration>;
}

/// 广播给订阅者（TUI）的事件。
#[derive(Debug, Clone)]
pub struct WatchEvent {
    pub id: WatchId,
    pub label: String,
    pub state: WatchState,
    pub detail: Option<String>,
    pub payload: Option<serde_json::Value>,
    pub ts: Instant,
}

/// 快照：供 TUI 初始渲染 / 状态查询。
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
}

struct Entry {
    label: String,
    state: WatchState,
    detail: Option<String>,
    payload: Option<serde_json::Value>,
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

    /// 注册一个 watchable；带间隔的会 spawn 周期轮询任务。
    pub fn register(self: &Arc<Self>, watchable: Box<dyn Watchable>) -> WatchId {
        let poll = watchable.poll();
        let label = watchable.label();
        let id = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let id = WatchId(inner.next_id);
            inner.next_id += 1;
            inner.entries.insert(
                id,
                Entry {
                    label: label.clone(),
                    state: poll.state,
                    detail: poll.detail.clone(),
                    payload: poll.payload.clone(),
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
                });
        }
        let _ = self.tx.send(WatchEvent {
            id,
            label,
            state: poll.state,
            detail: poll.detail.clone(),
            payload: poll.payload.clone(),
            ts: Instant::now(),
        });
        let interval = watchable.check_interval();
        if let Some(interval) = interval {
            let registry = self.clone();
            let watchable = Arc::new(watchable);
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval(interval);
                loop {
                    ticker.tick().await;
                    let p = watchable.poll();
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

    /// 实现者主动更新状态（无 interval 或轮询之外的即时变化）。
    /// 状态转换才广播：同状态同 detail 幂等跳过。
    pub fn set_state(
        &self,
        id: WatchId,
        state: WatchState,
        detail: Option<String>,
        payload: Option<serde_json::Value>,
    ) {
        let label = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let Some(entry) = inner.entries.get_mut(&id) else {
                return;
            };
            if entry.state == state && entry.detail == detail {
                return;
            }
            entry.state = state;
            entry.detail = detail.clone();
            entry.payload = payload.clone();
            let label = entry.label.clone();
            let notify = state.is_terminal() || state == WatchState::Idle;
            if notify {
                inner.notifications.push_back(Notification {
                    id,
                    label: label.clone(),
                    state,
                    detail: detail.clone(),
                    payload: payload.clone(),
                });
            }
            label
        };
        let _ = self.tx.send(WatchEvent {
            id,
            label,
            state,
            detail,
            payload,
            ts: Instant::now(),
        });
    }

    pub fn subscribe(&self) -> broadcast::Receiver<WatchEvent> {
        self.tx.subscribe()
    }

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

    /// 取出待注入模型的通知（合并同 id 相邻 Idle 为一条轮次汇总）。
    pub fn consume_notifications(&self) -> Vec<String> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<String> = Vec::new();
        // (id, label, 轮次计数, 最近 detail)
        let mut pending: Option<(WatchId, String, u32, String)> = None;
        while let Some(n) = inner.notifications.pop_front() {
            let is_same_idle = n.state == WatchState::Idle
                && pending.as_ref().is_some_and(|(id, ..)| *id == n.id);
            if is_same_idle {
                if let Some((_, _, count, last)) = &mut pending {
                    *count += 1;
                    if let Some(d) = n.detail {
                        *last = d;
                    }
                }
            } else {
                if let Some((id, label, count, last)) = pending.take() {
                    out.push(format_notification(id, label, count, last));
                }
                pending = Some((n.id, n.label, 1, n.detail.unwrap_or_default()));
            }
        }
        if let Some((id, label, count, last)) = pending.take() {
            out.push(format_notification(id, label, count, last));
        }
        out
    }
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
        let id = reg.register(Box::new(FakeWatch {
            label: "watch -n 2 ls",
            sequence: vec![WatchPoll {
                state: WatchState::Running,
                detail: None,
                payload: None,
            }],
            index: AtomicUsize::new(0),
            interval: None,
        }));
        let ev = rx.recv().await.unwrap_or_else(|_| unreachable!());
        assert_eq!(ev.id, id);
        assert_eq!(ev.state, WatchState::Running);
        assert_eq!(ev.label, "watch -n 2 ls");
    }

    #[test]
    fn set_state_idempotent_and_notifies() {
        let reg = watch();
        let id = reg.register(Box::new(FakeWatch {
            label: "l",
            sequence: vec![WatchPoll {
                state: WatchState::Running,
                detail: None,
                payload: None,
            }],
            index: AtomicUsize::new(0),
            interval: None,
        }));
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
        let id = reg.register(Box::new(FakeWatch {
            label: "poll",
            sequence: vec![WatchPoll {
                state: WatchState::Running,
                detail: None,
                payload: None,
            }],
            index: AtomicUsize::new(0),
            interval: None,
        }));
        reg.set_state(id, WatchState::Idle, Some("第 1 轮".into()), None);
        reg.set_state(id, WatchState::Idle, Some("第 2 轮".into()), None);
        reg.set_state(id, WatchState::Done, Some("fin".into()), None);
        let notes = reg.consume_notifications();
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes[0].contains("已完成 2 轮"), "merged: {}", notes[0]);
        assert!(notes[1].contains("fin"), "terminal: {}", notes[1]);
        assert!(reg.consume_notifications().is_empty(), "consumed");
    }

    #[tokio::test]
    async fn interval_polling_publishes_transitions() {
        let reg = watch();
        let mut rx = reg.subscribe();
        let id = reg.register(Box::new(FakeWatch {
            label: "slow",
            sequence: vec![
                WatchPoll { state: WatchState::Running, detail: None, payload: None },
                WatchPoll { state: WatchState::Idle, detail: Some("第 1 轮".into()), payload: None },
                WatchPoll { state: WatchState::Idle, detail: Some("第 2 轮".into()), payload: None },
                WatchPoll { state: WatchState::Done, detail: Some("fin".into()), payload: None },
                WatchPoll { state: WatchState::Done, detail: Some("fin".into()), payload: None },
            ],
            index: AtomicUsize::new(0),
            interval: Some(Duration::from_millis(5)),
        }));
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
