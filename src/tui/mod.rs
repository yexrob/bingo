//! Terminal front end.
//!
//! - [`chat`] is the state machine and the document builder (`build_rows`).
//! - [`app`] is the event loop and the frame assembly.
//! - [`view`] converts document rows to ratatui text; [`term`] is the only
//!   module that writes to the terminal.
//!
//! The renderer-agnostic contract (`UiEvent`, the dialog types, `tui_hooks`)
//! lives in [`crate::ui`].

pub mod activities;
mod app;
pub mod chat;
mod entity;
pub mod gfx;
pub mod history;
pub mod input;
pub mod keys;
pub mod line;
pub mod markdown;
pub mod math;
pub(crate) mod term;
#[cfg(test)]
mod test_util;
pub mod theme;
mod view;

use std::io::stdout;
use std::sync::Arc;

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use crate::query::Session;
use crate::tui::chat::Chat;
use crate::tui::theme::{Theme, ThemeSetting};

/// 启动 TUI 会话。`fullscreen=false`（默认）：inline 模式——定稿内容
/// 打印进终端 scrollback、视口只画动态尾部；`fullscreen=true`：全屏
/// canvas（app 内滚动 + 鼠标交互）。
pub async fn run_tui_session(
    session: Arc<Session>,
    expand_rx: tokio::sync::watch::Receiver<bool>,
    fullscreen: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // raw mode 之前查一次终端背景色，供 auto 主题解析（探测自己会临时
    // 开关 raw mode 并直接读 /dev/tty）。
    let detected_background = Theme::detect_system_theme().await;

    // 全屏每帧 diff 重绘无法稳定承载 kitty 图片 → 只有 inline 模式启用
    // 真实图片显示（定稿行一次落盘进 scrollback）。同样必须在 raw mode
    // 之前完成：探测走的是同一条 /dev/tty 查询路径。
    let image_probe = if fullscreen {
        gfx::ImageProbe::default()
    } else {
        gfx::detect_image_cap().await
    };
    let image_cap = image_probe.cap;
    if std::env::var_os("BINGO_DEBUG").is_some() {
        eprintln!(
            "[bingo] image_cap={image_cap:?} TERM={:?} TERM_PROGRAM={:?}",
            std::env::var("TERM").ok(),
            std::env::var("TERM_PROGRAM").ok(),
        );
    }

    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let (asks_tx, asks_rx) = mpsc::unbounded_channel();
    let mut chat = Chat::new(
        session.clone(),
        events_tx,
        events_rx,
        asks_tx,
        asks_rx,
        Theme::for_terminal(
            ThemeSetting::parse(session.settings.theme.as_deref()),
            detected_background,
        ),
        detected_background,
    );
    chat.image_cap = image_cap;
    // 探测到 kitty 终端但 tmux passthrough 没开等情况：告诉用户怎么开。
    if let Some(warning) = image_probe.warning {
        chat.push_warning(warning);
    }

    enable_raw_mode()?;
    let mut out = stdout();
    let setup = if fullscreen {
        execute!(
            out,
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste
        )
    } else {
        execute!(out, EnableBracketedPaste)
    };
    // 进入失败也要把 raw mode 收回去（终端否则留在半配置状态）。
    if let Err(e) = setup {
        let _ = disable_raw_mode();
        return Err(e.into());
    }

    // 宿主构造失败也要走完拆除（下面的反序拆除对两条路径都生效）。
    let result: Result<(), Box<dyn std::error::Error>> = if fullscreen {
        match Terminal::new(CrosstermBackend::new(stdout())) {
            Ok(terminal) => app::run_fullscreen(chat, expand_rx, terminal).await,
            Err(e) => Err(e.into()),
        }
    } else {
        // 光标此刻停在 shell 提示行：驱动以它为视口原点。
        match term::InlineTerm::stdout() {
            Ok(host) => app::run_inline(chat, expand_rx, host).await,
            Err(e) => Err(e.into()),
        }
    };

    // 反序拆除，每一步都尽力而为：中间某步失败不能把终端留在 raw mode。
    let mut out = stdout();
    if fullscreen {
        let _ = execute!(
            out,
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
    } else {
        let _ = execute!(out, DisableBracketedPaste);
    }
    let _ = disable_raw_mode();
    let _ = execute!(out, crossterm::cursor::Show);
    result
}
