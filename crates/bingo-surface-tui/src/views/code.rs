//! `View::Code`: fenced, syntax-highlighted, never wrapped — it folds to the
//! width and opens in a sheet (design §5). The fence is the language on its own
//! row and a four-cell indent, which is what a fence in an answer already looks
//! like; past [`NUMBERED`] rows the indent becomes a line-number gutter, as a
//! `Read` result's is.
//!
//! A block whose language is a diff is a diff: it goes to the one unified-diff
//! renderer and wears its tints, so a patch reads the same whether a plugin
//! published it, a card previews it or an answer fenced it.

use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::theme;
use crate::views::clip;
use crate::{highlight, preview};

/// Rows from which a block is worth numbering.
const NUMBERED: usize = 8;
/// The indent of an unnumbered block, and the narrowest number gutter.
const GUTTER: usize = 4;

pub fn lines(lang: Option<&str>, text: &str, width: usize) -> Vec<Line<'static>> {
    let lang = lang.unwrap_or_default();
    if is_diff(lang) {
        return preview::diff(text);
    }
    let rows = highlight::lines(lang, text);
    let mut out: Vec<Line<'static>> = fence(lang, width).into_iter().collect();
    let gutter = gutter(rows.len());
    out.extend(
        rows.into_iter()
            .enumerate()
            .map(|(n, row)| line(gutter.then_some(n + 1), row, width)),
    );
    out
}

/// The two words that mean a block is a patch rather than a program.
fn is_diff(lang: &str) -> bool {
    matches!(lang, "diff" | "patch")
}

/// The language, dim, above the block; nothing when the plugin named none.
fn fence(lang: &str, width: usize) -> Option<Line<'static>> {
    if lang.is_empty() {
        return None;
    }
    Some(Line::from(Span::styled(
        clip(&format!("{}{lang}", " ".repeat(GUTTER)), width),
        theme::dim(),
    )))
}

/// Whether the block is long enough to number.
fn gutter(rows: usize) -> bool {
    rows > NUMBERED
}

fn line(number: Option<usize>, row: Line<'static>, width: usize) -> Line<'static> {
    let lead = match number {
        Some(n) => format!("{n:>GUTTER$}  "),
        None => " ".repeat(GUTTER),
    };
    let room = width.saturating_sub(lead.width());
    let mut spans = vec![Span::styled(lead, theme::dim())];
    spans.extend(cut(row, room));
    Line::from(spans)
}

/// A highlighted row, cut to the cells it has. Code never wraps (design §7):
/// what does not fit is elided, and the whole of it opens in the pager.
fn cut(row: Line<'static>, width: usize) -> Vec<Span<'static>> {
    if row
        .spans
        .iter()
        .map(|span| span.content.width())
        .sum::<usize>()
        <= width
    {
        return row.spans;
    }
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for span in row.spans {
        if used >= width {
            break;
        }
        let text = clip(&span.content, width - used);
        used += text.width();
        out.push(Span::styled(text, span.style));
    }
    out
}
