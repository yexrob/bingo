//! Markdown to styled lines. Assistant text is re-parsed every frame, so a
//! half-written document renders as whatever it currently is; there is no
//! incremental state to keep in step with the stream.
//!
//! Logical lines come out unwrapped; [`crate::wrap`] fits them to the width.

use pulldown_cmark::{CodeBlockKind, CowStr, Event as Md, Options, Parser, Tag, TagEnd};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::theme;

/// Render CommonMark to lines. `width` is used only where a construct is
/// defined by the column count (the thematic break).
pub fn render(text: &str, width: usize) -> Vec<Line<'static>> {
    let mut out = Writer::new(width);
    for event in Parser::new_ext(text, Options::ENABLE_STRIKETHROUGH) {
        out.event(event);
    }
    out.finish()
}

/// A block's leading decoration: quote bars and list indentation.
#[derive(Clone, Debug, Default)]
struct Margin(Vec<Span<'static>>);

impl Margin {
    fn spans(&self) -> Vec<Span<'static>> {
        self.0.clone()
    }
}

struct Writer {
    width: usize,
    lines: Vec<Line<'static>>,
    spans: Vec<Span<'static>>,
    styles: Vec<Style>,
    margin: Margin,
    /// One counter per open list; `None` for a bullet list.
    lists: Vec<Option<u64>>,
    code: bool,
    /// The destination of the link being read, appended when it closes.
    link: Option<CowStr<'static>>,
}

impl Writer {
    fn new(width: usize) -> Self {
        Self {
            width,
            lines: Vec::new(),
            spans: Vec::new(),
            styles: vec![theme::text()],
            margin: Margin::default(),
            lists: Vec::new(),
            code: false,
            link: None,
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.flush();
        while self.lines.last().is_some_and(is_blank) {
            self.lines.pop();
        }
        self.lines
    }

    fn event(&mut self, event: Md<'_>) {
        match event {
            Md::Start(tag) => self.open(tag),
            Md::End(tag) => self.close(tag),
            Md::Text(text) if self.code => self.code_text(&text),
            Md::Text(text) => self.push(&text, self.style()),
            Md::Code(code) => self.push(&format!("`{code}`"), self.style()),
            Md::Html(html) | Md::InlineHtml(html) => self.push(html.trim_end(), theme::dim()),
            Md::SoftBreak => self.push(" ", self.style()),
            Md::HardBreak => self.flush(),
            Md::Rule => self.rule(),
            Md::TaskListMarker(done) => self.push(&format!("{} ", theme::todo(done)), theme::dim()),
            _ => {}
        }
    }

    fn open(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::CodeBlock(_) | Tag::Heading { .. } | Tag::Item => self.flush(),
            _ => {}
        }
        match tag {
            Tag::Heading { .. } => self.styles.push(theme::bold()),
            Tag::Emphasis => self.styles.push(theme::italic()),
            Tag::Strong => self.styles.push(theme::bold()),
            Tag::Strikethrough => self.styles.push(theme::struck()),
            Tag::Link { dest_url, .. } => {
                self.styles.push(theme::link());
                self.link = Some(CowStr::from(dest_url.into_string()));
            }
            Tag::CodeBlock(kind) => self.open_code(&kind),
            Tag::BlockQuote(_) => self
                .margin
                .0
                .push(Span::styled(format!("{} ", theme::wall()), theme::dim())),
            Tag::List(start) => self.lists.push(start),
            Tag::Item => self.marker(),
            _ => {}
        }
    }

    fn close(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Heading(_)
            | TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::Link => {
                self.styles.pop();
            }
            TagEnd::CodeBlock => self.code = false,
            TagEnd::BlockQuote(_) => {
                self.margin.0.pop();
            }
            TagEnd::Item => {
                self.margin.0.pop();
            }
            TagEnd::List(_) => {
                self.lists.pop();
            }
            _ => {}
        }
        if let TagEnd::Link = tag
            && let Some(url) = self.link.take()
        {
            self.push(&format!(" ({url})"), theme::dim());
        }
        if matches!(
            tag,
            TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::CodeBlock | TagEnd::Item
        ) {
            self.flush();
        }
        if matches!(
            tag,
            TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::CodeBlock | TagEnd::List(_)
        ) {
            self.blank();
        }
    }

    fn open_code(&mut self, kind: &CodeBlockKind<'_>) {
        self.code = true;
        if let CodeBlockKind::Fenced(lang) = kind
            && !lang.is_empty()
        {
            self.line(vec![Span::styled(format!("    {lang}"), theme::dim())]);
        }
    }

    /// A fenced block is emitted line by line: indented, dim, never wrapped
    /// into prose.
    fn code_text(&mut self, text: &str) {
        for line in text.trim_end_matches('\n').split('\n') {
            self.line(vec![Span::styled(format!("    {line}"), theme::dim())]);
        }
    }

    /// The bullet or number that opens a list item, plus the indent its
    /// continuation lines inherit.
    fn marker(&mut self) {
        let marker = match self.lists.last_mut() {
            Some(Some(n)) => {
                let marker = format!("{n}. ");
                *n += 1;
                marker
            }
            _ => format!("{} ", theme::point()),
        };
        let indent = " ".repeat(marker.chars().count());
        self.spans = self.margin.spans();
        self.spans.push(Span::styled(marker, theme::dim()));
        self.margin.0.push(Span::raw(indent));
    }

    fn rule(&mut self) {
        self.flush();
        self.line(vec![Span::styled(
            theme::rule().repeat(self.width.max(1)),
            theme::dim(),
        )]);
    }

    fn style(&self) -> Style {
        self.styles.last().copied().unwrap_or_default()
    }

    fn push(&mut self, text: &str, style: Style) {
        if self.spans.is_empty() {
            self.spans = self.margin.spans();
        }
        self.spans.push(Span::styled(text.to_string(), style));
    }

    /// Emit one finished line, decorated with the current margin.
    fn line(&mut self, spans: Vec<Span<'static>>) {
        let mut all = self.margin.spans();
        all.extend(spans);
        self.lines.push(Line::from(all));
    }

    fn flush(&mut self) {
        if self.spans.is_empty() {
            return;
        }
        let spans = std::mem::take(&mut self.spans);
        self.lines.push(Line::from(spans));
    }

    fn blank(&mut self) {
        if !self.lines.is_empty() && !self.lines.last().is_some_and(is_blank) {
            self.lines.push(Line::default());
        }
    }
}

fn is_blank(line: &Line<'static>) -> bool {
    line.spans.iter().all(|s| s.content.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Modifier;

    fn text(lines: &[Line<'static>]) -> Vec<String> {
        lines.iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn a_heading_is_bold_and_stands_alone() {
        let lines = render("# Title\n\nbody", 40);
        assert_eq!(text(&lines), vec!["Title", "", "body"]);
        assert!(
            lines[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
    }

    #[test]
    fn emphasis_and_strong_carry_their_modifiers() {
        let lines = render("plain *soft* **hard**", 40);
        let styles: Vec<_> = lines[0]
            .spans
            .iter()
            .map(|s| s.style.add_modifier)
            .collect();
        assert!(styles.iter().any(|m| m.contains(Modifier::ITALIC)));
        assert!(styles.iter().any(|m| m.contains(Modifier::BOLD)));
    }

    #[test]
    fn a_fence_is_indented_and_dim_line_by_line() {
        let lines = render("```rust\nfn main() {}\nlet x = 1;\n```", 40);
        assert_eq!(
            text(&lines),
            vec!["    rust", "    fn main() {}", "    let x = 1;"]
        );
        assert!(lines[1].spans[0].style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn an_inline_code_span_keeps_its_backticks_and_spends_no_colour() {
        let lines = render("call `run()` now", 40);
        assert_eq!(text(&lines), vec!["call `run()` now"]);
        assert_eq!(lines[0].spans[1].style, theme::text());
    }

    #[test]
    fn bullets_and_numbers_open_their_items() {
        assert_eq!(
            text(&render("- one\n- two", 40)),
            vec!["• one", "• two"],
            "a tight bullet list is one line per item"
        );
        assert_eq!(
            text(&render("1. one\n2. two", 40)),
            vec!["1. one", "2. two"]
        );
    }

    #[test]
    fn a_quote_gets_a_bar_in_the_margin() {
        assert_eq!(text(&render("> quoted", 40)), vec!["│ quoted"]);
    }

    #[test]
    fn a_link_shows_its_text_then_its_url() {
        assert_eq!(
            text(&render("see [docs](https://x.dev)", 40)),
            vec!["see docs (https://x.dev)"]
        );
    }

    #[test]
    fn a_rule_spans_the_width() {
        assert_eq!(text(&render("---", 8)), vec!["────────"]);
    }

    #[test]
    fn half_written_text_renders_as_what_it_is() {
        assert_eq!(text(&render("**bol", 40)), vec!["**bol"]);
    }
}
