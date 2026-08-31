//! `View::Code`: fenced, dim, never wrapped — it folds to the width and opens
//! in a sheet (design §5). The fence is the language on its own row and a
//! four-cell indent, which is what a fence in an answer already looks like;
//! past [`NUMBERED`] rows the indent becomes a line-number gutter, as a
//! `Read` result's is. Highlighting is M11e's.

use ratatui::text::{Line, Span};

use crate::theme;
use crate::views::clip;

/// Rows from which a block is worth numbering.
const NUMBERED: usize = 8;
/// The indent of an unnumbered block, and the narrowest number gutter.
const GUTTER: usize = 4;

pub fn lines(lang: Option<&str>, text: &str, width: usize) -> Vec<Line<'static>> {
    let rows: Vec<&str> = text.trim_end_matches('\n').split('\n').collect();
    let mut out: Vec<Line<'static>> = fence(lang, width).into_iter().collect();
    let gutter = gutter(rows.len());
    out.extend(
        rows.iter()
            .enumerate()
            .map(|(n, row)| line(gutter.then_some(n + 1), row, width)),
    );
    out
}

/// The language, dim, above the block; nothing when the plugin named none.
fn fence(lang: Option<&str>, width: usize) -> Option<Line<'static>> {
    let lang = lang.filter(|lang| !lang.is_empty())?;
    Some(Line::from(Span::styled(
        clip(&format!("{}{lang}", " ".repeat(GUTTER)), width),
        theme::dim(),
    )))
}

/// Whether the block is long enough to number.
fn gutter(rows: usize) -> bool {
    rows > NUMBERED
}

fn line(number: Option<usize>, row: &str, width: usize) -> Line<'static> {
    let lead = match number {
        Some(n) => format!("{n:>GUTTER$}  "),
        None => " ".repeat(GUTTER),
    };
    Line::from(vec![
        Span::styled(lead.clone(), theme::dim()),
        Span::styled(clip(row, width.saturating_sub(lead.len())), theme::text()),
    ])
}
