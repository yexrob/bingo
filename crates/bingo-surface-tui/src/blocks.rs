//! The transcript as blocks: one rendered block per item, kept between
//! frames.
//!
//! The reducer is still the only history — a block is a *memo* of
//! [`crate::transcript`]'s rendering of one item at one width, thrown away the
//! moment either changes, never a second copy of what the item says. An item
//! that has finished is drawn once and then only cloned; the one that is still
//! arriving is the only one that is drawn again, because it is the only one
//! that can differ.
//!
//! Scrolling asks for a window of lines, which is why the blocks are kept
//! rather than the flat transcript: a window is walked in blocks and cut to
//! the row, so what it hands back is always an exact slice of the whole.

use std::collections::HashMap;

use bingo_sdk::{Item, ItemBody, ItemId, ItemStatus, SessionState};
use ratatui::text::Line;

use crate::tree::Agents;
use crate::{transcript, wrap};

/// What can change about an item's block while it is on the screen. A
/// terminal item's revision never moves again, so its block is rendered once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Revision {
    status: ItemStatus,
    /// How much the body holds. Anything that grows a block grows this: the
    /// text of an answer, a tail, an output.
    size: usize,
    /// The `↳` row under a call that spawned a session, when there is one.
    agent: usize,
}

fn revision(item: &Item, agent: Option<&String>) -> Revision {
    Revision {
        status: item.status,
        size: size(&item.body),
        agent: agent.map(String::len).unwrap_or(0),
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
    revision: Revision,
    lines: Vec<Line<'static>>,
}

/// The rendered transcript of one session at one width.
#[derive(Default)]
pub struct Blocks {
    width: usize,
    /// The items that have a block, in transcript order.
    order: Vec<ItemId>,
    blocks: HashMap<ItemId, Entry>,
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
        agents: &Agents,
        width: usize,
        spinner: &str,
    ) -> usize {
        if self.width != width {
            self.blocks.clear();
            self.width = width;
        }
        self.order.clear();
        for item in &state.items {
            self.block(item, agents.get(&item.id), spinner);
        }
        self.forget_the_dropped();
        self.tail = transcript::failure(state);
        self.height()
    }

    /// Render one item, or keep the block that is already right.
    fn block(&mut self, item: &Item, agent: Option<&String>, spinner: &str) {
        let revision = revision(item, agent);
        let known = self.blocks.get(&item.id).map(|e| e.revision);
        if item.is_terminal() && known == Some(revision) {
            self.order.push(item.id.clone());
            return;
        }
        let lines = self.render(item, agent, spinner);
        if lines.is_empty() {
            // An item with nothing to say is not a block, and caching it would
            // make the cache larger than the transcript for ever.
            self.blocks.remove(&item.id);
            return;
        }
        self.blocks
            .insert(item.id.clone(), Entry { revision, lines });
        self.order.push(item.id.clone());
    }

    fn render(&mut self, item: &Item, agent: Option<&String>, spinner: &str) -> Vec<Line<'static>> {
        self.renders += 1;
        let mut block = transcript::item_lines(item, self.width, spinner);
        if block.is_empty() {
            return block;
        }
        if let Some(agent) = agent {
            block.push(transcript::child_line(agent));
        }
        wrap::wrap_all(&block, self.width)
    }

    /// A rewind or a compaction takes items away; their blocks go with them.
    fn forget_the_dropped(&mut self) {
        if self.blocks.len() <= self.order.len() {
            return;
        }
        let live: std::collections::HashSet<&ItemId> = self.order.iter().collect();
        self.blocks.retain(|id, _| live.contains(id));
    }

    /// The whole transcript's height in wrapped lines.
    pub fn height(&self) -> usize {
        self.segments()
            .map(|(gap, lines)| usize::from(gap) + lines.len())
            .sum()
    }

    /// The lines `[from, from + height)` of the whole transcript.
    pub fn window(&self, from: usize, height: usize) -> Vec<Line<'static>> {
        let mut out: Vec<Line<'static>> = Vec::with_capacity(height);
        let mut y = 0;
        for (gap, lines) in self.segments() {
            let rows = usize::from(gap) + lines.len();
            if y + rows <= from {
                y += rows;
                continue;
            }
            if gap {
                if y >= from {
                    out.push(Line::default());
                }
                y += 1;
            }
            for line in lines {
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
    #[cfg(test)]
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

    /// Every block in transcript order, each with whether a blank row goes
    /// before it. A block is separated from the one above it, never from the
    /// top of the transcript.
    fn segments(&self) -> impl Iterator<Item = (bool, &[Line<'static>])> {
        self.order
            .iter()
            .enumerate()
            .filter_map(|(i, id)| self.blocks.get(id).map(|e| (i > 0, e.lines.as_slice())))
            .chain(
                (!self.tail.is_empty()).then_some((!self.order.is_empty(), self.tail.as_slice())),
            )
    }
}

impl std::fmt::Debug for Blocks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Blocks")
            .field("width", &self.width)
            .field("blocks", &self.order.len())
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
        blocks.sync(state, &Agents::new(), width, "⠋")
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
        assert_eq!(lines.iter().filter(|l| l.trim().is_empty()).count(), 1);
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
