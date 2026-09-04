//! The transcript as blocks: one rendered block per item, kept between
//! frames.
//!
//! The reducer is still the only history — a block is a *memo* of
//! [`crate::transcript`]'s rendering of one item at one width in one look,
//! thrown away the moment any of the three changes, never a second copy of
//! what the item says. An item
//! that has finished is drawn once and then only cloned; the one that is still
//! arriving is the only one that is drawn again, because it is the only one
//! that can differ. A call that spawned a session is drawn again whenever the
//! child's state moves, since its row is read from that state.
//!
//! Scrolling asks for a window of lines, which is why the blocks are kept
//! rather than the flat transcript: a window is walked in blocks and cut to
//! the row, so what it hands back is always an exact slice of the whole.

use std::time::{Duration, Instant};

use bingo_sdk::{Item, ItemBody, ItemId, ItemStatus, Seq, SessionState};
use ratatui::text::Line;

use crate::clock::Now;
use crate::fold::{self, Fold};
use crate::graphics::Picture;
use crate::skill;
use crate::transcript::{self, Cue, Rows};
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
    /// How much of the block is shown: what `ctrl+o` and a click both set, and
    /// the start its kind has until they do.
    fold: Fold,
    /// Whether the catalogue calls this run a skill, which decides the row's
    /// mark and its signature ([`crate::skill`]). The catalogue is fetched
    /// after the first frames are drawn and read again whenever the host
    /// rebuilds it, so a block drawn before it landed has to be drawn again.
    skill: bool,
    /// How many pictures the words named have been read in so far (M51). A
    /// block that drew a chip for one is a different block once it has
    /// arrived, and the arrival is the only thing that says so — it is not in
    /// the item, which has not changed a byte.
    linked: u64,
    /// The look it was drawn in. A style is baked into a `Line`, and the
    /// terminal may change which palette a token is worth under a running
    /// surface (M71): the lines are then a memo of a drawing nobody would make
    /// again. It is here rather than beside the width because a look changes
    /// what a block *says* about itself and not what it is — the entry, and the
    /// landing it is in the middle of, are kept and only drawn again.
    look: crate::theme::Theme,
}

fn revision(item: &Item, agent: Option<&SessionState>, fold: Fold, rows: &Rows<'_>) -> Revision {
    Revision {
        status: item.status,
        size: size(&item.body),
        agent: agent.map(|child| child.seq),
        fold,
        skill: skill::of(item, rows.commands).is_some(),
        linked: rows.linked.answers(),
        look: crate::theme::current(),
    }
}

/// How much of the body is on the screen. Anything that grows a block grows
/// this: the text of an answer, a tail, an output — and a thought's text, which
/// M34-B left out because the row said `✻ Thinking…` whatever the deltas
/// carried and nothing under it was drawn from them. A thought now streams its
/// tail under that row (§6), so a delta does change the rows and the block has
/// to be drawn again for it.
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

/// Where a block is in its own motion (§6). A block is drawn from the item and
/// the clock alone; this is the clock's half — when this rendering arrived,
/// when it stops changing, whether it arrived by finishing, and whether what
/// is held is a frame of that motion rather than its rest.
#[derive(Clone, Copy, Debug)]
struct Motion {
    since: Instant,
    until: Instant,
    flipped: bool,
    moved: bool,
}

impl Motion {
    fn new(since: Instant, settles: Duration, flipped: bool) -> Self {
        Self {
            since,
            until: since + settles,
            flipped,
            moved: !settles.is_zero(),
        }
    }

    fn moving(&self, now: Instant) -> bool {
        now < self.until
    }

    /// Whether the block has to be drawn again: it is still moving, or it has
    /// stopped and what is held is the last frame of the motion rather than
    /// the resting form that replaces it.
    fn redraw(&self, now: Instant) -> bool {
        self.moving(now) || self.moved
    }

    /// The same motion, as of a rendering taken at `now`. A rendering taken
    /// after the motion has stopped *is* the resting form, so it pays off the
    /// frame [`Motion::redraw`] was owed; a debt never marked paid keeps the
    /// whole surface awake for the rest of the run. It is the same sample the
    /// cue is drawn from, so a block can never come to rest while the frame
    /// held of it is still a flashing one.
    fn drawn(self, now: Instant) -> Self {
        Self {
            moved: self.moving(now),
            ..self
        }
    }

    fn cue(&self, now: Instant) -> Cue {
        Cue {
            since: self.since,
            flip: self.flipped && self.moving(now),
        }
    }
}

/// How long a block goes on changing after it is drawn: the frames a landed
/// call's own words move for, the comet tail of an answer still arriving, and
/// no time at all for anything that is simply there.
fn settles(item: &Item, flip: bool, moving: bool) -> Duration {
    if !moving {
        return Duration::ZERO;
    }
    if flip {
        return transcript::LANDING;
    }
    match (&item.body, item.status) {
        (ItemBody::Assistant { .. }, ItemStatus::Running) => transcript::COMET,
        _ => Duration::ZERO,
    }
}

struct Entry {
    id: ItemId,
    revision: Revision,
    /// A receipt hangs from the row above it: no blank row before it.
    joins: bool,
    lines: Vec<Line<'static>>,
    /// The pictures these lines stand for (§5). They are kept with the lines
    /// because they are the same rendering: a block drawn once and cloned
    /// ever after would otherwise forget, on its second frame, what its own
    /// placeholder cells are placeholders for.
    pictures: Vec<Picture>,
    /// The destinations these words named (M51), for the same reason.
    wanted: Vec<String>,
    motion: Motion,
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
    /// The live cards, where there is no rail to put them in (ADR-0013 §2):
    /// they belong to no item either, and they sit under the running rows.
    live: Vec<Line<'static>>,
    /// How many blocks have been drawn since this cache was made. A test
    /// watches it: scrolling must not move it.
    renders: usize,
    /// Whether any block would draw differently on the next frame, as the
    /// last sync left it.
    moving: bool,
}

impl Blocks {
    /// Bring the cache up to date with the state and answer with the height of
    /// the whole transcript, in wrapped lines.
    pub fn sync(
        &mut self,
        state: &SessionState,
        agents: &Agents<'_>,
        rows: &Rows<'_>,
        live: Vec<Line<'static>>,
    ) -> usize {
        if self.width != rows.width {
            self.blocks.clear();
            self.width = rows.width;
        }
        let boxed = welcome::lines(state, rows.width, rows.update);
        // The opening plays in the welcome box's place and lands on it, so while
        // it runs the box *is* the frame (M70, M72; design §11).
        self.head = match rows.opening {
            Some(t) => crate::opening::frame(t, &boxed),
            None => boxed,
        };
        let mut kept = 0;
        let mut previous: Option<&Item> = None;
        for item in &state.items {
            kept += self.block(kept, item, previous, agents, rows);
            previous = Some(item);
        }
        // Whatever is left behind the last item was rewound away.
        self.blocks.truncate(kept);
        self.tail = transcript::failure(state, rows);
        self.live = live;
        self.moving = self.still_moving(rows.now);
        self.height = self.measure();
        self.height
    }

    /// Whether any block would draw differently on the next frame, as of this
    /// one.
    fn still_moving(&self, now: Now) -> bool {
        self.blocks
            .iter()
            .any(|entry| entry.motion.redraw(now.instant))
    }

    /// Whether any block would draw differently on the next frame, as the last
    /// [`sync`] left it: a tail cooling, or a completion flashing.
    ///
    /// [`sync`]: Blocks::sync
    pub fn moving(&self) -> bool {
        self.moving
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
        let now = rows.now.instant;
        let agent = agents.get(&item.id).copied();
        let revision = revision(item, agent, fold::fold_of(rows.folds, item), rows);
        let held = self.blocks.get(at).filter(|entry| entry.id == item.id);
        let same = held.is_some_and(|entry| entry.revision == revision);
        // An item that has only just finished is not yet terminal for this
        // cache: it has a frame of flashing left to do, and a frame after that
        // to settle into.
        if same && item.is_terminal() && !held.is_some_and(|e| e.motion.redraw(now)) {
            return 1;
        }
        let motion = self.motion(held, item, &revision, rows.now);
        self.renders += 1;
        let drawn = transcript::item_block(item, previous, agents, rows, motion.cue(now));
        if drawn.lines.is_empty() {
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
            lines: drawn.lines,
            pictures: drawn.pictures,
            wanted: drawn.wanted,
            motion,
        };
        match self.blocks.get_mut(at) {
            Some(slot) if slot.id == item.id => *slot = entry,
            _ => self.blocks.insert(at, entry),
        }
        1
    }

    /// The clock this rendering is drawn against: the one it already had while
    /// nothing about it changed — settled against this frame, which is the one
    /// paying off whatever it still owed — and a new one the moment it did.
    fn motion(&self, held: Option<&Entry>, item: &Item, revision: &Revision, now: Now) -> Motion {
        let was = held.filter(|entry| &entry.revision == revision);
        if let Some(entry) = was {
            return entry.motion.drawn(now.instant);
        }
        let finished = held.is_some_and(|entry| !entry.revision.status.is_terminal())
            && item.status.is_terminal();
        Motion::new(now.instant, settles(item, finished, now.motion), finished)
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

    /// Every picture the transcript is holding, in transcript order: what the
    /// terminal is asked to hold ([`crate::graphics::Stored`]). Derived from
    /// the blocks rather than remembered beside them, so a rewind that drops
    /// an item drops its picture with it.
    pub fn pictures(&self) -> Vec<Picture> {
        self.blocks
            .iter()
            .flat_map(|entry| entry.pictures.iter().cloned())
            .collect()
    }

    /// Every destination the transcript's own words named (M51), for the run
    /// to read in the ones this session has not got
    /// ([`crate::graphics::linked`]). Derived from the blocks for the same
    /// reason [`Blocks::pictures`] is: a rewind that drops an item stops
    /// anything being fetched for it.
    pub fn wanted(&self) -> Vec<String> {
        self.blocks
            .iter()
            .flat_map(|entry| entry.wanted.iter().cloned())
            .collect()
    }

    /// The last `rows` lines as plain text: what is printed back into the
    /// shell's own screen when the surface leaves (design §3).
    ///
    /// A picture's cells are not text. Printed into the shell's own screen
    /// they would be a row of glyphs no font has, standing for a picture the
    /// terminal has already been told to forget — so those rows are left
    /// behind with the alternate screen they were drawn on.
    pub fn tail(&self, rows: usize) -> Vec<String> {
        let height = self.height();
        let from = height.saturating_sub(rows);
        self.window(from, height - from)
            .iter()
            .map(|line| line.to_string().trim_end().to_string())
            .filter(|line| !line.contains(crate::graphics::kitty::PLACEHOLDER))
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
        let opened = !(self.head.is_empty() && self.blocks.is_empty());
        let tail = (!self.tail.is_empty()).then_some(Segment {
            gap: opened,
            id: None,
            lines: self.tail.as_slice(),
        });
        let live = (!self.live.is_empty()).then_some(Segment {
            gap: opened || !self.tail.is_empty(),
            id: None,
            lines: self.live.as_slice(),
        });
        head.into_iter().chain(blocks).chain(tail).chain(live)
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
        sync_at(blocks, state, width, scene().1)
    }

    fn sync_at(blocks: &mut Blocks, state: &SessionState, width: usize, now: Now) -> usize {
        let folds = crate::fold::Folds::new();
        let pictures = crate::graphics::Decoded::default();
        let linked = crate::graphics::Linked::default();
        let rows = Rows::of(state, width, &folds, &[], &pictures, &linked, now);
        blocks.sync(state, &Agents::new(), &rows, Vec::new())
    }

    /// The rows the welcome box takes at the top, plus the blank under it.
    fn head(state: &SessionState, width: usize) -> usize {
        let rows = welcome::lines(state, width, None).len();
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

    /// A block is drawn once and cloned ever after, so a picture read in
    /// after its block was drawn would never reach the screen unless the
    /// answer's arrival were part of what a block is a rendering of (M51).
    #[test]
    fn a_picture_read_in_after_its_block_makes_it_draw_again() {
        let state = many(3);
        let mut blocks = cache();
        let folds = crate::fold::Folds::new();
        let pictures = crate::graphics::Decoded::default();
        let mut linked = crate::graphics::Linked::default();
        let now = scene().1;
        let sync = |blocks: &mut Blocks, linked: &crate::graphics::Linked| {
            let rows = Rows::of(&state, 80, &folds, &[], &pictures, linked, now);
            blocks.sync(&state, &Agents::new(), &rows, Vec::new());
        };
        sync(&mut blocks, &linked);
        sync(&mut blocks, &linked);
        assert_eq!(blocks.renders(), 3, "nothing changed, nothing drawn again");

        assert!(linked.take("x.png"));
        sync(&mut blocks, &linked);
        assert_eq!(blocks.renders(), 3, "a read in flight changes no row");

        linked.answered(crate::graphics::linked::Answer {
            dest: "x.png".into(),
            result: Err("not found".into()),
        });
        sync(&mut blocks, &linked);
        assert_eq!(blocks.renders(), 6, "the answer landing draws them again");
        sync(&mut blocks, &linked);
        assert_eq!(blocks.renders(), 6, "and only once");
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

    /// The flash owes every frame it lasts, and one more: the frame that
    /// replaces it with the resting form. Owing for longer keeps the whole
    /// surface awake for the rest of the run; owing for less leaves the
    /// flashed rendering on the screen with nothing coming to relieve it.
    #[test]
    fn a_flash_owes_frames_until_its_resting_form_is_drawn() {
        let mut state = state();
        state.items = vec![assistant("itm_1", "half a s", ItemStatus::Running)];
        let mut blocks = cache();
        let (_, now) = scene();
        sync_at(&mut blocks, &state, 60, now);
        assert!(blocks.moving(), "an answer still arriving is moving");

        // It finishes, so its own words move for the frames after (§6).
        state.items = vec![assistant("itm_1", "half a second", ItemStatus::Completed)];
        sync_at(&mut blocks, &state, 60, now);
        assert!(blocks.moving(), "the completion is still landing");
        sync_at(&mut blocks, &state, 60, later(now, 16));
        assert!(blocks.moving(), "and has not run out halfway through");

        // Past the landing: this sync draws the rest, and owes nothing after.
        let landed = transcript::LANDING.as_millis() as i64;
        sync_at(&mut blocks, &state, 60, later(now, landed));
        assert!(
            !blocks.moving(),
            "the resting form is the frame just drawn: nothing more is owed"
        );
        let settled = blocks.renders();
        sync_at(&mut blocks, &state, 60, later(now, 5_000));
        assert_eq!(
            blocks.renders(),
            settled,
            "and a block at rest is never drawn again"
        );
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
