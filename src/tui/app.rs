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
//! [prompt]      ╭──╮ / `❯ {input}▋` / ╰──╯
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

use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
use futures_util::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Size;

use crate::tui::chat::{Chat, Row};
use std::collections::HashMap;
use std::sync::Arc;

use crate::tui::chrome;
use crate::tui::el;
use crate::tui::gfx;
use crate::tui::line::{Line, SegStyle};
use crate::tui::statics::pick_flush_mark;
use crate::tui::term::{HistoryItem, StdoutTerm, write_gfx};
use crate::tui::view;

/// Per-frame tick interval (spinner/thinking timing).
const TICK_MS: u64 = 33;
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
    /// Document row of the first content row in `rows` (before chrome).
    pub doc_start: usize,
    /// Number of leading rows that belong to the transcript content (the rest
    /// is chrome). Image placements only exist inside this span.
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
                doc_start: 0,
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
        // Map content rows back to document rows for image placement: the
        // `+N lines` omission hint is not a doc row; dropped rows above the
        // budget shift the doc start.
        let hidden_rows = usize::from(hidden > 0);
        let content_len = tail_len.saturating_sub(dropped);
        let doc_start = chat.tail_start + dropped.saturating_sub(hidden_rows);
        Self {
            rows,
            cursor,
            doc_start,
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

/// Newly settled rows → scrollback entries. The first row of an image block emits real kitty bytes (transfer +
/// placement + cursor advance); the sequence consumes the continuation rows, so they are skipped.
fn flush_items(chat: &Chat, width: usize, end: usize) -> Vec<HistoryItem> {
    let end = end.min(chat.doc.rows.len());
    if end <= chat.tail_start {
        return Vec::new();
    }
    let pending = &chat.doc.rows[chat.tail_start..end];
    let mut items = Vec::with_capacity(pending.len());
    for (i, row) in pending.iter().enumerate() {
        if let Some(img) = &row.line.image {
            if !image_block_head(pending, i) {
                continue;
            }
            if let (Some(cap), Some(meta)) = (chat.image_cap, chat.images.get(&img.url)) {
                let bytes = gfx::image_print_bytes(
                    &cap,
                    &meta.bytes,
                    img.cols,
                    img.rows,
                    gfx::image_id_for(&img.url),
                );
                items.push(HistoryItem::Raw {
                    bytes,
                    rows: u16::try_from(img.rows).unwrap_or(u16::MAX),
                });
                continue;
            }
        }
        items.push(HistoryItem::Line(view::history_line(
            row,
            chat.theme.text,
            width,
        )));
    }
    items
}

/// Whether this row is an image block's first row (continuation rows return false; boundaries are detected by url).
fn image_block_head(rows: &[Row], i: usize) -> bool {
    let Some(img) = &rows[i].line.image else {
        return false;
    };
    rows.get(i.wrapping_sub(1))
        .is_none_or(|prev| prev.line.image.as_ref().map(|p| &p.url) != Some(&img.url))
}

/// Image blocks fully visible in the frame's content area → screen-cell
/// placements (`origin_row` = the viewport's top screen row). Partially
/// visible or still-loading blocks stay as `#[image]` placeholder rows; tmux
/// placeholder mode is not supported in the live viewport yet, so it gets no
/// placements either.
fn desired_placements(
    frame: &Frame,
    cap: gfx::ImageCap,
    images: &HashMap<String, Arc<crate::ui::ImageMeta>>,
    origin_row: u16,
) -> Vec<gfx::Placement> {
    if cap.mode != gfx::ImageMode::Direct {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < frame.content_len {
        let Some(img) = &frame.rows[i].line.image else {
            i += 1;
            continue;
        };
        if !image_block_head(&frame.rows, i) {
            i += 1;
            continue;
        }
        let block_rows = img.rows;
        let end = i + block_rows;
        if end <= frame.content_len
            && let Some(meta) = images.get(&img.url)
        {
            out.push(gfx::Placement {
                id: gfx::placement_id(&img.url, frame.doc_start + i),
                url: img.url.clone(),
                cols: meta.cols,
                rows: meta.rows,
                row: origin_row.saturating_add(u16::try_from(i).unwrap_or(u16::MAX)),
                col: 0,
            });
        }
        i = end;
    }
    out
}

/// Chrome height, measured by rendering the tree (never predicted — the same
/// source the frame assembler draws from).
fn chrome_height(chat: &Chat, width: usize, fullscreen: bool) -> usize {
    el::height(chrome::chrome(chat, width, fullscreen))
}

/// Key dispatch. In inline mode ctrl+o toggles expand/collapse (CC non-fullscreen semantics);
/// neither direction touches the already-printed scrollback: expand = replay the whole transcript and freeze it
/// into scrollback (readable by scrolling up); collapse = fold back to aggregates, then close up like resize (clear-redraw +
/// rehydration). All other keys (including Ctrl+C's interrupt/clear/quit three states) go to
/// [`Chat`]; quitting is expressed via `chat.exit`.
fn dispatch_key(chat: &mut Chat, key: KeyEvent, inline: bool) {
    if key.kind == KeyEventKind::Release {
        return;
    }
    if inline && key.code == KeyCode::Char('o') && key.modifiers.contains(KeyModifiers::CONTROL) {
        if chat.transcript_fully_expanded() {
            if chat.collapse_transcript() {
                // Cancel the not-yet-rendered replay (pressing twice = net effect of collapse),
                // clear the visible screen and redraw by rehydrating to the collapsed height — the expanded
                // replay rows on screen stay only in scrollback.
                chat.dump_transcript = false;
                chat.force_redraw = true;
                let chrome_len = chrome_height(chat, chat.width, false);
                let budget = chat.height.saturating_sub(2).saturating_sub(chrome_len);
                chat.rehydrate(chat.width, budget);
            }
        } else {
            chat.expand_transcript();
        }
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
        chat.reconcile_scroll(viewport);
        chat.build_rows(width);
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
    let mut layer = gfx::PlacementLayer::default();
    let mut pending_resize: Option<(Size, Instant)> = None;

    loop {
        tokio::select! {
            event = events.next() => match event {
                Some(Ok(Event::Key(key))) => {
                    dispatch_key(&mut chat, key, true);
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

        // Entity view (ctrl+g then Enter): the alternate-screen modal takes over; afterwards, a deterministic
        // redraw goes through the resize channel (clear + rehydrate, without guessing whether alt-screen restore works).
        if let Some(open) = chat.open_entity.take() {
            crate::tui::entity::run_entity_modal(&mut chat, &mut events, open, false).await?;
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
            term.write_gfx(&layer.clear())?;
        }

        let size = term.size();
        rebuild(&mut chat, size, false);

        // Lazy flush (composited with drawing into one `term.frame` batch): freeze only the settled segments
        // whose start row has crossed the window top — fully visible settled segments stay in the live doc
        // for re-layout at any time. Rows freed by a shrinking viewport go into the gap bank and frozen rows
        // are written into them right away, so settling migrates without flicker or blank bands. The cursor
        // advances per segment — even an image-only continuation segment (no items) must advance, or the next frame would redraw it.
        let mut items = Vec::new();
        if std::mem::take(&mut chat.dump_transcript) {
            // ctrl+o full replay: the cursor has rewound and the doc fully rebuilt from the welcome card (everything
            // expanded); the settled part freezes into scrollback in one go — the user scrolls up to see it all,
            // while the dynamic tail stays in the viewport as usual.
            if let Some(mark) = chat.doc.settled_marks.last().copied() {
                items = flush_items(&chat, size.width as usize, mark.row_end);
                chat.advance_flushed_upto(mark);
            }
        } else {
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
            if let Some(mark) = pick_flush_mark(&chat.doc.settled_marks, chat.tail_start, win_start)
            {
                items = flush_items(&chat, size.width as usize, mark.row_end);
                chat.advance_flushed_upto(mark);
            }
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

        // Live-viewport image placements: loaded, fully visible image blocks
        // render immediately instead of waiting for the scrollback flush.
        if let Some(cap) = chat.image_cap {
            let placements = desired_placements(&frame, cap, &chat.images, term.viewport_top());
            term.write_gfx(&layer.sync(&chat.images, &placements))?;
        }
        if chat.exit {
            break;
        }
    }

    term.write_gfx(&layer.clear())?;
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
            doc_start: 0,
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
        doc_start: chat.scroll,
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
    let mut layer = gfx::PlacementLayer::default();

    loop {
        tokio::select! {
            event = events.next() => match event {
                Some(Ok(Event::Key(key))) => {
                    dispatch_key(&mut chat, key, false);
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
                    // Resize purges the terminal's image store (ratatui's
                    // autoresize also clears the screen); the placement layer's
                    // transmit cache is now lies. Route through force_redraw:
                    // clear + drop the cache + retransmit everything visible.
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

        // Entity view: already on the alternate screen, the modal takes over the canvas directly; full repaint after return.
        if let Some(open) = chat.open_entity.take() {
            crate::tui::entity::run_entity_modal(&mut chat, &mut events, open, true).await?;
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
            write_gfx(terminal.backend_mut(), &layer.clear())?;
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

        // Live-viewport image placements on the alternate screen.
        if let Some(cap) = chat.image_cap {
            let placements = desired_placements(&frame, cap, &chat.images, 0);
            write_gfx(
                terminal.backend_mut(),
                &layer.sync(&chat.images, &placements),
            )?;
        }
        if chat.exit {
            break;
        }
    }

    write_gfx(terminal.backend_mut(), &layer.clear())?;
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
            assert_eq!(hidden, total - visible, "省略数 = 未显示行数");
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
        chat.push_warning("mcp 连接失败".to_string());
        let frame = Frame::assemble(&chat, size(60, 6));
        assert_eq!(frame.rows.len(), 4, "height-2 上限");
        let text: Vec<String> = frame.rows.iter().map(row_text).collect();
        // The dropped rows are the top ones (status/warning); the input and footer stay.
        assert!(
            text.last().is_some_and(|l| l.contains("ctrl+o to expand")),
            "{text:?}"
        );
        assert!(
            text.iter().any(|l| l.starts_with('╰')),
            "输入框下边框仍在: {text:?}"
        );
        assert!(
            text.iter().any(|l| l.starts_with('╭')),
            "输入框上边框仍在: {text:?}"
        );
    }

    /// The frame caret lands on the input row's `▋` (still aligned after the assembly offsets).
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
        assert_eq!(row, "❯ hello▋");
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
        assert_eq!(items.len(), 2, "只落定稿前缀");
        let HistoryItem::Line(first) = &items[0] else {
            panic!("text row");
        };
        assert_eq!(history_text(first), "first");
        let HistoryItem::Line(bubble) = &items[1] else {
            panic!("bubble row");
        };
        assert_eq!(text_width(&history_text(bubble)), 40, "气泡满行");
    }

    /// Image blocks: the head emits bytes (occupying rows rows), continuations are skipped; without capability, fall back to the placeholder text.
    #[test]
    fn flush_items_emit_one_payload_per_image_block() {
        let mut chat = chat_at(40, 24);
        let img = |url: &str| Line {
            segs: Vec::new(),
            image: Some(ImageRef {
                url: url.into(),
                cols: 4,
                rows: 2,
            }),
        };
        chat.doc.rows = vec![
            Row::new(img("a.png")),
            Row::new(img("a.png")),
            Row::new(Line::plain("text")),
            Row::new(img("b.png")),
        ];
        chat.doc.settled = 4;
        // No capability/cache: the block head falls back to the `#[image]` placeholder; continuations emit nothing.
        let items = flush_items(&chat, 40, chat.doc.settled);
        assert_eq!(items.len(), 3);
        let HistoryItem::Line(head) = &items[0] else {
            panic!("placeholder row");
        };
        assert_eq!(history_text(head), view::IMAGE_PLACEHOLDER);

        // Capable + loaded: one payload per block, row count = image row count.
        chat.image_cap = Some(crate::tui::gfx::ImageCap::default_cells());
        chat.images.insert(
            "a.png".into(),
            std::sync::Arc::new(crate::ui::ImageMeta {
                cols: 4,
                rows: 2,
                bytes: b"png".to_vec(),
            }),
        );
        let items = flush_items(&chat, 40, chat.doc.settled);
        assert_eq!(items.len(), 3, "块内续行不重复落盘");
        match &items[0] {
            HistoryItem::Raw { bytes, rows } => {
                assert_eq!(*rows, 2, "占两行");
                assert!(!bytes.is_empty());
            }
            HistoryItem::Line(_) => panic!("image head should be raw bytes"),
        }
    }

    /// Block head/continuation detection (an image block emits bytes exactly once).
    #[test]
    fn image_block_head_detects_block_boundaries() {
        let img = |url: &str| Line {
            segs: Vec::new(),
            image: Some(ImageRef {
                url: url.to_string(),
                cols: 10,
                rows: 3,
            }),
        };
        let rows = vec![
            Row::new(img("a.png")),
            Row::new(img("a.png")),
            Row::new(img("a.png")),
            Row::new(Line::plain("x")),
            Row::new(img("b.png")),
            Row::new(img("b.png")),
        ];
        assert!(image_block_head(&rows, 0), "块首");
        assert!(!image_block_head(&rows, 1), "续行");
        assert!(!image_block_head(&rows, 2), "续行");
        assert!(!image_block_head(&rows, 3), "普通行");
        assert!(image_block_head(&rows, 4), "新块首");
        assert!(!image_block_head(&rows, 5), "新块续行");
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
            "首帧含欢迎卡: {text:?}"
        );

        let items = flush_items(&chat, 80, chat.doc.settled);
        assert!(
            items.iter().any(|item| match item {
                HistoryItem::Line(line) => history_text(line).contains("Welcome back"),
                HistoryItem::Raw { .. } => false,
            }),
            "欢迎卡进 scrollback"
        );
        chat.advance_flushed();

        let text: Vec<String> = Frame::assemble(&chat, size(80, 24))
            .rows
            .iter()
            .map(row_text)
            .collect();
        assert!(
            !text.iter().any(|l| l.contains("Welcome back")),
            "落盘之后不再重画: {text:?}"
        );
        assert!(
            text.iter().any(|l| l.contains("? for shortcuts")),
            "chrome 仍在"
        );
    }

    /// The flush cursor counts by message segment: width changes alter every row number without reprinting.
    #[test]
    fn flush_cursor_survives_a_width_change() {
        let mut chat = chat_at(80, 24);
        chat.messages.push(crate::tui::chat::UiMessage {
            role: crate::tui::chat::Role::User,
            text: "一条足够长的用户消息，宽度变化后折行数会变".repeat(2),
            activities: Vec::new(),
            insert_points: Vec::new(),
            groups: Vec::new(),
            group_of: Vec::new(),
        });
        chat.dirty = true;
        rebuild(&mut chat, size(80, 24), false);
        let first = flush_items(&chat, 80, chat.doc.settled);
        assert!(!first.is_empty(), "首轮落盘欢迎卡 + 消息");
        chat.advance_flushed();
        // Another round at the same width: no new settled content → zero items.
        assert!(
            flush_items(&chat, 80, chat.doc.settled).is_empty(),
            "不重复落盘"
        );
        // Narrower rebuild: the segment cursor is unchanged, so still nothing new to flush.
        chat.dirty = true;
        rebuild(&mut chat, size(40, 24), false);
        assert!(
            flush_items(&chat, 40, chat.doc.settled).is_empty(),
            "宽度变化不会让已落盘的段再打印一次"
        );
    }

    /// Inline ctrl+o: full replay — the flush cursor rewinds + the replay flag is set; the replay frame
    /// freezes every settled segment into scrollback, leaving only the dynamic tail and chrome in the viewport.
    #[test]
    fn ctrl_o_replays_the_full_transcript_inline() {
        let mut chat = chat_at(80, 24);
        let key = |code, modifiers| KeyEvent::new(code, modifiers);
        // Empty session, everything on screen → no-op: no characters inserted, no replay.
        chat.set_input("hi");
        dispatch_key(
            &mut chat,
            key(KeyCode::Char('o'), KeyModifiers::CONTROL),
            true,
        );
        assert_eq!(chat.input, "hi", "ctrl+o 未插入字符");
        assert!(!chat.dump_transcript, "屏上已是全貌，无需重放");

        // Esc always passes through (menu exits happen inside on_key).
        chat.set_input("/model");
        chat.submit();
        assert!(chat.model_menu.is_some(), "菜单已打开");
        dispatch_key(&mut chat, key(KeyCode::Esc, KeyModifiers::empty()), true);
        assert!(chat.model_menu.is_none(), "Esc 经 gate 退出菜单");

        // A message has flushed → ctrl+o requests the replay; simulate the replay frame: rebuild the full doc
        // and freeze everything up to the last checkpoint.
        chat.messages.push(crate::tui::chat::UiMessage {
            role: crate::tui::chat::Role::Assistant,
            text: "reply".into(),
            activities: Vec::new(),
            insert_points: Vec::new(),
            groups: Vec::new(),
            group_of: Vec::new(),
        });
        chat.build_rows(80);
        chat.advance_flushed();
        dispatch_key(
            &mut chat,
            key(KeyCode::Char('o'), KeyModifiers::CONTROL),
            true,
        );
        assert!(chat.dump_transcript, "已落盘内容 → 重放");
        assert!(chat.force_redraw, "重放帧先清可见屏（置顶）");
        assert!(chat.dirty, "重放帧前必然重建");
        chat.dirty = false;
        chat.build_rows(80);
        let mark = chat
            .doc
            .settled_marks
            .last()
            .copied()
            .expect("全量文档有检查点");
        let items = flush_items(&chat, 80, mark.row_end);
        let texts: Vec<String> = items
            .iter()
            .filter_map(|item| match item {
                HistoryItem::Line(line) => Some(history_text(line)),
                HistoryItem::Raw { .. } => None,
            })
            .collect();
        assert!(
            texts.iter().any(|l| l.contains("Welcome")),
            "重放从欢迎卡开始: {texts:?}"
        );
        assert!(
            texts.iter().any(|l| l.contains("reply")),
            "重放含已落盘消息: {texts:?}"
        );
        chat.advance_flushed_upto(mark);
        chat.build_rows(80);
        assert!(chat.doc.rows.is_empty(), "重放后活文档只剩动态尾部");
    }

    /// Release events do not re-trigger (they occur when the terminal reports enhanced keyboards).
    #[test]
    fn key_release_is_ignored() {
        let mut chat = chat_at(80, 24);
        let mut key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::empty());
        key.kind = KeyEventKind::Release;
        dispatch_key(&mut chat, key, true);
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
            msg: "登录已失效，请重新配置凭据后重试。".to_string(),
            level: ErrorLevel::Full,
            context: ErrorContext::LongTurn,
        });

        let frame = fullscreen_frame(&chat, size(80, 24));
        let text: Vec<String> = frame.rows.iter().map(row_text).collect();
        assert!(text.iter().any(|line| line.contains("出错了")), "{text:?}");
        assert!(
            text.iter().any(|line| line.contains("code=AUTH_REQUIRED")),
            "{text:?}"
        );
        assert!(
            !text
                .iter()
                .any(|line| line.starts_with('╭') || line.starts_with('╰')),
            "全屏错误态不应露出输入框: {text:?}"
        );
        assert!(frame.cursor.is_none(), "全屏错误态隐藏输入光标");
        assert_eq!(frame.content_len, 0, "错误态无内容区");
    }

    /// Loaded, fully visible image blocks produce one placement at the right
    /// screen position; partial blocks, unloaded images, continuation rows and
    /// tmux mode produce nothing.
    #[test]
    fn desired_placements_only_for_fully_visible_loaded_blocks() {
        use crate::tui::line::ImageRef;
        let img_line = |url: &str, rows: usize| Line {
            segs: Vec::new(),
            image: Some(ImageRef {
                url: url.to_string(),
                cols: 4,
                rows,
            }),
        };
        let cap = crate::tui::gfx::ImageCap::default_cells(); // Direct mode
        let images = HashMap::from([(
            "a.png".to_string(),
            Arc::new(crate::ui::ImageMeta {
                cols: 4,
                rows: 2,
                bytes: b"png".to_vec(),
            }),
        )]);

        // Block fully inside content (rows 2..4 of a 6-row content area).
        let frame = Frame {
            rows: vec![
                Row::new(Line::plain("t")),
                Row::new(Line::plain("t")),
                Row::new(img_line("a.png", 2)),
                Row::new(img_line("a.png", 2)),
                Row::new(Line::plain("t")),
                Row::new(Line::plain("t")),
            ],
            cursor: None,
            doc_start: 10,
            content_len: 6,
        };
        let placements = desired_placements(&frame, cap, &images, 3);
        assert_eq!(placements.len(), 1, "恰好一个完整可见块");
        assert_eq!(placements[0].url, "a.png");
        assert_eq!(
            (placements[0].row, placements[0].col),
            (3 + 2, 0),
            "锚定屏幕单元格：视口顶 + 内容行"
        );
        assert_eq!(
            placements[0].id,
            crate::tui::gfx::placement_id("a.png", 10 + 2),
            "实例 id 锚定 doc 行"
        );

        // Partially visible (block clipped at the bottom of the content area) → skipped.
        let clipped = Frame {
            rows: vec![
                Row::new(Line::plain("t")),
                Row::new(Line::plain("t")),
                Row::new(img_line("a.png", 2)),
                Row::new(img_line("a.png", 2)),
            ],
            cursor: None,
            doc_start: 0,
            content_len: 3,
        };
        assert!(
            desired_placements(&clipped, cap, &images, 0).is_empty(),
            "被裁剪的块不放置"
        );

        // Unloaded url → skipped.
        let unloaded = Frame {
            rows: vec![Row::new(img_line("missing.png", 1))],
            cursor: None,
            doc_start: 0,
            content_len: 1,
        };
        assert!(desired_placements(&unloaded, cap, &images, 0).is_empty());

        // tmux placeholder mode → no viewport placements.
        let tmux = crate::tui::gfx::ImageCap {
            mode: crate::tui::gfx::ImageMode::TmuxPlaceholder,
            ..crate::tui::gfx::ImageCap::default_cells()
        };
        assert!(
            desired_placements(&frame, tmux, &images, 3).is_empty(),
            "tmux 占位模式视口不放置"
        );
    }

    /// Feedback loop for "images render live, not as `#[image]`": a loaded,
    /// fully visible image block must yield a placement with transmit bytes on
    /// the FIRST assembled frame after load — inline and fullscreen alike,
    /// with the screen row anchored to the frame position.
    #[test]
    fn loaded_image_block_placement_on_first_frame_inline_and_fullscreen() {
        let meta = Arc::new(crate::ui::ImageMeta {
            cols: 4,
            rows: 2,
            bytes: b"png".to_vec(),
        });
        let img = |url: &str| Line {
            segs: Vec::new(),
            image: Some(ImageRef {
                url: url.to_string(),
                cols: 4,
                rows: 2,
            }),
        };
        for fullscreen in [false, true] {
            let mut chat = chat_at(80, 30);
            chat.image_cap = Some(crate::tui::gfx::ImageCap::default_cells());
            chat.images.insert("a.png".to_string(), meta.clone());
            chat.doc.rows = vec![
                Row::new(Line::plain("hi")),
                Row::new(img("a.png")),
                Row::new(img("a.png")),
                Row::new(Line::plain("tail")),
            ];
            let frame = if fullscreen {
                fullscreen_frame(&chat, size(80, 30))
            } else {
                Frame::assemble(&chat, size(80, 30))
            };
            let origin: u16 = if fullscreen { 0 } else { 7 };
            let cap = chat.image_cap.unwrap();
            let placements = desired_placements(&frame, cap, &chat.images, origin);
            assert_eq!(placements.len(), 1, "fullscreen={fullscreen} 首帧即放置");
            assert_eq!(
                (placements[0].row, placements[0].col),
                (origin + 1, 0),
                "fullscreen={fullscreen} 锚定帧内屏幕行"
            );
            let mut layer = crate::tui::gfx::PlacementLayer::default();
            let ops = layer.sync(&chat.images, &placements);
            assert_eq!(ops.len(), 1, "fullscreen={fullscreen}");
            assert_eq!(ops[0].at, Some((origin + 1, 0)));
            assert!(
                String::from_utf8_lossy(&ops[0].bytes).contains("a=T"),
                "fullscreen={fullscreen} 首帧传输+放置"
            );
        }
    }

    /// Fullscreen frames carry the doc row of their first content row so the
    /// placement layer can anchor instance ids.
    #[test]
    fn fullscreen_frame_tracks_doc_start() {
        let mut chat = chat_at(80, 24);
        chat.dirty = true;
        rebuild(&mut chat, size(80, 24), true);
        chat.scroll = 7;
        let frame = fullscreen_frame(&chat, size(80, 24));
        assert_eq!(frame.doc_start, 7);
        assert_eq!(
            frame.content_len + chrome_height(&chat, 80, true),
            24,
            "内容区 + chrome = 屏高"
        );
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
            .expect("错误行可见");
        assert!(
            text[error_at + 1].starts_with('╭'),
            "错误行钉在输入框上方: {:?}",
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
        chat.push_warning("mcp 连接失败".to_string());
        chat.help_visible = true;
        let frame = fullscreen_frame(&chat, size(60, 6));
        assert!(frame.rows.len() <= 6, "不超过屏高");
        let text: Vec<String> = frame.rows.iter().map(row_text).collect();
        assert!(
            text.iter().any(|l| l.starts_with('╰')),
            "输入框下边框仍在: {text:?}"
        );
        assert!(
            text.last().is_some_and(|l| l.contains("ctrl+o to expand")),
            "footer 仍在: {text:?}"
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
        assert!(!chat.doc.settled_marks.is_empty(), "欢迎卡有定稿检查点");
        let chrome_len = chrome_height(&chat, 80, false);
        let (win_start, _) = tail_window(chat.doc.rows.len(), chat.tail_start, chrome_len, 24);
        assert_eq!(
            pick_flush_mark(&chat.doc.settled_marks, chat.tail_start, win_start),
            None,
            "装得下就不冻结——欢迎卡留在活文档里可重排"
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
            "前提成立：瞬态行确实把窗口挤过了欢迎卡"
        );

        // The production path excludes transient rows: the welcome card stays live.
        let persistent = total - chat.doc.transient_rows;
        let (win_start, _) = tail_window(persistent, chat.tail_start, chrome_len, 24);
        assert_eq!(
            pick_flush_mark(&chat.doc.settled_marks, chat.tail_start, win_start),
            None,
            "瞬态列表只是暂时盖住内容，不是驱逐"
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
        assert_eq!(chat.flushed_segments, 1, "欢迎卡已落盘");
        chat.dirty = true;
        rebuild(&mut chat, size(80, 24), false);
        assert!(chat.doc.rows.is_empty(), "落盘后活文档为空");

        // Budget is enough: pull the welcome card back (users accept the duplicates when scrolling up).
        chat.rehydrate(80, 24);
        assert_eq!(chat.flushed_segments, 0, "容量够就回灌");
        chat.dirty = true;
        rebuild(&mut chat, size(80, 24), false);
        assert_eq!(chat.doc.rows.len(), welcome_rows, "欢迎卡回到活文档");

        // Not enough budget: rehydration would overflow → roll back, keeping the flushed state.
        chat.advance_flushed();
        chat.rehydrate(80, welcome_rows.saturating_sub(1));
        assert_eq!(chat.flushed_segments, 1, "装不下就不取回");
    }
}
