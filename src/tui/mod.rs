//! 交互层：iocraft 全屏 TUI。
//!
//! - [`tui_hooks`]：把 query 的 [`UiHooks`] 接到事件通道上（原样保留）。
//! - [`run_tui_session`]：全屏 render loop 宿主。
//! - 状态机在 [`chat`]，渲染层在 [`components`]。

pub mod activities;
pub mod chat;
pub mod components;
pub mod gfx;
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
    /// standalone=true：`!` 命令等非模型工具——只设摘要，不参与折叠组。
    ToolReady {
        name: String,
        input: serde_json::Value,
        standalone: bool,
    },
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
    /// `/model` 二级选择器：某 provider 的模型列表异步拉取完成（补充进菜单）。
    ModelsLoaded {
        provider: String,
        models: Vec<String>,
    },
    /// 消息定稿后图片异步加载完成（meta=None = 加载失败，显示占位）。
    ImageReady {
        url: String,
        meta: Option<crate::tui::gfx::ImageMeta>,
    },
    TurnEnd,
    /// 非致命警告（如 MCP 连接失败），显示在输入框上方。
    Warning(String),
    /// slash 命令异步结果（/compact /status /context）：渲染在消息之后。
    SlashOutput(String),
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
    let ask_asks = asks.clone();
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
        on_tool_ready: Box::new(move |name, input, standalone| {
            let _ = ready_events.send(UiEvent::ToolReady {
                name,
                input,
                standalone,
            });
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
        ask: Arc::new(move |tool_name, reason| {
            let request = PermissionRequest::new(
                format!("允许执行 {tool_name}"),
                reason,
                vec!["允许".to_string(), "拒绝".to_string()],
            );
            let (tx, rx) = oneshot::channel();
            if ask_asks.send((request, tx)).is_err() {
                return Box::pin(async { false });
            }
            Box::pin(async move {
                matches!(rx.await, Ok(DialogAction::Confirm(0)))
            })
        }),
        ask_question: Arc::new(move |title, question, options| {
            let mut request = PermissionRequest::new(title, question, Vec::new());
            request.free_text = true;
            request.options = options.iter().map(|(l, _d)| l.clone()).collect();
            request.descriptions = options.into_iter().map(|(_l, d)| d).collect();
            let (tx, rx) = oneshot::channel();
            if asks.send((request, tx)).is_err() {
                return Box::pin(async { None });
            }
            Box::pin(async move {
                match rx.await {
                    Ok(DialogAction::Confirm(index)) => {
                        Some(crate::query::AskAnswer::Option(index))
                    }
                    Ok(DialogAction::Answer(text)) => {
                        Some(crate::query::AskAnswer::Other(text))
                    }
                    Ok(DialogAction::Cancel) | Err(_) => None,
                }
            })
        }),
    }
}

/// 启动 TUI 会话。`fullscreen=false`（默认）：inline 模式——定稿内容
/// 打印进终端 scrollback、canvas 只画动态尾部（非全屏）；
/// `fullscreen=true`：iocraft 全屏 canvas（app 内滚动 + 鼠标交互）。
/// iocraft 的 render loop 自带 raw mode / 事件流，退出或 panic 时恢复终端。
pub async fn run_tui_session(
    session: Arc<Session>,
    expand_rx: tokio::sync::watch::Receiver<bool>,
    fullscreen: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use iocraft::prelude::*;

    // fullscreen（raw mode）之前查一次终端背景色，供 auto 主题解析。
    let detected_background = theme::Theme::detect_system_theme().await;

    // 全屏 canvas 每帧 diff 重绘无法稳定承载 kitty 图片 → 只有 inline
    // 模式启用真实图片显示（定稿行一次落盘进 scrollback）。
    let image_cap = if fullscreen {
        None
    } else {
        gfx::detect_image_cap().await
    };
    if std::env::var_os("BINGO_DEBUG").is_some() {
        eprintln!(
            "[bingo] image_cap={image_cap:?} TERM={:?} TERM_PROGRAM={:?}",
            std::env::var("TERM").ok(),
            std::env::var("TERM_PROGRAM").ok(),
        );
    }

    let mut root = element!(components::Bingo(
        session: Some(session),
        expand_rx: Some(expand_rx),
        detected_background: detected_background,
        inline: Some(!fullscreen),
        image_cap: image_cap,
    ));
    if fullscreen {
        root.fullscreen().await?;
    } else {
        // Ctrl+C 交给组件处理：busy 时取消请求，空闲时退出会话。
        root.render_loop().ignore_ctrl_c().await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AskUserQuestion 的 TUI 挂钩：请求走权限模态（标题/问题/选项），
    /// 确认 → Some(索引)，Esc 取消 → None。
    #[tokio::test]
    async fn ask_question_hook_maps_confirm_and_cancel() {
        let (events_tx, _events_rx) = mpsc::unbounded_channel();
        let (asks_tx, mut asks_rx) = mpsc::unbounded_channel();
        let ui = tui_hooks(events_tx, asks_tx);

        let fut = (ui.ask_question)(
            "技术选型".to_string(),
            "用哪个库？".to_string(),
            vec![
                ("A".to_string(), None),
                ("B".to_string(), Some("更快".to_string())),
            ],
        );
        let (request, tx) = asks_rx.try_recv().expect("模态请求已发出");
        assert_eq!(request.title, "技术选型");
        assert_eq!(request.question, "用哪个库？");
        assert_eq!(request.options, vec!["A", "B"]);
        assert!(request.free_text, "AskUserQuestion 请求带 Other 自由输入");
        tx.send(DialogAction::Confirm(1)).unwrap();
        assert_eq!(
            fut.await,
            Some(crate::query::AskAnswer::Option(1)),
            "按 2 选中 B"
        );

        let fut = (ui.ask_question)(
            "t".to_string(),
            "q?".to_string(),
            vec![("a".to_string(), None)],
        );
        let (_request, tx) = asks_rx.try_recv().expect("第二个模态请求");
        tx.send(DialogAction::Cancel).unwrap();
        assert_eq!(fut.await, None, "Esc 取消 → 未回答");

        let fut = (ui.ask_question)(
            "t".to_string(),
            "q?".to_string(),
            vec![("a".to_string(), None)],
        );
        let (_request, tx) = asks_rx.try_recv().expect("第三个模态请求");
        tx.send(DialogAction::Answer("自定义".into())).unwrap();
        assert_eq!(
            fut.await,
            Some(crate::query::AskAnswer::Other("自定义".to_string())),
            "Other 自由输入回填文本"
        );
    }
}
