//! 显示层的样式化行模型：markdown 渲染与 activities 布局都产出
//! [`Line`]，再由 UI 层映射为 iocraft 元素。与显示库解耦。

use iocraft::prelude::Color;
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

/// 一个带样式的文本段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegStyle {
    /// 前景色。
    pub fg: Option<Color>,
    /// 背景色。
    pub bg: Option<Color>,
    /// 加粗。
    pub bold: bool,
    /// 斜体。
    pub italic: bool,
    /// 下划线。
    pub underline: bool,
}

impl SegStyle {
    pub const fn plain() -> Self {
        Self {
            fg: None,
            bg: None,
            bold: false,
            italic: false,
            underline: false,
        }
    }

    /// 叠加另一层样式：`other` 中出现的字段覆盖/启用自身（只加不减，
    /// 与 ratatui `Style::patch` 语义一致）。
    pub fn patch(self, other: SegStyle) -> SegStyle {
        SegStyle {
            fg: other.fg.or(self.fg),
            bg: other.bg.or(self.bg),
            bold: self.bold || other.bold,
            italic: self.italic || other.italic,
            underline: self.underline || other.underline,
        }
    }

    pub fn fg(color: Color) -> Self {
        Self { fg: Some(color), ..SegStyle::plain() }
    }

    /// 设置背景色（链式）。
    pub fn with_bg(mut self, color: Color) -> Self {
        self.bg = Some(color);
        self
    }

    pub fn bold(self) -> Self {
        Self { bold: true, ..self }
    }

    pub fn italic(self) -> Self {
        Self { italic: true, ..self }
    }

    pub fn underline(self) -> Self {
        Self { underline: true, ..self }
    }
}

/// 一行样式化文本（多个段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// 按顺序排布的段。
    pub segs: Vec<Seg>,
    /// 图片块引用：块内每一行都携带（首行 + 后续空行），显示层按 url
    /// 识别块边界：块首行输出 kitty 序列，续行跳过。
    pub image: Option<ImageRef>,
}

/// 图片块引用（指向 [`crate::tui::gfx::ImageMeta`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef {
    pub url: String,
    pub cols: usize,
    pub rows: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seg {
    pub text: String,
    pub style: SegStyle,
}

impl Line {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            segs: vec![Seg { text: text.into(), style: SegStyle::plain() }],
            image: None,
        }
    }

    pub fn styled(text: impl Into<String>, style: SegStyle) -> Self {
        Self {
            segs: vec![Seg { text: text.into(), style }],
            image: None,
        }
    }

    pub fn empty() -> Self {
        Self { segs: Vec::new(), image: None }
    }

    pub fn is_empty(&self) -> bool {
        self.segs.iter().all(|s| s.text.is_empty())
    }

    /// 整行应用一个样式（叠加，不覆盖已有颜色）。
    pub fn styled_all(mut self, style: SegStyle) -> Self {
        for seg in &mut self.segs {
            seg.style = seg.style.patch(style);
        }
        self
    }

    /// 行首插入一段（活动点前缀等）。
    pub fn prepend(&mut self, seg: Seg) {
        self.segs.insert(0, seg);
    }

    pub fn prepend_styled(&mut self, text: impl Into<String>, style: SegStyle) {
        self.prepend(Seg { text: text.into(), style });
    }

    pub fn push_styled(&mut self, text: impl Into<String>, style: SegStyle) {
        self.segs.push(Seg { text: text.into(), style });
    }

    /// 纯文本内容。
    pub fn plain_text(&self) -> String {
        self.segs.iter().map(|s| s.text.as_str()).collect()
    }

}

/// CJK 感知的字符串显示宽度。
pub fn text_width(s: &str) -> usize {
    s.width()
}

/// 单个字符的显示宽度。
pub fn char_width(c: char) -> usize {
    c.width().unwrap_or(0)
}
