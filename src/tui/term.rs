//! Inline terminal driver.
//!
//! This is the only module in the crate that is allowed to touch the cursor position, scroll
//! regions or screen clearing. Everything else renders into a [`Buffer`] or hands finalized rows
//! to [`InlineTerm::insert_history`].
//!
//! The architecture mirrors `ratatui::Terminal::insert_before` (with the `scrolling-regions`
//! feature) and the custom terminal used by codex-rs:
//!
//! - Finalized rows are pushed into the terminal's own scrollback exactly once and never touched
//!   again. Nothing above the viewport is ever repainted, so terminal reflow on resize behaves
//!   like it does for ordinary shell output.
//! - Only a small bottom-anchored viewport is repainted, through a double-buffered diff that emits
//!   a clear-to-end-of-line after the last non-blank cell instead of painting trailing spaces.
//! - Every mutation is wrapped in one synchronized-update batch (CSI ?2026h / CSI ?2026l) followed
//!   by a single flush.
//! - The visible screen is never purged: no `Clear(All) + Purge` path exists here.

use std::io::Write as IoWrite;

use ratatui::backend::{Backend, ClearType, CrosstermBackend};
use ratatui::buffer::{Buffer, Cell, CellWidth};
use ratatui::layout::{Position, Rect, Size};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// CSI ? 2026 h — begin synchronized update.
const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";
/// CSI ? 2026 l — end synchronized update.
const SYNC_END: &[u8] = b"\x1b[?2026l";

/// Raw byte access on top of a [`Backend`].
///
/// Two concerns need bytes the `Backend` trait cannot express: synchronized-update bracketing and
/// [`HistoryItem::Raw`] payloads (terminal graphics escape sequences). Backends opt in by
/// implementing this trait; the bytes are written verbatim at the current cursor position.
pub trait RawWrite: Backend {
    /// Write `bytes` to the terminal verbatim, without interpretation.
    fn write_raw(&mut self, bytes: &[u8]) -> Result<(), Self::Error>;

    /// Scroll rows `[0, top)` up by `n`, pushing the rows that leave the top
    /// of the screen into the terminal's scrollback.
    ///
    /// kitty-family terminals (kitty, Ghostty) send `CSI S` scrolls to the
    /// bit bucket, never to scrollback; line feeds at the bottom of a
    /// top-anchored DECSTBM region are the only portable scrollback push, so
    /// the real backend must emit that form instead of
    /// [`Backend::scroll_region_up`].
    fn scroll_into_scrollback(&mut self, top: u16, n: u16) -> Result<(), Self::Error>;
}

impl<W: IoWrite> RawWrite for CrosstermBackend<W> {
    fn write_raw(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.write_all(bytes)
    }

    fn scroll_into_scrollback(&mut self, top: u16, n: u16) -> std::io::Result<()> {
        // DECSTBM ignores regions of fewer than two rows (top must be < bottom
        // in 1-based terms), and a subsequent scroll would then hit the whole
        // screen. `top < 2` only happens on terminals of one or two rows —
        // dropping the scroll there beats corrupting the viewport.
        if top < 2 || n == 0 {
            return Ok(());
        }
        // DECSTBM is 1-based inclusive: the region covers screen rows
        // [1, top]. Park the cursor on the region's bottom row and let `n`
        // line feeds scroll the region up. DECSTBM homes the cursor; the
        // driver marks `cursor_synced = false` at every call site and
        // repositions before the next write.
        let mut seq = format!("\x1b[1;{top}r\x1b[{top};1H").into_bytes();
        seq.resize(seq.len() + usize::from(n), b'\n');
        seq.extend_from_slice(b"\x1b[r");
        self.write_all(&seq)
    }
}

/// One unit of scrollback output.
#[derive(Debug, Clone)]
pub enum HistoryItem {
    /// Exactly one pre-wrapped terminal row.
    ///
    /// The display width is expected to be at most the viewport width; the driver truncates on
    /// display width as a safety net and never wraps.
    Line(Line<'static>),
    /// Raw bytes written verbatim with the cursor at column 0 of the current insertion row.
    ///
    /// `rows` is how many terminal rows the payload occupies after being written (0 = occupies
    /// none, e.g. a kitty `a=T` transmission with no placement; N = e.g. a `C=1` kitty image that
    /// advances N rows). Those rows are reserved but never cleared by the driver.
    Raw {
        /// Escape sequence payload.
        bytes: Vec<u8>,
        /// Terminal rows consumed by the payload.
        rows: u16,
    },
}

impl HistoryItem {
    /// Terminal rows this item occupies once written.
    fn rows(&self) -> u16 {
        match self {
            Self::Line(_) => 1,
            Self::Raw { rows, .. } => *rows,
        }
    }
}

/// A bottom-anchored inline viewport driver.
///
/// The viewport is a rectangle of `height` rows at the bottom of the screen. Content above it
/// belongs to the terminal (scrollback) and is written once through [`Self::insert_history`].
pub struct InlineTerm<B: Backend + RawWrite> {
    backend: B,
    /// Double buffer for the viewport diff. Both buffers use viewport-relative coordinates.
    buffers: [Buffer; 2],
    /// Index of the buffer the next frame renders into.
    current: usize,
    /// Absolute screen rectangle currently owned by the viewport.
    viewport: Rect,
    /// Last accepted terminal size (never zero in either dimension).
    size: Size,
    /// Absolute position the cursor is parked at after each operation.
    parked: Position,
    /// Whether the terminal cursor is known to sit at [`Self::parked`].
    cursor_synced: bool,
    /// Last requested cursor visibility (`None` = never set).
    cursor_shown: Option<bool>,
    /// Rows immediately above the viewport that a viewport shrink already
    /// cleared. They are blank on screen; the next insert or grow consumes
    /// them instead of scrolling, so a settle transition (tail rows leaving
    /// the viewport for scrollback) nets zero blank rows.
    gap_above: u16,
}

/// The driver used by the TUI host: ratatui's crossterm backend over stdout.
pub type StdoutTerm = InlineTerm<CrosstermBackend<std::io::Stdout>>;

impl InlineTerm<CrosstermBackend<std::io::Stdout>> {
    /// Build the driver over `stdout`.
    ///
    /// The caller is expected to have entered raw mode already, and the cursor is assumed to sit
    /// at column 0 of the row where the viewport should begin.
    pub fn stdout() -> std::io::Result<Self> {
        Self::new(CrosstermBackend::new(std::io::stdout()))
    }
}

impl<B: Backend + RawWrite> InlineTerm<B> {
    /// Build a driver over `backend`.
    ///
    /// The cursor is assumed to sit at column 0 of the row where the viewport should begin (the
    /// shell prompt's row at startup). The initial viewport height is 0.
    ///
    /// Terminals that do not answer a cursor position report leave us without an anchor; in that
    /// case the viewport is anchored on the last screen row, which is the only fallback that
    /// cannot paint over existing screen content (growing from there scrolls instead).
    pub fn new(mut backend: B) -> Result<Self, B::Error> {
        let size = sanitize(backend.size()?);
        let bottom = size.height.saturating_sub(1);
        let row = match backend.get_cursor_position() {
            Ok(position) => position.y.min(bottom),
            Err(_) => bottom,
        };
        let area = Rect::new(0, 0, size.width, 0);
        Ok(Self {
            backend,
            buffers: [Buffer::empty(area), Buffer::empty(area)],
            current: 0,
            viewport: Rect::new(0, row, size.width, 0),
            size,
            parked: Position::new(0, row),
            cursor_synced: true,
            cursor_shown: None,
            gap_above: 0,
        })
    }

    /// The last accepted terminal size.
    pub fn size(&self) -> Size {
        self.size
    }

    /// Accept a new terminal size.
    ///
    /// The viewport height is clamped to the new screen and the viewport stays
    /// content-anchored: it keeps its row unless the new screen is too short,
    /// in which case it moves up just enough to fit. (Re-anchoring to the
    /// bottom here would abandon the previously painted rows above the new
    /// origin as permanent garbage on every width drag — the viewport starts
    /// where the content starts, exactly like a freshly launched session.)
    /// The back buffer is dropped (forcing a full repaint on the next
    /// [`Self::draw`]) and everything from the viewport origin to the end of
    /// the screen is cleared, which also wipes whatever the terminal's reflow
    /// wrapped out of the old viewport. Scrollback is never purged.
    pub fn resize(&mut self, size: Size) -> Result<(), B::Error> {
        self.size = sanitize(size);
        // The terminal reflowed the screen; banked blank rows are gone.
        self.gap_above = 0;
        let height = self.viewport.height.clamp(1, self.max_viewport_height());
        let max_y = self.size.height.saturating_sub(height);
        let origin = Position::new(0, self.viewport.y.min(max_y));
        self.viewport = Rect::new(0, origin.y, self.size.width, height);
        let area = Rect::new(0, 0, self.size.width, height);
        self.buffers = [Buffer::empty(area), Buffer::empty(area)];
        self.batch(|this| {
            this.backend.set_cursor_position(origin)?;
            this.backend.clear_region(ClearType::AfterCursor)?;
            this.parked = origin;
            this.cursor_synced = true;
            Ok(())
        })
    }

    /// Draw one frame at the requested viewport height.
    ///
    /// `height` is clamped to `size().height - 2` (minimum 1) so the viewport never fills the
    /// screen. Height changes are absorbed first: growing while the viewport is not yet at the
    /// physical bottom extends it downward, growing while it is at the bottom writes real
    /// newlines on the last screen row (the only universally scrollback-preserving scroll) and
    /// re-anchors the viewport upward, shrinking clears the rows vacated above the new origin.
    ///
    /// `render` receives a buffer whose area is `(0, 0, width, height)`, i.e. viewport-relative,
    /// matching the coordinate space of `cursor`.
    ///
    /// `cursor` is a viewport-relative position to show the terminal cursor at (the input caret),
    /// or `None` to hide it. All output goes out as one synchronized-update batch.
    // The production host drives everything through `frame`; the two narrower
    // operations stay as the driver's documented primitives and test surface.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn draw(
        &mut self,
        height: u16,
        render: impl FnOnce(&mut Buffer),
        cursor: Option<(u16, u16)>,
    ) -> Result<(), B::Error> {
        self.frame(Vec::new(), height, render, cursor)
    }

    /// One combined step: adjust the viewport height, insert `items` into
    /// scrollback, draw the frame — all inside a single synchronized-update
    /// batch.
    ///
    /// The order matters: shrinking first banks the rows the tail vacated
    /// (`gap_above`), and the insert then writes the settled rows straight
    /// into them — a settle transition neither flashes nor leaves a blank
    /// band behind.
    pub fn frame(
        &mut self,
        items: Vec<HistoryItem>,
        height: u16,
        render: impl FnOnce(&mut Buffer),
        cursor: Option<(u16, u16)>,
    ) -> Result<(), B::Error> {
        let target = height.clamp(1, self.max_viewport_height());
        self.batch(|this| {
            this.set_viewport_height(target)?;
            this.insert_items(items)?;
            let index = this.current;
            render(&mut this.buffers[index]);
            this.flush_viewport()?;
            this.place_cursor(cursor)?;
            this.swap_buffers();
            Ok(())
        })
    }

    /// Insert rows into scrollback immediately above the viewport.
    ///
    /// While the viewport is not yet at the physical bottom of the screen it is pushed down first
    /// (a reverse index inside a scroll region), which costs no scrollback. Once it is at the
    /// bottom, the region above the viewport is scrolled up, which is what pushes rows into the
    /// terminal's scrollback; insertions larger than the space above the viewport are chunked.
    ///
    /// [`HistoryItem::Line`] rows are written through the backend at explicit positions with a
    /// clear-to-end-of-line after the last non-blank cell. [`HistoryItem::Raw`] payloads are
    /// written verbatim at column 0 of their row and the rows they occupy are reserved but never
    /// cleared. The cursor is restored to its parked position afterwards.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn insert_history(&mut self, items: Vec<HistoryItem>) -> Result<(), B::Error> {
        if items.is_empty() {
            return Ok(());
        }
        self.batch(|this| {
            this.insert_items(items)?;
            this.restore_cursor()
        })
    }

    /// Body of [`Self::insert_history`], usable inside an already-open batch.
    fn insert_items(&mut self, items: Vec<HistoryItem>) -> Result<(), B::Error> {
        if items.is_empty() {
            return Ok(());
        }
        if self.viewport.top() == 0 && self.viewport.bottom() >= self.size.height {
            return self.insert_history_borrowing_top_row(items);
        }
        // Rows `[gap, viewport.top())` are vacated and writable; rows banked
        // by a previous viewport shrink are already blank and count first.
        let mut gap = self.viewport.top().saturating_sub(self.gap_above);
        let mut remaining = items
            .iter()
            .map(HistoryItem::rows)
            .fold(0u16, u16::saturating_add);
        for item in items {
            let rows = item.rows();
            let row = if rows == 0 {
                // A payload that occupies no rows prints nothing, so it needs no vacated row
                // of its own; it only needs a defined cursor row above the viewport.
                gap.min(self.viewport.top().saturating_sub(1))
            } else {
                gap = self.make_room(gap, rows, remaining)?;
                remaining = remaining.saturating_sub(rows);
                if gap >= self.viewport.top() {
                    // No room could be made; dropping output is better than painting over the
                    // viewport. Unreachable while the viewport cannot fill the screen.
                    continue;
                }
                gap
            };
            match item {
                HistoryItem::Line(line) => self.write_history_line(row, line)?,
                HistoryItem::Raw { bytes, .. } => self.write_history_raw(row, &bytes)?,
            }
            gap = gap.saturating_add(rows);
        }
        // Whatever the batch did not fill stays banked for the next round.
        self.gap_above = self.viewport.top().saturating_sub(gap);
        Ok(())
    }

    /// Clear the visible screen, re-anchor the viewport at the top and force a full repaint on the
    /// next [`Self::draw`].
    ///
    /// This is the Ctrl+L path. Scrollback is preserved: the erase covers the visible screen only.
    pub fn clear_visible(&mut self) -> Result<(), B::Error> {
        let height = self.viewport.height.clamp(0, self.max_viewport_height());
        self.gap_above = 0;
        self.viewport = Rect::new(0, 0, self.size.width, height);
        let area = Rect::new(0, 0, self.size.width, height);
        self.buffers = [Buffer::empty(area), Buffer::empty(area)];
        self.batch(|this| {
            let home = Position::new(0, 0);
            this.backend.set_cursor_position(home)?;
            this.backend.clear_region(ClearType::All)?;
            this.backend.set_cursor_position(home)?;
            this.parked = home;
            this.cursor_synced = true;
            Ok(())
        })
    }

    /// Leave the viewport contents on screen and hand the backend back.
    ///
    /// The cursor ends up at column 0 of the row below the viewport, scrolling one line when the
    /// viewport touches the bottom of the screen, and is made visible again.
    pub fn finish(mut self) -> Result<B, B::Error> {
        let below = self.viewport.bottom();
        let last = self.size.height.saturating_sub(1);
        if below < self.size.height {
            self.backend.set_cursor_position(Position::new(0, below))?;
        } else {
            self.backend.set_cursor_position(Position::new(0, last))?;
            self.backend.append_lines(1)?;
            self.backend.set_cursor_position(Position::new(0, last))?;
        }
        self.backend.show_cursor()?;
        Backend::flush(&mut self.backend)?;
        Ok(self.backend)
    }

    /// Largest viewport height that still leaves two rows of the screen above
    /// the viewport.
    ///
    /// Two rows, not one: the scrollback push uses a DECSTBM region covering
    /// rows `[1, viewport_top]` (1-based inclusive), and DECSTBM silently
    /// ignores regions of fewer than two rows — the following scroll would
    /// then hit the whole screen, viewport included. `viewport.top >= 2`
    /// keeps the region unconditionally valid.
    fn max_viewport_height(&self) -> u16 {
        self.size.height.saturating_sub(2).max(1)
    }

    /// Wrap `body` in one synchronized-update batch followed by a single flush.
    fn batch<T>(
        &mut self,
        body: impl FnOnce(&mut Self) -> Result<T, B::Error>,
    ) -> Result<T, B::Error> {
        self.backend.write_raw(SYNC_BEGIN)?;
        let outcome = body(self);
        let closed = self.backend.write_raw(SYNC_END);
        let flushed = Backend::flush(&mut self.backend);
        let value = outcome?;
        closed?;
        flushed?;
        Ok(value)
    }

    /// Move the viewport to `height` rows, scrolling or clearing the screen as required.
    fn set_viewport_height(&mut self, height: u16) -> Result<(), B::Error> {
        let mut old = self.viewport;
        if height == old.height && old.width == self.size.width {
            return Ok(());
        }
        if height > old.height && self.gap_above > 0 {
            // Grow upward into rows a previous shrink already cleared: no
            // scroll and no clear, just reclaim them. Only those rows are
            // blank — the rest of the screen still shows the old frame, so
            // the back buffer keeps mirroring it (shifted down, blank top);
            // emptying it here made the diff treat the screen as blank, and
            // blank rows in the new frame then never erased the stale glyphs
            // underneath them.
            let reclaimed = (height - old.height).min(self.gap_above).min(old.y);
            if reclaimed > 0 {
                self.gap_above -= reclaimed;
                self.viewport =
                    Rect::new(0, old.y - reclaimed, self.size.width, old.height + reclaimed);
                self.retarget_buffers(
                    old.height,
                    self.viewport.height,
                    -i32::from(reclaimed),
                );
                old = self.viewport;
                if height == old.height && old.width == self.size.width {
                    return Ok(());
                }
            }
        }
        let vacated = if height > old.height {
            let delta = height - old.height;
            let below = self.size.height.saturating_sub(old.bottom());
            let grow_down = delta.min(below);
            let scrolled = delta - grow_down;
            if scrolled > 0 {
                // A newline on the last screen row is the only scroll every terminal turns into
                // scrollback; append_lines() emits exactly that.
                self.backend
                    .set_cursor_position(Position::new(0, self.size.height.saturating_sub(1)))?;
                self.backend.append_lines(scrolled)?;
                self.cursor_synced = false;
            }
            let top = old.y.saturating_sub(scrolled);
            self.viewport = Rect::new(0, top, self.size.width, height);
            // Rows acquired at the bottom may still hold whatever the terminal had there.
            top.saturating_add(old.height)..top.saturating_add(height)
        } else {
            let top = old.y.saturating_add(old.height - height);
            self.viewport = Rect::new(0, top, self.size.width, height);
            old.y..top
        };
        for row in vacated {
            self.backend.set_cursor_position(Position::new(0, row))?;
            self.backend.clear_region(ClearType::UntilNewLine)?;
            self.cursor_synced = false;
        }
        if height < old.height {
            // Bank the rows just cleared above the new top; the next insert
            // or grow consumes them, so no blank band is left behind.
            self.gap_above = self
                .gap_above
                .saturating_add(old.height - height)
                .min(self.viewport.y);
        }
        self.retarget_buffers(
            old.height,
            height,
            i32::from(old.height.saturating_sub(height)),
        );
        Ok(())
    }

    /// Re-shape both diff buffers for the new viewport height.
    ///
    /// Buffers are viewport-relative; new row `y` keeps the content of old row `y + offset`, so
    /// the back buffer keeps mirroring the physical rows it now covers. A shrink keeps the bottom
    /// rows (`offset > 0`: the viewport bottom stays anchored on screen), a downward or scrolled
    /// grow keeps the top rows (`offset == 0`), and a reclaim-grow extends the top upward
    /// (`offset < 0`: old content shifts down, the fresh top rows stay blank — exactly the
    /// physically blank rows the viewport grew into).
    fn retarget_buffers(&mut self, old_height: u16, height: u16, offset: i32) {
        let area = Rect::new(0, 0, self.size.width, height);
        let previous = 1 - self.current;
        let old = std::mem::replace(&mut self.buffers[previous], Buffer::empty(area));
        let mut kept = Buffer::empty(area);
        for y in 0..height {
            let Ok(source) = u16::try_from(i32::from(y) + offset) else {
                continue;
            };
            if source >= old_height {
                break;
            }
            for x in 0..area.width {
                let Some(cell) = old.cell((x, source)).cloned() else {
                    break;
                };
                if let Some(slot) = kept.cell_mut((x, y)) {
                    *slot = cell;
                }
            }
        }
        self.buffers[previous] = kept;
        self.buffers[self.current] = Buffer::empty(area);
    }

    /// Emit the diff between the rendered frame and the frame on screen.
    ///
    /// Runs of changed cells are written per row; when a row shrank, a clear-to-end-of-line is
    /// emitted after its last non-blank cell instead of painting spaces out to the margin.
    fn flush_viewport(&mut self) -> Result<(), B::Error> {
        let area = *self.buffers[self.current].area();
        let previous = 1 - self.current;
        if *self.buffers[previous].area() != area {
            self.buffers[previous].resize(area);
            self.buffers[previous].reset();
        }
        let top = self.viewport.y;
        let mut clears: Vec<Position> = Vec::new();
        let mut wrote = false;
        {
            let Self {
                backend, buffers, ..
            } = self;
            let next = &buffers[self.current];
            let prev = &buffers[previous];
            let mut puts: Vec<(u16, u16, &Cell)> = Vec::new();
            for y in 0..area.height {
                let next_end = row_end(next, y);
                let prev_end = row_end(prev, y);
                let mut invalidated: u16 = 0;
                let mut to_skip: u16 = 0;
                for x in 0..next_end {
                    let Some(cell) = next.cell((x, y)) else { break };
                    let old = prev.cell((x, y));
                    let changed = old.is_none_or(|old| old != cell);
                    if (changed || invalidated > 0) && to_skip == 0 {
                        puts.push((x, top.saturating_add(y), cell));
                    }
                    let width = cell.cell_width();
                    to_skip = width.saturating_sub(1);
                    let affected = width.max(old.map_or(0, CellWidth::cell_width));
                    invalidated = affected.max(invalidated).saturating_sub(1);
                }
                if next_end < prev_end {
                    clears.push(Position::new(next_end, top.saturating_add(y)));
                }
            }
            if !puts.is_empty() {
                wrote = true;
                backend.draw(puts.into_iter())?;
            }
        }
        for position in clears {
            wrote = true;
            self.backend.set_cursor_position(position)?;
            self.backend.clear_region(ClearType::UntilNewLine)?;
        }
        if wrote {
            self.cursor_synced = false;
        }
        Ok(())
    }

    /// Show the caret at a viewport-relative position, or hide the cursor, then park it.
    fn place_cursor(&mut self, cursor: Option<(u16, u16)>) -> Result<(), B::Error> {
        let target = match cursor {
            Some((x, y)) => {
                if self.cursor_shown != Some(true) {
                    self.backend.show_cursor()?;
                    self.cursor_shown = Some(true);
                }
                Position::new(
                    x.min(self.viewport.width.saturating_sub(1)),
                    self.viewport
                        .y
                        .saturating_add(y.min(self.viewport.height.saturating_sub(1))),
                )
            }
            None => {
                if self.cursor_shown != Some(false) {
                    self.backend.hide_cursor()?;
                    self.cursor_shown = Some(false);
                }
                Position::new(0, self.viewport.bottom().saturating_sub(1))
            }
        };
        self.parked = target;
        self.restore_cursor()
    }

    /// Move the terminal cursor back to the parked position if it drifted.
    fn restore_cursor(&mut self) -> Result<(), B::Error> {
        if !self.cursor_synced {
            self.backend.set_cursor_position(self.parked)?;
            self.cursor_synced = true;
        }
        Ok(())
    }

    /// Publish the frame just drawn and hand a blank buffer to the next one.
    fn swap_buffers(&mut self) {
        self.buffers[1 - self.current].reset();
        self.current = 1 - self.current;
    }

    /// Make sure at least `want` vacated rows are available above the viewport.
    ///
    /// Returns the first writable row. Pushing the viewport down is preferred while there is room
    /// below it because it costs no scrollback; once the viewport sits on the bottom, the region
    /// above it is scrolled up, which is what pushes rows into scrollback.
    ///
    /// `remaining` is how many rows the rest of the batch still needs. Vacating that much at once
    /// makes each scroll cover a whole chunk, exactly like ratatui's `insert_before`.
    fn make_room(&mut self, gap: u16, want: u16, remaining: u16) -> Result<u16, B::Error> {
        let mut gap = gap;
        loop {
            let top = self.viewport.top();
            let available = top.saturating_sub(gap);
            if available >= want {
                return Ok(gap);
            }
            let missing = remaining.max(want).saturating_sub(available);
            let below = self.size.height.saturating_sub(self.viewport.bottom());
            if below > 0 {
                let step = missing.min(below);
                let region = top..self.viewport.bottom().saturating_add(step);
                self.backend.scroll_region_down(region, step)?;
                self.viewport.y = self.viewport.y.saturating_add(step);
                self.parked.y = self
                    .parked
                    .y
                    .saturating_add(step)
                    .min(self.size.height.saturating_sub(1));
                self.cursor_synced = false;
                continue;
            }
            if top == 0 {
                return Ok(gap);
            }
            let step = missing.min(top);
            self.backend.scroll_into_scrollback(top, step)?;
            gap = gap.saturating_sub(step);
            self.cursor_synced = false;
        }
    }

    /// Write one pre-wrapped row at absolute screen row `row`.
    fn write_history_line(&mut self, row: u16, line: Line<'static>) -> Result<(), B::Error> {
        let width = self.size.width;
        let area = Rect::new(0, row, width, 1);
        let mut scratch = Buffer::empty(area);
        scratch.set_line(0, row, &clamp_line(line, width), width);
        let end = row_end(&scratch, row);
        {
            let mut cells: Vec<(u16, u16, &Cell)> = Vec::new();
            let mut x = 0u16;
            while x < width {
                let Some(cell) = scratch.cell((x, row)) else {
                    break;
                };
                if *cell != Cell::EMPTY {
                    cells.push((x, row, cell));
                }
                x = x.saturating_add(cell.cell_width().max(1));
            }
            if !cells.is_empty() {
                self.backend.draw(cells.into_iter())?;
                self.cursor_synced = false;
            }
        }
        if end < width {
            self.backend.set_cursor_position(Position::new(end, row))?;
            self.backend.clear_region(ClearType::UntilNewLine)?;
            self.cursor_synced = false;
        }
        Ok(())
    }

    /// Write a raw payload at column 0 of absolute screen row `row`.
    fn write_history_raw(&mut self, row: u16, bytes: &[u8]) -> Result<(), B::Error> {
        self.backend.set_cursor_position(Position::new(0, row))?;
        self.backend.write_raw(bytes)?;
        self.cursor_synced = false;
        Ok(())
    }

    /// Degenerate path for a viewport that fills the whole screen (a one-row terminal).
    ///
    /// There is no space above the viewport, so the top row is borrowed: each item is drawn over
    /// it and immediately scrolled into scrollback, then the borrowed row is repainted from the
    /// committed frame. This mirrors ratatui's full-screen `insert_before` path.
    fn insert_history_borrowing_top_row(
        &mut self,
        items: Vec<HistoryItem>,
    ) -> Result<(), B::Error> {
        for item in items {
            let rows = item.rows().max(1);
            match item {
                HistoryItem::Line(line) => self.write_history_line(0, line)?,
                HistoryItem::Raw { bytes, .. } => self.write_history_raw(0, &bytes)?,
            }
            for _ in 0..rows {
                self.backend.scroll_into_scrollback(1, 1)?;
                self.cursor_synced = false;
            }
        }
        {
            let previous = 1 - self.current;
            let Self {
                backend, buffers, ..
            } = self;
            let committed = &buffers[previous];
            let mut cells: Vec<(u16, u16, &Cell)> = Vec::new();
            let mut x = 0u16;
            while x < committed.area.width {
                let Some(cell) = committed.cell((x, 0)) else {
                    break;
                };
                if *cell != Cell::EMPTY {
                    cells.push((x, 0, cell));
                }
                x = x.saturating_add(cell.cell_width().max(1));
            }
            if !cells.is_empty() {
                backend.draw(cells.into_iter())?;
            }
        }
        self.restore_cursor()
    }

    #[cfg(test)]
    fn backend(&self) -> &B {
        &self.backend
    }

    #[cfg(test)]
    fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    #[cfg(test)]
    fn viewport(&self) -> Rect {
        self.viewport
    }
}

/// Reject degenerate terminal sizes: a zero-sized screen has no valid viewport.
fn sanitize(size: Size) -> Size {
    Size::new(size.width.max(1), size.height.max(1))
}

/// Column just past the last non-blank cell of `row`, honoring multi-width glyphs.
fn row_end(buffer: &Buffer, row: u16) -> u16 {
    let width = buffer.area.width;
    let mut end = 0u16;
    let mut x = 0u16;
    while x < width {
        let Some(cell) = buffer.cell((x, row)) else {
            break;
        };
        let cell_width = cell.cell_width().max(1);
        if *cell != Cell::EMPTY {
            end = x.saturating_add(cell_width).min(width);
        }
        x = x.saturating_add(cell_width);
    }
    end
}

/// Truncate a history row to `width` display columns.
///
/// Rows arrive pre-wrapped; this is the safety net that keeps an over-long row from wrapping and
/// desynchronizing the row accounting. Never allocates when the row already fits.
fn clamp_line(line: Line<'static>, width: u16) -> Line<'static> {
    let total: usize = line.spans.iter().map(|span| span.content.width()).sum();
    let limit = usize::from(width);
    if total <= limit {
        return line;
    }
    let mut remaining = limit;
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(line.spans.len());
    for span in line.spans {
        if remaining == 0 {
            break;
        }
        let span_width = span.content.width();
        if span_width <= remaining {
            remaining -= span_width;
            spans.push(span);
            continue;
        }
        let mut taken = String::new();
        let mut used = 0usize;
        for ch in span.content.chars() {
            let char_width = ch.width().unwrap_or(0);
            if used + char_width > remaining {
                break;
            }
            used += char_width;
            taken.push(ch);
        }
        spans.push(Span::styled(taken, span.style));
        break;
    }
    Line {
        spans,
        style: line.style,
        alignment: line.alignment,
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::ops::Range;

    use ratatui::backend::{TestBackend, WindowSize};
    use ratatui::style::{Color, Style};

    use super::*;

    /// TestBackend plus the raw-byte sink and command counters the driver needs asserting on.
    struct Recorder {
        inner: TestBackend,
        raw: Vec<u8>,
        draw_calls: usize,
        clear_calls: usize,
        scrolled_up: Vec<(Range<u16>, u16)>,
        scrolled_down: Vec<(Range<u16>, u16)>,
        appended: u16,
    }

    impl Recorder {
        fn new(width: u16, height: u16) -> Self {
            Self {
                inner: TestBackend::new(width, height),
                raw: Vec::new(),
                draw_calls: 0,
                clear_calls: 0,
                scrolled_up: Vec::new(),
                scrolled_down: Vec::new(),
                appended: 0,
            }
        }

        fn reset_counters(&mut self) {
            self.raw.clear();
            self.draw_calls = 0;
            self.clear_calls = 0;
            self.scrolled_up.clear();
            self.scrolled_down.clear();
            self.appended = 0;
        }

        fn screen(&self) -> Vec<String> {
            rows_of(self.inner.buffer())
        }

        fn scrollback(&self) -> Vec<String> {
            rows_of(self.inner.scrollback())
        }
    }

    fn rows_of(buffer: &Buffer) -> Vec<String> {
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    impl Backend for Recorder {
        type Error = Infallible;

        fn draw<'a, I>(&mut self, content: I) -> Result<(), Infallible>
        where
            I: Iterator<Item = (u16, u16, &'a Cell)>,
        {
            let cells: Vec<(u16, u16, &Cell)> = content.collect();
            if !cells.is_empty() {
                self.draw_calls += 1;
            }
            self.inner.draw(cells.into_iter())
        }

        fn append_lines(&mut self, n: u16) -> Result<(), Infallible> {
            self.appended = self.appended.saturating_add(n);
            self.inner.append_lines(n)
        }

        fn hide_cursor(&mut self) -> Result<(), Infallible> {
            self.inner.hide_cursor()
        }

        fn show_cursor(&mut self) -> Result<(), Infallible> {
            self.inner.show_cursor()
        }

        fn get_cursor_position(&mut self) -> Result<Position, Infallible> {
            self.inner.get_cursor_position()
        }

        fn set_cursor_position<P: Into<Position>>(
            &mut self,
            position: P,
        ) -> Result<(), Infallible> {
            self.inner.set_cursor_position(position)
        }

        fn clear(&mut self) -> Result<(), Infallible> {
            self.clear_calls += 1;
            self.inner.clear()
        }

        fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Infallible> {
            self.clear_calls += 1;
            self.inner.clear_region(clear_type)
        }

        fn size(&self) -> Result<Size, Infallible> {
            self.inner.size()
        }

        fn window_size(&mut self) -> Result<WindowSize, Infallible> {
            self.inner.window_size()
        }

        fn flush(&mut self) -> Result<(), Infallible> {
            self.inner.flush()
        }

        fn scroll_region_up(
            &mut self,
            region: Range<u16>,
            line_count: u16,
        ) -> Result<(), Infallible> {
            self.scrolled_up.push((region.clone(), line_count));
            self.inner.scroll_region_up(region, line_count)
        }

        fn scroll_region_down(
            &mut self,
            region: Range<u16>,
            line_count: u16,
        ) -> Result<(), Infallible> {
            self.scrolled_down.push((region.clone(), line_count));
            self.inner.scroll_region_down(region, line_count)
        }
    }

    impl RawWrite for Recorder {
        fn write_raw(&mut self, bytes: &[u8]) -> Result<(), Infallible> {
            self.raw.extend_from_slice(bytes);
            Ok(())
        }

        // Semantically a top-anchored region scroll whose evicted rows land
        // in scrollback — exactly what TestBackend models for
        // `scroll_region_up`, so the driver tests keep asserting real
        // semantics while the production backend emits the LF form.
        fn scroll_into_scrollback(&mut self, top: u16, n: u16) -> Result<(), Infallible> {
            self.scrolled_up.push((0..top, n));
            self.inner.scroll_region_up(0..top, n)
        }
    }

    fn term(width: u16, height: u16, row: u16) -> InlineTerm<Recorder> {
        let mut backend = Recorder::new(width, height);
        backend.set_cursor_position(Position::new(0, row)).unwrap();
        InlineTerm::new(backend).unwrap()
    }

    /// Fill the viewport buffer with `rows`, top-down.
    fn paint<'a>(rows: &'a [&'a str]) -> impl FnOnce(&mut Buffer) + 'a {
        move |buffer: &mut Buffer| {
            for (y, text) in rows.iter().enumerate() {
                let Ok(y) = u16::try_from(y) else { continue };
                if y >= buffer.area.height {
                    break;
                }
                buffer.set_string(0, y, text, Style::default());
            }
        }
    }

    fn lines(texts: &[String]) -> Vec<HistoryItem> {
        texts
            .iter()
            .map(|text| HistoryItem::Line(Line::from(text.clone())))
            .collect()
    }

    fn sync_only(raw: &[u8]) -> bool {
        raw == [SYNC_BEGIN, SYNC_END].concat().as_slice()
    }

    /// The last `n` rows of a scrollback snapshot.
    fn tail(rows: &[String], n: usize) -> Vec<String> {
        rows[rows.len().saturating_sub(n)..].to_vec()
    }

    #[test]
    fn new_anchors_viewport_at_cursor_row() {
        let term = term(10, 8, 5);
        assert_eq!(term.viewport(), Rect::new(0, 5, 10, 0));
        assert_eq!(term.size(), Size::new(10, 8));
    }

    #[test]
    fn draw_grows_viewport_downward_when_space_below() {
        let mut term = term(10, 8, 2);
        term.draw(3, paint(&["a", "b", "c"]), None).unwrap();

        assert_eq!(term.viewport(), Rect::new(0, 2, 10, 3));
        assert_eq!(term.backend().appended, 0, "no scroll needed");
        term.backend().inner.assert_scrollback_empty();
        assert_eq!(
            term.backend().screen(),
            vec!["", "", "a", "b", "c", "", "", ""]
        );
    }

    #[test]
    fn draw_grow_at_bottom_scrolls_exactly_delta_rows_into_scrollback() {
        let mut term = term(4, 6, 5);
        term.draw(1, paint(&["v0"]), None).unwrap();
        assert_eq!(term.viewport(), Rect::new(0, 5, 4, 1));
        assert_eq!(term.backend().appended, 0);

        term.backend_mut().reset_counters();
        term.draw(4, paint(&["r0", "r1", "r2", "r3"]), None)
            .unwrap();

        assert_eq!(term.viewport(), Rect::new(0, 2, 4, 4));
        assert_eq!(term.backend().appended, 3, "grew by 3 rows at the bottom");
        assert_eq!(term.backend().scrollback().len(), 3);
        assert_eq!(
            term.backend().screen(),
            vec!["", "", "r0", "r1", "r2", "r3"]
        );
    }

    #[test]
    fn draw_renders_contents_and_honors_cursor_placement() {
        let mut term = term(8, 6, 3);
        term.draw(2, paint(&["hi", "there"]), Some((3, 1))).unwrap();

        assert_eq!(term.backend().screen()[3], "hi");
        assert_eq!(term.backend().screen()[4], "there");
        assert!(term.backend().inner.cursor_visible());
        assert_eq!(
            term.backend().inner.cursor_position(),
            Position::new(3, 4),
            "viewport-relative (3,1) maps to absolute row 4"
        );
    }

    #[test]
    fn draw_hides_cursor_when_none() {
        let mut term = term(8, 6, 3);
        term.draw(2, paint(&["hi", "there"]), None).unwrap();
        assert!(!term.backend().inner.cursor_visible());
        assert_eq!(term.backend().inner.cursor_position(), Position::new(0, 4));
    }

    #[test]
    fn draw_with_smaller_height_clears_vacated_rows() {
        let mut term = term(6, 8, 7);
        term.draw(4, paint(&["r0", "r1", "r2", "r3"]), None)
            .unwrap();
        assert_eq!(term.viewport(), Rect::new(0, 4, 6, 4));

        term.draw(2, paint(&["k0", "k1"]), None).unwrap();

        assert_eq!(term.viewport(), Rect::new(0, 6, 6, 2));
        assert_eq!(
            term.backend().screen(),
            vec!["", "", "", "", "", "", "k0", "k1"],
            "rows 4 and 5 were vacated and cleared"
        );
    }

    #[test]
    fn draw_clears_stale_tail_when_a_row_shrinks() {
        let mut term = term(10, 6, 5);
        term.draw(1, paint(&["abcdefgh"]), None).unwrap();
        assert_eq!(term.backend().screen()[5], "abcdefgh");

        term.draw(1, paint(&["ab"]), None).unwrap();

        assert_eq!(term.backend().screen()[5], "ab", "no stale tail left over");
        let row: String = (0..10)
            .map(|x| term.backend().inner.buffer()[(x, 5)].symbol().to_string())
            .collect();
        assert_eq!(row, "ab        ");
    }

    #[test]
    fn draw_never_paints_trailing_spaces() {
        let mut term = term(10, 6, 5);
        term.backend_mut().reset_counters();
        term.draw(1, paint(&["ab"]), None).unwrap();

        // Two glyphs painted in one draw call; the eight blank columns are not written at all.
        assert_eq!(term.backend().draw_calls, 1);
        assert_eq!(
            term.backend().clear_calls,
            1,
            "the viewport row is cleared once while growing, never padded"
        );
    }

    #[test]
    fn identical_frame_emits_no_backend_writes() {
        let mut term = term(10, 6, 5);
        term.draw(2, paint(&["one", "two"]), Some((1, 0))).unwrap();

        term.backend_mut().reset_counters();
        term.draw(2, paint(&["one", "two"]), Some((1, 0))).unwrap();

        assert_eq!(term.backend().draw_calls, 0);
        assert_eq!(term.backend().clear_calls, 0);
        assert_eq!(term.backend().appended, 0);
        assert!(term.backend().scrolled_up.is_empty());
        assert!(term.backend().scrolled_down.is_empty());
        assert!(
            sync_only(&term.backend().raw),
            "only the synchronized-update brackets go out"
        );
    }

    #[test]
    fn insert_history_at_bottom_pushes_rows_into_scrollback() {
        let mut term = term(6, 5, 4);
        term.draw(2, paint(&["v0", "v1"]), None).unwrap();
        assert_eq!(term.viewport(), Rect::new(0, 3, 6, 2));

        term.backend_mut().reset_counters();
        let texts: Vec<String> = (0..5).map(|i| format!("h{i}")).collect();
        term.insert_history(lines(&texts)).unwrap();

        assert_eq!(term.viewport(), Rect::new(0, 3, 6, 2), "viewport unmoved");
        assert_eq!(
            term.backend().screen(),
            vec!["h2", "h3", "h4", "v0", "v1"],
            "viewport content untouched"
        );
        // The blank rows the screen started with are pushed out first; the overflowing history
        // rows land behind them, in order.
        assert_eq!(term.backend().scrollback().last(), Some(&"h1".to_string()));
        assert_eq!(tail(&term.backend().scrollback(), 2), vec!["h0", "h1"]);
        assert_eq!(
            term.backend().inner.cursor_position(),
            Position::new(0, 4),
            "cursor restored to the parked position"
        );
    }

    #[test]
    fn insert_history_not_at_bottom_pushes_viewport_down_first() {
        let mut term = term(6, 8, 1);
        term.draw(2, paint(&["v0", "v1"]), None).unwrap();
        assert_eq!(term.viewport(), Rect::new(0, 1, 6, 2));

        term.backend_mut().reset_counters();
        let texts: Vec<String> = (0..3).map(|i| format!("h{i}")).collect();
        term.insert_history(lines(&texts)).unwrap();

        assert_eq!(term.viewport(), Rect::new(0, 4, 6, 2));
        term.backend().inner.assert_scrollback_empty();
        assert!(
            term.backend().scrolled_up.is_empty(),
            "nothing entered scrollback yet"
        );
        assert_eq!(
            term.backend().screen(),
            vec!["", "h0", "h1", "h2", "v0", "v1", "", ""]
        );
    }

    #[test]
    fn insert_history_larger_than_space_above_chunks_in_order() {
        let mut term = term(6, 6, 5);
        term.draw(2, paint(&["v0", "v1"]), None).unwrap();
        assert_eq!(term.viewport(), Rect::new(0, 4, 6, 2));

        term.backend_mut().reset_counters();
        let texts: Vec<String> = (0..10).map(|i| format!("h{i}")).collect();
        term.insert_history(lines(&texts)).unwrap();

        assert!(
            term.backend().scrolled_up.len() > 1,
            "more rows than fit above the viewport must be chunked"
        );
        assert_eq!(
            tail(&term.backend().scrollback(), 6),
            vec!["h0", "h1", "h2", "h3", "h4", "h5"]
        );
        assert_eq!(
            term.backend().screen(),
            vec!["h6", "h7", "h8", "h9", "v0", "v1"]
        );
    }

    #[test]
    fn insert_history_raw_row_accounting() {
        let mut term = term(6, 8, 7);
        term.draw(1, paint(&["v0"]), None).unwrap();
        assert_eq!(term.viewport(), Rect::new(0, 7, 6, 1));

        term.backend_mut().reset_counters();
        term.insert_history(vec![
            HistoryItem::Line(Line::from("a")),
            HistoryItem::Raw {
                bytes: b"\x1b_Gq=2;PAYLOAD\x1b\\".to_vec(),
                rows: 0,
            },
            HistoryItem::Line(Line::from("b")),
        ])
        .unwrap();

        assert_eq!(
            term.backend().screen(),
            vec!["", "", "", "", "", "a", "b", "v0"],
            "a zero-row payload consumes no rows"
        );

        term.backend_mut().reset_counters();
        term.insert_history(vec![
            HistoryItem::Raw {
                bytes: b"\x1b_Gq=2,C=1;IMG\x1b\\".to_vec(),
                rows: 2,
            },
            HistoryItem::Line(Line::from("c")),
        ])
        .unwrap();

        assert_eq!(
            term.backend().screen(),
            vec!["", "", "a", "b", "", "", "c", "v0"],
            "a two-row payload reserves two rows and they are never cleared"
        );
        let raw = &term.backend().raw;
        assert!(
            raw.windows(3).any(|window| window == b"IMG"),
            "raw payload written verbatim"
        );
        assert!(raw.starts_with(SYNC_BEGIN) && raw.ends_with(SYNC_END));
    }

    #[test]
    fn insert_history_truncates_overlong_rows() {
        let mut term = term(6, 5, 4);
        term.draw(1, paint(&["v0"]), None).unwrap();

        term.insert_history(vec![HistoryItem::Line(Line::from(
            "0123456789abcdef".to_string(),
        ))])
        .unwrap();

        assert_eq!(
            term.backend().screen()[3],
            "012345",
            "over-long rows are truncated on display width, never wrapped"
        );
    }

    #[test]
    fn insert_history_preserves_styles() {
        let mut term = term(6, 5, 4);
        term.draw(1, paint(&["v0"]), None).unwrap();

        let line = Line::from(Span::styled("hey", Style::default().fg(Color::Green)));
        term.insert_history(vec![HistoryItem::Line(line)]).unwrap();

        let cell = &term.backend().inner.buffer()[(0, 3)];
        assert_eq!(cell.symbol(), "h");
        assert_eq!(cell.fg, Color::Green);
    }

    #[test]
    fn resize_reanchors_to_bottom_and_forces_full_repaint() {
        let mut term = term(6, 8, 7);
        term.draw(3, paint(&["r0", "r1", "r2"]), None).unwrap();
        assert_eq!(term.viewport(), Rect::new(0, 5, 6, 3));

        term.backend_mut().inner.resize(6, 6);
        term.resize(Size::new(6, 6)).unwrap();

        assert_eq!(term.viewport(), Rect::new(0, 3, 6, 3));
        assert_eq!(term.size(), Size::new(6, 6));

        term.backend_mut().reset_counters();
        term.draw(3, paint(&["r0", "r1", "r2"]), None).unwrap();
        assert_eq!(
            term.backend().draw_calls,
            1,
            "the back buffer was dropped, so the frame repaints in full"
        );
        assert_eq!(term.backend().screen(), vec!["", "", "", "r0", "r1", "r2"]);
    }

    #[test]
    fn resize_clamps_viewport_height_to_the_new_screen() {
        let mut term = term(6, 10, 9);
        term.draw(6, paint(&["a", "b", "c", "d", "e", "f"]), None)
            .unwrap();
        assert_eq!(term.viewport().height, 6);

        term.backend_mut().inner.resize(6, 4);
        term.resize(Size::new(6, 4)).unwrap();

        assert_eq!(
            term.viewport(),
            Rect::new(0, 2, 6, 2),
            "height clamped to screen height - 2 and re-anchored to the bottom"
        );
        term.draw(6, paint(&["a", "b", "c", "d", "e", "f"]), None)
            .unwrap();
        assert_eq!(term.viewport().height, 2);
    }

    #[test]
    fn resize_taller_keeps_the_viewport_content_anchored() {
        let mut term = term(6, 6, 5);
        term.draw(2, paint(&["a", "b"]), None).unwrap();
        assert_eq!(term.viewport(), Rect::new(0, 4, 6, 2));

        term.backend_mut().inner.resize(6, 10);
        term.resize(Size::new(6, 10)).unwrap();

        // Content-anchored: a taller screen does not fling the viewport to
        // the new bottom — it stays where the content is, so nothing painted
        // earlier is left stranded above the origin as garbage.
        assert_eq!(term.viewport(), Rect::new(0, 4, 6, 2));
        term.draw(2, paint(&["a", "b"]), None).unwrap();
        assert_eq!(term.backend().screen()[4], "a");
        assert_eq!(term.backend().screen()[5], "b");
    }

    #[test]
    fn clear_visible_blanks_the_screen_and_anchors_at_the_top() {
        let mut term = term(6, 6, 5);
        term.draw(2, paint(&["a", "b"]), None).unwrap();
        let texts: Vec<String> = (0..3).map(|i| format!("h{i}")).collect();
        term.insert_history(lines(&texts)).unwrap();

        let before = term.backend().scrollback();
        term.clear_visible().unwrap();

        assert!(term.backend().screen().iter().all(String::is_empty));
        assert_eq!(term.viewport(), Rect::new(0, 0, 6, 2));
        assert_eq!(
            term.backend().scrollback(),
            before,
            "clear_visible never purges scrollback"
        );

        term.draw(2, paint(&["a", "b"]), None).unwrap();
        assert_eq!(term.backend().screen()[0], "a");
        assert_eq!(term.backend().screen()[1], "b");
    }

    #[test]
    fn finish_parks_the_cursor_below_the_viewport() {
        let mut term = term(6, 8, 3);
        term.draw(2, paint(&["a", "b"]), None).unwrap();
        assert_eq!(term.viewport(), Rect::new(0, 3, 6, 2));

        let backend = term.finish().unwrap();
        assert_eq!(backend.inner.cursor_position(), Position::new(0, 5));
        assert!(backend.inner.cursor_visible());
        assert_eq!(backend.screen()[3], "a");
        assert_eq!(backend.screen()[4], "b");
    }

    #[test]
    fn finish_scrolls_one_line_when_the_viewport_touches_the_bottom() {
        let mut term = term(6, 4, 3);
        term.draw(2, paint(&["a", "b"]), None).unwrap();
        assert_eq!(term.viewport(), Rect::new(0, 2, 6, 2));

        term.backend_mut().reset_counters();
        let mut backend = term.finish().unwrap();
        assert_eq!(backend.appended, 1);
        assert_eq!(backend.get_cursor_position().unwrap(), Position::new(0, 3));
        assert_eq!(backend.screen()[1], "a", "content scrolled up by one row");
        assert_eq!(backend.screen()[2], "b");
    }

    #[test]
    fn single_row_terminal_inserts_history_without_hanging() {
        let mut term = term(6, 1, 0);
        term.draw(1, paint(&["v0"]), None).unwrap();
        assert_eq!(term.viewport(), Rect::new(0, 0, 6, 1));

        let texts: Vec<String> = (0..3).map(|i| format!("h{i}")).collect();
        term.insert_history(lines(&texts)).unwrap();

        assert_eq!(term.backend().scrollback(), vec!["h0", "h1", "h2"]);
        assert_eq!(
            term.backend().screen(),
            vec!["v0"],
            "the borrowed viewport row is repainted"
        );
    }

    /// The old failure mode: history rows duplicated or lost when the terminal resized after a
    /// large batch of inserts.
    #[test]
    fn history_survives_resize_without_duplication() {
        let width = 6u16;
        let mut term = term(width, 30, 29);
        let chrome: Vec<&str> = vec!["c0", "c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8", "c9"];
        term.draw(10, paint(&chrome), None).unwrap();
        assert_eq!(term.viewport(), Rect::new(0, 20, width, 10));

        let texts: Vec<String> = (0..50).map(|i| format!("L{i:02}")).collect();
        term.insert_history(lines(&texts)).unwrap();

        // 20 rows fit above the viewport; the first 30 went into scrollback behind the blank rows
        // the screen started with.
        assert_eq!(
            tail(&term.backend().scrollback(), 30),
            texts[..30].to_vec(),
            "the overflow reaches scrollback in order"
        );
        let seen: Vec<String> = term
            .backend()
            .scrollback()
            .into_iter()
            .chain(term.backend().screen())
            .collect();
        for text in &texts {
            assert_eq!(
                seen.iter().filter(|row| *row == text).count(),
                1,
                "{text} must appear exactly once before the resize"
            );
        }

        term.backend_mut().inner.resize(width, 22);
        term.resize(Size::new(width, 22)).unwrap();
        assert_eq!(term.viewport(), Rect::new(0, 12, width, 10));

        let after: Vec<&str> = vec!["n0", "n1", "n2", "n3", "n4", "n5", "n6", "n7", "n8", "n9"];
        term.draw(10, paint(&after), None).unwrap();

        let scrollback = term.backend().scrollback();
        let screen = term.backend().screen();
        let all: Vec<String> = scrollback
            .iter()
            .cloned()
            .chain(screen.iter().cloned())
            .collect();

        for text in &texts {
            let count = all.iter().filter(|row| *row == text).count();
            assert!(count <= 1, "{text} duplicated after the resize");
        }
        // Everything not covered by the re-anchored viewport is still there exactly once.
        for text in texts.iter().take(42) {
            assert_eq!(
                all.iter().filter(|row| *row == text).count(),
                1,
                "{text} lost after the resize"
            );
        }
        for (index, row) in after.iter().enumerate() {
            assert_eq!(
                all.iter()
                    .filter(|candidate| candidate.as_str() == *row)
                    .count(),
                1,
                "chrome row {row} duplicated"
            );
            assert_eq!(screen[12 + index], *row);
        }
        assert!(
            scrollback.iter().all(|row| !after.contains(&row.as_str())),
            "chrome must never reach scrollback"
        );
    }

    #[test]
    fn frame_settles_tail_into_banked_rows_without_scrolling() {
        let mut term = term(10, 12, 0);
        // A tall frame: 6 tail rows + 2 chrome rows.
        term.draw(8, paint(&["t0", "t1", "t2", "t3", "t4", "t5", "c0", "c1"]), None)
            .unwrap();
        term.backend_mut().reset_counters();

        // The tail settles: the same 6 rows leave the viewport for scrollback
        // and the chrome redraws in the shrunken viewport.
        let settled: Vec<String> = (0..6).map(|i| format!("t{i}")).collect();
        term.frame(lines(&settled), 2, paint(&["c0", "c1"]), None)
            .unwrap();

        assert_eq!(term.viewport(), Rect::new(0, 6, 10, 2));
        assert!(
            term.backend().scrolled_up.is_empty(),
            "the bank absorbed the whole settle"
        );
        assert_eq!(term.backend().appended, 0);
        term.backend().inner.assert_scrollback_empty();
        assert_eq!(
            term.backend().screen(),
            vec!["t0", "t1", "t2", "t3", "t4", "t5", "c0", "c1", "", "", "", ""],
            "settled rows sit exactly where the viewport freed them — no blank band"
        );
        assert_eq!(term.gap_above, 0);
    }

    #[test]
    fn frame_scrolls_only_the_rows_the_bank_cannot_cover() {
        let mut term = term(10, 8, 7);
        term.draw(6, paint(&["t0", "t1", "t2", "t3", "c0", "c1"]), None)
            .unwrap();
        term.backend_mut().reset_counters();

        // 6 settled rows, but shrinking 6 -> 2 banks only 4.
        let settled: Vec<String> = (0..6).map(|i| format!("s{i}")).collect();
        term.frame(lines(&settled), 2, paint(&["c0", "c1"]), None)
            .unwrap();

        assert_eq!(
            term.backend().scrolled_up,
            vec![(0u16..6, 2u16)],
            "only the two uncovered rows scroll"
        );
        let screen = term.backend().screen();
        assert_eq!(&screen[0..6], &["s0", "s1", "s2", "s3", "s4", "s5"]);
        assert_eq!(&screen[6..8], &["c0", "c1"]);
        assert_eq!(term.gap_above, 0);
    }

    #[test]
    fn grow_reclaims_banked_rows_without_scrolling() {
        let mut term = term(10, 12, 0);
        term.draw(8, paint(&["t0", "t1", "t2", "t3", "t4", "t5", "c0", "c1"]), None)
            .unwrap();
        // Settle only 3 of the 6 freed rows: 3 stay banked as blanks.
        let settled: Vec<String> = (0..3).map(|i| format!("t{i}")).collect();
        term.frame(lines(&settled), 2, paint(&["c0", "c1"]), None)
            .unwrap();
        assert_eq!(term.gap_above, 3);
        term.backend_mut().reset_counters();

        // The next turn grows the viewport again: it reclaims the blanks
        // instead of scrolling anything into scrollback.
        term.draw(5, paint(&["u0", "u1", "u2", "c0", "c1"]), None)
            .unwrap();

        assert_eq!(term.gap_above, 0);
        assert_eq!(term.viewport(), Rect::new(0, 3, 10, 5));
        assert_eq!(term.backend().appended, 0);
        assert!(term.backend().scrolled_up.is_empty());
        term.backend().inner.assert_scrollback_empty();
        let screen = term.backend().screen();
        assert_eq!(&screen[0..3], &["t0", "t1", "t2"]);
        assert_eq!(&screen[3..8], &["u0", "u1", "u2", "c0", "c1"]);
    }

    /// 回归（真机回修五）：收缩把顶行清空进银行后再增长（reclaim 路径），
    /// prev buffer 必须继续镜像物理屏——曾被整个清空成「全空白」，diff 便
    /// 认定屏幕已空：新帧的空行不清底下的旧字、变短的行不清行尾。真机上
    /// 回合结束（status 行消失 → 收缩）后按 Ctrl+C（notice 行出现 → 增长）
    /// 稳定触发整屏错位重影。
    #[test]
    fn grow_after_shrink_repaints_over_every_stale_row() {
        let mut term = term(10, 12, 0);
        term.draw(4, paint(&["aaaa", "bbbb", "cccc", "dddd"]), None)
            .unwrap();
        // 收缩 1 行：视口底锚（y=1），顶行清空进银行。
        term.draw(3, paint(&["bbbb", "cccc", "dddd"]), None).unwrap();
        assert_eq!(term.gap_above, 1);
        assert_eq!(term.viewport(), Rect::new(0, 1, 10, 3));

        // 增长回 4 行走 reclaim：新帧在旧字位置对出一个空行和一个短行。
        term.draw(4, paint(&["eeee", "", "ffff", "gg"]), None).unwrap();

        assert_eq!(term.viewport(), Rect::new(0, 0, 10, 4));
        assert!(term.backend().scrolled_up.is_empty());
        assert_eq!(term.backend().appended, 0);
        let screen = term.backend().screen();
        assert_eq!(
            &screen[0..4],
            &["eeee", "", "ffff", "gg"],
            "空行清掉旧字、短行清掉行尾——diff 面向物理屏，不是空 buffer"
        );
    }

    #[test]
    fn resize_and_clear_visible_reset_the_bank() {
        let mut term = term(10, 12, 0);
        term.draw(8, paint(&["x"; 8]), None).unwrap();
        term.frame(Vec::new(), 2, paint(&["c0", "c1"]), None).unwrap();
        assert_eq!(term.gap_above, 6, "an empty flush keeps the bank");

        term.resize(Size::new(10, 12)).unwrap();
        assert_eq!(term.gap_above, 0);

        term.draw(8, paint(&["x"; 8]), None).unwrap();
        term.frame(Vec::new(), 2, paint(&["c0", "c1"]), None).unwrap();
        assert_eq!(term.gap_above, 6);
        term.clear_visible().unwrap();
        assert_eq!(term.gap_above, 0);
    }
}
