//! Markdown AST → styled lines (a port of rsmarkdown-tui's
//! `StreamMarkdownRenderer`).
//!
//! Parsing is delegated to `rsmarkdown-core` (display-independent); this
//! module renders the block AST into [`Line`]s — CJK-aware wrapping, cached
//! per block once the width is fixed, so only blocks whose source text
//! changed re-render.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use unicode_width::UnicodeWidthStr;

use rsmarkdown_core::{Alignment, Ast, Block, Document, Inline, ListItem, Renderer};

use crate::tui::gfx::ImageCap;
use crate::tui::line::{ImageRef, Line, SegStyle, char_width};
use crate::tui::theme::Theme;
use crate::ui::ImageMeta;

/// A display adapter: markdown AST → styled lines.
pub struct MarkdownRenderer {
    /// Wrap width.
    width: usize,
    /// Semantic colour palette.
    pub theme: Theme,
    /// Per-block cache: (block source text, rendered lines).
    cached: Vec<Option<(String, Vec<Line>)>>,
    lines: Vec<Line>,
    /// Terminal image capability (kitty protocol); `None` = only the
    /// `#[image]` placeholder is shown.
    image_cap: Option<ImageCap>,
    /// Loaded images (url → metadata).
    images: HashMap<String, Arc<ImageMeta>>,
    /// Urls whose load failed (rendered as a failure marker instead of the
    /// indistinguishable loading placeholder).
    images_failed: HashSet<String>,
    /// Image cache version (bumped by Chat; on change the per-block cache is
    /// cleared).
    images_version: u64,
}

impl Default for MarkdownRenderer {
    fn default() -> Self {
        Self::new(80)
    }
}

impl MarkdownRenderer {
    /// Build a renderer with the given wrap width (dark theme).
    pub fn new(width: usize) -> Self {
        Self::with_theme(width, Theme::dark())
    }

    /// With an explicit theme.
    pub fn with_theme(width: usize, theme: Theme) -> Self {
        Self {
            width,
            theme,
            cached: Vec::new(),
            lines: Vec::new(),
            image_cap: None,
            images: HashMap::new(),
            images_failed: HashSet::new(),
            images_version: 0,
        }
    }

    /// Change the wrap width (invalidates the per-block cache).
    pub fn set_width(&mut self, width: usize) {
        if self.width != width {
            self.width = width;
            self.cached.clear();
        }
    }

    /// Image cache version (incremented once `set_images` takes effect).
    pub fn images_version(&self) -> u64 {
        self.images_version
    }

    /// Update the image capability, loaded images and failed urls; on a
    /// version change the per-block cache is cleared (images go from
    /// placeholder to block or failure rendering).
    pub fn set_images(
        &mut self,
        cap: Option<ImageCap>,
        images: &HashMap<String, Arc<ImageMeta>>,
        failed: &HashSet<String>,
        version: u64,
    ) {
        self.image_cap = cap;
        self.images_version = version;
        self.images.clear();
        self.images
            .extend(images.iter().map(|(k, v)| (k.clone(), v.clone())));
        self.images_failed = failed.clone();
        self.cached.clear();
    }

    /// The output lines of the most recent `render`.
    pub fn lines(&self) -> &[Line] {
        &self.lines
    }
}

impl Renderer for MarkdownRenderer {
    fn render(&mut self, doc: &Document) {
        self.cached.resize(doc.blocks.len(), None);
        let mut out: Vec<Line> = Vec::new();
        for (index, block) in doc.blocks.iter().enumerate() {
            let cached = &mut self.cached[index];
            let rendered = match cached {
                Some((src, lines)) if src == &block.content => lines.clone(),
                _ => {
                    // Blocks whose paragraph ends in an image render as block
                    // images (mirroring rsmarkdown-tui's image_paragraph);
                    // a failed load renders a distinct marker; when merely not
                    // loaded yet, fall back to an ordinary paragraph
                    // (inline #[image]).
                    let lines = match block
                        .ast
                        .as_ref()
                        .and_then(|a| a.children.iter().find_map(image_paragraph))
                    {
                        Some(url) => match self.images.get(url) {
                            Some(meta) => image_block_lines(url, meta, self.theme.clone()),
                            None if self.images_failed.contains(url) => {
                                failed_image_lines(&self.theme)
                            }
                            None => render_block(&block.ast, self.width, &self.theme),
                        },
                        None => render_block(&block.ast, self.width, &self.theme),
                    };
                    *cached = Some((block.content.clone(), lines.clone()));
                    lines
                }
            };
            out.extend(rendered);
            if !out.last().is_none_or(Line::is_empty) {
                out.push(Line::empty());
            }
        }
        self.lines = out;
    }
}

/// Returns the image url when the paragraph ends in an image (block-image
/// detection).
fn image_paragraph(block: &Block) -> Option<&str> {
    let Block::Paragraph(inlines) = block else {
        return None;
    };
    if let Some(Inline::Image { url, .. }) = inlines.last() {
        return Some(url);
    }
    None
}

/// One-line marker for an image whose load failed: unlike the bare loading
/// placeholder, the user can tell the picture is not coming (the warning line
/// carries the url).
fn failed_image_lines(theme: &Theme) -> Vec<Line> {
    let mut line = Line::empty();
    line.push_styled("#[image ✗ load failed]", theme.dim());
    vec![line]
}

/// Image block → `rows` lines, each carrying an [`ImageRef`] with its row
/// index. The render layer turns every row into kitty Unicode placeholder
/// cells; the `#[image]` text on the first row is the fallback shown by
/// terminals without image support (where no [`ImageRef`] rows exist at all)
/// and by width-truncated renders.
fn image_block_lines(url: &str, meta: &ImageMeta, theme: Theme) -> Vec<Line> {
    let mut out = Vec::with_capacity(meta.rows);
    for row in 0..meta.rows {
        let mut line = Line::empty();
        if row == 0 {
            line.push_styled("#[image]", theme.dim());
        }
        line.image = Some(ImageRef {
            url: url.to_string(),
            cols: meta.cols,
            rows: meta.rows,
            row,
        });
        out.push(line);
    }
    out
}

/// Render a block's AST into styled lines.
pub fn render_block(ast: &Option<Ast>, width: usize, theme: &Theme) -> Vec<Line> {
    let Some(ast) = ast else { return Vec::new() };
    let mut out = Vec::new();
    for block in &ast.children {
        render_into(block, &mut out, width, 0, theme);
    }
    out
}

fn render_into(block: &Block, out: &mut Vec<Line>, width: usize, indent: usize, theme: &Theme) {
    match block {
        Block::Paragraph(inlines) => {
            let lines = render_inlines(inlines, width.saturating_sub(indent), theme);
            push_indented(out, lines, indent);
        }
        Block::Heading { level, children } => {
            let lines = render_inlines(children, width.saturating_sub(indent), theme);
            let lines: Vec<Line> = lines
                .into_iter()
                .map(|l| l.styled_all(theme.heading(*level)))
                .collect();
            push_indented(out, lines, indent);
        }
        Block::Code { lang, text } => {
            let _ = lang;
            let body = render_code(text, width.saturating_sub(indent), theme);
            push_indented(out, body, indent);
        }
        Block::BlockQuote(children) => {
            let mut inner = Vec::new();
            for child in children {
                render_into(child, &mut inner, width - 2, 0, theme);
            }
            for line in inner {
                let mut styled = Line::styled("▌ ", theme.quote_bar());
                for seg in line.segs {
                    let s = seg.style.patch(theme.quote());
                    styled.push_styled(seg.text, s);
                }
                out.push(styled);
            }
        }
        Block::List {
            ordered,
            start,
            items,
        } => {
            render_list(theme, items, *ordered, *start, out, width, indent);
        }
        Block::Table {
            headers,
            rows,
            aligns,
        } => {
            render_table(theme, headers, rows, aligns, out, width, indent);
        }
        Block::ThematicBreak => {
            let n = width.saturating_sub(indent);
            out.push(Line::styled("─".repeat(n), theme.hr()));
        }
        Block::Html(html) => {
            for line in html.split('\n') {
                let t = line.trim_end();
                if !t.is_empty() {
                    out.push(Line::styled(t.to_string(), theme.dim()));
                }
            }
        }
        Block::FootnoteDefinition { label, children } => {
            let mut inner = Vec::new();
            for child in children {
                render_into(child, &mut inner, width - 2, 0, theme);
            }
            if let Some(first) = inner.first_mut() {
                first.prepend_styled(format!("[^{}] ", label), theme.footnote());
            }
            for line in inner {
                let mut styled = Line::styled("  ", theme.footnote());
                for seg in line.segs {
                    let s = seg.style.patch(theme.footnote());
                    styled.push_styled(seg.text, s);
                }
                out.push(styled);
            }
        }
    }
}

fn push_indented(out: &mut Vec<Line>, lines: Vec<Line>, indent: usize) {
    let pad = " ".repeat(indent);
    for line in lines {
        let mut styled = Line::empty();
        if indent > 0 {
            styled.push_styled(pad.clone(), SegStyle::plain());
        }
        styled.segs.extend(line.segs);
        out.push(styled);
    }
}

fn render_code(text: &str, width: usize, theme: &Theme) -> Vec<Line> {
    let mut out = Vec::new();
    for line in text.split('\n') {
        if line.is_empty() {
            out.push(Line::empty());
            continue;
        }
        let line = truncate(line, width);
        out.push(Line::styled(format!("  {line}"), theme.code_block()));
    }
    out
}

fn render_list(
    theme: &Theme,
    items: &[ListItem],
    ordered: bool,
    start: u64,
    out: &mut Vec<Line>,
    width: usize,
    indent: usize,
) {
    let any_task = items.iter().any(|i| i.checked.is_some());
    let marker_w = if ordered {
        let max_num = start + items.len() as u64 - 1;
        max_num.to_string().len() + 2
    } else if any_task {
        4
    } else {
        2
    };
    for (i, item) in items.iter().enumerate() {
        let (marker, marker_w_used) = if let Some(checked) = item.checked {
            (if checked { "[x]" } else { "[ ]" }.to_string(), 3)
        } else if ordered {
            let text = format!("{}.", start + i as u64);
            (text.clone(), text.width())
        } else {
            ("•".to_string(), 1)
        };
        let marker_style = if item.checked.is_some() {
            if item.checked == Some(true) {
                theme.task_done()
            } else {
                theme.task_open()
            }
        } else {
            theme.list_marker()
        };

        let mut inner = Vec::new();
        for child in &item.children {
            render_into(
                child,
                &mut inner,
                width.saturating_sub(indent + marker_w),
                0,
                theme,
            );
        }
        if inner.is_empty() {
            let mut styled = Line::plain(" ".repeat(indent));
            styled.push_styled(marker, marker_style);
            out.push(styled);
            continue;
        }
        let mut styled = Line::plain(" ".repeat(indent));
        styled.push_styled(marker, marker_style);
        styled.push_styled(
            " ".repeat(marker_w.saturating_sub(marker_w_used)),
            SegStyle::plain(),
        );
        let first = inner.remove(0);
        styled.segs.extend(first.segs);
        out.push(styled);
        for line in inner {
            let mut cont = Line::plain(" ".repeat(indent + marker_w));
            cont.segs.extend(line.segs);
            out.push(cont);
        }
    }
}

fn render_table(
    theme: &Theme,
    headers: &[Vec<Inline>],
    rows: &[Vec<Vec<Inline>>],
    aligns: &[Alignment],
    out: &mut Vec<Line>,
    width: usize,
    indent: usize,
) {
    let available = width.saturating_sub(indent + 2);
    let mut cols = aligns.len().max(headers.len());
    for row in rows {
        cols = cols.max(row.len());
    }
    if cols == 0 {
        return;
    }
    // Plain-text width of each column
    let mut widths = vec![0usize; cols];
    for (c, cell) in headers.iter().enumerate() {
        widths[c] = plain_width(cell);
    }
    for row in rows {
        for (c, cell) in row.iter().enumerate() {
            widths[c] = widths[c].max(plain_width(cell));
        }
    }
    // Fit the available width
    let total: usize = widths.iter().sum::<usize>() + (cols.saturating_sub(1) * 3);
    if total > available {
        shrink_columns(&mut widths, available - (cols.saturating_sub(1) * 3));
    }

    let sep = |w: &[usize]| {
        let mut styled = Line::styled("│", theme.table_border());
        for (i, cw) in w.iter().enumerate() {
            styled.push_styled("─".repeat(*cw + 2), theme.table_border());
            styled.push_styled(
                if i + 1 == w.len() { "│" } else { "┼" },
                theme.table_border(),
            );
        }
        styled
    };

    let push_row =
        |out: &mut Vec<Line>, widths: &[usize], cells: &[Vec<Inline>], style: SegStyle| {
            let mut styled = Line::styled("│", theme.table_border());
            for (c, w) in widths.iter().enumerate() {
                let cell = cells.get(c);
                let text = cell.map(|c| plain_text(c)).unwrap_or_default();
                styled.push_styled(" ", SegStyle::plain());
                styled.push_styled(truncate(&text, *w), style);
                let pad = w.saturating_sub(text.width()) + 1;
                styled.push_styled(" ".repeat(pad), SegStyle::plain());
                styled.push_styled("│", theme.table_border());
            }
            out.push(styled);
        };

    push_row(out, &widths, headers, theme.table_header());
    out.push(sep(&widths));
    for row in rows {
        push_row(out, &widths, row, theme.text());
    }
    let _ = aligns;
}

fn shrink_columns(widths: &mut [usize], budget: usize) {
    let total: usize = widths.iter().sum();
    if total <= budget {
        return;
    }
    let n = widths.len();
    let per = total.saturating_sub(budget).div_ceil(n).max(4);
    for w in widths.iter_mut() {
        if *w > per {
            *w -= per;
        }
    }
    let remaining = budget.min(widths.iter().sum());
    // Hard cap
    let mut sum: usize = widths.iter().sum();
    while sum > remaining {
        let mut max_i = 0;
        for i in 0..n {
            if widths[i] > widths[max_i] {
                max_i = i;
            }
        }
        widths[max_i] = widths[max_i].saturating_sub(1);
        sum -= 1;
    }
    for w in widths.iter_mut() {
        *w = (*w).max(1);
    }
}

// ---------------------------------------------------------------------------
// Inline rendering + wrapping
// ---------------------------------------------------------------------------

/// Render inline content into wrapped styled lines.
pub fn render_inlines(inlines: &[Inline], width: usize, theme: &Theme) -> Vec<Line> {
    let mut segs: Vec<Seg> = Vec::new();
    collect_inlines(inlines, theme.text(), theme, &mut segs);
    wrap_segs(segs, width)
}

#[derive(Debug, Clone)]
struct Seg {
    text: String,
    style: SegStyle,
}

fn collect_inlines(inlines: &[Inline], style: SegStyle, theme: &Theme, out: &mut Vec<Seg>) {
    let mut i = 0usize;
    while i < inlines.len() {
        let inline = &inlines[i];
        // The parser emits the alt of `![alt](url)` as the Text immediately
        // before the image: a Text adjacent to an image is the alt itself, so
        // skip it (keep only `#[image]`).
        if matches!(inline, Inline::Text(_))
            && matches!(inlines.get(i + 1), Some(Inline::Image { .. }))
        {
            i += 1;
            continue;
        }
        match inline {
            Inline::Text(t) => out.push(Seg {
                text: t.clone(),
                style,
            }),
            Inline::SoftBreak => out.push(Seg {
                text: " ".to_string(),
                style,
            }),
            Inline::HardBreak => out.push(Seg {
                text: " ".to_string(),
                style,
            }),
            Inline::Code(c) => out.push(Seg {
                text: c.clone(),
                style: theme.code(),
            }),
            Inline::Strong(children) => {
                collect_inlines(children, style.patch(theme.strong()), theme, out)
            }
            Inline::Emphasis(children) => {
                collect_inlines(children, style.patch(theme.emphasis()), theme, out)
            }
            Inline::Strikethrough(children) => {
                collect_inlines(children, style.patch(theme.strikethrough()), theme, out)
            }
            Inline::Link { text, url } => {
                collect_inlines(text, style.patch(theme.link()), theme, out);
                out.push(Seg {
                    text: format!("({url})"),
                    style: style.patch(theme.dim()),
                });
            }
            Inline::Image { .. } => {
                out.push(Seg {
                    text: "#[image]".to_string(),
                    style: style.patch(theme.dim()),
                });
            }
            Inline::Math(m, _display) => {
                let content = crate::tui::math::latex_to_unicode(m);
                out.push(Seg {
                    text: content,
                    style: style.patch(theme.math()),
                });
            }
            Inline::Html(h) => out.push(Seg {
                text: h.clone(),
                style: style.patch(theme.dim()),
            }),
            Inline::FootnoteRef(l) => {
                out.push(Seg {
                    text: format!("[^{l}]"),
                    style: style.patch(theme.footnote()),
                });
            }
        }
        i += 1;
    }
}

/// Wrap by `width` display columns (CJK-aware), folding leading whitespace.
fn wrap_segs(segs: Vec<Seg>, width: usize) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    let mut current: Vec<Seg> = Vec::new();
    let mut col = 0usize;
    let mut leading = true;
    for seg in segs {
        let style = seg.style;
        for ch in seg.text.chars() {
            let w = char_width(ch);
            if ch == '\n' {
                push_line(&mut lines, &mut current);
                col = 0;
                leading = true;
                continue;
            }
            if col + w > width {
                push_line(&mut lines, &mut current);
                col = 0;
                leading = true;
            }
            if ch.is_whitespace() && leading {
                continue;
            }
            let mut owned = String::with_capacity(ch.len_utf8());
            owned.push(ch);
            current.push(Seg { text: owned, style });
            col += w;
            if !ch.is_whitespace() {
                leading = false;
            }
        }
    }
    push_line(&mut lines, &mut current);
    lines
}

/// Push `current` onto `lines`, dropping trailing whitespace segments.
fn push_line(lines: &mut Vec<Line>, current: &mut Vec<Seg>) {
    while let Some(seg) = current.last() {
        if seg.text.trim().is_empty() {
            current.pop();
        } else {
            break;
        }
    }
    if !current.is_empty() {
        lines.push(Line {
            segs: std::mem::take(current)
                .into_iter()
                .map(|s| crate::tui::line::Seg {
                    text: s.text,
                    style: s.style,
                })
                .collect(),
            image: None,
        });
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Flatten inline AST to plain text.
pub fn plain_text(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for i in inlines {
        match i {
            Inline::Text(t) => out.push_str(t),
            Inline::Code(c) => out.push_str(c),
            Inline::Strong(c) | Inline::Emphasis(c) | Inline::Strikethrough(c) => {
                out.push_str(&plain_text(c))
            }
            Inline::Link { text, .. } => out.push_str(&plain_text(text)),
            Inline::SoftBreak | Inline::HardBreak => out.push(' '),
            Inline::Image { alt, .. } => out.push_str(alt),
            Inline::Math(m, _) => out.push_str(m),
            Inline::Html(h) => out.push_str(h),
            Inline::FootnoteRef(l) => out.push_str(l),
        }
    }
    out
}

/// Display width of an inline run.
pub fn plain_width(inlines: &[Inline]) -> usize {
    plain_text(inlines).width()
}

/// Truncate a string to `width` display columns (CJK-aware).
pub fn truncate(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut col = 0usize;
    for ch in text.chars() {
        let w = char_width(ch);
        if col + w > width.saturating_sub(1) {
            break;
        }
        out.push(ch);
        col += w;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::Theme;
    use rsmarkdown_core::MarkdownProcessor;

    fn render_to_plain(md: &str, width: usize) -> Vec<String> {
        let mut processor = MarkdownProcessor::default();
        let mut renderer = MarkdownRenderer::with_theme(width, Theme::dark());
        let doc = processor.process_static(md);
        renderer.render(&doc);
        renderer.lines().iter().map(Line::plain_text).collect()
    }

    #[test]
    fn paragraph_wraps_cjk() {
        let lines = render_to_plain("ＡＢＣＤＥＦＧＨＩＪ", 6);
        assert_eq!(lines[0], "ＡＢＣ");
        assert_eq!(lines[1], "ＤＥＦ");
        assert_eq!(lines[2], "ＧＨＩ");
        assert_eq!(lines[3], "Ｊ");
    }

    #[test]
    fn inline_styles_apply() {
        let mut processor = MarkdownProcessor::default();
        let mut renderer = MarkdownRenderer::with_theme(80, Theme::dark());
        let doc = processor.process_static("**bold** and *it* and `code`");
        renderer.render(&doc);
        let line = &renderer.lines()[0];
        let bold: String = line
            .segs
            .iter()
            .filter(|s| s.style.bold)
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(bold, "bold");
        let code = line
            .segs
            .iter()
            .find(|s| s.style.fg == Some(Theme::dark().code_fg));
        assert!(code.is_some(), "code styled");
    }

    #[test]
    fn code_block_keeps_lines() {
        let lines = render_to_plain("```\nlet x = 1;\n```", 40);
        assert_eq!(lines[0], "  let x = 1;");
    }

    #[test]
    fn truncate_adds_ellipsis() {
        assert_eq!(truncate("hello", 3), "he…");
        assert_eq!(truncate("hi", 3), "hi");
        assert_eq!(truncate("ＡＢ", 3), "Ａ…");
    }

    #[test]
    fn image_placeholder_without_capability() {
        let lines = render_to_plain("![alt](a.png)", 40);
        assert_eq!(lines[0], "#[image]");
    }

    #[test]
    fn image_inline_placeholder_mid_paragraph() {
        let lines = render_to_plain("front ![a](a.png) back", 40);
        assert_eq!(lines[0], "front #[image] back");
    }

    #[test]
    fn loaded_image_renders_block_rows() {
        let mut processor = MarkdownProcessor::default();
        let mut renderer = MarkdownRenderer::with_theme(40, Theme::dark());
        let meta = Arc::new(ImageMeta {
            cols: 10,
            rows: 4,
            bytes: vec![1, 2, 3],
        });
        let mut images = HashMap::new();
        images.insert("a.png".to_string(), meta);
        renderer.set_images(Some(ImageCap::default_cells()), &images, &HashSet::new(), 1);
        let doc = processor.process_static("![alt](a.png)");
        renderer.render(&doc);
        let lines = renderer.lines().to_vec();
        assert_eq!(lines.len(), 4, "block occupies 4 rows");
        assert_eq!(lines[0].plain_text(), "#[image]");
        assert_eq!(
            lines[0].image,
            Some(ImageRef {
                url: "a.png".into(),
                cols: 10,
                rows: 4,
                row: 0
            })
        );
        assert_eq!(
            lines[3].image,
            Some(ImageRef {
                url: "a.png".into(),
                cols: 10,
                rows: 4,
                row: 3
            }),
            "continuation rows carry their own row index"
        );
        assert_eq!(lines[3].plain_text(), "");
    }

    /// A failed url renders the failure marker instead of the loading
    /// placeholder — the user can tell the picture is not coming.
    #[test]
    fn failed_image_renders_failure_marker() {
        let mut processor = MarkdownProcessor::default();
        let mut renderer = MarkdownRenderer::with_theme(40, Theme::dark());
        let mut failed = HashSet::new();
        failed.insert("a.png".to_string());
        renderer.set_images(Some(ImageCap::default_cells()), &HashMap::new(), &failed, 1);
        let doc = processor.process_static("![alt](a.png)");
        renderer.render(&doc);
        assert_eq!(renderer.lines()[0].plain_text(), "#[image ✗ load failed]");
        assert!(
            renderer.lines()[0].image.is_none(),
            "failed rows carry no image reference"
        );
    }

    #[test]
    fn image_not_loaded_falls_back_to_paragraph() {
        let mut processor = MarkdownProcessor::default();
        let mut renderer = MarkdownRenderer::with_theme(40, Theme::dark());
        renderer.set_images(
            Some(ImageCap::default_cells()),
            &HashMap::new(),
            &HashSet::new(),
            1,
        );
        let doc = processor.process_static("![alt](a.png)");
        renderer.render(&doc);
        assert_eq!(renderer.lines()[0].plain_text(), "#[image]");
        assert!(renderer.lines()[0].image.is_none());
    }

    #[test]
    fn image_block_invalidated_on_version_bump() {
        let mut processor = MarkdownProcessor::default();
        let mut renderer = MarkdownRenderer::with_theme(40, Theme::dark());
        renderer.set_images(
            Some(ImageCap::default_cells()),
            &HashMap::new(),
            &HashSet::new(),
            1,
        );
        let doc = processor.process_static("![alt](a.png)");
        renderer.render(&doc);
        assert!(
            renderer.lines()[0].image.is_none(),
            "not loaded → placeholder row"
        );

        let meta = Arc::new(ImageMeta {
            cols: 10,
            rows: 4,
            bytes: vec![1, 2, 3],
        });
        let mut images = HashMap::new();
        images.insert("a.png".to_string(), meta);
        renderer.set_images(Some(ImageCap::default_cells()), &images, &HashSet::new(), 2);
        renderer.render(&doc);
        assert!(
            renderer.lines()[0].image.is_some(),
            "rebuilt as an image block after the version changes"
        );
    }
}
