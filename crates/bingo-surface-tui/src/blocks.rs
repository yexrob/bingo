//! The transcript as blocks: one rendered block per item, kept between
//! frames.
//!
//! The reducer is still the only history — a block is a *memo* of
//! [`crate::transcript`]'s rendering of one item at one width, thrown away the
//! moment either changes, never a second copy of what the item says. An item
//! that has finished is drawn once and then only cloned; the one that is still
//! arriving is the only one that is drawn again, because it is the only one
//! that can differ. A call that spawned a session is drawn again whenever the
//! child's state moves, since its row is read from that state.
//!
//! Scrolling asks for a window of lines, which is why the blocks are kept
//! rather than the flat transcript: a window is walked in blocks and cut to
//! the row, so what it hands back is always an exact slice of the whole.

use std::collections::BTreeSet;

use bingo_sdk::{Item, ItemBody, ItemId, ItemStatus, Seq, SessionState};
use ratatui::text::Line;

use crate::transcript::{self, Rows};
use crate::tree::Agents;
use crate::welcome;

/// What can change about an item's block while it is on the screen. A
/// terminal item's revision never moves again, so its block is rendered once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Revision {
    status: ItemStatus,
    /// How much the body holds. Anything that grows a block grows this: the
    /// text of an answer, a tail, an output.
    size: usize,
    /// Where the child this call spawned is, when it spawned one: its row is
    /// read from the child's state, so the child's seq is the row's revision.
    agent: Option<Seq>,
    /// Opened whole with `ctrl+o`.
    expanded: bool,
}

fn revision(item: &Item, agent: Option<&SessionState>, expanded: bool) -> Revision {
    Revision {
        status: item.status,
        size: size(&item.body),
        agent: agent.map(|child| child.seq),
        expanded,
    }
}

fn size(body: &ItemBody) -> usize {
    match body {
        ItemBody::Assistant { text } => text.len(),
        ItemBody::Reasoning { text, .. } => text.len(),
        ItemBody::ToolCall {
            output, progress, ..
        } => {
            output.as_ref().map(|o| o.parts.len() + 1).unwrap_or(0)
                + progress.as_ref().map(String::len).unwrap_or(0)
        }
        ItemBody::Action { result, .. } => result.is_some() as usize,
        _ => 0,
    }
}

struct Entry {
    id: ItemId,
    revision: Revision,
    /// A receipt hangs from the row above it: no blank row before it.
    joins: bool,
    lines: Vec<Line<'static>>,
}

/// One run of lines in the stacked transcript: the welcome box, an item's
/// block, or the failed turn's line — with whether a blank row goes before it
/// and whose it is.
struct Segment<'a> {
    gap: bool,
    id: Option<&'a ItemId>,
    lines: &'a [Line<'static>],
}

/// The rendered transcript of one session at one width. The blocks are kept
/// in transcript order and matched to the items by position, so a frame that
/// changed nothing costs one comparison per item and not one allocation.
#[derive(Default)]
pub struct Blocks {
    width: usize,
    /// The welcome box, on a session this surface opened; it belongs to no item.
    head: Vec<Line<'static>>,
    blocks: Vec<Entry>,
    /// The transcript's height in wrapped lines, counted while the blocks are
    /// brought up to date rather than walked for again.
    height: usize,
    /// The failed turn's line, which belongs to no item.
    tail: Vec<Line<'static>>,
    /// How many blocks have been drawn since this cache was made. A test
    /// watches it: scrolling must not move it.
    renders: usize,
}

impl Blocks {
    /// Bring the cache up to date with the state and answer with the height of
    /// the whole transcript, in wrapped lines.
    pub fn sync(
        &mut self,
        state: &SessionState,
        agents: &Agents<'_>,
        width: usize,
        expanded: &BTreeSet<ItemId>,
    ) -> usize {
        if self.width != width {
            self.blocks.clear();
            self.width = width;
        }
        let rows = Rows {
            cwd: &state.summary.cwd,
            width,
            expanded,
        };
        self.head = welcome::lines(state, width);
        let mut kept = 0;
        let mut previous: Option<&Item> = None;
        for item in &state.items {
            kept += self.block(kept, item, previous, agents, &rows);
            previous = Some(item);
        }
        // Whatever is left behind the last item was rewound away.
        self.blocks.truncate(kept);
        self.tail = transcript::failure(state, &rows);
        self.height = self.measure();
        self.height
    }

    /// Keep the block at `at` when it is still this item's, else draw it.
    /// Answers with how many blocks the item now occupies: one, or none when
    /// it has nothing to say.
    fn block(
        &mut self,
        at: usize,
        item: &Item,
        previous: Option<&Item>,
        agents: &Agents<'_>,
        rows: &Rows<'_>,
    ) -> usize {
        let agent = agents.get(&item.id).copied();
        let revision = revision(item, agent, rows.expanded.contains(&item.id));
        let same = self
            .blocks
            .get(at)
            .is_some_and(|entry| entry.id == item.id && entry.revision == revision);
        if same && item.is_terminal() {
            return 1;
        }
        self.renders += 1;
        let lines = transcript::item_lines(item, previous, agents, rows);
        if lines.is_empty() {
            // An item with nothing to say is not a block at all.
            if same {
                self.blocks.remove(at);
            }
            return 0;
        }
        let entry = Entry {
            id: item.id.clone(),
            revision,
            joins: transcript::joins_the_row_above(item),
            lines,
        };
        match self.blocks.get_mut(at) {
            Some(slot) if slot.id == item.id => *slot = entry,
            _ => self.blocks.insert(at, entry),
        }
        1
    }

    /// Where an item's block sits in the transcript: its first line, and the
    /// line just after it — where a card the item asked for hangs from.
    /// `None` when the item has no block.
    pub fn span(&self, item: &ItemId) -> Option<(usize, usize)> {
        let mut y = 0;
        for segment in self.segments() {
            y += usize::from(segment.gap);
            if segment.id == Some(item) {
                return Some((y, y + segment.lines.len()));
            }
            y += segment.lines.len();
        }
        None
    }

    /// The item whose block holds a transcript line, which is what a click in
    /// the transcript lands on.
    pub fn at(&self, line: usize) -> Option<ItemId> {
        let mut y = 0;
        for segment in self.segments() {
            y += usize::from(segment.gap);
            if (y..y + segment.lines.len()).contains(&line) {
                return segment.id.cloned();
            }
            y += segment.lines.len();
        }
        None
    }

    /// The whole transcript's height in wrapped lines, as the last [`sync`]
    /// counted it.
    ///
    /// [`sync`]: Blocks::sync
    pub fn height(&self) -> usize {
        self.height
    }

    fn measure(&self) -> usize {
        self.segments()
            .map(|segment| usize::from(segment.gap) + segment.lines.len())
            .sum()
    }

    /// The lines `[from, from + height)` of the whole transcript.
    pub fn window(&self, from: usize, height: usize) -> Vec<Line<'static>> {
        let mut out: Vec<Line<'static>> = Vec::with_capacity(height);
        let mut y = 0;
        for segment in self.segments() {
            let rows = usize::from(segment.gap) + segment.lines.len();
            if y + rows <= from {
                y += rows;
                continue;
            }
            if segment.gap {
                if y >= from {
                    out.push(Line::default());
                }
                y += 1;
            }
            for line in segment.lines {
                if out.len() == height {
                    return out;
                }
                if y >= from {
                    out.push(line.clone());
                }
                y += 1;
            }
            if out.len() == height {
                return out;
            }
        }
        out
    }

    /// The last `rows` lines as plain text: what is printed back into the
    /// shell's own screen when the surface leaves (design §3).
    pub fn tail(&self, rows: usize) -> Vec<String> {
        let from = self.height().saturating_sub(rows);
        self.window(from, rows)
            .iter()
            .map(|line| line.to_string().trim_end().to_string())
            .collect()
    }

    #[cfg(test)]
    pub fn renders(&self) -> usize {
        self.renders
    }

    /// Every segment in transcript order. A blank row separates a segment
    /// from whatever is above it — never from the top of the transcript, and
    /// never a receipt from the row it answers.
    fn segments(&self) -> impl Iterator<Item = Segment<'_>> {
        let head = (!self.head.is_empty()).then_some(Segment {
            gap: false,
            id: None,
            lines: self.head.as_slice(),
        });
        let mut above = !self.head.is_empty();
        let blocks = self.blocks.iter().map(move |entry| {
            let gap = above && !entry.joins;
            above = true;
            Segment {
                gap,
                id: Some(&entry.id),
                lines: entry.lines.as_slice(),
            }
        });
        let tail = (!self.tail.is_empty()).then_some(Segment {
            gap: !(self.head.is_empty() && self.blocks.is_empty()),
            id: None,
            lines: self.tail.as_slice(),
        });
        head.into_iter().chain(blocks).chain(tail)
    }
}

impl std::fmt::Debug for Blocks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Blocks")
            .field("width", &self.width)
            .field("blocks", &self.blocks.len())
            .field("renders", &self.renders)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use bingo_sdk::{Event, ItemStatus};
    use std::time::{Duration, Instant};

    fn cache() -> Blocks {
        Blocks::default()
    }

    fn many(n: usize) -> SessionState {
        let mut state = state();
        state.items = (0..n)
            .map(|i| {
                assistant(
                    &format!("itm_{i}"),
                    &format!("Answer {i} — a line of prose that wraps a little."),
                    ItemStatus::Completed,
                )
            })
            .collect();
        state
    }

    fn sync(blocks: &mut Blocks, state: &SessionState, width: usize) -> usize {
        blocks.sync(state, &Agents::new(), width, &BTreeSet::new())
    }

    /// The rows the welcome box takes at the top, plus the blank under it.
    fn head(state: &SessionState, width: usize) -> usize {
        let rows = welcome::lines(state, width).len();
        rows + usize::from(rows > 0)
    }

    #[test]
    fn a_finished_item_is_drawn_once_and_scrolling_draws_nothing() {
        let state = many(5_000);
        let mut blocks = cache();
        let started = Instant::now();
        let height = sync(&mut blocks, &state, 80);
        let warm = started.elapsed();
        assert_eq!(blocks.renders(), 5_000);
        assert!(
            warm < Duration::from_millis(200),
            "five thousand blocks warmed in {warm:?}"
        );

        for from in (0..height).step_by(97) {
            blocks.window(from, 40);
        }
        sync(&mut blocks, &state, 80);
        assert_eq!(
            blocks.renders(),
            5_000,
            "a transcript that did not change is not drawn again"
        );
    }

    #[test]
    fn a_width_change_drops_every_block_once() {
        let state = many(20);
        let mut blocks = cache();
        sync(&mut blocks, &state, 80);
        sync(&mut blocks, &state, 100);
        assert_eq!(blocks.renders(), 40, "a new width is a new rendering");
        sync(&mut blocks, &state, 100);
        assert_eq!(blocks.renders(), 40, "and only one");
    }

    #[test]
    fn the_item_that_is_still_arriving_is_the_only_one_drawn_again() {
        let mut state = many(3);
        state
            .items
            .push(assistant("itm_live", "half a s", ItemStatus::Running));
        let mut blocks = cache();
        sync(&mut blocks, &state, 80);
        assert_eq!(blocks.renders(), 4);
        sync(&mut blocks, &state, 80);
        assert_eq!(blocks.renders(), 5, "one block, not four");
    }

    #[test]
    fn a_window_is_an_exact_slice_of_the_whole_transcript() {
        let state = many(40);
        let mut blocks = cache();
        let height = sync(&mut blocks, &state, 60);
        let whole: Vec<String> = blocks
            .window(0, height)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(whole.len(), height);
        for (from, rows) in [(0usize, 5usize), (1, 5), (7, 13), (height - 3, 9)] {
            let window: Vec<String> = blocks
                .window(from, rows)
                .iter()
                .map(ToString::to_string)
                .collect();
            let want = &whole[from..(from + rows).min(height)];
            assert_eq!(window, want, "window from {from}");
        }
    }

    #[test]
    fn a_block_knows_which_lines_are_its_own() {
        let state = many(3);
        let mut blocks = cache();
        sync(&mut blocks, &state, 60);
        let first = ItemId::from_raw("itm_0");
        let second = ItemId::from_raw("itm_1");
        let top = head(&state, 60);
        assert_eq!(blocks.span(&first), Some((top, top + 1)));
        assert_eq!(
            blocks.span(&second),
            Some((top + 2, top + 3)),
            "past the blank row"
        );
        assert_eq!(blocks.at(top), Some(first));
        assert_eq!(blocks.at(top + 1), None, "the blank row is nobody's");
        assert_eq!(blocks.at(top + 2), Some(second));
        assert_eq!(blocks.at(999), None);
    }

    #[test]
    fn a_blank_row_separates_the_blocks_and_never_opens_the_transcript() {
        let state = many(2);
        let mut blocks = cache();
        let height = sync(&mut blocks, &state, 60);
        let lines: Vec<String> = blocks
            .window(0, height)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert!(!lines[0].trim().is_empty(), "{lines:?}");
        let under_the_welcome = usize::from(head(&state, 60) > 0);
        assert_eq!(
            lines.iter().filter(|l| l.trim().is_empty()).count(),
            1 + under_the_welcome
        );
    }

    #[test]
    fn a_dropped_item_takes_its_block_with_it() {
        let mut state = many(4);
        let mut blocks = cache();
        sync(&mut blocks, &state, 60);
        state.items.truncate(2);
        sync(&mut blocks, &state, 60);
        assert_eq!(
            blocks.blocks.len(),
            2,
            "the cache is never larger than the transcript"
        );
    }

    #[test]
    fn a_failed_turn_keeps_its_line_at_the_foot_of_the_transcript() {
        let state = folded(vec![
            frame(
                1,
                Event::ItemCompleted {
                    item: user("itm_1", "go"),
                },
            )
            .clone(),
        ]);
        let mut with_failure = state.clone();
        with_failure.last_turn = Some(bingo_sdk::TurnStatus::Failed {
            error: bingo_sdk::KernelError::new(bingo_sdk::ErrorCode::Internal, "boom"),
        });
        let mut blocks = cache();
        let plain = sync(&mut blocks, &state, 60);
        let failed = sync(&mut blocks, &with_failure, 60);
        assert_eq!(failed, plain + 2);
        assert!(blocks.tail(1)[0].contains("boom"), "{:?}", blocks.tail(1));
    }

    #[test]
    fn the_tail_is_the_last_rows_as_plain_text() {
        let state = many(10);
        let mut blocks = cache();
        let height = sync(&mut blocks, &state, 60);
        let tail = blocks.tail(4);
        assert_eq!(tail.len(), 4);
        let whole = blocks.window(0, height);
        assert_eq!(tail[3], whole[height - 1].to_string().trim_end());
        assert!(tail[3].contains("Answer 9"), "{tail:?}");
    }
}
