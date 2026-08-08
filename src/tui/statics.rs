//! The Static region: the transcript as an append-mostly log of blocks with a
//! frozen prefix — Ink's `<Static>` semantics, formalized.
//!
//! Ink guarantees stable scrollback by rendering `<Static>` items exactly once
//! above the dynamic region and never touching them again. The inline driver
//! here already carries that contract (write-once scrollback, D26/D27); this
//! module gives it a shape at the composition layer:
//!
//! - The transcript is a list of [`Block`]s (welcome card, one per message,
//!   trailing dialog/transient output). [`layout`] renders them into the
//!   [`Doc`] the frame and the flush path share.
//! - A block marked `settled` may freeze into scrollback. Freezing is *lazy*:
//!   a settled block stays in the live doc (re-wrappable on resize,
//!   collapsible) while it still fits the visible window, and freezes
//!   wholesale only once its start row crosses the window top
//!   ([`pick_flush_mark`]) — half-frozen would leave the hidden part neither
//!   on screen nor in scrollback. Frozen blocks are skipped by the next
//!   build; nothing above the viewport is ever repainted.
//! - Settlement is monotone and prefix-only: marks stop at the first
//!   unsettled block, so the flush cursor can only chase a stable prefix.
//!
//! The commit cursor itself (`flushed_segments` / `tail_start` / `mark_base`)
//! lives on [`crate::tui::chat::Chat`] next to the state that drives it;
//! [`SettledMark::segments`] counts blocks relative to the current skip so the
//! cursor advances by block boundary, never by row number (row numbers change
//! on every re-layout, block boundaries do not).

use crate::tui::el::{self, ClickRange, El, Row};

/// Scrollable document: all rows + click ranges.
///
/// In inline mode the document only covers the "not yet flushed" part (messages after `Chat::flushed_segments`),
/// so row numbers are not global — click targeting and scrolling are only used in fullscreen mode.
#[derive(Debug, Clone)]
pub struct Doc {
    pub rows: Vec<Row>,
    pub click_ranges: Vec<ClickRange>,
    /// Number of leading "settled" rows: rows that no longer change and can be printed
    /// into the terminal scrollback in one go (the print boundary for REPL mode; unused in fullscreen).
    /// Production uses `settled_marks` checkpoints; this aggregate is kept as the test-facing
    /// "settled prefix row count" handle.
    #[cfg_attr(not(test), allow(dead_code))]
    pub settled: usize,
    /// 定稿检查点（欢迎卡 / 每条定稿消息各一个，行号递增）：
    /// 懒落盘按检查点整段冻结，resize 回灌按检查点整段取回。
    pub settled_marks: Vec<SettledMark>,
    /// Number of transient rows at the end of the document (slash output, gone after TTL): lazy-flush
    /// window math must exclude them — a transient list shrinking the window is no reason to freeze live content.
    pub transient_rows: usize,
}

/// 一个定稿检查点：`row_end` 之前的行全部定稿。`segments` 是构建内
/// 累计值，跨多次 [`crate::tui::chat::Chat::advance_flushed_upto`] 的增量由
/// `Chat::mark_base` 消化。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettledMark {
    /// Row count covered by this checkpoint (exclusive end within doc.rows).
    pub row_end: usize,
    /// Message segments covered (build-internal accumulation, including the welcome card).
    pub segments: usize,
}

/// One transcript block: an element plus its Static-region role.
pub struct Block {
    pub el: El,
    /// Rows no longer change — the block may freeze into scrollback
    /// (write-once). Only a settled *prefix* produces checkpoints.
    pub settled: bool,
    /// Trailing transient output (slash lists): excluded from the lazy-flush
    /// window so it covers live content instead of evicting it.
    pub transient: bool,
}

impl Block {
    /// A block still changing (streaming message, dialog): never freezes.
    pub fn live(el: El) -> Self {
        Self {
            el,
            settled: false,
            transient: false,
        }
    }

    /// A block whose rows are final; `settled` still only takes effect inside
    /// a settled prefix.
    pub fn settled(el: El, settled: bool) -> Self {
        Self {
            el,
            settled,
            transient: false,
        }
    }

    /// Transient trailing output (slash lists, gone after TTL).
    pub fn transient(el: El) -> Self {
        Self {
            el,
            settled: false,
            transient: true,
        }
    }
}

/// Render the block list into the shared document. Click ranges come out of
/// the element walk (absolute, in resolution order); settled checkpoints stop
/// at the first unsettled block (prefix monotonicity).
pub fn layout(blocks: Vec<Block>) -> Doc {
    let mut rows: Vec<Row> = Vec::new();
    let mut click_ranges: Vec<ClickRange> = Vec::new();
    let mut settled_marks: Vec<SettledMark> = Vec::new();
    let mut settled = 0usize;
    let mut transient_rows = 0usize;
    let mut settled_blocks = 0usize;
    let mut prefix_settled = true;
    for block in blocks {
        let rendered = el::render(block.el);
        let base = rows.len();
        click_ranges.extend(rendered.clicks.into_iter().map(|c| ClickRange {
            start: base + c.start,
            end: base + c.end,
            target: c.target,
        }));
        if block.transient {
            transient_rows += rendered.rows.len();
        }
        rows.extend(rendered.rows);
        if prefix_settled && block.settled {
            settled_blocks += 1;
            settled = rows.len();
            settled_marks.push(SettledMark {
                row_end: settled,
                segments: settled_blocks,
            });
        } else {
            prefix_settled = false;
        }
    }
    Doc {
        rows,
        click_ranges,
        settled,
        settled_marks,
        transient_rows,
    }
}

/// Lazy-flush pick: the furthest settled checkpoint whose segment's start row has crossed the window top.
/// Fully visible settled segments stay unfrozen (kept re-layoutable/collapsible); a segment crossing the top
/// freezes wholesale — otherwise its hidden part exists neither on screen nor in scrollback, with nowhere to look.
pub fn pick_flush_mark(
    marks: &[SettledMark],
    tail_start: usize,
    win_start: usize,
) -> Option<SettledMark> {
    let mut chosen = None;
    let mut prev_end = tail_start;
    for mark in marks {
        if mark.row_end > tail_start && prev_end.max(tail_start) < win_start {
            chosen = Some(*mark);
        }
        prev_end = mark.row_end;
    }
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::el::{ClickTarget, LocalClick};
    use crate::tui::line::Line;

    fn block_of(texts: &[&str]) -> El {
        El::Lines(texts.iter().map(|t| Line::plain(*t)).collect())
    }

    #[test]
    fn layout_marks_only_the_settled_prefix() {
        let doc = layout(vec![
            Block::settled(block_of(&["welcome"]), true),
            Block::settled(block_of(&["m0a", "m0b"]), true),
            Block::settled(block_of(&["m1"]), false),
            // A settled block behind an unsettled one must not produce a mark
            // (freezing past a live block would print intermediate state).
            Block::settled(block_of(&["m2"]), true),
        ]);
        assert_eq!(doc.rows.len(), 5);
        assert_eq!(doc.settled, 3, "定稿前缀止于首个未定稿块");
        assert_eq!(
            doc.settled_marks,
            vec![
                SettledMark {
                    row_end: 1,
                    segments: 1
                },
                SettledMark {
                    row_end: 3,
                    segments: 2
                },
            ]
        );
        assert_eq!(doc.transient_rows, 0);
    }

    #[test]
    fn layout_counts_transient_rows_and_offsets_clicks() {
        let clickable = El::Annotated {
            rows: vec![Row::new(Line::plain("opt"))],
            clicks: vec![LocalClick {
                start: 0,
                end: 1,
                target: ClickTarget::AskOption(0),
            }],
        };
        let doc = layout(vec![
            Block::settled(block_of(&["a", "b"]), true),
            Block::live(clickable),
            Block::transient(block_of(&["s1", "s2", "s3"])),
        ]);
        assert_eq!(doc.rows.len(), 6);
        assert_eq!(doc.transient_rows, 3);
        assert_eq!(doc.click_ranges.len(), 1);
        assert_eq!(
            (doc.click_ranges[0].start, doc.click_ranges[0].end),
            (2, 3),
            "块内局部行号被摆放偏移修正"
        );
    }

    /// Lazy flush: nothing freezes when it fits in the window; segments past the window top (including ones crossing it) freeze wholesale.
    #[test]
    fn pick_flush_mark_freezes_only_segments_past_the_window_top() {
        let marks = vec![
            SettledMark {
                row_end: 5,
                segments: 1,
            },
            SettledMark {
                row_end: 20,
                segments: 2,
            },
        ];
        // Everything visible (window starts at 0): nothing freezes.
        assert_eq!(pick_flush_mark(&marks, 0, 0), None);
        // Window top at row 3: segment 1 (0..5) crosses the top → freeze up to 5; segment 2 from 5 on
        // is fully visible → stays live.
        assert_eq!(pick_flush_mark(&marks, 0, 3), Some(marks[0]));
        // Window top at row 10: segment 2 (5..20) also crosses → freeze it too.
        assert_eq!(pick_flush_mark(&marks, 0, 10), Some(marks[1]));
        // After freezing up to 5, the window has not moved further up: do not re-pick a consumed checkpoint.
        assert_eq!(pick_flush_mark(&marks, 5, 5), None);
    }
}
