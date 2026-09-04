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
    rendered(text, width).lines
}

/// The same rendering, with the pictures the words named beside it — for the
/// one caller that can draw them ([`crate::transcript`]). Every other caller
/// wants the lines alone, and gets the chip that names each picture in them.
pub fn rendered(text: &str, width: usize) -> Rendered {
    let mut out = Writer::new(width);
    for event in Parser::new_ext(text, GFM) {
        out.event(event);
    }
    out.finish()
}

/// A document as rows, and the pictures its own words named.
#[derive(Debug)]
pub struct Rendered {
    pub lines: Vec<Line<'static>>,
    /// In the order they were written.
    pub images: Vec<Linked>,
}

/// One `![what it is](path or URL)` the words carried: which line of
/// [`Rendered::lines`] holds its chip, what it is called, and where it is.
///
/// The destination is the word as it was written. Whether it names a file on
/// this machine or an address to fetch is nobody's business here — that is
/// read where the picture is ([`crate::graphics::linked`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Linked {
    pub line: usize,
    pub alt: String,
    pub dest: String,
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

/// An image being read: where it is, and the alt text collecting between its
/// tags.
#[derive(Debug)]
struct Alt {
    dest: String,
    text: String,
}

impl Alt {
    /// What a terminal that draws no picture shows for one: the name in the
    /// `[image: …]` [`crate::transcript::pictured`] already uses for a picture
    /// it cannot draw. A picture nobody named is named by where it is.
    fn chip(&self) -> String {
        let name = match self.text.trim() {
            "" => self.dest.as_str(),
            named => named,
        };
        format!("[image: {name}]")
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
    /// The image being read; while there is one, text is its name.
    image: Option<Alt>,
    /// The images read since the last line was emitted: a picture stands on a
    /// line of its own, after the words it was written among.
    pending: Vec<Alt>,
    /// Whether anything but the line's own decoration has been written to it.
    /// A list item whose whole content is a picture has a marker and no
    /// words, and its chip belongs on the marker's line ([`Writer::chips`]).
    words: bool,
    images: Vec<Linked>,
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
            image: None,
            pending: Vec::new(),
            words: false,
            images: Vec::new(),
        }
    }

    fn finish(mut self) -> Rendered {
        self.flush();
        while self.lines.last().is_some_and(is_blank) {
            self.lines.pop();
        }
        // A trailing blank line cannot be a chip's, so nothing dropped above
        // can leave an image pointing at a line that is no longer there.
        Rendered {
            lines: self.lines,
            images: self.images,
        }
    }

    fn event(&mut self, event: Md<'_>) {
        if self.table.is_some()
            && let Some(text) = flat_text(&event)
        {
            if let Some(table) = self.table.as_mut() {
                table.text(&text);
            }
            return;
        }
        if let Some(image) = self.image.as_mut()
            && let Some(text) = flat_text(&event)
        {
            image.text.push_str(&text);
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
            Tag::Image { dest_url, .. } => {
                self.image = Some(Alt {
                    dest: dest_url.into_string(),
                    text: String::new(),
                })
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
            TagEnd::Image => self.end_image(),
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
        self.words = true;
        self.spans.push(Span::styled(text.to_string(), style));
    }

    /// The image is whole: it waits for the line it was written among to be
    /// emitted, and takes the one after it ([`Writer::chips`]).
    fn end_image(&mut self) {
        if let Some(image) = self.image.take() {
            self.pending.push(image);
        }
    }

    /// One line per image read since the last flush, each carrying the chip
    /// that names it and each remembered by the line it landed on. A line
    /// that has only its own decoration on it — the marker of a list item
    /// whose whole content is the picture — keeps it and takes the chip.
    fn chips(&mut self) {
        for image in std::mem::take(&mut self.pending) {
            self.images.push(Linked {
                line: self.lines.len(),
                alt: image.text.trim().to_string(),
                dest: image.dest.clone(),
            });
            let mut spans = match self.spans.is_empty() {
                true => self.margin.spans(),
                false => std::mem::take(&mut self.spans),
            };
            spans.push(Span::styled(image.chip(), theme::dim()));
            self.lines.push(Line::from(spans));
        }
    }

    /// Emit one finished line, decorated with the current margin.
    fn line(&mut self, spans: Vec<Span<'static>>) {
        let mut all = self.margin.spans();
        all.extend(spans);
        self.lines.push(Line::from(all));
    }

    fn flush(&mut self) {
        if !self.spans.is_empty() && (self.words || self.pending.is_empty()) {
            let spans = std::mem::take(&mut self.spans);
            self.lines.push(Line::from(spans));
        }
        self.words = false;
        self.chips();
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

/// What an event contributes to a run of prose read as plain text: a table's
/// cell, and the name between an image's own tags. A table's cells are text —
/// emphasis inside one changes no column width, and a rule is what says these
/// rows are one table (design §5) — and an alt text is a name, which is the
/// same thing said of a shorter run.
fn flat_text(event: &Md<'_>) -> Option<String> {
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

    /// The picture the words named stands on a line of its own, after the
    /// words it was written among — so what replaces its chip hangs under a
    /// line of its own too.
    #[test]
    fn an_image_in_a_paragraph_takes_the_line_after_it() {
        let out = rendered("look at ![the shot](docs/x.png) here", 40);
        assert_eq!(text(&out.lines), vec!["look at  here", "[image: the shot]"]);
        assert_eq!(
            out.images,
            vec![Linked {
                line: 1,
                alt: "the shot".into(),
                dest: "docs/x.png".into(),
            }]
        );
        assert_eq!(out.lines[1].spans[0].style, theme::dim(), "the chip is dim");
    }

    #[test]
    fn an_image_alone_is_its_own_line_and_nothing_else() {
        let out = rendered("![shot](x.png)", 40);
        assert_eq!(text(&out.lines), vec!["[image: shot]"]);
        assert_eq!(
            out.images.iter().map(|i| i.line).collect::<Vec<_>>(),
            vec![0]
        );
    }

    /// A reference definition and an angled destination are both spellings of
    /// the same thing, and the destination that comes out is the resolved one
    /// rather than the label or the brackets.
    #[test]
    fn a_reference_and_a_bracketed_destination_resolve_to_the_path() {
        let out = rendered("![a plan][plan]\n\n[plan]: docs/plan.png", 40);
        assert_eq!(text(&out.lines)[0], "[image: a plan]");
        assert_eq!(out.images[0].dest, "docs/plan.png");

        let spaced = rendered("![two words](<my shots/a b.png>)", 40);
        assert_eq!(spaced.images[0].dest, "my shots/a b.png");
        assert_eq!(text(&spaced.lines), vec!["[image: two words]"]);
    }

    /// A picture nobody named is named by where it is: an empty alt would
    /// otherwise draw `[image: ]` and say nothing at all.
    #[test]
    fn a_picture_with_no_name_wears_its_destination() {
        let out = rendered("![](https://x.dev/a.png)", 40);
        assert_eq!(text(&out.lines), vec!["[image: https://x.dev/a.png]"]);
        assert_eq!(out.images[0].alt, "");
    }

    /// A list item whose whole content is a picture keeps its bullet: the
    /// chip takes the marker's line rather than leaving it empty.
    #[test]
    fn a_bulleted_picture_stands_on_its_bullet() {
        let out = rendered("- ![one](a.png)\n- ![two](b.png)", 40);
        assert_eq!(text(&out.lines), vec!["• [image: one]", "• [image: two]"]);
        assert_eq!(
            out.images.iter().map(|i| i.line).collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert_eq!(
            text(&rendered("- see ![one](a.png)", 40).lines),
            vec!["• see ", "[image: one]"],
            "and an item with words of its own puts them first"
        );
    }

    /// Two pictures in one paragraph are two lines, in the order they were
    /// written, each knowing which line is its own.
    #[test]
    fn two_pictures_in_one_paragraph_keep_their_order() {
        let out = rendered("![one](a.png) and ![two](b.png)", 40);
        assert_eq!(
            text(&out.lines),
            vec![" and ", "[image: one]", "[image: two]"]
        );
        assert_eq!(
            out.images,
            vec![
                Linked {
                    line: 1,
                    alt: "one".into(),
                    dest: "a.png".into()
                },
                Linked {
                    line: 2,
                    alt: "two".into(),
                    dest: "b.png".into()
                },
            ]
        );
    }

    /// Everything a document without a picture renders to, byte for byte:
    /// this milestone put a branch in the writer's every line, and the one
    /// thing it must not have done is move a row of somebody's answer.
    #[test]
    fn a_document_with_no_picture_renders_exactly_as_it_did() {
        const DOC: &str = "# Title\n\nSome *prose* with `code`, a [link](https://x.dev) and a \
soft\nbreak.\n\n- one\n- two\n  - nested\n\n1. first\n2. second\n\n> quoted\n> on two lines\n\n\
```rust\nfn main() {}\n```\n\n| crate | tests |\n|---|---|\n| sdk | 41 |\n\n---\n\nlast word.";
        let out = rendered(DOC, 40);
        assert!(out.images.is_empty());
        insta::assert_snapshot!("markdown_without_pictures", text(&out.lines).join("\n"));
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
