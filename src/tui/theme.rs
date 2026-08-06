//! 语义色板（1:1 对标 Claude Code 2.1.88 `utils/theme.ts` 的 dark 令牌），
//! 附 bingo 自有的语义令牌（diff/code/thinking 等）。所有渲染走这一个
//! [`Theme`]，样式方法返回 [`SegStyle`]。

use iocraft::prelude::Color;

use crate::tui::line::SegStyle;

/// 语义颜色令牌集合。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    /// 正文（CC `text` = rgb(255,255,255)）。
    pub text: Color,
    /// 弱化文本（CC `inactive` = rgb(153,153,153)，即 ink dimColor）。
    pub inactive: Color,
    /// 极弱文本（CC `subtle`，sticky header 的 ❯ 用）。
    pub subtle: Color,
    /// 主强调色（CC `claude` = rgb(215,119,87)）。
    pub claude: Color,
    /// 权限对话框强调色（CC `permission` = rgb(177,185,249)）。
    pub permission: Color,
    /// 成功（CC `success` = rgb(78,186,101)）。
    pub success: Color,
    /// 错误（CC `error` = rgb(255,107,128)）。
    pub error: Color,
    /// 警告（CC `warning` = rgb(255,193,7)）。
    pub warning: Color,
    /// plan 模式（CC `planMode` = rgb(72,150,140)）。
    pub plan_mode: Color,
    /// accept edits 模式（CC `autoAccept` = rgb(175,135,255)）。
    pub accept_edits: Color,
    /// 输入框下边框（CC `promptBorder` = rgb(136,136,136)）。
    pub prompt_border: Color,
    /// bash 模式强调（CC `bashBorder` = rgb(253,93,177)）。
    pub bash_border: Color,
    /// 用户消息气泡背景（CC `userMessageBackground` = rgb(55,55,55)）。
    pub user_message_bg: Color,
    /// 行内代码。
    pub code_fg: Color,
    /// 代码块文本。
    pub code_block_fg: Color,
    /// 代码块背景。
    pub code_block_bg: Color,
    /// 链接。
    pub link: Color,
    /// 数学公式。
    pub math: Color,
    /// 标题色（按级别，1 起；越界回退最后一项）。
    pub headings: Vec<Color>,
    /// 引用文本。
    pub quote: Color,
    /// 引用左栏。
    pub quote_bar: Color,
    /// 已完成任务标记。
    pub task_done: Color,
    /// 未完成任务标记。
    pub task_open: Color,
    /// 列表符号。
    pub list_marker: Color,
    /// 表格网格线。
    pub table_border: Color,
    /// 表格表头。
    pub table_header: Color,
    /// 分隔线。
    pub hr: Color,
    /// 脚注。
    pub footnote: Color,
    /// 思考块（CC：dim italic `∴ Thinking`）。
    pub thinking: Color,
    /// 运行中的工具。
    pub tool_running: Color,
    /// 工具输出预览。
    pub tool_output: Color,
    /// Diff hunk 头。
    pub diff_hunk: Color,
    /// Diff 上下文行。
    pub diff_context: Color,
    /// `✻` 编辑标记。
    pub diff_edit: Color,
}

impl Theme {
    /// 深色预设（默认；令牌值对标 CC dark）。
    pub fn dark() -> Self {
        Self {
            text: Color::Rgb { r: 255, g: 255, b: 255 },
            inactive: Color::Rgb { r: 153, g: 153, b: 153 },
            subtle: Color::Rgb { r: 80, g: 80, b: 80 },
            claude: Color::Rgb { r: 215, g: 119, b: 87 },
            permission: Color::Rgb { r: 177, g: 185, b: 249 },
            success: Color::Rgb { r: 78, g: 186, b: 101 },
            error: Color::Rgb { r: 255, g: 107, b: 128 },
            warning: Color::Rgb { r: 255, g: 193, b: 7 },
            plan_mode: Color::Rgb { r: 72, g: 150, b: 140 },
            accept_edits: Color::Rgb { r: 175, g: 135, b: 255 },
            prompt_border: Color::Rgb { r: 136, g: 136, b: 136 },
            bash_border: Color::Rgb { r: 253, g: 93, b: 177 },
            user_message_bg: Color::Rgb { r: 55, g: 55, b: 55 },
            code_fg: Color::Yellow,
            code_block_fg: Color::Rgb { r: 170, g: 170, b: 170 },
            code_block_bg: Color::Rgb { r: 24, g: 24, b: 24 },
            link: Color::Blue,
            math: Color::Magenta,
            headings: vec![
                Color::White,
                Color::Cyan,
                Color::Blue,
                Color::DarkGrey,
            ],
            quote: Color::Grey,
            quote_bar: Color::Blue,
            task_done: Color::Green,
            task_open: Color::DarkGrey,
            list_marker: Color::Cyan,
            table_border: Color::DarkGrey,
            table_header: Color::Reset,
            hr: Color::DarkGrey,
            footnote: Color::DarkGrey,
            thinking: Color::DarkGrey,
            tool_running: Color::Cyan,
            tool_output: Color::DarkGrey,
            diff_hunk: Color::Cyan,
            diff_context: Color::Grey,
            diff_edit: Color::Yellow,
        }
    }

    /// 浅色预设（对齐 CC `lightTheme` token：正文黑、claude 橙保持橙）。
    pub fn light() -> Self {
        Self {
            text: Color::Rgb { r: 0, g: 0, b: 0 },
            inactive: Color::Rgb { r: 102, g: 102, b: 102 },
            subtle: Color::Rgb { r: 175, g: 175, b: 175 },
            claude: Color::Rgb { r: 215, g: 119, b: 87 },
            permission: Color::Rgb { r: 87, g: 105, b: 247 },
            success: Color::Rgb { r: 44, g: 122, b: 57 },
            error: Color::Rgb { r: 171, g: 43, b: 63 },
            warning: Color::Rgb { r: 150, g: 108, b: 30 },
            plan_mode: Color::Rgb { r: 0, g: 102, b: 102 },
            accept_edits: Color::Rgb { r: 135, g: 0, b: 255 },
            prompt_border: Color::Rgb { r: 153, g: 153, b: 153 },
            bash_border: Color::Rgb { r: 255, g: 0, b: 135 },
            user_message_bg: Color::Rgb { r: 240, g: 240, b: 240 },
            code_fg: Color::Rgb { r: 150, g: 108, b: 30 },
            code_block_fg: Color::Rgb { r: 60, g: 60, b: 60 },
            code_block_bg: Color::Rgb { r: 245, g: 245, b: 245 },
            link: Color::Rgb { r: 0, g: 90, b: 180 },
            math: Color::Rgb { r: 130, g: 0, b: 130 },
            headings: vec![
                Color::Rgb { r: 30, g: 30, b: 30 },
                Color::Rgb { r: 0, g: 102, b: 102 },
                Color::Rgb { r: 0, g: 90, b: 180 },
                Color::Rgb { r: 120, g: 120, b: 120 },
            ],
            quote: Color::Rgb { r: 90, g: 90, b: 90 },
            quote_bar: Color::Rgb { r: 0, g: 90, b: 180 },
            task_done: Color::Rgb { r: 44, g: 122, b: 57 },
            task_open: Color::Rgb { r: 153, g: 153, b: 153 },
            list_marker: Color::Rgb { r: 0, g: 102, b: 102 },
            table_border: Color::Rgb { r: 153, g: 153, b: 153 },
            table_header: Color::Rgb { r: 30, g: 30, b: 30 },
            hr: Color::Rgb { r: 153, g: 153, b: 153 },
            footnote: Color::Rgb { r: 120, g: 120, b: 120 },
            thinking: Color::Rgb { r: 102, g: 102, b: 102 },
            tool_running: Color::Rgb { r: 0, g: 102, b: 102 },
            tool_output: Color::Rgb { r: 90, g: 90, b: 90 },
            diff_hunk: Color::Rgb { r: 0, g: 102, b: 102 },
            diff_context: Color::Rgb { r: 90, g: 90, b: 90 },
            diff_edit: Color::Rgb { r: 150, g: 108, b: 30 },
        }
    }
}

/// 主题设置（对标 CC `ThemeSetting`）：auto 跟随终端背景亮度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeSetting {
    Auto,
    Dark,
    Light,
}

impl ThemeSetting {
    /// 从 settings 字符串解析；缺省 / 非法值回落 auto。
    pub fn parse(s: Option<&str>) -> Self {
        match s.map(str::trim).unwrap_or("") {
            "dark" => Self::Dark,
            "light" => Self::Light,
            _ => Self::Auto,
        }
    }
}

impl Theme {
    /// 检测终端 truecolor 能力（COLORTERM=truecolor/24bit）。
    pub fn terminal_supports_truecolor() -> bool {
        std::env::var("COLORTERM")
            .map(|v| v.contains("truecolor") || v.contains("24bit"))
            .unwrap_or(false)
    }

    /// 解析 `$COLORFGBG`（fg;bg 或 fg;other;bg，bg 为 ANSI 色号）：
    /// rxvt 约定 0-6/8 为暗色背景、7/9-15 为亮色。
    fn detect_from_color_fgbg() -> Option<bool> {
        let bg = std::env::var("COLORFGBG")
            .ok()?
            .split(';')
            .next_back()
            .and_then(|v| v.parse::<u8>().ok())?;
        if bg > 15 {
            return None;
        }
        Some(bg <= 6 || bg == 8)
    }

    /// 通过 OSC 11 查询终端真实背景色（对标 CC systemThemeWatcher）。
    /// 发送 `ESC ] 11 ; ? ESC \`，读回 `rgb:RRRR/GGGG/BBBB`，按 BT.709
    /// 相对亮度判断：> 0.5 为浅色。
    /// 必须在 raw mode 下读：OSC 回复不带换行，规范模式行缓冲会吞掉；
    /// 查询期间临时进 raw mode（约 200ms，其间按键可能被消费，可接受）。
    ///
    /// 不能用 tokio::fs 读：timeout 后 blocking 线程的 read() 无法取消，
    /// 会一直卡着吞掉之后的第一个输入，且 tokio 关停时 join 该线程导致
    /// 进程退不出。这里用 O_NONBLOCK + 轮询，任何时刻都不阻塞。
    pub async fn detect_system_theme() -> Option<bool> {
        use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
        use std::io::{Read, Write};
        use std::os::unix::fs::OpenOptionsExt;

        let mut tty = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open("/dev/tty")
            .ok()?;
        enable_raw_mode().ok()?;
        let mut buf: Vec<u8> = Vec::new();
        let result = async {
            tty.write_all(b"\x1b]11;?\x1b\\").ok()?;
            let mut chunk = [0u8; 64];
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
            loop {
                match tty.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        if buf.windows(2).any(|w| w == b"\x1b\\") {
                            return Some(());
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(_) => break,
                }
                if std::time::Instant::now() >= deadline {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            Some(())
        }
        .await;
        disable_raw_mode().ok();
        result?;
        theme_from_osc(&buf)
    }

    /// 当前终端的主题：按设置解析（auto 优先 OSC 11 实测背景色，
    /// 其次 `$COLORFGBG`，最后回落 dark），
    /// 不支持 truecolor 时 RGB 降级为 256 色近似
    /// （否则终端忽略 24bit 序列，全部文本回退默认色）。
    pub fn for_terminal(setting: ThemeSetting, detected: Option<bool>) -> Self {
        let t = match setting {
            ThemeSetting::Dark => Theme::dark(),
            ThemeSetting::Light => Theme::light(),
            ThemeSetting::Auto => match detected.or_else(Self::detect_from_color_fgbg) {
                Some(true) => Theme::dark(),
                Some(false) => Theme::light(),
                None => Theme::dark(),
            },
        };
        if Self::terminal_supports_truecolor() {
            t
        } else {
            t.downgrade_to_256()
        }
    }

    /// 把所有 RGB 颜色映射为 256 色（AnsiValue）近似。
    fn downgrade_to_256(mut self) -> Self {
        let f = |c: Color| match c {
            Color::Rgb { r, g, b } => Color::AnsiValue(rgb_to_ansi256(r, g, b)),
            c => c,
        };
        self.text = f(self.text);
        self.inactive = f(self.inactive);
        self.subtle = f(self.subtle);
        self.claude = f(self.claude);
        self.permission = f(self.permission);
        self.success = f(self.success);
        self.error = f(self.error);
        self.warning = f(self.warning);
        self.plan_mode = f(self.plan_mode);
        self.accept_edits = f(self.accept_edits);
        self.prompt_border = f(self.prompt_border);
        self.bash_border = f(self.bash_border);
        self.user_message_bg = f(self.user_message_bg);
        self.code_fg = f(self.code_fg);
        self.code_block_fg = f(self.code_block_fg);
        self.code_block_bg = f(self.code_block_bg);
        self.link = f(self.link);
        self.math = f(self.math);
        self.headings = self.headings.into_iter().map(f).collect();
        self.quote = f(self.quote);
        self.quote_bar = f(self.quote_bar);
        self.task_done = f(self.task_done);
        self.task_open = f(self.task_open);
        self.list_marker = f(self.list_marker);
        self.table_border = f(self.table_border);
        self.table_header = f(self.table_header);
        self.hr = f(self.hr);
        self.footnote = f(self.footnote);
        self.thinking = f(self.thinking);
        self.tool_running = f(self.tool_running);
        self.tool_output = f(self.tool_output);
        self.diff_hunk = f(self.diff_hunk);
        self.diff_context = f(self.diff_context);
        self.diff_edit = f(self.diff_edit);
        self
    }

    /// 正文。
    pub fn text(&self) -> SegStyle {
        SegStyle::fg(self.text)
    }
    /// 弱化文本。
    pub fn dim(&self) -> SegStyle {
        SegStyle::fg(self.inactive)
    }
    /// 加粗。
    pub fn strong(&self) -> SegStyle {
        SegStyle::plain().bold()
    }
    /// 斜体。
    pub fn emphasis(&self) -> SegStyle {
        SegStyle::plain().italic()
    }
    /// 删除线（终端无删除线 → 弱化呈现）。
    pub fn strikethrough(&self) -> SegStyle {
        SegStyle::fg(self.inactive)
    }
    /// 行内代码。
    pub fn code(&self) -> SegStyle {
        SegStyle::fg(self.code_fg)
    }
    /// 代码块。
    pub fn code_block(&self) -> SegStyle {
        SegStyle::fg(self.code_block_fg).with_bg(self.code_block_bg)
    }
    /// 链接（下划线）。
    pub fn link(&self) -> SegStyle {
        SegStyle::fg(self.link).underline()
    }
    /// 数学（斜体）。
    pub fn math(&self) -> SegStyle {
        SegStyle::fg(self.math).italic()
    }
    /// 标题（级别 1 起；0 或越界回退）。
    pub fn heading(&self, level: u8) -> SegStyle {
        let color = if level == 0 {
            self.text
        } else {
            self.headings
                .get((level - 1) as usize)
                .copied()
                .unwrap_or(*self.headings.last().unwrap_or(&self.text))
        };
        SegStyle::fg(color).bold().underline()
    }
    /// 引用文本。
    pub fn quote(&self) -> SegStyle {
        SegStyle::fg(self.quote)
    }
    /// 引用左栏。
    pub fn quote_bar(&self) -> SegStyle {
        SegStyle::fg(self.quote_bar)
    }
    /// 已完成任务标记。
    pub fn task_done(&self) -> SegStyle {
        SegStyle::fg(self.task_done).bold()
    }
    /// 未完成任务标记。
    pub fn task_open(&self) -> SegStyle {
        SegStyle::fg(self.task_open)
    }
    /// 列表符号。
    pub fn list_marker(&self) -> SegStyle {
        SegStyle::fg(self.list_marker)
    }
    /// 表格网格线。
    pub fn table_border(&self) -> SegStyle {
        SegStyle::fg(self.table_border)
    }
    /// 表格表头。
    pub fn table_header(&self) -> SegStyle {
        SegStyle::fg(self.table_header).bold()
    }
    /// 分隔线。
    pub fn hr(&self) -> SegStyle {
        SegStyle::fg(self.hr)
    }
    /// 脚注。
    pub fn footnote(&self) -> SegStyle {
        SegStyle::fg(self.footnote)
    }
    /// 思考块（dim italic）。
    pub fn thinking(&self) -> SegStyle {
        SegStyle::fg(self.thinking).italic()
    }
    /// 主活动点 `⏺`。
    pub fn activity_dot(&self) -> SegStyle {
        SegStyle::fg(self.text)
    }
    /// 运行中的工具。
    pub fn tool_running(&self) -> SegStyle {
        SegStyle::fg(self.tool_running).bold()
    }
    /// 完成的工具。
    pub fn tool_done(&self) -> SegStyle {
        SegStyle::fg(self.success).bold()
    }
    /// 失败的工具。
    pub fn tool_error(&self) -> SegStyle {
        SegStyle::fg(self.error).bold()
    }
    /// 工具输出预览。
    pub fn tool_output(&self) -> SegStyle {
        SegStyle::fg(self.tool_output)
    }
    /// Diff hunk 头。
    pub fn diff_hunk(&self) -> SegStyle {
        SegStyle::fg(self.diff_hunk).bold()
    }
    /// Diff 新增。
    pub fn diff_added(&self) -> SegStyle {
        SegStyle::fg(self.success)
    }
    /// Diff 删除。
    pub fn diff_removed(&self) -> SegStyle {
        SegStyle::fg(self.error)
    }
    /// Diff 上下文。
    pub fn diff_context(&self) -> SegStyle {
        SegStyle::fg(self.diff_context)
    }
    /// `✻` 编辑标记。
    pub fn diff_edit(&self) -> SegStyle {
        SegStyle::fg(self.diff_edit).bold()
    }
    /// 权限标题。
    pub fn permission(&self) -> SegStyle {
        SegStyle::fg(self.permission).bold()
    }
}

/// RGB → 256 色（AnsiValue）近似：6×6×6 cube + 灰阶。
fn rgb_to_ansi256(r: u8, g: u8, b: u8) -> u8 {
    // 灰阶：r==g==b 且不在 cube 节点附近
    if r == g && g == b {
        let v = r;
        if v < 8 {
            return 16;
        }
        if v > 248 {
            return 231;
        }
        // 24 级灰阶近似（灰阶步长 10）
        let idx = (((v as u16 - 8) + 5) / 10).clamp(0, 23) as u8;
        return 232 + idx;
    }
    let q = |v: u8| {
        if v < 48 {
            0
        } else if v < 115 {
            1
        } else {
            ((v as u16 - 35) / 40) as u8
        }
    };
    16 + 36 * q(r) + 6 * q(g) + q(b)
}

/// 解析 OSC 11 颜色响应（`rgb:RRRR/GGGG/BBBB` 或 `#RRGGBB`），
/// 按 BT.709 相对亮度返回：true = 深色背景，false = 浅色。无法解析返回 None。
fn theme_from_osc(data: &[u8]) -> Option<bool> {
    let text = std::str::from_utf8(data).ok()?;
    let payload = text.split("]11;").nth(1)?.split("\x1b\\").next()?;
    let (r, g, b) = if let Some(rest) = payload.strip_prefix("rgb:") {
        let mut parts = rest.split('/');
        let comp = |p: Option<&str>| -> Option<f64> {
            let hex = p?.trim_end_matches(';');
            let n = hex.len();
            if !(1..=4).contains(&n) {
                return None;
            }
            Some(u16::from_str_radix(hex, 16).ok()? as f64 / (16f64.powi(n as i32) - 1.0))
        };
        (comp(parts.next())?, comp(parts.next())?, comp(parts.next())?)
    } else if let Some(rest) = payload.strip_prefix('#') {
        let n = rest.len() / 3;
        if !(1..=4).contains(&n) || rest.len() % 3 != 0 {
            return None;
        }
        let comp = |s: &str| -> Option<f64> {
            Some(u16::from_str_radix(s, 16).ok()? as f64 / (16f64.powi(n as i32) - 1.0))
        };
        (
            comp(&rest[..n])?,
            comp(&rest[n..2 * n])?,
            comp(&rest[2 * n..])?,
        )
    } else {
        return None;
    };
    let luminance = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    Some(luminance <= 0.5)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_matches_claude_code_tokens() {
        let t = Theme::dark();
        assert_eq!(t.claude, Color::Rgb { r: 215, g: 119, b: 87 });
        assert_eq!(
            t.permission,
            Color::Rgb { r: 177, g: 185, b: 249 }
        );
        assert_eq!(
            t.user_message_bg,
            Color::Rgb { r: 55, g: 55, b: 55 }
        );
    }

    #[test]
    fn light_differs() {
        // CC light 主题正文黑、背景浅；claude 橙两主题相同。
        assert_eq!(Theme::dark().claude, Theme::light().claude);
        assert_ne!(Theme::dark().text, Theme::light().text);
        assert_ne!(
            Theme::dark().user_message_bg,
            Theme::light().user_message_bg
        );
    }

    #[test]
    fn rgb_downgrades_to_ansi256() {
        // claude 橙 → 256 色
        let downgraded = Theme::dark().downgrade_to_256();
        assert_eq!(downgraded.claude, Color::AnsiValue(173));
        assert_eq!(downgraded.permission, Color::AnsiValue(147));
        assert_eq!(downgraded.user_message_bg, Color::AnsiValue(237));
        // 非 RGB 保持
        assert_eq!(downgraded.code_fg, Theme::dark().code_fg);
    }

    #[test]
    fn rgb_to_ansi256_approximation() {
        assert_eq!(rgb_to_ansi256(0, 0, 0), 16);
        assert_eq!(rgb_to_ansi256(255, 255, 255), 231);
        assert_eq!(rgb_to_ansi256(128, 128, 128), 244);
        // 红 cube
        assert_eq!(rgb_to_ansi256(255, 0, 0), 196);
        assert_eq!(rgb_to_ansi256(0, 255, 0), 46);
        assert_eq!(rgb_to_ansi256(0, 0, 255), 21);
    }

    #[test]
    fn heading_falls_back_for_deep_levels() {
        let t = Theme::dark();
        assert_eq!(t.heading(1).fg, Some(Color::White));
        assert_eq!(t.heading(9).fg, Some(Color::DarkGrey));
        assert_eq!(t.heading(0).fg, Some(t.text), "level 0 = plain text color");
    }

    #[test]
    fn theme_setting_parses() {
        assert_eq!(ThemeSetting::parse(None), ThemeSetting::Auto);
        assert_eq!(ThemeSetting::parse(Some("dark")), ThemeSetting::Dark);
        assert_eq!(ThemeSetting::parse(Some("light")), ThemeSetting::Light);
        assert_eq!(ThemeSetting::parse(Some("  dark ")), ThemeSetting::Dark);
        assert_eq!(ThemeSetting::parse(Some("weird")), ThemeSetting::Auto);
        assert_eq!(ThemeSetting::parse(Some("")), ThemeSetting::Auto);
    }

    #[test]
    fn for_terminal_respects_setting() {
        // 显式主题不依赖终端检测。
        let dark = Theme::for_terminal(ThemeSetting::Dark, None);
        let light = Theme::for_terminal(ThemeSetting::Light, None);
        assert_eq!(dark.text, Color::Rgb { r: 255, g: 255, b: 255 });
        assert_eq!(light.text, Color::Rgb { r: 0, g: 0, b: 0 });
    }

    #[test]
    fn auto_uses_detected_background() {
        // Some(true) = 深色背景。
        let dark = Theme::for_terminal(ThemeSetting::Auto, Some(true));
        assert_eq!(dark.text, Color::Rgb { r: 255, g: 255, b: 255 });
        // Some(false) = 浅色背景。
        let light = Theme::for_terminal(ThemeSetting::Auto, Some(false));
        assert_eq!(light.text, Color::Rgb { r: 0, g: 0, b: 0 });
    }

    #[test]
    fn osc11_parses_rgb_components() {
        // 深色：亮度低 → true。
        assert_eq!(theme_from_osc(b"\x1b]11;rgb:0000/0000/0000\x1b\\"), Some(true));
        assert_eq!(theme_from_osc(b"\x1b]11;rgb:1a1a/1a1a/1a1a\x1b\\"), Some(true));
        // 浅色：亮度高 → false。
        assert_eq!(theme_from_osc(b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\"), Some(false));
        // 短格式（Terminal.app / kitty 等 1-4 位 hex）。
        assert_eq!(theme_from_osc(b"\x1b]11;rgb:ff/ff/ff\x1b\\"), Some(false));
        assert_eq!(theme_from_osc(b"\x1b]11;rgb:00/00/00\x1b\\"), Some(true));
        // 半位 #RRGGBB 格式。
        assert_eq!(theme_from_osc(b"\x1b]11;#ffffff\x1b\\"), Some(false));
        assert_eq!(theme_from_osc(b"\x1b]11;#000000\x1b\\"), Some(true));
        // 无法解析 → None。
        assert_eq!(theme_from_osc(b"garbage"), None);
        assert_eq!(theme_from_osc(b"\x1b]11;rgb:zzzz/0000/0000\x1b\\"), None);
    }
}
