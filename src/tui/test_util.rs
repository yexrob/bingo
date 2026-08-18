//! Shared TUI test infrastructure: a test backend with assertable render
//! output.
//!
//! [`Recorder`] wraps ratatui's `TestBackend`, recording draw/clear/scroll/raw
//! events and providing `screen()`/`scrollback()` line-level text assertions
//! plus `buffer()` style access.
//!
//! Serves: TUI module tests + presentation-layer regression (AC table B/D
//! area scroll visibility / retry reachability, A-area error-line highlight
//! style assertions). It is orthogonal to "state injection" (UiEvent
//! injection / test hooks) — this module only defines "what can be asserted
//! after rendering", not "how the state got there".

use std::convert::Infallible;
use std::ops::Range;
use std::sync::Arc;

use ratatui::backend::{Backend, ClearType, TestBackend, WindowSize};
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::{Position, Size};
use ratatui::style::Color;

use crate::query::Session;
use crate::tui::chat::Chat;
use crate::tui::term::RawWrite;
use crate::tui::theme::Theme;

/// A minimal offline session (no real endpoint is ever contacted in tests).
pub fn test_session() -> Arc<Session> {
    let core = crate::app::AppCore::start(Default::default());
    Arc::new(Session {
        client: crate::api::client::Client::new(
            "test-key".to_string(),
            "https://example.com".to_string(),
        ),
        runtime: crate::query::Runtime::new("test-model".to_string(), None, Default::default()),
        permission_mode: crate::permission::PermissionMode::Default,
        settings: crate::settings::Settings::default(),
        system: Vec::new(),
        depth: 0,
        cwd: Arc::new(std::sync::Mutex::new(std::env::temp_dir())),
        home: std::env::temp_dir(),
        user_config_dir: std::env::temp_dir().join(".config"),
        quiet: true,
        compact_failures: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        watch: core.watch(),
        tasks: Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "test")),
        expand_tasks: tokio::sync::watch::channel(false).0,
        agents: core.agents(),
        channels: core.channels(),
        turns: core.turns(),
        instance: None,
        attachments: crate::api::image::Attachments::new(),
    })
}

/// One document row with its avatar gutter taken off — the message column
/// alone.
///
/// A conversation row wears a gutter when `experimental.chatAvatars` is on
/// (D110: the one switch every avatar follows), spliced in as whole segments
/// after the body was built. A test that enables the switch and asserts what
/// a row *says*, or what colour the first thing on it is, drops the leading
/// segments the gutter occupies and reads what is left. With the switch off
/// there is nothing to strip and this helper must not be used.
pub fn body(line: &crate::tui::line::Line, images: bool) -> crate::tui::line::Line {
    let want = crate::tui::avatar::gutter_width(images);
    let mut taken = 0;
    let mut segs = line.segs.clone();
    while taken < want && !segs.is_empty() {
        taken += crate::tui::line::text_width(&segs[0].text);
        segs.remove(0);
    }
    crate::tui::line::Line {
        segs,
        image: line.image.clone(),
    }
}

/// A [`Chat`] sized to the given terminal (dark theme, offline session).
pub fn chat_at(width: usize, height: usize) -> Chat {
    let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel();
    let (asks_tx, asks_rx) = tokio::sync::mpsc::unbounded_channel();
    let session = test_session();
    let events = crate::ui::EventSink::new(crate::ui::ConvKey::Main, events_tx);
    session.agents.set_events(events.clone());
    let mut chat = Chat::new(
        session,
        events,
        events_rx,
        asks_tx,
        asks_rx,
        Theme::dark(),
        crate::tui::theme::ThemeSetting::Auto,
        None,
    );
    chat.width = width;
    chat.height = height;
    chat
}

/// TestBackend plus the raw-byte sink and command counters the driver needs asserting on.
///
/// All fields are `pub` (test-helper convention): TUI module tests read the
/// counters/raw bytes directly to assert, `screen()`/`scrollback()` provide
/// line-level text snapshots, `buffer()` provides style assertions.
pub struct Recorder {
    pub inner: TestBackend,
    pub raw: Vec<u8>,
    pub draw_calls: usize,
    pub clear_calls: usize,
    pub scrolled_up: Vec<(Range<u16>, u16)>,
    pub scrolled_down: Vec<(Range<u16>, u16)>,
    pub appended: u16,
}

impl Recorder {
    pub fn new(width: u16, height: u16) -> Self {
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

    pub fn reset_counters(&mut self) {
        self.raw.clear();
        self.draw_calls = 0;
        self.clear_calls = 0;
        self.scrolled_up.clear();
        self.scrolled_down.clear();
        self.appended = 0;
    }

    /// Line-level text snapshot (trailing whitespace stripped): asserts "what
    /// this screen row shows".
    pub fn screen(&self) -> Vec<String> {
        rows_of(self.inner.buffer())
    }

    /// Line-level text snapshot (scrollback region).
    pub fn scrollback(&self) -> Vec<String> {
        rows_of(self.inner.scrollback())
    }

    /// Underlying buffer access, for style assertions (error-line highlight
    /// fg/bg, spinner icon positions, etc.). Cell symbols come from
    /// `buffer[(x, y)].symbol()`, styles from `buffer[(x, y)].style`.
    pub fn buffer(&self) -> &Buffer {
        self.inner.buffer()
    }

    /// Style-aware assertion (qa R2 requirement): asserts row `y` satisfies
    /// **both** — a cell whose fg/bg matches the given colours, and row text
    /// containing `contains`. Only the fg/bg dimensions are compared (bold/
    /// underline etc. are ignored) to keep the test decoupled from the
    /// implementation; passing `None` for `fg`/`bg` leaves that dimension
    /// unrestricted. Used for "error-line highlight" assertions (the error
    /// colour `(255,107,128)` is distinguishable from the normal colour).
    pub fn assert_row_styled(&self, y: u16, fg: Option<Color>, bg: Option<Color>, contains: &str) {
        let buf = self.buffer();
        if y >= buf.area.height {
            panic!(
                "assert_row_styled: row {y} out of range (height {})",
                buf.area.height
            );
        }
        let row: String = (0..buf.area.width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect();
        let styled = (0..buf.area.width).any(|x| {
            let s = buf[(x, y)].style();
            let fg_ok = match fg {
                Some(c) => s.fg == Some(c),
                None => true,
            };
            let bg_ok = match bg {
                Some(c) => s.bg == Some(c),
                None => true,
            };
            fg_ok && bg_ok
        });
        assert!(
            styled,
            "row {y} has no matching style fg={fg:?} bg={bg:?}: {row:?}"
        );
        assert!(
            row.contains(contains),
            "row {y} does not contain {contains:?}: {row:?}"
        );
    }

    /// Viewport row lookup (B-area "scrolled into the visible region"
    /// assertion): returns the index of the first viewport row containing
    /// `needle` (`None` = not in the visible region). Unlike
    /// `scrollback()` — this only checks the viewport; use
    /// [`Self::scrollback`] for the scrollback.
    pub fn visible_row_containing(&self, needle: &str) -> Option<usize> {
        self.screen().iter().position(|row| row.contains(needle))
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

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Infallible> {
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

    fn scroll_region_up(&mut self, region: Range<u16>, line_count: u16) -> Result<(), Infallible> {
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

// ---------------------------------------------------------------------------
// Error-state fixtures (#14 R1 layer 1: shared vehicle for qa assertions +
// presentation acceptance + dev local preview)
// ---------------------------------------------------------------------------

/// Trigger context (production contract, `src/error.rs`): the presentation
/// level is decided by it, not by the code alone.
pub use crate::error::ErrorContext;

/// Presentation level (production contract, `src/error.rs`): the render side
/// branches on it.
pub use crate::error::ErrorLevel;

/// Error-state fixture: a single data carrier (shared by qa assertions /
/// ui/ux acceptance / dev preview). Fields align with the presentation v1.10.1
/// FX checklist (11 injectable stable codes + the second TIMEOUT context).
#[derive(Debug, Clone, Copy)]
pub struct ErrorFixture {
    pub code: &'static str,
    /// Human-readable message (AC-28: what happened + what you can do). qa
    /// assertions anchor only on `code`; `msg` is never asserted.
    pub msg: &'static str,
    pub context: ErrorContext,
    pub level: ErrorLevel,
    /// User action (retry / go back / check network…), the action anchor for
    /// D-area retry/back reachability.
    pub action: &'static str,
    /// Expected error colour RGB (swatch baseline error (255,107,128), the
    /// A-area style assertion anchor).
    pub expect_style: (u8, u8, u8),
}

impl ErrorFixture {
    /// Inject: send a structured `UiEvent::Error` into the test_chat events
    /// channel. Zero production changes — `UiEvent::Error` is already a
    /// structured event; the chat consumer records the error state by
    /// `level`, the render side branches on it.
    pub fn inject(&self, events: &crate::ui::EventSink) {
        events.send(crate::ui::UiEvent::Error {
            code: self.code,
            msg: self.msg.to_string(),
            level: self.level,
            context: self.context,
        });
    }
}

/// All 11 injectable stable codes of §4.4 (FX-01…12). `GENERIC` stays out of
/// the fixtures (no real return point; its guard belongs to error.rs unit
/// tests); mixed state / collapsed detail are not in the injection set.
///
/// Note: **only the TIMEOUT class derives its level from the context**
/// (short sync = page level, long turn = full-flow level, FX-01/FX-11 share
/// the code and are graded by context); every other code has an intrinsic
/// level (AUTH_REQUIRED/PERMISSION_DENIED/CONTEXT_OVERFLOW = full-flow,
/// CONFIG_INVALID = field, the rest = page), and its context field is only recorded
/// informationally (known at production emission time), never used for level
/// derivation.
pub fn error_fixtures() -> Vec<ErrorFixture> {
    use ErrorContext::{LongTurn, ShortSync};
    use ErrorLevel::{Field, Full, Page};
    const ERR: (u8, u8, u8) = (255, 107, 128); // swatch baseline
    vec![
        // FX-01 short-sync read timeout → page level
        ErrorFixture {
            code: "TIMEOUT",
            msg: "request timed out; retry",
            context: ShortSync,
            level: Page,
            action: "retry",
            expect_style: ERR,
        },
        // FX-02 server error → page level
        ErrorFixture {
            code: "SERVER_ERROR",
            msg: "server error; retry later",
            context: ShortSync,
            level: Page,
            action: "retry later",
            expect_style: ERR,
        },
        // FX-03 no network → page level
        ErrorFixture {
            code: "OFFLINE",
            msg: "no network connection; check the network and retry",
            context: ShortSync,
            level: Page,
            action: "check the network and retry",
            expect_style: ERR,
        },
        // FX-04 login expired / missing key → full-flow level
        ErrorFixture {
            code: "AUTH_REQUIRED",
            msg: "login expired or API key missing; sign in again or configure a key",
            context: ShortSync,
            level: Full,
            action: "sign in again",
            expect_style: ERR,
        },
        // FX-05 no permission → full-flow level
        ErrorFixture {
            code: "PERMISSION_DENIED",
            msg: "no permission for this action; go back or request permission",
            context: ShortSync,
            level: Full,
            action: "go back / request permission",
            expect_style: ERR,
        },
        // FX-06 config validation failed → field level (marks only the error object)
        ErrorFixture {
            code: "CONFIG_INVALID",
            msg: "config validation failed; fix the config",
            context: ShortSync,
            level: Field,
            action: "fix the config",
            expect_style: ERR,
        },
        // FX-07 rate limited / 429 → page level
        ErrorFixture {
            code: "RATE_LIMITED",
            msg: "too many requests; retry later",
            context: ShortSync,
            level: Page,
            action: "retry later",
            expect_style: ERR,
        },
        // FX-08 tool execution failed → page level
        ErrorFixture {
            code: "TOOL_FAILED",
            msg: "tool execution failed; check the output and retry",
            context: ShortSync,
            level: Page,
            action: "check the output and retry",
            expect_style: ERR,
        },
        // FX-09 hook execution failed → page level
        ErrorFixture {
            code: "HOOK_FAILED",
            msg: "hook execution failed; check the hook config",
            context: ShortSync,
            level: Page,
            action: "check the hook config",
            expect_style: ERR,
        },
        // FX-10 local storage failed → page level
        ErrorFixture {
            code: "STORAGE_ERROR",
            msg: "local storage failed; check the disk or permissions",
            context: ShortSync,
            level: Page,
            action: "check the disk / permissions",
            expect_style: ERR,
        },
        // FX-11 long-turn transport timeout → full-flow level (AC-53, shares
        // the TIMEOUT code with FX-01, level told apart by context)
        ErrorFixture {
            code: "TIMEOUT",
            msg: "long turn interrupted; retry or go back",
            context: LongTurn,
            level: Full,
            action: "retry or go back",
            expect_style: ERR,
        },
        // FX-12 context overflow after recovery → full-flow level
        ErrorFixture {
            code: "CONTEXT_OVERFLOW",
            msg: "context window exceeded after compaction; reduce or clear context",
            context: LongTurn,
            level: Full,
            action: "reduce or clear context",
            expect_style: ERR,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    /// Helper self-test (qa depends on these APIs, so prove them correct
    /// first): the error colour `(255,107,128)` is distinguishable from a
    /// normal row, and viewport lookup is accurate.
    #[test]
    fn styled_row_assertion_distinguishes_error_color() {
        let mut r = Recorder::new(20, 5);
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 5));
        buf.set_string(
            0,
            2,
            "ERROR: boom",
            Style::default().fg(Color::Rgb(255, 107, 128)),
        );
        buf.set_string(0, 3, "normal row", Style::default());
        let cells = buf.content().iter().enumerate().map(|(i, c)| {
            let x = (i as u16) % 20;
            let y = (i as u16) / 20;
            (x, y, c)
        });
        Backend::draw(&mut r, cells).unwrap();
        // Both style and text conditions hit.
        r.assert_row_styled(2, Some(Color::Rgb(255, 107, 128)), None, "boom");
        // Viewport lookup: visible rows hit / miss.
        assert_eq!(r.visible_row_containing("boom"), Some(2));
        assert_eq!(r.visible_row_containing("normal"), Some(3));
        assert_eq!(r.visible_row_containing("missing"), None);
        // With both style dimensions unrestricted, a plain row can be
        // asserted too (text match is enough).
        r.assert_row_styled(3, None, None, "normal");
    }

    /// Fixture-list completeness (the single data source for qa assertions /
    /// ui/ux acceptance / dev preview): covers all 11 injectable stable codes
    /// of §4.4; `TIMEOUT` has two levels (short sync / long turn) told apart
    /// by context; `GENERIC` is absent.
    #[test]
    fn fixtures_cover_all_injectable_codes() {
        let fxs = error_fixtures();
        let mut codes: Vec<&str> = fxs.iter().map(|f| f.code).collect();
        codes.sort_unstable();
        codes.dedup();
        let expect = [
            "AUTH_REQUIRED",
            "CONFIG_INVALID",
            "CONTEXT_OVERFLOW",
            "HOOK_FAILED",
            "OFFLINE",
            "PERMISSION_DENIED",
            "RATE_LIMITED",
            "SERVER_ERROR",
            "STORAGE_ERROR",
            "TIMEOUT",
            "TOOL_FAILED",
        ];
        assert_eq!(
            codes, expect,
            "the fixture covers all 11 injectable stable codes in §4.4"
        );
        assert!(
            !fxs.iter().any(|f| f.code == "GENERIC"),
            "GENERIC stays out of the fixture (it has no real return site)"
        );
        for f in &fxs {
            assert!(
                !f.code.is_empty()
                    && f.code
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_uppercase())
                    && f.code
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
                "fixture codes must be SCREAMING_SNAKE: {:?}",
                f.code
            );
        }
        // FX-01 and FX-11 share TIMEOUT; the level is told apart by context.
        let short = fxs
            .iter()
            .find(|f| f.code == "TIMEOUT" && f.context == ErrorContext::ShortSync);
        let long = fxs
            .iter()
            .find(|f| f.code == "TIMEOUT" && f.context == ErrorContext::LongTurn);
        assert_eq!(
            short.map(|f| f.level),
            Some(ErrorLevel::Page),
            "short-sync TIMEOUT = page level"
        );
        assert_eq!(
            long.map(|f| f.level),
            Some(ErrorLevel::Full),
            "long-turn TIMEOUT = full-flow level"
        );
    }

    /// Dev local preview (fixture data dump): visible via
    /// `cargo test -- --nocapture`, showing the defining carrier of each
    /// error level. The real render preview upgrades once the #18
    /// presentation layer lands (inject → render → dump).
    #[test]
    fn preview_error_fixtures() {
        for (i, f) in error_fixtures().iter().enumerate() {
            println!(
                "FX-{:02} {:14} ctx={:?} level={:?} style={:?} action={:?} | {}",
                i + 1,
                f.code,
                f.context,
                f.level,
                f.expect_style,
                f.action,
                f.msg
            );
        }
    }
}
