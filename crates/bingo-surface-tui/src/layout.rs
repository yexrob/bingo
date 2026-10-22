//! A frame's geometry, cells, targets and next change, without a terminal.

use crate::blocks::Blocks;
use crate::clock::Now;
use crate::frame::Regions;
use crate::graphics::Placed;
use crate::tree::Tree;
use crate::ui::{Card, Ui};
use ratatui::layout::Rect;
use std::time::Instant;

mod canvas;
pub(crate) mod count;
mod deadline;
mod drawing;
pub mod grid;
pub use canvas::Canvas;

#[derive(Debug, Default)]
pub struct Layout {
    pub sheet_actions: Vec<crate::views::targets::Target>,
    pub sheet: Option<Vec<ratatui::text::Line<'static>>>,
    pub growth: Option<crate::dock::Growth>,
    pub card_actions: Vec<crate::views::targets::Target>,
    pub band_actions: Vec<crate::views::targets::Target>,
    pub seating: crate::pane::seat::Seating,
    pub waiting: Vec<(String, Rect)>,
    pub pulls: Vec<crate::pulled::Key>,
    pub blocks: Blocks,
    /// Header measurements move with the block memo, never through a shared UI borrow.
    pub measurements: crate::graphics::Decoded,
    pub canvas: Canvas,
    next_frame: Option<Instant>,
    #[cfg(test)]
    pub renders: [usize; 4],
    pub regions: Regions,
    /// The exact text cells of the composer in this frame, when it drew one.
    pub composer_text: Option<ratatui::layout::Rect>,
    /// The transcript's height in wrapped lines.
    pub height: usize,
    /// The first transcript line the frame showed.
    pub top: usize,
    /// The card on the screen, when one is open.
    pub card: Option<Card>,
    /// Where this frame drew each picture's cells, in the terminal's own rows
    /// and columns: what a click on a picture is answered against. It is read
    /// back out of the lines that were drawn
    /// ([`crate::graphics::placed::cells`]), so it can never say a picture is
    /// somewhere the screen does not (M56).
    pub cells: Vec<Placed>,
    /// Where this frame drew the way back to the foot ([`crate::pill`]), when
    /// the transcript was held: what a click there is answered against.
    pub foot: Option<ratatui::layout::Rect>,
}

impl Layout {
    pub fn below(&self) -> usize {
        self.height
            .saturating_sub(self.top + self.regions.transcript.height as usize)
    }

    pub fn wake(&mut self, next: Option<Instant>) {
        self.next_frame = self.next_frame.into_iter().chain(next).min();
    }
    pub fn deadline(&self) -> Option<Instant> {
        self.next_frame
    }

    pub fn collect<P: drawing::Part>(&mut self, drawing: P) -> P::Content {
        let (content, next) = drawing.split();
        self.wake(next);
        content
    }

    pub fn place(&mut self, drawing: crate::views::Drawing, area: Rect) {
        let lines = self.collect(drawing);
        self.render_widget(ratatui::widgets::Paragraph::new(lines), area);
    }

    /// The picture a pointer is on, when it is on one. The cells under it say
    /// which picture they are cells of, and this frame's own list says which
    /// picture that is — so a click can only ever open a picture the screen is
    /// showing (M56).
    pub fn picture_at(&self, at: ratatui::layout::Position) -> Option<crate::graphics::Picture> {
        let id = self.cells.iter().find(|one| one.area.contains(at))?.id;
        self.placed().into_iter().find(|picture| picture.id() == id)
    }

    /// Every picture in the rendered transcript, oldest first.
    pub fn placed(&self) -> Vec<crate::graphics::Picture> {
        self.blocks.pictures()
    }

    pub fn padding(&self) -> usize {
        usize::from(self.regions.transcript.height)
            .saturating_sub(self.height.saturating_sub(self.top))
    }

    /// The transcript line drawn at a row of the transcript region, when that
    /// row carries one. The one mapping from the screen back into the
    /// transcript: a click, a drag and a mark are all answered by it.
    pub fn line_at(&self, row: usize) -> Option<usize> {
        let line = self.top + row.checked_sub(self.padding())?;
        if line >= self.height {
            return None;
        }
        Some(line)
    }

    /// The transcript lines on screen, as runs of consecutive lines.
    pub fn visible_lines(&self, height: usize) -> Vec<std::ops::Range<usize>> {
        // An empty stream still exposes its insertion point for a row's arrival.
        if self.height == 0 && height > 0 {
            return std::iter::once(0..1).collect();
        }
        let mut ranges: Vec<std::ops::Range<usize>> = Vec::new();
        for line in (0..height).filter_map(|row| self.line_at(row)) {
            if let Some(last) = ranges.last_mut()
                && last.end == line
            {
                last.end += 1;
            } else {
                ranges.push(line..line + 1);
            }
        }
        ranges
    }

    /// The row of the transcript region a line was drawn at, when it is on the
    /// screen — [`Layout::line_at`] the other way round.
    pub fn row_of(&self, line: usize) -> Option<u16> {
        let row = u16::try_from(line.checked_sub(self.top)? + self.padding()).ok()?;
        (row < self.regions.transcript.height).then_some(row)
    }
}

impl std::ops::Deref for Layout {
    type Target = Canvas;
    fn deref(&self) -> &Canvas {
        &self.canvas
    }
}
impl std::ops::DerefMut for Layout {
    fn deref_mut(&mut self) -> &mut Canvas {
        &mut self.canvas
    }
}

/// Consume the previous memo; neither the tree nor the UI is changed.
pub fn layout(
    previous: Layout,
    tree: &Tree,
    ui: &Ui,
    size: Rect,
    now: Now,
    render: &crate::render::Render,
) -> Layout {
    render.scope(|| {
        compose(
            previous,
            tree,
            ui,
            size,
            Now {
                motion: render.motion,
                ..now
            },
        )
    })
}

fn compose(previous: Layout, tree: &Tree, ui: &Ui, size: Rect, now: Now) -> Layout {
    let mut next = Layout {
        growth: previous.growth,
        blocks: previous.blocks,
        measurements: previous.measurements,
        canvas: Canvas::new(size),
        ..Layout::default()
    };
    count::begin();
    crate::view::compose(tree, ui, &mut next, now);
    count::finish(&mut next);
    next.wake(deadline::next(tree, ui, &next, now));
    next
}

/// Painting has no geometry, state or clock to decide against.
pub fn paint(layout: &Layout, frame: &mut ratatui::Frame, render: &crate::render::Render) {
    render.scope(|| paint_cells(layout, frame));
}

fn paint_cells(layout: &Layout, frame: &mut ratatui::Frame) {
    // Terminal::draw has resized both buffers. A layout built before that
    // resize must not replace their area; leave a blank frame until rebuilt.
    if frame.area() != layout.canvas.area() {
        return;
    }
    frame.buffer_mut().clone_from(&layout.canvas.buffer);
    if let Some(cursor) = layout.canvas.cursor {
        frame.set_cursor_position(cursor);
    }
}

#[cfg(test)]
mod tests;
