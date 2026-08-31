//! Markdown to styled lines. Assistant text is re-parsed every frame, so a
//! half-written document renders as whatever it currently is; there is no
//! incremental state to keep in step with the stream.
//!
//! Logical lines come out unwrapped; [`crate::wrap`] fits them to the width.

use pulldown_cmark::{CodeBlockKind, CowStr, Event as Md, Options, Parser, Tag, TagEnd};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::{theme, views};

/// What this renderer understands beyond CommonMark: GFM's strikethrough and
/// its tables, which go to the one table renderer (design §5).
const GFM: Options = Options::ENABLE_STRIKETHROUGH.union(Options::ENABLE_TABLES);

/// Render CommonMark to lines. `width` is used where a construct is defined by
/// the column count: the thematic break, and a table's columns.
pub fn render(text: &str, width: usize) -> Vec<Line<'static>> {
    let mut out = Writer::new(width);
    for event in Parser::new_ext(text, GFM) {
        out.event(event);
    }
    out.finish()
}

/// A GFM table being read. It holds cells rather than lines because the table
/// is laid out only once it is whole — a column's width is a fact about every
/// row of it.
#[derive(Debug, Default)]
struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    row: Vec<String>,
    cell: String,
    /// The row being read is the header row.
    heading: bool,
}

impl Table {
    fn text(&mut self, text: &str) {
        self.cell.push_str(text);
    }

    fn end_cell(&mut self) {
        let cell = std::mem::take(&mut self.cell);
        self.row.push(cell.trim().to_string());
    }

    fn end_row(&mut self) {
        let row = std::mem::take(&mut self.row);
        match std::mem::take(&mut self.heading) {
            true => self.headers = row,
            false => self.rows.push(row),
        }
    }

    /// The ruled rows, from the renderer a plugin's `View::Table` uses.
    fn lines(&self, width: usize) -> Vec<Line<'static>> {
        views::table::lines(&self.headers, &self.rows, width)
    }
}

/// A block's leading decoration: quote bars and list indentation.
#[derive(Clone, Debug, Default)]
struct Margin(Vec<Span<'static>>);

impl Margin {
    fn spans(&self) -> Vec<Span<'static>> {
        self.0.clone()
    }
}

/// A fenced block being read: the word after its backticks and its text.
#[derive(Debug)]
struct Fence {
    lang: String,
    text: String,
}

impl Fence {
    fn lines(&self, width: usize) -> Vec<Line<'static>> {
        views::code::lines(Some(&self.lang), &self.text, width)
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
    /// The fenced block being read; it is laid out only once it is whole, so
    /// the highlighter sees the block and not one line of it at a time.
    code: Option<Fence>,
    /// The table being read; while there is one, text is a cell and not prose.
    table: Option<Table>,
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
            code: None,
            table: None,
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
        if self.table.is_some()
            && let Some(text) = cell_text(&event)
        {
            if let Some(table) = self.table.as_mut() {
                table.text(&text);
            }
            return;
        }
        match event {
            Md::Start(tag) => self.open(tag),
            Md::End(tag) => self.close(tag),
            Md::Text(text) if self.code.is_some() => self.code_text(&text),
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
            Tag::CodeBlock(_) | Tag::Heading { .. } | Tag::Item | Tag::Table(_) => self.flush(),
            _ => {}
        }
        match tag {
            // The alignment row is not read: which column is numbers is a fact
            // about the cells, and the one table renderer already knows it.
            Tag::Table(_) => self.table = Some(Table::default()),
            Tag::TableHead => self.heading(),
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
            TagEnd::TableCell => self.end_cell(),
            TagEnd::TableHead | TagEnd::TableRow => self.end_row(),
            TagEnd::Table => self.end_table(),
            _ => {}
        }
        match tag {
            TagEnd::Heading(_)
            | TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::Link => {
                self.styles.pop();
            }
            TagEnd::CodeBlock => self.end_code(),
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
            TagEnd::Paragraph
                | TagEnd::Heading(_)
                | TagEnd::CodeBlock
                | TagEnd::List(_)
                | TagEnd::Table
        ) {
            self.blank();
        }
    }

    fn heading(&mut self) {
        if let Some(table) = self.table.as_mut() {
            table.heading = true;
        }
    }

    fn end_cell(&mut self) {
        if let Some(table) = self.table.as_mut() {
            table.end_cell();
        }
    }

    fn end_row(&mut self) {
        if let Some(table) = self.table.as_mut() {
            table.end_row();
        }
    }

    /// The table is whole: lay it out and let the cells go.
    fn end_table(&mut self) {
        let Some(table) = self.table.take() else {
            return;
        };
        for line in table.lines(self.width) {
            self.line(line.spans);
        }
    }

    fn open_code(&mut self, kind: &CodeBlockKind<'_>) {
        let lang = match kind {
            CodeBlockKind::Fenced(lang) => lang.to_string(),
            CodeBlockKind::Indented => String::new(),
        };
        self.code = Some(Fence {
            lang,
            text: String::new(),
        });
    }

    fn code_text(&mut self, text: &str) {
        if let Some(fence) = self.code.as_mut() {
            fence.text.push_str(text);
        }
    }

    /// The block is whole: through the one code renderer, so a fence in an
    /// answer is drawn exactly like a plugin's `View::Code` (design §5).
    fn end_code(&mut self) {
        let Some(fence) = self.code.take() else {
            return;
        };
        for line in fence.lines(self.width) {
            self.line(line.spans);
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

/// What an event contributes to the cell being read. A table's cells are text:
/// emphasis inside one changes no column width, and a rule is what says these
/// rows are one table (design §5).
fn cell_text(event: &Md<'_>) -> Option<String> {
    match event {
        Md::Text(text) => Some(text.to_string()),
        Md::Code(code) => Some(format!("`{code}`")),
        Md::SoftBreak | Md::HardBreak => Some(" ".to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Modifier;
    use unicode_width::UnicodeWidthStr;

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
    fn a_fence_is_indented_and_highlighted_line_by_line() {
        let lines = render("```rust\nfn main() {}\n// go\n```", 40);
        assert_eq!(
            text(&lines),
            vec!["    rust", "    fn main() {}", "    // go"]
        );
        assert_eq!(lines[0].spans[0].style, theme::dim(), "the fence's word");
        assert_eq!(lines[1].spans[1].style, theme::mode(), "`fn` is a keyword");
        assert_eq!(lines[2].spans[1].style, theme::dim(), "the comment");
    }

    #[test]
    fn a_fence_that_names_a_diff_is_a_diff() {
        let lines = render("```diff\n@@ -1 +1 @@\n-old\n+new\n```", 40);
        assert_eq!(text(&lines), vec!["@@ -1 +1 @@", "-old", "+new"]);
        assert_eq!(lines[1].spans[0].style, theme::removed());
        assert_eq!(lines[2].spans[0].style, theme::added());
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

    const TABLE: &str = "| crate | tests |\n|---|---|\n| sdk | 41 |\n| core | 7 |";

    #[test]
    fn a_table_is_ruled_and_its_numbers_hug_the_right_edge() {
        assert_eq!(
            text(&render(TABLE, 40)),
            vec![
                "crate  tests".to_string(),
                "────────────".to_string(),
                "sdk       41".to_string(),
                "core       7".to_string(),
            ],
        );
    }

    #[test]
    fn a_tables_header_is_bold_and_its_rule_is_dim() {
        let lines = render(TABLE, 40);
        assert_eq!(lines[0].spans[0].style, theme::text().patch(theme::bold()));
        assert_eq!(lines[1].spans[0].style, theme::dim());
    }

    #[test]
    fn a_cell_a_row_has_not_is_marked_and_emphasis_in_one_is_plain_text() {
        assert_eq!(
            text(&render("| a | b |\n|---|---|\n| *one* |\n", 40)),
            vec![
                "a    b".to_string(),
                "──────".to_string(),
                "one  –".to_string()
            ],
        );
    }

    #[test]
    fn a_table_wider_than_the_measure_folds_to_it() {
        let wide = format!("| {0} | {0} |\n|---|---|\n| {0} | {0} |", "x".repeat(20));
        for line in render(&wide, 24) {
            assert!(line.to_string().width() <= 24, "{line}");
        }
    }

    #[test]
    fn a_table_stands_apart_from_the_prose_around_it() {
        assert_eq!(
            text(&render(&format!("before\n\n{TABLE}\n\nafter"), 40)),
            vec![
                "before".to_string(),
                String::new(),
                "crate  tests".to_string(),
                "────────────".to_string(),
                "sdk       41".to_string(),
                "core       7".to_string(),
                String::new(),
                "after".to_string(),
            ],
        );
    }
}
