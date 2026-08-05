//! 交互层：iocraft 全屏 TUI（布局对标 Claude Code 2.1.88）。
//!
//! - [`tui_hooks`]：把 query 的 [`UiHooks`] 接到事件通道上（原样保留）。
//! - [`run_tui_session`]：全屏 render loop 宿主。
//! - 状态机在 [`chat`]，渲染层在 [`components`]。

pub mod activities;
pub mod chat;
pub mod components;
pub mod line;
pub mod markdown;
pub mod math;
pub mod theme;

use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};

use crate::api::types::StreamEvent;
use crate::query::{Session, ToolCallDone, UiHooks};
use crate::tui::activities::WatchStatus;
use crate::tui::chat::{AskRequest, DialogAction, PermissionRequest};

/// agent task → 组件的事件通道。
#[derive(Debug, Clone)]
pub enum UiEvent {
    TurnStart,
    /// 一批 toolcall 全部执行完（query loop 一轮收口）。
    RoundEnd,
    TextDelta(String),
    ThinkingDelta(String),
    /// message_delta 的输出 token 累计值。
    OutputTokens(u64),
    ToolStart { name: String },
    /// 工具 block 流式接收完整（含 input）：折叠判定时机。
    ToolReady { name: String, input: serde_json::Value },
    ToolDone(ToolCallDone),
    /// Watchable 状态事件（命令/agent 生命周期，转发自 registry）。
    WatchEvent {
        label: String,
        status: WatchStatus,
        detail: Option<String>,
        duration_ms: u64,
        payload: Option<serde_json::Value>,
        signal: Option<String>,
    },
    TurnEnd,
    /// 非致命警告（如 MCP 连接失败），显示在输入框上方。
    Warning(String),
    Error(String),
}

/// 把 query 的 UiHooks 接到 TUI 通道上。
pub fn tui_hooks(
    events: mpsc::UnboundedSender<UiEvent>,
    asks: mpsc::UnboundedSender<AskRequest>,
) -> UiHooks {
    let tool_events = events.clone();
    let ready_events = events.clone();
    let round_events = events.clone();
    let warn_events = events.clone();
    UiHooks {
        on_event: Box::new(move |event| match event {
            StreamEvent::TextDelta { text, .. } => {
                let _ = events.send(UiEvent::TextDelta(text.clone()));
            }
            StreamEvent::ThinkingDelta { thinking, .. } => {
                let _ = events.send(UiEvent::ThinkingDelta(thinking.clone()));
            }
            StreamEvent::ToolUseStart { name, .. } => {
                let _ = events.send(UiEvent::ToolStart { name: name.clone() });
            }
            StreamEvent::StopReason { output_tokens: Some(tokens), .. } => {
                let _ = events.send(UiEvent::OutputTokens(*tokens));
            }
            _ => {}
        }),
        on_tool_ready: Box::new(move |name, input| {
            let _ = ready_events.send(UiEvent::ToolReady { name, input });
        }),
        on_tool_done: Box::new(move |done| {
            let _ = tool_events.send(UiEvent::ToolDone(crate::query::ToolCallDone {
                name: done.name.clone(),
                summary: done.summary.clone(),
                output: done.output.clone(),
                is_error: done.is_error,
                diff: done.diff.clone(),
                duration_ms: done.duration_ms,
            }));
        }),
        on_round_end: Box::new(move || {
            let _ = round_events.send(UiEvent::RoundEnd);
        }),
        on_warning: Box::new(move |message| {
            let _ = warn_events.send(UiEvent::Warning(message));
        }),
        ask: Box::new(move |tool_name, reason| {
            let request = PermissionRequest::new(
                format!("允许执行 {tool_name}"),
                reason,
                vec!["允许".to_string(), "拒绝".to_string()],
            );
            let (tx, rx) = oneshot::channel();
            if asks.send((request, tx)).is_err() {
                return Box::pin(async { false });
            }
            Box::pin(async move {
                matches!(rx.await, Ok(DialogAction::Confirm(0)))
            })
        }),
    }
}

/// 启动全屏 TUI 会话。iocraft 的 render loop 自带 alternate screen /
/// raw mode / mouse capture，退出或 panic 时恢复终端。
pub async fn run_tui_session(
    session: Arc<Session>,
    expand_rx: tokio::sync::watch::Receiver<bool>,
) -> Result<(), Box<dyn std::error::Error>> {
    use iocraft::prelude::*;

    // fullscreen（raw mode）之前查一次终端背景色，供 auto 主题解析。
    let detected_background = theme::Theme::detect_system_theme().await;

    let mut root = element!(components::Bingo(
        session: Some(session),
        expand_rx: Some(expand_rx),
        detected_background: detected_background,
    ));
    root.fullscreen().await?;
    Ok(())
}
