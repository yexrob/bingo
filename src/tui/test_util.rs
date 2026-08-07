//! TUI 测试共用基建：可断言渲染输出的测试 backend。
//!
//! [`Recorder`] 封装 ratatui `TestBackend`，记录 draw/clear/scroll/raw 事件，
//! 提供 `screen()`/`scrollback()` 行级文本断言与 `buffer()` 样式访问。
//!
//! 服务对象：TUI 各模块测试 + 呈现层回归（AC 表 B/D 区滚动可见/重试可达、
//! A 区错误行高亮样式断言）。与「状态注入」（UiEvent 注入 / 测试钩子）正交——
//! 本模块只负责「渲染后可断言什么」，不负责「状态怎么来」。

use std::convert::Infallible;
use std::ops::Range;

use ratatui::backend::{Backend, ClearType, TestBackend, WindowSize};
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::{Position, Size};
use ratatui::style::Color;

use crate::tui::term::RawWrite;

/// TestBackend plus the raw-byte sink and command counters the driver needs asserting on.
///
/// 字段全部 `pub`（测试 helper 惯例）：TUI 各模块测试直接读计数器/原始字节
/// 断言，`screen()`/`scrollback()` 提供行级文本快照，`buffer()` 提供样式断言。
pub struct Recorder {
    pub inner: TestBackend,
    pub raw: Vec<u8>,
    pub draw_calls: usize,
    pub clear_calls: usize,
    pub scrolled_up: Vec<(Range<u16>, u16)>,
    pub scrolled_down: Vec<(Range<u16>, u16)>,
    pub appended: u16,
}

impl Recorder {
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            inner: TestBackend::new(width, height),
            raw: Vec::new(),
            draw_calls: 0,
            clear_calls: 0,
            scrolled_up: Vec::new(),
            scrolled_down: Vec::new(),
            appended: 0,
        }
    }

    pub fn reset_counters(&mut self) {
        self.raw.clear();
        self.draw_calls = 0;
        self.clear_calls = 0;
        self.scrolled_up.clear();
        self.scrolled_down.clear();
        self.appended = 0;
    }

    /// 行级文本快照（去尾部空白）：断言「屏幕这一行显示了什么」。
    pub fn screen(&self) -> Vec<String> {
        rows_of(self.inner.buffer())
    }

    /// 行级文本快照（scrollback 区）。
    pub fn scrollback(&self) -> Vec<String> {
        rows_of(self.inner.scrollback())
    }

    /// 底层 buffer 访问：样式断言（错误行高亮 fg/bg、spinner 图标位等）用。
    /// 单元格符号用 `buffer[(x, y)].symbol()`，样式用 `buffer[(x, y)].style`。
    pub fn buffer(&self) -> &Buffer {
        self.inner.buffer()
    }

    /// 样式感知断言（qa R2 需求）：断言第 `y` 行**同时满足**——存在一个
    /// 前景/背景色匹配指定色的单元格，且行文本包含 `contains`。
    /// 只比较 fg/bg 维度（不关心粗体/下划线等修饰），降低测试与实现耦合；
    /// `fg`/`bg` 传 `None` 表示该维度不限。用于「错误行高亮」断言
    /// （error 色 `(255,107,128)` vs 正常色可辨）。
    pub fn assert_row_styled(
        &self,
        y: u16,
        fg: Option<Color>,
        bg: Option<Color>,
        contains: &str,
    ) {
        let buf = self.buffer();
        if y >= buf.area.height {
            panic!(
                "assert_row_styled: row {y} out of range (height {})",
                buf.area.height
            );
        }
        let row: String = (0..buf.area.width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect();
        let styled = (0..buf.area.width).any(|x| {
            let s = buf[(x, y)].style();
            let fg_ok = match fg {
                Some(c) => s.fg == Some(c),
                None => true,
            };
            let bg_ok = match bg {
                Some(c) => s.bg == Some(c),
                None => true,
            };
            fg_ok && bg_ok
        });
        assert!(styled, "row {y} 无匹配样式 fg={fg:?} bg={bg:?}：{row:?}");
        assert!(
            row.contains(contains),
            "row {y} 不含 {contains:?}：{row:?}"
        );
    }

    /// 视口行定位（B 区「滚动到可见区」断言）：返回第一个包含 `needle`
    /// 的视口行索引（`None` = 不在可见区）。区别于 `scrollback()`——
    /// 只查视口，查 scrollback 用 [`Self::scrollback`]。
    pub fn visible_row_containing(&self, needle: &str) -> Option<usize> {
        self.screen()
            .iter()
            .position(|row| row.contains(needle))
    }
}

fn rows_of(buffer: &Buffer) -> Vec<String> {
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

impl Backend for Recorder {
    type Error = Infallible;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Infallible>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let cells: Vec<(u16, u16, &Cell)> = content.collect();
        if !cells.is_empty() {
            self.draw_calls += 1;
        }
        self.inner.draw(cells.into_iter())
    }

    fn append_lines(&mut self, n: u16) -> Result<(), Infallible> {
        self.appended = self.appended.saturating_add(n);
        self.inner.append_lines(n)
    }

    fn hide_cursor(&mut self) -> Result<(), Infallible> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> Result<(), Infallible> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> Result<Position, Infallible> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(
        &mut self,
        position: P,
    ) -> Result<(), Infallible> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> Result<(), Infallible> {
        self.clear_calls += 1;
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Infallible> {
        self.clear_calls += 1;
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> Result<Size, Infallible> {
        self.inner.size()
    }

    fn window_size(&mut self) -> Result<WindowSize, Infallible> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> Result<(), Infallible> {
        self.inner.flush()
    }

    fn scroll_region_up(
        &mut self,
        region: Range<u16>,
        line_count: u16,
    ) -> Result<(), Infallible> {
        self.scrolled_up.push((region.clone(), line_count));
        self.inner.scroll_region_up(region, line_count)
    }

    fn scroll_region_down(
        &mut self,
        region: Range<u16>,
        line_count: u16,
    ) -> Result<(), Infallible> {
        self.scrolled_down.push((region.clone(), line_count));
        self.inner.scroll_region_down(region, line_count)
    }
}

impl RawWrite for Recorder {
    fn write_raw(&mut self, bytes: &[u8]) -> Result<(), Infallible> {
        self.raw.extend_from_slice(bytes);
        Ok(())
    }

    // Semantically a top-anchored region scroll whose evicted rows land
    // in scrollback — exactly what TestBackend models for
    // `scroll_region_up`, so the driver tests keep asserting real
    // semantics while the production backend emits the LF form.
    fn scroll_into_scrollback(&mut self, top: u16, n: u16) -> Result<(), Infallible> {
        self.scrolled_up.push((0..top, n));
        self.inner.scroll_region_up(0..top, n)
    }
}

// ---------------------------------------------------------------------------
// 错误态 fixture（#14 R1 层1：qa 断言 + 呈现层验收 + dev 本地预览共用载体）
// ---------------------------------------------------------------------------

/// 触发上下文（生产契约，`src/error.rs`）：呈现级别由它决定，不单由 code 推断。
pub use crate::error::ErrorContext;

/// 呈现级别（生产契约，`src/error.rs`）：渲染端按级别分支。
pub use crate::error::ErrorLevel;

/// 错误态 fixture：单一数据载体（qa 断言 / ui/ux 验收 / dev 预览共用）。
/// 字段对齐 presentation v1.5 FX 清单（10 个可注入稳定码 + FX-11 长回合）。
#[derive(Debug, Clone, Copy)]
pub struct ErrorFixture {
    pub code: &'static str,
    /// 人话文案（AC-28：发生了什么 + 能做什么）。qa 断言只锚 code，msg 永不断言。
    pub msg: &'static str,
    pub context: ErrorContext,
    pub level: ErrorLevel,
    /// 用户动作（重试 / 返回 / 检查网络…），D 区重试/返回可达性的动作锚。
    pub action: &'static str,
    /// 期望错误色 RGB（样色基线 error (255,107,128)，A 区样式断言锚）。
    pub expect_style: (u8, u8, u8),
}

impl ErrorFixture {
    /// 注入：转结构化 `UiEvent::Error` 发到 test_chat 的 events 通道。
    /// 零生产改动——`UiEvent::Error` 已是结构化事件，chat 消费端按
    /// `level` 记录错误态、渲染端按级别分支。
    pub fn inject(&self, events: &tokio::sync::mpsc::UnboundedSender<crate::ui::UiEvent>) {
        let _ = events.send(crate::ui::UiEvent::Error {
            code: self.code,
            msg: self.msg.to_string(),
            level: self.level,
            context: self.context,
        });
    }
}

/// §4.4 全部 10 个可注入稳定码（FX-01…11）。`GENERIC` 不进 fixture（无实际
/// 返回点，护栏归 error.rs 单测）；FX-12 混合态 / FX-13 折叠详情不在注入集合。
///
/// 注意：**仅 TIMEOUT 类由 context 决定级别**（短同步=页面级、长回合=全流程级，
/// FX-01/FX-11 同码分档）；其余码级别固有（AUTH_REQUIRED/PERMISSION_DENIED=
/// 全流程级、CONFIG_INVALID=字段级、其余=页面级），其 context 字段仅为信息性
/// 记录（生产发射时已知），不参与级别推导。
pub fn error_fixtures() -> Vec<ErrorFixture> {
    use ErrorContext::{LongTurn, ShortSync};
    use ErrorLevel::{Field, Full, Page};
    const ERR: (u8, u8, u8) = (255, 107, 128); // 样色基线
    vec![
        // FX-01 短同步读超时 → 页面级
        ErrorFixture { code: "TIMEOUT", msg: "请求超时，可重试", context: ShortSync, level: Page, action: "重试", expect_style: ERR },
        // FX-02 服务端错误 → 页面级
        ErrorFixture { code: "SERVER_ERROR", msg: "服务端错误，稍后重试", context: ShortSync, level: Page, action: "稍后重试", expect_style: ERR },
        // FX-03 无网络 → 页面级
        ErrorFixture { code: "OFFLINE", msg: "无网络连接，请检查网络后重试", context: ShortSync, level: Page, action: "检查网络后重试", expect_style: ERR },
        // FX-04 登录过期/缺 key → 全流程级
        ErrorFixture { code: "AUTH_REQUIRED", msg: "登录已过期或缺少 API key，请重新登录或配置 key", context: ShortSync, level: Full, action: "重新登录", expect_style: ERR },
        // FX-05 无权限 → 全流程级
        ErrorFixture { code: "PERMISSION_DENIED", msg: "无权限执行此操作，请返回或申请权限", context: ShortSync, level: Full, action: "返回/申请权限", expect_style: ERR },
        // FX-06 配置校验失败 → 字段级（仅标错误对象）
        ErrorFixture { code: "CONFIG_INVALID", msg: "配置校验失败，请修正配置", context: ShortSync, level: Field, action: "修正配置", expect_style: ERR },
        // FX-07 限流/429 → 页面级
        ErrorFixture { code: "RATE_LIMITED", msg: "请求过于频繁，请稍后重试", context: ShortSync, level: Page, action: "稍后重试", expect_style: ERR },
        // FX-08 工具执行失败 → 页面级
        ErrorFixture { code: "TOOL_FAILED", msg: "工具执行失败，请查看输出后重试", context: ShortSync, level: Page, action: "查看输出后重试", expect_style: ERR },
        // FX-09 hook 执行失败 → 页面级
        ErrorFixture { code: "HOOK_FAILED", msg: "hook 执行失败，请检查 hook 配置", context: ShortSync, level: Page, action: "检查 hook 配置", expect_style: ERR },
        // FX-10 本地存储失败 → 页面级
        ErrorFixture { code: "STORAGE_ERROR", msg: "本地存储失败，请检查磁盘或权限", context: ShortSync, level: Page, action: "检查磁盘/权限", expect_style: ERR },
        // FX-11 长回合传输层超时 → 全流程级（AC-53，与 FX-01 共用 TIMEOUT 码、级别由上下文区分）
        ErrorFixture { code: "TIMEOUT", msg: "长回合中断，可重试或返回", context: LongTurn, level: Full, action: "可重试或返回", expect_style: ERR },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    /// helper 自测（qa 依赖这些 API，先证明它正确）：
    /// error 色 `(255,107,128)` 与正常行可辨，视口定位准确。
    #[test]
    fn styled_row_assertion_distinguishes_error_color() {
        let mut r = Recorder::new(20, 5);
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 5));
        buf.set_string(
            0,
            2,
            "ERROR: boom",
            Style::default().fg(Color::Rgb(255, 107, 128)),
        );
        buf.set_string(0, 3, "normal row", Style::default());
        let cells = buf.content().iter().enumerate().map(|(i, c)| {
            let x = (i as u16) % 20;
            let y = (i as u16) / 20;
            (x, y, c)
        });
        Backend::draw(&mut r, cells).unwrap();
        // 样式 + 文本双条件命中。
        r.assert_row_styled(2, Some(Color::Rgb(255, 107, 128)), None, "boom");
        // 视口定位：可见行命中 / 未命中。
        assert_eq!(r.visible_row_containing("boom"), Some(2));
        assert_eq!(r.visible_row_containing("normal"), Some(3));
        assert_eq!(r.visible_row_containing("missing"), None);
        // 不限样式维度时，普通行也可断言（文本命中即可）。
        r.assert_row_styled(3, None, None, "normal");
    }

    /// fixture 清单完整性（qa 断言 / ui/ux 验收 / dev 预览的单一数据源）：
    /// 覆盖 §4.4 全部 10 个可注入稳定码；`TIMEOUT` 双级别（短同步/长回合）
    /// 由 context 区分；不含 `GENERIC`。
    #[test]
    fn fixtures_cover_all_injectable_codes() {
        let fxs = error_fixtures();
        let mut codes: Vec<&str> = fxs.iter().map(|f| f.code).collect();
        codes.sort_unstable();
        codes.dedup();
        let expect = [
            "AUTH_REQUIRED",
            "CONFIG_INVALID",
            "HOOK_FAILED",
            "OFFLINE",
            "PERMISSION_DENIED",
            "RATE_LIMITED",
            "SERVER_ERROR",
            "STORAGE_ERROR",
            "TIMEOUT",
            "TOOL_FAILED",
        ];
        assert_eq!(codes, expect, "fixture 覆盖 §4.4 全部 10 个可注入稳定码");
        assert!(
            !fxs.iter().any(|f| f.code == "GENERIC"),
            "GENERIC 不进 fixture（无实际返回点）"
        );
        for f in &fxs {
            assert!(
                !f.code.is_empty()
                    && f.code.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                    && f.code.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
                "fixture 码必须 SCREAMING_SNAKE：{:?}",
                f.code
            );
        }
        // FX-01 与 FX-11 共用 TIMEOUT，级别由 context 区分。
        let short = fxs.iter().find(|f| f.code == "TIMEOUT" && f.context == ErrorContext::ShortSync);
        let long = fxs.iter().find(|f| f.code == "TIMEOUT" && f.context == ErrorContext::LongTurn);
        assert_eq!(short.map(|f| f.level), Some(ErrorLevel::Page), "短同步 TIMEOUT = 页面级");
        assert_eq!(long.map(|f| f.level), Some(ErrorLevel::Full), "长回合 TIMEOUT = 全流程级");
    }

    /// dev 本地预览（fixture 数据 dump）：`cargo test -- --nocapture` 可见
    /// 各错误级的定义载体。真实渲染预览等 #18 呈现层实现后升级（注入→渲染→dump）。
    #[test]
    fn preview_error_fixtures() {
        for (i, f) in error_fixtures().iter().enumerate() {
            println!(
                "FX-{:02} {:14} ctx={:?} level={:?} style={:?} action={:?} | {}",
                i + 1, f.code, f.context, f.level, f.expect_style, f.action, f.msg
            );
        }
    }
}
