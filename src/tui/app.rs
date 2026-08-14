//! Event loop and frame assembly.
//!
//! Shared fullscreen/inline layout, top to bottom — everything below the
//! transcript is "chrome" (declared in [`crate::tui::chrome`] as an element
//! tree), and chrome rows are *rendered*, never predicted:
//!
//! ```text
//! [transcript]  live tail only (settled rows already went to scrollback)
//! [status]      `✻ Working… (esc to interrupt · 3s)`
//! [tasks]       todo · N/M tasks
//! [warning]     `⚠ …`
//! [help]        `?` panel
//! [prompt]      ╭──╮ / `❯ {input}` (the real terminal cursor sits in it) / ╰──╯
//! [search]      `(reverse-i-search)…`
//! [queue]       `> queued message`
//! [suggestions] slash menu / `/model` picker
//! [notice]      `Press ctrl-c again to exit`
//! [footer]      mode badge · hints · model
//! [ask]         `Waiting for permission…`
//! ```
//!
//! Two invariants carry the whole design:
//!
//! 1. **Settled rows are written once.** `chat.doc.settled` marks the prefix
//!    that can no longer change; it goes out through
//!    [`crate::tui::term::InlineTerm::insert_history`] and `advance_flushed` moves the cursor
//!    past it. Nothing above the viewport is ever repainted.
//! 2. **The frame is measured, not predicted.** [`Frame::assemble`] builds the
//!    row list and takes its length as the viewport height (clamped to
//!    terminal height − 1). There is no second chrome formula to drift out of
//!    sync with what is drawn.

use std::io::Stdout;
use std::time::{Duration, Instant};

use crossterm::event::{Event, EventStream, KeyEvent, KeyEventKind, MouseEvent, MouseEventKind};
use futures_util::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Size;

use crate::tui::chat::{Chat, Row};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::tui::chrome;
use crate::tui::el;
use crate::tui::gfx;
use crate::tui::line::{Line, SegStyle};
use crate::tui::statics::pick_flush_mark;
use crate::tui::term::{StdoutTerm, write_attention, write_transmits};
use crate::tui::view;
use ratatui::text::Line as TextLine;

/// Per-frame tick interval. Owned by the motion layer, which converts ticks to
/// the milliseconds every animation cadence is actually specified in (D87).
use crate::tui::motion::TICK_MS;
/// Disk-snapshot refresh interval for the task list (in ticks).
const TASKS_REFRESH_TICKS: u64 = 15;
/// Rows scrolled per mouse wheel notch (fullscreen only).
const WHEEL_ROWS: usize = 3;
/// Drag-resizing is an event storm: stay quiet this long before applying the new size and repainting. Painting at
/// the old width during the storm only piles more mis-width rows on screen (terminal reflow folds them into shards).
const RESIZE_QUIET_MS: u64 = 120;

/// Fullscreen host: the ready-made ratatui Terminal.
pub type FullscreenHost = Terminal<CrosstermBackend<Stdout>>;

/// One assembled frame: the rows to draw and where the caret sits in them.
pub struct Frame {
    pub rows: Vec<Row>,
    pub cursor: Option<(u16, u16)>,
    /// Number of leading rows that belong to the transcript content (the rest
    /// is chrome). Image references only exist inside this span.
    pub content_len: usize,
}

/// Inline tail window: returns (start row, hidden row count). The budget is the terminal height minus
/// chrome and a two-row margin — at least 2 screen rows always remain above the viewport top, so the DECSTBM
/// scroll region (which needs two rows) is always legal (same origin as term.rs's viewport cap).
fn tail_window(total: usize, tail_start: usize, chrome: usize, height: usize) -> (usize, usize) {
    let start = tail_start.min(total);
    let budget = height.saturating_sub(chrome).saturating_sub(2);
    let len = total - start;
    if budget == 0 {
        return (total, 0);
    }
    if len <= budget {
        return (start, 0);
    }
    // The omission hint takes a row of its own.
    let visible = budget - 1;
    (total - visible, len - visible)
}

impl Frame {
    /// The transcript-content prefix of `rows` (chrome and error rows
    /// excluded) — the only rows that can reference images.
    fn content(&self) -> &[Row] {
        &self.rows[..self.content_len.min(self.rows.len())]
    }

    /// Inline frame: dynamic tail (over budget → keep only the last rows + the omission hint) + chrome.
    /// The row count is the viewport height, so it is always ≤ terminal height - 2 (the DECSTBM region stays legal).
    /// #18: the full-flow error state (`last_error.level == Full`) covers the content area with a full-screen error,
    /// and the input caret is hidden (the user is on the error screen; the key layer handles primary actions).
    pub fn assemble(chat: &Chat, size: Size) -> Self {
        if let Some(err) = &chat.last_error
            && err.level == crate::error::ErrorLevel::Full
        {
            return Self {
                rows: el::render(chrome::error_screen(err, &chat.theme)).rows,
                cursor: None,
                content_len: 0,
            };
        }
        let width = size.width as usize;
        let height = size.height as usize;
        let chrome = el::render(chrome::chrome(chat, width, false));
        let (tail_start, hidden) = tail_window(
            chat.doc.rows.len(),
            chat.tail_start,
            chrome.rows.len(),
            height,
        );
        let mut rows: Vec<Row> = Vec::new();
        if hidden > 0 {
            rows.push(chrome::dim_row(format!("… +{hidden} lines"), &chat.theme));
        }
        rows.extend(chat.doc.rows[tail_start..].iter().cloned());
        let tail_len = rows.len();
        // #18 error row (Page/Field levels): generated from the structured `last_error`, highlighted in the error
        // color (A zone), appended at the end of the content area — no doc rebuild, no double display.
        if let Some(err) = &chat.last_error
            && err.level != crate::error::ErrorLevel::Full
        {
            rows.push(Row::new(Line::styled(
                format!("[error] code={} msg={}", err.code, err.msg),
                SegStyle::fg(chat.theme.error),
            )));
        }
        // The caret offset counts every row before the chrome block, error row
        // included (the old hand-counted `prompt_row` arithmetic skipped it,
        // parking the caret one row high whenever an error row showed).
        let pre_chrome = rows.len();
        rows.extend(chrome.rows);

        // Last line of defense: chrome itself can exceed the budget (very short terminals),
        // in which case drop the top rows — the input box and footer are the part that must stay.
        // Budget = height − 2: same as term.rs's viewport cap (two rows left on top,
        // so the DECSTBM scroll region is always legal).
        let budget = height.saturating_sub(2).max(1);
        let dropped = rows.len().saturating_sub(budget);
        if dropped > 0 {
            rows.drain(..dropped);
        }
        let cursor = chrome.caret.and_then(|(row, col)| {
            caret_position(pre_chrome + row, col, dropped, rows.len(), width)
        });
        let content_len = tail_len.saturating_sub(dropped);
        Self {
            rows,
            cursor,
            content_len,
        }
    }
}

/// Caret cell: shown only if it still lands on screen after the frame top dropped `dropped` rows.
fn caret_position(
    row: usize,
    col: usize,
    dropped: usize,
    rows: usize,
    width: usize,
) -> Option<(u16, u16)> {
    let y = row.checked_sub(dropped)?;
    if y >= rows || col >= width {
        return None;
    }
    Some((u16::try_from(col).ok()?, u16::try_from(y).ok()?))
}

/// Newly settled rows → scrollback lines. Image rows freeze as placeholder
/// cells like any other text ([`view::history_line`] → [`view::to_line`]);
/// the image data behind them is transmitted separately
/// ([`image_transmits`]).
fn flush_items(chat: &Chat, width: usize, end: usize) -> Vec<TextLine<'static>> {
    let end = end.min(chat.doc.rows.len());
    if end <= chat.tail_start {
        return Vec::new();
    }
    chat.doc.rows[chat.tail_start..end]
        .iter()
        .map(|row| view::history_line(row, chat.theme.text, width))
        .collect()
}

/// Concatenated transmit payloads for every loaded image referenced by `rows`
/// that the terminal does not hold yet. Position- and order-independent
/// (`U=1` virtual placement): the placeholder cells painted by the render
/// layer do the placing, whether they went out before or after the data —
/// so a block whose head row is scrolled off still transmits, keyed by any
/// of its rows.
pub(super) fn image_transmits(
    cap: gfx::ImageCap,
    images: &HashMap<String, Arc<crate::ui::ImageMeta>>,
    rows: &[Row],
    transmits: &mut gfx::Transmits,
) -> Vec<u8> {
    let mut out = Vec::new();
    for row in rows {
        let Some(img) = &row.line.image else {
            continue;
        };
        let Some(meta) = images.get(&img.url) else {
            continue;
        };
        let id = gfx::image_id_for(&img.url);
        if transmits.needs(id) {
            out.extend_from_slice(&gfx::transmit_bytes(
                &meta.bytes,
                meta.cols,
                meta.rows,
                id,
                cap.transport,
            ));
        }
    }
    out
}

/// The portrait data behind the sender bands' placeholder cells. Driven by the
/// set the row builders recorded rather than by scanning rows: a portrait is
/// composed *beside* text on its line (`gutter_cell`), so unlike a full-width
/// image block it carries no [`crate::tui::line::ImageRef`] for a sweep to find.
/// After a store purge this resends exactly the faces still referenced, in
/// scrollback as well as on screen.
fn avatar_transmits(
    cap: gfx::ImageCap,
    faces: &HashSet<usize>,
    transmits: &mut gfx::Transmits,
) -> Vec<u8> {
    let indices: Vec<usize> = faces.iter().copied().collect();
    crate::tui::avatar::transmits(&indices, &cap, transmits)
}

/// Chrome height, measured by rendering the tree (never predicted — the same
/// source the frame assembler draws from).
fn chrome_height(chat: &Chat, width: usize, fullscreen: bool) -> usize {
    el::height(chrome::chrome(chat, width, fullscreen))
}

/// Key dispatch. Every key (including Ctrl+C's interrupt/clear/quit three
/// states and Ctrl+O's transcript view) goes to [`Chat`]; quitting is expressed
/// via `chat.exit`, opening the pager via `chat.open_transcript`.
fn dispatch_key(chat: &mut Chat, key: KeyEvent) {
    if key.kind == KeyEventKind::Release {
        return;
    }
    // Dialog keys are handled inside on_key (single dispatch order for both
    // hosts) — the old extra ask_key call here gave inline a different key
    // priority than fullscreen for the same dialog.
    chat.on_key(key.code, key.modifiers);
}

/// Document rebuild (on size change or dirty state). viewport = terminal height - chrome rows,
/// from the same source as the actual assembly.
fn rebuild(chat: &mut Chat, size: Size, fullscreen: bool) {
    let width = size.width as usize;
    let height = size.height as usize;
    if chat.width != width || chat.height != height {
        chat.width = width;
        chat.height = height;
        chat.dirty = true;
    }
    let chrome = chrome_height(chat, width, fullscreen);
    let viewport = height.saturating_sub(chrome).max(1);
    if !chat.dirty && chat.viewport_height == viewport {
        return;
    }
    chat.viewport_height = viewport;
    if chat.dirty {
        chat.dirty = false;
        // Build first, reconcile second (D93). Reconciling against the document
        // as it stood *before* the rebuild pinned the scroll to the old last
        // row, so every batch of rows that arrived in one frame — a buffer
        // switch's divider and replay, most visibly — landed below the fold
        // even for a viewer who had never scrolled away from the bottom.
        chat.build_rows(width);
        chat.reconcile_scroll(viewport);
    }
}

/// Inline host: settled rows go into scrollback in one go; only the bottom viewport is repainted.
///
/// The host type is hard-coded here (instead of being generic over `Backend`): the driver's constraint on the backend
/// is tighter than `Backend` (it must write raw bytes); generifying would only blow up at integration time.
pub async fn run_inline(
    mut chat: Chat,
    mut expand_rx: tokio::sync::watch::Receiver<bool>,
    mut term: StdoutTerm,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut events = EventStream::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(TICK_MS));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut ticks: u64 = 0;
    let mut expand_open = true;
    let mut dirty = true;
    let mut transmits = gfx::Transmits::default();
    let mut pending_resize: Option<(Size, Instant)> = None;

    loop {
        tokio::select! {
            event = events.next() => match event {
                Some(Ok(Event::Key(key))) => {
                    dispatch_key(&mut chat, key);
                    dirty = true;
                }
                Some(Ok(Event::Paste(text))) => {
                    chat.on_paste(&text);
                    dirty = true;
                }
                Some(Ok(Event::Resize(width, height))) => {
                    // Debounce: rapid resizes only record the latest value, applied once things quiet down.
                    pending_resize = Some((Size::new(width, height), Instant::now()));
                }
                Some(Ok(_)) => {}
                // Reading events failed (or stdin closed): the session cannot
                // be driven any more.
                Some(Err(_)) | None => break,
            },
            _ = ticker.tick() => {
                if let Some((size, at)) = pending_resize
                    && at.elapsed() >= Duration::from_millis(RESIZE_QUIET_MS)
                {
                    pending_resize = None;
                    term.resize(size)?;
                    chat.width = size.width as usize;
                    chat.height = size.height as usize;
                    // Terminal reflow happens before the resize event arrives; the old frame's wrapped rows
                    // shift by an unknown amount (content can even scroll the whole screen) — do not guess geometry:
                    // clear the visible screen and redraw the whole window at the new width (via the Ctrl+L path).
                    // Rehydration pulls the content back to fill the screen losslessly; the old-geometry copies stay
                    // in scrollback (accept duplicates when scrolling up).
                    chat.force_redraw = true;
                    let chrome_len = chrome_height(&chat, size.width as usize, false);
                    let doc_budget = (size.height as usize)
                        .saturating_sub(2)
                        .saturating_sub(chrome_len);
                    chat.rehydrate(size.width as usize, doc_budget);
                    chat.dirty = true;
                    dirty = true;
                }
                if chat.needs_tick() {
                    chat.tick();
                    if chat.drain_all() {
                        ticks = 0;
                    }
                    // Skip disk reads while the task area is hidden.
                    if ticks.is_multiple_of(TASKS_REFRESH_TICKS) && chat.tasks_visible {
                        chat.refresh_tasks();
                    }
                    ticks = ticks.wrapping_add(1);
                    dirty = true;
                } else if !dirty {
                    // Idle: no animation, no pending events, no pending draw changes → zero writes.
                    continue;
                }
            },
            changed = expand_rx.changed(), if expand_open => {
                if changed.is_err() {
                    expand_open = false;
                } else {
                    if *expand_rx.borrow() {
                        chat.tasks_visible = true;
                        chat.tasks_auto = true;
                    }
                    chat.refresh_tasks();
                    dirty = true;
                }
            },
        }

        // Transcript view (Ctrl+O, D82): the same alternate-screen contract as the
        // entity modal. Scrollback is untouched while it is open — that is what
        // lets a fold be opened at all in a host that can never rewrite a printed
        // row — and the return goes through the resize channel so the inline
        // window is rebuilt rather than guessed at.
        if std::mem::take(&mut chat.open_transcript) {
            crate::tui::transcript::run_transcript_modal(&mut chat, &mut events, false).await?;
            if let Ok((w, h)) = crossterm::terminal::size() {
                pending_resize = Some((Size::new(w, h), Instant::now()));
            } else {
                chat.force_redraw = true;
            }
            chat.dirty = true;
            dirty = true;
        }

        // `$EDITOR` compose (ctrl+g / ctrl+x ctrl+e, D86). Unlike the pager
        // this hands the terminal to a foreign process, so the event stream is
        // replaced rather than reused; the return goes through the resize
        // channel for the same reason the pager's does — the editor may well
        // have been resized while it had the screen.
        if std::mem::take(&mut chat.open_editor) {
            crate::tui::composer::run_editor(&mut chat, &mut events);
            if let Ok((w, h)) = crossterm::terminal::size() {
                pending_resize = Some((Size::new(w, h), Instant::now()));
            } else {
                chat.force_redraw = true;
            }
            chat.dirty = true;
            dirty = true;
        }

        // Do not render before the resize storm quiets down (the terminal geometry has changed; old-width
        // frames only add noise); events are handled as usual and one frame catches up after the quiet.
        if pending_resize.is_some() {
            if chat.exit {
                break;
            }
            continue;
        }

        // Finish the current frame before quitting: the last screen stays in the terminal (inline exit does not clear).
        if !dirty {
            if chat.exit {
                break;
            }
            continue;
        }
        dirty = false;

        // ctrl+l: clear and repaint (recover from a garbled screen).
        if chat.force_redraw {
            chat.force_redraw = false;
            term.clear_visible()?;
            // The terminal may have purged its image store (resize routes
            // through here): forget what was transmitted, the next frame
            // retransmits whatever its placeholder cells reference.
            transmits.reset();
        }

        let size = term.size();
        rebuild(&mut chat, size, false);

        // Lazy flush (composited with drawing into one `term.frame` batch): freeze only the settled segments
        // whose start row has crossed the window top — fully visible settled segments stay in the live doc
        // for re-layout at any time. Rows freed by a shrinking viewport go into the gap bank and frozen rows
        // are written into them right away, so settling migrates without flicker or blank bands.
        let mut items = Vec::new();
        let mut flushed = None;
        let chrome_len = chrome_height(&chat, size.width as usize, false);
        // The window counts "persistent content": transient slash output (gone after TTL) squeezing the window
        // is no reason to freeze live content — it merely covers it temporarily, not evicts it.
        let persistent = chat.doc.rows.len().saturating_sub(chat.doc.transient_rows);
        let (win_start, _) = tail_window(
            persistent,
            chat.tail_start,
            chrome_len,
            size.height as usize,
        );
        if let Some(mark) = pick_flush_mark(&chat.doc.settled_marks, chat.tail_start, win_start) {
            flushed = Some((chat.tail_start, mark.row_end.min(chat.doc.rows.len())));
            items = flush_items(&chat, size.width as usize, mark.row_end);
            chat.advance_flushed_upto(mark);
        }

        let frame = Frame::assemble(&chat, size);
        let height = u16::try_from(frame.rows.len()).unwrap_or(u16::MAX).max(1);
        let fg = chat.theme.text;
        term.frame(
            items,
            height,
            |buf| {
                let area = buf.area;
                view::render_rows(&frame.rows, fg, buf, area);
            },
            frame.cursor,
        )?;

        // The image data behind the placeholder cells: transmit whatever the
        // frame or the freshly flushed rows reference and the terminal does
        // not hold yet (advance_flushed_upto moved the cursor, not the rows,
        // so the flushed slice indices are still valid).
        if let Some(cap) = chat.image_cap {
            let mut bytes = image_transmits(cap, &chat.images, frame.content(), &mut transmits);
            if let Some((start, end)) = flushed {
                bytes.extend_from_slice(&image_transmits(
                    cap,
                    &chat.images,
                    &chat.doc.rows[start..end],
                    &mut transmits,
                ));
            }
            bytes.extend_from_slice(&avatar_transmits(cap, &chat.faces, &mut transmits));
            term.write_transmits(&bytes)?;
        }
        // Attention channel (D79): bell / notification OSC / terminal title,
        // emitted after the frame so it never lands mid-diff.
        term.write_attention(&chat.notify.take())?;
        if chat.exit {
            break;
        }
    }

    term.finish()?;
    Ok(())
}

/// Assembles the alternate-screen canvas. Unlike [`Frame::assemble`], normal content
/// fills the terminal and pins chrome to the bottom.
fn fullscreen_frame(chat: &Chat, size: Size) -> Frame {
    if let Some(err) = &chat.last_error
        && err.level == crate::error::ErrorLevel::Full
    {
        return Frame {
            rows: el::render(chrome::error_screen(err, &chat.theme)).rows,
            cursor: None,
            content_len: 0,
        };
    }

    let width = size.width as usize;
    let height = size.height as usize;
    let chrome = el::render(chrome::chrome(chat, width, true));
    // Chrome taller than the screen (short terminal + a tall picker): drop rows
    // from the top and keep the bottom — the input box and footer must survive.
    // Same last line of defense as the inline assembler.
    let overflow = chrome.rows.len().saturating_sub(height);
    let mut chrome_rows = chrome.rows;
    if overflow > 0 {
        chrome_rows.drain(..overflow);
    }
    let chrome_start = height - chrome_rows.len();
    // #18 error row (Page/Field): pinned right above the input box. The
    // fullscreen host previously rendered these errors nowhere at all.
    let error_row = chat
        .last_error
        .as_ref()
        .filter(|err| err.level != crate::error::ErrorLevel::Full)
        .map(|err| {
            Row::new(Line::styled(
                format!("[error] code={} msg={}", err.code, err.msg),
                SegStyle::fg(chat.theme.error),
            ))
        });
    let content_rows = chrome_start.saturating_sub(usize::from(error_row.is_some()));
    let mut rows: Vec<Row> = chat
        .doc
        .rows
        .iter()
        .skip(chat.scroll)
        .take(content_rows)
        .cloned()
        .collect();
    rows.resize_with(content_rows, || Row::new(Line::plain("")));
    rows.extend(error_row);
    rows.extend(chrome_rows);
    let cursor = chrome.caret.and_then(|(row, col)| {
        let row = row.checked_sub(overflow)?;
        caret_position(chrome_start + row, col, 0, height, width)
    });
    Frame {
        rows,
        cursor,
        content_len: chrome_start,
    }
}

/// Fullscreen host: the whole document + in-app scrolling + mouse-click folding, input area pinned to the bottom.
pub async fn run_fullscreen(
    mut chat: Chat,
    mut expand_rx: tokio::sync::watch::Receiver<bool>,
    mut terminal: FullscreenHost,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut events = EventStream::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(TICK_MS));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut ticks: u64 = 0;
    let mut expand_open = true;
    let mut dirty = true;
    let mut transmits = gfx::Transmits::default();

    loop {
        tokio::select! {
            event = events.next() => match event {
                Some(Ok(Event::Key(key))) => {
                    dispatch_key(&mut chat, key);
                    dirty = true;
                }
                Some(Ok(Event::Paste(text))) => {
                    chat.on_paste(&text);
                    dirty = true;
                }
                Some(Ok(Event::Mouse(mouse))) => {
                    if mouse_event(&mut chat, mouse) {
                        dirty = true;
                    }
                }
                Some(Ok(Event::Resize(_, _))) => {
                    // Resize may purge the terminal's image store (ratatui's
                    // autoresize also clears the screen). Route through
                    // force_redraw: clear, forget transmits, retransmit what
                    // the repainted placeholder cells reference.
                    chat.force_redraw = true;
                    chat.dirty = true;
                    dirty = true;
                }
                Some(Ok(_)) => {}
                Some(Err(_)) | None => break,
            },
            _ = ticker.tick() => {
                if chat.needs_tick() {
                    chat.tick();
                    if chat.drain_all() {
                        ticks = 0;
                    }
                    if ticks.is_multiple_of(TASKS_REFRESH_TICKS) && chat.tasks_visible {
                        chat.refresh_tasks();
                    }
                    ticks = ticks.wrapping_add(1);
                    dirty = true;
                } else if !dirty {
                    continue;
                }
            },
            changed = expand_rx.changed(), if expand_open => {
                if changed.is_err() {
                    expand_open = false;
                } else {
                    if *expand_rx.borrow() {
                        chat.tasks_visible = true;
                        chat.tasks_auto = true;
                    }
                    chat.refresh_tasks();
                    dirty = true;
                }
            },
        }

        // Transcript view: already on the alternate screen, so the pager takes the
        // canvas over directly; full repaint after return.
        if std::mem::take(&mut chat.open_transcript) {
            crate::tui::transcript::run_transcript_modal(&mut chat, &mut events, true).await?;
            chat.force_redraw = true;
            chat.dirty = true;
            dirty = true;
        }

        // `$EDITOR` compose (D86): the suspend leaves the alternate screen and
        // the resume re-enters it, so the canvas is repainted from scratch.
        if std::mem::take(&mut chat.open_editor) {
            crate::tui::composer::run_editor(&mut chat, &mut events);
            chat.force_redraw = true;
            chat.dirty = true;
            dirty = true;
        }

        if !dirty {
            if chat.exit {
                break;
            }
            continue;
        }
        dirty = false;

        // ctrl+l: full repaint (recover from a garbled screen).
        if chat.force_redraw {
            chat.force_redraw = false;
            terminal.clear()?;
            // Resize routes through here and may have purged the terminal's
            // image store: forget transmits, the redraw below retransmits.
            transmits.reset();
        }

        let size = terminal.size()?;
        rebuild(&mut chat, size, true);
        let frame = fullscreen_frame(&chat, size);
        let fg = chat.theme.text;
        terminal.draw(|terminal_frame| {
            let area = terminal_frame.area();
            let buf = terminal_frame.buffer_mut();
            view::render_rows(&frame.rows, fg, buf, area);
            if let Some(position) = frame.cursor {
                terminal_frame.set_cursor_position(position);
            }
        })?;

        // The image data behind the frame's placeholder cells.
        if let Some(cap) = chat.image_cap {
            let mut bytes = image_transmits(cap, &chat.images, frame.content(), &mut transmits);
            bytes.extend_from_slice(&avatar_transmits(cap, &chat.faces, &mut transmits));
            write_transmits(terminal.backend_mut(), &bytes)?;
        }
        // Attention channel (D79). The fullscreen host has no inline driver, so
        // its single write point is the crossterm backend behind the Terminal.
        write_attention(terminal.backend_mut(), &chat.notify.take())?;
        if chat.exit {
            break;
        }
    }

    Ok(())
}

/// Fullscreen mouse: wheel scrolls, clicks fold/expand (clicked row number = scroll position + screen row).
fn mouse_event(chat: &mut Chat, mouse: MouseEvent) -> bool {
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            chat.auto_scroll = false;
            chat.scroll = chat.scroll.saturating_sub(WHEEL_ROWS);
            true
        }
        MouseEventKind::ScrollDown => {
            chat.auto_scroll = false;
            chat.scroll = chat.scroll.saturating_add(WHEEL_ROWS);
            true
        }
        MouseEventKind::Down(_) => {
            let doc_row = chat.scroll.saturating_add(mouse.row as usize);
            chat.doc_click(doc_row)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    use crate::tui::line::{ImageRef, text_width};
    use crate::tui::test_util::chat_at;

    /// Text of an assembled row.
    fn row_text(row: &Row) -> String {
        row.line.plain_text()
    }

    /// A flushed scrollback line's text.
    fn history_text(line: &ratatui::text::Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn size(width: u16, height: u16) -> Size {
        Size::new(width, height)
    }

    #[test]
    fn tail_window_keeps_the_frame_below_terminal_height() {
        let total = 100usize;
        for height in 7..40usize {
            let chrome = 4usize;
            let (start, hidden) = tail_window(total, 0, chrome, height);
            let visible = total - start;
            let frame = visible + usize::from(hidden > 0) + chrome;
            assert!(frame < height, "height={height} frame={frame}");
            assert_eq!(hidden, total - visible, "hidden count = rows not shown");
        }
        // Zero budget (chrome + two-row margin fill it): no tail row is drawn; the hidden count is zero.
        assert_eq!(tail_window(100, 0, 4, 6), (100, 0));
        // When content fits, nothing is omitted or clipped.
        assert_eq!(tail_window(3, 0, 4, 40), (0, 0));
        // The flushed prefix is outside the tail window.
        assert_eq!(tail_window(3, 2, 4, 40), (2, 0));
        // Chrome fills everything: the tail is empty (nothing is drawn if it does not fit; still never overflows).
        assert_eq!(tail_window(3, 0, 4, 4), (3, 0));
    }

    /// D93: a rebuild reconciles the scroll against the document it just built,
    /// not the one it replaced.
    ///
    /// Rows that arrive as a batch inside a single frame — a conversation
    /// switch's rule and replay, most visibly — used to land below the fold
    /// even for a viewer sitting at the bottom, because `max_scroll` had been
    /// computed before they existed.
    #[test]
    fn a_batch_of_new_rows_is_visible_on_the_frame_it_arrives() {
        let mut chat = chat_at(80, 24);
        for i in 0..80 {
            chat.messages.push(crate::tui::chat::UiMessage {
                role: crate::tui::chat::Role::User,
                text: format!("line {i}"),
                at: 0,
                activities: Vec::new(),
                insert_points: Vec::new(),
                groups: Vec::new(),
                group_of: Vec::new(),
            });
        }
        chat.dirty = true;
        rebuild(&mut chat, size(80, 24), true);
        let viewport = chat.viewport_height;
        assert_eq!(
            chat.scroll,
            chat.doc.rows.len().saturating_sub(viewport),
            "a settled document sits at its tail"
        );

        // A batch lands in one frame: the tail has to be the tail of the new
        // document, not of the one that was there when the frame started.
        for i in 0..20 {
            chat.messages.push(crate::tui::chat::UiMessage {
                role: crate::tui::chat::Role::User,
                text: format!("arrived {i}"),
                at: 0,
                activities: Vec::new(),
                insert_points: Vec::new(),
                groups: Vec::new(),
                group_of: Vec::new(),
            });
        }
        chat.dirty = true;
        rebuild(&mut chat, size(80, 24), true);
        assert_eq!(
            chat.scroll,
            chat.doc.rows.len().saturating_sub(chat.viewport_height),
            "the batch is on screen, not below the fold"
        );
        let tail: String = chat
            .doc
            .rows
            .iter()
            .skip(chat.scroll)
            .map(row_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(tail.contains("arrived 19"), "{tail}");
    }

    /// D93: switching conversation lands you at the tail, the way opening a
    /// chat anywhere does — including for a viewer who had scrolled up.
    #[test]
    fn switching_conversation_snaps_the_view_to_the_bottom() {
        use crate::tui::buffer::BufferId;

        let mut chat = chat_at(80, 24);
        chat.session.agents.insert(
            "scout",
            crate::agents::AgentKind::Hire,
            None,
            "test instance".to_string(),
            chat.session.clone(),
        );
        chat.refresh_conversations();
        for i in 0..80 {
            chat.messages.push(crate::tui::chat::UiMessage {
                role: crate::tui::chat::Role::User,
                text: format!("hub line {i}"),
                at: 0,
                activities: Vec::new(),
                insert_points: Vec::new(),
                groups: Vec::new(),
                group_of: Vec::new(),
            });
        }
        chat.dirty = true;
        rebuild(&mut chat, size(80, 24), true);

        // The reader is somewhere up the transcript, reading.
        chat.scroll = 3;
        chat.auto_scroll = false;

        chat.switch_to(BufferId::Dm("scout".to_string()));
        assert!(chat.auto_scroll, "a switch re-arms the stick");
        rebuild(&mut chat, size(80, 24), true);
        assert_eq!(
            chat.scroll,
            chat.doc.rows.len().saturating_sub(chat.viewport_height),
            "the rule and the replay are on screen"
        );
        let tail: String = chat
            .doc
            .rows
            .iter()
            .skip(chat.scroll)
            .map(row_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(tail.contains("── @scout ──"), "{tail}");
    }

    /// The inline host has no scroll offset to go stale: its window is computed
    /// from the tail every frame. Asserted rather than assumed, because it is
    /// the reason the D93 scroll fix is fullscreen-only.
    #[test]
    fn a_switch_leaves_the_inline_tail_on_screen() {
        use crate::tui::buffer::BufferId;

        let mut chat = chat_at(80, 24);
        chat.session.agents.insert(
            "scout",
            crate::agents::AgentKind::Hire,
            None,
            "test instance".to_string(),
            chat.session.clone(),
        );
        chat.refresh_conversations();
        for i in 0..80 {
            chat.messages.push(crate::tui::chat::UiMessage {
                role: crate::tui::chat::Role::User,
                text: format!("hub line {i}"),
                at: 0,
                activities: Vec::new(),
                insert_points: Vec::new(),
                groups: Vec::new(),
                group_of: Vec::new(),
            });
        }
        chat.dirty = true;
        rebuild(&mut chat, size(80, 24), false);

        chat.switch_to(BufferId::Dm("scout".to_string()));
        chat.dirty = true;
        rebuild(&mut chat, size(80, 24), false);
        let frame = Frame::assemble(&chat, size(80, 24));
        let text = frame
            .rows
            .iter()
            .map(row_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("── @scout ──"),
            "the rule the switch printed is in the live region: {text}"
        );
    }

    /// Frame height = the assembled row count, always < terminal height: no second chrome
    /// formula can drift from the actual assembly.
    #[test]
    fn frame_height_never_reaches_terminal_height() {
        let mut chat = chat_at(80, 24);
        chat.doc.rows = (0..200)
            .map(|i| Row::new(Line::plain(format!("r{i}"))))
            .collect();
        for height in 4..40u16 {
            chat.height = height as usize;
            let frame = Frame::assemble(&chat, size(80, height));
            assert!(
                frame.rows.len() < height as usize,
                "height={height} rows={}",
                frame.rows.len()
            );
        }
    }

    /// Very short terminals: when chrome itself exceeds the budget, keep the bottom (input + footer);
    /// the frame still never overflows.
    #[test]
    fn tiny_terminal_keeps_the_prompt_and_footer() {
        let mut chat = chat_at(60, 6);
        chat.busy = true;
        chat.push_warning("mcp connection failed".to_string());
        let frame = Frame::assemble(&chat, size(60, 6));
        assert_eq!(frame.rows.len(), 4, "height-2 cap");
        let text: Vec<String> = frame.rows.iter().map(row_text).collect();
        // The dropped rows are the top ones (status/warning); the input and footer stay.
        assert!(
            text.last().is_some_and(|l| l.contains("ctrl+o to expand")),
            "{text:?}"
        );
        assert!(
            text.iter().any(|l| l.starts_with('╰')),
            "input box bottom border still present: {text:?}"
        );
        assert!(
            text.iter().any(|l| l.starts_with('╭')),
            "input box top border still present: {text:?}"
        );
    }

    /// The frame caret lands one cell past the input text (still aligned after the assembly
    /// offsets), and no row draws a cursor glyph — the terminal cursor is the only caret.
    #[test]
    fn frame_cursor_points_at_the_caret() {
        let mut chat = chat_at(80, 24);
        chat.set_input("hello");
        chat.doc.rows = (0..5)
            .map(|i| Row::new(Line::plain(format!("r{i}"))))
            .collect();
        let frame = Frame::assemble(&chat, size(80, 24));
        let (x, y) = frame.cursor.expect("caret visible");
        assert_eq!(x, 7, "❯ + hello");
        let row = row_text(&frame.rows[y as usize]);
        assert_eq!(row, "❯ hello");
        assert!(
            !frame.rows.iter().any(|r| r.line.plain_text().contains('▋')),
            "no fake cursor glyph survives anywhere in the frame"
        );

        // Wide glyphs: the caret column counts display cells, not chars.
        chat.set_input("你好");
        let frame = Frame::assemble(&chat, size(80, 24));
        let (x, y) = frame.cursor.expect("caret visible");
        assert_eq!(x, 6, "❯ + two double-width glyphs");
        assert_eq!(row_text(&frame.rows[y as usize]), "❯ 你好");
    }

    /// Flushing: the settled prefix becomes scrollback entries; bubble rows fill the terminal width.
    #[test]
    fn flush_items_convert_settled_rows() {
        let mut chat = chat_at(40, 24);
        chat.doc.rows = vec![
            Row::new(Line::plain("first")),
            Row::bubble(Line::plain("❯ hi"), chat.theme.user_message_bg),
            Row::new(Line::plain("tail")),
        ];
        chat.doc.settled = 2;
        let items = flush_items(&chat, 40, chat.doc.settled);
        assert_eq!(items.len(), 2, "only the settled prefix lands");
        assert_eq!(history_text(&items[0]), "first");
        assert_eq!(
            text_width(&history_text(&items[1])),
            40,
            "bubble fills the row"
        );
    }

    fn img_row(url: &str, row: usize) -> Line {
        Line {
            segs: Vec::new(),
            image: Some(ImageRef {
                url: url.into(),
                cols: 4,
                rows: 2,
                row,
            }),
        }
    }

    /// Image rows freeze as placeholder cells — one line per row, every row
    /// carrying its own coordinates, exactly what the viewport painted.
    #[test]
    fn flush_items_freeze_image_rows_as_placeholder_cells() {
        let mut chat = chat_at(40, 24);
        chat.doc.rows = vec![
            Row::new(img_row("a.png", 0)),
            Row::new(img_row("a.png", 1)),
            Row::new(Line::plain("text")),
        ];
        chat.doc.settled = 3;
        let items = flush_items(&chat, 40, chat.doc.settled);
        assert_eq!(items.len(), 3, "image rows freeze one per row");
        let head = history_text(&items[0]);
        assert!(
            head.starts_with(crate::tui::gfx::PLACEHOLDER),
            "placeholder cell: {head:?}"
        );
        assert_ne!(
            history_text(&items[0]),
            history_text(&items[1]),
            "row diacritics change with the row index"
        );
        assert_eq!(history_text(&items[2]), "text");
    }

    /// The transmit layer sends each image once, keyed by any of its rows
    /// (a block cut at the top still transmits), and resets with the cache.
    #[test]
    fn image_transmits_send_each_image_once() {
        let cap = crate::tui::gfx::ImageCap::default_cells();
        let images = HashMap::from([
            (
                "a.png".to_string(),
                Arc::new(crate::ui::ImageMeta {
                    cols: 4,
                    rows: 2,
                    bytes: b"png".to_vec(),
                }),
            ),
            (
                "b.png".to_string(),
                Arc::new(crate::ui::ImageMeta {
                    cols: 4,
                    rows: 2,
                    bytes: b"png".to_vec(),
                }),
            ),
        ]);
        let mut transmits = crate::tui::gfx::Transmits::default();

        // A block whose head row is scrolled off: the continuation row alone
        // still keys the transmit.
        let rows = vec![
            Row::new(img_row("a.png", 1)),
            Row::new(Line::plain("t")),
            Row::new(img_row("b.png", 0)),
            Row::new(img_row("b.png", 1)),
            Row::new(img_row("missing.png", 0)),
        ];
        let bytes = image_transmits(cap, &images, &rows, &mut transmits);
        let s = String::from_utf8_lossy(&bytes);
        assert_eq!(
            s.matches("a=T,U=1").count(),
            2,
            "each image exactly once: {s}"
        );
        let id_a = crate::tui::gfx::image_id_for("a.png");
        let id_b = crate::tui::gfx::image_id_for("b.png");
        assert!(
            s.contains(&format!("i={id_a}")),
            "a.png's id is in the transmission"
        );
        assert!(
            s.contains(&format!("i={id_b}")),
            "b.png's id is in the transmission"
        );

        // Same rows again: the terminal already holds both images.
        assert!(
            image_transmits(cap, &images, &rows, &mut transmits).is_empty(),
            "already-transmitted images are not repeated"
        );
        // After a reset (resize purged the store) they transmit again.
        transmits.reset();
        assert!(
            !image_transmits(cap, &images, &rows, &mut transmits).is_empty(),
            "re-transmitted after reset"
        );

        // The tmux transport wraps every chunk in a passthrough envelope.
        let tmux = crate::tui::gfx::ImageCap {
            transport: crate::tui::gfx::Transport::Tmux,
            ..cap
        };
        let mut transmits = crate::tui::gfx::Transmits::default();
        let wrapped = image_transmits(tmux, &images, &rows, &mut transmits);
        assert!(
            String::from_utf8_lossy(&wrapped).starts_with("\x1bPtmux;"),
            "tmux transmissions go through passthrough"
        );
    }

    /// The core inline invariant: settled content flushes once; afterwards the viewport holds only the tail + chrome.
    #[test]
    fn flushed_rows_leave_the_viewport() {
        let mut chat = chat_at(80, 24);
        chat.dirty = true;
        rebuild(&mut chat, size(80, 24), false);
        let text: Vec<String> = Frame::assemble(&chat, size(80, 24))
            .rows
            .iter()
            .map(row_text)
            .collect();
        assert!(
            text.iter().any(|l| l.contains("Welcome back")),
            "first frame contains the welcome card: {text:?}"
        );

        let items = flush_items(&chat, 80, chat.doc.settled);
        assert!(
            items
                .iter()
                .any(|line| history_text(line).contains("Welcome back")),
            "the welcome card lands in scrollback"
        );
        chat.advance_flushed();

        let text: Vec<String> = Frame::assemble(&chat, size(80, 24))
            .rows
            .iter()
            .map(row_text)
            .collect();
        assert!(
            !text.iter().any(|l| l.contains("Welcome back")),
            "not redrawn after flushing: {text:?}"
        );
        assert!(
            text.iter().any(|l| l.contains("? for shortcuts")),
            "chrome is still there"
        );
    }

    /// The flush cursor counts by message segment: width changes alter every row number without reprinting.
    #[test]
    fn flush_cursor_survives_a_width_change() {
        let mut chat = chat_at(80, 24);
        chat.messages.push(crate::tui::chat::UiMessage {
            role: crate::tui::chat::Role::User,
            text: "a long-enough user message whose wrap count changes with the width".repeat(2),
            at: 0,
            activities: Vec::new(),
            insert_points: Vec::new(),
            groups: Vec::new(),
            group_of: Vec::new(),
        });
        chat.dirty = true;
        rebuild(&mut chat, size(80, 24), false);
        let first = flush_items(&chat, 80, chat.doc.settled);
        assert!(
            !first.is_empty(),
            "first round flushes the welcome card + the message"
        );
        chat.advance_flushed();
        // Another round at the same width: no new settled content → zero items.
        assert!(
            flush_items(&chat, 80, chat.doc.settled).is_empty(),
            "no duplicate flush"
        );
        // Narrower rebuild: the segment cursor is unchanged, so still nothing new to flush.
        chat.dirty = true;
        rebuild(&mut chat, size(40, 24), false);
        assert!(
            flush_items(&chat, 40, chat.doc.settled).is_empty(),
            "a width change never reprints an already-flushed segment"
        );
    }

    /// Ctrl+O requests the transcript view instead of rewriting the screen, and
    /// a permission dialog outranks it: the pager would bury the question that
    /// is blocking the turn.
    #[test]
    fn ctrl_o_requests_the_transcript_view() {
        let mut chat = chat_at(80, 24);
        let key = |code, modifiers| KeyEvent::new(code, modifiers);
        chat.set_input("hi");
        dispatch_key(&mut chat, key(KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert_eq!(chat.input, "hi", "ctrl+o does not insert characters");
        assert!(chat.open_transcript, "the host is asked to open the pager");

        // Esc always passes through (menu exits happen inside on_key).
        chat.open_transcript = false;
        chat.set_input("/model");
        chat.submit();
        assert!(chat.model_menu.is_some(), "menu is open");
        dispatch_key(&mut chat, key(KeyCode::Esc, KeyModifiers::empty()));
        assert!(
            chat.model_menu.is_none(),
            "Esc exits the menu through the gate"
        );

        // With a question on screen the key is inert.
        let (tx, _rx) = tokio::sync::oneshot::channel();
        chat.pending_ask = Some((
            crate::ui::PermissionRequest::new(
                "Allow Bash",
                "cargo test",
                vec![crate::ui::ASK_YES.into(), crate::ui::ASK_NO.into()],
            ),
            tx,
        ));
        dispatch_key(&mut chat, key(KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert!(
            !chat.open_transcript,
            "a pending dialog keeps priority over the pager"
        );
    }

    /// Release events do not re-trigger (they occur when the terminal reports enhanced keyboards).
    #[test]
    fn key_release_is_ignored() {
        let mut chat = chat_at(80, 24);
        let mut key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::empty());
        key.kind = KeyEventKind::Release;
        dispatch_key(&mut chat, key);
        assert!(chat.input.is_empty());
    }

    /// Full-flow errors must take over the real alternate-screen canvas too, not only
    /// the inline [`Frame::assemble`] seam.
    #[test]
    fn fullscreen_frame_presents_full_error_and_hides_prompt() {
        use crate::error::{ErrorContext, ErrorLevel};
        use crate::tui::chat::ErrorState;

        let mut chat = chat_at(80, 24);
        chat.last_error = Some(ErrorState {
            code: "AUTH_REQUIRED",
            msg: "login has expired; reconfigure the credentials and retry.".to_string(),
            level: ErrorLevel::Full,
            context: ErrorContext::LongTurn,
        });

        let frame = fullscreen_frame(&chat, size(80, 24));
        let text: Vec<String> = frame.rows.iter().map(row_text).collect();
        assert!(
            text.iter()
                .any(|line| line.contains("something went wrong")),
            "{text:?}"
        );
        assert!(
            text.iter().any(|line| line.contains("code=AUTH_REQUIRED")),
            "{text:?}"
        );
        assert!(
            !text
                .iter()
                .any(|line| line.starts_with('╭') || line.starts_with('╰')),
            "the fullscreen error state must not expose the input box: {text:?}"
        );
        assert!(
            frame.cursor.is_none(),
            "the fullscreen error state hides the input caret"
        );
        assert_eq!(frame.content_len, 0, "the error state has no content area");
    }

    /// Feedback loop for "images render live, not as `#[image]`": on the
    /// FIRST assembled frame after load — inline and fullscreen alike — the
    /// image rows are inside the frame's content span (so the render layer
    /// paints their placeholder cells) and the transmit layer sends the data.
    #[test]
    fn loaded_image_renders_and_transmits_on_first_frame_inline_and_fullscreen() {
        let meta = Arc::new(crate::ui::ImageMeta {
            cols: 4,
            rows: 2,
            bytes: b"png".to_vec(),
        });
        for fullscreen in [false, true] {
            let mut chat = chat_at(80, 30);
            chat.image_cap = Some(crate::tui::gfx::ImageCap::default_cells());
            chat.images.insert("a.png".to_string(), meta.clone());
            chat.doc.rows = vec![
                Row::new(Line::plain("hi")),
                Row::new(img_row("a.png", 0)),
                Row::new(img_row("a.png", 1)),
                Row::new(Line::plain("tail")),
            ];
            let frame = if fullscreen {
                fullscreen_frame(&chat, size(80, 30))
            } else {
                Frame::assemble(&chat, size(80, 30))
            };
            let image_rows = frame
                .content()
                .iter()
                .filter(|row| row.line.image.is_some())
                .count();
            assert_eq!(
                image_rows, 2,
                "fullscreen={fullscreen} image rows live in the content area"
            );

            let mut transmits = crate::tui::gfx::Transmits::default();
            let cap = chat.image_cap.expect("cap set above");
            let bytes = image_transmits(cap, &chat.images, frame.content(), &mut transmits);
            let s = String::from_utf8_lossy(&bytes);
            assert!(
                s.contains("a=T,U=1"),
                "fullscreen={fullscreen} the first frame transmits: {s}"
            );
            assert!(
                image_transmits(cap, &chat.images, frame.content(), &mut transmits).is_empty(),
                "fullscreen={fullscreen} the next frame does not retransmit"
            );
        }
    }

    /// Fullscreen frames split content from chrome so the transmit layer
    /// scans exactly the transcript span.
    #[test]
    fn fullscreen_frame_content_spans_screen_minus_chrome() {
        let mut chat = chat_at(80, 24);
        chat.dirty = true;
        rebuild(&mut chat, size(80, 24), true);
        chat.scroll = 7;
        let frame = fullscreen_frame(&chat, size(80, 24));
        assert_eq!(
            frame.content_len + chrome_height(&chat, 80, true),
            24,
            "content area + chrome = screen height"
        );
        assert_eq!(frame.content().len(), frame.content_len);
    }

    /// P0-7 regression: the fullscreen host renders Page/Field error rows
    /// (pinned above the input box) — it used to render them nowhere.
    #[test]
    fn fullscreen_frame_renders_page_error_row() {
        let mut chat = chat_at(80, 24);
        chat.last_error = Some(crate::tui::chat::ErrorState {
            code: "TIMEOUT",
            msg: "list_models timeout".into(),
            level: crate::error::ErrorLevel::Page,
            context: crate::error::ErrorContext::ShortSync,
        });
        let frame = fullscreen_frame(&chat, size(80, 24));
        let text: Vec<String> = frame.rows.iter().map(row_text).collect();
        let error_at = text
            .iter()
            .position(|l| l.contains("[error] code=TIMEOUT"))
            .expect("error row is visible");
        assert!(
            text[error_at + 1].starts_with('╭'),
            "error row pinned above the input box: {:?}",
            &text[error_at..error_at + 2]
        );
    }

    /// Fullscreen last line of defense: chrome taller than a short terminal
    /// drops rows from the top — the input box and footer must survive
    /// (the inline assembler has had this guard from day one).
    #[test]
    fn fullscreen_tiny_terminal_keeps_the_prompt_and_footer() {
        let mut chat = chat_at(60, 6);
        chat.busy = true;
        chat.push_warning("mcp connection failed".to_string());
        chat.help_visible = true;
        let frame = fullscreen_frame(&chat, size(60, 6));
        assert!(frame.rows.len() <= 6, "no taller than the screen");
        let text: Vec<String> = frame.rows.iter().map(row_text).collect();
        assert!(
            text.iter().any(|l| l.starts_with('╰')),
            "input box bottom border still present: {text:?}"
        );
        assert!(
            text.last().is_some_and(|l| l.contains("ctrl+o to expand")),
            "footer still present: {text:?}"
        );
    }

    /// Wheel scrolling and clicks (fullscreen).
    #[test]
    fn mouse_scrolls_and_clicks() {
        let mut chat = chat_at(80, 24);
        chat.scroll = 10;
        let wheel = |kind| MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::empty(),
        };
        assert!(mouse_event(&mut chat, wheel(MouseEventKind::ScrollUp)));
        assert_eq!(chat.scroll, 7);
        assert!(!chat.auto_scroll);
        assert!(mouse_event(&mut chat, wheel(MouseEventKind::ScrollDown)));
        assert_eq!(chat.scroll, 10);
    }

    /// Settled content stays live inside the window: a small doc freezes nothing, and width changes re-layout on rebuild.
    #[test]
    fn settled_rows_stay_live_while_they_fit() {
        let mut chat = chat_at(80, 24);
        chat.dirty = true;
        rebuild(&mut chat, size(80, 24), false);
        assert!(
            !chat.doc.settled_marks.is_empty(),
            "the welcome card has settled checkpoints"
        );
        let chrome_len = chrome_height(&chat, 80, false);
        let (win_start, _) = tail_window(chat.doc.rows.len(), chat.tail_start, chrome_len, 24);
        assert_eq!(
            pick_flush_mark(&chat.doc.settled_marks, chat.tail_start, win_start),
            None,
            "fits in the window → nothing freezes — the welcome card stays in the live document and can re-layout"
        );
    }

    /// Transient slash output (e.g. /resume lists) squeezes the window; it must not freeze live content.
    #[test]
    fn transient_slash_output_does_not_freeze_live_rows() {
        let mut chat = chat_at(80, 24);
        chat.slash_lines = (0..40).map(|i| format!("session-{i}")).collect();
        chat.dirty = true;
        rebuild(&mut chat, size(80, 24), false);
        assert_eq!(chat.doc.transient_rows, 40);
        let chrome_len = chrome_height(&chat, 80, false);
        let total = chat.doc.rows.len();

        // Regression guard: computing the window over the full doc would misjudge the welcome card as past the top.
        let (naive_start, _) = tail_window(total, chat.tail_start, chrome_len, 24);
        assert!(
            pick_flush_mark(&chat.doc.settled_marks, chat.tail_start, naive_start).is_some(),
            "precondition holds: the transient rows really squeeze the window past the welcome card"
        );

        // The production path excludes transient rows: the welcome card stays live.
        let persistent = total - chat.doc.transient_rows;
        let (win_start, _) = tail_window(persistent, chat.tail_start, chrome_len, 24);
        assert_eq!(
            pick_flush_mark(&chat.doc.settled_marks, chat.tail_start, win_start),
            None,
            "a transient list only temporarily covers content, it does not evict it"
        );
    }

    /// Rehydration: when capacity grows, pull flushed segments back for re-rendering; over budget, roll back.
    #[test]
    fn rehydrate_refills_the_window_after_capacity_growth() {
        let mut chat = chat_at(80, 24);
        chat.dirty = true;
        rebuild(&mut chat, size(80, 24), false);
        let welcome_rows = chat.doc.rows.len();
        chat.advance_flushed();
        assert_eq!(chat.flushed_segments, 1, "the welcome card has flushed");
        chat.dirty = true;
        rebuild(&mut chat, size(80, 24), false);
        assert!(
            chat.doc.rows.is_empty(),
            "the live document is empty after flushing"
        );

        // Budget is enough: pull the welcome card back (users accept the duplicates when scrolling up).
        chat.rehydrate(80, 24);
        assert_eq!(chat.flushed_segments, 0, "enough capacity → pulled back");
        chat.dirty = true;
        rebuild(&mut chat, size(80, 24), false);
        assert_eq!(
            chat.doc.rows.len(),
            welcome_rows,
            "the welcome card returns to the live document"
        );

        // Not enough budget: rehydration would overflow → roll back, keeping the flushed state.
        chat.advance_flushed();
        chat.rehydrate(80, welcome_rows.saturating_sub(1));
        assert_eq!(chat.flushed_segments, 1, "no room → not pulled back");
    }
}
