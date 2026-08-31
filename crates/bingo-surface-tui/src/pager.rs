//! One block, whole, over the frame.
//!
//! A transcript row is a glance: a result folds to five lines, a thought decays
//! to how long it took. The pager is where the rest of it is — `⏎` on a focused
//! block, or `ctrl+o` on a result already open (design §5: code, a diff, a long
//! output and reasoning each open in a sheet).
//!
//! It keeps where a person is looking and nothing else: the content is read
//! from the item every frame, so a block that is still arriving grows under the
//! sheet rather than being copied into it.

use bingo_sdk::{Item, ItemBody, ItemId};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::text::{Line, Span};

use crate::clock::Now;
use crate::effect::Effect;
use crate::search::Search;
use crate::tree::Tree;
use crate::ui::{Open, Ui};
use crate::{markdown, search, theme, transcript, wrap};

/// The rows the sheet spends on itself: what it is, and the air under it.
pub const HEAD: usize = 2;

/// What the title row says on its right while nothing is being searched for.
/// The hint lives in the thing it acts on (design §4).
const HINT: &str = "j/k · pgup/pgdn · g/G · / to search · esc";

/// The block a person opened, and where they are in it.
#[derive(Clone, Debug, PartialEq)]
pub struct Pager {
    pub item: ItemId,
    /// The first line of the content the sheet shows.
    pub top: usize,
    /// `/` within the sheet, while one is open.
    pub search: Option<Search>,
}

impl Pager {
    pub fn open(item: ItemId) -> Self {
        Self {
            item,
            top: 0,
            search: None,
        }
    }

    /// Move by lines, never past either end of the content.
    pub fn by(&mut self, lines: isize, window: Window) {
        self.top = self.top.saturating_add_signed(lines).min(window.last());
    }

    pub fn home(&mut self) {
        self.top = 0;
    }

    pub fn end(&mut self, window: Window) {
        self.top = window.last();
    }

    /// The first row shown, never past the end of what there is: a frame that
    /// shrank leaves the window ahead of the content until the next key.
    pub fn at(&self, window: Window) -> usize {
        self.top.min(window.last())
    }

    /// Put a line on the screen: what a search hit asks for.
    pub fn show(&mut self, line: usize, window: Window) {
        if line < self.top {
            self.top = line.min(window.last());
        } else if line >= self.top + window.rows {
            self.top = (line + 1).saturating_sub(window.rows).min(window.last());
        }
    }
}

/// How much content there is and how many rows there are to show it in — what
/// every move of the pager is measured against.
#[derive(Clone, Copy, Debug)]
pub struct Window {
    pub height: usize,
    pub rows: usize,
}

impl Window {
    /// The furthest the content can be scrolled: the last row still fills the
    /// sheet, so nothing is parked past its own end.
    fn last(self) -> usize {
        self.height.saturating_sub(self.rows)
    }
}

/// Everything an item says, with nothing folded away — and nothing at all for
/// one a transcript row already shows whole, which is what says `⏎` does not
/// open it.
pub fn lines(item: &Item, width: usize) -> Vec<Line<'static>> {
    let body = match &item.body {
        ItemBody::Reasoning { text, .. } | ItemBody::Assistant { text } => {
            markdown::render(text, width)
        }
        ItemBody::ToolCall {
            output: Some(output),
            ..
        } => transcript::whole(output, width),
        _ => Vec::new(),
    };
    wrap::wrap_all(&body, width)
}

/// What the sheet is of, on its first row.
pub fn title(item: &Item) -> String {
    match &item.body {
        ItemBody::Reasoning { .. } => "Thinking".to_string(),
        ItemBody::Assistant { .. } => "Answer".to_string(),
        ItemBody::ToolCall { name, input, .. } => {
            format!("{name}({})", transcript::summarize(input))
        }
        _ => String::new(),
    }
}

/// The sheet: what it is on the first row, the window into it under.
pub fn sheet(
    title: &str,
    content: &[Line<'static>],
    pager: &Pager,
    window: Window,
) -> Vec<Line<'static>> {
    let mut out = vec![head(title, pager.search.as_ref()), Line::default()];
    out.extend(
        content
            .iter()
            .skip(pager.at(window))
            .take(window.rows)
            .cloned(),
    );
    out
}

/// The title, and on its right either the keys or the query being typed.
fn head(title: &str, searching: Option<&Search>) -> Line<'static> {
    let mut spans = vec![
        Span::styled(title.to_string(), theme::text().patch(theme::bold())),
        Span::raw("  "),
    ];
    match searching {
        Some(open) => spans.extend(search::row(open).spans),
        None => spans.push(Span::styled(HINT.to_string(), theme::dim())),
    }
    Line::from(spans)
}

// ---- what its keys do ---------------------------------------------------
//
// The pager owns the noun, so it owns the keys that move in it:
// [`crate::input`] routes and this decides, exactly as a dialog does.

/// Open a block whole, and say whether one was there to open. Nothing opens
/// for a row that already shows everything it has.
pub fn open_block(ui: &mut Ui, tree: &Tree, now: Now, focused: Option<&ItemId>) -> bool {
    let width = width_of(ui);
    let Some(id) = crate::input::latest(tree.viewed(), focused, |item| {
        !lines(item, width).is_empty()
    }) else {
        return false;
    };
    ui.layer.show(Open::Pager(Pager::open(id)), now.instant);
    true
}

/// The cells the sheet has, which is what its content is laid out for.
fn width_of(ui: &Ui) -> usize {
    ui.painted.borrow().regions.above().width as usize
}

/// How much the open pager has to show and how many rows it has.
fn window_of(ui: &Ui, tree: &Tree) -> Window {
    let rows = usize::from(ui.painted.borrow().regions.above().height).saturating_sub(HEAD);
    Window {
        height: content_of(ui, tree).len(),
        rows,
    }
}

/// What the open pager is showing, as the sheet lays it out.
fn content_of(ui: &Ui, tree: &Tree) -> Vec<Line<'static>> {
    let Open::Pager(open) = &ui.layer.open else {
        return Vec::new();
    };
    let width = width_of(ui);
    tree.viewed()
        .items
        .iter()
        .find(|item| item.id == open.item)
        .map(|item| lines(item, width))
        .unwrap_or_default()
}

/// The pager owns the keyboard while it is up: `j/k` and the page keys move
/// the window, `g`/`G` take its ends, `/` looks through it, and `esc` gives the
/// frame back — folding the result it was opened from.
pub fn keys(ui: &mut Ui, tree: &Tree, key: KeyEvent, now: Now) -> Vec<Effect> {
    if searching(ui) {
        return searched(ui, tree, key);
    }
    if key.code == KeyCode::Esc {
        closed(ui, now);
        return Vec::new();
    }
    let window = window_of(ui, tree);
    if let Open::Pager(open) = &mut ui.layer.open {
        moved(open, key, window);
    }
    Vec::new()
}

fn searching(ui: &Ui) -> bool {
    matches!(&ui.layer.open, Open::Pager(open) if open.search.is_some())
}

fn moved(open: &mut Pager, key: KeyEvent, window: Window) {
    let page = window.rows.max(1) as isize;
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => open.by(1, window),
        KeyCode::Char('k') | KeyCode::Up => open.by(-1, window),
        KeyCode::PageDown | KeyCode::Char(' ') => open.by(page, window),
        KeyCode::PageUp => open.by(-page, window),
        KeyCode::Char('g') | KeyCode::Home => open.home(),
        KeyCode::Char('G') | KeyCode::End => open.end(window),
        KeyCode::Char('/') => open.search = Some(Search::open()),
        _ => {}
    }
}

/// Leaving the sheet folds the result it came from, so `ctrl+o` twice and
/// `esc` once is a round trip rather than a state a person is left in.
fn closed(ui: &mut Ui, now: Now) {
    let opened = match &ui.layer.open {
        Open::Pager(open) => Some(open.item.clone()),
        _ => None,
    };
    if let Some(item) = opened {
        ui.expanded.remove(&item);
    }
    ui.layer.close(now.instant);
}

/// `/` inside the sheet: the same query row as `ctrl+f`, over this block alone.
fn searched(ui: &mut Ui, tree: &Tree, key: KeyEvent) -> Vec<Effect> {
    let window = window_of(ui, tree);
    let text: Vec<String> = content_of(ui, tree)
        .iter()
        .map(ToString::to_string)
        .collect();
    let Open::Pager(open) = &mut ui.layer.open else {
        return Vec::new();
    };
    if key.code == KeyCode::Esc {
        open.search = None;
        return Vec::new();
    }
    let Some(search) = open.search.as_mut() else {
        return Vec::new();
    };
    let line = walk(search, key, &text);
    if let Some(line) = line {
        open.show(line, window);
    }
    Vec::new()
}

/// One key of a query row: what it does, and the line it wants on the screen.
fn walk(search: &mut Search, key: KeyEvent, text: &[String]) -> Option<usize> {
    let by = match (search.typing, key.code) {
        (true, KeyCode::Char(c)) => {
            search.typed(c);
            return None;
        }
        (true, KeyCode::Backspace) => {
            search.backspace();
            return None;
        }
        (true, KeyCode::Enter) => {
            search.find(text);
            0
        }
        (false, KeyCode::Char('n') | KeyCode::Enter) => 1,
        (false, KeyCode::Char('N')) => -1,
        _ => return None,
    };
    search.step(by);
    search.current().map(|hit| hit.line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use bingo_sdk::{ItemStatus, ToolOutput};

    fn window(height: usize) -> Window {
        Window { height, rows: 10 }
    }

    fn pager() -> Pager {
        Pager::open(ItemId::from_raw("itm_1"))
    }

    #[test]
    fn a_result_opens_with_every_line_it_folded_away() {
        let output = ToolOutput::text((1..=40).map(|i| format!("line {i}\n")).collect::<String>());
        let item = tool(
            "itm_1",
            "Read",
            serde_json::json!({"file_path": "src/lib.rs"}),
            Some(output),
            ItemStatus::Completed,
        );
        let content = lines(&item, 60);
        assert_eq!(content.len(), 40);
        assert_eq!(content[39].to_string(), "line 40");
        assert_eq!(title(&item), "Read(src/lib.rs)");
    }

    #[test]
    fn a_thought_opens_as_what_was_thought() {
        let item = item(
            "itm_1",
            ItemStatus::Completed,
            ItemBody::Reasoning {
                text: "The manifest first, then the lockfile.".into(),
                provider_metadata: Default::default(),
            },
        );
        assert_eq!(
            lines(&item, 60)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            vec!["The manifest first, then the lockfile.".to_string()],
        );
        assert_eq!(title(&item), "Thinking");
    }

    /// A table never wraps: it folds to the width it is laid out in (design
    /// §7). In the transcript that width is the prose measure; in the sheet it
    /// is the whole frame, which is what opening one is for.
    #[test]
    fn a_table_too_wide_for_the_measure_has_more_room_in_the_sheet() {
        let cell = "x".repeat(60);
        let table = format!("| {cell} | {cell} |\n|---|---|\n| {cell} | {cell} |\n");
        let item = assistant("itm_1", &table, ItemStatus::Completed);
        let widest = |rows: &[Line<'static>]| {
            rows.iter()
                .map(|row| unicode_width::UnicodeWidthStr::width(row.to_string().trim_end()))
                .max()
                .unwrap_or(0)
        };
        let in_transcript = crate::markdown::render(&table, crate::wrap::measure(160) - 2);
        assert_eq!(widest(&in_transcript), 98, "cut to the measure, with an …");
        assert_eq!(widest(&lines(&item, 160)), 122, "and whole in the sheet");
    }

    #[test]
    fn a_row_that_already_shows_everything_opens_nothing() {
        let item = user("itm_1", "run the tests");
        assert!(lines(&item, 60).is_empty());
    }

    #[test]
    fn the_window_never_parks_past_either_end() {
        let mut pager = pager();
        pager.by(5, window(40));
        assert_eq!(pager.top, 5);
        pager.by(1_000, window(40));
        assert_eq!(pager.top, 30, "the last row still fills the sheet");
        pager.by(-1_000, window(40));
        assert_eq!(pager.top, 0);
        pager.end(window(40));
        assert_eq!(pager.top, 30);
        pager.home();
        assert_eq!(pager.top, 0);
        pager.end(window(4));
        assert_eq!(pager.top, 0, "content shorter than the sheet does not move");
    }

    #[test]
    fn a_hit_below_the_fold_is_brought_onto_the_screen() {
        let mut pager = pager();
        pager.show(25, window(40));
        assert_eq!(pager.top, 16, "the hit is the last row shown");
        pager.show(3, window(40));
        assert_eq!(pager.top, 3, "and above the fold it becomes the first");
        pager.show(5, window(40));
        assert_eq!(pager.top, 3, "one already on the screen moves nothing");
    }

    #[test]
    fn the_title_row_carries_the_keys_until_a_query_takes_it() {
        let mut pager = pager();
        let head = sheet("Read(src/lib.rs)", &[], &pager, window(0))[0].to_string();
        assert!(head.starts_with("Read(src/lib.rs)"), "{head}");
        assert!(head.contains(HINT), "{head}");
        pager.search = Some(Search::open());
        let head = sheet("Read(src/lib.rs)", &[], &pager, window(0))[0].to_string();
        assert!(head.contains('/'), "{head}");
        assert!(
            !head.contains(HINT),
            "the keys give way to the query: {head}"
        );
    }
}
